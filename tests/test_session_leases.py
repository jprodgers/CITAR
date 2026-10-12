"""Leases for the session's seats (design/phase3-features.md P3.3.3, P3.3.5; package 3-08's gates 1 to 3).

A model seat's lease is made when its game's session is registered (created, loaded paused, brought back by
``restore_live``, reloaded by a benchmark, launched from a scenario) and released when the seat changes or is
eliminated, the game ends, stops, crashes or makes way; ``pool.seats.occupied()`` lists the leases live on a machine,
so a queued benchmark job waits for every game that will want the machine, whoever moves first. A probe run and a
report's narrative hold a non-lending lease for their runs. A benchmark job's start waits for the machine's pooled
requests to drain (``drain_s``); a drain cut short by its bound marks the job ``contaminated`` and leaves its timing out
of the speed score.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import db, servers
from citar.agents.providers.base import Conversation, StepResult
from citar.db.models import Base
from citar.pool import scheduler as S
from citar.pool import seats as pool_seats
from citar.server import session as session_mod
from citar.server.benchmarks import BenchmarkScheduler
from citar.server.session import SessionManager
from tests import has_test_ops

_URL = "sqlite:///" + (Path(tempfile.mkdtemp(prefix="citar_session_leases_")) / "test.db").as_posix()


def setUpModule():
    """This module's own database, with the test registry's machines (its games play ``sv_gpu`` and ``sv_dryrun``)."""
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()


def until(fn, timeout: float = 15.0, step: float = 0.02):
    """Poll ``fn`` until it returns something true (returned) or ``timeout`` passes (its last value)."""
    deadline = time.monotonic() + timeout
    while True:
        value = fn()
        if value or time.monotonic() > deadline:
            return value
        time.sleep(step)


#: A small game with no barbarians, its turn limit far off.
CONFIG = {"map_size": "duel", "seed": 7, "barbarians": "off", "turn_limit": 400}
GPU = {"server_id": "sv_gpu", "model": "dry-run"}


def labels(sid: str) -> list:
    """The labels of the leases live on a machine, as the scheduler holds them."""
    return [lease.label for lease in S.get().leases(sid)]


class LeaseCase(unittest.TestCase):
    """A fresh process scheduler, a session manager whose games are closed afterwards, and benchmark schedulers."""

    def setUp(self):
        S.reset()
        self.addCleanup(S.reset)
        self.manager = SessionManager()
        self.dir = Path(tempfile.mkdtemp(prefix="citar-leases-"))
        self.schedulers: list = []

    def tearDown(self):
        for sch in self.schedulers:
            sch.stop()
            for run in list(sch.runs.values()):
                for job in run["jobs"]:
                    if job.get("game_id") and sch.manager.get(job["game_id"]):
                        sch.manager.delete(job["game_id"])
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
        shutil.rmtree(self.dir, ignore_errors=True)

    def game(self, seats, *, start: bool = True, manager=None, **config):
        """A lobby game, registered and started (its leases made)."""
        return (manager or self.manager).create({**CONFIG, **config}, seats, name=f"leases {len(self.manager.sessions)}",
                                                start=start)

    def bench(self, sid: str = "sv_gpu", model_id: str = "m_gpu", turn_limit: int = 2, manager=None):
        """A benchmark scheduler (ticked by hand) and a one-job run on ``sid``."""
        sch = BenchmarkScheduler(manager or self.manager, self.dir / f"bench-{len(self.schedulers)}", autostart=False)
        self.schedulers.append(sch)
        run = sch.create_run({"name": "leases", "mode": "sequential", "repeats": 1,
                              "servers": [{"server_id": sid, "models": [{"model_id": model_id}]}],
                              "scenarios": [{"name": "tiny", "map_size": "duel", "map_type": "pangaea", "seed": 5,
                                             "opponents": 1, "turn_limit": turn_limit, "barbarians": "off",
                                             "max_turn_minutes": 5}]})
        return sch, run, run["jobs"][0]

    def assert_job_waits_for(self, sid: str, label: str):
        """A one-job benchmark run on ``sid`` stays queued, naming ``label`` as what holds the machine."""
        sch, run, job = self.bench(sid)
        sch.tick()
        sch.tick()
        self.assertEqual(job["status"], "queued", f"the benchmark job waits for {label}: {job}")
        self.assertIn(label, job.get("waiting") or "")
        sch.control_run(run["id"], "cancel")


# ---------------------------------------------------------------------------- gate 1

class Registration(LeaseCase):
    """Gate 1: leases from the session's registration to the seat's or the game's end."""

    def test_a_lobby_game_loaded_paused_holds_its_machine_and_a_benchmark_job_waits(self):
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}])
        path = g.save("leases-loaded")
        self.manager.delete(g.id)
        self.assertEqual(pool_seats.occupied("sv_gpu"), [], "a closed game holds nothing")
        s = self.manager.load(path)
        self.assertTrue(s.paused)
        self.assertEqual(pool_seats.occupied("sv_gpu"), [f"game “{s.name}”"])
        self.assertNotIn(1, s.agents, "made at registration: the seat has been asked nothing")
        self.assertEqual([(o["kind"], o["id"], o["game"]) for o in pool_seats.occupants("sv_gpu")],
                         [("game", s.id, s.id)])
        self.assert_job_waits_for("sv_gpu", f"game “{s.name}”")
        self.assertEqual(pool_seats.occupied("sv_gpu", exclude=frozenset({s.id})), [], "its own games excluded")

    def test_a_game_brought_back_by_restore_live_holds_its_machine_and_a_benchmark_job_waits(self):
        folder = Path(tempfile.mkdtemp(prefix="citar-restore-"))
        self.addCleanup(shutil.rmtree, folder, ignore_errors=True)
        with mock.patch.object(session_mod, "SAVE_DIR", folder):
            g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}])
            self.assertTrue(g.flush_saves(10))
            self.assertTrue((folder / g.id / session_mod.GameSession.LIVE_MARK).exists())
            # the server goes away: the game is dropped from memory with its live mark left on the disk
            g.stop()
            self.manager.sessions.pop(g.id)
            self.assertEqual(pool_seats.occupied("sv_gpu"), [])
            manager2 = SessionManager()
            restored = manager2.restore_live()
            self.addCleanup(lambda: [manager2.delete(sid) for sid in list(manager2.sessions)])
            s = manager2.get(g.id)
            self.assertIsNotNone(s, restored)
            self.assertFalse(s.paused, "it was playing, so it plays again")
            self.assertEqual(pool_seats.occupied("sv_gpu"), [f"game “{s.name}”"])
            self.assertNotIn(1, s.agents)
            self.assert_job_waits_for("sv_gpu", f"game “{s.name}”")

    def test_a_game_whose_bots_move_first_holds_its_machine_before_its_model_seat_plays(self):
        g = self.game([{"type": "bot"}, {"type": "bot"}, {"type": "llm", "llm": GPU}], start=False, map_size="small")
        g.ai_delay = 5.0                    # the driver rests after each bot's turn: the model seat's comes much later
        g.start()
        self.assertEqual(pool_seats.occupied("sv_gpu"), [f"game “{g.name}”"], "held from its start")
        self.assert_job_waits_for("sv_gpu", f"game “{g.name}”")
        self.assertNotIn(2, g.agents, "the model seat has not been asked anything yet")
        self.assertEqual(g.leases[2].kind, "game")

    def test_a_make_way_pause_drops_the_game_and_the_resume_brings_it_back(self):
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}])
        label = f"game “{g.name}”"
        g.suspend({"kind": "queue", "message": "making way"})
        self.assertEqual(pool_seats.occupied("sv_gpu"), [], "a game that made way does not hold its machine")
        g.resume()
        self.assertEqual(pool_seats.occupied("sv_gpu"), [label])
        # every other pause keeps the machine: the game resumes where it left off
        for reason in (None, {"kind": "restricted", "message": "quiet"}, {"kind": "disconnect", "message": "gone"}):
            g.suspend(reason)
            self.assertEqual(pool_seats.occupied("sv_gpu"), [label], reason)
            g.resume()
        # made way, then resumed by a person from the game screen (the control route holds the game's lock): the
        # driver makes the lease again, off the lock
        g.suspend({"kind": "queue", "message": "making way"})
        with g.lock:
            g.set_paused(False)
            self.assertEqual(pool_seats.occupied("sv_gpu"), [], "nothing is read from the database under the lock")
        self.assertTrue(until(lambda: pool_seats.occupied("sv_gpu") == [label]), pool_seats.occupied("sv_gpu"))

    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_an_eliminated_seat_lets_its_lease_go_and_a_game_over_every_lease_for_good(self):
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}, {"type": "llm", "llm": {**GPU,
                                                                                               "server_id": "sv_dryrun"}}],
                      map_size="small")
        self.assertEqual((len(labels("sv_gpu")), len(labels("sv_dryrun"))), (1, 1))
        with g.lock:
            g.game.test_ops([{"op": "eliminate", "player": 1}])
        self.assertEqual(g.game.phase, "playing")
        self.assertEqual(pool_seats.occupied("sv_gpu"), [], "an eliminated seat's machine is free")
        self.assertIsNone(g.lease_for(1), "and no lease is made for it again")
        self.assertEqual(len(labels("sv_dryrun")), 1, "the other model seat keeps its lease")
        g.open_leases()
        self.assertEqual(labels("sv_gpu"), [])
        with g.lock:
            g.game.test_ops([{"op": "end_game"}])
        self.assertNotEqual(g.game.phase, "playing")
        self.assertEqual(labels("sv_dryrun"), [], "a game that is over holds nothing")
        self.assertIsNone(g.lease_for(2))
        self.assertIsNone(g.get_agent(2), "and plays no model again")

    def test_update_seat_to_another_machine_moves_the_lease(self):
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}])
        label = f"game “{g.name}”"
        g.update_seat(1, llm={**GPU, "server_id": "sv_dryrun"})
        self.assertEqual((pool_seats.occupied("sv_gpu"), pool_seats.occupied("sv_dryrun")), ([], [label]),
                         "the lease moved with the seat at once")
        g.update_seat(1, type="bot")
        self.assertEqual(pool_seats.occupied("sv_dryrun"), [], "a seat made a bot holds nothing")
        g.update_seat(1, type="llm", llm=GPU)
        self.assertEqual(pool_seats.occupied("sv_gpu"), [label], "a seat made a model seat again holds its machine")
        with g.lock:                        # under the game's lock (as a test or a later caller might): the driver
            g.update_seat(1, llm={**GPU, "server_id": "sv_dryrun"})     # makes the new lease, off the lock
            self.assertEqual(pool_seats.occupied("sv_gpu"), [])
        self.assertTrue(until(lambda: pool_seats.occupied("sv_dryrun") == [label]))

    def test_a_lease_made_before_its_game_had_an_owner_follows_the_owner_given_after(self):
        """The load route gives a save whose game had no owner to the member loading it after the load registered
        the game (and made its leases): the seat's lease then carries her as its consumer, capped by her grant."""
        from citar.auth import accounts
        from citar.db.models import AccessGrant, Server, ServerGroup
        with db.session() as s:
            owner = accounts.create_user(s, handle="lender-l", email="ll@x.cc", password="a long enough phrase",
                                         status="active", email_verified=True)
            member = accounts.create_user(s, handle="loader-l", email="lo@x.cc", password="a long enough phrase",
                                          status="active", email_verified=True)
            group = ServerGroup(owner_id=owner.id, name="lent for loads", visibility="server")
            s.add(group)
            s.flush()
            row = Server(owner_id=owner.id, name="lent box", kind="test", provider="dryrun", reach="direct",
                         group_id=group.id, max_concurrent=2,
                         config={"connection": {"provider": "dryrun"}, "models": [{"key": "dry-run"}]})
            s.add(row)
            s.add(AccessGrant(group_id=group.id, subject_type="everyone", concurrency=1))
            s.flush()
            sid, member_id = row.id, member.id
        g = self.game([{"type": "human"}, {"type": "llm", "llm": {"server_id": sid, "model": "dry-run"}}])
        lease = g.leases[1]
        self.assertEqual((lease.consumer, lease.cap), (None, 0), "the server's own game: no consumer, no cap")
        g.owner_id = member_id                      # what the load route does for a game with no owner
        self.assertIs(g.lease_for(1), lease, "the same lease")
        self.assertEqual((lease.consumer, lease.cap), (member_id, 1), "hers now, capped by her grant")

    def test_a_scenario_launch_and_a_benchmark_reload_make_their_leases_when_registered(self):
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU}], start=False)
        self.assertEqual(labels("sv_gpu"), [], "not registered yet: no lease")
        self.manager.register_new(g)                   # the scenario launch's path: registered, then started
        g.start()
        self.assertEqual(labels("sv_gpu"), [f"game “{g.name}”"])
        self.manager.delete(g.id)
        # a benchmark job's game, reloaded after a restart: its lease is a benchmark's, made when it is loaded
        sch, run, job = self.bench(turn_limit=30)
        self.assertTrue(until(lambda: (sch.tick() or True) and job["status"] == "running"
                              and self.manager.get(job["game_id"]) is not None, 30), job)
        gid = job["game_id"]
        game = self.manager.get(gid)
        self.assertEqual([lease.kind for lease in S.get().leases("sv_gpu")], ["benchmark"])
        game.autosave(force=True)
        self.assertTrue(game.flush_saves(10))
        game.stop()
        self.manager.sessions.pop(gid)
        self.assertEqual(labels("sv_gpu"), [])
        sch._recover()
        reloaded = self.manager.get(gid)
        self.assertIsNotNone(reloaded)
        self.assertTrue(reloaded.paused)
        self.assertEqual([(lease.kind, lease.ref) for lease in S.get().leases("sv_gpu")],
                         [("benchmark", ("benchmark", run["id"]))], "reloaded paused, already holding its machine")
        sch.control_run(run["id"], "cancel")
        self.assertEqual(labels("sv_gpu"), [], "a cancelled job's game holds nothing")
        # a cancelled job's game opened to be watched is only looked at: it holds no machine
        watched = sch.open_job_game(run["id"], job["id"])
        self.assertFalse(watched.holds_machines)
        self.assertEqual(labels("sv_gpu"), [])


# ---------------------------------------------------------------------------- gate 2

class OccupiedIsTheLeases(LeaseCase):
    """Gate 2: occupied() is the scheduler's live leases; benchmark jobs wait for lease slots; probe runs and reports
    hold theirs for their runs; the usage ledger counts only leased machines."""

    def test_occupied_lists_exactly_the_live_leases(self):
        sched = S.get()

        def same():
            self.assertEqual(pool_seats.occupied("sv_dryrun"), labels("sv_dryrun"))
            return labels("sv_dryrun")
        self.assertEqual(same(), [])
        two = self.game([{"type": "human"}, {"type": "llm", "llm": {**GPU, "server_id": "sv_dryrun"}},
                         {"type": "llm", "llm": {**GPU, "server_id": "sv_dryrun"}}], map_size="small")
        label = f"game “{two.name}”"
        self.assertEqual(same(), [label, label], "one per lease: a game with two model seats there holds two")
        self.assertEqual(pool_seats.held_text(pool_seats.occupied("sv_dryrun")), label, "named once in a sentence")
        report = sched.lease("sv_dryrun", "dry-run", kind="report", label="report “r”", ref=("report", "r1"))
        probe = sched.lease("sv_dryrun", "dry-run", kind="probe_case", label="probe run “p”", ref=("probe", "p1"))
        self.assertEqual(same(), [label, label, "report “r”", "probe run “p”"])
        self.assertEqual([(o["kind"], o["id"]) for o in pool_seats.occupants("sv_dryrun")],
                         [("game", two.id), ("game", two.id), ("report", "r1"), ("probe", "p1")])
        self.assertEqual(pool_seats.occupied("sv_dryrun", exclude=frozenset({two.id})), ["report “r”", "probe run “p”"])
        report.release()
        two.update_seat(2, type="bot")
        self.assertEqual(same(), [label, "probe run “p”"])
        probe.release()
        self.manager.delete(two.id)
        self.assertEqual(same(), [])

    def test_a_benchmark_job_waits_for_a_lease_slot(self):
        """On a machine whose games may run two at once, a job starts beside one lease and waits behind two."""
        servers.upsert({"id": "sv_two", "name": "Two slots", "kind": "test",
                        "connection": {"provider": "dryrun", "max_parallel": 2},
                        "models": [{"id": "m_two", "key": "dry-two"}]})
        servers.invalidate()
        seat = {"type": "llm", "llm": {"server_id": "sv_two", "model": "dry-two"}}
        a = self.game([{"type": "human"}, seat, seat], map_size="small")
        self.assertEqual(len(labels("sv_two")), 2)
        sch, run, job = self.bench("sv_two", "m_two")
        sch.tick()
        self.assertEqual(job["status"], "queued", "two leases fill a machine that takes two games")
        self.assertIn(f"in use by game “{a.name}”", job["waiting"])
        a.update_seat(2, type="bot")                         # one lease left: a slot for the job
        self.assertTrue(until(lambda: (sch.tick() or True) and job["status"] in ("loading", "running"), 15), job)
        sch.control_run(run["id"], "cancel")

    def test_a_probe_run_holds_its_lease_for_the_run_and_its_cases_play_under_it(self):
        from citar import engine_api as E, probes as P
        from citar.engine_api import EngineGame
        E.save_map(dict(E.generate_map(40, 26, "pangaea", 2, 2, seed=11, name="lease-map"), id="lease-map"))
        g = EngineGame.new({"map": "lease-map", "players": [{"controller": "human"}, {"controller": "llm"}],
                            "seed": 5, "speed": "Quick"})
        g.apply_ops([{"op": "set_player", "player": "all", "gold": 500}, {"op": "meet", "a": 0, "b": 1}])
        g.save_scenario("lease-scn", "Lease scenario", "", [{"type": "script"}, {"type": "llm"}])
        self.addCleanup(E.delete_scenario, "lease-scn")
        probe = P.validate({"id": "lease-probe", "scenario": "lease-scn", "subject": 1, "counterparty": 0, "cases": [
            {"id": "gift", "kind": "offer", "give": [{"type": "gold", "amount": 100}], "receive": []},
            {"id": "turn", "kind": "turn"}]})
        runner = P.ProbeRunner.__new__(P.ProbeRunner)
        runner.manager, runner.lock, runner._stop_run, runner._thread = self.manager, threading.Lock(), set(), None
        runner.lives, runner._workers, runner._loaded = {}, {}, {}
        rid = f"lease-run-{int(time.time() * 1000) % 100000}"
        runner._write({"id": rid, "name": "lease run", "probe": probe, "llm": dict(GPU), "repeats": 1,
                       "jobs": [{"case": "gift", "rep": 0}, {"case": "turn", "rep": 0}], "status": "queued", "done": 0,
                       "created": time.time(), "started": None, "finished": None, "summary": {}, "owner_id": None})
        events, seen = [], []
        S.get().add_listener(events.append)
        worker = threading.Thread(target=runner._run, args=(rid,), daemon=True)
        worker.start()
        while worker.is_alive():
            seen.append(tuple(labels("sv_gpu")))
            time.sleep(0.02)
        worker.join()
        held = [x for x in seen if x]
        self.assertTrue(held, "the run held its machine while it ran")
        self.assertEqual(set(held), {("probe run “lease run”",)},
                         "one lease for the whole run: no case made one of its own")
        self.assertEqual(labels("sv_gpu"), [], "released when the run is over")
        self.assertEqual(runner.get_run(rid)["status"], "finished", runner.get_run(rid))
        self.assertTrue(events, "the cases asked the model")
        self.assertEqual({(e.machine, e.cls, e.activity, e.task_class) for e in events},
                         {("sv_gpu", "probe_case", f"probe:{rid}", "full")})

    def test_a_report_holds_its_lease_while_its_narrative_is_written(self):
        from citar import reports
        runner = reports.ReportRunner(self.dir / "reports")
        runner.POLL_SECONDS = 0.05
        events, seen = [], []
        S.get().add_listener(events.append)
        m = runner.start({"title": "Lease report", "sections": ["summary"],
                          "narrative": {"enabled": True, "server_id": "sv_gpu", "model_id": "m_gpu"}})
        deadline = time.monotonic() + 60
        while runner.meta(m["id"])["status"] in ("queued", "running") and time.monotonic() < deadline:
            seen.append(tuple(labels("sv_gpu")))
            time.sleep(0.02)
        meta = runner.meta(m["id"])
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertIn("Dry run narrative", runner.html_path(m["id"]).read_text(encoding="utf-8"))
        self.assertIn(("report “Lease report”",), seen, "the report held its machine while it was built")
        self.assertEqual({x for x in seen if x}, {("report “Lease report”",)})
        self.assertTrue(until(lambda: labels("sv_gpu") == [], 5), "released when it is built")
        self.assertEqual([(e.machine, e.cls, e.activity) for e in events],
                         [("sv_gpu", "report", f"report:{m['id']}")], "one scheduled request, metered to the report")
        shutil.rmtree(runner.dir, ignore_errors=True)

    def test_the_usage_ledger_counts_only_leased_machines(self):
        from citar import usage
        g = self.game([{"type": "human"}, {"type": "llm", "llm": GPU},
                       {"type": "llm", "llm": {**GPU, "server_id": "sv_dryrun"}}], map_size="small")
        host = servers.load().get("host_server_id")
        self.assertEqual(set(usage.session_servers(g)), {host, "sv_gpu", "sv_dryrun"})
        g.update_seat(2, type="bot")
        self.assertEqual(set(usage.session_servers(g)), {host, "sv_gpu"}, "a seat changed away holds nothing")
        g.suspend({"kind": "queue", "message": "making way"})
        self.assertEqual(usage.session_servers(g), {}, "a paused game holds nothing")
        g.resume()
        self.assertEqual(set(usage.session_servers(g)), {host, "sv_gpu"})
        g.update_seat(1, llm={**GPU, "server_id": "sv_dryrun"})
        self.assertEqual(set(usage.session_servers(g)), {host, "sv_dryrun"}, "the seat's new machine only")


# ---------------------------------------------------------------------------- gate 3

class HeldStep(Conversation):
    """A provider's conversation whose one step stays in flight until it is closed, and is then abandoned with its
    helper still generating it (``unfinished``), as a v1 helper's cancelled request (P3.3.3)."""

    def __init__(self):
        self.usage = {"input_tokens": 0, "output_tokens": 0}
        self.next_request_id = None
        self.unfinished = None
        self.rid = None
        self.started = threading.Event()
        self.closed = threading.Event()

    def add_user_text(self, text):
        pass

    def step(self) -> StepResult:
        self.rid = self.next_request_id
        self.started.set()
        self.closed.wait(60)
        self.unfinished = (self.rid, 600.0)
        raise RuntimeError("the request was cancelled")

    def close(self):
        self.closed.set()


class BenchmarkDrain(LeaseCase):
    """Gate 3: a benchmark job that starts while a pooled request runs on its machine waits for it to drain and records
    ``drain_s``; one whose drain is cut short by its bound is ``contaminated``, its timing left out of the speed score."""

    def setUp(self):
        super().setUp()
        # a machine of its own, whose model no other test's saved game plays: the speed score reads only these jobs
        servers.upsert({"id": "sv_drain", "name": "Drain box", "kind": "test",
                        "connection": {"provider": "dryrun", "max_parallel": 4, "dry_run_delay": 0.05},
                        "models": [{"id": "m_drain", "key": "drain-model"}]})
        servers.invalidate()

    def held_ticket(self, sid: str = "sv_drain"):
        """A pooled prompt running on ``sid``: its request in flight until the benchmark cancels it."""
        ticket = S.get().ticket(sid, "drain-model", cls="test", consumer="someone-else")
        inner = HeldStep()
        conv = S.ScheduledConversation(inner, ticket)
        failed: dict = {}

        def run():
            try:
                conv.step()
            except Exception as e:
                failed["e"] = e
        worker = threading.Thread(target=run, daemon=True)
        worker.start()
        self.assertTrue(inner.started.wait(5), "the pooled request is in flight")
        self.addCleanup(ticket.close)
        return ticket, inner, worker, failed

    def run_job(self, *, release_after: float = None):
        """A one-job benchmark run on ``sv_drain`` (four slots) started while a test ticket's request is in flight
        there; with ``release_after``, the helper's late answer for it comes that long after the job's first request
        began to wait. Returns the job once done, its benchmark scheduler and the held request."""
        sched = S.get()
        ticket, inner, worker, failed = self.held_ticket()
        sch, run, job = self.bench("sv_drain", "m_drain", turn_limit=3)
        m = sched.machine("sv_drain")
        self.assertTrue(until(lambda: (sch.tick() or True) and job["status"] in ("running", "failed"), 30), job)
        self.assertEqual(ticket.state, "cancelled", "the benchmark's lease cancels the pooled request running there")
        self.assertEqual(ticket.error.code, "benchmark")
        worker.join(5)
        self.assertIsInstance(failed.get("e"), RuntimeError)
        self.assertEqual(m.status()["draining"], 1, "its request drains: the helper is still generating it")
        if release_after is not None:
            self.assertTrue(until(lambda: any(r.binding.is_lease for r in list(m.waiting)), 30),
                            "the job's first request waits for the drain")
            time.sleep(release_after)
            self.assertTrue(sched.late_frame("sv_drain", inner.rid), "the helper's late answer frees the slot")
        self.assertTrue(until(lambda: (sch.tick() or True) and job["status"] in ("done", "failed"), 60), job)
        self.assertEqual(job["status"], "done", job)
        return job, sch, inner

    def test_a_job_started_beside_a_running_ticket_records_its_drain(self):
        S.get().drain_seconds = 30.0
        job, sch, _inner = self.run_job(release_after=0.6)
        result = job["result"]
        self.assertGreaterEqual(result["drain_s"], 0.5, result)
        self.assertLess(result["drain_s"], 10, result)
        self.assertIs(result["contaminated"], False)

    def test_a_drain_cut_short_by_its_bound_marks_the_job_contaminated_and_out_of_the_speed_score(self):
        from citar.server import scoring
        empty = Path(tempfile.mkdtemp(prefix="citar-no-saves-"))
        self.addCleanup(shutil.rmtree, empty, ignore_errors=True)
        patch = mock.patch.object(scoring, "SAVE_DIR", empty)       # the live jobs' games only
        patch.start()
        self.addCleanup(patch.stop)
        S.get().drain_seconds = 0.8
        with self.assertLogs("citar.scheduler", level="WARNING") as logged:
            job, sch, inner = self.run_job()
        self.assertTrue(any("contaminated" in line for line in logged.output), logged.output)
        result = job["result"]
        self.assertIs(result["contaminated"], True, result)
        self.assertGreaterEqual(result["drain_s"], 0.7, result)
        game = self.manager.get(job["game_id"])
        self.assertTrue(game.benchmark["contaminated"], "kept with the game, so its saves carry it")
        weights = sch.get_settings()["score_weights"]
        game.seats[0].llm["provider"] = "openai_compatible"     # a dry run never scores: as a real model's game
        row = {r["model"]: r for r in scoring.model_scores(self.manager, weights)["models"]}[job["model"]]
        self.assertIsNone(row["speed"], "its only game's timing is contaminated: no speed score")
        self.assertEqual((row["metrics"]["timed_turns"], row["metrics"]["contaminated_games"]), (0, 1))
        self.assertIsNotNone(row["overall"], "the overall score is made of performance and reliability")
        self.assertEqual(row["overall"], round((weights["benchmark"] * row["benchmark"]
                                                + weights["reliability"] * row["reliability"])
                                               / (weights["benchmark"] + weights["reliability"]), 1))
        # a second, clean job of the same model gives it a speed score from that job's timing alone (the first one's
        # cancelled request answered at last, so nothing is left to drain)
        self.assertTrue(S.get().late_frame("sv_drain", inner.rid))
        S.get().drain_seconds = 120.0
        sch2, run2, job2 = self.bench("sv_drain", "m_drain", turn_limit=3)
        self.assertTrue(until(lambda: (sch2.tick() or True) and job2["status"] in ("done", "failed"), 60), job2)
        self.assertIs(job2["result"]["contaminated"], False)
        self.manager.get(job2["game_id"]).seats[0].llm["provider"] = "openai_compatible"
        row = {r["model"]: r for r in scoring.model_scores(self.manager, weights)["models"]}[job["model"]]
        clean = job2["result"]["turns_played"]
        self.assertEqual((row["metrics"]["timed_turns"], row["metrics"]["contaminated_games"]), (clean, 1))
        self.assertIsNotNone(row["speed"])
        self.assertEqual(row["benchmark_games"], 2, "both games still count for performance")


class SpeedScoreLeavesContaminatedTimingsOut(unittest.TestCase):
    """compare_models on made-up games: a contaminated benchmark job's model seat counts for reliability, never for
    timing; every other seat of its game, and the model's other games, count as before."""

    @staticmethod
    def row(turns: int, turn_s: float, gen_s: float, **kw) -> dict:
        base = {"controller": "llm", "model": "m", "turns": turns, "avg_turn_s": turn_s, "max_turn_s": turn_s * 2,
                "avg_model_s": gen_s, "avg_queue_s": 1.0, "avg_model_steps": 3.0, "avg_tool_calls": 4.0,
                "avg_errors": 0.0, "avg_repeats": 0.0, "avg_output_tokens": 100, "output_tokens_per_s": 10.0,
                "malformed_calls": 0, "blocked_repeats": 0, "stall_nudges": 0, "end_reasons": {"end_turn": turns}}
        return {**base, **kw}

    def test_contaminated_timings_are_left_out(self):
        from citar.server.scoring import compare_models, model_speed
        reports = {
            "clean": {"name": "clean", "turn": 11, "benchmark": {"llm_player": 0, "contaminated": False},
                      "summary": {0: self.row(10, 30.0, 20.0)}},
            "dirty": {"name": "dirty", "turn": 21, "benchmark": {"llm_player": 0, "contaminated": True},
                      "summary": {0: self.row(20, 900.0, 600.0),
                                  1: self.row(5, 2.0, 1.0, model="other")}},
        }
        rows = {r["model"]: r for r in compare_models(None, reports)}
        m = rows["m"]
        self.assertEqual((m["games"], m["turns"], m["timed_turns"], m["contaminated_games"]), (2, 30, 10, 1))
        self.assertEqual((m["avg_turn_s"], m["avg_model_s"], m["max_turn_s"]), (30.0, 20.0, 60.0))
        self.assertEqual(m["clean_turn_end_rate"], 1.0, "reliability counts every turn")
        self.assertEqual(rows["other"]["avg_turn_s"], 2.0, "another seat of a contaminated game is timed as before")
        self.assertEqual(model_speed(m), model_speed({"avg_model_s": 20.0, "avg_turn_s": 30.0}))
        only = {"dirty": reports["dirty"]}
        m = {r["model"]: r for r in compare_models(None, only)}["m"]
        self.assertEqual((m["avg_turn_s"], m["avg_model_s"], m["timed_turns"]), (None, None, 0))
        self.assertIsNone(model_speed(m))


if __name__ == "__main__":
    unittest.main()
