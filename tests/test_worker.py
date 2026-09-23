"""The worker agent: protocol, the hub, and a real connection end to end.

The end-to-end test runs a real uvicorn server and a real worker agent in the same process,
connected by a real websocket, and drives a completion through both. Mocking the socket would leave
the interesting part — the thread/event-loop bridge — untested, and that bridge is where this design
is most likely to deadlock.
"""
from __future__ import annotations

import asyncio
import os
import tempfile
import threading
import time
import unittest
from unittest import mock
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_worker_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, settings
from citar.auth import accounts, policy, tokens
from citar.db.models import Base, Server, WorkerToken
from citar.server import workers as W
from citar.worker import protocol as P

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class Protocol(unittest.TestCase):
    def test_frames_round_trip(self):
        raw = P.request_frame(P.Request(id="r1", model="m", messages=[{"role": "user", "content": "hi"}]))
        data = P.parse(raw)
        self.assertEqual(data["t"], P.REQUEST)
        self.assertEqual(data["id"], "r1")
        self.assertEqual(len(data["messages"]), 1)

    def test_malformed_frames_are_rejected(self):
        for bad in ("not json", "[]", '{"no":"type"}', '"a string"'):
            with self.assertRaises(P.ProtocolError, msg=bad):
                P.parse(bad)

    def test_oversized_frame_is_rejected(self):
        with self.assertRaises(P.ProtocolError):
            P.parse("x" * (P.MAX_FRAME_BYTES + 1))

    def test_tokens_are_never_logged(self):
        # `hello` carries a live credential; a debug log must not.
        redacted = P.redact({"t": "hello", "token": "SECRET-TOKEN", "models": [{"key": "m"}]})
        self.assertEqual(redacted["token"], "<redacted>")
        self.assertNotIn("SECRET-TOKEN", str(redacted))

    def test_summary_is_readable(self):
        self.assertIn("request", P.summarize({"t": P.REQUEST, "id": "a", "model": "m", "messages": []}))


class Hub(unittest.TestCase):
    """The hub without a socket: registration, liveness, and the failure paths."""

    def setUp(self):
        _reset()
        self.hub = W.WorkerHub()

    def _connection(self, server_id="sv1", **hello):
        payload = {"hostname": "testbox", "provider": "lmstudio",
                   "models": [{"key": "m1"}], "max_concurrent": 1, **hello}
        return W.WorkerConnection(websocket=None, server_id=server_id, hello=payload, loop=None)

    def test_unknown_server_is_offline(self):
        self.assertFalse(self.hub.is_online("nope"))
        self.assertEqual(self.hub.in_flight("nope"), 0)

    def test_register_and_unregister(self):
        c = self._connection()
        self.hub.register(c)
        self.assertTrue(self.hub.is_online("sv1"))
        self.hub.unregister(c)
        self.assertFalse(self.hub.is_online("sv1"))

    def test_reconnect_replaces_the_previous_connection(self):
        """A flapping link must not lock the owner out of their own server for a minute."""
        first = self._connection()
        self.hub.register(first)
        second = self._connection()
        previous = self.hub.register(second)
        self.assertIs(previous, first)
        self.assertTrue(first.closed)
        self.assertTrue(self.hub.is_online("sv1"))

    def test_stale_connection_counts_as_offline(self):
        c = self._connection()
        self.hub.register(c)
        c.last_seen = time.time() - (W.STALE_AFTER + 5)
        self.assertFalse(self.hub.is_online("sv1"))

    def test_submit_without_a_worker_refuses_rather_than_hanging(self):
        with self.assertRaises(W.WorkerError) as caught:
            self.hub.submit("sv1", P.Request(id="r", model="m", messages=[]), timeout=1)
        self.assertEqual(caught.exception.refusal, "closed")
        self.assertTrue(caught.exception.retryable)

    def test_missing_worker_reads_as_a_connection_problem_to_the_agent(self):
        """The turn driver waits out connection errors; a dropped worker must look like one, not like a
        broken model (which used to skip the turn instantly, every turn, until the worker came back)."""
        from citar.agents.providers.worker_provider import WorkerConversation, WorkerConnectionError
        from citar.agents.llm_agent import _is_unreachable
        conv = WorkerConversation({"server_id": "sv-nobody", "model": "m"}, "system", [])
        with self.assertRaises(WorkerConnectionError) as caught:
            conv.step()
        self.assertTrue(_is_unreachable(caught.exception))

    def test_disconnect_fails_everything_in_flight(self):
        """Work in flight when the socket drops can never be answered; the waiting thread must be
        released rather than blocking until its timeout."""
        c = self._connection()
        self.hub.register(c)
        pending = c.open_request("r1")
        released = threading.Event()

        def waiter():
            pending.event.wait(5)
            released.set()

        threading.Thread(target=waiter, daemon=True).start()
        self.hub.unregister(c)
        self.assertTrue(released.wait(3), "waiting thread was not released on disconnect")
        self.assertIsNotNone(pending.error)

    def test_late_answer_to_a_forgotten_request_is_ignored(self):
        c = self._connection()
        self.hub.register(c)
        c.resolve("never-heard-of-it", result={"text": "hi"})   # must not raise

    def test_in_flight_counts_open_requests(self):
        c = self._connection()
        self.hub.register(c)
        self.assertEqual(self.hub.in_flight("sv1"), 0)
        c.open_request("a")
        c.open_request("b")
        self.assertEqual(self.hub.in_flight("sv1"), 2)
        c.resolve("a", result={})
        self.assertEqual(self.hub.in_flight("sv1"), 1)


class PooledWork(unittest.TestCase):
    """Benchmarks on a machine from the Servers page, and waiting for a game that holds it."""

    def setUp(self):
        _reset()
        with db.session() as s:
            owner = accounts.create_user(s, handle="owner", email="o@x.cc", password="a long enough phrase",
                                         status="active", email_verified=True)
            server = Server(owner_id=owner.id, name="GPU box", kind="owned", provider="lmstudio",
                            config={"id": "sv_gpu", "models": [{"key": "m1"}]})
            s.add(server)
            s.flush()
            self.server_id = server.id
        from citar.server.session import SessionManager
        self.manager = SessionManager()
        self.dir = Path(tempfile.mkdtemp(prefix="citar-pooled-"))

    def tearDown(self):
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)

    def test_a_queued_benchmark_job_waits_for_a_game_on_its_machine(self):
        from citar.pool import seats
        from citar.server.benchmarks import BenchmarkScheduler, resolve_group
        group = resolve_group({"server_id": self.server_id, "models": [{"model": "m1"}]})
        self.assertFalse(group["missing"])
        self.assertTrue(group["pooled"])
        self.assertEqual(group["models"][0]["model"], "m1")

        game = self.manager.create({"map_size": "duel", "seed": 3},
                                   [{"type": "llm", "llm": {"server_id": self.server_id, "model": "m1"}}, {"type": "bot"}])
        game.suspend()                      # a paused game still holds its machine: it will resume
        self.assertEqual(len(seats.occupied(self.server_id)), 1)

        sch = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        try:
            run = sch.create_run({"name": "t", "mode": "sequential", "repeats": 1,
                                  "servers": [{"server_id": self.server_id, "models": [{"model": "m1"}]}],
                                  "scenarios": [{"name": "s", "map_size": "duel", "turn_limit": 2, "opponents": 1}]})
            sch.tick()
            job = run["jobs"][0]
            self.assertEqual(job["status"], "queued")
            self.assertIn("GPU box is in use by", job["waiting"])

            self.manager.delete(game.id)     # the game ends: the machine is free, the job may start
            self.assertEqual(seats.occupied(self.server_id), [])
            sch.tick()
            self.assertIn(job["status"], ("loading", "running"))
            self.assertIsNone(job["waiting"])
        finally:
            sch.stop()

    def test_an_eliminated_model_frees_its_machine(self):
        from citar.pool import seats
        game = self.manager.create({"map_size": "duel", "seed": 3},
                                   [{"type": "llm", "llm": {"server_id": self.server_id, "model": "m1"}}, {"type": "bot"}])
        game.suspend()
        self.assertEqual(len(seats.occupied(self.server_id)), 1)
        game.game.player(0).alive = False       # the bots play on; the model will never be asked again
        self.assertEqual(seats.occupied(self.server_id), [])

    def _bare_runner(self):
        """A probe runner without its background dispatcher, to drive by hand."""
        from citar import probes
        runner = probes.ProbeRunner.__new__(probes.ProbeRunner)
        runner.manager, runner.lock, runner._stop_run, runner._thread = self.manager, threading.Lock(), set(), None
        runner.lives, runner._workers, runner._loaded = {}, {}, {}
        runner.POLL_SECONDS = 0.05
        return runner

    def test_probe_runs_on_different_machines_do_not_wait_for_each_other(self):
        """One busy machine held up probe runs on every other machine, and runs went one at a time."""
        from citar.pool import seats
        runner = self._bare_runner()
        ids = []
        for n, server in (("busy", self.server_id), ("free", "sv-elsewhere"), ("third", "sv-third")):
            rid = f"test-{n}-{int(time.time() * 1000) % 100000}"
            runner._write({"id": rid, "name": n, "llm": {"server_id": server}, "status": "queued", "jobs": []})
            ids.append(rid)
        runner.queue = list(ids)
        started, both = [], threading.Event()
        release = threading.Event()

        def fake_run(rid):
            started.append(rid)
            if len([r for r in started if r != ids[0]]) == 2:
                both.set()
            release.wait(10)
        runner._run = fake_run
        seats.claim(self.server_id, "game “long one”")
        loop = threading.Thread(target=runner._loop, daemon=True)
        try:
            loop.start()
            self.assertTrue(both.wait(10), f"runs on two free machines should run at the same time: {started}")
            self.assertNotIn(ids[0], started, "the busy machine's run waits")
            waiting = runner.get_run(ids[0])
            self.assertEqual(waiting["status"], "waiting (machine busy)")
            self.assertIn("long one", waiting["waiting"])
        finally:
            seats.release(self.server_id, "game “long one”")
            release.set()
        loop.join(10)
        self.assertFalse(loop.is_alive(), "the dispatcher stops when everything has run")
        self.assertEqual(set(started), set(ids))

    def test_one_queue_orders_benchmarks_and_probes_by_priority_then_age(self):
        from citar.pool import queue as work_queue
        from citar.server.benchmarks import BenchmarkScheduler
        runner = self._bare_runner()
        rid = f"test-old-probe-{int(time.time() * 1000) % 100000}"
        runner._write({"id": rid, "name": "old probe", "llm": {"server_id": self.server_id}, "status": "queued",
                       "jobs": [], "created": time.time() - 3600})
        runner.queue = [rid]
        work_queue.register("probe", runner.queue_items)
        sch = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        try:
            run = sch.create_run({"name": "t", "mode": "sequential", "repeats": 1,
                                  "servers": [{"server_id": self.server_id, "models": [{"model": "m1"}]}],
                                  "scenarios": [{"name": "s", "map_size": "duel", "turn_limit": 2, "opponents": 1}]})
            sch.tick()
            job = run["jobs"][0]
            self.assertEqual(job["status"], "queued", "the probe run queued an hour earlier goes first")
            self.assertIn("old probe goes first", job["waiting"])
            self.assertEqual([i["id"] for i in work_queue.waiting(self.server_id)], [rid, job["id"]])
            work_queue.set_priority("benchmark", run["id"], 5)
            self.assertEqual(work_queue.waiting(self.server_id)[0]["id"], job["id"])
            sch.tick()
            self.assertIn(job["status"], ("loading", "running"), "a higher priority jumps the queue")
        finally:
            sch.stop()
            work_queue.set_priority("benchmark", run["id"], 0)
            work_queue.register("probe", lambda: [])

    def test_a_probe_run_holds_its_machine(self):
        from citar.pool import seats
        seats.claim(self.server_id, "probe run “x”")
        try:
            self.assertEqual(seats.occupied(self.server_id), ["probe run “x”"])
        finally:
            seats.release(self.server_id, "probe run “x”")
        self.assertEqual(seats.occupied(self.server_id), [])


class QuietHours(unittest.TestCase):
    """Quiet hours of a Servers-page machine are its owner's wall clock, not the web server's."""

    def test_read_in_the_owners_zone(self):
        from datetime import datetime
        from zoneinfo import ZoneInfo
        from citar.pool import seats
        ny = ZoneInfo("America/New_York")
        pooled = {"id": "sv_d", "name": "Desk", "owner_tz": "America/New_York",
                  "config": {"restricted_hours": {"enabled": True, "windows": [
                      {"days": [0, 1, 2, 3, 4, 5, 6], "start": "18:00", "end": "06:00"}]}}}

        def local(*a):
            return datetime(*a, tzinfo=ny).astimezone().replace(tzinfo=None)

        self.assertEqual(seats.restricted(pooled, local(2026, 9, 21, 22, 0)), local(2026, 9, 22, 6, 0))
        self.assertEqual(seats.restricted(pooled, local(2026, 9, 22, 3, 0)), local(2026, 9, 22, 6, 0))
        self.assertIsNone(seats.restricted(pooled, local(2026, 9, 22, 7, 0)))
        pooled["config"]["restricted_hours"]["enabled"] = False
        self.assertIsNone(seats.restricted(pooled, local(2026, 9, 21, 22, 0)))

    def test_benchmarks_and_probes_see_them(self):
        """Everything asks servers.restricted_at, which falls back to the Servers-page machine."""
        from datetime import datetime
        from citar import servers
        pooled = {"id": "sv_d", "name": "Desk", "owner_tz": "UTC",
                  "config": {"restricted_hours": {"enabled": True, "grace_minutes": 5, "windows": [
                      {"days": [0, 1, 2, 3, 4, 5, 6], "start": "00:00", "end": "23:59"}]}}}
        with mock.patch("citar.pool.seats.lookup", lambda sid: pooled if sid == "sv_d" else None):
            self.assertIsNotNone(servers.restricted_at("sv_d", datetime(2026, 9, 21, 12, 0)))
            self.assertIsNone(servers.restricted_at("sv_other", datetime(2026, 9, 21, 12, 0)))
            self.assertEqual(servers.server_name("sv_d"), "Desk")
            self.assertEqual(servers.restriction_config("sv_d")["grace_minutes"], 5)


class LocalSettings(unittest.TestCase):
    """What the server may and may not decide about a completion the helper runs."""

    def _config_seen(self, config, params):
        """Run `_complete` with the local model call stubbed out; return the config it was given."""
        from types import SimpleNamespace
        from citar.worker.agent import Worker
        seen = {}

        class FakeConversation:
            def __init__(self, cfg, system, tools):
                seen.update(cfg)
                self.usage = {}

            def step(self):
                return SimpleNamespace(text="", thinking="", stop_reason="stop", malformed=0,
                                       tool_calls=[])

            def close(self):
                pass

        with mock.patch("citar.agents.providers.openai_provider.OpenAIConversation", FakeConversation):
            Worker(config)._complete({"model": "test-model", "messages": [], "params": params})
        return seen

    def test_server_params_cannot_change_where_or_with_what_key(self):
        """Where to connect and with which key are the owner's settings. A server that could set
        them could aim the helper at another machine on the owner's network, or have it send the
        owner's API key to a host of the server's choosing."""
        from citar.worker.agent import WorkerConfig
        seen = self._config_seen(
            WorkerConfig(server_url="https://citar.test", token="t", base_url="http://localhost:1234/v1",
                         provider="lmstudio", api_key_env="CITAR_TEST_HELPER_KEY", collect_hardware=False),
            {"base_url": "http://192.168.1.1/v1", "provider": "openai",
             "api_key": "sk-the-servers-own", "api_key_env": "AWS_SECRET_ACCESS_KEY",
             "temperature": 0.3, "max_tokens": 64, "reasoning_effort": "low", "top_p": 0.9, "seed": 7})

        self.assertEqual(seen["base_url"], "http://localhost:1234/v1")
        self.assertEqual(seen["provider"], "lmstudio")
        self.assertEqual(seen["api_key_env"], "CITAR_TEST_HELPER_KEY")
        self.assertNotIn("api_key", seen)
        # Generation settings are the server's to choose, and still get through.
        self.assertEqual((seen["temperature"], seen["max_tokens"], seen["reasoning_effort"],
                          seen["top_p"], seen["seed"]), (0.3, 64, "low", 0.9, 7))

    def test_a_helper_without_a_key_cannot_be_given_one(self):
        from citar.worker.agent import WorkerConfig
        seen = self._config_seen(WorkerConfig(server_url="https://citar.test", token="t",
                                              collect_hardware=False),
                                 {"api_key_env": "AWS_SECRET_ACCESS_KEY"})
        self.assertNotIn("api_key_env", seen)


class Authentication(unittest.TestCase):
    def setUp(self):
        _reset()
        with db.session() as s:
            owner = accounts.create_user(s, handle="hostess", email="h@x.cc",
                                         password="a long enough phrase", status="active",
                                         email_verified=True)
            server = Server(owner_id=owner.id, name="Box", kind="owned", provider="lmstudio",
                            config={"id": "sv_x"})
            s.add(server)
            s.flush()
            self.server_id = server.id
            self.raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=server.id, token_hash=hashed,
                              prefix=tokens.prefix(self.raw), label="test"))

    def test_a_seat_on_a_pooled_machine_plays_through_its_helper(self):
        """A machine from the Servers page is not in the registry; a seat naming it must still resolve,
        to the worker provider, instead of failing with 'No server'."""
        from citar import servers
        cfg = servers.resolve_llm({"server_id": self.server_id, "model": "qwen/qwen3.8-27b", "reconnect_seconds": 30})
        self.assertEqual(cfg["provider"], "worker")
        self.assertEqual(cfg["server_id"], self.server_id)
        self.assertEqual(cfg["model"], "qwen/qwen3.8-27b")
        self.assertEqual(cfg["reconnect_seconds"], 30)
        info = servers.describe_seat({"server_id": self.server_id, "model": "qwen/qwen3.8-27b"})
        self.assertEqual(info["server"], "Box")
        self.assertFalse(info["missing_server"])

    def test_a_game_on_an_offline_pooled_machine_is_refused(self):
        from citar.pool import seats as pool_seats
        with db.session() as s:
            owner = s.query(Server).get(self.server_id).owner_id
            from citar.db.models import User
            user = s.get(User, owner)
            with self.assertRaises(ValueError):
                pool_seats.authorize(s, user, [{"type": "llm", "llm": {"server_id": self.server_id, "model": "m"}}])
            pool_seats.authorize(s, user, [{"type": "bot"}, {"type": "llm", "llm": {"provider": "mock"}}])

    def test_valid_token_authenticates(self):
        result = W.authenticate(self.raw)
        self.assertIsNotNone(result)
        self.assertEqual(result[0], self.server_id)

    def test_unknown_and_empty_tokens_are_refused(self):
        self.assertIsNone(W.authenticate("not-a-real-token"))
        self.assertIsNone(W.authenticate(""))

    def test_revoked_token_stops_working(self):
        from datetime import datetime, timezone
        with db.session() as s:
            row = s.query(WorkerToken).first()
            row.revoked_at = datetime.now(timezone.utc)
        self.assertIsNone(W.authenticate(self.raw))

    def test_disabled_server_refuses_its_worker(self):
        with db.session() as s:
            s.get(Server, self.server_id).enabled = False
        self.assertIsNone(W.authenticate(self.raw))

    def test_only_the_hash_is_stored(self):
        """A leaked database backup must not yield a usable worker credential."""
        with db.session() as s:
            row = s.query(WorkerToken).first()
            self.assertNotEqual(row.token_hash, self.raw)
            self.assertNotIn(self.raw, row.token_hash)
            self.assertEqual(row.token_hash, tokens.hash_token(self.raw))


class EndToEnd(unittest.TestCase):
    """A real server, a real worker, a real websocket, and a completion driven from a thread."""

    @classmethod
    def setUpClass(cls):
        _reset()
        import uvicorn
        from citar.server.app import app

        with db.session() as s:
            owner = accounts.create_user(s, handle="hostess", email="h@x.cc",
                                         password="a long enough phrase", status="active",
                                         email_verified=True)
            server = Server(owner_id=owner.id, name="Test box", kind="owned",
                            provider="lmstudio", config={"id": "sv_e2e"}, max_concurrent=2)
            s.add(server)
            s.flush()
            cls.server_id = server.id
            cls.token, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=server.id, token_hash=hashed,
                              prefix=tokens.prefix(cls.token), label="e2e"))

        cls.port = 8791
        config = uvicorn.Config(app, host="127.0.0.1", port=cls.port, log_level="error")
        cls.server = uvicorn.Server(config)
        cls.thread = threading.Thread(target=cls.server.run, daemon=True)
        cls.thread.start()
        for _ in range(100):
            if cls.server.started:
                break
            time.sleep(0.1)

    @classmethod
    def tearDownClass(cls):
        cls.server.should_exit = True
        cls.thread.join(timeout=10)

    def _run_worker(self, *, fake_completion=None, max_concurrent=1, quiet=None, models=None):
        """Start a worker in a background thread with its local model call stubbed out.

        Stubbing only `_complete` keeps everything that matters real: the websocket, the handshake,
        the framing, the thread bridge and the concurrency accounting.
        """
        from citar.worker.agent import Worker, WorkerConfig

        cfg = WorkerConfig(server_url=f"http://127.0.0.1:{self.port}", token=self.token,
                           max_concurrent=max_concurrent, name="testbox",
                           collect_hardware=False, quiet_hours=quiet or [], models=models)
        worker = Worker(cfg)
        worker.discover_models = lambda: _async_value([{"key": "test-model", "label": "Test"}])
        worker._complete = fake_completion or (lambda data: {
            "text": "hello from the worker",
            "thinking": "", "stop_reason": "stop", "malformed": 0,
            "tool_calls": [{"id": "c1", "name": "end_turn", "args": {}}],
            "usage": {"input_tokens": 11, "output_tokens": 7, "reasoning_tokens": 0},
        })

        loop = asyncio.new_event_loop()

        def run():
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(worker.run())
            except asyncio.CancelledError:
                pass          # how the worker is stopped; not an error

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        for _ in range(100):
            if W.hub().is_online(self.server_id):
                break
            time.sleep(0.1)
        return worker, loop, thread

    def _stop_worker(self, worker, loop, thread):
        """Stop the worker and wait for its thread, quietly.

        worker.stop() clears the run flag, then cancelling the tasks interrupts the `await` it is
        currently sitting in. The runner below swallows the resulting CancelledError, which is the
        expected outcome of a cancel rather than a failure worth printing.
        """
        worker.stop()
        loop.call_soon_threadsafe(
            lambda: [task.cancel() for task in asyncio.all_tasks(loop)])
        thread.join(timeout=5)

    def test_worker_connects_and_serves_a_completion(self):
        worker, loop, thread = self._run_worker()
        try:
            self.assertTrue(W.hub().is_online(self.server_id), "worker did not connect")
            connection = W.hub().get(self.server_id)
            self.assertEqual(connection.hostname, "testbox")
            self.assertEqual(connection.model_keys(), ["test-model"])

            # Submit from a plain thread, exactly as the turn driver does.
            result = {}

            def caller():
                result["answer"] = W.hub().submit(
                    self.server_id,
                    P.Request(id="req-1", model="test-model",
                              messages=[{"role": "user", "content": "go"}], timeout=20),
                    timeout=20)

            t = threading.Thread(target=caller)
            t.start()
            t.join(timeout=25)
            self.assertFalse(t.is_alive(), "submit() never returned — the thread bridge deadlocked")
            answer = result.get("answer") or {}
            self.assertEqual(answer.get("text"), "hello from the worker")
            self.assertEqual(answer["usage"]["input_tokens"], 11)
            self.assertEqual(len(answer["tool_calls"]), 1)
        finally:
            self._stop_worker(worker, loop, thread)

    def test_worker_refuses_when_busy(self):
        def slow(data):
            time.sleep(2)
            return {"text": "done", "tool_calls": [], "usage": {}, "stop_reason": "stop",
                    "thinking": "", "malformed": 0}

        worker, loop, thread = self._run_worker(fake_completion=slow, max_concurrent=1)
        try:
            errors = []
            results = []

            def caller(n):
                try:
                    results.append(W.hub().submit(
                        self.server_id,
                        P.Request(id=f"busy-{n}", model="test-model", messages=[], timeout=20),
                        timeout=20))
                except W.WorkerError as exc:
                    errors.append(exc)

            threads = [threading.Thread(target=caller, args=(i,)) for i in range(3)]
            for t in threads:
                t.start()
                time.sleep(0.15)
            for t in threads:
                t.join(timeout=25)

            # The machine's own limit is what protects it; the hub reports "busy", not an error.
            self.assertTrue(errors, "expected at least one busy refusal")
            self.assertTrue(any(e.refusal == "busy" for e in errors),
                            f"refusals were {[e.refusal for e in errors]}")
            self.assertTrue(all(e.retryable for e in errors))
        finally:
            self._stop_worker(worker, loop, thread)

    def test_worker_enforces_its_own_quiet_hours(self):
        """The owner's machine has the last word: even though the server admitted the request, the
        worker refuses during its local quiet hours."""
        from datetime import datetime
        now = datetime.now()
        # A window covering right now, on today's weekday.
        window = [(now.weekday(), 0, 1440)]
        worker, loop, thread = self._run_worker(quiet=window)
        try:
            with self.assertRaises(W.WorkerError) as caught:
                W.hub().submit(self.server_id,
                               P.Request(id="quiet-1", model="test-model", messages=[], timeout=15),
                               timeout=15)
            self.assertEqual(caught.exception.refusal, "closed")
            self.assertIn("quiet hours", str(caught.exception))
        finally:
            self._stop_worker(worker, loop, thread)

    def test_worker_refuses_a_model_its_owner_did_not_list(self):
        """The configured list is an allowlist, not an advertisement: the server can ask for any name,
        and the worker must say no to one its owner did not offer."""
        calls = []

        def completion(data):
            calls.append(data)
            return {"text": "ran", "tool_calls": [], "usage": {}, "stop_reason": "stop",
                    "thinking": "", "malformed": 0}

        worker, loop, thread = self._run_worker(fake_completion=completion, models=["test-model"])
        try:
            with self.assertRaises(W.WorkerError) as caught:
                W.hub().submit(self.server_id,
                               P.Request(id="unlisted-1", model="some-other-model", messages=[],
                                         timeout=15),
                               timeout=15)
            self.assertEqual(caught.exception.refusal, "unknown_model")
            self.assertFalse(caught.exception.retryable)
            self.assertEqual(calls, [], "the unlisted model was run anyway")

            answer = W.hub().submit(self.server_id,
                                    P.Request(id="listed-1", model="test-model", messages=[],
                                              timeout=15),
                                    timeout=15)
            self.assertEqual(answer.get("text"), "ran")
        finally:
            self._stop_worker(worker, loop, thread)

    def test_bad_token_is_refused_and_the_worker_gives_up(self):
        from citar.worker.agent import Worker, WorkerConfig

        cfg = WorkerConfig(server_url=f"http://127.0.0.1:{self.port}", token="wrong-token",
                           collect_hardware=False)
        worker = Worker(cfg)
        worker.discover_models = lambda: _async_value([])
        loop = asyncio.new_event_loop()

        def run():
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(worker.run())
            except asyncio.CancelledError:
                pass

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        thread.join(timeout=15)
        # A bad token will never start working, so the worker stops instead of retrying forever.
        self.assertFalse(thread.is_alive(), "worker kept retrying a token that will never work")
        self.assertFalse(worker.running)

    def test_tls_context_finds_roots_when_the_platform_paths_are_wrong(self):
        """A frozen helper's OpenSSL looks where the build machine kept its CA roots; on another Linux
        distribution that is nowhere, and every handshake failed. The bundled roots must still load."""
        import os
        import ssl
        from citar.worker.agent import tls_context
        with mock.patch.dict(os.environ, {"SSL_CERT_FILE": "/nonexistent/cert.pem", "SSL_CERT_DIR": "/nonexistent"}):
            ctx = tls_context()
        self.assertGreater(ctx.cert_store_stats()["x509_ca"], 50)
        self.assertEqual(ctx.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(ctx.check_hostname)

    def test_unreachable_server_is_retried_not_abandoned(self):
        """'Connection refused' is what a worker sees while the server restarts; it must keep trying."""
        import socket
        from citar.worker.agent import Worker, WorkerConfig
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            closed_port = s.getsockname()[1]
        worker = Worker(WorkerConfig(server_url=f"http://127.0.0.1:{closed_port}", token="t", collect_hardware=False))
        worker.discover_models = lambda: _async_value([])
        loop = asyncio.new_event_loop()

        def run():
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(worker.run())
            except asyncio.CancelledError:
                pass

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        thread.join(timeout=4)
        self.assertTrue(thread.is_alive(), "worker gave up on a server that was only unreachable")
        self.assertTrue(worker.running)
        worker.stop()
        loop.call_soon_threadsafe(lambda: [t.cancel() for t in asyncio.all_tasks(loop)])
        thread.join(timeout=10)

    def test_worker_error_reaches_the_caller_as_a_failure(self):
        def broken(data):
            raise RuntimeError("the local model is not loaded")

        worker, loop, thread = self._run_worker(fake_completion=broken)
        try:
            with self.assertRaises(W.WorkerError) as caught:
                W.hub().submit(self.server_id,
                               P.Request(id="err-1", model="test-model", messages=[], timeout=15),
                               timeout=15)
            self.assertIn("not loaded", str(caught.exception))
        finally:
            self._stop_worker(worker, loop, thread)


def _async_value(value):
    async def _inner():
        return value
    return _inner()


if __name__ == "__main__":
    unittest.main()
