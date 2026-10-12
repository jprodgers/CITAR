"""The prompt scheduler end to end (design/phase3-features.md P3.3.3-P3.3.4), package 3-07's gates 3, 4 and 5.

- **Draining** (gate 3): a real server, a real Python helper (the worker agent in a thread, its completions made by the
  OpenAI-compatible provider against the QA harness's scripted endpoint, which takes 20 s a completion). A running
  ticket cancelled sends ``CANCEL``; the helper (protocol v1) finishes the completion anyway, so the slot drains: the
  next request waits for the helper's late ``Done``, which reaches the scheduler, and the helper refuses nothing as
  busy.
- **One slot, several seats** (gate 4): the dry-run provider on a row with ``max_concurrent = 1`` (the test
  registry's ``sv_gpu``). Two lobby games with a model seat each play five rounds with never more than one request
  in flight and no busy refusal; two model seats of one game: the one that opened a trade and waits lends its slot,
  and the other's answer (a lease request) gets it, with no timeout; a test ticket answering the chat gets it too.
- **Metering** (gate 5), through the scripted endpoint: an honest helper's ``gen_s`` is its ``duration``; one
  reporting ten times the time the server observed is credited the observed time and flagged after three such
  requests; token counts above ``max_tokens`` are capped; ``queue_s`` is recorded; a turn's metrics carry the
  generation and the queue time apart.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import asyncio
import importlib.util
import json
import os
import socket
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import db, servers, settings
from citar.agents.providers import make_conversation
from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.auth import tokens
from citar.db.models import AuditLog, Base, Server, WorkerToken
from citar.pool import scheduler as S
from citar.server import workers as W
from citar.worker import protocol as P

ROOT = Path(__file__).resolve().parent.parent
_TMP = Path(tempfile.mkdtemp(prefix="citar_sched_e2e_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()


def _qa():
    """``scripts/qa_phase3.py``, for its scripted endpoint."""
    spec = importlib.util.spec_from_file_location("qa_phase3_for_scheduler", ROOT / "scripts" / "qa_phase3.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa = _qa()
MODEL = qa.FakeEndpoint.MODEL


def _database():
    """This module's own database, with the test registry's machines."""
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()


def _helper_machine(name: str, slots: int = 1) -> tuple:
    """A helper machine (nobody's) and a worker token for it: (server id, token)."""
    with db.session() as s:
        row = Server(owner_id=None, name=name, kind="owned", provider="openai_compatible", reach="worker",
                     max_concurrent=slots, config={"models": [{"key": MODEL}]})
        s.add(row)
        s.flush()
        raw, hashed = tokens.new_pair()
        s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="e2e"))
        return row.id, raw


def _until(check, limit: float = 10.0, step: float = 0.02):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(step)
    return check()


class Overreporting:
    """A helper whose ``Done`` frames claim ten times the time it took (and five seconds more)."""

    @staticmethod
    def wrap(worker):
        original = worker.handle_request

        async def handle_request(websocket, data):
            class Proxy:
                async def send(self, text):
                    frame = json.loads(text)
                    if frame.get("t") == P.DONE:
                        frame["duration"] = round(float(frame.get("duration") or 0) * 10 + 5, 3)
                        text = json.dumps(frame)
                    await websocket.send(text)
            await original(Proxy(), data)
        worker.handle_request = handle_request
        return worker


class HelperBase(unittest.TestCase):
    """A real server on a free port; helpers connect to it in threads of their own.

    The app's start makes process-wide workers that would outlive this module's server: the host's power sampler
    (which runs ``nvidia-smi`` every few seconds where there is one, and later modules watch for outbound calls) is
    not started, and the benchmark scheduler, the probe and report runners it made are stopped and put back as they
    were when the server is gone."""

    @classmethod
    def setUpClass(cls):
        import uvicorn
        from citar import usage
        from citar.server import app as appmod, reports_api
        _database()
        S.reset()
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            cls.port = probe.getsockname()[1]
        cls.server = uvicorn.Server(uvicorn.Config(appmod.app, host="127.0.0.1", port=cls.port, log_level="error"))
        cls.thread = threading.Thread(target=cls.server.run, daemon=True)
        cls.saved = (appmod.scheduler, appmod._probe_runner, reports_api._runner)
        cls.sampler = usage._sampler
        # the server's start brings back the games other test modules left marked as open: not this one's business
        from citar.server.session import SessionManager
        with mock.patch.object(SessionManager, "restore_live", lambda self: []), \
                mock.patch.object(usage, "start_power_sampler", lambda follow_host=False: None):
            cls.thread.start()
            _until(lambda: cls.server.started, 15)

    @classmethod
    def tearDownClass(cls):
        from citar.server import app as appmod, reports_api
        cls.server.should_exit = True
        cls.thread.join(timeout=10)
        if appmod.scheduler is not None and appmod.scheduler is not cls.saved[0]:
            appmod.scheduler.stop()
        appmod.scheduler, appmod._probe_runner, reports_api._runner = cls.saved
        S.reset()

    def setUp(self):
        self.events: list = []
        S.get().add_listener(self.events.append)
        self.addCleanup(S.get().remove_listener, self.events.append)

    def helper(self, sid: str, token: str, endpoint, *, wrap=None, slots: int = 1):
        """A real helper serving ``endpoint``, connected for ``sid``; stopped at the test's end."""
        from citar.worker.agent import Worker, WorkerConfig
        worker = Worker(WorkerConfig(server_url=f"http://127.0.0.1:{self.port}", token=token, base_url=endpoint.url,
                                     provider="openai_compatible", max_concurrent=slots, name=f"e2e-{sid}",
                                     collect_hardware=False))
        if wrap is not None:
            wrap(worker)
        refusals: list = []
        refuse = worker._refuse

        async def recording_refuse(websocket, request_id, refusal, message, retryable=True):
            refusals.append(refusal)
            await refuse(websocket, request_id, refusal, message, retryable)
        worker._refuse = recording_refuse
        loop = asyncio.new_event_loop()

        def run():
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(worker.run())
            except asyncio.CancelledError:
                pass
        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        self.assertTrue(_until(lambda: W.hub().is_online(sid), 15), "the helper did not connect")

        def stop():
            worker.stop()
            if not loop.is_closed():
                loop.call_soon_threadsafe(lambda: [t.cancel() for t in asyncio.all_tasks(loop)])
            thread.join(timeout=10)
        self.addCleanup(stop)
        return worker, refusals

    @staticmethod
    def conversation(sid: str, binding, **cfg):
        conv = make_conversation({"provider": "worker", "server_id": sid, "model": MODEL, "timeout": 60, **cfg},
                                 "You are a test.", [], binding=binding)
        conv.add_user_text("Reply with exactly: CITAR OK")
        return conv


class Draining(HelperBase):
    """Gate 3."""

    def test_the_servers_start_left_no_power_sampler_behind(self):
        from citar import usage
        self.assertIs(usage._sampler, self.sampler, "the app's start made no process-wide power sampler here")
        self.assertFalse([t for t in threading.enumerate() if t.name == "power-sampler" and self.sampler is None])

    def test_a_cancelled_ticket_drains_until_the_helpers_late_answer(self):
        endpoint = qa.FakeEndpoint(delay=20.0)
        self.addCleanup(endpoint.close)
        sid, token = _helper_machine("drain box")
        _worker, refusals = self.helper(sid, token, endpoint)
        base = len(endpoint.connections)                    # the helper's model listing at its hello
        sched = S.get()
        m = sched.machine(sid)
        self.assertEqual(m.capacity(), 1)
        ticket = sched.ticket(sid, MODEL, cls="test", label="drained test")
        self.assertEqual(ticket.state, "queued", ticket.reason)
        first = self.conversation(sid, ticket)
        out: dict = {}

        def run(conv, key):
            try:
                out[key] = conv.step()
            except Exception as e:
                out[key] = e
        a = threading.Thread(target=run, args=(first, "a"), daemon=True)
        a.start()
        self.assertTrue(_until(lambda: len(endpoint.connections) >= base + 1, 15), "the request reached the endpoint")
        with self.assertLogs("citar.worker", level="INFO") as logged:
            ticket.cancel()
            a.join(5)
            self.assertFalse(a.is_alive(), "the caller stops waiting at once")
            self.assertIsInstance(out["a"], Exception)
            self.assertTrue(_until(lambda: any("cancelled" in line for line in logged.output), 5),
                            "the helper was sent CANCEL")
        self.assertTrue(m.slots and m.slots[0].draining, "its slot drains while the helper finishes it")

        lease = sched.lease(sid, MODEL, kind="game", label="next")
        second = self.conversation(sid, lease)
        b = threading.Thread(target=run, args=(second, "b"), daemon=True)
        b.start()
        self.assertTrue(_until(lambda: len(endpoint.connections) >= base + 2, 40), "the next request went out")
        done = endpoint.completions()
        self.assertEqual(len(done), 1, "the first completion was finished by the helper")
        first_end = done[0]["t"] + done[0]["seconds"]
        second_sent = endpoint.connections[base + 1][0]
        self.assertGreaterEqual(second_sent, first_end - 0.05, "the next request waited for the drain")
        self.assertGreaterEqual(m.counters["late_frames"], 1, "the late Done reached the scheduler")
        self.assertEqual(m.counters["drain_timeouts"], 0)
        self.assertEqual(m.counters["busy_refusals"], 0)
        self.assertNotIn("busy", refusals, "the helper refused nothing as busy")
        second.close()                                      # it drains too; the helper is stopped with the test
        b.join(5)
        self.assertGreater(second.last_queue_s, 15.0, "its wait was its queue time")


class Metering(HelperBase):
    """Gate 5, through the scripted endpoint."""

    def test_an_honest_helper_is_credited_its_duration(self):
        endpoint = qa.FakeEndpoint(delay=0.3)
        self.addCleanup(endpoint.close)
        sid, token = _helper_machine("honest box")
        self.helper(sid, token, endpoint)
        conv = self.conversation(sid, S.get().lease(sid, MODEL, kind="game", activity="game:e2e"))
        conv.step()
        e = self.events[-1]
        self.assertEqual(e.machine, sid)
        self.assertGreaterEqual(e.reported_s, 0.3)
        self.assertLessEqual(e.reported_s, e.observed_s)
        self.assertEqual(e.gen_s, e.reported_s, "an honest helper's duration is what it is credited")
        self.assertFalse(e.flags.get("overreported"))

    def test_an_overreporting_helper_is_credited_the_observed_time_and_flagged(self):
        endpoint = qa.FakeEndpoint(delay=0.3)
        self.addCleanup(endpoint.close)
        sid, token = _helper_machine("boastful box")
        self.helper(sid, token, endpoint, wrap=Overreporting.wrap)
        lease = S.get().lease(sid, MODEL, kind="game")
        for _ in range(3):
            self.conversation(sid, lease).step()
        steps = [e for e in self.events if e.machine == sid]
        self.assertEqual(len(steps), 3)
        for e in steps:
            self.assertGreater(e.reported_s, 9 * e.observed_s)
            self.assertEqual(e.gen_s, e.observed_s, "credited what the server saw, not what it claimed")
            self.assertTrue(e.flags["overreported"])

        def flagged():
            with db.session() as s:
                row = s.get(Server, sid)
                return (row.flags or {}).get("overreports") if row is not None else None
        flag = _until(flagged, 10)
        self.assertTrue(flag, "the machine is flagged after three such requests")
        self.assertEqual(flag["count"], 3)
        with db.session() as s:
            rows = [r for r in s.query(AuditLog).filter(AuditLog.action == "server.overreports")
                    if r.object_id == sid]
        self.assertEqual(len(rows), 1, "with one audit row")

    def test_token_counts_are_capped_and_queue_time_is_recorded(self):
        endpoint = qa.FakeEndpoint(delay=0.5)
        self.addCleanup(endpoint.close)
        sid, token = _helper_machine("capped box")
        self.helper(sid, token, endpoint)
        lease = S.get().lease(sid, MODEL, kind="game")
        convs = [self.conversation(sid, lease, max_tokens=5) for _ in range(2)]
        threads = [threading.Thread(target=c.step) for c in convs]
        for t in threads:
            t.start()
            time.sleep(0.05)
        for t in threads:
            t.join(15)
        steps = [e for e in self.events if e.machine == sid]
        self.assertEqual(len(steps), 2)
        for e in steps:
            self.assertEqual(e.usage["output_tokens"], 5, "the endpoint's 10 tokens capped at max_tokens")
            self.assertTrue(e.flags["capped_tokens"])
        self.assertGreater(max(c.last_queue_s for c in convs), 0.3, "one waited for the other: its queue time")
        self.assertEqual(sorted(e.queue_s > 0.3 for e in steps), [False, True])

    def test_a_turns_metrics_carry_generation_and_queue_time_apart(self):
        from citar.server.session import SessionManager
        endpoint = qa.FakeEndpoint(delay=0.2)
        self.addCleanup(endpoint.close)
        sid, token = _helper_machine("turn box")
        self.helper(sid, token, endpoint)
        manager = SessionManager()
        s = manager.create({"map_size": "duel", "seed": 11, "barbarians": "off"},
                           [{"type": "llm", "llm": {"server_id": sid, "model": MODEL}}, {"type": "bot"}])
        self.addCleanup(manager.delete, s.id)
        self.assertTrue(_until(lambda: s.game.turn >= 2, 60), "the model seat played its turn")
        s.set_paused(True)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertGreater(rec["model_steps"], 0)
        self.assertGreater(rec["model_s"], 0.2 * rec["model_steps"] * 0.9, "generation seconds")
        self.assertIn("queue_s", rec, "and the time waited for the machine, apart")
        mine = [e for e in self.events if e.machine == sid]
        self.assertEqual(round(sum(e.gen_s for e in mine if e.cls == "game"), 3) >= rec["model_s"] - 0.01, True)


# ---------------------------------------------------------------------------- gate 4: one slot, several seats

class Opener(Conversation):
    """Seat A's model: opens a trade with seat 1 (maps for maps), then ends its turn."""

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.usage = {"input_tokens": 0, "output_tokens": 0}
        self.tools = {t["name"] for t in tools}
        self.n = 0

    def add_user_text(self, text):
        pass

    def add_tool_results(self, results):
        pass

    def step(self):
        self.n += 1
        if "end_turn" not in self.tools:                    # answering a chat out of turn: nothing to say
            return StepResult(text="")
        if self.n == 1:
            return StepResult(text="", tool_calls=[ToolCall("o1", "open_negotiation", {
                "to": 1, "message": "maps for maps?", "give": [{"type": "share_map"}],
                "receive": [{"type": "share_map"}]})])
        return StepResult(text="", tool_calls=[ToolCall(f"e{self.n}", "end_turn", {})])


def _factory(original):
    """``make_conversation`` with seat A (persona "opener") scripted and every other seat the dry run."""
    def make(cfg, system, tools, binding=None):
        if cfg.get("persona") == "opener":
            return Opener(cfg, system, tools)
        return original(cfg, system, tools)
    return make


class OneSlot(unittest.TestCase):
    """Gate 4: the dry-run provider through ``sv_gpu`` (``max_concurrent = 1``)."""

    @classmethod
    def setUpClass(cls):
        _database()

    def setUp(self):
        from citar.server.session import SessionManager
        S.reset()
        self.addCleanup(S.reset)
        self.sched = S.get()
        self.events: list = []
        self.sched.add_listener(self.events.append)
        self.manager = SessionManager()

    def _create(self, seats, seed=21):
        s = self.manager.create({"map_size": "duel", "seed": seed, "barbarians": "off"}, seats, start=False)
        self.addCleanup(self.manager.delete, s.id)
        return s

    def test_two_lobby_games_share_one_slot_never_two_requests_in_flight(self):
        llm = {"server_id": "sv_gpu", "model": "dry-run", "dry_run_delay": 0.05}
        games = [self._create([{"type": "llm", "llm": dict(llm)}, {"type": "bot"}], seed=seed) for seed in (31, 32)]
        for s in games:
            s.start()
        self.assertTrue(_until(lambda: all(s.game.turn >= 6 for s in games), 120), "both played five rounds")
        for s in games:
            s.set_paused(True)
        m = self.sched.machine("sv_gpu")
        steps = sorted((e.at - e.gen_s, e.at) for e in self.events if e.machine == "sv_gpu")
        self.assertGreaterEqual(len(steps), 2 * 5 * 2)
        overlaps = [(a, b) for a, b in zip(steps, steps[1:]) if b[0] < a[1] - 0.005]
        self.assertEqual(overlaps, [], "never more than one request in flight (from the listener's steps)")
        self.assertEqual(m.max_in_flight, 1)
        self.assertEqual(m.counters["busy_refusals"], 0)
        self.assertEqual({e.consumer for e in self.events}, {None})
        for s in games:
            self.assertEqual([r["end_reason"] for r in s.metrics.data["turns"] if r["player"] == 0 and r["ended"]
                              and r["turn"] <= 5], ["end_turn"] * 5)

    def test_a_seat_waiting_on_a_chat_lends_its_slot_to_the_other_seats_answer(self):
        from citar.agents import llm_agent
        s = self._create([{"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run", "persona": "opener"}},
                          {"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run", "dry_run_delay": 0.05,
                                                  "dry_run_negotiation": "accept"}},
                          {"type": "bot"}])
        with s.lock:
            s.game.meet(0, 1)
        seen = []

        def answered(event):
            lease_a = s.leases.get(0)
            if event.machine == "sv_gpu" and lease_a is not None and s.leases.get(1) is not None \
                    and event.activity == s.usage_act and lease_a.active is False and s.game.current == 0:
                seen.append(event)
        self.sched.add_listener(answered)
        t0 = time.monotonic()
        with mock.patch.object(llm_agent, "make_conversation", _factory(llm_agent.make_conversation)):
            s.start()
            self.assertTrue(_until(lambda: s.game.current != 0 or s.game.turn > 1, 60), "seat A's turn ended")
            s.set_paused(True)
        took = time.monotonic() - t0
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec["end_reason"], "end_turn", "ended by the model, not by a timeout")
        self.assertLess(took, 30.0, "the chat was answered, not waited out")
        self.assertTrue(seen, "seat B's answer ran on the slot while seat A waited (A's lease idle)")
        chats = s.game.negotiations(0)
        self.assertEqual([n["status"] for n in chats], ["accepted"], "seat B's model accepted it")

    def test_a_test_ticket_gets_the_slot_of_a_seat_waiting_on_a_chat(self):
        from citar.agents import llm_agent
        s = self._create([{"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run", "persona": "opener"}},
                          {"type": "human"}, {"type": "bot"}])
        with s.lock:
            s.game.meet(0, 1)
        with mock.patch.object(llm_agent, "make_conversation", _factory(llm_agent.make_conversation)):
            s.start()
            waiting = _until(lambda: s.leases.get(0) is not None and not s.leases[0].active
                             and s.game.open_negotiation_heads(0), 30)
            self.assertTrue(waiting, "seat A opened a chat and waits, idle")
            m = self.sched.machine("sv_gpu")
            ticket = self.sched.ticket("sv_gpu", "dry-run", cls="test", consumer="someone", start_within=5)
            self.assertEqual(ticket.state, "queued", ticket.reason)
            grant = ticket.acquire()
            self.assertTrue(ticket.borrowing, "it runs on the slot seat A lends")
            self.assertEqual(len(m.slots), 1)
            nid = s.game.open_negotiation_heads(0)[0]["id"]
            res = s.call_tool(1, "respond_negotiation", {"negotiation_id": nid, "action": "accept",
                                                          "message": "agreed (a test ticket's answer)"})
            ticket.finish(grant, gen_s=0.1)
            ticket.close()
            self.assertTrue(res["ok"], res)
            self.assertTrue(_until(lambda: s.game.current != 0 or s.game.turn > 1, 30), "seat A's turn ended")
            s.set_paused(True)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec["end_reason"], "end_turn")


class GrantCap(unittest.TestCase):
    """The per-consumer cap comes from the grant (``AccessGrant.concurrency``; P3.3.7): a member whose grant has
    concurrency 1 gets a lease capped at 1, so the scheduler never runs two of her requests at once there; the
    machine's owner and a game with no owner are not capped."""

    @classmethod
    def setUpClass(cls):
        _database()

    def test_a_seats_lease_carries_its_owners_grant_concurrency(self):
        from citar.auth import accounts
        from citar.db.models import AccessGrant, ServerGroup
        from citar.pool import seats as pool_seats
        from citar.server.session import SessionManager
        S.reset()
        self.addCleanup(S.reset)
        with db.session() as s:
            owner = accounts.create_user(s, handle="lender", email="l@x.cc", password="a long enough phrase",
                                         status="active", email_verified=True)
            member = accounts.create_user(s, handle="borrower", email="b@x.cc", password="a long enough phrase",
                                          status="active", email_verified=True)
            group = ServerGroup(owner_id=owner.id, name="lent", visibility="server")
            s.add(group)
            s.flush()
            row = Server(owner_id=owner.id, name="lent box", kind="test", provider="dryrun", reach="direct",
                         group_id=group.id, max_concurrent=2,
                         config={"connection": {"provider": "dryrun"}, "models": [{"key": "dry-run"}]})
            s.add(row)
            s.add(AccessGrant(group_id=group.id, subject_type="everyone", concurrency=1))
            s.flush()
            sid, owner_id, member_id = row.id, owner.id, member.id
        self.assertEqual(pool_seats.consumer_cap(member_id, sid), 1)
        self.assertEqual(pool_seats.consumer_cap(owner_id, sid), 0, "the owner is not capped on her own machine")
        self.assertEqual(pool_seats.consumer_cap(None, sid), 0, "nor is the server's own work")
        manager = SessionManager()
        game = manager.create({"map_size": "duel", "seed": 41, "barbarians": "off"},
                              [{"type": "llm", "llm": {"server_id": sid, "model": "dry-run"}}, {"type": "bot"}],
                              start=False, owner_id=member_id)
        self.addCleanup(manager.delete, game.id)
        lease = game.lease_for(0)
        self.assertEqual((lease.server_id, lease.consumer, lease.cap, lease.kind), (sid, member_id, 1, "game"))
        m = lease.machine
        first = m.take(m.submit(lease))
        second = m.submit(lease)
        self.assertIsNotNone(first)
        self.assertIsNone(m.take(second), "her second request waits though a slot is free")
        m.finish(first, gen_s=1)
        self.assertIsNotNone(m.take(second))


if __name__ == "__main__":
    unittest.main()
