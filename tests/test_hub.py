"""The hub on protocol 2 (design/phase4-helper.md P4.5.1, P4.5.3; package 4-05), and what stays true of it from protocol 1.

- ``Hub``: the hub without a socket: registration, liveness, the failure paths, the clamp of an acknowledgement.
- ``Cancel`` (gate 3): a fake helper over a real socket acknowledging a cancel within 50 ms frees the slot without
  draining, so a ticket waiting behind it starts within 200 ms; one that never acknowledges leaves the slot draining
  and its machine flagged ``cancel_unacked``.
- ``Keys`` (gate 4): the first hello registers its key, the same key reconnects, another is ``refused`` ``key`` until
  the owner's reset (one audit row, the trust unchanged), after which it registers; the key reset and rescan routes'
  access tests.
- ``RateLimit`` (gate 5): 20 distinct refused tokens from one address, then a good token refused ``rate`` before any
  lookup; one revoked token retried 40 times counting once, another machine's good token from the same address
  connecting; a forged ``X-Forwarded-For`` changing nothing with ``behind_proxy`` off.
- ``Refusal`` (gate 6): a 0.1.5 hello, a protocol 3 hello and one with neither key each get the v1 refusal and 4426,
  with no token lookup and no audit row; 100 of them from one address log one line.
- ``Revocation``: revoking a token, disabling, deleting a machine or suspending its owner drops its helper within a
  second with ``shutdown`` and its code; a request in flight fails, not retried; a change made elsewhere drops it
  within a ping; a revoke between the hello and its registration still drops it.
- ``Overhead`` (gate 10): 1,000 request and ``done`` round trips through a fake helper that answers at once, the hub's
  own time per round trip (observed minus the fake helper's) under 3 ms at the median.
- ``Settings`` (4-05's fix round): a slot change made in the machine editor, by another process or while a helper was
  being registered reaches it; an acknowledgement is held to the settings frame it answers, so one that crossed a push
  is no overclaim; a change that changes nothing waits for nothing, and one request a machine waits.
- ``Frames`` (4-05's fix round): a first frame over 2 MiB is refused unread, and a long frame is parsed off the event
  loop, so a frame nobody signed in for does not stall every other connection.
- ``Tokens``: only a token's hash is stored, a database that fails refuses nothing, two first hellos cannot both
  register a key, and a seat on a pooled machine plays through its helper (what ``test_worker.py`` held of them).
- ``Power`` (4-06, gate 3's hub half): a helper's minute of samples is one power row of its machine, placed by the
  hub's clock and never overlapping the last; a frame sooner than ``POWER_MIN_SECONDS`` after the last, samples that do
  not read, and a machine whose owner turned live sampling off write nothing; and costing prices the helper machine's
  energy from its rows (measured, not estimated). A ``rescan`` reaches the helper and its ``hardware`` frame is kept
  as the last report.
- ``PooledWork`` and ``QuietHours``: what the old module held of pooled machines, unchanged.
- ``JobsHandshake`` (4-10's fix round): a dispatcher that cannot say which runs it holds gets a helper no welcome
  (1011), none in the process keeps every run and wants every result, and an acknowledgement sent ``again`` with its
  job host's figures answers no settings frame.

Replaces ``tests/test_worker.py`` (protocol 1, the Python worker); the real binary end to end is
``tests/test_helper_e2e.py``.
"""
from __future__ import annotations

import functools
import os
import shutil
import statistics
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401
from tests import has_test_ops

_TMP = Path(tempfile.mkdtemp(prefix="citar_hub_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, servers, settings
from citar.auth import accounts, policy, sessions, tokens
from citar.db.models import AuditLog, Base, Server, User, WorkerToken
from citar.pool import scheduler as S
from citar.server import helper_protocol as HP
from citar.server import workers as W
from tests import helper_fixtures as F
from tests import helperfake as H

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

MODEL = "model-a"
CI = bool(os.environ.get("CI"))


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()
    servers.invalidate()


def _machine(owner_id, name: str, *, slots: int = 1, key: str = None) -> tuple:
    """A helper machine of ``owner_id``'s and a worker token for it: (server id, token, token id)."""
    with db.session() as s:
        server = Server(owner_id=owner_id, name=name, kind="owned", provider="lmstudio", reach="worker",
                        max_concurrent=slots, config={"models": [{"key": MODEL}]}, public_key=key)
        s.add(server)
        s.flush()
        raw, hashed = tokens.new_pair()
        token = WorkerToken(server_id=server.id, token_hash=hashed, prefix=tokens.prefix(raw), label="t")
        s.add(token)
        s.flush()
        servers.invalidate()
        return server.id, raw, token.id


# ---------------------------------------------------------------------------- the hub without a socket

class Hub(unittest.TestCase):
    """The hub without a socket: registration, liveness, the failure paths."""

    def setUp(self):
        _reset()
        self.hub = W.WorkerHub()

    def _connection(self, server_id="sv1", **kw):
        return H.StubConnection(server_id, [H.model("m1")], **kw)

    def test_a_token_never_starts_like_an_option(self):
        """One URL-safe token in 64 starts with "-", and ``citar-helper --token -x...`` (the command the Servers page
        shows) then reads it as an option and does not start: none is handed out."""
        drawn = iter(["-starts-like-an-option", "a-usable-token"])
        with mock.patch.object(tokens.secrets, "token_urlsafe", lambda n: next(drawn)):
            self.assertEqual(tokens.generate(), "a-usable-token")
        self.assertFalse(any(tokens.generate().startswith("-") for _ in range(2000)))

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
        """A flapping link must not lock the owner out of their own machine for a minute."""
        first = self._connection()
        self.hub.register(first)
        second = self._connection()
        previous = self.hub.register(second)
        self.assertIs(previous, first)
        self.assertTrue(first.closed)
        self.assertTrue(self.hub.is_online("sv1"))

    def test_a_power_row_is_placed_by_the_hubs_clock_and_never_overlaps(self):
        """The helper's own sample times are its clock's, not the server's: the row ends when the frame came and covers
        ten seconds a sample, never reaching back before ``since`` (the machine's last frame, or its connection's
        start); with under ten seconds left after it, there is no row at all rather than one reaching back."""
        samples = [{"t": 1000.0 + 10 * i, "gpu_w": 40.0 + i, "cpu_util": 0.1 * i} for i in range(6)]
        row = W.power_row("sv1", samples, 10_000.0, 9_000.0)
        self.assertEqual(row, {"k": "pw", "srv": "sv1", "t0": 9_940.0, "t1": 10_000.0, "n": 6, "cpu_util": 0.25,
                               "gpu_w": 42.5})
        self.assertEqual(W.power_row("sv1", samples, 10_000.0, 9_970.0)["t0"], 9_970.0, "not before the last row")
        self.assertEqual(W.power_row("sv1", samples, 10_000.0, 9_990.0)["t0"], 9_990.0, "ten seconds is a row")
        self.assertIsNone(W.power_row("sv1", samples, 10_000.0, 9_995.0), "under ten seconds is none")
        # a draw no machine has is left out; a sample with nothing that reads is no row
        odd = [{"t": 1.0, "gpu_w": 1e9, "cpu_util": 0.5}, {"t": 2.0, "gpu_w": 100.0, "cpu_util": None}]
        self.assertEqual(W.power_row("sv1", odd, 100.0, 0.0), {"k": "pw", "srv": "sv1", "t0": 80.0, "t1": 100.0, "n": 2,
                                                              "cpu_util": 0.5, "gpu_w": 100.0})
        self.assertIsNone(W.power_row("sv1", [{"t": 1.0, "gpu_w": None, "cpu_util": None}], 100.0, 0.0))
        self.assertIsNone(W.power_row("sv1", [{"t": 1.0, "gpu_w": 1e9, "cpu_util": None}], 100.0, 0.0))
        # nor is a draw over what the machine's own scan says its GPUs can draw
        capped = [{"t": 1.0, "gpu_w": 100.0, "cpu_util": 0.5}, {"t": 2.0, "gpu_w": 451.0, "cpu_util": 0.5}]
        self.assertEqual(W.power_row("sv1", capped, 100.0, 0.0, 450.0)["gpu_w"], 100.0)
        self.assertEqual(W.power_row("sv1", capped, 100.0, 0.0)["gpu_w"], 275.5, "without a cap only 20 kW holds")

    def test_the_cap_on_a_gpu_draw_is_what_the_machines_own_scan_says_its_cards_can_draw(self):
        """1.5 times the power limits of the GPUs whose draw the helper samples (``power_readable``), when the scan
        lists every one of them; otherwise none (only 20 kW holds)."""
        def card(limit, readable=True, **kw):
            return {"name": "card", "vendor": "NVIDIA", "power_limit_w": limit, "power_readable": readable, **kw}
        self.assertEqual(W.gpu_draw_cap({"gpus": [card(300.0)]}), 450.0)
        self.assertEqual(W.gpu_draw_cap({"gpus": [card(300.0), card(200)]}), 750.0)
        # a card whose draw does not read (the system's list, an Intel GPU) neither counts nor stops the cap
        self.assertEqual(W.gpu_draw_cap({"gpus": [card(300.0), card(None, readable=False)]}), 450.0)
        for scan in ({"gpus": [card(None)]}, {"gpus": [card(300.0), card(0)]}, {"gpus": [card("lots")]},
                     {"gpus": [card(None, readable=False)]}, {"gpus": []}, {"gpus": "x"}, {}, None, "x",
                     {"gpus": [card(float("nan"))]}):
            self.assertIsNone(W.gpu_draw_cap(scan), scan)
        self.assertEqual(W.gpu_draw_cap({"gpus": [card(100_000.0)]}), W.POWER_W_MAX)

    def test_nothing_reaches_a_helper_before_its_welcome(self):
        """Registering a helper wakes the requests waiting for its machine, which may be sent before the server has
        sent its welcome; a helper takes nothing but the welcome first. Frames sent before the welcome wait and follow
        it in order, one sent while they go out joining them; then frames go as they come."""
        import asyncio
        sent: list = []

        class Socket:
            async def send_text(self, text):
                sent.append(text)
                await asyncio.sleep(0)          # the loop runs other sends meanwhile, as a real socket's would

        async def scenario():
            c = W.WorkerConnection(Socket(), "sv1", H.hello(), asyncio.get_running_loop(), settings=H.settings())
            await c.send("request 1")
            await c.send("cancel 1")
            self.assertEqual(sent, [], "nothing goes before the welcome")
            late = asyncio.ensure_future(c.send("request 2"))       # sent while the welcome's backlog goes out
            await c.welcome("welcome")
            await late
            await c.send("request 3")
            return c
        loop = asyncio.new_event_loop()
        try:
            c = loop.run_until_complete(scenario())
        finally:
            loop.close()
        self.assertEqual(sent, ["welcome", "request 1", "cancel 1", "request 2", "request 3"])
        self.assertTrue(c.welcomed)

    def test_stale_connection_counts_as_offline(self):
        c = self._connection()
        self.hub.register(c)
        c.last_seen = time.time() - (W.STALE_AFTER + 5)
        self.assertFalse(self.hub.is_online("sv1"))

    def test_submit_without_a_helper_refuses_rather_than_hanging(self):
        with self.assertRaises(W.WorkerError) as caught:
            self.hub.submit("sv1", "r", "m", [{"role": "user", "content": "hi"}], None, {}, 1)
        self.assertEqual(caught.exception.refusal, "closed")
        self.assertTrue(caught.exception.retryable)
        self.assertIn("citar-helper", str(caught.exception))

    def test_missing_helper_reads_as_a_connection_problem_to_the_agent(self):
        """The turn driver waits out connection errors; a dropped helper must look like one, not like a broken
        model."""
        from citar.agents.llm_agent import _is_unreachable
        from citar.agents.providers.worker_provider import WorkerConnectionError, WorkerConversation
        conv = WorkerConversation({"server_id": "sv-nobody", "model": "m"}, "system", [])
        with self.assertRaises(WorkerConnectionError) as caught:
            conv.step()
        self.assertTrue(_is_unreachable(caught.exception))

    def test_disconnect_fails_everything_in_flight(self):
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
        c.resolve("never-heard-of-it", result={"t": "done"})   # must not raise

    def test_in_flight_counts_open_requests(self):
        c = self._connection()
        self.hub.register(c)
        self.assertEqual(self.hub.in_flight("sv1"), 0)
        c.open_request("a")
        c.open_request("b")
        self.assertEqual(self.hub.in_flight("sv1"), 2)
        c.resolve("a", result={})
        self.assertEqual(self.hub.in_flight("sv1"), 1)

    def test_a_request_on_a_connection_dropped_meanwhile_fails_at_once(self):
        c = self._connection()
        self.hub.register(c)
        t0 = time.perf_counter()
        with mock.patch.object(self.hub, "get", return_value=c):        # handed out, then dropped
            self.assertTrue(self.hub.drop("sv1", "its worker token was revoked"))
            with self.assertRaises(W.WorkerError) as caught:
                self.hub.submit("sv1", "late", "m1", [{"role": "user", "content": "hi"}], None, {}, 30)
        self.assertLess(time.perf_counter() - t0, 1.0)
        self.assertEqual((caught.exception.refusal, caught.exception.retryable), ("dropped", False))
        self.assertIn("revoked", str(caught.exception))
        self.assertEqual(c.in_flight, 0)
        gone = self._connection("sv2")
        self.hub.register(gone)
        self.hub.unregister(gone)                                       # a disconnect: worth trying again later
        with self.assertRaises(W.WorkerError) as caught:
            gone.open_request("after")
        self.assertEqual((caught.exception.refusal, caught.exception.retryable), ("closed", True))

    def test_an_acknowledgement_is_held_to_what_the_site_asked(self):
        """PROTOCOL.md, "Settings": each number the smaller (a site 0 takes the helper's), the kinds both list, work on
        only when both say so; a claim of more is named."""
        site = {"llm": {"enabled": True, "slots": 2, "allow_swaps": False},
                "cpu": {"enabled": False, "threads": 4, "cpu_percent": 0, "memory_mb": 1024, "work": ["lab_game"],
                        "yield_to_llm": True}}
        ack = {"effective": {"llm": {"enabled": True, "slots": 8},
                             "cpu": {"enabled": True, "threads": 2, "cpu_percent": 30, "memory_mb": 4096,
                                     "work": ["lab_game", "reference_game"], "yield_to_llm": False}},
               "capped": [], "limits": {"cpu": "soft", "memory": "soft"}}
        clamped, over = W.clamp(site, ack)
        self.assertEqual(clamped, {"llm": {"enabled": True, "slots": 2},
                                   "cpu": {"enabled": False, "threads": 2, "cpu_percent": 30, "memory_mb": 1024,
                                           "work": ["lab_game"], "yield_to_llm": True}})
        self.assertEqual(over, ["llm.slots", "cpu.enabled", "cpu.memory_mb", "cpu.work"])
        honest = {"effective": clamped, "capped": [], "limits": ack["limits"]}
        self.assertEqual(W.clamp(site, honest), (clamped, []), "a correct helper's acknowledgement is unchanged")

    def test_a_helper_binary_named_but_missing_fails_rather_than_skips(self):
        """CI names the build-ext job's helper in ``CITAR_HELPER_BIN``: a wrong path or a lost artifact must fail the
        end-to-end tests, not skip them into a green run; on CI even no binary at all fails them."""
        import unittest as ut
        missing = str(_TMP / "no-such-helper.exe")
        with mock.patch.dict(os.environ, {"CITAR_HELPER_BIN": missing}):
            with self.assertRaises(H.NoHelperBinary):
                H.helper_binary()
        bare = {k: v for k, v in os.environ.items() if k not in ("CITAR_HELPER_BIN", "CITAR_EXT_DIR", "CI")}
        with mock.patch.dict(os.environ, bare, clear=True):
            self.assertIsNone(H.helper_binary())
            with self.assertRaises(ut.SkipTest):
                H.require_helper_binary("a test")
            with mock.patch.dict(os.environ, {"CI": "true"}):
                try:
                    H.require_helper_binary("a test")
                except H.NoHelperBinary:
                    pass
                except ut.SkipTest:
                    self.fail("on CI a missing helper skipped the tests instead of failing them")
                else:
                    self.fail("on CI a missing helper was not noticed")
        present = _TMP / "citar-helper-stand-in"
        present.write_bytes(b"")
        with mock.patch.dict(os.environ, {"CITAR_HELPER_BIN": str(present), "CI": "true"}):
            self.assertEqual(H.require_helper_binary("a test"), str(present))

    def test_a_helper_that_serves_no_models_is_refused_like_model_work_off(self):
        """A helper started with ``--no-llm`` on a machine whose site settings take model work: its tickets and its
        seats' requests are refused, not left waiting for a slot that never comes."""
        S.reset()
        self.addCleanup(S.reset)
        sid, _token, _tid = _machine(None, "no models")
        c = H.StubConnection(sid, [], slots=1)
        c.hello_llm = {"enabled": False, "slots": 0}
        W.hub().register(c)
        self.addCleanup(W.hub().unregister, c)
        sched = S.get()
        t = sched.ticket(sid, MODEL, cls="test")
        self.assertEqual((t.state, t.error.code), ("refused", "disabled"))
        self.assertIn("serves no models", str(t.error))


# ---------------------------------------------------------------------------- a real hub

class _Live(unittest.TestCase):
    """A real hub (the app under uvicorn) for the class, a fresh database, and signed-in accounts: lena (owner), gus
    (a member her group lends her machines to), sam (another member), mia (a moderator), ada (an administrator)."""

    WHO = (("lena", "user"), ("gus", "user"), ("sam", "user"), ("mia", "moderator"), ("ada", "admin"))

    @classmethod
    def setUpClass(cls):
        import httpx
        from citar import pool
        _reset()
        S.reset()
        cls.headers, cls.ids = {}, {}
        with db.session() as s:
            for handle, role in cls.WHO:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@x.cc", password="a long enough phrase",
                                            role=role, status="active", email_verified=True)
                row, raw = sessions.start(s, user)
                cls.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                       sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                cls.ids[handle] = user.id
            lena = s.get(User, cls.ids["lena"])
            group = pool.create_group(s, lena, name="Lena's", visibility="private")
            pool.create_grant(s, lena, group, subject_type="user", subject_handle="gus", purposes=["game"])
            cls.group_id = group.id
        cls.headers["anon"] = {"origin": "https://citar.test"}
        cls.server = H.HubServer()
        cls.http = httpx.Client(base_url=cls.server.base, timeout=20)

    @classmethod
    def tearDownClass(cls):
        cls.http.close()
        cls.server.stop()
        S.reset()

    def setUp(self):
        W.hub().limiter.clear()

    def call(self, who, method, path, body=None):
        return self.http.request(method, path, json=body, headers=self.headers[who])

    def connect(self, token: str, *, key: str = None, models=None, slots: int = 1, **kw) -> H.FakeHelper:
        """A fake helper, welcomed: its hello with ``token`` and ``key``."""
        helper = H.FakeHelper(self.server.url, **kw)
        self.addCleanup(helper.close)
        welcome = helper.handshake(H.hello(token=token, key=key or H.key_b64(), models=models or [H.model(MODEL)],
                                           slots=slots))
        sid = welcome["machine"]["id"]
        self.assertTrue(H.until(lambda: (W.hub().get(sid) or helper).ack is not None if W.hub().get(sid) else False),
                        "the acknowledgement reached the hub")
        return helper

    def refused(self, token: str, *, key: str = None, headers=None) -> tuple:
        """A hello that is not welcomed: (the frame the hub sent, the close)."""
        helper = H.FakeHelper(self.server.url, headers=headers)
        self.addCleanup(helper.close)
        helper.send("hello", **{k: v for k, v in H.hello(token=token, key=key or H.key_b64()).items() if k != "t"})
        frame = helper.recv()
        with self.assertRaises(H.Closed) as closed:
            helper.recv(timeout=5)
        return frame, closed.exception


class Cancel(_Live):
    """Gate 3 (P4.5.3)."""

    def setUp(self):
        super().setUp()
        S.reset()
        self.addCleanup(S.reset)

    def scheduled(self, sid: str, ticket, text: str = "go"):
        from citar.agents.providers import make_conversation
        conv = make_conversation({"provider": "worker", "server_id": sid, "model": MODEL, "timeout": 60},
                                 "You are a test.", [], binding=ticket)
        conv.add_user_text(text)
        return conv

    def stepping(self, conv) -> tuple:
        out: dict = {}

        def go():
            try:
                out["result"] = conv.step()
            except BaseException as exc:
                out["error"] = exc
            out["at"] = time.perf_counter()
        th = threading.Thread(target=go, daemon=True)
        th.start()
        self.addCleanup(th.join, 10)
        return th, out

    def test_an_acknowledged_cancel_frees_the_slot_without_draining(self):
        sid, token, _tid = _machine(self.ids["lena"], "acked box")
        helper = self.connect(token)
        served = H.Responder(helper, lambda f: None if "hold" in f["messages"][-1]["content"] else H.done_fields(f),
                             ack_delay=0.03)
        sched = S.get()
        m = sched.machine(sid)
        self.assertEqual(m.capacity(), 1)
        first = sched.ticket(sid, MODEL, cls="chat", label="held")
        conv_a = self.scheduled(sid, first, "hold this one")
        a, out_a = self.stepping(conv_a)
        self.assertTrue(H.until(lambda: served.held, 10), "the first request reached the helper")
        behind = sched.ticket(sid, MODEL, cls="test", label="behind")
        conv_b = self.scheduled(sid, behind, "answer this one")
        b, out_b = self.stepping(conv_b)
        self.assertTrue(H.until(lambda: m.waiting, 5), "the second waits behind the first")
        first.cancel()
        a.join(5)
        self.assertIsInstance(out_a.get("error"), Exception)
        cancelled_at = H.until(lambda: served.cancels and served.cancels[0][1], 5)
        self.assertTrue(H.until(lambda: len(served.requests) >= 2, 5), "the ticket behind it started")
        b.join(5)
        self.assertIn("result", out_b, out_b)
        # the second request reached the helper within 200 ms of the helper's acknowledgement (30 ms after the cancel)
        second_at = served.read_at[1]
        bound = 0.2 * (3 if CI else 1)
        self.assertLess(second_at - (cancelled_at + 0.03), bound, "the slot was free at the acknowledgement")
        self.assertEqual(m.counters["draining"], 0, "nothing drained")
        self.assertEqual(m.counters.get("cancel_unacked", 0), 0)
        with db.session() as s:
            self.assertNotIn("cancel_unacked", s.get(Server, sid).flags or {})

    def test_an_unacknowledged_cancel_drains_and_flags_the_machine(self):
        sid, token, _tid = _machine(self.ids["lena"], "silent box")
        helper = self.connect(token)
        served = H.Responder(helper, lambda f: None, ack_delay=None)
        sched = S.install(S.Scheduler())
        m = sched.machine(sid)
        ticket = sched.ticket(sid, MODEL, cls="chat", label="never acked")
        conv = self.scheduled(sid, ticket)
        th, out = self.stepping(conv)
        self.assertTrue(H.until(lambda: served.held, 10))
        ticket.cancel()
        th.join(5)
        self.assertTrue(H.until(lambda: served.cancels, 5), "the helper was sent cancel")
        self.assertEqual([s.cancelling for s in m.slots], [True], "the slot waits for the acknowledgement")
        self.assertTrue(H.until(lambda: [s.draining for s in m.slots] == [True], S.CANCEL_ACK_SECONDS + 5),
                        "then drains")
        self.assertEqual((m.counters["draining"], m.counters["cancel_unacked"]), (1, 1))

        def flagged():
            with db.session() as s:
                return (s.get(Server, sid).flags or {}).get("cancel_unacked")
        flag = H.until(flagged, 5)
        self.assertTrue(flag, "the machine is flagged")
        self.assertEqual(flag["count"], 1)
        served.release()                         # the helper's late answer frees it
        self.assertTrue(H.until(lambda: m.slots == [], 5))


class Keys(_Live):
    """Gate 4 (P4.3.7, P4.5.1)."""

    def test_the_first_hello_registers_its_key_and_another_is_refused_until_a_reset(self):
        sid, token, _tid = _machine(self.ids["lena"], "keyed box")
        mine, theirs = H.key_b64(1), H.key_b64(2)
        first = self.connect(token, key=mine)
        with db.session() as s:
            row = s.get(Server, sid)
            self.assertEqual(row.public_key, mine, "the first hello registered its key")
            self.assertIsNotNone(row.key_at)
            row.trust_tier = "quarantined"            # a reset never changes the trust (P4.5.8)
        first.close()
        H.until(lambda: not W.hub().is_online(sid), 5)
        again = self.connect(token, key=mine)
        again.close()
        H.until(lambda: not W.hub().is_online(sid), 5)
        frame, closed = self.refused(token, key=theirs)
        self.assertEqual((frame["t"], frame["code"], closed.code), ("refused", "key", 4401))
        self.assertIn("reset its key", frame["message"])
        # the owner resets it: one audit row, the trust as it was
        r = self.call("lena", "POST", f"/api/machines/{sid}/key/reset")
        self.assertEqual(r.status_code, 200, r.text)
        with db.session() as s:
            row = s.get(Server, sid)
            self.assertIsNone(row.public_key)
            self.assertEqual(row.trust_tier, "quarantined")
            rows = s.query(AuditLog).filter(AuditLog.action == "server.key_reset", AuditLog.object_id == sid).all()
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0].actor_id, self.ids["lena"])
        self.connect(token, key=theirs)
        with db.session() as s:
            self.assertEqual(s.get(Server, sid).public_key, theirs, "the next hello registered its own")

    def test_a_reset_while_connected_makes_the_helper_reconnect_and_register_its_key(self):
        """A reset leaves no connection serving under the key the row no longer holds: the hub closes it with 1012 and
        no ``shutdown`` (a reconnect, not a refusal), and the helper's next hello registers its key at once."""
        sid, token, _tid = _machine(self.ids["lena"], "reset while connected")
        old, new = H.key_b64(11), H.key_b64(12)
        helper = self.connect(token, key=old)
        r = self.call("lena", "POST", f"/api/machines/{sid}/key/reset")
        self.assertEqual(r.status_code, 200, r.text)
        with self.assertRaises(H.Closed) as closed:
            frame = helper.recv(timeout=5)
            self.fail(f"a frame came before the close: {frame}")
        self.assertEqual((closed.exception.code, closed.exception.reason), (HP.CLOSE_RECONNECT, "key_reset"))
        self.assertFalse(W.hub().is_online(sid), "the old connection is gone")
        self.connect(token, key=new)
        with db.session() as s:
            self.assertEqual(s.get(Server, sid).public_key, new, "the reconnecting helper registered its key")

    def test_the_card_shows_the_protocol_the_version_and_the_keys_fingerprint(self):
        sid, token, _tid = _machine(self.ids["lena"], "card box")
        key = H.key_b64(5)
        self.connect(token, key=key)
        listed = self.call("lena", "GET", f"/api/pool/servers/{sid}/workers").json()
        worker = listed["worker"]
        from citar import engine_api
        self.assertEqual((worker["protocol"], worker["version"]), (2, "0.1.6"))
        self.assertEqual(worker["key_fingerprint"], engine_api.key_fingerprint(key))
        self.assertEqual(len(worker["key_fingerprint"]), 16)
        self.assertTrue(listed["command"].startswith("citar-helper "), listed["command"])

    def test_the_key_routes_access(self):
        """P3.9.1: the owner and an administrator 200; gus (whom her group lends it to) 403; sam (another member) and
        mia (a moderator) 404; a visitor 401. The rescan of an offline machine is 409."""
        sid, token, _tid = _machine(self.ids["lena"], "routes box")
        with db.session() as s:
            s.get(Server, sid).group_id = self.group_id
        servers.invalidate()
        for path in (f"/api/machines/{sid}/key/reset", f"/api/machines/{sid}/rescan"):
            for who, want in (("anon", 401), ("sam", 404), ("mia", 404), ("gus", 403)):
                with self.subTest(path=path, who=who):
                    self.assertEqual(self.call(who, "POST", path).status_code, want)
        self.assertEqual(self.call("lena", "POST", f"/api/machines/{sid}/key/reset").status_code, 200)
        self.assertEqual(self.call("ada", "POST", f"/api/machines/{sid}/key/reset").status_code, 200)
        self.assertEqual(self.call("lena", "POST", f"/api/machines/{sid}/rescan").status_code, 409)
        helper = self.connect(token)
        for who in ("lena", "ada"):
            with self.subTest(who=who):
                self.assertEqual(self.call(who, "POST", f"/api/machines/{sid}/rescan").status_code, 200)
                self.assertEqual(helper.recv(timeout=5)["t"], "rescan", "its helper was asked to scan")
        for who in ("sam", "mia", "gus"):
            self.assertNotEqual(self.call(who, "POST", f"/api/machines/{sid}/key/reset").status_code, 200)
        with db.session() as s:
            n = s.query(AuditLog).filter(AuditLog.action == "server.key_reset", AuditLog.object_id == sid).count()
        self.assertEqual(n, 2, "only the owner's and the administrator's resets happened")


class RateLimit(_Live):
    """Gate 5 (P4.5.1; P4.0's defects 1 and 2)."""

    def test_twenty_refused_tokens_then_a_good_one_is_refused_before_any_lookup(self):
        _sid, token, _tid = _machine(self.ids["lena"], "limited box")
        for i in range(20):
            frame, closed = self.refused(f"cwt_wrong_{i}")
            self.assertEqual((frame["code"], closed.code), ("token", 4401))
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 20)
        with mock.patch.object(W, "authenticate", wraps=W.authenticate) as looked_up:
            frame, closed = self.refused(token)
        self.assertEqual((frame["code"], closed.code), ("rate", 4429))
        self.assertGreater(frame["retry_after_s"], 0)
        looked_up.assert_not_called()

    def test_hellos_sent_at_once_get_no_more_lookups_than_the_limit(self):
        """The check, the lookup and the count of its failure are one step per address: 40 hellos with distinct bad
        tokens sent together make at most 20 lookups (and audit rows), and the rest are refused ``rate`` (4429)."""
        n = 40
        helpers = [H.FakeHelper(self.server.url) for _ in range(n)]
        for h in helpers:
            self.addCleanup(h.close)
        lookups0 = W.hub().lookups
        with db.session() as s:
            audit0 = s.query(AuditLog).filter(AuditLog.action == "worker.auth_failed").count()
        barrier = threading.Barrier(n)
        answers: list = []
        lock = threading.Lock()

        def hello(i, h):
            barrier.wait()
            h.send("hello", **{k: v for k, v in H.hello(token=f"cwt_together_{i}").items() if k != "t"})
            frame = h.recv(timeout=30)
            with self.assertRaises(H.Closed) as closed:
                h.recv(timeout=10)
            with lock:
                answers.append((frame["code"], closed.exception.code))
        threads = [threading.Thread(target=hello, args=(i, h)) for i, h in enumerate(helpers)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(60)
        self.assertEqual(len(answers), n, "every hello was answered")
        self.assertEqual(W.hub().lookups - lookups0, 20, "no more lookups than the limit")
        self.assertEqual(sorted(set(answers)), [("rate", 4429), ("token", 4401)])
        self.assertEqual(answers.count(("token", 4401)), 20)
        with db.session() as s:
            audited = s.query(AuditLog).filter(AuditLog.action == "worker.auth_failed").count() - audit0
        self.assertEqual(audited, 20, "one audit row a lookup that failed, no more")
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 20)
        self.assertEqual(W.hub()._gates, {}, "no gate is left behind")

    def test_one_revoked_token_retried_counts_once_and_a_neighbour_connects(self):
        from datetime import datetime, timezone
        _sid, revoked, tid = _machine(self.ids["lena"], "revoked box")
        with db.session() as s:
            s.get(WorkerToken, tid).revoked_at = datetime.now(timezone.utc)
        for _ in range(40):
            frame, _closed = self.refused(revoked)
            self.assertEqual(frame["code"], "token")
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 1, "one token, however often it is retried")
        other_sid, good, _ = _machine(self.ids["lena"], "neighbour box")
        self.connect(good)
        self.assertTrue(W.hub().is_online(other_sid), "another machine behind the same address connects")

    def test_a_forged_forwarded_for_changes_nothing_without_a_proxy(self):
        self.assertFalse(settings.get().behind_proxy)
        frame, _closed = self.refused("cwt_forged", headers={"X-Forwarded-For": "10.9.9.9"})
        self.assertEqual(frame["code"], "token")
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 1, "counted against the socket's own address")
        self.assertEqual(W.hub().limiter.count("10.9.9.9"), 0)
        # the control: behind a trusted proxy the forwarded address is the one counted
        import dataclasses
        trusted = dataclasses.replace(settings.get(), behind_proxy=True, trusted_proxy_hops=1)
        with mock.patch("citar.auth.ratelimit.settings.get", return_value=trusted):
            self.refused("cwt_proxied", headers={"X-Forwarded-For": "10.9.9.9"})
        self.assertEqual(W.hub().limiter.count("10.9.9.9"), 1)


class Refusal(_Live):
    """Gate 6 (P4.2.8)."""

    def hellos(self) -> dict:
        """The three hellos the transcripts send that are not protocol 2."""
        out = {}
        for t in F.transcripts():
            if t.name.startswith("handshake-v1-refusal-") and t.plays.get("hub"):
                out[t.name] = t.steps[0].body
        self.assertEqual(len(out), 3)
        return out

    def test_a_hello_that_is_not_protocol_2_gets_the_v1_refusal_and_costs_nothing(self):
        import json
        for name, hello in self.hellos().items():
            with self.subTest(name):
                before_lookups = W.hub().lookups
                with db.session() as s:
                    before_audit = s.query(AuditLog).count()
                helper = H.FakeHelper(self.server.url)
                self.addCleanup(helper.close)
                helper.send_text(json.dumps(hello))
                frame = helper.recv()
                self.assertEqual(frame, {"t": "error", "ts": frame["ts"], "id": "", "message": HP.V1_REFUSAL_MESSAGE})
                with self.assertRaises(H.Closed) as closed:
                    helper.recv(timeout=5)
                self.assertEqual(closed.exception.code, 4426)
                self.assertEqual(W.hub().lookups, before_lookups, "no token lookup")
                with db.session() as s:
                    self.assertEqual(s.query(AuditLog).count(), before_audit, "no audit row")
                self.assertEqual(W.hub().limiter.count(H.ADDRESS), 0, "no rate-limit count")

    def test_a_hundred_refusals_from_one_address_log_one_line(self):
        import json
        hello = self.hellos()["handshake-v1-refusal-v015"]
        W.hub()._v1_logged.clear()
        with self.assertLogs("citar.workers", level="WARNING") as logged:
            for _ in range(100):
                helper = H.FakeHelper(self.server.url)
                helper.send_text(json.dumps(hello))
                self.assertEqual(helper.recv()["t"], "error")
                helper.close()
        lines = [line for line in logged.output if "protocol 1" in line]
        self.assertEqual(len(lines), 1, lines)


class Revocation(_Live):
    """Package 3-05's gate 4 on protocol 2: revoking the helper's token, disabling its machine, deleting it or
    suspending its owner closes its socket within a second, with ``shutdown`` and its code; its next request is
    refused, a request it was serving fails as not retryable, and a hello with what was taken away is refused."""

    def ends_within(self, helper: H.FakeHelper, t0: float, code: str, limit: float = 1.0) -> float:
        frame = helper.recv(timeout=limit + 2)
        self.assertEqual((frame["t"], frame["code"]), ("shutdown", code))
        with self.assertRaises(H.Closed) as closed:
            helper.recv(timeout=limit + 2)
        self.assertEqual(closed.exception.code, 4403)
        took = time.perf_counter() - t0
        self.assertLessEqual(took, limit, f"dropped after {took:.2f} s")
        return took

    def refused_now(self, sid: str) -> None:
        with self.assertRaises(W.WorkerError) as caught:
            W.hub().submit(sid, "after", MODEL, [{"role": "user", "content": "hi"}], None, {}, 5)
        self.assertEqual(caught.exception.refusal, "closed")

    def test_revoking_the_token_drops_its_helper(self):
        sid, token, tid = _machine(self.ids["lena"], "revoked box")
        helper = self.connect(token)
        t0 = time.perf_counter()
        r = self.call("lena", "DELETE", f"/api/pool/servers/{sid}/workers/tokens/{tid}")
        self.assertEqual(r.status_code, 200, r.text)
        self.ends_within(helper, t0, "revoked")
        self.assertFalse(W.hub().is_online(sid))
        self.refused_now(sid)
        frame, closed = self.refused(token)
        self.assertEqual((frame["code"], closed.code), ("token", 4401), "a hello with the revoked token is refused")

    def test_a_request_in_flight_fails_and_is_not_retried(self):
        sid, token, tid = _machine(self.ids["lena"], "busy box")
        helper = self.connect(token)
        served = H.Responder(helper, lambda f: None)
        outcome = {}

        def caller():
            try:
                W.hub().submit(sid, "slow-1", MODEL, [{"role": "user", "content": "hi"}], None, {}, 20)
            except W.WorkerError as exc:
                outcome["error"] = exc
        t = threading.Thread(target=caller)
        t.start()
        self.assertTrue(H.until(lambda: served.held, 10))
        t0 = time.perf_counter()
        self.assertEqual(self.call("lena", "DELETE", f"/api/pool/servers/{sid}/workers/tokens/{tid}").status_code, 200)
        t.join(timeout=5)
        self.assertFalse(t.is_alive(), "the waiting request is released, not left to its timeout")
        self.assertLess(time.perf_counter() - t0, 2.0)
        self.assertFalse(outcome["error"].retryable)
        self.assertEqual(outcome["error"].refusal, "dropped")

    def test_disabling_the_machine_drops_its_helper(self):
        sid, token, _tid = _machine(self.ids["lena"], "disabled box")
        helper = self.connect(token)
        t0 = time.perf_counter()
        r = self.call("lena", "PUT", f"/api/pool/servers/{sid}", {"enabled": False})
        self.assertEqual(r.status_code, 200, r.text)
        self.ends_within(helper, t0, "disabled")
        self.refused_now(sid)
        self.assertEqual(W.authenticate(token, H.key_b64()).refusal, "token", "a disabled machine's helper is refused")
        self.assertEqual(self.call("lena", "PUT", f"/api/pool/servers/{sid}", {"enabled": True}).status_code, 200)
        self.assertTrue(H.until(lambda: W.authenticate(token, H.key_b64()).refusal == "", 3),
                        "and let in again once it is enabled")

    def test_deleting_the_machine_drops_its_helper_and_leaves_no_connection(self):
        sid, token, _tid = _machine(self.ids["lena"], "deleted box")
        helper = self.connect(token)
        t0 = time.perf_counter()
        r = self.call("lena", "DELETE", f"/api/pool/servers/{sid}")
        self.assertEqual(r.status_code, 200, r.text)
        self.ends_within(helper, t0, "deleted")
        self.assertNotIn(sid, W.hub()._workers, "no connection is left")
        self.refused_now(sid)
        with db.session() as s:
            self.assertIsNone(s.get(Server, sid))
            self.assertEqual(s.query(WorkerToken).filter(WorkerToken.server_id == sid).count(), 0)

    def test_deleting_it_through_the_registry_routes_drops_it_too(self):
        sid, token, _tid = _machine(self.ids["lena"], "deleted through the registry")
        helper = self.connect(token)
        t0 = time.perf_counter()
        self.assertEqual(self.call("lena", "DELETE", f"/api/servers/{sid}").status_code, 200)
        self.ends_within(helper, t0, "deleted")

    def test_suspending_the_owner_drops_their_helpers(self):
        sid, token, _tid = _machine(self.ids["sam"], "sam's box")
        helper = self.connect(token)
        t0 = time.perf_counter()
        r = self.call("ada", "PATCH", f"/api/admin/users/{self.ids['sam']}", {"status": "suspended", "reason": "tests"})
        self.assertEqual(r.status_code, 200, r.text)
        self.ends_within(helper, t0, "suspended")
        self.assertEqual(W.authenticate(token, H.key_b64()).refusal, "token")
        self.assertEqual(self.call("ada", "PATCH", f"/api/admin/users/{self.ids['sam']}", {"status": "active"}).status_code,
                         200)
        self.assertTrue(H.until(lambda: W.authenticate(token, H.key_b64()).refusal == "", 3))

    def test_a_change_made_elsewhere_drops_it_within_a_ping(self):
        """``admin_cli`` or another process revokes a token in the database, where no route can drop the helper: the
        keep-alive (here every second) finds it at its next round and drops it."""
        from datetime import datetime, timezone
        sid, token, tid = _machine(self.ids["lena"], "revoked elsewhere")
        helper = self.connect(token)
        self.assertEqual(W.hub().get(sid).token_id, tid)
        before = W.hub().ping_s
        W.hub().ping_s = 1
        W.hub().poke()
        self.addCleanup(setattr, W.hub(), "ping_s", before)
        t0 = time.perf_counter()
        with db.session() as s:
            s.get(WorkerToken, tid).revoked_at = datetime.now(timezone.utc)
        self.ends_within(helper, t0, "revoked", limit=3.0)

    def test_a_revoke_between_the_hello_and_its_registration_still_drops_it(self):
        from datetime import datetime, timezone
        sid, token, tid = _machine(self.ids["lena"], "revoked mid-hello")
        real = W.authenticate
        missed = []

        def checked_then_revoked(raw, key, address=""):
            identity = real(raw, key, address)
            if not identity.refusal:
                with db.session() as s:
                    s.get(WorkerToken, tid).revoked_at = datetime.now(timezone.utc)
                missed.append(W.hub().drop(sid, "its worker token was revoked", token_id=tid))
            return identity
        with mock.patch.object(W, "authenticate", checked_then_revoked):
            helper = H.FakeHelper(self.server.url)
            self.addCleanup(helper.close)
            helper.send("hello", **{k: v for k, v in H.hello(token=token).items() if k != "t"})
            frame = helper.recv()
            with self.assertRaises(H.Closed) as closed:
                helper.recv(timeout=5)
        self.assertEqual(missed, [False], "the revoke's own drop came too early to find the connection")
        self.assertEqual((frame["t"], frame["code"], closed.exception.code), ("shutdown", "revoked", 4403))
        self.assertFalse(W.hub().is_online(sid))
        self.assertNotIn(sid, W.hub()._workers)

    def test_a_drop_names_its_token(self):
        sid, token, tid = _machine(self.ids["lena"], "two tokens")
        raw, hashed = tokens.new_pair()
        with db.session() as s:
            other = WorkerToken(server_id=sid, token_hash=hashed, prefix=tokens.prefix(raw), label="other")
            s.add(other)
            s.flush()
            other_id = other.id
        self.connect(token)
        self.assertEqual(self.call("lena", "DELETE", f"/api/pool/servers/{sid}/workers/tokens/{other_id}").status_code,
                         200)
        time.sleep(0.3)
        self.assertTrue(W.hub().is_online(sid), "its own token still stands")
        self.assertEqual(self.call("lena", "DELETE", f"/api/pool/servers/{sid}/workers/tokens/{tid}").status_code, 200)
        self.assertTrue(H.until(lambda: not W.hub().is_online(sid), 2))

    def test_last_ip_and_last_seen_are_written(self):
        """P4.0's defect 3: a token's last address was never written."""
        sid, token, tid = _machine(self.ids["lena"], "seen box")
        self.connect(token)
        with db.session() as s:
            row = s.get(WorkerToken, tid)
            self.assertEqual(row.last_ip, H.ADDRESS)
            self.assertIsNotNone(row.last_seen_at)


class Overhead(_Live):
    """Gate 10, P4.7.9: the hub's own time per request and ``done`` round trip."""

    def test_a_thousand_round_trips(self):
        sid, token, _tid = _machine(self.ids["lena"], "fast box", slots=4)
        helper = self.connect(token, slots=4)
        served = H.Responder(helper)
        messages = [{"role": "system", "content": "You are a test."}, {"role": "user", "content": "go"}]
        hub = W.hub()
        for i in range(50):                                     # warm up
            hub.submit(sid, f"warm{i}", MODEL, messages, None, {}, 30)
        own = []
        for i in range(1000):
            rid = f"r{i:04d}"
            t0 = time.perf_counter()
            hub.submit(sid, rid, MODEL, messages, None, {}, 30)
            observed = time.perf_counter() - t0
            own.append(observed - served.spent.get(rid, 0.0))
        median = statistics.median(own)
        print(f"\nthe hub's own time per round trip: median {median * 1000:.3f} ms, p95 "
              f"{sorted(own)[int(len(own) * 0.95)] * 1000:.3f} ms over {len(own)}")
        self.assertLess(median, 0.003 * (3 if CI else 1))


class Settings(_Live):
    """The site's settings reach a connected helper whichever way they change, and its acknowledgements are held to
    what each settings frame asked (4-05's fix round)."""

    def setUp(self):
        super().setUp()
        S.reset()
        self.addCleanup(S.reset)

    def served(self, token: str, ceiling: int = 4) -> H.Responder:
        """A fake helper whose own ceiling is ``ceiling`` slots, welcomed, acknowledging every settings frame."""
        return H.Responder(self.connect(token, slots=ceiling), slots=ceiling)

    def test_a_slot_change_in_the_machine_editor_reaches_its_helper(self):
        """The machine editor ("Requests at once": ``PUT /api/pool/servers/{id}``, and the registry's own
        ``POST /api/servers``) changes the slots; the helper is sent them and the scheduler's capacity follows at once,
        not when the helper next reconnects."""
        sid, token, _tid = _machine(self.ids["lena"], "editor box", slots=1)
        served = self.served(token)
        m = S.get().machine(sid)
        self.assertEqual(m.capacity(), 1)
        view = self.call("lena", "GET", f"/api/pool/servers/{sid}").json()
        cfg = dict(view.get("config") or {})
        cfg["connection"] = dict(cfg.get("connection") or {}, provider="lmstudio", max_parallel=3)
        cfg.setdefault("name", "editor box")
        r = self.call("lena", "PUT", f"/api/pool/servers/{sid}", {"config": cfg})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertTrue(H.until(lambda: [f["llm"]["slots"] for f in served.settings] == [3], 5),
                        f"the helper was sent the new slots: {served.settings}")
        self.assertTrue(H.until(lambda: m.capacity() == 3, 5), f"the capacity is {m.capacity()}")
        sv = servers.get(sid)
        sv["connection"] = dict(sv["connection"], max_parallel=2)
        r = self.call("lena", "POST", "/api/servers", sv)
        self.assertEqual(r.status_code, 200, r.text)
        self.assertTrue(H.until(lambda: [f["llm"]["slots"] for f in served.settings] == [3, 2], 5),
                        f"the registry's editor reached it too: {served.settings}")
        self.assertTrue(H.until(lambda: m.capacity() == 2, 5), f"the capacity is {m.capacity()}")

    def test_a_change_made_elsewhere_is_pushed_within_a_ping(self):
        """``admin_cli`` or another process changes the row where no route can push: the keep-alive finds the
        difference at its next round and pushes it."""
        sid, token, _tid = _machine(self.ids["lena"], "changed elsewhere", slots=1)
        served = self.served(token)
        before = W.hub().ping_s
        W.hub().ping_s = 1
        W.hub().poke()
        self.addCleanup(setattr, W.hub(), "ping_s", before)
        with db.session() as s:
            s.get(Server, sid).max_concurrent = 3
        servers.invalidate()                    # another process's change: this one learns of it at its cache's age
        self.assertTrue(H.until(lambda: [f["llm"]["slots"] for f in served.settings] == [3], 5),
                        f"pushed by the keep-alive: {served.settings}")
        self.assertTrue(H.until(lambda: W.hub().get(sid).llm_slots == 3, 5))

    def test_a_change_during_the_registration_follows_the_welcome(self):
        """The settings are read before the connection is registered: a change committed in between found no
        connection to push to, so the hub looks again once it is registered and sends it right after the welcome."""
        sid, token, _tid = _machine(self.ids["lena"], "changed mid-hello", slots=1)
        real = W.site_settings
        calls: list = []

        def read_then_change(server_id):
            out = real(server_id)
            if not calls:
                with db.session() as s:
                    s.get(Server, server_id).max_concurrent = 3
                servers.invalidate()
            calls.append(out["llm"]["slots"])
            return out
        with mock.patch.object(W, "site_settings", read_then_change):
            helper = H.FakeHelper(self.server.url)
            self.addCleanup(helper.close)
            welcome = helper.handshake(H.hello(token=token, models=[H.model(MODEL)], slots=4))
            self.assertEqual(welcome["settings"]["llm"]["slots"], 1, "the welcome carries what was read")
            pushed = helper.recv(timeout=5)
        self.assertEqual((pushed["t"], pushed["llm"]["slots"]), ("settings", 3), "the change follows the welcome")
        self.assertEqual(calls[:2], [1, 3])

    def test_an_acknowledgement_is_held_to_the_settings_frame_it_answers(self):
        """The welcome asks 3 slots; the owner lowers them to 1 before the helper's acknowledgement of the welcome
        arrives. That acknowledgement (3) crossed the push: it is no overclaim, and the capacity is held to the site's
        1 meanwhile. One that claims more than its own frame asked is still flagged."""
        sid, token, _tid = _machine(self.ids["lena"], "crossing box", slots=3)
        helper = H.FakeHelper(self.server.url)
        self.addCleanup(helper.close)
        helper.send("hello", **{k: v for k, v in H.hello(token=token, models=[H.model(MODEL)], slots=4).items()
                                if k != "t"})
        welcome = helper.recv()
        self.assertEqual(welcome["settings"]["llm"]["slots"], 3)
        with db.session() as s:
            s.get(Server, sid).max_concurrent = 1
        servers.invalidate()
        self.assertFalse(W.hub().push_settings(sid), "nothing acknowledged yet")
        pushed = helper.recv()
        self.assertEqual((pushed["t"], pushed["llm"]["slots"]), ("settings", 1))

        def ack(slots: int) -> None:
            site = welcome["settings"]
            helper.send("settings_ack", effective={"llm": {"enabled": True, "slots": slots},
                                                   "cpu": {**site["cpu"], "enabled": False}},
                        capped=[], limits={"cpu": "soft", "memory": "soft"})
        ack(3)                                  # the welcome's, correctly
        self.assertTrue(H.until(lambda: W.hub().get(sid).ack is not None, 5))
        self.assertEqual(W.hub().get(sid).llm_slots, 1, "held to what the site asks now")
        ack(1)                                  # the push's, correctly
        self.assertTrue(H.until(lambda: W.hub().push_settings(sid), 5), "every frame acknowledged")
        time.sleep(0.3)
        with db.session() as s:
            self.assertNotIn("settings_overclaim", s.get(Server, sid).flags or {}, "an honest helper is not flagged")
        with db.session() as s:
            s.get(Server, sid).max_concurrent = 2
        servers.invalidate()
        W.hub().push_settings(sid)
        self.assertEqual(helper.recv()["llm"]["slots"], 2)
        ack(4)                                  # more than this frame asked

        def flagged():
            with db.session() as s:
                return (s.get(Server, sid).flags or {}).get("settings_overclaim")
        self.assertEqual(H.until(flagged, 5)["fields"], ["llm.slots"])
        self.assertEqual(W.hub().get(sid).llm_slots, 2, "held to the site's")

    def test_a_change_that_changes_nothing_waits_for_nothing_and_one_request_a_machine_waits(self):
        """The settings route waits for the acknowledgement only when something changed, and only one request a
        machine waits: a helper that never acknowledges cannot hold the server's request threads."""
        sid, token, _tid = _machine(self.ids["lena"], "silent settings", slots=1)
        self.connect(token, slots=4)            # acknowledges its welcome, then reads nothing more
        path = f"/api/machines/{sid}/settings"
        t0 = time.perf_counter()
        self.assertEqual(self.call("lena", "PUT", path, {}).status_code, 200)
        self.assertEqual(self.call("lena", "PUT", path, {"slots": 1}).status_code, 200)
        self.assertLess(time.perf_counter() - t0, 1.0, "nothing changed: nothing to wait for")
        took: list = []
        barrier = threading.Barrier(4)

        def change(slots: int) -> None:
            import httpx
            with httpx.Client(base_url=self.server.base, timeout=20) as client:
                barrier.wait()
                started = time.perf_counter()
                r = client.put(path, json={"slots": slots}, headers=self.headers["lena"])
                took.append((r.status_code, time.perf_counter() - started))
        threads = [threading.Thread(target=change, args=(n,)) for n in (2, 3, 4, 5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(30)
        self.assertEqual([c for c, _ in took], [200] * 4)
        waited = [s for _, s in took if s >= 1.5]
        self.assertLessEqual(len(waited), 1, f"one request at a time waits: {took}")
        self.assertEqual(len(waited), 1, f"the one that waited waited for its acknowledgement: {took}")


class Frames(_Live):
    """A frame is never parsed on the event loop when that would hold it (P4.7.4; 4-05's fix round): a first frame,
    which anyone can send before any token is checked, over 2 MiB is refused unread, and a long one is parsed on a
    thread; so is a welcomed helper's long frame."""

    @staticmethod
    def stalls(during) -> float:
        """The longest the hub's event loop went without running a callback while ``during()`` ran (a chain of
        ``call_soon`` callbacks on it: exact, not rounded to the system's timer)."""
        loop = W.hub()._loop
        state = {"last": None, "stop": False, "gaps": []}

        def tick():
            now = time.perf_counter()
            if state["last"] is not None:
                state["gaps"].append(now - state["last"])
            state["last"] = now
            if not state["stop"]:
                loop.call_soon(tick)
        loop.call_soon_threadsafe(tick)
        time.sleep(0.05)
        try:
            during()
        finally:
            state["stop"] = True
        time.sleep(0.05)
        return max(state["gaps"])

    @staticmethod
    def parse_seconds(raw: str) -> float:
        """What parsing ``raw`` costs on this machine (what it would hold the loop for, parsed there)."""
        t0 = time.perf_counter()
        try:
            HP.parse_helper_frame(raw)
        except HP.ProtocolError:
            pass
        return time.perf_counter() - t0

    def refused_unread(self, raw: str) -> H.Closed:
        """Send ``raw`` as a first frame; the close it got (after the v1 refusal's frame, for a hello that names no
        protocol)."""
        helper = H.FakeHelper(self.server.url)
        self.addCleanup(helper.close)
        helper.send_text(raw)
        deadline = time.perf_counter() + 20
        while time.perf_counter() < deadline:
            try:
                helper.recv(timeout=max(0.1, deadline - time.perf_counter()))
            except H.Closed as closed:
                return closed
        self.fail("the hub did not close the connection")

    def test_a_first_frame_over_two_mib_is_refused_unread(self):
        lookups = W.hub().lookups
        big = H.hello(token="cwt_big")
        big["report"] = {"hardware": {"format": "citar-hardware", "pad": "x" * HP.MAX_HELLO_BYTES},
                         "hardware_source": "file"}
        import json
        raw = json.dumps(big)
        with mock.patch.object(HP, "parse_helper_frame", wraps=HP.parse_helper_frame) as parsed:
            closed = self.refused_unread(raw)
        self.assertEqual((closed.code, closed.reason), (4400, "too_large"))
        parsed.assert_not_called()
        self.assertEqual(W.hub().lookups, lookups, "no lookup")
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 0, "no count")
        # just under it, a hello is read as any other
        big["report"]["hardware"]["pad"] = "x" * (HP.MAX_HELLO_BYTES - 4096)
        frame, closed = None, None
        helper = H.FakeHelper(self.server.url)
        self.addCleanup(helper.close)
        helper.send_text(json.dumps(big))
        frame = helper.recv(timeout=20)
        self.assertEqual((frame["t"], frame["code"]), ("refused", "token"), "read, and refused for its token")

    def test_a_long_frame_is_parsed_off_the_event_loop(self):
        """Text that is not JSON and a hello of millions of tiny values are the parse's worst shapes (about 0.1 s a MiB
        here). Parsed on the loop they held every other connection for the whole parse; parsed on a thread the loop
        waits at most a handoff of the interpreter's lock. The bound is a third of the parse's own time, measured in
        the same run, and never under 50 ms (a thread switch on Windows can take 16 ms, and the fake helper's own
        sending shares this process); on CI three times that (p3-notes.md), still under the parse's own time."""
        n = HP.MAX_HELLO_BYTES - 64 * 1024
        shapes = {
            "not JSON (the depth scan)": "[" * 60 + "0," * ((n - 120) // 2) + "}",
            "millions of zeros (the walk)": '{"t":"hello","x":[' + "0," * ((n - 40) // 2) + "0]}",
            "eight MiB, over the hello's cap": "[" * 60 + "0," * ((8 * 1024 * 1024 - 200) // 2) + "}",
        }
        # the server's first connections pay one-time costs on the loop (about 40 ms here): not what is measured
        for _ in range(3):
            self.refused_unread('{"t": "ping"}')
        for name, raw in shapes.items():
            with self.subTest(name):
                bound = max(0.05, self.parse_seconds(raw) / 3) * (3 if CI else 1)
                closed: list = []
                longest = self.stalls(functools.partial(lambda text, out: out.append(self.refused_unread(text)),
                                                        raw, closed))
                self.assertIn(closed[0].code, (4400, 4426))
                print(f"\n{name}: the loop's longest stall {longest * 1000:.1f} ms (bound {bound * 1000:.0f} ms)")
                self.assertLess(longest, bound)
        # a welcomed helper's long bad frame: parsed on a thread too, and only logged
        sid, token, _tid = _machine(self.ids["lena"], "long frames")
        helper = self.connect(token)
        raw = "[" * 60 + "0," * ((8 * 1024 * 1024 - 200) // 2) + "}"
        bound = max(0.05, self.parse_seconds(raw) / 3) * (3 if CI else 1)

        def send_and_settle():
            helper.send_text(raw)
            helper.send("ping")
            self.assertEqual(helper.recv(timeout=20, skip=())["t"], "pong", "the connection carries on")
        longest = self.stalls(send_and_settle)
        print(f"a welcomed helper's 8 MiB bad frame: the loop's longest stall {longest * 1000:.1f} ms "
              f"(bound {bound * 1000:.0f} ms)")
        self.assertLess(longest, bound)
        self.assertTrue(W.hub().is_online(sid))

    def test_waiting_for_a_hello_does_not_poll(self):
        """A connection that sends nothing costs nothing until its 30 s are up: the wait sleeps on the frame and on
        the hub's clock being moved, rather than looking at the clock every 50 ms."""
        import asyncio
        hub = W.WorkerHub()
        looks = []

        class Counting(W.Clock):
            def time(self):
                looks.append(1)
                return super().time()
        hub.clock = Counting()

        class Silent:
            async def receive_text(self):
                await asyncio.sleep(3600)
        with mock.patch.object(W, "HELLO_SECONDS", 0.6):
            got = asyncio.run(W._first_frame(Silent(), hub))
        self.assertIsNone(got)
        self.assertLessEqual(len(looks), 5, f"the clock was read {len(looks)} times in 0.6 s (polling: about 13)")


class Tokens(_Live):
    """What stays true of a worker token and of the lookup (``test_worker.py``'s ``Authentication``, on protocol 2),
    and the lookup's failures (4-05's fix round)."""

    def test_only_the_hash_is_stored(self):
        """A leaked database backup must not yield a usable worker credential."""
        sid, _token, _tid = _machine(self.ids["lena"], "hashed box")
        r = self.call("lena", "POST", f"/api/pool/servers/{sid}/workers/tokens", {"label": "issued"})
        self.assertEqual(r.status_code, 200, r.text)
        raw, token_id = r.json()["token"], r.json()["id"]
        with db.session() as s:
            row = s.get(WorkerToken, token_id)
            self.assertNotEqual(row.token_hash, raw)
            self.assertNotIn(raw, row.token_hash)
            self.assertEqual(row.token_hash, tokens.hash_token(raw))
        found = W.authenticate(raw, H.key_b64(21))
        self.assertEqual((found.refusal, found.server_id, found.token_id), ("", sid, token_id))
        self.assertEqual(W.authenticate("cwt_not_a_real_token", H.key_b64(21)).refusal, "token")
        self.assertEqual(W.authenticate("", H.key_b64(21)).refusal, "token")

    def test_a_database_that_fails_refuses_nothing(self):
        """A failing lookup is the server's fault: the helper is closed with 1011 and no ``refused`` (which it would
        take as permanent), and nothing is counted against its address or audited."""
        from sqlalchemy.exc import OperationalError
        _sid, token, _tid = _machine(self.ids["lena"], "locked box")
        with db.session() as s:
            audit0 = s.query(AuditLog).count()

        def locked():
            raise OperationalError("SELECT", {}, Exception("database is locked"))
        with mock.patch.object(db, "session", side_effect=locked), \
                self.assertLogs("citar.workers", level="ERROR"):
            helper = H.FakeHelper(self.server.url)
            self.addCleanup(helper.close)
            helper.send("hello", **{k: v for k, v in H.hello(token=token).items() if k != "t"})
            with self.assertRaises(H.Closed) as closed:
                frame = helper.recv(timeout=10)
                self.fail(f"a frame came before the close: {frame}")
        self.assertEqual(closed.exception.code, HP.CLOSE_SERVER_ERROR)
        self.assertEqual(W.hub().limiter.count(H.ADDRESS), 0, "nothing counted")
        with db.session() as s:
            self.assertEqual(s.query(AuditLog).count(), audit0, "nothing audited")
        self.connect(token)                     # the database back, the same helper is let in

    def test_two_first_hellos_cannot_both_register_a_key(self):
        """Trust on first use is a conditional write: a hello that read "no key yet" while another registered its own
        meanwhile finds that key and is refused ``key``, rather than overwrite it."""
        sid, token, _tid = _machine(self.ids["lena"], "raced box")
        mine, theirs = H.key_b64(31), H.key_b64(32)
        real = W._refusal

        def checked_then_registered_elsewhere(s, row, server):
            out = real(s, row, server)
            with db.session() as other:
                other.get(Server, sid).public_key = theirs
            return out
        with mock.patch.object(W, "_refusal", checked_then_registered_elsewhere):
            found = W.authenticate(token, mine)
        self.assertIn(found.refusal, ("key", "error"), "never let in over another key")
        with db.session() as s:
            self.assertEqual(s.get(Server, sid).public_key, theirs, "the key that won stays")
        self.assertEqual(W.authenticate(token, mine).refusal, "key")
        self.assertEqual(W.authenticate(token, theirs).refusal, "")

    def test_a_seat_on_a_pooled_machine_plays_through_its_helper(self):
        """A machine from the Servers page is not in the registry's file; a seat naming it resolves to the worker
        provider instead of failing with "No server", and a game on it is refused while its helper is offline."""
        from citar.pool import seats as pool_seats
        sid, _token, _tid = _machine(self.ids["lena"], "Box")
        cfg = servers.resolve_llm({"server_id": sid, "model": "qwen/qwen3.8-27b", "reconnect_seconds": 30})
        self.assertEqual((cfg["provider"], cfg["server_id"], cfg["model"], cfg["reconnect_seconds"]),
                         ("worker", sid, "qwen/qwen3.8-27b", 30))
        info = servers.describe_seat({"server_id": sid, "model": "qwen/qwen3.8-27b"})
        self.assertEqual(info["server"], "Box")
        self.assertFalse(info["missing_server"])
        with db.session() as s:
            lena = s.get(User, self.ids["lena"])
            with self.assertRaises(ValueError):
                pool_seats.authorize(s, lena, [{"type": "llm", "llm": {"server_id": sid, "model": "m"}}])
            pool_seats.authorize(s, lena, [{"type": "bot"}, {"type": "llm", "llm": {"provider": "mock"}}])


# ---------------------------------------------------------------------------- pooled machines (unchanged)

class Power(_Live):
    """A helper's ``power`` frames as its machine's power rows, and costing reading them (4-06)."""

    def _rows(self, sid: str) -> list:
        import json
        from citar import usage
        out = []
        for path in sorted(usage.USAGE_DIR.glob(f"power-{sid}-*.jsonl")):
            out += [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
        return out

    def _powered(self, name: str, **power) -> tuple:
        """A helper machine of lena's with these power figures, its helper connected: (server id, fake helper)."""
        sid, token, _tid = _machine(self.ids["lena"], name)
        with db.session() as s:
            row = s.get(Server, sid)
            row.config = {**row.config, "power": {"idle_w": 20, "cpu_max_w": 60, "gpu_max_w": 100, **power}}
        servers.invalidate()
        helper = self.connect(token)
        return sid, helper

    @staticmethod
    def _minute(gpu_w=50.0, cpu_util=0.5) -> list:
        return [{"t": 1_000.0 + 10 * i, "gpu_w": gpu_w, "cpu_util": cpu_util} for i in range(6)]

    def test_a_minute_of_samples_is_one_row_of_the_machine_and_costing_measures_its_energy(self):
        from citar import costing, usage
        sid, helper = self._powered("powered box")
        connection = W.hub().get(sid)
        connection.connected_at -= 120          # connected two minutes ago
        sent = time.time()
        helper.send("power", samples=self._minute(gpu_w=70.0, cpu_util=0.5))
        self.assertTrue(H.until(lambda: self._rows(sid), 5), "a row was written")
        rows = self._rows(sid)
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertEqual((row["k"], row["srv"], row["n"], row["gpu_w"], row["cpu_util"]), ("pw", sid, 6, 70.0, 0.5))
        self.assertLess(abs(row["t1"] - sent), 5, "placed by the hub's clock, not the helper's samples' times")
        self.assertAlmostEqual(row["t1"] - row["t0"], 60.0, delta=0.2)
        # a second frame at once is no row (one a minute); one a minute later is, after the first
        helper.send("power", samples=self._minute())
        time.sleep(0.3)
        self.assertEqual(len(self._rows(sid)), 1, "a frame sooner than a minute is dropped")
        W.hub()._power_at[sid] -= 60          # the machine's last frame a minute ago
        helper.send("power", samples=self._minute(gpu_w=90.0))
        self.assertTrue(H.until(lambda: len(self._rows(sid)) == 2, 5))
        second = self._rows(sid)[1]
        self.assertEqual(second["gpu_w"], 90.0)
        # costing: an activity holding the machine over those minutes is priced from the rows (measured energy, the
        # draw above the machine's idle GPU), and the machine's metered energy counts both rows
        since, until = row["t0"] - 1, second["t1"] + 1
        ledger = usage.read(since, until)
        self.assertEqual(len(ledger["power"][sid]), 2)
        ledger["spans"].append({"act": "game:x", "srv": sid, "t0": row["t0"], "t1": second["t1"],
                                "held": second["t1"] - row["t0"], "busy": 30.0})
        r = costing.compute(since, until, ledger=ledger)
        c = r["acts"]["game:x"]["servers"][sid]
        self.assertGreater(c["energy_measured_h"], 0, c)
        self.assertEqual(c["energy_estimated_h"], 0, "every minute the machine was held was measured")
        self.assertEqual(r["servers"][sid]["metered"]["minutes"], 2)
        self.assertGreater(r["servers"][sid]["metered"]["kwh"], 0)

    def test_an_owner_who_turned_live_sampling_off_gets_no_rows_and_samples_that_do_not_read_are_none(self):
        sid, helper = self._powered("unsampled box", sampling="off")
        W.hub().get(sid).connected_at -= 120
        helper.send("power", samples=self._minute())
        sid2, helper2 = self._powered("blind box")
        W.hub().get(sid2).connected_at -= 120
        helper2.send("power", samples=self._minute(gpu_w=None, cpu_util=None))
        # a ping and its pong: the frames before it were handled
        for h in (helper, helper2):
            h.send("ping")
            self.assertEqual(h.recv(timeout=5, skip=())["t"], "pong")
        time.sleep(0.3)
        self.assertEqual(self._rows(sid), [])
        self.assertEqual(self._rows(sid2), [])

    def test_a_minute_without_the_gpus_draw_is_a_row_costed_with_the_gpus_estimate(self):
        """A machine whose GPU draw its driver does not report samples its CPU alone: its row carries the CPU, and
        costing still prices the GPU's work from the model's busy time (an estimate, so not a measured minute)."""
        from citar import costing, usage
        sid, helper = self._powered("radeon box", gpu_max_w=200)
        W.hub().get(sid).connected_at -= 120
        helper.send("power", samples=self._minute(gpu_w=None, cpu_util=0.1))
        self.assertTrue(H.until(lambda: self._rows(sid), 5), "a row was written")
        row = self._rows(sid)[0]
        self.assertEqual((row["gpu_w"], row["cpu_util"]), (None, 0.1))
        ledger = usage.read(row["t0"] - 1, row["t1"] + 1)
        ledger["spans"].append({"act": "game:r", "srv": sid, "t0": row["t0"], "t1": row["t1"],
                                "held": row["t1"] - row["t0"], "busy": row["t1"] - row["t0"]})
        r = costing.compute(row["t0"] - 1, row["t1"] + 1, ledger=ledger)
        c = r["acts"]["game:r"]["servers"][sid]
        # busy all the time it was held: the GPU's 200 W as estimated over that time, and the CPU's 10% of 60 W as
        # sampled in each clock minute the row touches (two at most)
        gpu_kwh = 200 * (row["t1"] - row["t0"]) / 3600 / 1000
        self.assertGreaterEqual(c["kwh_dynamic"], 0.999 * gpu_kwh, c)
        self.assertLessEqual(c["kwh_dynamic"], gpu_kwh + 6 * 120 / 3600 / 1000 + 1e-9, c)
        self.assertGreater(c["energy_estimated_h"], 0, c)
        self.assertEqual(c["energy_measured_h"], 0, c)
        self.assertNotIn("metered", r["servers"][sid], "its samples read no GPU: no meter reading of the machine")

    def test_a_reconnect_neither_overlaps_the_machines_last_row_nor_reaches_back_before_it(self):
        """The power rows' guarantees are the machine's, not a connection's: a helper that reconnects and sends
        samples at once writes no row over its last one, and a frame just after a connection began writes none
        reaching back before it."""
        clock = H.FakeClock()
        W.hub().clock = clock
        self.addCleanup(lambda: setattr(W.hub(), "clock", W.Clock()))
        sid, token, _tid = _machine(self.ids["lena"], "flapping box")

        def reconnect(old):
            old.close()
            self.assertTrue(H.until(lambda: W.hub().get(sid) is None, 5), "the old connection is gone")
            return self.connect(token)

        def handled(h):
            h.send("ping")
            self.assertEqual(h.recv(timeout=5, skip=())["t"], "pong")

        helper = self.connect(token)
        W.hub().get(sid).connected_at -= 120
        helper.send("power", samples=self._minute(gpu_w=60.0))
        self.assertTrue(H.until(lambda: self._rows(sid), 5))
        # at once over a new connection: under a minute after the machine's last frame, dropped
        helper = reconnect(helper)
        helper.send("power", samples=self._minute(gpu_w=61.0))
        handled(helper)
        # 35 s on: a row from where the last one ended, not reaching back over it
        clock.advance(35)
        helper.send("power", samples=self._minute(gpu_w=62.0))
        self.assertTrue(H.until(lambda: len(self._rows(sid)) == 2, 5))
        # another connection 30 s on, and a frame 5 s into it: no row reaching back before the connection
        clock.advance(30)
        helper = reconnect(helper)
        clock.advance(5)
        helper.send("power", samples=self._minute(gpu_w=63.0))
        handled(helper)
        time.sleep(0.3)
        rows = self._rows(sid)
        self.assertEqual([r["gpu_w"] for r in rows], [60.0, 62.0], rows)
        first, second = rows
        self.assertGreaterEqual(second["t0"], first["t1"], "the rows do not overlap")
        self.assertAlmostEqual(second["t1"] - second["t0"], 35.0, delta=0.2)

    def test_a_gpu_draw_over_what_the_machines_scan_says_it_can_draw_is_left_out(self):
        """A reading 1.5 times past the power limits the helper's own scan reports is not the machine's."""
        sid, helper = self._powered("capped box")
        W.hub().get(sid).connected_at -= 120
        scan = {"format": "citar-hardware", "collector_version": 2, "collector": "citar-helper",
                "gpus": [{"name": "Card", "vendor": "NVIDIA", "power_limit_w": 300.0, "power_readable": True}]}
        helper.send("hardware", scan=scan, source="scan")
        samples = self._minute(gpu_w=100.0)
        samples[-1]["gpu_w"] = 5000.0
        helper.send("power", samples=samples)
        self.assertTrue(H.until(lambda: self._rows(sid), 5))
        self.assertEqual(self._rows(sid)[0]["gpu_w"], 100.0)
        self.assertEqual(W.hub().get(sid).gpu_cap_w, 450.0)

    def test_a_rescan_reaches_the_helper_and_its_frame_is_the_last_report(self):
        sid, token, _tid = _machine(self.ids["lena"], "rescanned box")
        helper = self.connect(token)
        r = self.call("lena", "POST", f"/api/machines/{sid}/rescan")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(helper.recv(timeout=5)["t"], "rescan")
        scan = {"format": "citar-hardware", "collector_version": 2, "collector": "citar-helper",
                "cpu": {"model": "Rescanned CPU", "cores": 4, "threads": 8}, "memory": {"ram_gb": 8.0}}
        helper.send("hardware", scan=scan, source="scan")

        def kept():
            with db.session() as s:
                d = s.get(Server, sid).hardware_detected or {}
                return d.get("hardware", {}).get("cpu", {}).get("model") == "Rescanned CPU" and d.get("hardware_at")
        self.assertTrue(H.until(kept, 5), "the scan is the last report")
        page = self.call("lena", "GET", f"/api/machines/{sid}/settings").json()
        self.assertEqual(page["report"]["hardware"]["cpu"]["model"], "Rescanned CPU")
        self.assertTrue(page["report"]["hardware_at"])


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

    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_an_eliminated_model_frees_its_machine(self):
        from citar.pool import seats
        game = self.manager.create({"map_size": "small", "seed": 3},
                                   [{"type": "llm", "llm": {"server_id": self.server_id, "model": "m1"}},
                                    {"type": "bot"}, {"type": "bot"}])
        game.suspend()
        self.assertEqual(len(seats.occupied(self.server_id)), 1)
        game.game.test_ops([{"op": "eliminate", "player": 0}])   # the bots play on; the model is never asked again
        self.assertEqual(game.game.phase, "playing")
        self.assertFalse(game.game.is_alive(0))
        self.assertEqual(seats.occupied(self.server_id), [])

    def _bare_runner(self):
        """A probe runner without its background dispatcher, to drive by hand."""
        from citar import probes
        runner = probes.ProbeRunner.__new__(probes.ProbeRunner)
        runner.manager, runner.lock, runner._stop_run, runner._thread = self.manager, threading.Lock(), set(), None
        runner.lives, runner._workers, runner._loaded, runner.asking = {}, {}, {}, {}
        runner.POLL_SECONDS = 0.05
        written, write = [], runner._write

        def write_and_remember(run):
            written.append(run["id"])
            write(run)
        runner._write = write_and_remember
        self.addCleanup(lambda: [shutil.rmtree(runner._run_dir(rid), ignore_errors=True) for rid in written])
        return runner

    def test_probe_runs_on_different_machines_do_not_wait_for_each_other(self):
        from citar.pool import scheduler as prompt_scheduler, seats
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
        held = prompt_scheduler.get().lease(self.server_id, "m1", kind="benchmark", label="benchmark run “long one”",
                                            ref=("benchmark", "long-one"))
        loop = threading.Thread(target=runner._loop, daemon=True)
        try:
            loop.start()
            self.assertTrue(both.wait(10), f"runs on two free machines should run at the same time: {started}")
            self.assertNotIn(ids[0], started, "the busy machine's run waits")
            waiting = runner.get_run(ids[0])
            self.assertEqual(waiting["status"], "waiting (machine busy)")
            self.assertIn("long one", seats.note_text(waiting["waiting_note"], lambda h: h["label"]))
            self.assertEqual(waiting["waiting"], "waiting for someone's benchmark run")
        finally:
            held.release()
            release.set()
        loop.join(10)
        self.assertFalse(loop.is_alive(), "the dispatcher stops when everything has run")
        self.assertEqual(set(started), set(ids))

    def test_one_queue_orders_benchmarks_and_probes_by_priority_then_age(self):
        from citar.pool import queue as work_queue, seats
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
            self.assertIn("old probe goes first", seats.note_text(job["waiting_note"], lambda h: h["label"]))
            self.assertNotIn("old probe", job["waiting"], "the stored text names no other work")
            self.assertEqual([i["id"] for i in work_queue.waiting(self.server_id)], [rid, job["id"]])
            work_queue.set_priority("benchmark", run["id"], 5)
            self.assertEqual(work_queue.waiting(self.server_id)[0]["id"], job["id"])
            sch.tick()
            self.assertIn(job["status"], ("loading", "running"), "a higher priority jumps the queue")
        finally:
            sch.stop()
            work_queue.set_priority("benchmark", run["id"], 0)
            work_queue.register("probe", lambda: [])

    def test_a_lobby_game_does_not_hold_up_offer_and_message_cases_but_a_benchmark_does(self):
        from citar.pool import scheduler as prompt_scheduler
        runner = self._bare_runner()
        rid = f"test-beside-{int(time.time() * 1000) % 100000}"
        runner._write({"id": rid, "name": "beside", "llm": {"server_id": self.server_id}, "status": "queued",
                       "jobs": []})
        game = prompt_scheduler.get().lease(self.server_id, "m1", kind="game", label="game “a lobby game”",
                                            ref=("game", "lobby-1"))
        try:
            self.assertFalse(runner._machine_busy(rid), "a lobby game holds no probe run up")
            waited = {}
            worker = threading.Thread(target=lambda: waited.setdefault("w", runner._wait_machine(
                rid, self.server_id, full=False)), daemon=True)
            worker.start()
            worker.join(5)
            self.assertEqual(waited.get("w"), False, "nor its next message case")
            bench = prompt_scheduler.get().lease(self.server_id, "m1", kind="benchmark", label="benchmark run “b”",
                                                 ref=("benchmark", "b-1"))
            try:
                self.assertTrue(runner._machine_busy(rid), "a benchmark does")
                self.assertEqual(runner.get_run(rid)["waiting"], "waiting for someone's benchmark run")
            finally:
                bench.release()
            self.assertFalse(runner._machine_busy(rid))
        finally:
            game.release()

    def test_a_turn_case_waits_while_leases_fill_the_machines_slots(self):
        from citar.pool import scheduler as prompt_scheduler
        runner = self._bare_runner()
        runner.BENCHMARK_POLL_SECONDS = 0.05
        rid = f"test-turn-{int(time.time() * 1000) % 100000}"
        runner._write({"id": rid, "name": "turns", "llm": {"server_id": self.server_id}, "status": "running",
                       "jobs": []})
        game = prompt_scheduler.get().lease(self.server_id, "m1", kind="game", label="game “a lobby game”",
                                            ref=("game", "lobby-1"))
        waited = {}
        worker = threading.Thread(target=lambda: waited.setdefault("w", runner._wait_machine(
            rid, self.server_id, full=True)), daemon=True)
        try:
            worker.start()
            deadline = time.perf_counter() + 10
            while runner.get_run(rid).get("waiting") is None and time.perf_counter() < deadline:
                time.sleep(0.02)
            run = runner.get_run(rid)
            self.assertEqual((run["status"], run["waiting"]), ("waiting (machine busy)", "waiting for someone's game"))
            time.sleep(0.2)
            self.assertTrue(worker.is_alive(), "the turn case waits while the game fills the one slot")
        finally:
            game.release()
        worker.join(5)
        self.assertEqual(waited.get("w"), True)
        run = runner.get_run(rid)
        self.assertEqual((run["status"], run.get("waiting")), ("running", None))
        with db.session() as s:
            s.get(Server, self.server_id).max_concurrent = 2
        servers.invalidate()
        prompt_scheduler.get().refresh(self.server_id)
        game = prompt_scheduler.get().lease(self.server_id, "m1", kind="game", label="game “a lobby game”",
                                            ref=("game", "lobby-1"))
        try:
            self.assertFalse(runner._wait_machine(rid, self.server_id, full=True), "a second slot is left for it")
        finally:
            game.release()


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
        from datetime import datetime
        _reset()
        with db.session() as s:
            s.add(Server(id="sv_d", name="Desk", kind="owned", provider="lmstudio", reach="worker",
                         config={"restricted_hours": {"enabled": True, "grace_minutes": 5, "windows": [
                             {"days": [0, 1, 2, 3, 4, 5, 6], "start": "00:00", "end": "23:59"}]}}))
        self.assertIsNotNone(servers.restricted_at("sv_d", datetime(2026, 9, 21, 12, 0)))
        self.assertIsNone(servers.restricted_at("sv_other", datetime(2026, 9, 21, 12, 0)))
        self.assertEqual(servers.server_name("sv_d"), "Desk")
        self.assertEqual(servers.restriction_config("sv_d")["grace_minutes"], 5)

    def test_a_helpers_own_quiet_hours_are_the_machines(self):
        """P4.5.3: the scheduler reads a helper's ``status.quiet_until`` like the site's quiet hours: a ticket is
        refused as temporary, its ``retry_at`` the helper's own end."""
        from datetime import datetime
        S.reset()
        self.addCleanup(S.reset)
        _reset()
        sid, _token, _tid = _machine(None, "quiet helper")
        c = H.StubConnection(sid, [H.model(MODEL)])
        W.hub().register(c)
        self.addCleanup(W.hub().unregister, c)
        sched = S.get()
        self.assertEqual(sched.ticket(sid, MODEL, cls="test").state, "queued")
        end = time.time() + 3600
        c.quiet_until = end
        sched.helper_changed(sid)
        t = sched.ticket(sid, MODEL, cls="test")
        self.assertEqual((t.state, t.error.code), ("refused", "quiet_hours"))
        self.assertTrue(t.error.temporary)
        self.assertEqual(t.error.retry_at, datetime.fromtimestamp(end))


class JobsHandshake(_Live):
    """A helper that plays jobs (4-10's fix round): the welcome's ``jobs`` never throw its work away when the
    dispatcher cannot answer, and an acknowledgement sent ``again`` with its job host's figures answers no settings
    frame."""

    def jobs_hello(self, token: str) -> dict:
        fields = H.hello(token=token, models=[H.model(MODEL)], slots=4)
        fields["helper"]["jobs_build"] = "0f1e2d3c4b5a"
        fields["running"], fields["results"] = ["cr_playing"], ["cr_finished"]
        return fields

    def test_a_dispatcher_that_cannot_answer_gets_the_helper_no_welcome(self):
        """The dispatcher failed (its database locked): a welcome then would drop the runs the helper plays and have it
        delete the results it holds, so there is none; 1011 has it connect again with backoff, keeping both."""
        from citar.jobs import dispatcher as D
        sid, token, _tid = _machine(self.ids["lena"], "jobs box")

        class Failing:
            def welcome_jobs(self, *args):
                raise RuntimeError("database is locked")
        with mock.patch.object(D, "current", return_value=Failing()), \
                self.assertLogs("citar.workers", level="ERROR"):
            helper = H.FakeHelper(self.server.url)
            self.addCleanup(helper.close)
            helper.send("hello", **{k: v for k, v in self.jobs_hello(token).items() if k != "t"})
            with self.assertRaises(H.Closed) as closed:
                frame = helper.recv(timeout=10)
                self.fail(f"a frame came before the close: {frame}")
        self.assertEqual(closed.exception.code, HP.CLOSE_SERVER_ERROR)
        self.assertTrue(H.until(lambda: W.hub().get(sid) is None, 5), "nothing stays registered")

    def test_without_a_dispatcher_its_runs_are_kept_and_its_results_wanted(self):
        """No dispatcher in the process (one not started yet, or stopping): nothing is decided about its runs, so the
        welcome keeps every one and wants every held result."""
        from citar.jobs import dispatcher as D
        _sid, token, _tid = _machine(self.ids["lena"], "early box")
        with mock.patch.object(D, "current", return_value=None):
            helper = H.FakeHelper(self.server.url)
            self.addCleanup(helper.close)
            welcome = helper.handshake(self.jobs_hello(token))
        self.assertEqual(welcome["jobs"], {"keep": ["cr_playing"], "drop": [], "resend": ["cr_finished"]})

    def test_an_acknowledgement_sent_again_answers_no_settings_frame(self):
        """A helper whose job host became ready sends its last acknowledgement again with the host's figures
        (``again``) while a push is on its way: it settles nothing (the push is still unanswered), is held to what its
        first sending answered (no overclaim), and its memory a run is the one the dispatcher matches jobs to; an
        acknowledgement without one says no host runs for those settings yet."""
        sid, token, _tid = _machine(self.ids["lena"], "again box", slots=3)
        helper = H.FakeHelper(self.server.url)
        self.addCleanup(helper.close)
        helper.send("hello", **{k: v for k, v in H.hello(token=token, models=[H.model(MODEL)], slots=4).items()
                                if k != "t"})
        welcome = helper.recv()
        site = welcome["settings"]

        def ack(slots: int, **extra) -> None:
            helper.send("settings_ack", effective={"llm": {"enabled": True, "slots": slots},
                                                   "cpu": {**site["cpu"], "enabled": False}},
                        capped=[], limits={"cpu": "soft", "memory": "soft"}, **extra)
        ack(3, memory_per_run_mb=500)
        self.assertTrue(H.until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).ack is not None, 5))
        self.assertEqual(W.hub().get(sid).memory_per_run_mb, 500)
        with db.session() as s:
            s.get(Server, sid).max_concurrent = 1
        servers.invalidate()
        self.assertFalse(W.hub().push_settings(sid), "nothing acknowledged yet")
        self.assertEqual(helper.recv()["llm"]["slots"], 1)
        ack(3, memory_per_run_mb=400, again=True)           # the welcome's again: 3 is what the welcome asked
        self.assertTrue(H.until(lambda: W.hub().get(sid).memory_per_run_mb == 400, 5))
        self.assertFalse(W.hub().push_settings(sid), "the push is still unanswered")
        self.assertEqual(W.hub().get(sid).llm_slots, 1, "held to what the site asks now")
        ack(1)                                              # the push's, no job host running for it yet
        self.assertTrue(H.until(lambda: W.hub().push_settings(sid), 5), "every frame acknowledged")
        self.assertIsNone(W.hub().get(sid).memory_per_run_mb)
        time.sleep(0.3)
        with db.session() as s:
            self.assertNotIn("settings_overclaim", s.get(Server, sid).flags or {}, "an honest helper is not flagged")


if __name__ == "__main__":
    unittest.main()
