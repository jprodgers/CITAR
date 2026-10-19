"""Pooled work: admission by task class, report narratives and probe cases on tickets, the Queue page
(design/phase3-features.md P3.3.3, P3.3.5, P3.3.7, P3.3.8; package 3-09's gates 1 to 4).

- **Admission** (gate 1): one classifier says what a seat plays (``pool.seats.seat_class``: ``llm`` full, ``hybrid``
  partial, every other seat nothing); a full seat needs ``full_llm`` and a grant for ``game``, a partial one
  ``partial_llm`` and ``pooled``; a pooled prompt's ticket is admitted when it is made (``pool.seats.ticket``) and a
  member without a ``pooled`` grant gets none, with the admission's reason; admission skips the capacity step, so a
  ticket on a busy machine queues; a grant's concurrency of 1 is the scheduler's cap on that member, whose requests
  never run two at once (a listener records them); ``update_seat`` to a machine the caller has no grant for is a 403
  that leaves the seat as it was.
- **Reports** (gate 2): a narrative is a ``report`` ticket; on a machine whose lobby game's seat is mid-turn no
  narrative step starts, and once the seat's turn begins again its request waits at most the one step the narrative
  borrowed; on a machine a benchmark holds the narrative waits and runs after it; no report holds a lease. A narrative
  on a helper machine waits for its helper (holding nothing), rides out a helper that drops, and gives up when it must.
- **Probes** (gate 3): offer and message cases are ``probe`` tickets, with no lease while they play; a turn case holds
  a ``probe_case`` lease for the case only; the cases' results are those of the dry run (``tests/test_editor.py``'s
  ``ProbeTests`` too); stopping a run cancels an answer waiting for its machine.
- **The Queue page** (gate 4): ``GET /api/queue`` lists the machines the caller may view, their leases and their
  running and waiting prompts with positions, naming another member's work "someone's <class>" and leaving out another
  member's private machine (only counted); priority by the work's manager only, the work's tickets following it;
  ``POST /api/queue/tickets/{id}/cancel`` by the work's owner, the machine's owner and administrators, 403 to a
  viewer of the work who is none of those, 404 to a stranger (P3.9.1).
"""
from __future__ import annotations

import tests  # temporary saves folder and registry; must be imported before citar

import json
import os
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_pooled_work_"))
#: This module's own database: the environment's names whichever module was imported last.
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, items, pool, servers, settings
from citar.auth import access, accounts, policy, sessions
from citar.db.models import Base, Server, User
from citar.pool import queue as work_queue
from citar.pool import scheduler as S
from citar.pool import seats as pool_seats
from citar.server import workers as W
from tests.helperfake import StubConnection

settings.reset()

DRY = {"connection": {"provider": "dryrun"}, "models": [{"key": "dry-run"}]}


def until(fn, timeout: float = 15.0, step: float = 0.02):
    """Poll ``fn`` until it returns something true (returned) or ``timeout`` passes (its last value)."""
    deadline = time.perf_counter() + timeout
    while True:
        value = fn()
        if value or time.perf_counter() > deadline:
            return value
        time.sleep(step)


def labels(sid: str) -> list:
    """The labels of the leases live on a machine."""
    return [lease.label for lease in S.get().leases(sid)]


class StubHelper(StubConnection):
    """A connected helper with no socket, answering each request at once with a ``done``."""

    def __init__(self, server_id: str, models: list):
        super().__init__(server_id, models, content="Written on the helper.")


class World(unittest.TestCase):
    """A fresh database (the suite's machines seeded), a fresh process scheduler, and these accounts: alice, a member
    who owns the one-slot dry-run machine "Alice box" (and the two-slot "Alice pair") in her group; bob, whose grant
    there covers games and pooled prompts; dave, whose grant covers pooled prompts only, with a concurrency of 1;
    gina, whose grant covers games only; carol, who has no grant and owns the private machine "Carol box"; ray, who
    may only read bob's report and whose grant covers pooled prompts; moe, a moderator; zara, an administrator."""

    PEOPLE = (("alice", "user"), ("bob", "user"), ("dave", "user"), ("gina", "user"), ("carol", "user"),
              ("ray", "user"), ("moe", "moderator"), ("zara", "admin"))

    def setUp(self):
        db.dispose()
        db.configure(_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.ids, self.headers = {}, {}
        with db.session() as s:
            for handle, role in self.PEOPLE:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.max_concurrent_games = 20
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            alice, carol = s.get(User, self.ids["alice"]), s.get(User, self.ids["carol"])
            group = pool.create_group(s, alice, name="Alice's", visibility="private")
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="bob", purposes=["game", "pooled"])
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="dave", purposes=["pooled"],
                              concurrency=1)
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="gina", purposes=["game"])
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="ray", purposes=["pooled"])
            self.box = self._machine(s, alice.id, "Alice box", group.id, 1)
            self.pair = self._machine(s, alice.id, "Alice pair", group.id, 2)
            self.carols = self._machine(s, carol.id, "Carol box", None, 1)
        servers.invalidate()
        self.headers["anon"] = {"origin": "https://citar.test"}

    @staticmethod
    def _machine(s, owner_id, name, group_id, slots) -> str:
        row = Server(owner_id=owner_id, name=name, kind="owned", provider="dryrun", reach="direct",
                     group_id=group_id, max_concurrent=slots, config=json.loads(json.dumps(DRY)))
        s.add(row)
        s.flush()
        return row.id

    def user(self, handle: str):
        with db.session() as s:
            u = s.get(User, self.ids[handle])
            s.expunge(u)
            return u

    def ticket(self, sid, handle=None, cls="report", **kw):
        """A pooled prompt of ``handle``'s on ``sid``, admitted as ``pool.seats.ticket`` admits it."""
        return pool_seats.ticket(sid, "dry-run", cls=cls, consumer=self.ids[handle] if handle else None, **kw)


# ---------------------------------------------------------------------------- gate 1: admission by task class

class Classifier(unittest.TestCase):
    def test_every_seat_type_has_its_class(self):
        """One function says what a seat plays: an ``llm`` seat every turn, a ``hybrid`` seat a prompt at a time, any
        other seat (a person, an MCP client, a bot, a script, an unknown type, a seat with no type) no model, whatever
        block it carries; a session's ``Seat`` the same as its dict."""
        from citar.server import app as appmod
        from citar.server.session import Seat
        block = {"server_id": "sv_gpu", "model": "dry-run"}
        for seat, want in (({"type": "llm", "llm": block}, "full"), ({"type": "llm"}, "full"),
                           ({"type": "hybrid", "llm": block}, "partial"), ({"type": "human", "llm": block}, None),
                           ({"type": "mcp"}, None), ({"type": "bot", "llm": block}, None), ({"type": "script"}, None),
                           ({"type": "robot"}, None), ({}, None), ({"type": None}, None), ({"type": 3}, None),
                           (None, None), ("llm", None), (Seat(player=0, type="llm", llm=dict(block)), "full"),
                           (Seat(player=1, type="bot"), None)):
            with self.subTest(seat=seat):
                self.assertEqual(pool_seats.seat_class(seat), want)
        self.assertEqual(set(pool_seats.MODEL_SEAT_TYPES), {"llm", "hybrid"})
        self.assertEqual(appmod.MODEL_SEATS, pool_seats.MODEL_SEAT_TYPES, "the route's model seats are the classifier's")
        self.assertEqual(pool_seats.CLASS_NEEDS, {"full": ("full_llm", "game"), "partial": ("partial_llm", "pooled")})


class AdmissionByClass(World):
    def test_a_member_without_a_pooled_grant_gets_no_ticket_there(self):
        """gina's grant covers games only: her report's prompt is refused when it is made, with the admission's reason;
        bob's and dave's grants cover pooled prompts; carol may not see the machine and is told only that; the
        machine's owner and an administrator need no grant; the server's own prompt (no consumer) is not admitted."""
        refused = self.ticket(self.box, "gina")
        self.assertEqual((refused.state, refused.error.code), ("refused", "purpose"), refused.reason)
        self.assertIn("pooled prompts", refused.reason)
        self.assertIn("ordinary games", refused.reason)
        hidden = self.ticket(self.box, "carol")
        self.assertEqual((hidden.state, hidden.error.code), ("refused", "no_grant"))
        self.assertEqual(hidden.reason, "You may not use that machine.", "carol is not told the group's name")
        for who in ("bob", "dave", "alice", "zara", None):
            with self.subTest(who=who):
                t = self.ticket(self.box, who)
                self.assertEqual(t.state, "queued", t.reason)
                t.close()
        with db.session() as s:
            s.get(User, self.ids["bob"]).caps = {"partial_llm": False}
        policy.invalidate()
        off = self.ticket(self.box, "bob")
        self.assertEqual((off.state, off.error.code), ("refused", "capability"))
        self.assertEqual(off.reason, policy.denial(self.user("bob"), "partial_llm"))
        with db.session() as s:
            s.get(User, self.ids["bob"]).caps = {"run_reports": False}
        self.assertEqual(self.ticket(self.box, "bob", capability="run_reports").error.code, "capability",
                         "the work's own capability, when the maker names it")

    def test_seats_are_admitted_by_their_task_class(self):
        """A full seat needs ``full_llm`` and a grant for games, a partial seat ``partial_llm`` and one for pooled
        prompts; a seat that plays no model is never refused for the machine its block names."""
        llm = {"type": "llm", "llm": {"server_id": self.box, "model": "dry-run"}}
        hybrid = {"type": "hybrid", "llm": {"server_id": self.box, "model": "dry-run"}}
        bot = {"type": "bot", "llm": {"server_id": self.box, "model": "dry-run"}}
        with db.session() as s:
            def check(handle, seat):
                try:
                    pool_seats.authorize(s, s.get(User, self.ids[handle]), [seat])
                    return None
                except ValueError as e:
                    return str(e)
            self.assertIsNone(check("gina", llm))
            self.assertIn("pooled prompts", check("gina", hybrid))
            self.assertIn("ordinary games", check("dave", llm))
            self.assertIsNone(check("dave", hybrid))
            self.assertIsNone(check("bob", llm))
            self.assertIsNone(check("bob", hybrid))
            self.assertEqual(check("carol", llm), "You may not use that machine. Pick one of the machines you may use.")
            self.assertIsNone(check("carol", bot), "a bot seat's block is never admitted")
            self.assertIsNone(check("alice", llm))
            s.get(User, self.ids["bob"]).caps = {"full_llm": False}
            self.assertEqual(check("bob", llm), policy.denial(s.get(User, self.ids["bob"]), "full_llm"))
            self.assertEqual(check("bob", {"type": "llm", "llm": {"provider": "dryrun"}}),
                             policy.denial(s.get(User, self.ids["bob"]), "full_llm"),
                             "the capability is the seat's, with or without a machine")
            self.assertIsNone(check("bob", hybrid))
            s.get(User, self.ids["bob"]).caps = {"partial_llm": False}
            self.assertEqual(check("bob", hybrid), policy.denial(s.get(User, self.ids["bob"]), "partial_llm"))

    def test_a_busy_machine_queues_a_ticket_rather_than_refusing_it(self):
        """Admission skips the capacity step (P3.3.7): with the one slot taken, bob's prompt is queued, not refused,
        and starts when the slot frees."""
        holder = self.ticket(self.box, "alice", cls="test")
        holding = holder.acquire()
        self.assertEqual(len(S.get().machine(self.box).slots), 1)
        t = self.ticket(self.box, "bob")
        self.assertEqual(t.state, "queued", t.reason)
        got = {}

        def step():
            got["grant"] = t.acquire()
        waiter = threading.Thread(target=step, daemon=True)
        waiter.start()
        self.assertTrue(until(lambda: S.get().machine(self.box).view()["waiting"]), "it waits in the machine's line")
        self.assertNotIn("grant", got)
        holder.finish(holding, gen_s=0.01)
        holder.close()
        waiter.join(5)
        self.assertIn("grant", got, "it started once the slot was free")
        t.finish(got["grant"], gen_s=0.01)
        t.close()
        with db.session() as s:
            from citar.pool import admission
            d = admission.check(s, s.get(User, self.ids["bob"]), s.get(Server, self.box), "pooled", in_flight=None)
            self.assertTrue(d.allowed, d.reason)

    def test_a_member_whose_grant_has_concurrency_one_never_has_two_requests_running(self):
        """dave's grant has a concurrency of 1: his tickets carry that cap, and of four of his prompts of two steps
        each, sent at once on the two-slot machine beside bob's, no two of his requests ever ran together (the
        listener's steps), while the machine itself ran two at once."""
        from citar.agents.providers import make_conversation
        events: list = []
        S.get().add_listener(events.append)

        made = [(who, self.ticket(self.pair, who, cls="probe")) for who in ("dave", "dave", "bob", "dave", "dave", "bob")]
        for who, t in made:
            self.assertEqual(t.state, "queued", t.reason)
            self.assertEqual(t.cap, 1 if who == "dave" else 0, "the grant's concurrency is the scheduler's cap")

        def prompt(t):
            conv = make_conversation({"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0.15}, "", [],
                                     binding=t)
            conv.add_user_text("Write something.")
            try:
                conv.step()
                conv.step()
            finally:
                conv.close()
                t.close()
        threads = [threading.Thread(target=prompt, args=(t,), daemon=True) for _who, t in made]
        for th in threads:
            th.start()
        for th in threads:
            th.join(30)
        tol = 0.005 + 2 * time.get_clock_info("time").resolution + time.get_clock_info("perf_counter").resolution
        daves = sorted((e.at - e.gen_s, e.at) for e in events if e.consumer == self.ids["dave"])
        self.assertEqual(len(daves), 8)
        overlaps = [(a, b) for a, b in zip(daves, daves[1:]) if b[0] < a[1] - tol]
        self.assertEqual(overlaps, [], "two of dave's requests ran at once")
        self.assertEqual(S.get().machine(self.pair).max_in_flight, 2, "the machine's two slots were both used")


class UpdateSeat(World):
    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)

    def call(self, who, method, path, body=None):
        return self.client.request(method, path, json=body, headers=self.headers[who])

    def test_update_seat_is_admitted_again_by_its_class(self):
        """A seat made a model seat on a machine the caller has no grant for is a 403 that leaves the seat as it was:
        carol's machine for gina (not hers to see), alice's for dave (his grant covers pooled prompts, not a seat that
        plays every turn); gina's grant for games lets her; a bot seat naming a machine is never admitted."""
        for who, sid, want in (("gina", self.carols, 403), ("dave", self.box, 403), ("gina", self.box, 200)):
            with self.subTest(who=who, machine=sid):
                made = self.call(who, "POST", "/api/games", {"name": f"{who}'s", "config": {
                    "map_size": "duel", "seed": 5, "barbarians": "off"}, "seats": [{"type": "human"}, {"type": "bot"}]})
                self.assertEqual(made.status_code, 200, made.text[:300])
                gid = made.json()["id"]
                self.addCleanup(self.manager.delete, gid)
                got = self.call(who, "POST", f"/api/games/{gid}/seats/1",
                                {"type": "llm", "llm": {"server_id": sid, "model": "dry-run"}})
                self.assertEqual(got.status_code, want, got.text[:300])
                seat = self.manager.get(gid).seats[1]
                if want == 403:
                    self.assertEqual((seat.type, seat.llm or {}), ("bot", {}), "the seat is as it was")
                    if who == "dave":
                        self.assertIn("pooled prompts", got.json()["detail"])
                    else:
                        self.assertNotIn("Carol", got.text, "the machine is not named to someone who may not see it")
                else:
                    self.assertEqual((seat.type, seat.llm.get("server_id")), ("llm", sid))


class ServersPageTest(World):
    """The Servers page's test is a pooled prompt (P3.3.7): on a helper machine in alice's group, dave (a grant for
    pooled prompts) gets the helper's reply and gina (a grant for games only) is refused at admission, naming the
    purpose; with ``partial_llm`` off, dave is refused too."""

    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod, pool_api
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        pool_api._tests_waiting.clear()
        with db.session() as s:
            group_id = s.get(Server, self.box).group_id
            row = Server(owner_id=self.ids["alice"], name="Alice helper", kind="owned", provider="lmstudio",
                         reach="worker", group_id=group_id, max_concurrent=1, config={"models": [{"key": "m"}]})
            s.add(row)
            s.flush()
            self.helper_box = row.id
        servers.invalidate()
        helper = StubHelper(self.helper_box, [{"key": "m", "loaded": True}])
        W.hub().register(helper)
        self.addCleanup(W.hub().unregister, helper)

    def ask(self, who):
        got = self.client.post(f"/api/pool/servers/{self.helper_box}/test", params={"wait": 10},
                               headers=self.headers[who])
        self.assertEqual(got.status_code, 200, got.text[:300])
        return got.json()

    def test_the_test_needs_a_grant_for_pooled_prompts(self):
        self.assertTrue(self.ask("dave").get("ok"), "a grant for pooled prompts")
        refused = self.ask("gina")
        self.assertEqual((refused.get("ok"), refused.get("stage"), refused.get("code")), (False, "admission", "purpose"))
        self.assertIn("pooled prompts", refused.get("reason", ""))
        with db.session() as s:
            s.get(User, self.ids["dave"]).caps = {"partial_llm": False}
        off = self.ask("dave")
        self.assertEqual((off.get("ok"), off.get("stage"), off.get("code")), (False, "admission", "capability"))


# ---------------------------------------------------------------------------- gate 2: reports on tickets

class _Recorded:
    """The dry run's steps, recorded: when each began and ended (``time.perf_counter``), and whether it was a plain
    completion (a report's narrative)."""

    def __init__(self):
        from citar.agents.providers.dryrun import DryRunConversation
        self.steps: list = []
        original = DryRunConversation.step
        recorded = self

        def step(conv):
            t0 = time.perf_counter()
            try:
                return original(conv)
            finally:
                recorded.steps.append({"start": t0, "end": time.perf_counter(), "narrative": not conv._tools})
        self.patch = mock.patch.object(DryRunConversation, "step", step)

    def narratives(self) -> list:
        return [x for x in self.steps if x["narrative"]]


class _ReportCase(World):
    """A report runner of the test's own, the dry run's steps recorded."""

    def setUp(self):
        super().setUp()
        from citar import reports
        self.reports = reports
        self.runner = reports.ReportRunner(_TMP / f"reports-{self.id().rsplit('.', 1)[-1]}")
        self.addCleanup(shutil.rmtree, self.runner.dir, ignore_errors=True)
        self.runner.RETRY_MIN_SECONDS = 0.05
        self.runner.RECONNECT_POLL_SECONDS = 0.1
        self.record = _Recorded()
        self.record.patch.start()
        self.addCleanup(self.record.patch.stop)
        self.seen_leases: list = []

    def start(self, sid="sv_gpu", model_id="m_gpu", title="Pooled report"):
        return self.runner.start({"title": title, "sections": ["summary"],
                                  "narrative": {"enabled": True, "server_id": sid, "model_id": model_id}})

    def built(self, rid: str, sid: str = "sv_gpu", timeout: float = 60.0, runner=None) -> dict:
        runner = runner or self.runner

        def done():
            self.seen_leases.append(tuple(lease.kind for lease in S.get().leases(sid)))
            return runner.meta(rid)["status"] not in ("queued", "running")
        self.assertTrue(until(done, timeout), runner.meta(rid))
        return runner.meta(rid)

    def waiting_report(self, sid="sv_gpu"):
        return [t for t in S.get().machine(sid).view()["waiting"] if t["class"] == "report"]


class Reports(_ReportCase):

    def test_no_narrative_step_starts_while_the_lobby_seat_is_mid_turn(self):
        """A lobby game's seat on the one-slot machine is mid-turn: the report's narrative waits in the machine's
        line and starts only once the seat is idle, on the slot it lends; the seat's next turn then begins, and its
        request waits for that one step only, far under the report class's cap; the report holds no lease."""
        sched = S.get()
        seat = sched.lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        spans: list = []
        seat.activate()
        began = time.perf_counter()
        m = self.start()
        self.assertTrue(until(self.waiting_report), "the narrative waits in the machine's line")
        time.sleep(0.5)
        self.assertEqual(self.record.narratives(), [], "no narrative step started while the seat was mid-turn")
        idle_at = time.perf_counter()
        spans.append((began, idle_at))
        seat.idle()

        def running_report():
            return [t for t in sched.machine("sv_gpu").view()["running"] if t["class"] == "report"]
        running = until(running_report, 10)
        self.assertTrue(running and running[0]["borrowing"], f"it runs on the slot the idle seat lends: {running}")
        # the seat's turn begins while the narrative's one step runs: its request waits for that step only
        seat.activate()
        active_again = time.perf_counter()
        grant = seat.acquire()
        spans.append((active_again, time.perf_counter()))
        seat.finish(grant, gen_s=0.01)
        seat.idle()
        meta = self.built(m["id"])
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertIn("Dry run narrative", self.runner.html_path(m["id"]).read_text(encoding="utf-8"))
        starts = [x["start"] for x in self.record.narratives()]
        self.assertEqual(len(starts), 1)
        for a, b in spans[:1]:
            self.assertFalse(any(a <= t <= b for t in starts), "a narrative step started while the seat was mid-turn")
        step = self.record.narratives()[0]
        self.assertGreater(grant.queue_s, 0.0, "the seat waited for the borrowed step")
        self.assertLessEqual(grant.queue_s, (step["end"] - step["start"]) + 1.0, "for one step at most")
        self.assertLessEqual(grant.queue_s, S.BORROW_SECONDS["report"] + 1.0)
        self.assertEqual({k for x in self.seen_leases for k in x}, {"game"}, "the report held no lease")
        self.assertNotIn("report", S.LEASE_KINDS)

    def test_a_narrative_waits_for_a_benchmark_and_runs_after_it(self):
        """A benchmark holds the machine: the narrative's ticket waits for it (a report waits for benchmarks), never
        starting beside it, and runs once the benchmark lets the machine go."""
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="benchmark run “b”", ref=("benchmark", "b"))
        m = self.start()
        self.assertTrue(until(self.waiting_report), "the narrative waits, it is not refused")
        time.sleep(0.5)
        self.assertEqual(self.record.narratives(), [])
        released = time.perf_counter()
        bench.release()
        meta = self.built(m["id"])
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertIn("Dry run narrative", self.runner.html_path(m["id"]).read_text(encoding="utf-8"))
        self.assertGreaterEqual(self.record.narratives()[0]["start"], released)
        self.assertNotIn("report", {k for x in self.seen_leases for k in x})

    def test_members_narratives_go_by_priority_then_take_turns(self):
        """Four narratives on the one-slot machine, two of bob's and two of dave's (both their grants cover pooled
        prompts there), queued behind a seat mid-turn: dave's second, raised, goes first; then the members take turns,
        the one served longest ago first, each in the order they asked (P3.3.3)."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        tests.share_with_members("sv_gpu")
        seat.activate()
        order, made = [], []
        with mock.patch.object(self.reports, "complete", self._completing(order)):
            for title, who in (("bob 1", "bob"), ("bob 2", "bob"), ("dave 1", "dave"), ("dave 2", "dave")):
                made.append(self.runner.start({"title": title, "sections": ["summary"], "narrative": {
                    "enabled": True, "server_id": "sv_gpu", "model_id": "m_gpu"}}, owner_id=self.ids[who]))
                self.assertTrue(until(lambda n=len(made): len(self.waiting_report()) == n), title)
            raised = made[3]["id"]
            work_queue.set_priority("report", raised, 10)          # as the priority route does
            self.addCleanup(work_queue.set_priority, "report", raised, 0)
            self.assertEqual(S.get().set_priority(("report", raised), 10), 1, "its ticket follows its work")
            self.assertEqual([t["label"] for t in self.waiting_report()],
                             ["report “dave 2”", "report “bob 1”", "report “bob 2”", "report “dave 1”"],
                             "the line as it stands: dave's raised one, then by arrival (nobody served yet)")
            seat.idle()
            for m in made:
                self.assertEqual(self.built(m["id"])["status"], "done")
        self.assertEqual(order, ["dave 2", "bob 1", "dave 1", "bob 2"])

    def test_a_narratives_ticket_carries_its_reports_priority_from_the_start(self):
        """A report raised on the Queue page before its narrative's ticket is made: the ticket is made with that
        priority (its class's base plus its report's), not only told of changes after."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        seat.activate()
        with self.runner._cpu:              # the build waits to gather: its ticket is not made yet
            m = self.start()
            work_queue.set_priority("report", m["id"], 7)
            self.addCleanup(work_queue.set_priority, "report", m["id"], 0)
        self.assertTrue(until(self.waiting_report))
        waiting = self.waiting_report()[0]
        self.assertEqual(waiting["priority"], 7)
        ticket = next(t for t in S.get().machine("sv_gpu")._tickets() if t.ref == ("report", m["id"]))
        self.assertEqual(ticket.priority, S.BASE_PRIORITY["report"] + 7)
        seat.idle()
        self.assertEqual(self.built(m["id"])["status"], "done")

    def test_deleting_a_report_whose_narrative_waits_cancels_its_prompt(self):
        """A report whose narrative waits in its machine's line (a seat mid-turn holds the one slot) is deleted: it is
        gone from the list at once, its prompt leaves the line, its files go once its build lets go, and its narrative
        never ran."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        seat.activate()
        m = self.start()
        self.assertTrue(until(self.waiting_report), "the narrative waits in the machine's line")
        self.assertEqual(self.runner.meta(m["id"])["status"], "running")
        self.runner.delete(m["id"])
        self.assertNotIn(m["id"], [x["id"] for x in self.runner.list()], "gone from the list at once")
        with self.assertRaises(KeyError):
            self.runner.meta(m["id"])
        self.assertTrue(until(lambda: not self.waiting_report(), 10), "its prompt left the line")
        self.assertTrue(until(lambda: not (self.runner.dir / m["id"]).exists(), 10), "its files went")
        self.assertTrue(until(lambda: not self.runner._builds, 10))
        self.assertEqual(self.record.narratives(), [])
        # the build lets go of the id after its folder is gone (so a listing between the two cannot show it): waited
        # for, not looked at (CI, test.yml at b0ebab0 on ubuntu-latest 3.11 looked in between)
        self.assertTrue(until(lambda: not self.runner._deleting, 10), self.runner._deleting)
        seat.idle()

    def test_a_listing_never_shows_a_report_whose_deletion_ends_while_it_reads(self):
        """A deleted report's build removes its folder and then lets go of its id. A listing that read the report's
        record just before must not show it once the id is let go (CI, test.yml run 38385560480: the report above was
        listed again right after its deletion, its build having ended between ``list``'s read and its look)."""
        rid = "20261013-000000-race"
        folder = self.runner.dir / rid
        folder.mkdir(parents=True)
        (folder / "meta.json").write_text(json.dumps({"id": rid, "title": "Raced", "status": "done"}), encoding="utf-8")
        self.runner._deleting.add(rid)
        real = self.reports.fsutil.read_text

        def read_then_let_go(p):
            text = real(p)
            if Path(p).parent == folder:            # the build's last steps (``_build``), between the read and the look
                shutil.rmtree(folder)
                self.runner._deleting.discard(rid)
            return text
        with mock.patch.object(self.reports.fsutil, "read_text", read_then_let_go):
            listed = [m["id"] for m in self.runner.list()]
        self.assertNotIn(rid, listed)

    def test_a_report_the_server_restarted_under_is_built_again(self):
        """A report the server restarted under (left ``running``) is queued again by the next runner and built, its
        restart counted; one restarted under ``MAX_RESTARTS`` times already fails, so a build that brings the server
        down does not do so for ever."""
        from citar import reports
        plain = {"title": "Plain", "sections": ["summary"]}
        first = self.runner.start(plain)
        again = self.runner.start(plain)
        for m in (first, again):
            self.assertEqual(self.built(m["id"])["status"], "done")
        self.runner._set(first["id"], status="running", progress="Collecting data")     # as a crash leaves it
        self.runner._set(again["id"], status="running", restarts=reports.ReportRunner.MAX_RESTARTS)
        restarted = reports.ReportRunner(self.runner.dir)
        meta = self.built(first["id"], runner=restarted)
        self.assertEqual((meta["status"], meta.get("restarts")), ("done", 1), meta)
        failed = restarted.meta(again["id"])
        self.assertEqual(failed["status"], "failed")
        self.assertIn("restarted while this report was running", failed["error"])

    def test_the_dispatcher_survives_a_failed_pass_and_fails_a_report_it_cannot_read(self):
        """A pass of the reports' dispatcher that fails (a disk error reading the queue) is logged and the dispatcher
        looks again, so the queued report is built; a queued report whose record cannot be read is failed with the
        reason (its title from its specification), not tried on every pass, and the next report is still built."""
        self.runner.POLL_SECONDS = 0.1
        ranked, calls = self.runner._ranked, []

        def flaky():
            calls.append(1)
            if len(calls) == 1:
                raise OSError("the disk hiccuped")
            return ranked()
        with self.assertLogs("citar.reports", "ERROR"), mock.patch.object(self.runner, "_ranked", flaky):
            m = self.runner.start({"title": "After a hiccup", "sections": ["summary"]})
            self.assertEqual(self.built(m["id"])["status"], "done")
        rid = "20261012-000000-beef"
        d = self.runner.dir / rid
        d.mkdir(parents=True)
        (d / "spec.json").write_text(json.dumps({"title": "Broken one"}), encoding="utf-8")
        (d / "meta.json").write_text("{not json", encoding="utf-8")
        with self.runner.lock:
            self.runner.queue.append(rid)
        with self.assertLogs("citar.reports", "WARNING"):
            self.runner._wake()
            later = self.runner.start({"title": "After the broken one", "sections": ["summary"]})
            self.assertEqual(self.built(later["id"])["status"], "done")
        meta = self.runner.meta(rid)
        self.assertEqual((meta["status"], meta["title"]), ("failed", "Broken one"), meta)
        self.assertIn("could not be read", meta["error"])
        self.assertNotIn(rid, self.runner.queue)

    def _completing(self, order: list):
        def complete(cfg, system, prompt, binding):
            # the ticket's slot is taken: one at a time, in the scheduler's order
            grant = binding.acquire()
            try:
                order.append(binding.label.split("“")[1].rstrip("”"))
                time.sleep(0.05)
            finally:
                binding.finish(grant, gen_s=0.05)
            return "Written.", 0.05
        return complete


class ReportOnHelper(_ReportCase):
    """A narrative on a helper machine: it waits for an offline helper holding nothing, rides out a helper that drops,
    and gives up when it must (package 3-08's behaviour, on tickets)."""

    def setUp(self):
        super().setUp()
        with db.session() as s:
            row = Server(owner_id=None, name="Helper box", kind="owned", provider="lmstudio", reach="worker",
                         config={"models": [{"key": "helper-model"}]})
            s.add(row)
            s.flush()
            self.sid = row.id
        servers.invalidate()
        self.calls: list = []
        self.helper = None

    def connect(self):
        self.helper = StubHelper(self.sid, [{"key": "helper-model", "loaded": True}])
        W.hub().register(self.helper)
        self.addCleanup(W.hub().unregister, self.helper)

    def test_an_offline_helper_is_waited_for_holding_nothing(self):
        m = self.runner.start({"title": "Helper report", "sections": ["summary"],
                               "narrative": {"enabled": True, "server_id": self.sid, "model_id": "helper-model"}})
        self.assertTrue(until(lambda: "offline" in (self.runner.meta(m["id"]).get("progress") or ""), 15),
                        self.runner.meta(m["id"]))
        self.assertEqual(self.runner.meta(m["id"])["status"], "running")
        self.assertEqual(labels(self.sid), [], "it holds nothing while it waits")
        self.connect()
        meta = self.built(m["id"], self.sid)
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertIn("Written on the helper.", self.runner.html_path(m["id"]).read_text(encoding="utf-8"))

    def test_a_helper_that_drops_while_the_narrative_is_written_is_waited_for(self):
        from citar.agents.providers.worker_provider import WorkerConnectionError
        self.connect()
        answers = [WorkerConnectionError("the helper disconnected"), WorkerConnectionError("not connected")]

        def complete(cfg, system, prompt, binding):
            self.calls.append((time.perf_counter(), binding.cls))
            if answers:
                raise answers.pop(0)
            return "Written after the helper came back.", 0.5
        with mock.patch.object(self.reports, "complete", complete):
            meta = self.built(self.start(self.sid, "helper-model")["id"], self.sid)
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertEqual(len(self.calls), 3, "tried again on a new ticket once the helper was back")
        self.assertEqual({c[1] for c in self.calls}, {"report"})
        self.assertGreaterEqual(self.calls[1][0] - self.calls[0][0], 0.04, "never in a tight loop")

    def test_the_wait_is_bounded_and_other_failures_are_not_retried(self):
        from citar.agents.providers.worker_provider import WorkerConnectionError
        self.connect()
        self.runner.RECONNECT_SECONDS = 0.3
        notes, narrative = [], self.runner._narrative

        def noted(*a, **kw):
            out = narrative(*a, **kw)
            notes.append(out[1])
            return out
        self.runner._narrative = noted

        def gone(cfg, system, prompt, binding):
            self.calls.append("gone")
            raise WorkerConnectionError("the helper is not connected")
        with mock.patch.object(self.reports, "complete", gone):
            meta = self.built(self.start(self.sid, "helper-model")["id"], self.sid)
        self.assertEqual(meta["status"], "done", "the report is built without its narrative")
        self.assertTrue(notes[-1].startswith("The narrative failed: WorkerConnectionError"), notes)
        self.assertGreater(len(self.calls), 1)
        self.calls.clear()

        def refused(cfg, system, prompt, binding):
            self.calls.append("refused")
            raise RuntimeError("the machine refused the request")
        with mock.patch.object(self.reports, "complete", refused):
            meta = self.built(self.start(self.sid, "helper-model")["id"], self.sid)
        self.assertEqual(meta["status"], "done")
        self.assertEqual(self.calls, ["refused"], "a refusal is not a dropped helper: not tried again")
        self.assertTrue(notes[-1].startswith("The narrative failed: RuntimeError"), notes)

    def test_a_prompt_the_machine_will_not_take_skips_the_narrative(self):
        """A model the helper has not loaded (its owner allows no swaps) is refused at once: the report is built,
        its narrative skipped with the reason; and one whose start window closes before the refusal clears gives
        up at once too."""
        self.helper = StubHelper(self.sid, [{"key": "helper-model", "loaded": False}])
        W.hub().register(self.helper)
        self.addCleanup(W.hub().unregister, self.helper)
        notes, narrative = [], self.runner._narrative

        def noted(*a, **kw):
            out = narrative(*a, **kw)
            notes.append(out[1])
            return out
        self.runner._narrative = noted
        m = self.start(self.sid, "helper-model")
        meta = self.built(m["id"], self.sid)
        self.assertEqual(meta["status"], "done")
        self.assertTrue(notes[-1].startswith("The narrative was skipped: The model is not loaded"), notes)
        # the reader is told why the report has no analysis: on its page, and in its record for the Reports list
        page = self.runner.html_path(m["id"]).read_text(encoding="utf-8")
        self.assertIn('<section id="narrative">', page)
        self.assertIn("The narrative was skipped: The model is not loaded", page)
        self.assertEqual((meta["narrative_written"], meta["narrative_note"]), (False, notes[-1]))

    def test_narratives_waiting_for_their_machine_hold_up_no_other_report(self):
        """Four narratives of two members wait for an offline helper: a report with no narrative is built at once
        beside them (the waiting builds do not count against ``MAX_BUILDS``); a member's third narrative stays
        queued while two of theirs are under way, and another member's starts; all are built once the helper is
        back."""
        tests.share_with_members(self.sid)

        def narrated(title, who):
            return self.runner.start({"title": title, "sections": ["summary"], "narrative": {
                "enabled": True, "server_id": self.sid, "model_id": "helper-model"}}, owner_id=self.ids[who])

        def offline(m):
            return "offline" in (self.runner.meta(m["id"]).get("progress") or "")
        waiting = [narrated(f"{who} {i}", who) for who in ("bob", "dave") for i in (1, 2)]
        self.assertTrue(until(lambda: all(offline(m) for m in waiting), 15),
                        [self.runner.meta(m["id"]) for m in waiting])
        third = narrated("bob 3", "bob")
        t0 = time.perf_counter()
        plain = self.runner.start({"title": "No narrative", "sections": ["summary"]}, owner_id=self.ids["alice"])
        self.assertEqual(self.built(plain["id"], self.sid, timeout=15)["status"], "done")
        self.assertLess(time.perf_counter() - t0, 10.0, "built at once, beside the waiting narratives")
        other = narrated("ray 1", "ray")
        self.assertTrue(until(lambda: offline(other), 15), "another member's narrative starts")
        time.sleep(0.3)
        self.assertEqual(self.runner.meta(third["id"])["status"], "queued", "bob's third waits for one of his two")
        self.connect()
        for m in waiting + [third, other]:
            meta = self.built(m["id"], self.sid)
            self.assertEqual((meta["status"], meta.get("narrative_written")), ("done", True), meta)


# ---------------------------------------------------------------------------- gate 3: probes on tickets

class _ProbeScenario(World):
    """A two-player scenario (a script and the subject, met, with gold) and a probe on it with an offer, a message
    and a turn case."""

    @classmethod
    def setUpClass(cls):
        from citar import engine_api as E
        from citar import probes as P
        from citar.engine_api import EngineGame
        E.save_map(dict(E.generate_map(40, 26, "pangaea", 2, 2, seed=11, name="pooled-map"), id="pooled-map"))
        g = EngineGame.new({"map": "pooled-map", "players": [{"controller": "human"}, {"controller": "llm"}],
                            "seed": 5, "speed": "Quick"})
        g.apply_ops([{"op": "set_player", "player": "all", "gold": 500}, {"op": "meet", "a": 0, "b": 1}])
        g.save_scenario("pooled-scn", "Pooled scenario", "", [{"type": "script"}, {"type": "llm"}])
        cls.probe = P.validate({"id": "pooled-probe", "scenario": "pooled-scn", "subject": 1, "counterparty": 0,
                                "cases": [
                                    {"id": "gift", "kind": "offer", "give": [{"type": "gold", "amount": 100}],
                                     "receive": [], "expect": "accept"},
                                    {"id": "hello", "kind": "message", "message": "Greetings."},
                                    {"id": "turn", "kind": "turn", "expect_tools": ["end_turn"]}]})

    @classmethod
    def tearDownClass(cls):
        from citar import engine_api as E
        try:
            E.delete_scenario("pooled-scn")
        except Exception:
            pass


class Probes(_ProbeScenario):
    def setUp(self):
        super().setUp()
        from citar import probes as P
        from citar.server.session import SessionManager
        self.P = P
        self.manager = SessionManager()
        self.addCleanup(lambda: [self.manager.delete(sid) for sid in list(self.manager.sessions)])
        runner = P.ProbeRunner.__new__(P.ProbeRunner)
        runner.manager, runner.lock, runner._stop_run, runner._thread = self.manager, threading.Lock(), set(), None
        runner.lives, runner._workers, runner._loaded, runner.queue, runner.asking = {}, {}, {}, [], {}
        written, write = [], runner._write

        def write_and_remember(run):
            written.append(run["id"])
            write(run)
        runner._write = write_and_remember
        self.addCleanup(lambda: [shutil.rmtree(runner._run_dir(rid), ignore_errors=True) for rid in written])
        self.runner = runner

    def run_probe(self, owner=None, llm=None, cases=("gift", "hello", "turn")):
        rid = f"pooled-run-{int(time.time() * 1000) % 1000000}"
        self.runner._write({"id": rid, "name": "pooled run", "probe": self.probe,
                            "llm": dict(llm or {"server_id": "sv_gpu", "model": "dry-run"}), "repeats": 1,
                            "jobs": [{"case": c, "rep": 0} for c in cases], "status": "queued", "done": 0,
                            "created": time.time(), "started": None, "finished": None, "summary": {},
                            "owner_id": owner})
        return rid

    def test_offer_and_message_cases_run_as_tickets_and_turn_cases_on_a_lease_of_their_own(self):
        """A run on the one-slot machine: its offer and message cases ask the model on ``probe`` tickets (task class
        partial, metered to the run), with no lease on the machine while they play; its turn case plays on a
        ``probe_case`` lease named for the run, made for the case and gone after it; the results are the dry run's."""
        tests.share_with_members("sv_gpu")
        rid = self.run_probe(owner=self.ids["bob"])
        events, seen = [], []
        S.get().add_listener(events.append)
        worker = threading.Thread(target=self.runner._run, args=(rid,), daemon=True)
        worker.start()
        while worker.is_alive():
            seen.append(((self.runner.lives.get(rid) or {}).get("case"), tuple(labels("sv_gpu"))))
            time.sleep(0.01)
        worker.join()
        run = self.runner.get_run(rid)
        self.assertEqual(run["status"], "finished", run)
        by_case = {}
        for case, held in seen:
            by_case.setdefault(case, set()).add(held)
        self.assertEqual(by_case.get("gift", set()) | by_case.get("hello", set()), {()},
                         "no lease while the offer and message cases play")
        self.assertIn(("probe run “pooled run”",), by_case.get("turn", set()), "the turn case's lease")
        self.assertEqual(labels("sv_gpu"), [], "gone after the case")
        kinds = {(e.cls, e.task_class, e.activity, e.consumer) for e in events}
        self.assertEqual(kinds, {("probe", "partial", f"probe:{rid}", self.ids["bob"]),
                                 ("probe_case", "full", f"probe:{rid}", self.ids["bob"])}, kinds)
        results = {r["case"]: r for r in self.runner.results(rid)}
        self.assertEqual((results["gift"]["outcome"], results["gift"]["passed"]), ("accept", True), results["gift"])
        self.assertEqual(results["hello"]["outcome"], "reply")
        self.assertEqual((results["turn"]["outcome"], results["turn"]["passed"]), ("end_turn", True))
        for r in results.values():
            self.assertIsNone(r["error"], r)

    def test_a_case_whose_prompt_is_refused_says_why(self):
        """dave's grant on alice's machine covers pooled prompts, gina's games only: a probe run of gina's has its
        offer case's answer refused when its ticket is made, and the case says why, scored for nobody (the model was
        never asked); dave's answers."""
        rid = self.run_probe(owner=self.ids["gina"], llm={"server_id": self.box, "model": "dry-run"}, cases=("gift",))
        self.runner._run(rid)
        rec = self.runner.results(rid)[0]
        self.assertEqual((rec["outcome"], rec["passed"]), ("error", None), rec)
        self.assertIn("The prompt was not asked", rec["error"] or "")
        self.assertIn("pooled prompts", rec["error"])
        rid = self.run_probe(owner=self.ids["dave"], llm={"server_id": self.box, "model": "dry-run"}, cases=("gift",))
        self.runner._run(rid)
        self.assertEqual(self.runner.results(rid)[0]["outcome"], "accept")

    def _seat_mid_turn(self):
        """A lobby game's seat on the one-slot machine, mid-turn with a request of its own on the slot: a prompt waits
        in the machine's line behind it. Returns (the lease, a function that ends the turn)."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        seat.activate()
        grant = seat.acquire()

        def end_turn():
            seat.finish(grant, gen_s=0.01)
            seat.idle()
        return seat, end_turn

    def test_stopping_a_run_cancels_an_answer_waiting_for_its_machine(self):
        """A lobby seat mid-turn holds the one slot: the run's offer case's answer waits in the machine's line;
        stopping the run cancels that ticket, and the run ends at once, stopped, the case not scored."""
        _seat, end_turn = self._seat_mid_turn()
        self.addCleanup(end_turn)
        rid = self.run_probe(cases=("gift", "hello"))
        worker = threading.Thread(target=self.runner._run, args=(rid,), daemon=True)
        worker.start()
        self.assertTrue(until(lambda: self.runner.asking.get(rid) is not None
                              and S.get().machine("sv_gpu").view()["waiting"]), "the answer waits for the machine")
        self.assertNotIn("ticket", json.dumps(self.runner.status(), default=str).lower(),
                         "the probe page's status carries no ticket")
        t0 = time.perf_counter()
        self.runner.stop(rid)
        worker.join(15)
        self.assertFalse(worker.is_alive())
        self.assertLess(time.perf_counter() - t0, 10.0)
        self.assertEqual(self.runner.get_run(rid)["status"], "stopped")
        self.assertEqual(S.get().machine("sv_gpu").view()["waiting"], [], "nothing left in the line")
        rec = self.runner.results(rid)[0]
        self.assertEqual((rec["outcome"], rec["passed"]), ("stopped", None), rec)

    def test_a_stop_that_lands_while_the_answers_ticket_is_made_cancels_it(self):
        """The run is stopped after the answer's ticket is made and before the runner knows of it (so ``stop`` finds
        nothing to cancel): the answer notices the stop and cancels its own ticket, rather than waiting up to an hour
        for the slot a lobby seat holds."""
        _seat, end_turn = self._seat_mid_turn()
        self.addCleanup(end_turn)
        stopped: list = []

        def on_ticket(ticket):
            if ticket is not None and not stopped:
                stopped.append(ticket)      # the stop lands here: the runner never heard of this ticket
        t0 = time.perf_counter()
        with mock.patch.object(self.P, "PROBE_START_SECONDS", 60.0):
            rec = self.P.run_case(self.manager, self.P.engine_api.load_scenario("pooled-scn"), self.probe,
                                  self.probe["cases"][0], {"server_id": "sv_gpu", "model": "dry-run"},
                                  should_stop=lambda: bool(stopped), on_ticket=on_ticket, run_id="r-race")
        self.assertLess(time.perf_counter() - t0, 10.0, "the answer did not wait for its slot")
        self.assertEqual(stopped[0].state, "cancelled")
        self.assertEqual((rec["outcome"], rec["passed"]), ("stopped", None), rec)
        self.assertEqual(S.get().machine("sv_gpu").view()["waiting"], [])

    def _interrupting(self, seat, times: int):
        """The dry run's answer in two steps (text first, then its tool call), the lobby seat's turn beginning between
        them the first ``times`` times it is asked: the seat asks for the slot the answer borrowed, so the answer's
        second step is refused (``yielded``); the seat plays its turn's one request and goes idle again. The chat keeps
        one conversation across the tries (package 3-11), so the interruptions are counted over the case."""
        from citar.agents.providers import dryrun
        from citar.agents.providers.base import StepResult
        original = dryrun.DryRunConversation._negotiate
        left = {"n": times}

        def seat_turn():
            seat.activate()
            grant = seat.acquire()
            time.sleep(0.05)
            seat.finish(grant, gen_s=0.05)
            seat.idle()

        def two_steps(conv):
            if left["n"] > 0:
                left["n"] -= 1
                threading.Thread(target=seat_turn, daemon=True).start()
                self.assertTrue(until(lambda: S.get().machine("sv_gpu").view()["lease_requests"], 5))
                return StepResult(text="Let me think about this offer.", tool_calls=[])
            conv._step = 1                  # the next step answers as the dry run's first would
            return original(conv)
        return mock.patch.object(dryrun.DryRunConversation, "_negotiate", two_steps)

    def test_an_answer_a_lobby_seat_cut_off_is_asked_again(self):
        """The subject's answer runs on the slot an idle lobby seat lends; the seat's turn begins between the answer's
        two steps and takes its slot back, so the answer's ticket is refused its second step. That is the machine's
        contention, not the model's answer: it is asked again on a new ticket once the seat is idle, and the case is the
        model's own (accept, passed), as on a free machine."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        seat.idle()
        events: list = []
        S.get().add_listener(events.append)
        with self._interrupting(seat, 1):
            rec = self.P.run_case(self.manager, self.P.engine_api.load_scenario("pooled-scn"), self.probe,
                                  self.probe["cases"][0], {"server_id": "sv_gpu", "model": "dry-run"},
                                  run_id="r-yield", run_name="yield run")
        self.assertEqual((rec["outcome"], rec["passed"], rec["error"]), ("accept", True, None), rec)
        self.assertEqual(sum(1 for e in events if e.cls == "probe"), 2, "the cut-off answer's step, then the answer")
        self.assertEqual([c["tool"] for c in rec["tool_calls"]], ["respond_negotiation"])

    def test_an_answer_cut_off_every_time_is_not_scored(self):
        """An answer cut off on every try (``PROBE_ANSWER_TRIES``) records why and is not scored as the model's."""
        seat = S.get().lease("sv_gpu", "dry-run", kind="game", label="game “lobby”", ref=("game", "g-lobby"))
        self.addCleanup(seat.release)
        seat.idle()
        with self._interrupting(seat, 2), mock.patch.object(self.P, "PROBE_ANSWER_TRIES", 2):
            rec = self.P.run_case(self.manager, self.P.engine_api.load_scenario("pooled-scn"), self.probe,
                                  self.probe["cases"][0], {"server_id": "sv_gpu", "model": "dry-run"},
                                  run_id="r-yield", run_name="yield run")
        self.assertEqual((rec["outcome"], rec["passed"]), ("error", None), rec)
        self.assertIn("cut off 2 times", rec["error"] or "")

    def test_a_benchmark_that_takes_the_machine_mid_answer_has_the_answer_asked_again(self):
        """A benchmark takes the machine while the answer's first step runs: the scheduler cancels the ticket and
        closes the conversation on it. The answer is asked again on a fresh ticket, in a new conversation (the closed
        one refuses every step), and the case is the model's own answer (accept, passed), not a non-answer."""
        from citar.agents.providers import dryrun
        from citar.agents.providers.base import StepResult
        original = dryrun.DryRunConversation._negotiate
        held = {}

        def first_cut(conv):
            if not held:
                held["bench"] = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="benchmark run “b”",
                                              ref=("benchmark", "b"))
                threading.Timer(1.0, held["bench"].release).start()
                return StepResult(text="Let me think about this offer.", tool_calls=[])
            conv._step = 1
            return original(conv)
        with mock.patch.object(dryrun.DryRunConversation, "_negotiate", first_cut), \
                mock.patch.object(self.P, "PROBE_RETRY_SECONDS", 0.2):
            rec = self.P.run_case(self.manager, self.P.engine_api.load_scenario("pooled-scn"), self.probe,
                                  self.probe["cases"][0], {"server_id": "sv_gpu", "model": "dry-run"},
                                  run_id="r-bench-mid")
        self.assertEqual((rec["outcome"], rec["passed"], rec["error"]), ("accept", True, None), rec)
        self.assertEqual([c["tool"] for c in rec["tool_calls"]], ["respond_negotiation"])

    def test_a_benchmark_that_takes_the_machine_between_cases_is_waited_for(self):
        """A benchmark takes the machine after the run's first case: the run waits for it at the job level, saying so
        (its status and waiting note), with no prompt waiting silently in the machine's line, and its next case
        answers once the benchmark lets the machine go."""
        self.runner.BENCHMARK_POLL_SECONDS = 0.1
        rid = self.run_probe(cases=("gift", "hello"))
        held = {}
        original = self.P.run_case

        def run_case(*a, **kw):
            rec = original(*a, **kw)
            if rec["case"] == "gift":
                held["bench"] = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="benchmark run “b”",
                                              ref=("benchmark", "b"))
            return rec
        with mock.patch.object(self.P, "run_case", run_case):
            worker = threading.Thread(target=self.runner._run, args=(rid,), daemon=True)
            worker.start()
            self.assertTrue(until(lambda: self.runner.get_run(rid)["status"] == "waiting (machine busy)"),
                            self.runner.get_run(rid))
            run = self.runner.get_run(rid)
            self.assertEqual(run["waiting"], "waiting for someone's benchmark run")
            time.sleep(0.3)
            self.assertEqual(S.get().machine("sv_gpu").view()["waiting"], [], "no prompt waits in the line")
            self.assertEqual(len(self.runner.results(rid)), 1, "the next case waits")
            held["bench"].release()
            worker.join(30)
        self.assertFalse(worker.is_alive())
        run = self.runner.get_run(rid)
        self.assertEqual((run["status"], run.get("waiting")), ("finished", None))
        results = {r["case"]: r for r in self.runner.results(rid)}
        self.assertEqual((results["gift"]["outcome"], results["hello"]["outcome"]), ("accept", "reply"))
        self.assertIsNone(results["hello"]["error"])

    def test_a_benchmark_that_takes_the_machine_from_a_waiting_answer_is_waited_for(self):
        """The answer's ticket waits behind a lobby seat mid-turn when a benchmark takes the machine: the ticket is
        refused (a probe's prompt does not wait for benchmarks), the run waits for the benchmark at the job level,
        saying so, and the answer is asked once the benchmark is over: the model's own answer, not an error."""
        self.runner.BENCHMARK_POLL_SECONDS = 0.1
        _seat, end_turn = self._seat_mid_turn()
        rid = self.run_probe(cases=("gift",))
        worker = threading.Thread(target=self.runner._run, args=(rid,), daemon=True)
        worker.start()
        self.assertTrue(until(lambda: S.get().machine("sv_gpu").view()["waiting"]), "the answer waits in the line")
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="benchmark run “b”", ref=("benchmark", "b"))
        self.addCleanup(bench.release)
        self.assertTrue(until(lambda: self.runner.get_run(rid)["status"] == "waiting (machine busy)"),
                        self.runner.get_run(rid))
        self.assertEqual(S.get().machine("sv_gpu").view()["waiting"], [], "refused, not waiting silently")
        end_turn()
        bench.release()
        worker.join(30)
        self.assertFalse(worker.is_alive())
        rec = self.runner.results(rid)[0]
        self.assertEqual((rec["outcome"], rec["passed"], rec["error"]), ("accept", True, None), rec)

    def test_a_case_played_with_no_runner_waits_out_a_benchmark_within_its_window(self):
        """A case played on its own (no runner to wait at the job level): its answer, refused while a benchmark holds
        the machine, is asked again within its start window and answered once the benchmark is over."""
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="benchmark run “b”", ref=("benchmark", "b"))
        self.addCleanup(bench.release)
        threading.Timer(1.0, bench.release).start()
        with mock.patch.object(self.P, "PROBE_RETRY_SECONDS", 0.2):
            rec = self.P.run_case(self.manager, self.P.engine_api.load_scenario("pooled-scn"), self.probe,
                                  self.probe["cases"][0], {"server_id": "sv_gpu", "model": "dry-run"}, run_id="r-alone")
        self.assertEqual((rec["outcome"], rec["passed"], rec["error"]), ("accept", True, None), rec)

    def test_a_model_the_runner_loads_is_told_to_the_scheduler(self):
        """The runner loads a run's model on a machine CITAR manages: the scheduler is told at once
        (``models_changed``), so the answer's ticket made next reads the model now loaded, not its cached list."""
        sv = {"id": "sv_gpu", "name": "GPU", "connection": {"manage_loading": True}}
        with mock.patch.object(servers, "find", return_value=sv), \
                mock.patch.object(servers, "model_entry", return_value={}), \
                mock.patch.object(servers, "profile", return_value=None), \
                mock.patch.object(servers, "ensure_model", return_value=3.0), \
                mock.patch.object(S.get(), "models_changed") as told:
            self.assertTrue(self.runner._prepare_model({"id": "r-load", "llm": {"server_id": "sv_gpu", "model": "m"}}))
        told.assert_called_with("sv_gpu")
        self.assertEqual(self.runner._loaded.get("sv_gpu"), "m")

    def test_a_runs_purposes_follow_its_cases(self):
        P = self.P
        self.assertEqual(P.case_purposes(self.probe), ("probe", "pooled"))
        self.assertEqual(P.case_purposes(self.probe, ["turn"]), ("probe",))
        self.assertEqual(P.case_purposes(self.probe, ["gift", "hello"]), ("pooled",))
        self.assertEqual(P.case_purposes({"cases": []}), ("probe",))


class ProbeRunRoute(_ProbeScenario):
    """``POST /api/probes/{id}/run`` admits the run's subject on its machine for what its chosen cases are (P3.3.7):
    ``probe`` for a turn case (a lease), ``pooled`` for an offer or message case (pooled prompts, which also need
    ``partial_llm``). gina's grant on alice's machine is changed to cover probes only; dave's covers pooled prompts
    only. The runner is a stub: what is tested is who may start the run."""

    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar import probes as P
        from citar.db.models import AccessGrant
        from citar.server import app as appmod
        from sqlalchemy import select
        with db.session() as s:
            scn = items.register(s, "scenario", "pooled-scn", self.ids["alice"], "Pooled scenario")
            scn.visibility = "server"
            row = items.register(s, "probe", "pooled-probe", self.ids["alice"], "Pooled probe")
            row.visibility = "server"
            gina = s.scalars(select(AccessGrant).where(AccessGrant.subject_user_id == self.ids["gina"])).one()
            gina.purposes = ["probe"]
        P.save_probe(dict(self.probe))
        self.addCleanup(lambda: P.delete_probe("pooled-probe"))
        self.started: list = []
        self.started_kw: list = []
        stub = mock.Mock()
        stub.start.side_effect = lambda pid, llm, repeats, name, cases, **kw: self.started.append(
            (kw.get("owner_id"), cases)) or self.started_kw.append(kw) or {"id": "stub-run", "llm": llm}
        patcher = mock.patch.object(appmod, "_probes", return_value=stub)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)

    def ask_run(self, who, cases):
        return self.client.post("/api/probes/pooled-probe/run", headers=self.headers[who], json={
            "llm": {"server_id": self.box, "model": "dry-run"}, **({"cases": cases} if cases else {})})

    def test_the_subject_is_admitted_for_what_its_cases_are(self):
        for who, cases, want, says in (
                ("gina", ["gift"], 403, "pooled prompts"),          # a probes-only grant: no offer case
                ("gina", ["hello"], 403, "pooled prompts"),
                ("gina", None, 403, "pooled prompts"),               # every case: the offer and message ones too
                ("gina", ["turn"], 200, None),                       # a turn case is a probe's lease
                ("dave", ["gift", "hello"], 200, None),              # a pooled-only grant: offer and message cases
                ("dave", ["turn"], 403, "probes"),                   # but no turn case
                ("dave", None, 403, "probes"),
                ("bob", ["turn"], 403, "probes"),                    # games and pooled prompts: no probe's lease
                ("bob", ["gift"], 200, None)):
            with self.subTest(who=who, cases=cases):
                got = self.ask_run(who, cases)
                self.assertEqual(got.status_code, want, got.text[:300])
                if says:
                    self.assertIn(says, got.json()["detail"])
        self.assertEqual(self.started, [(self.ids["gina"], ["turn"]), (self.ids["dave"], ["gift", "hello"]),
                                        (self.ids["bob"], ["gift"])])

    def test_the_list_says_what_a_run_is_admitted_for(self):
        """``GET /api/probes`` gives each probe's ``purposes`` (``case_purposes`` of all its cases), which the run
        dialog lists the machines by, so a refusal shows before the run is asked."""
        got = self.client.get("/api/probes", headers=self.headers["gina"])
        self.assertEqual(got.status_code, 200, got.text[:300])
        row = next(x for x in got.json() if x["id"] == "pooled-probe")
        self.assertEqual(row["purposes"], ["probe", "pooled"])
        from citar import probes as P
        P.save_probe({**self.probe, "cases": [c for c in self.probe["cases"] if c["kind"] == "turn"]})
        row = next(x for x in self.client.get("/api/probes", headers=self.headers["gina"]).json()
                   if x["id"] == "pooled-probe")
        self.assertEqual(row["purposes"], ["probe"])

    def test_a_hybrid_run_is_admitted_for_pooled_prompts_only(self):
        """A hybrid run (3-16, P3.6.9) holds no lease: its offer and message answers and its turn cases' diplomat
        phases are all pooled prompts, so a grant for probes alone does not cover it, and one for pooled prompts
        does; its settings are checked first (400 with the reason), and its subject is a model, never the bot."""
        from citar import probes as P
        self.assertEqual(P.case_purposes(self.probe, None, "hybrid"), ("pooled",))
        for who, body, want, says in (
                ("gina", {"cases": ["turn"], "mode": "hybrid"}, 403, "pooled prompts"),
                ("dave", {"cases": ["turn"], "mode": "hybrid"}, 200, None),      # pooled-only: a hybrid turn case
                ("dave", {"mode": "hybrid", "owners": {"trades": "llm"}}, 200, None),
                ("dave", {"mode": "hybrid", "owners": "most"}, 400, "Unknown preset"),
                ("dave", {"mode": "hybrid", "owners": {}}, 400, "nothing for its model"),
                ("dave", {"mode": "sideways"}, 400, "mode"),
                ("dave", {"mode": "hybrid", "llm": {"provider": "bot"}}, 400, "not the scripted bot"),
                ("alice", {"mode": "hybrid"}, 200, None),                      # the machine's and the probe's owner
                ("zara", {"mode": "hybrid"}, 200, None)):                      # an administrator
            with self.subTest(who=who, body=body):
                got = self.client.post("/api/probes/pooled-probe/run", headers=self.headers[who], json={
                    "llm": {"server_id": self.box, "model": "dry-run"}, **body})
                self.assertEqual(got.status_code, want, got.text[:300])
                if says:
                    self.assertIn(says, got.json()["detail"])
        self.assertEqual(self.started, [(self.ids["dave"], ["turn"]), (self.ids["dave"], None),
                                        (self.ids["alice"], None), (self.ids["zara"], None)])
        self.assertEqual([(kw["mode"], kw["owners"]) for kw in self.started_kw],
                         [("hybrid", None), ("hybrid", {"trades": "llm"}), ("hybrid", None), ("hybrid", None)])
        self.assertEqual(self.client.post("/api/probes/pooled-probe/run", headers=self.headers["anon"], json={
            "llm": {"server_id": self.box, "model": "dry-run"}, "mode": "hybrid"}).status_code, 401)
        with db.session() as s:
            items.get(s, "probe", "pooled-probe").visibility = "private"
        stranger = self.client.post("/api/probes/pooled-probe/run", headers=self.headers["dave"], json={
            "llm": {"server_id": self.box, "model": "dry-run"}, "mode": "hybrid"})
        self.assertEqual(stranger.status_code, 404, "another member's private probe")

    def test_pooled_cases_need_partial_llm(self):
        with db.session() as s:
            s.get(User, self.ids["dave"]).caps = {"partial_llm": False}
        policy.invalidate()
        got = self.ask_run("dave", ["gift"])
        self.assertEqual(got.status_code, 403, got.text[:300])
        self.assertEqual(got.json()["detail"], policy.denial(self.user("dave"), "partial_llm"))
        self.assertEqual(self.started, [])


class HybridProbes(_ProbeScenario):
    """Package 3-16's gate 5 (P3.6.9): a hybrid probe run seats its subject as a hybrid seat, trades and talk its
    model's (``trade_only``); each offer or message goes to the seat's bot first and to its model when the model owns
    it, and the run records which side answered each case; a turn case plays the bot's turn, then a diplomat phase
    with every trigger forced."""

    def setUp(self):
        super().setUp()
        from citar import probes as P
        from citar.server.session import SessionManager
        self.P = P
        self.manager = SessionManager()
        self.addCleanup(lambda: [self.manager.delete(sid) for sid in list(self.manager.sessions)])
        runner = P.ProbeRunner.__new__(P.ProbeRunner)
        runner.manager, runner.lock, runner._stop_run, runner._thread = self.manager, threading.Lock(), set(), None
        runner.lives, runner._workers, runner._loaded, runner.queue, runner.asking = {}, {}, {}, [], {}
        written, write = [], runner._write

        def write_and_remember(run):
            written.append(run["id"])
            write(run)
        runner._write = write_and_remember
        self.addCleanup(lambda: [shutil.rmtree(runner._run_dir(rid), ignore_errors=True) for rid in written])
        self.runner = runner

    def test_a_hybrid_run_records_which_side_answered_each_case(self):
        dof = [{"type": "declaration_of_friendship"}]
        probe = dict(self.probe, cases=self.probe["cases"] + [
            {"id": "friends", "kind": "offer", "message": "Friends?", "give": dof, "receive": dof}])
        rid = "pooled-hybrid-run"
        self.runner._write({"id": rid, "name": "hybrid run", "probe": probe,
                            "llm": {"server_id": self.box, "model": "dry-run"}, "repeats": 1,
                            "jobs": [{"case": c["id"], "rep": 0} for c in probe["cases"]], "status": "queued",
                            "done": 0, "created": time.time(), "started": None, "finished": None, "summary": {},
                            "owner_id": self.ids["bob"], "mode": "hybrid",
                            "hybrid": self.P.hybrid_settings("hybrid", {}, "trade_only")})
        events: list = []
        S.get().add_listener(events.append)
        seen: list = []
        worker = threading.Thread(target=self.runner._run, args=(rid,), daemon=True)
        worker.start()
        while worker.is_alive():
            seen.append(tuple(labels(self.box)))
            time.sleep(0.01)
        worker.join()
        run = self.runner.get_run(rid)
        self.assertEqual(run["status"], "finished", run)
        res = {r["case"]: r for r in self.runner.results(rid)}
        self.assertEqual({c: r.get("answered_by") for c, r in res.items()},
                         {"gift": "model", "hello": "model", "friends": "bot", "turn": "model"}, res)
        self.assertEqual(res["gift"]["answers"], ["model"])
        self.assertEqual(res["gift"]["outcome"], "accept", "the dry-run model takes a gift")
        self.assertNotEqual(res["friends"]["outcome"], "no_response")
        self.assertIsNone(res["friends"].get("error"))
        phases = res["turn"]["diplomat"]
        self.assertEqual([x["outcome"] for x in phases], ["done"])
        self.assertTrue(any("probe of your diplomacy" in t for t in phases[0]["triggers"]), phases)
        self.assertEqual(res["turn"]["end_reason"], "end_turn")
        self.assertEqual(set(seen), {()}, "a hybrid run holds no lease, not even for its turn case")
        classes = {e.task_class for e in events if e.machine == self.box}
        self.assertEqual(classes, {"partial"}, "every model call a pooled prompt")
        self.assertEqual(run["summary"]["cases"]["friends"]["answered_by"], {"bot": 1})
        self.assertEqual(run["summary"]["cases"]["gift"]["answered_by"], {"model": 1})

    def test_a_case_whose_model_prompt_did_not_run_was_answered_by_nobody(self):
        """The bot left the gift to the model, whose ticket was refused: nobody answered it (the case's error says
        why), not its model."""
        dof = [{"type": "declaration_of_friendship"}]
        probe = dict(self.probe, cases=[c for c in self.probe["cases"] if c["id"] == "gift"] + [
            {"id": "friends", "kind": "offer", "message": "Friends?", "give": dof, "receive": dof}])
        rid = "pooled-hybrid-refused"
        self.runner._write({"id": rid, "name": "hybrid run", "probe": probe,
                            "llm": {"server_id": self.box, "model": "dry-run"}, "repeats": 1,
                            "jobs": [{"case": c["id"], "rep": 0} for c in probe["cases"]], "status": "queued",
                            "done": 0, "created": time.time(), "started": None, "finished": None, "summary": {},
                            "owner_id": self.ids["bob"], "mode": "hybrid",
                            "hybrid": self.P.hybrid_settings("hybrid", {}, "trade_only")})
        refusal = "The prompt was not asked: the machine refused it (a test)."
        with mock.patch.object(self.P, "_probe_ticket", return_value=(None, refusal)) as asked:
            self.runner._run(rid)
        self.assertTrue(asked.called, "the model was asked for the gift")
        res = {r["case"]: r for r in self.runner.results(rid)}
        self.assertEqual((res["gift"]["answered_by"], res["gift"]["answers"]), ("none", ["none"]))
        self.assertEqual((res["gift"]["outcome"], res["gift"]["error"]), ("error", refusal))
        self.assertEqual(res["friends"]["answered_by"], "bot")
        self.assertEqual(self.runner.get_run(rid)["summary"]["cases"]["gift"]["answered_by"], {"none": 1})


class HybridSuiteRoutes(World):
    """Package 3-16 (P3.6.9): a benchmark run whose suite has a hybrid scenario is admitted for pooled prompts as well
    as benchmarks on its machines (its jobs' models are asked pooled prompts), a full suite for benchmarks alone; a
    suite's hybrid settings are checked when it is saved (400 naming the scenario), and its hybrid seat's bot is one the
    member may use. gina's grant on alice's machines is changed to cover benchmarks only, bob's to benchmarks and
    pooled prompts. The scheduler is a stub: what is tested is who may start the run."""

    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.db.models import AccessGrant
        from citar.server import app as appmod
        from sqlalchemy import select
        with db.session() as s:
            for handle, purposes in (("gina", ["benchmark"]), ("bob", ["benchmark", "pooled"])):
                grant = s.scalars(select(AccessGrant).where(AccessGrant.subject_user_id == self.ids[handle])).one()
                grant.purposes = purposes
            items.register(s, "bot_profile", "carols-bot", self.ids["carol"], "Carol's bot")
        self.started: list = []
        stub = mock.Mock()
        stub.create_run.side_effect = lambda suite, name, **kw: self.started.append(
            (kw.get("owner_id"), [sc.get("mode") for sc in suite["scenarios"]])) or {"id": "stub-run", "jobs": []}
        stub.save_suite.side_effect = lambda suite: suite
        stub._suite_path.side_effect = lambda sid: _TMP / f"{sid}.json"
        patcher = mock.patch.object(appmod, "_scheduler", return_value=stub)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)

    def suite(self, mode: str, **hybrid) -> dict:
        sc = {"name": "Tiny", "mode": mode, "map_size": "duel", "map_type": "pangaea", "opponents": 1, "turn_limit": 2}
        if mode == "hybrid":
            sc["hybrid"] = {"owners": "trade_only", **hybrid}
        return {"name": "suite", "servers": [{"server_id": self.box, "models": [{"model": "dry-run"}]}],
                "scenarios": [sc]}

    def test_a_hybrid_run_is_admitted_for_pooled_prompts_too(self):
        for who, mode, want, says in (
                ("gina", "full", 200, None),                    # benchmarks only: a full run
                ("gina", "hybrid", 403, "pooled prompts"),      # but its hybrid jobs ask pooled prompts
                ("bob", "hybrid", 200, None),                   # benchmarks and pooled prompts
                ("dave", "hybrid", 403, "benchmark"),           # pooled prompts but no benchmarks
                ("carol", "hybrid", 403, "may not use"),        # no grant, and not her machine to see
                ("anon", "hybrid", 401, None)):
            with self.subTest(who=who, mode=mode):
                got = self.client.post("/api/benchmarks/runs", headers=self.headers[who],
                                       json={"suite": self.suite(mode)})
                self.assertEqual(got.status_code, want, got.text[:300])
                if says:
                    self.assertIn(says, got.json()["detail"])
        self.assertEqual(self.started, [(self.ids["gina"], ["full"]), (self.ids["bob"], ["hybrid"])])
        self.assertEqual(self.client.post("/api/benchmarks/runs", headers=self.headers["alice"],
                                          json={"suite": self.suite("hybrid")}).status_code, 200, "the owner")
        self.assertEqual(self.client.post("/api/benchmarks/runs", headers=self.headers["zara"],
                                          json={"suite": self.suite("hybrid")}).status_code, 200, "an administrator")

    def test_a_suites_hybrid_settings_are_checked_when_it_is_saved(self):
        for hybrid, want, says in (({"owners": {}}, 400, "nothing for its model"),
                                   ({"handicap": "robot"}, 400, "handicap"),
                                   ({"prompt_every": 99}, 400, "prompt_every"),
                                   ({"bot_profile": "carols-bot"}, 404, None),
                                   ({"bot_profile": "best"}, 200, None)):
            with self.subTest(hybrid=hybrid):
                got = self.client.post("/api/benchmarks/suites", headers=self.headers["bob"],
                                       json=self.suite("hybrid", **hybrid))
                self.assertEqual(got.status_code, want, got.text[:300])
                if says:
                    self.assertIn(says, got.json()["detail"])
                    self.assertIn("Tiny", got.json()["detail"], "the scenario is named")
        got = self.client.post("/api/benchmarks/suites", headers=self.headers["carol"],
                               json=self.suite("hybrid", bot_profile="carols-bot"))
        self.assertNotEqual(got.status_code, 404, "her own bot is hers to use")


# ---------------------------------------------------------------------------- gate 4: the Queue page

class QueuePage(World):
    """The one-slot "Alice box" holds alice's game's seat mid-request; in its line wait alice's report, bob's report
    (ray may read it) and bob's test; carol's private machine holds a game of hers."""

    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self._quiet_work_queue()
        with db.session() as s:
            items.register(s, "report", "r-alice", self.ids["alice"], "alice's secret report")
            row = items.register(s, "report", "r-bob", self.ids["bob"], "bob's report")
            access.grant(s, row, s.get(User, self.ids["ray"]), access.VIEW, actor=s.get(User, self.ids["bob"]))
        sched = S.get()
        self.seat = sched.lease(self.box, "dry-run", kind="game", label="game “alice's secret game”",
                                consumer=self.ids["alice"], ref=("game", "g-alice"))
        self.addCleanup(self.seat.release)
        self.seat.activate()
        self.seat_grant = self.seat.acquire()
        self.carol_game = sched.lease(self.carols, "dry-run", kind="game", label="game “carol's hidden game”",
                                      consumer=self.ids["carol"], ref=("game", "g-carol"))
        self.addCleanup(self.carol_game.release)
        m = sched.machine(self.box)
        self.tickets = {}
        for key, handle, cls, ref, label in (
                ("alice", "alice", "report", ("report", "r-alice"), "report “alice's secret report”"),
                ("bob", "bob", "report", ("report", "r-bob"), "report “bob's report”"),
                ("test", "bob", "test", None, "test of Alice box")):
            t = self.ticket(self.box, handle, cls=cls, ref=ref, label=label)
            self.assertEqual(t.state, "queued", t.reason)
            m.submit(t)
            self.tickets[key] = t
            self.addCleanup(t.close)

    def _quiet_work_queue(self) -> None:
        """The shared work queue (``citar.pool.queue``) lists nothing but this world's work while a test runs: its
        producers are process-wide, and one an earlier module left registered (a benchmark scheduler of its own, a job
        still holding its machine) would count as another busy machine (``others_busy``). Each is put back at the
        test's end, unless something registered its kind again meanwhile."""
        def nothing():
            return []
        replaced = []
        for table in (work_queue._providers, work_queue._running):
            for kind, fn in list(table.items()):
                replaced.append((table, kind, fn))
                table[kind] = nothing

        def restore():
            for table, kind, fn in replaced:
                if table.get(kind) is nothing:
                    table[kind] = fn
        self.addCleanup(restore)

    def call(self, who, method, path, body=None):
        return self.client.request(method, path, json=body, headers=self.headers[who])

    def queue(self, who) -> dict:
        got = self.call(who, "GET", "/api/queue")
        self.assertEqual(got.status_code, 200, got.text[:300])
        return got.json()

    def machine(self, q, sid):
        return next((m for m in q["machines"] if m["server_id"] == sid), None)

    def test_the_queue_lists_leases_and_prompts_naming_only_what_the_caller_may_see(self):
        self.assertEqual(self.call("anon", "GET", "/api/queue").status_code, 401)
        # bob: alice's machine (his grant reaches it), not carol's (only counted)
        q = self.queue("bob")
        box = self.machine(q, self.box)
        self.assertIsNotNone(box)
        self.assertIsNone(self.machine(q, self.carols))
        self.assertEqual(q["others_busy"], 1)
        self.assertNotIn("Carol box", json.dumps(q))
        self.assertNotIn("carol's hidden game", json.dumps(q))
        self.assertNotIn("secret", json.dumps(q), "alice's work is not named to bob")
        self.assertEqual((box["capacity"], box["in_flight"]), (1, 1))
        self.assertEqual([(lease["label"], lease["member"], lease["state"]) for lease in box["leases"]],
                         [("someone's game", None, "active")])
        waiting = box["prompts"]["waiting"]
        self.assertEqual([(p["position"], p["class"], p["label"], p["member"]) for p in waiting], [
            (1, "test", "test of Alice box", "bob"),
            (2, "report", "someone's report", None),
            (3, "report", "report “bob's report”", "bob")])
        for p in waiting:
            self.assertEqual(p["model"], "dry-run")
            self.assertGreaterEqual(p["waited_s"], 0.0)
            self.assertIsNotNone(p["eta_s"])
            self.assertIn("starts_within_s", p)
        self.assertEqual([(p["can_manage"], p["can_cancel"], p["id"] is not None, p["ref"]) for p in waiting], [
            (False, True, True, None), (False, False, False, None), (True, True, True, ["report", "r-bob"])])
        self.assertEqual(waiting[2]["eta_s"] >= waiting[0]["eta_s"], True, "the ETA rises with the position")
        # alice, the machine's owner: her own work by name, bob's private report and test as someone's, both hers
        # to cancel
        box = self.machine(self.queue("alice"), self.box)
        self.assertEqual(box["leases"][0]["label"], "game “alice's secret game”")
        self.assertEqual(box["leases"][0]["member"], "alice")
        self.assertEqual([(p["label"], p["member"], p["can_cancel"]) for p in box["prompts"]["waiting"]], [
            ("someone's test", None, True), ("report “alice's secret report”", "alice", True),
            ("someone's report", None, True)])
        # ray reads bob's report: named to him, not his to cancel or move
        q = self.queue("ray")
        ray = {p["label"]: p for p in self.machine(q, self.box)["prompts"]["waiting"]}
        self.assertIn("report “bob's report”", ray)
        self.assertEqual((ray["report “bob's report”"]["member"], ray["report “bob's report”"]["can_cancel"],
                          ray["report “bob's report”"]["can_manage"]), ("bob", False, False))
        # carol: her own machine, and alice's only counted
        q = self.queue("carol")
        self.assertIsNone(self.machine(q, self.box))
        self.assertEqual(q["others_busy"], 1)
        mine = self.machine(q, self.carols)
        self.assertEqual([(lease["label"], lease["member"]) for lease in mine["leases"]],
                         [("game “carol's hidden game”", "carol")])
        self.assertNotIn("Alice box", json.dumps(q))
        # a moderator is a member here; an administrator sees every machine and every name
        self.assertIsNone(self.machine(self.queue("moe"), self.box))
        q = self.queue("zara")
        self.assertEqual(q["others_busy"], 0)
        text = json.dumps(q, ensure_ascii=False)
        for name in ("alice's secret report", "bob's report", "carol's hidden game", "alice's secret game"):
            self.assertIn(name, text)

    def test_priority_is_the_works_managers_and_its_tickets_follow_it(self):
        for who, want in (("alice", 404), ("ray", 403), ("carol", 404), ("anon", 401)):
            with self.subTest(who=who):
                got = self.call(who, "POST", "/api/queue/priority", {"kind": "report", "id": "r-bob", "priority": 50})
                self.assertEqual(got.status_code, want, got.text[:300])
        self.assertEqual(self.tickets["bob"].queue_priority, 0)
        got = self.call("bob", "POST", "/api/queue/priority", {"kind": "report", "id": "r-bob", "priority": 50})
        self.assertEqual(got.status_code, 200, got.text[:300])
        self.addCleanup(work_queue.set_priority, "report", "r-bob", 0)
        self.assertEqual((self.tickets["bob"].queue_priority, self.tickets["bob"].priority),
                         (50, S.BASE_PRIORITY["report"] + 50))
        waiting = self.machine(self.queue("bob"), self.box)["prompts"]["waiting"]
        self.assertEqual([(p["position"], p["label"], p["priority"]) for p in waiting][0],
                         (1, "report “bob's report”", 50), "it moved to the front of the line")
        got = self.call("zara", "POST", "/api/queue/priority", {"kind": "report", "id": "r-alice", "priority": -5})
        self.assertEqual(got.status_code, 200)
        self.addCleanup(work_queue.set_priority, "report", "r-alice", 0)
        self.assertEqual(self.tickets["alice"].queue_priority, -5)

    def test_cancel_by_the_works_owner_the_machines_owner_and_administrators(self):
        path = "/api/queue/tickets/{}/cancel"
        alice_t, bob_t, test_t = self.tickets["alice"], self.tickets["bob"], self.tickets["test"]
        self.assertEqual(self.call("anon", "POST", path.format(bob_t.id)).status_code, 401)
        for who, ticket, want in (("carol", bob_t, 404), ("moe", bob_t, 404), ("bob", alice_t, 404),
                                  ("ray", bob_t, 403), ("dave", test_t, 404)):
            with self.subTest(who=who, ticket=ticket.label):
                got = self.call(who, "POST", path.format(ticket.id))
                self.assertEqual(got.status_code, want, got.text[:300])
                self.assertEqual(ticket.state, "queued", "nothing was cancelled")
        self.assertEqual(self.call("bob", "POST", path.format("not-a-ticket")).status_code, 404)
        got = self.call("alice", "POST", path.format(test_t.id))            # the machine's owner
        self.assertEqual((got.status_code, got.json().get("state")), (200, "cancelled"), got.text[:300])
        got = self.call("bob", "POST", path.format(bob_t.id))               # the work's owner
        self.assertEqual((got.status_code, got.json().get("state")), (200, "cancelled"), got.text[:300])
        got = self.call("zara", "POST", path.format(alice_t.id))            # an administrator
        self.assertEqual((got.status_code, got.json().get("state")), (200, "cancelled"), got.text[:300])
        self.assertEqual(self.machine(self.queue("zara"), self.box)["prompts"]["waiting"], [])
        self.assertEqual(self.call("alice", "POST", path.format(alice_t.id)).status_code, 404, "gone from the line")
        from citar.db.models import AuditLog
        from sqlalchemy import select
        with db.session() as s:
            rows = list(s.scalars(select(AuditLog).where(AuditLog.action == "queue.prompt_cancelled")))
        self.assertEqual(len(rows), 3)


if __name__ == "__main__":
    unittest.main()
