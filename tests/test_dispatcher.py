"""The job dispatcher and the local executor (design/phase4-helper.md P4.5.5-P4.5.7; package 4-09's gates).

- **An experiment runs to its end on the local executor** (gate 1): six 30-round duels, played by this server's job
  host under the host machine's CPU settings; six result lines, each with ``executor.machine`` the host and
  ``verification: reference``; six ``cpu`` events used by the experiment's owner and given by nobody; the ratings
  include its games; the experiment moves to done/.
- **A ranking and a reference experiment of an administrator** (gate 2): the server's own collection (no consumer);
  the reference lines in ``saves/lab/reference/``, which ``scripts/refcheck/summarize.py`` reads as a baseline's.
- **Restart and the order of writing** (gate 3): a server killed with two runs in flight comes back and plays exactly
  those two again, with no duplicate line and no job host left; a crash between a result's commit and its line leaves
  exactly one line after the restart; a line whose job is not accepted (and a line cut short) is removed at the start.
- **Order** (gate 4): priority, then round-robin across owners; ``stop`` (the games playing finish, nothing new starts)
  and ``resume``; a benchmark lease on the host pauses its games, and its end resumes them.
- **Memory** (gate 5): a job whose ``mem_mb`` is above the executor's memory a run is not offered to it (it waits,
  saying why); a ``memory`` failure raises the job's ``mem_mb`` to the peak its run reached.
- **Deadlines** (gate 6): the deadline from the cost table and the executor's share (a table of cases), extended by
  paused time up to twice, and an expiry its pause caused counted neither as an attempt nor as an abandon.
- **The old runner is gone** (gate 7): its process handling nowhere in the tree; ``citar lab run`` says where the lab
  went and exits 2.
- **Routes and speed** (gate 8, P4.7.3, P4.7.9): the routes' access; a tick with 2,000 queued jobs and ten executors.

Every job host here holds at most two cores' worth and 400 MB (P4.7.1).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import contextlib
import io
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_dispatcher_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import select
from sqlalchemy.exc import OperationalError

from citar import db, engine_api, items, lab, ledger, servers, settings
from citar.auth import accounts, policy, sessions
from citar.bots import ratings
from citar.db.models import Base, ComputeEvent, CpuJob, CpuJobRun, Server, User
from citar.jobs import dispatcher as D, host as H, local as L, store
from citar.machine_settings import CPU_WORK
from citar.pool import scheduler as S
from tests import has_test_ops
from tests.helperfake import result_frame, test_key

settings.reset()
ROOT = Path(__file__).resolve().parent.parent
ON_CI = bool(os.environ.get("CI"))


def tearDownModule():
    ledger.reset()
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


def two_cores() -> int:
    """At most two cores' worth of this machine, in percent (P4.7.1)."""
    return max(1, min(100, 200 // H.machine_cpus()))


def duels(name: str, games: int, turns: int = 30, **over) -> dict:
    return {"name": name, "games": games, "seed": 9100, "size": "duel", "maps": ["pangaea"], "turns": turns,
            "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}], **over}


class Fake:
    """An executor in this process: it plays a job with ``engine_api.run_job`` when told (``play``), or at once
    (``auto``), and answers as a job host does. Its frames wait until the dispatcher's next look."""

    def __init__(self, machine: str = "sv_host", *, name: str = "fake", kind: str = "local", threads: int = 1,
                 per_run: int = 500, auto: bool = False, work=CPU_WORK):
        self.id, self.machine, self.name, self.kind = machine, machine, name, kind
        self.threads, self.memory_per_run_mb, self.auto = threads, per_run, auto
        self.work = tuple(work)
        self.state, self.reason, self.verifiable, self.online = "ready", "", True, True
        self.want = {"enabled": True}
        self.cpu_percent, self.cpus, self.speed = 100, threads, 1.0
        self.jobs_build = engine_api.jobs_build()
        self.in_flight: set = set()
        self.offers: list = []
        self.jobs: dict = {}
        self.frames: list = []
        self.paused = False
        self.aborted: list = []

    @property
    def free(self) -> int:
        return max(0, self.threads - len(self.in_flight)) if self.state == "ready" else 0

    def refuses(self, job):
        return L.LocalExecutor.refuses(self, job)

    def describe(self) -> dict:
        return {"machine": self.id, "name": self.name, "kind": self.kind, "on": True, "state": self.state,
                "threads": self.threads, "running": len(self.in_flight)}

    def refresh(self, wanted: bool) -> None:
        pass

    def offer(self, run, job, deadline_s) -> None:
        self.in_flight.add(run)
        self.offers.append((run, job, deadline_s))
        self.jobs[run] = job
        self.frames.append({"t": "job_accept", "run": run})
        if self.auto:
            self.play(run)

    def play(self, run: str, *, peak: float = 1.0, **figures) -> None:
        job = self.jobs[run]
        out = engine_api.run_job(job.kind, job.spec, spec_hash=job.spec_hash)
        frame = {"t": "job_result", "run": run, "outcome": {
            "result": out["result"], "result_hash": out["result_hash"], "chain": out["chain"],
            "cpu_s": figures.get("cpu_s", out["cpu_s"]), "wall_s": figures.get("wall_s", out["wall_s"]),
            "paused_s": figures.get("paused_s", 0.0), "peak_mb": peak}}
        if self.kind != "local":
            # a helper's result carries its claim, signed by the fake helpers' throwaway key (its machine's row
            # registers it)
            offer = {"job": job.id, "run": run, "kind": job.kind, "spec": job.spec, "spec_hash": job.spec_hash,
                     "jobs_build": job.jobs_build}
            signed = result_frame(offer, out, self.machine)
            frame["claim"], frame["signature"] = signed["claim"], signed["signature"]
        self.frames.append(frame)

    def fail(self, run: str, code: str, **extra) -> None:
        self.frames.append({"t": "job_failed", "run": run, "code": code, "message": f"{code} (a test)",
                            "retryable": True, **extra})

    def progress(self, run: str, **f) -> None:
        self.frames.append({"t": "job_progress", "run": run, **f})

    def abort(self, run: str, reason: str = "") -> None:
        self.aborted.append(run)
        if run in self.in_flight:
            self.frames.append({"t": "job_aborted", "run": run})

    def pause(self) -> None:
        self.paused = True

    def resume(self) -> None:
        self.paused = False

    def poll(self) -> list:
        out, self.frames = self.frames, []
        for f in out:
            if f["t"] in ("job_result", "job_failed", "job_aborted", "job_reject"):
                self.in_flight.discard(f["run"])
        return out

    def close(self) -> None:
        pass


class Clock:
    def __init__(self):
        self.t = 1000.0

    def __call__(self) -> float:
        return self.t


class Lab(unittest.TestCase):
    """A fresh database (the suite's machines), the lab's folders of this test, a ledger written on the calling
    thread, and: alice and bob, members; mona, a moderator; zara, an administrator. The host machine (``sv_host``)
    lends two cores' worth and 400 MB to two threads."""

    THREADS = 2

    def setUp(self):
        db.dispose()
        db.configure(os.environ["CITAR_DB_URL"])
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        servers.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.led = ledger.reset(background=False)
        self.addCleanup(lambda: ledger.reset(background=False))
        self.dir = Path(tempfile.mkdtemp(prefix="citar-dispatch-lab-", dir=_TMP))
        labdir = self.dir / "saves" / "lab"
        for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"),
                          ("REFERENCE", "reference")):
            patcher = mock.patch.object(lab, name, labdir / sub if sub else labdir)
            patcher.start()
            self.addCleanup(patcher.stop)
        lab._dirs()
        self.ids, self.headers = {}, {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("mona", "moderator"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
        self.host_cpu(threads=self.THREADS, memory_mb=400)

    def host_cpu(self, **cpu) -> None:
        with db.session() as s:
            row = s.get(Server, "sv_host")
            row.capabilities = {"cpu": {"enabled": True, "threads": 2, "cpu_percent": two_cores(), "memory_mb": 400,
                                        "work": list(CPU_WORK), **cpu}}
        servers.invalidate()

    def submit(self, who: str, spec: dict) -> dict:
        """Queue an experiment as ``who``'s, its item registered as the A/B route and ``citar lab submit`` do (a
        ranking or reference run the server's own collection)."""
        exp = lab.submit(spec)
        with db.session() as s:
            items.register(s, "lab_experiment", exp["name"], s.get(User, self.ids[who]), exp["name"],
                           path=lab.QUEUE / f"{exp['name']}.json",
                           collection=lab.kind_of(exp) in lab.COLLECTION_KINDS)
        return exp

    def local(self, env: dict | None = None) -> L.LocalExecutor:
        """The local executor, its host's children given ``env`` (the test jobs' switches)."""
        def factory(threads, cpu_percent, memory_mb):
            return H.JobHost(threads, cpu_percent, memory_mb, play_unverifiable=True, env=env)
        return L.LocalExecutor(host_factory=factory)

    def dispatcher(self, executors=None, **kw) -> D.Dispatcher:
        """A dispatcher this test ticks by hand, the process's (the Lab page's and the routes' view are its)."""
        d = D.Dispatcher(executors if executors is not None else [self.local()], **kw)
        self.addCleanup(d.stop)
        old = D.install(d)
        self.addCleanup(D.install, old)
        return d

    def until(self, d: D.Dispatcher, cond, timeout: float = 120.0, step: float = 0.05):
        """Tick ``d`` until ``cond()`` holds."""
        deadline = time.perf_counter() + timeout
        while True:
            d.tick()
            got = cond()
            if got:
                return got
            if time.perf_counter() > deadline:
                self.fail(f"not within {timeout} s; the lab log:\n{lab._tail(lab.LAB / 'lab.log', 3000)}")
            time.sleep(step)

    def lines(self, name: str, kind: str = "lab") -> list:
        return lab.load_results(name, kind)

    def events(self) -> list:
        self.led.drain()
        with db.session() as s:
            return [{"activity": e.activity, "consumer": e.consumer_id, "provider": e.provider_id,
                     "machine": e.machine_id, "cpu": e.cpu_core_s, "observed": e.observed_s,
                     "reported": e.reported_s, "flags": dict(e.flags or {}),
                     "verification": e.verification, "request": e.request_id, "kind": e.kind, "task": e.task_class}
                    for e in s.scalars(select(ComputeEvent).order_by(ComputeEvent.id))]

    def jobs(self) -> list:
        with db.session() as s:
            return [(j.ref, j.state, j.mem_mb, j.attempts, j.owner_id) for j in
                    s.scalars(select(CpuJob).order_by(CpuJob.created_at, CpuJob.ref))]


# ---------------------------------------------------------------------------------------------------------- gate 1, 2

class Experiments(Lab):
    def test_six_duels_run_to_their_end_on_the_local_executor(self):
        self.submit("alice", duels("six-duels", 6))
        d = self.dispatcher()
        self.until(d, lambda: (lab.DONE / "six-duels.json").exists())
        self.assertFalse((lab.QUEUE / "six-duels.json").exists())
        lines = self.lines("six-duels")
        self.assertEqual(sorted(r["i"] for r in lines), list(range(6)), "each game once")
        for r in lines:
            self.assertEqual(r["executor"]["machine"], "sv_host")
            self.assertEqual(r["executor"]["verification"], "reference")
            self.assertTrue(r["executor"]["run"].startswith("cr_"))
            self.assertEqual((r["exp"], r["turns"], len(r["players"])), ("six-duels", 30, 2))
            self.assertGreaterEqual(r["seconds"], 0)
            self.assertIn("finished", r)
        events = [e for e in self.events() if e["activity"].startswith("lab:six-duels:")]
        self.assertEqual(len(events), 6)
        for e in events:
            self.assertEqual((e["consumer"], e["provider"], e["machine"], e["kind"], e["task"], e["verification"]),
                             (self.ids["alice"], None, "sv_host", "cpu", "cpu", "verified"))
            self.assertLessEqual(e["cpu"], e["observed"] * D.OBSERVED_FACTOR + D.OBSERVED_SLACK)
        self.assertEqual({e["request"] for e in events}, {r["executor"]["run"] for r in lines}, "one event a run")
        games, entries = ratings.collect()
        rated = [e for e in entries.values() if "six-duels" in e["experiments"]]
        self.assertTrue(rated, "the ratings include its games")
        self.assertEqual(len(games), 6)
        with db.session() as s:
            runs = list(s.scalars(select(CpuJobRun)))
            self.assertEqual({(r.state, r.verdict, r.executor) for r in runs}, {("done", "reference", "local")})
            self.assertTrue(all(r.result is None for r in runs), "a written line's result is pruned")
            self.assertTrue(all(r.chain_head and r.rounds == 30 for r in runs))
        self.assertEqual({j[1] for j in self.jobs()}, {"accepted"})
        log = (lab.LAB / "lab.log").read_text(encoding="utf-8")
        self.assertIn("experiment six-duels complete", log)
        view = lab.overview()
        self.assertTrue(view["dispatcher"]["alive"])
        shown = {e["name"]: e for e in view["experiments"]}["six-duels"]
        self.assertEqual((shown["state"], shown["done"]), ("complete", 6))

    def test_a_ranking_and_a_reference_experiment_of_an_administrator(self):
        self.submit("zara", duels("ladder", 2, kind="ranking"))
        self.submit("zara", {"name": "baseline", "kind": "reference", "games": 2, "seed": 4400, "size": "duel",
                             "maps": ["pangaea"], "turns": 40})
        d = self.dispatcher()
        self.until(d, lambda: (lab.DONE / "ladder.json").exists() and (lab.DONE / "baseline.json").exists())
        ladder, ref = self.lines("ladder"), self.lines("baseline", "reference")
        self.assertEqual(sorted(r["i"] for r in ladder), [0, 1])
        self.assertFalse(lab.results_path("baseline").exists(), "a reference run's lines are not lab results")
        self.assertTrue((lab.REFERENCE / "baseline.jsonl").exists())
        self.assertEqual(sorted(r["i"] for r in ref), [0, 1])
        self.assertEqual({j[0].split(":")[1]: j[4] for j in self.jobs()}, {"ladder": None, "baseline": None},
                         "the server's own collection: no consumer")
        self.assertEqual({(e["consumer"], e["provider"]) for e in self.events()}, {(None, None)})
        with db.session() as s:
            kinds = {j.kind for j in s.scalars(select(CpuJob))}
        self.assertEqual(kinds, {"ranking_game", "reference_game"})
        # summarize.py reads the reference lines as it reads a baseline's
        sys.path.insert(0, str(ROOT / "scripts" / "refcheck"))
        self.addCleanup(sys.path.remove, str(ROOT / "scripts" / "refcheck"))
        import summarize
        games, crashes = summarize.load(lab.REFERENCE / "baseline.jsonl")
        self.assertEqual((len(games), crashes), (2, []))
        for g in games:
            self.assertEqual({k: g[k] for k in ("size", "speed", "turn_limit")},
                             {"size": "duel", "speed": "Quick", "turn_limit": 40})
            self.assertTrue(g["civs"] and g["engine"])
        self.assertTrue(summarize.per_game(games, "score", "end"))
        # the ranking games are rated like any lab game
        _games, entries = ratings.collect()
        self.assertTrue(any("ladder" in e["experiments"] for e in entries.values()))

    def test_a_ranking_or_reference_run_is_an_administrators(self):
        spec = self.dir / "ladder.json"
        spec.write_text(json.dumps(duels("ladder", 1, kind="ranking")), encoding="utf-8")
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as cm:
            lab.main(["submit", str(spec), "--owner", "alice"])
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("is an administrator's: alice is not an administrator", err.getvalue())
        self.assertFalse((lab.QUEUE / "ladder.json").exists())
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            lab.main(["submit", str(spec), "--owner", "zara"])
        self.assertIn("registered to zara as the server's own collection", out.getvalue())
        with db.session() as s:
            self.assertTrue(items.get(s, "lab_experiment", "ladder").collection)
        with self.assertRaisesRegex(ValueError, "no seats or factors"):
            lab.normalize({"name": "r", "kind": "reference", "seats": [{"bot": "basic"}]})
        with self.assertRaisesRegex(ValueError, "kind is one of"):
            lab.normalize({"name": "x", "kind": "tournament", "seats": [{"bot": "basic"}]})

    def test_an_experiment_registered_after_its_file_takes_its_owner(self):
        """``citar lab submit`` and the A/B route write the queue's file, then commit its item: an experiment read in
        between makes no job until its item is found (looked up every tick for ``OWNER_SECONDS``), so no game of it
        is ever charged to nobody."""
        lab.submit(duels("late", 3))
        clock = Clock()
        fake = Fake()
        d = self.dispatcher([fake], clock=clock)
        d.tick()
        self.assertEqual(self.jobs(), [], "no job while its owner may still be registering it")
        self.assertEqual(d.published["experiments"]["late"]["waiting"], "looking up its owner")
        with db.session() as s:
            items.register(s, "lab_experiment", "late", s.get(User, self.ids["alice"]), "late",
                           path=lab.QUEUE / "late.json")
        clock.t += 1.0
        d.tick()
        self.assertEqual({j[4] for j in self.jobs()}, {self.ids["alice"]}, "found within the grace: every job hers")
        self.assertEqual(len(fake.offers), 1)

    def test_an_experiment_no_item_registers_plays_after_the_grace_and_takes_an_owner_found_later(self):
        """Queued by hand (no item until the next start adopts it): its games start once ``OWNER_SECONDS`` have
        passed, as nobody's; an owner found later reaches its live jobs, the one playing too, whose event then names
        the consumer."""
        lab.submit(duels("by-hand", 2))
        clock = Clock()
        fake = Fake()
        d = self.dispatcher([fake], clock=clock)
        d.tick()
        self.assertEqual(self.jobs(), [])
        clock.t += D.OWNER_SECONDS
        self.until(d, lambda: fake.offers)
        self.assertEqual({j[4] for j in self.jobs()}, {None})
        with db.session() as s:
            items.register(s, "lab_experiment", "by-hand", s.get(User, self.ids["bob"]), "by-hand",
                           path=lab.QUEUE / "by-hand.json")
        d.tick()
        self.assertEqual({j[4] for j in self.jobs()}, {None}, "looked up again only every OWNER_SECONDS past the grace")
        clock.t += D.OWNER_SECONDS
        d.tick()
        self.assertEqual({j[4] for j in self.jobs()}, {self.ids["bob"]}, "the playing job and the waiting one")
        fake.auto = True
        fake.play(fake.offers[0][0])
        self.until(d, lambda: (lab.DONE / "by-hand.json").exists())
        events = [e for e in self.events() if e["activity"].startswith("lab:by-hand:")]
        self.assertEqual([e["consumer"] for e in events], [self.ids["bob"]] * 2)

    def test_a_queued_experiment_a_job_refuses_is_shown_so_and_never_started(self):
        with mock.patch.object(lab, "check_games"):
            self.submit("alice", duels("old-long", 2, turns=500))
        self.submit("alice", duels("good", 1))
        d = self.dispatcher([Fake(auto=True)])
        self.until(d, lambda: (lab.DONE / "good.json").exists())
        d.tick()
        self.assertTrue((lab.QUEUE / "old-long.json").exists())
        self.assertEqual([j[0] for j in self.jobs()], ["lab:good:0"], "no job of the refused experiment")
        shown = {e["name"]: e for e in lab.overview()["experiments"]}["old-long"]
        self.assertEqual(shown["state"], "refused")
        self.assertIn("past the speed's last turn", shown["refused"])
        log = (lab.LAB / "lab.log").read_text(encoding="utf-8")
        self.assertEqual(log.count("experiment old-long cannot be played"), 1, log)

    def test_a_crash_is_a_final_result_line(self):
        """A game that panics is a result (P4.4.2): its line says so, final; crashes.log has it; never played
        again; the experiment completes."""
        self.submit("alice", duels("crashing", 2))
        fake = Fake()
        d = self.dispatcher([fake])
        self.until(d, lambda: fake.offers)
        run, job, _ = fake.offers[0]
        out = engine_api.run_job(job.kind, job.spec, spec_hash=job.spec_hash, test_panic={"player": 1, "turn": 5}) \
            if has_test_ops() else None
        if out is None:
            self.skipTest("the extension has no test operations")
        fake.frames.append({"t": "job_result", "run": run, "outcome": {
            "result": out["result"], "result_hash": out["result_hash"], "chain": out["chain"], "cpu_s": out["cpu_s"],
            "wall_s": out["wall_s"], "paused_s": 0.0, "peak_mb": 1.0}})
        d.tick()
        fake.auto = True
        for r, _j, _dl in fake.offers[1:]:
            fake.play(r)
        self.until(d, lambda: (lab.DONE / "crashing.json").exists())
        lines = {r["i"]: r for r in self.lines("crashing")}
        self.assertEqual(sorted(lines), [0, 1])
        crashed = lines[job.i]
        self.assertTrue(crashed["final"])
        self.assertRegex(crashed["crash"], r"^T\d+ P1 .*: panic: ")
        self.assertIn(f"crashing #{job.i} final", (lab.LAB / "crashes.log").read_text(encoding="utf-8"))
        self.assertEqual({j[0]: j[1] for j in self.jobs()}[f"lab:crashing:{job.i}"], "crashed")
        self.assertEqual(len([o for o in fake.offers if o[1].i == job.i]), 1, "never played again")


# ---------------------------------------------------------------------------------------------------------- gate 3

_CHILD = textwrap.dedent("""
    import os, sys, time, json
    sys.path.insert(0, {root!r})
    import tests  # noqa: F401
    from citar import db, lab
    from citar.jobs import dispatcher as D, host as H, local as L
    db.configure(os.environ["CITAR_DB_URL"])
    def factory(threads, cpu_percent, memory_mb):
        return H.JobHost(threads, cpu_percent, memory_mb, play_unverifiable=True,
                         env={{"CITAR_JOBS_TEST_STEP_MS": "30"}})
    d = D.Dispatcher([L.LocalExecutor(host_factory=factory)])
    told = False
    while True:
        d.tick()
        playing = [r for r in d.runs.values() if r.state == "running" and (r.round or 0) >= 1]
        if len(playing) == 2 and not told:
            host = d.executors[0].host
            print("RUNNING", json.dumps({{"jobs": sorted(r.job for r in playing), "host": host.host_pid}}),
                  flush=True)
            told = True
        time.sleep(0.05)
""")


class Restart(Lab):
    def test_a_server_killed_with_two_runs_in_flight_plays_exactly_those_two_again(self):
        if not has_test_ops():
            self.skipTest("the extension has no test operations (a slowed game)")
        self.submit("alice", duels("restart", 4))
        env = {**os.environ, "CITAR_SAVE_DIR": str(self.dir / "saves"), "PYTHONUNBUFFERED": "1"}
        child = subprocess.Popen([sys.executable, "-c", _CHILD.format(root=str(ROOT))], stdout=subprocess.PIPE,
                                 stderr=subprocess.DEVNULL, env=env, cwd=str(ROOT), text=True)
        try:
            line = ""
            deadline = time.perf_counter() + 120
            while not line.startswith("RUNNING") and time.perf_counter() < deadline:
                line = child.stdout.readline()
                if not line and child.poll() is not None:
                    break
            self.assertTrue(line.startswith("RUNNING"), f"the child said {line!r}")
            told = json.loads(line.split(" ", 1)[1])
        finally:
            child.kill()
            child.wait(30)
            child.stdout.close()
        # no job host outlives the server: its input closed with the server, it exits within two seconds
        gone = time.perf_counter() + 10
        while _alive(told["host"]) and time.perf_counter() < gone:
            time.sleep(0.05)
        self.assertFalse(_alive(told["host"]), "the killed server's job host is gone")
        self.assertEqual(self.lines("restart"), [], "no game had finished")
        fake = Fake(auto=True)
        d = self.dispatcher([fake])
        d.tick()
        with db.session() as s:
            aborted = list(s.scalars(select(CpuJobRun).where(CpuJobRun.state == "aborted")))
        self.assertEqual(sorted(r.job_id for r in aborted), told["jobs"])
        self.assertEqual({r.error["code"] for r in aborted}, {"restart"})
        self.until(d, lambda: len(fake.offers) >= 2)
        self.assertEqual(sorted(j.id for _r, j, _dl in fake.offers[:2]), told["jobs"], "exactly those two first")
        self.until(d, lambda: (lab.DONE / "restart.json").exists())
        lines = self.lines("restart")
        self.assertEqual(sorted(r["i"] for r in lines), [0, 1, 2, 3], "each game once")
        self.assertEqual(len({r["executor"]["run"] for r in lines}), 4)

    def test_a_crash_between_the_commit_and_the_append_leaves_exactly_one_line(self):
        self.submit("alice", duels("crash-commit", 3))
        fake = Fake(auto=True)
        d = self.dispatcher([fake])

        def crash(job_id):
            raise SystemError("the server died here (a test)")
        d.hooks["after_commit"] = crash
        with self.assertLogs("citar.jobs.dispatcher", "ERROR"):
            self.until(d, lambda: any(j[1] == "accepted" for j in self.jobs()))
        d.stop()
        accepted = [j for j in self.jobs() if j[1] == "accepted"]
        self.assertTrue(accepted)
        self.assertEqual(self.lines("crash-commit"), [], "committed, not appended")
        with db.session() as s:
            self.assertTrue(all(r.result is not None for r in s.scalars(select(CpuJobRun))
                                .all() if r.state == "done"), "its result is kept until its line is written")
        d2 = self.dispatcher([Fake(auto=True)])
        d2.tick()
        lines = self.lines("crash-commit")
        self.assertEqual(sorted(r["i"] for r in lines),
                         sorted(int(j[0].rsplit(":", 1)[1]) for j in accepted), "written at the start, once")
        with db.session() as s:
            started = {r.id: D._utc_ts(r.started_at or r.offered_at) for r in s.scalars(select(CpuJobRun))
                       if r.state == "done"}
        for r in lines:
            # the database's times are UTC, naive as SQLite gives them: never read as local time
            self.assertAlmostEqual(r["started_ts"], started[r["executor"]["run"]], delta=1.0)
            self.assertLess(abs(r["started_ts"] - time.time()), 600)
        self.until(d2, lambda: (lab.DONE / "crash-commit.json").exists())
        lines = self.lines("crash-commit")
        self.assertEqual(sorted(r["i"] for r in lines), [0, 1, 2])
        events = [e for e in self.events() if e["activity"].startswith("lab:crash-commit:")]
        self.assertEqual(len(events), 3, "the lost events written at the start too, one a run")
        self.assertEqual(len({e["request"] for e in events}), 3)

    def test_an_experiment_deleted_while_the_server_was_down_has_its_games_cancelled(self):
        self.submit("alice", duels("gone", 3))
        fake = Fake()
        d = self.dispatcher([fake])
        self.until(d, lambda: fake.offers)
        d.stop()
        (lab.QUEUE / "gone.json").unlink()
        d2 = self.dispatcher([Fake(auto=True)])
        d2.tick()
        self.assertEqual({j[1] for j in self.jobs()}, {"cancelled"})
        self.assertEqual(d2.jobs, {})
        self.assertIn("3 waiting game(s) of experiments no longer queued were cancelled",
                      (lab.LAB / "lab.log").read_text(encoding="utf-8"))

    def test_a_line_whose_job_is_not_accepted_is_removed_at_the_start(self):
        self.submit("alice", duels("stale", 2))
        fake = Fake()
        d = self.dispatcher([fake])
        self.until(d, lambda: fake.offers)
        run, job, _ = fake.offers[0]
        fake.fail(run, "host")
        d.tick()
        d.stop()
        path = lab.results_path("stale")
        legacy = {"exp": "stale", "i": 1, "turns": 30, "players": {}, "seconds": 1.0}
        stale = {"exp": "stale", "i": job.i, "executor": {"machine": "sv_host", "run": run,
                                                          "verification": "reference"}}
        path.write_text(json.dumps(legacy) + "\n" + json.dumps(stale) + "\n" + '{"exp": "stale", "i": 9, "tu',
                        encoding="utf-8")
        fake2 = Fake(auto=True)
        d2 = self.dispatcher([fake2])
        d2.tick()
        kept = self.lines("stale")
        self.assertEqual(kept, [legacy], "the failed run's line and the cut line removed; a line from before jobs kept")
        self.assertIn("1 line(s) whose job was not accepted removed", (lab.LAB / "lab.log").read_text("utf-8"))
        self.until(d2, lambda: (lab.DONE / "stale.json").exists())
        self.assertEqual(sorted(r["i"] for r in self.lines("stale")), [0, 1])
        self.assertEqual([o[1].i for o in fake2.offers], [job.i], "the game with a line from before is not played")


def _alive(pid) -> bool:
    if not pid:
        return False
    if os.name == "nt":
        import ctypes
        handle = ctypes.windll.kernel32.OpenProcess(0x1000, False, int(pid))      # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        code = ctypes.c_ulong()
        ctypes.windll.kernel32.GetExitCodeProcess(handle, ctypes.byref(code))
        ctypes.windll.kernel32.CloseHandle(handle)
        return code.value == 259                                                  # STILL_ACTIVE
    try:
        os.kill(int(pid), 0)
    except OSError:
        return False
    try:
        with open(f"/proc/{int(pid)}/stat", encoding="ascii") as f:
            return f.read().split(") ", 1)[1][0] != "Z"
    except OSError:
        return True


# ---------------------------------------------------------------------------------------------------------- gate 4

class Order(Lab):
    def test_priority_first_then_round_robin_across_owners(self):
        self.submit("alice", duels("a-low", 3))
        self.submit("alice", duels("a-high", 2, priority=5))
        self.submit("alice", duels("a-more", 3))
        self.submit("bob", duels("b-low", 3))
        fake = Fake(auto=True)
        d = self.dispatcher([fake])
        self.until(d, lambda: not any(lab.QUEUE.glob("*.json")))
        order = [o[1].exp for o in fake.offers]
        self.assertEqual(order[:2], ["a-high", "a-high"], "the higher priority first")
        rest = order[2:]
        owners = ["alice" if e.startswith("a-") else "bob" for e in rest]
        # alice was served last (a-high): bob is next, then they take turns
        self.assertEqual(owners[:6], ["bob", "alice"] * 3, f"round-robin across owners: {rest}")
        self.assertEqual(rest.index("a-more") > rest.index("a-low"), True, "an owner's own by age")

    def test_stop_lets_the_games_playing_finish_and_starts_nothing_until_resume(self):
        self.submit("alice", duels("paused", 3))
        fake = Fake()
        d = self.dispatcher([fake])
        self.until(d, lambda: fake.offers)
        with contextlib.redirect_stdout(io.StringIO()):
            lab.main(["stop"])
        self.assertIsNotNone(lab.paused())
        fake.play(fake.offers[0][0])
        for _ in range(3):
            d.tick()
        self.assertEqual(len(fake.offers), 1, "nothing new starts while paused")
        self.assertEqual(len(self.lines("paused")), 1, "the game playing finished")
        self.assertEqual(d.waiting(next(iter(d.jobs.values()))), "the lab is paused")
        self.assertTrue(lab.overview()["dispatcher"]["paused"])
        with contextlib.redirect_stdout(io.StringIO()):
            lab.main(["resume"])
        fake.auto = True
        self.until(d, lambda: (lab.DONE / "paused.json").exists())
        self.assertEqual(len(fake.offers), 3)

    def test_a_benchmark_lease_pauses_the_local_hosts_games_and_its_end_resumes_them(self):
        if not has_test_ops():
            self.skipTest("the extension has no test operations (a slowed game)")
        self.host_cpu(threads=1)
        self.submit("alice", duels("bench", 1, turns=40))
        d = self.dispatcher([self.local({"CITAR_JOBS_TEST_STEP_MS": "40"})])
        d.start()
        sched = S.get()
        self.until(d, lambda: any((r.round or 0) >= 2 for r in d.runs.values()))
        lease = sched.lease("sv_host", "dry-run", kind="benchmark", label="benchmark run")
        run = self.until(d, lambda: next((r for r in d.runs.values() if r.paused), None), timeout=30)
        held = run.round
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < 1.5:
            d.tick()
            time.sleep(0.1)
        self.assertEqual(run.round, held, "the round does not move while the benchmark holds the machine")
        self.assertEqual(d._available(d.executors[0], d.clock()), "a benchmark holds its machine")
        lease.release()
        self.until(d, lambda: (lab.DONE / "bench.json").exists(), timeout=120)
        with db.session() as s:
            (r,) = s.scalars(select(CpuJobRun)).all()
        self.assertGreaterEqual(r.paused_s, 1.0, "its paused time is reported")
        log = (lab.LAB / "lab.log").read_text(encoding="utf-8")
        self.assertIn("a benchmark holds Test host: its games are paused", log)
        self.assertIn("the benchmark on Test host ended: its games go on", log)


# ---------------------------------------------------------------------------------------------------------- gate 5

class Memory(Lab):
    def test_a_job_above_the_executors_memory_a_run_waits_and_says_why(self):
        self.host_cpu(threads=1, memory_mb=200)
        self.submit("alice", duels("big", 2))
        local = self.local()
        d = self.dispatcher([local])
        d.tick()
        (first,) = [j for j in d.jobs.values() if j.i == 0]
        with db.session() as s:
            s.get(CpuJob, first.id).mem_mb = first.mem_mb = 900
        self.until(d, lambda: any(lab.QUEUE.glob("*.json")) and len(self.lines("big")) == 1)
        for _ in range(3):
            d.tick()
        self.assertEqual([r["i"] for r in self.lines("big")], [1], "the job that fits played; the other waits")
        self.assertIn(first.id, d.jobs)
        why = d.waiting(first)
        self.assertRegex(why, r"^this server: it needs 900 MB a run, and this server's job host leaves each run "
                              r"\d+ MB$")
        self.assertIn(why, json.dumps(d.queued_view()))
        exp = {e["name"]: e for e in lab.overview()["experiments"]}["big"]
        self.assertEqual(exp["waiting"], why, "the Lab page says why")

    def test_a_memory_death_raises_the_jobs_memory_to_the_peak_its_run_reached(self):
        if not has_test_ops():
            self.skipTest("the extension has no test operations (the alloc job)")
        self.host_cpu(threads=1, memory_mb=200, work=list(CPU_WORK))
        local = self.local()
        local_work = mock.patch.object(L.LocalExecutor, "work", property(lambda e: tuple(CPU_WORK) + ("alloc",)))
        local_work.start()
        self.addCleanup(local_work.stop)
        self.submit("alice", duels("alloc", 1))
        d = self.dispatcher([local])
        d.tick()
        # the experiment's game becomes a job that allocates past the host's limit (the test jobs' alloc)
        (job,) = d.jobs.values()
        from citar import _engine
        # held 1.5 s once allocated: macOS's limit is a watchdog that looks every 500 ms (limits::WATCHDOG_EVERY), and an
        # allocation done at once could finish between two looks and return a result for the server to refuse (CI,
        # test.yml run 38933789735, macos: two runs "internal" before the third died of memory). Windows and Linux end
        # it as it allocates.
        spec = {"mb": 400, "step_mb": 8, "hold_s": 1.5}
        checked = json.loads(_engine.jobs.test_job_spec("alloc", json.dumps(spec).encode()))
        with db.session() as s:
            row = s.get(CpuJob, job.id)
            row.kind, row.spec, row.spec_hash, row.mem_mb = "alloc", checked["spec"], checked["spec_hash"], 0
        job.kind, job.spec, job.spec_hash, job.mem_mb = "alloc", checked["spec"], checked["spec_hash"], 0
        self.until(d, lambda: job.mem_mb > 0, timeout=60)
        with db.session() as s:
            failed = [r for r in s.scalars(select(CpuJobRun)) if r.state == "failed"]
        self.assertEqual([r.error["code"] for r in failed], ["memory"])
        self.assertGreater(failed[0].peak_mb, 50, "the run reported its peak as it grew")
        per_run = local.memory_per_run_mb or failed[0].peak_mb
        self.assertGreaterEqual(job.mem_mb, int(failed[0].peak_mb), "raised to the peak reached")
        self.assertGreater(job.mem_mb, 0)
        self.assertEqual(job.attempts, 0, "a memory death is not an attempt: it changes where the job may run")
        # its host's only run: it alone needed more than the host had, so it is offered to this executor no more
        self.until(d, lambda: local.state == "ready", timeout=60)
        self.assertGreater(job.mem_mb, per_run)
        self.assertRegex(d.waiting(job), r"it needs \d+ MB a run")

    def test_the_rule_for_a_dead_hosts_runs(self):
        """Each run of a host that died of memory: its own peak; past the host's memory a run when it reported none
        or was the host's only run."""
        self.submit("alice", duels("rule", 3))
        fake = Fake(threads=3, per_run=100)
        d = self.dispatcher([fake])
        self.until(d, lambda: len(fake.offers) == 3)
        (a, ja, _), (b, jb, _), (c, jc, _) = fake.offers
        fake.fail(a, "memory", peak_mb=120.0, alone=False, per_run_mb=100)
        fake.fail(b, "memory", peak_mb=30.0, alone=False, per_run_mb=100)
        fake.fail(c, "memory", peak_mb=None, alone=False, per_run_mb=100)
        d.tick()
        got = {j[0]: j[2] for j in self.jobs()}
        self.assertEqual((got[ja.ref], got[jb.ref], got[jc.ref]), (120, 30, 101))
        self.assertEqual({j[3] for j in self.jobs()}, {0})
        run2 = next(o for o in fake.offers[3:] if o[1].id == jb.id)[0]
        fake.fail(run2, "memory", peak_mb=40.0, alone=True, per_run_mb=100)
        d.tick()
        self.assertEqual({j[0]: j[2] for j in self.jobs()}[jb.ref], 101, "alone: past the host's memory a run")


# ---------------------------------------------------------------------------------------------------------- gate 6

class Deadlines(Lab):
    def test_the_deadline_from_the_cost_and_the_executors_share(self):
        cases = [
            # cost s, cpu %, cpus, threads, speed: deadline s
            ((1.0, 100, 4, 4, 1.0), 300.0),                 # a short game: the 5-minute floor
            ((100.0, 100, 4, 4, 1.0), 1000.0),              # a core a game: 10 x the cost
            ((100.0, 50, 4, 4, 1.0), 2000.0),               # half a core a game: twice as long
            ((100.0, 10, 12, 1, 1.0), 1000.0),              # 1.2 cores for one game: a core at most
            ((100.0, 10, 12, 2, 1.0), 1666.7),              # 0.6 of a core each
            ((100.0, 50, 1, 1, 1.0), 2000.0),               # the VPS: one vCPU at half
            ((100.0, 100, 4, 4, 2.0), 500.0),               # a machine measured twice as fast
            ((100.0, 100, 4, 4, 0.5), 2000.0),              # half as fast
            ((5000.0, 100, 4, 4, 1.0), 6 * 3600.0),         # the 6-hour ceiling
            ((0.0, 0, 0, 0, 0.0), 300.0),                   # nothing known: the floor
        ]
        for args, want in cases:
            with self.subTest(args=args):
                self.assertAlmostEqual(D.deadline_s(*args), want, delta=0.1)

    def test_paused_time_extends_a_deadline_up_to_twice(self):
        self.assertEqual(D.expires_at(0.0, 300.0, 0.0), 300.0 + D.EXPIRY_GRACE_SECONDS)
        self.assertEqual(D.expires_at(0.0, 300.0, 100.0), 400.0 + D.EXPIRY_GRACE_SECONDS)
        self.assertEqual(D.expires_at(0.0, 300.0, 1000.0), 600.0 + D.EXPIRY_GRACE_SECONDS, "twice at most")
        self.assertTrue(D.pause_caused(0.0, 300.0, 500.0, 700.0))
        self.assertFalse(D.pause_caused(0.0, 300.0, 50.0, 700.0), "its active time is past the deadline")
        self.assertFalse(D.pause_caused(0.0, 300.0, 0.0, 400.0), "never paused")

    def test_an_expiry_its_pause_caused_is_neither_an_attempt_nor_an_abandon(self):
        with db.session() as s:
            s.add(Server(id="sv_box", owner_id=self.ids["bob"], name="Bob box", kind="owned", provider="dryrun",
                         reach="worker", max_concurrent=1, config={}, trust_tier="trusted",
                         public_key=test_key()))
        servers.invalidate()
        clock = Clock()
        self.submit("alice", duels("dl", 2))
        fake = Fake("sv_box", name="Bob box", kind="remote", threads=2)
        d = self.dispatcher([fake], clock=clock)
        self.until(d, lambda: len(fake.offers) == 2)
        d.tick()                                    # their accepts
        (a, ja, dl_a), (b, jb, _dl_b) = fake.offers
        self.assertEqual(dl_a, 300.0)
        fake.progress(a, round=3, of=30, elapsed_s=1.0, cpu_s=1.0, paused_s=250.0, peak_mb=1.0, paused=False)
        fake.progress(b, round=3, of=30, elapsed_s=1.0, cpu_s=1.0, paused_s=0.0, peak_mb=1.0, paused=False)
        d.tick()
        clock.t += 300.0 + D.EXPIRY_GRACE_SECONDS + 1           # b is past its deadline; a's pauses extend it
        fake.progress(a, round=4, of=30, elapsed_s=2.0, cpu_s=2.0, paused_s=250.0, peak_mb=1.0, paused=False)
        d.tick()
        self.assertIn(b, fake.aborted)
        self.assertNotIn(a, fake.aborted)
        clock.t += 300.0                                         # now past even a's extended deadline: its pauses
        fake.progress(a, round=5, of=30, elapsed_s=3.0, cpu_s=3.0, paused_s=500.0, peak_mb=1.0, paused=False)
        d.tick()
        self.assertIn(a, fake.aborted)
        attempts = {j[0]: j[3] for j in self.jobs()}
        self.assertEqual((attempts[ja.ref], attempts[jb.ref]), (0, 1), "the pause-caused expiry is no attempt")
        with db.session() as s:
            self.assertEqual(s.get(Server, "sv_box").trust.get("abandons"), 1, "nor an abandon")
            self.assertEqual({r.error["message"] for r in s.scalars(select(CpuJobRun))
                              if r.state == "expired"},
                             {"past its deadline", "past its deadline (its pauses)"})

    def test_three_expiries_in_an_hour_keep_a_machine_from_offers_for_the_hour(self):
        with db.session() as s:
            s.add(Server(id="sv_box", owner_id=self.ids["bob"], name="box", kind="owned", provider="dryrun",
                         reach="worker", max_concurrent=1, config={}, trust_tier="trusted",
                         public_key=test_key()))
        servers.invalidate()
        clock = Clock()
        self.submit("alice", duels("ab", 4))
        fake = Fake("sv_box", name="box", kind="remote", threads=1)
        d = self.dispatcher([fake], clock=clock)
        for n in range(3):
            self.until(d, lambda n=n: len(fake.offers) == n + 1)
            clock.t += D.OFFER_TTL_SECONDS + 1                   # an offer never answered
            fake.frames = [f for f in fake.frames if f["t"] != "job_accept"]
            d.tick()
        self.assertEqual(d._available(fake, clock.t), "3 of its runs expired within the hour")
        d.tick()
        self.assertEqual(len(fake.offers), 3)
        clock.t += D.ABANDON_WINDOW
        self.until(d, lambda: len(fake.offers) == 4)


# ---------------------------------------------------------------------------------------------------------- gate 7

class OldRunnerGone(unittest.TestCase):
    #: Where PowerShell's Get-CimInstance stays: the hardware scans (the computer's processor, memory and graphics),
    #: which have nothing to do with the lab.
    HARDWARE = {"citar/hwinfo.py", "citar/data/collectors/collect_hardware.ps1", "crates/citar-helper/src/scan/windows.rs"}

    def test_no_trace_of_the_old_runners_process_handling(self):
        files = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True).stdout.split()
        if not files:
            self.skipTest("not a git checkout")
        # the job files' folder (saves/lab/jobs), not the route /api/lab/jobs
        words = ("taskkill", "_Adopted", "saves/lab/jobs", "lab\\jobs", 'LAB / "jobs"', "_find_running_games",
                 "_collect_orphans", "citar.lab play", "cmd_play", "_runner_step")
        found = []
        for name in files:
            # the designs and the changelog are history
            if name.startswith("design/") or name.endswith(("DESIGN.md", "CHANGELOG.md")) \
                    or name == "tests/test_dispatcher.py":
                continue
            try:
                text = (ROOT / name).read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            found += [f"{name}: {w}" for w in words if w in text]
            if "Get-CimInstance" in text and name not in self.HARDWARE:
                found.append(f"{name}: Get-CimInstance")
        self.assertEqual(found, [])

    def test_citar_lab_run_says_where_the_lab_went(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as cm:
            lab.main(["run", "--workers", "3"])
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("the lab now runs inside the CITAR server (local or server mode)", err.getvalue())
        for gone in ("run", "_runner_step", "_Adopted", "_find_running_games", "_collect_orphans", "cmd_play",
                     "JOBS", "ATTEMPTS", "refusal"):
            self.assertFalse(hasattr(lab, gone), gone)


# ---------------------------------------------------------------------------------------------------------- gate 8

class Routes(Lab):
    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.submit("alice", duels("mine", 2))
        self.fake = Fake(auto=False)
        self.d = D.start(D.Dispatcher([self.fake]))
        self.addCleanup(D.stop)
        self.until(self.d, lambda: self.fake.offers)

    def call(self, who, method, path):
        return self.client.request(method, path, headers=self.headers.get(who, {"origin": "https://citar.test"}))

    def test_an_experiments_jobs_by_who_may_see_it(self):
        path = "/api/lab/jobs?exp=mine"
        # a private experiment is nobody else's to see: a visitor too gets "not found" (a link-shared or global one
        # serves visitors: tests/test_items_access.py's table holds the route to every item's ladder)
        self.assertEqual(self.call(None, "GET", path).status_code, 404)
        self.assertEqual(self.call("bob", "GET", path).status_code, 404, "a stranger: as if it were not there")
        self.assertEqual(self.call("mona", "GET", path).status_code, 404, "a moderator is a member here")
        self.assertEqual(self.call("alice", "GET", "/api/lab/jobs?exp=nobodys").status_code, 404)
        r = self.call("alice", "GET", path)
        self.assertEqual(r.status_code, 200, r.text)
        jobs = r.json()["jobs"]
        self.assertEqual([j["i"] for j in jobs], [0, 1])
        self.assertEqual(jobs[0]["state"], "running")
        self.assertNotIn("runs", jobs[0], "the runs are administrators'")
        self.fake.play(self.fake.offers[0][0])
        self.d.tick()
        done = self.call("alice", "GET", path).json()["jobs"][0]
        self.assertEqual((done["state"], done["verification"], done["executor"]), ("done", "reference",
                         {"name": "this server", "kind": "local"}))
        admin = self.call("zara", "GET", path).json()["jobs"][0]
        self.assertEqual(admin["state"], "accepted")
        self.assertEqual(admin["executor"]["name"], "this server (Test host)")
        self.assertEqual([r["role"] for r in admin["runs"]], ["primary"])
        # shared with bob, he sees it
        with db.session() as s:
            from citar.auth import access
            row = items.get(s, "lab_experiment", "mine")
            alice = s.get(User, self.ids["alice"])
            access.set_visibility(s, row, access.ALLOWLIST, actor=alice)
            access.grant(s, row, s.get(User, self.ids["bob"]), access.VIEW, actor=alice)
        self.assertEqual(self.call("bob", "GET", path).status_code, 200)

    def test_the_dispatcher_is_administrators(self):
        for path in ("/api/admin/jobs",):
            self.assertEqual(self.call(None, "GET", path).status_code, 401)
            for who in ("alice", "mona"):
                self.assertEqual(self.call(who, "GET", path).status_code, 404, who)
        r = self.call("zara", "GET", "/api/admin/jobs")
        self.assertEqual(r.status_code, 200, r.text)
        body = r.json()
        self.assertTrue(body["dispatcher"]["alive"])
        self.assertIn("sv_host", body["running"])
        self.assertEqual(list(body["queued"]), ["mine"])
        self.assertTrue(body["costs"])
        for path in ("/api/admin/jobs/pause", "/api/admin/jobs/resume"):
            self.assertEqual(self.call(None, "POST", path).status_code, 401)
            for who in ("alice", "mona"):
                self.assertEqual(self.call(who, "POST", path).status_code, 404, who)
        no_csrf = {k: v for k, v in self.headers["zara"].items() if k != sessions.CSRF_HEADER}
        self.assertEqual(self.client.post("/api/admin/jobs/pause", headers=no_csrf).status_code, 403)
        r = self.call("zara", "POST", "/api/admin/jobs/pause")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(lab.paused()["by"], "zara")
        self.assertEqual(self.call("zara", "GET", "/api/admin/jobs").json()["paused"]["by"], "zara")
        self.assertEqual(self.call("zara", "POST", "/api/admin/jobs/resume").json()["was_paused"], True)
        self.assertIsNone(lab.paused())

    def test_the_retry_of_failed_games_is_administrators(self):
        from citar.db.models import AuditLog
        path = "/api/admin/jobs/retry?exp=mine"
        self.assertEqual(self.call(None, "POST", path).status_code, 401)
        for who in ("alice", "bob", "mona"):
            self.assertEqual(self.call(who, "POST", path).status_code, 404, f"{who}: even the experiment's owner")
        no_csrf = {k: v for k, v in self.headers["zara"].items() if k != sessions.CSRF_HEADER}
        self.assertEqual(self.client.post(path, headers=no_csrf).status_code, 403)
        self.assertEqual(self.call("zara", "POST", "/api/admin/jobs/retry?exp=nobodys").status_code, 404)
        r = self.call("zara", "POST", path)
        self.assertEqual((r.status_code, r.json()), (200, {"requested": True, "exp": "mine"}), r.text)
        self.assertEqual(self.call("zara", "POST", "/api/admin/jobs/retry").json()["exp"], None, "every experiment")
        with db.session() as s:
            rows = [(a.action, (a.detail or {}).get("exp")) for a in s.scalars(select(AuditLog))
                    if a.action == "lab.retried"]
        self.assertEqual((len(rows), set(rows)), (2, {("lab.retried", "mine"), ("lab.retried", None)}))

    def test_the_lab_and_queue_pages_name_only_what_the_caller_may_see(self):
        lab_view = self.call("bob", "GET", "/api/lab").json()
        self.assertEqual(lab_view["experiments"], [])
        self.assertEqual(lab_view["running"], [])
        self.assertNotIn("notes", lab_view["dispatcher"]["executors"][0])
        mine = self.call("alice", "GET", "/api/lab").json()
        self.assertEqual([r["exp"] for r in mine["running"]], ["mine"])
        self.assertEqual([r["executor"] for r in mine["running"]], ["this server"])
        self.assertNotIn("machine", mine["running"][0])
        admin = self.call("zara", "GET", "/api/lab").json()
        self.assertEqual([r["executor"] for r in admin["running"]], ["this server (Test host)"])
        q = self.call("bob", "GET", "/api/queue").json()["cpu"]
        (host,) = q["executors"]
        self.assertEqual(host["name"], "this server")
        self.assertEqual([g["label"] for g in host["games"]], ["someone's lab game"])
        q = self.call("alice", "GET", "/api/queue").json()["cpu"]
        self.assertEqual([g["label"] for g in q["executors"][0]["games"]], ["mine #0"])


class Speed(Lab):
    def test_a_tick_with_2000_queued_jobs_and_ten_executors(self):
        """P4.7.9: under 50 ms, the median of 100 ticks (three times that on CI), each tick ending ten runs and
        offering ten more."""
        self.submit("alice", duels("big", 2000))

        class Aborting(Fake):
            def poll(self):
                self.frames = [{"t": "job_aborted", "run": r} for r in sorted(self.in_flight)]
                return super().poll()
        fakes = [Aborting(f"sv_f{k}", name=f"f{k}") for k in range(10)]
        d = self.dispatcher(fakes)
        with mock.patch.object(D, "WINDOW", 1000):
            d.tick()
        self.assertGreaterEqual(sum(1 for j in d.jobs.values() if j.state == "queued"), 1990)
        times = []
        for _ in range(100):
            t = time.perf_counter()
            d.tick()
            times.append(time.perf_counter() - t)
        self.assertEqual(sum(1 for j in d.jobs.values() if j.state in ("queued", "running")), 2000)
        median = statistics.median(times)
        print(f"\ndispatcher tick: median {median * 1000:.1f} ms, p95 "
              f"{sorted(times)[94] * 1000:.1f} ms over 100 ticks (2,000 jobs, 10 executors, 10 runs ended and "
              "10 offered a tick)")
        self.assertLess(median, 0.050 * (3 if ON_CI else 1))


class HostDefaults(Lab):
    """The host machine's CPU sharing (P4.5.6): on by default with the mode's figures, a 0 the default's, so its job
    host is always given concrete limits; any other machine's off by default."""

    def test_the_modes_figures(self):
        from citar import machine_settings as M
        vps = M.host_cpu_defaults("server", cpus=1, ram_mb=961)
        self.assertEqual({k: vps[k] for k in ("enabled", "threads", "cpu_percent", "memory_mb")},
                         {"enabled": True, "threads": 1, "cpu_percent": 50, "memory_mb": 240})
        self.assertEqual(M.host_cpu_defaults("server", cpus=4, ram_mb=16384)["memory_mb"], 1024, "1,024 MB at most")
        laptop = M.host_cpu_defaults("local", cpus=12, ram_mb=32768)
        self.assertEqual({k: laptop[k] for k in ("threads", "cpu_percent", "memory_mb")},
                         {"threads": 6, "cpu_percent": 50, "memory_mb": 3072})
        self.assertEqual(laptop["work"], list(CPU_WORK))
        self.assertEqual(M.host_cpu_defaults("local", cpus=1, ram_mb=1024)["threads"], 1)

    def test_the_host_row_is_on_by_default_and_a_zero_is_the_default(self):
        from citar import machine_settings as M
        with db.session() as s:
            s.get(Server, "sv_host").capabilities = {}
        servers.invalidate()
        host = servers.machine_settings("sv_host")["cpu"]
        defaults = M.host_cpu_defaults()
        self.assertTrue(host["enabled"])
        self.assertEqual({k: host[k] for k in ("threads", "cpu_percent", "memory_mb")},
                         {k: defaults[k] for k in ("threads", "cpu_percent", "memory_mb")})
        self.assertFalse(servers.machine_settings("sv_gpu")["cpu"]["enabled"], "any other machine: off")
        with db.session() as s:
            row = s.get(Server, "sv_host")
            changed = M.update(row, {"capabilities": {"llm": {"allow_swaps": True}}})
            self.assertNotIn("cpu.enabled", changed, "a change of something else keeps the host's CPU on")
            self.assertTrue(row.capabilities["cpu"]["enabled"])
            M.update(row, {"capabilities": {"cpu": {"threads": 0, "enabled": False}}})
        servers.invalidate()
        off = servers.machine_settings("sv_host")["cpu"]
        self.assertFalse(off["enabled"])
        self.assertEqual(off["threads"], defaults["threads"], "a 0 is the default's")
        f = L.figures(servers.machine_settings("sv_host"))
        self.assertTrue(all(f[k] > 0 for k in ("threads", "cpu_percent", "memory_mb")), f)
        self.assertTrue(all(L.figures(None)[k] > 0 for k in ("threads", "cpu_percent", "memory_mb")))

    def test_an_executor_turned_off_plays_nothing(self):
        self.host_cpu(enabled=False)
        self.submit("alice", duels("off", 1))
        local = self.local()
        d = self.dispatcher([local])
        for _ in range(3):
            d.tick()
        self.assertEqual((local.state, local.host), ("off", None))
        (job,) = d.jobs.values()
        self.assertIn("CPU work is off in the host's machine settings", d.waiting(job))


class Costs(Lab):
    def test_the_cost_table_is_seeded_and_learns(self):
        d = self.dispatcher([Fake(auto=True)])
        d.tick()
        committed = store.Costs.committed()
        self.assertGreaterEqual(len(committed), 10)
        duel30 = d.costs.estimate("lab_game", "duel", 2, 30)
        self.assertTrue(duel30["exact"])
        # a shape not measured is estimated from the nearest of its family, scaled
        far = d.costs.estimate("lab_game", "duel", 2, 1200)
        self.assertFalse(far["exact"])
        self.assertGreater(far["cpu_s"], d.costs.estimate("lab_game", "duel", 2, 500)["cpu_s"])
        self.assertEqual(d.costs.estimate("ranking_game", "small", 4, 330)["exact"], False)
        self.submit("alice", duels("cost", 3))
        self.until(d, lambda: (lab.DONE / "cost.json").exists())
        d.tick()
        with db.session() as s:
            from citar.db.models import JobCost
            row = s.get(JobCost, ("lab_game", "duel", 2, 0))
        self.assertEqual(row.samples, 3 + 3, "the committed samples and the three runs")


# ---------------------------------------------------------------------------------------------------------- 4-09's review

class Timing(Lab):
    """A run is timed by when its frames arrived, never by when the dispatcher read them (the review's first finding:
    a game shorter than a tick had its accept and its result read together, so the server "observed" it for a few
    milliseconds and flagged its honest CPU ``overreported_cpu``)."""

    def test_a_run_read_late_is_timed_by_its_frames(self):
        self.host_cpu(threads=1)
        self.submit("alice", duels("timed", 1, turns=300))         # about a second of CPU here
        local = self.local()
        d = self.dispatcher([local])
        self.until(d, lambda: d.runs)
        # nothing reads the host's frames until its game is over: its accept and its result are read in one tick
        deadline = time.perf_counter() + 120
        while True:
            with local.host._frames.mutex:
                frames = list(local.host._frames.queue)
            if any(f.get("t") == "job_result" for f in frames):
                break
            self.assertLess(time.perf_counter(), deadline, "the game did not end")
            time.sleep(0.05)
        d.tick()
        (ev,) = [e for e in self.events() if e["activity"].startswith("lab:timed:")]
        with db.session() as s:
            (run,) = s.scalars(select(CpuJobRun)).all()
            span = (D._utc_ts(run.ended_at) - D._utc_ts(run.started_at))
            wall = run.wall_s
        print(f"\na late-read run: host wall {wall:.2f} s, observed {ev['observed']:.2f} s, reported CPU "
              f"{ev['reported']:.2f} s, counted {ev['cpu']:.2f} s")
        self.assertGreaterEqual(ev["observed"], 0.9 * wall - 0.05, "observed from its accept's arrival to its result's")
        self.assertAlmostEqual(span, ev["observed"], delta=0.05, msg="the run's row is timed by its frames too")
        self.assertNotIn("overreported_cpu", ev["flags"])
        self.assertGreaterEqual(ev["observed"], ev["reported"] / D.OBSERVED_FACTOR - D.OBSERVED_SLACK)
        self.assertEqual(ev["cpu"], round(ev["reported"], 6), "an honest run's CPU is counted whole")


class Wake(Lab):
    def test_a_run_ending_wakes_the_dispatcher(self):
        """P4.5.5: the dispatcher wakes on a job frame, not only every two seconds. With its timer at 30 s, twelve
        short duels on two threads (six offers each) end within seconds: each end, and the host's start, poke it."""
        self.submit("alice", duels("quick", 12))
        with mock.patch.object(D, "TICK_SECONDS", 30.0):
            d = self.dispatcher()
            t0 = time.perf_counter()
            d.start()
            bound = 20.0 * (3 if ON_CI else 1)
            while not (lab.DONE / "quick.json").exists():
                self.assertLess(time.perf_counter() - t0, bound, "the games waited for the dispatcher's timer: "
                                + lab._tail(lab.LAB / "lab.log", 2000))
                time.sleep(0.05)
            took = time.perf_counter() - t0
        print(f"\ntwelve 30-round duels on two threads, the timer at 30 s: {took:.1f} s")
        self.assertEqual(sorted(r["i"] for r in self.lines("quick")), list(range(12)))


class PausedDeadlines(Lab):
    def test_a_deadline_failure_its_pause_caused_is_neither_an_attempt_nor_an_abandon(self):
        """The job host ends a run held past twice its deadline itself (``job_failed`` ``deadline``), before the
        server's backstop: when its active time was within its deadline its pause caused it, so it is no attempt and
        no abandon, its paused time counted from the report that said it was held. A deadline its play passed is."""
        with db.session() as s:
            s.add(Server(id="sv_box", owner_id=self.ids["bob"], name="Bob box", kind="owned", provider="dryrun",
                         reach="worker", max_concurrent=1, config={}, trust_tier="trusted",
                         public_key=test_key()))
        servers.invalidate()
        clock = Clock()
        self.submit("alice", duels("held", 1))
        fake = Fake("sv_box", name="Bob box", kind="remote", threads=1)
        d = self.dispatcher([fake], clock=clock)
        self.until(d, lambda: fake.offers)
        d.tick()                                                 # its accept, at 1000
        run, job, dl = fake.offers[0]
        clock.t += 1.0
        fake.progress(run, round=3, of=30, elapsed_s=1.0, cpu_s=0.5, paused_s=0.0, peak_mb=1.0, paused=True)
        d.tick()                                                 # held from 1001; no report after it
        clock.t = 1000.0 + 2 * dl + 1
        fake.fail(run, "deadline", message=f"{2 * dl + 1:.1f} s in all, passed twice its deadline of {dl:.1f} s")
        d.tick()
        self.assertEqual({j[0]: j[3] for j in self.jobs()}[job.ref], 0, "no attempt")
        with db.session() as s:
            row = s.get(CpuJobRun, run)
            self.assertEqual((row.state, row.error["code"], row.error.get("pause_caused")), ("failed", "deadline", True))
            self.assertGreaterEqual(row.paused_s, 2 * dl - 1, "its pause counted though no report said so")
            self.assertFalse((s.get(Server, "sv_box").trust or {}).get("abandons"), "no abandon")
        self.assertIn("held by a pause past its deadline", (lab.LAB / "lab.log").read_text(encoding="utf-8"))
        # its own pauses held it (no benchmark): that machine does not get it again for the hour, so a helper's pauses
        # cannot keep a game from the others; after the hour it does
        for _ in range(3):
            d.tick()
        self.assertEqual(len(fake.offers), 1, "not offered to the same machine within the hour")
        clock.t += D.ABANDON_WINDOW
        self.until(d, lambda: len(fake.offers) == 2)
        d.tick()
        run2 = fake.offers[1][0]
        # its helper says a pause held it, and fails it at once: the server did not see it last twice its deadline, so
        # it is an attempt
        clock.t += 5.0
        fake.progress(run2, round=3, of=30, elapsed_s=5.0, cpu_s=0.5, paused_s=4.0, peak_mb=1.0, paused=True)
        d.tick()
        clock.t += 1.0
        fake.fail(run2, "deadline", message="held past twice its deadline (a false word)")
        d.tick()
        self.assertEqual({j[0]: j[3] for j in self.jobs()}[job.ref], 1, "a deadline failure the server did not see")
        with db.session() as s:
            self.assertNotIn("pause_caused", s.get(CpuJobRun, run2).error)
        # played again; this time its play passes its deadline: an attempt
        self.until(d, lambda: len(fake.offers) == 3)
        d.tick()
        run3 = fake.offers[2][0]
        clock.t += dl + 1
        fake.fail(run3, "deadline", message=f"{dl + 1:.1f} s of play passed its deadline of {dl:.1f} s")
        d.tick()
        self.assertEqual({j[0]: j[3] for j in self.jobs()}[job.ref], 2, "a deadline its play passed is an attempt")

    def test_a_benchmark_held_past_twice_the_deadline_costs_the_game_no_attempt(self):
        """The same with the real job host and a benchmark lease (the review's probe): held past twice its deadline,
        the host fails it ``deadline``; queued again with no attempt, it plays to its end once the lease ends."""
        if not has_test_ops():
            self.skipTest("the extension has no test operations (a slowed game)")
        self.host_cpu(threads=1)
        self.submit("alice", duels("bench-held", 1, turns=40))
        d = self.dispatcher([self.local({"CITAR_JOBS_TEST_STEP_MS": "20"})])
        sched = S.get()
        # 3 s of play on the laptop. CI's three-core macOS runner played 3 s without reaching round 2 (test.yml run
        # 38933789735: three deadline failures, given up), and with 10 s it passed the deadline twice when the game was
        # replayed after the hold (run 38937992921: two attempts where none were due); so CI's is 30 s, and the wait
        # for the held run scales with it. The game stays 40 turns: a shorter one ended before the hold began.
        deadline = 30.0 if os.environ.get("CI") else 3.0
        with mock.patch.object(D, "DEADLINE_MIN", deadline):
            self.until(d, lambda: any((r.round or 0) >= 2 for r in d.runs.values()))
            (run,) = d.runs.values()
            self.assertEqual(run.deadline_s, deadline)
            lease = sched.lease("sv_host", "dry-run", kind="benchmark", label="benchmark run")
            try:
                failed = self.until(d, lambda: self._failed_run(), timeout=3 * deadline + 10, step=0.1)
            finally:
                lease.release()
            self.until(d, lambda: (lab.DONE / "bench-held.json").exists(), timeout=60)
        self.assertEqual((failed.error["code"], failed.error.get("pause_caused")), ("deadline", True), failed.error)
        self.assertIn("twice its deadline", failed.error["message"])
        self.assertEqual([j[3] for j in self.jobs()], [0], "no attempt")
        self.assertEqual(len(self.lines("bench-held")), 1)

    def _failed_run(self):
        with db.session() as s:
            return next(iter(s.scalars(select(CpuJobRun).where(CpuJobRun.state == "failed"))), None)


class FailedGames(Lab):
    def _fail_three_times(self, d, fake):
        for n in range(D.MAX_ATTEMPTS):
            self.until(d, lambda n=n: len(fake.offers) == n + 1)
            fake.fail(fake.offers[n][0], "host")
            d.tick()

    def test_a_game_failed_three_times_is_played_again_at_the_next_start(self):
        """0.1.5's runner tried a failed game again at its next start; the dispatcher does too, its attempts back to
        0. One refused for its spec stays failed."""
        self.submit("alice", duels("flaky", 2))
        fake = Fake(threads=1)
        d = self.dispatcher([fake])
        self._fail_three_times(d, fake)
        jobs = {j[0]: j for j in self.jobs()}
        self.assertEqual((jobs["lab:flaky:0"][1], jobs["lab:flaky:0"][3]), ("failed", 3))
        with db.session() as s:          # the other refused for its spec (failed for good, with fewer attempts)
            row = s.scalars(select(CpuJob).where(CpuJob.ref == "lab:flaky:1")).one()
            row.state = "failed"
        d.stop()
        fake2 = Fake(threads=1, auto=True)
        d2 = self.dispatcher([fake2])
        self.until(d2, lambda: self.lines("flaky"))
        for _ in range(3):
            d2.tick()
        self.assertEqual([r["i"] for r in self.lines("flaky")], [0], "played again, its attempts back to 0")
        self.assertEqual({j[0]: j[1] for j in self.jobs()}, {"lab:flaky:0": "accepted", "lab:flaky:1": "failed"})
        self.assertIn("1 game(s) that had failed 3 times are queued again", (lab.LAB / "lab.log").read_text("utf-8"))

    def test_an_experiment_deleted_and_queued_again_plays_its_failed_games(self):
        self.submit("alice", duels("redo", 1))
        fake = Fake(threads=1)
        d = self.dispatcher([fake])
        self._fail_three_times(d, fake)
        self.assertEqual([j[1] for j in self.jobs()], ["failed"])
        (lab.QUEUE / "redo.json").unlink()
        d.tick()
        self.assertEqual([j[1] for j in self.jobs()], ["cancelled"], "a deleted experiment's failed games go with it")
        lab.submit(duels("redo", 1))
        fake.auto = True
        self.until(d, lambda: (lab.DONE / "redo.json").exists())
        self.assertEqual([r["i"] for r in self.lines("redo")], [0])

    def test_an_administrators_retry_plays_the_failed_games_again(self):
        self.submit("alice", duels("again", 1))
        fake = Fake(threads=1)
        d = self.dispatcher([fake])
        self._fail_three_times(d, fake)
        for _ in range(2):
            d.tick()
        self.assertEqual(len(fake.offers), 3, "failed: not offered again")
        self.assertEqual(d.published["experiments"]["again"]["failed"], [0])
        fake.auto = True
        d.retry("again")
        self.until(d, lambda: (lab.DONE / "again.json").exists())
        self.assertEqual(len(fake.offers), 4)
        self.assertIn("1 failed game(s) of again queued again (an administrator's retry)",
                      (lab.LAB / "lab.log").read_text(encoding="utf-8"))


class Window(Lab):
    """Jobs no executor would take hold no place in the window (the review's probe: a one-thread executor's window
    of four filled by an experiment whose games need more memory than it has, the same owner's next experiment got
    no job for 30 ticks and said nothing)."""

    def test_games_no_executor_would_take_never_hold_the_window(self):
        self.submit("alice", duels("huge", 6, turns=200))
        self.submit("alice", duels("small", 2))
        fake = Fake(threads=1, per_run=100, auto=True)
        d = self.dispatcher([fake])
        real = d.costs.of
        d.costs.of = lambda kind, spec: ({"cpu_s": 1.0, "peak_mb": 500.0, "exact": True}
                                         if spec.get("turns") == 200 else real(kind, spec))
        self.until(d, lambda: (lab.DONE / "small.json").exists(), timeout=60)
        self.assertEqual({j[0].split(":")[1] for j in self.jobs()}, {"small"}, "no row for a game nobody would take")
        info = d.published["experiments"]["huge"]
        self.assertEqual(info["waiting"], "this server: it needs 500 MB a run, and this server's job host leaves "
                                          "each run 100 MB")
        shown = {e["name"]: e for e in lab.overview()["experiments"]}["huge"]
        self.assertEqual(shown["waiting"], info["waiting"], "the Lab page says why")
        # the executor given more memory: the experiment's games are made and played
        fake.memory_per_run_mb = 600
        self.until(d, lambda: (lab.DONE / "huge.json").exists(), timeout=60)

    def test_jobs_raised_past_every_executor_wait_without_holding_places(self):
        """Rows made, then raised past the only executor's memory a run (as memory deaths on a one-thread machine
        raise them): they wait, saying why, and the same owner's next experiment plays."""
        self.submit("alice", duels("a-raised", 4))                # the older by its name: first in line
        self.submit("alice", duels("b-next", 2))
        fake = Fake(threads=1, per_run=100)
        fake.state = "starting"                                  # rows made, nothing offered yet
        d = self.dispatcher([fake])
        d.tick()
        raised = [j for j in d.jobs.values() if j.exp == "a-raised"]
        self.assertEqual(len(raised), 4, "the window of one thread: four")
        with db.session() as s:
            for j in raised:
                s.get(CpuJob, j.id).mem_mb = j.mem_mb = 900
        fake.state, fake.auto = "ready", True
        self.until(d, lambda: (lab.DONE / "b-next.json").exists(), timeout=60)
        self.assertEqual({j.exp for j in d.jobs.values()}, {"a-raised"})
        self.assertIn("it needs 900 MB a run", d.published["experiments"]["a-raised"]["waiting"])


class Writing(Lab):
    def test_a_line_the_results_file_refuses_is_written_at_the_next_tick_and_the_rest_meanwhile(self):
        """Each step after a result's commit stands alone: the line refused once (a sync client's lock past the
        retry) waits for the next tick while the run's event, its cost and its usage rows are written; its game
        settles, and its result is pruned, only once its line is there."""
        self.submit("alice", duels("locked", 1))
        fake = Fake(threads=1, auto=True)
        d = self.dispatcher([fake])
        real = D.Dispatcher._write_line
        calls = []

        def flaky(path, text):
            calls.append(path)
            if len(calls) <= 2:         # at its commit, and again at that tick's end
                raise PermissionError(13, "the file is locked (a test)")
            real(path, text)
        samples = lambda: next(c for c in d.costs.rows() if (c["kind"], c["size"], c["rounds"]) ==  # noqa: E731
                               ("lab_game", "duel", 0))["samples"]
        with mock.patch.object(D.Dispatcher, "_write_line", staticmethod(flaky)):
            d.tick()
            before = samples()
            with self.assertLogs("citar.jobs.dispatcher", "WARNING"):
                self.until(d, lambda: calls)
            self.assertEqual(self.lines("locked"), [])
            self.assertEqual(len([e for e in self.events() if e["activity"] == "lab:locked:0"]), 1, "its event")
            self.assertEqual(samples(), before + 1, "its cost")
            self.assertTrue((lab.QUEUE / "locked.json").exists(), "not complete without its line")
            with db.session() as s:
                self.assertIsNotNone(s.scalars(select(CpuJobRun)).one().result, "its result kept")
            self.until(d, lambda: (lab.DONE / "locked.json").exists())
        self.assertEqual(len(self.lines("locked")), 1)
        with db.session() as s:
            self.assertIsNone(s.scalars(select(CpuJobRun)).one().result, "pruned once its line is written")


class Commits(Lab):
    """The view in memory changes only once the database has taken the change: a commit refused (SQLite still locked
    past its busy timeout) leaves every job as it was, to be offered at the next tick."""

    def _refusing(self, cls):
        from sqlalchemy.orm import Session as SASession
        real = SASession.commit

        def commit(session):
            if any(isinstance(o, cls) for o in list(session.new) + list(session.dirty)):
                raise OperationalError("COMMIT", {}, Exception("database is locked (a test)"))
            return real(session)
        return mock.patch.object(SASession, "commit", commit)

    def test_an_offer_the_database_refuses_leaves_the_job_waiting(self):
        self.submit("alice", duels("locked-offer", 2))
        fake = Fake(threads=2)
        fake.state = "starting"
        d = self.dispatcher([fake])
        d.tick()
        fake.state = "ready"
        with self._refusing(CpuJobRun), self.assertRaises(OperationalError):
            d.tick()
        self.assertEqual(fake.offers, [])
        self.assertEqual(d.runs, {})
        self.assertEqual({j.state for j in d.jobs.values()}, {"queued"})
        fake.auto = True
        self.until(d, lambda: (lab.DONE / "locked-offer.json").exists())
        self.assertEqual(len(fake.offers), 2, "each offered once")

    def test_jobs_the_database_refuses_are_made_again(self):
        self.submit("alice", duels("locked-fill", 2))
        fake = Fake(threads=1, auto=True)
        d = self.dispatcher([fake])
        with self._refusing(CpuJob), self.assertRaises(OperationalError):
            d.tick()
        self.assertEqual(d.jobs, {})
        self.assertEqual(d.exps["locked-fill"].has_job, set(), "the games have no job: made again")
        self.until(d, lambda: (lab.DONE / "locked-fill.json").exists())
        self.assertEqual(sorted(r["i"] for r in self.lines("locked-fill")), [0, 1])


class MemoryBounds(Lab):
    def test_a_helpers_memory_figures_are_bounded_by_the_dispatchers_own(self):
        """A remote run's peak is its helper's word: a memory death raises its job at most past that executor's
        memory a run (the frame's ``per_run_mb`` and ``alone`` ignored), so no helper prices a job out of every
        machine; its peak never enters the cost table; no figure passes ``MEM_MAX``."""
        with db.session() as s:
            s.add(Server(id="sv_box", owner_id=self.ids["bob"], name="Bob box", kind="owned", provider="dryrun",
                         reach="worker", max_concurrent=1, config={}, trust_tier="trusted",
                         public_key=test_key()))
        servers.invalidate()
        self.submit("alice", duels("bounded", 2))
        fake = Fake("sv_box", name="Bob box", kind="remote", threads=1, per_run=100)
        d = self.dispatcher([fake])
        self.until(d, lambda: fake.offers)
        run, job, _ = fake.offers[0]
        fake.progress(run, round=1, of=30, elapsed_s=0.1, cpu_s=0.1, paused_s=0.0, peak_mb=1e15, paused=False)
        d.tick()
        self.assertEqual(d.runs[run].peak_mb, D.MEM_MAX)
        fake.fail(run, "memory", per_run_mb=10 ** 9, alone=True)
        d.tick()
        self.assertEqual({j[0]: j[2] for j in self.jobs()}[job.ref], 101, "past this executor's memory a run, no more")
        before = {(c["kind"], c["size"], c["rounds"]): c["peak_mb"] for c in d.costs.rows()}
        fake.memory_per_run_mb = 5000
        self.until(d, lambda: len(fake.offers) >= 2)
        run2 = fake.offers[1][0]
        fake.play(run2, peak=4000.0)
        d.tick()
        after = {(c["kind"], c["size"], c["rounds"]): c["peak_mb"] for c in d.costs.rows()}
        self.assertEqual(after[("lab_game", "duel", 0)], before[("lab_game", "duel", 0)],
                         "a helper's peak is not the cost table's")


class Naming(Lab):
    """A member is never told the host machine's name or id (the review: ``/api/lab`` carried the host row's name and
    id in its executors, and why a game waits named it), with the real local executor's naming."""

    def test_a_member_is_never_told_the_host_machines_name_or_id(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        self.host_cpu(threads=1, memory_mb=200)
        self.submit("alice", duels("named", 2))
        local = self.local()
        d = self.dispatcher([local])
        d.tick()
        (first,) = [j for j in d.jobs.values() if j.i == 0]
        with db.session() as s:
            s.get(CpuJob, first.id).mem_mb = first.mem_mb = 900
        self.until(d, lambda: len(self.lines("named")) == 1)
        d.tick()
        with db.session() as s:
            host_name = s.get(Server, "sv_host").name
        self.assertEqual(local.name, host_name, "the executor's own name is the host row's")
        for who in ("alice", "bob"):
            lab_view = client.get("/api/lab", headers=self.headers[who]).json()
            queue = client.get("/api/queue", headers=self.headers[who]).json()["cpu"]
            text = json.dumps([lab_view, queue])
            for secret in (host_name, "sv_host"):
                self.assertNotIn(secret, text, f"{who} is told {secret}")
            self.assertEqual([e["name"] for e in lab_view["dispatcher"]["executors"]], ["this server"])
            self.assertEqual([e["name"] for e in queue["executors"]], ["this server"])
        shown = {e["name"]: e for e in client.get("/api/lab", headers=self.headers["alice"]).json()["experiments"]}
        self.assertRegex(shown["named"]["waiting"], r"^this server: it needs 900 MB a run")
        admin = client.get("/api/lab", headers=self.headers["zara"]).json()
        self.assertIn(host_name, json.dumps(admin), "an administrator is told it")
        self.assertRegex({e["name"]: e for e in admin["experiments"]}["named"]["waiting"],
                         rf"^this server \({host_name}\): it needs 900 MB a run")


if __name__ == "__main__":
    unittest.main()


# ---------------------------------------------------------------------------------------------------------- 4-10

class Remote(Lab):
    """Remote executors (package 4-10, its gate 4: P4.5.5's eligibility). Bob's machine, in a group of his, trusted by
    an administrator, lends one thread through its helper (a connection with no socket that records what the hub sends
    it); each refusal leaves the job queued, and once a machine that takes it appears (a stand-in for the host's own
    executor), the job is offered there."""

    def setUp(self):
        super().setUp()
        from citar.db.models import ServerGroup
        from citar.jobs import remote
        remote.forget_owner_tiers()
        self.addCleanup(remote.forget_owner_tiers)
        self.registry = remote.Remotes()
        with db.session() as s:
            group = ServerGroup(owner_id=self.ids["bob"], name="Bob's machines", slug="bobs-machines")
            s.add(group)
            s.flush()
            row = Server(owner_id=self.ids["bob"], name="Bob's PC", kind="owned", provider="lmstudio", reach="worker",
                         max_concurrent=1, group_id=group.id, trust_tier="trusted", public_key=test_key(),
                         capabilities={"cpu": {"enabled": True, "threads": 1, "cpu_percent": 50, "memory_mb": 1024,
                                               "work": ["lab_game", "ranking_game"], "yield_to_llm": True}})
            s.add(row)
            s.flush()
            self.sid, self.group_id = row.id, group.id
        servers.invalidate()

    def grant(self, who: str, purposes: list) -> None:
        from citar import pool
        from citar.db.models import ServerGroup
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, self.group_id),
                              subject_type="user", subject_handle=who, purposes=purposes)

    def connect(self, *, lent: bool = True, verifiable: bool = True, jobs_build=None, site_cpu: bool = True,
                work=("lab_game", "ranking_game"), per_run: int = 500, quiet_until=None, threads: int = 1):
        """Bob's helper connected: its hello (CPU lent or not, its canary's verdict, its jobs build) and its
        acknowledgement of the site's settings."""
        from tests.helperfake import StubConnection, hello, settings as site_settings
        site = site_settings(llm=False)
        site["cpu"] = {"enabled": site_cpu, "threads": threads, "cpu_percent": 50, "memory_mb": 1024,
                       "work": list(work), "yield_to_llm": True}
        conn = StubConnection(self.sid, site=site)
        h = hello(hostname="bob-pc", llm=False, quiet_until=quiet_until)
        h["helper"]["jobs_build"] = jobs_build or engine_api.jobs_build()
        h["cpu"] = {"enabled": lent, "ceiling": {"threads": threads, "cpu_percent": 50, "memory_mb": 1024,
                                                  "work": list(work)},
                    "limits": {"cpu": "hard", "memory": "hard"}, "verifiable": verifiable, "bots": [],
                    "ruleset": "0" * 64}
        conn.hello, conn.jobs_build, conn.memory_per_run_mb = h, h["helper"]["jobs_build"], per_run
        conn.quiet_until = quiet_until
        conn.acknowledge({"effective": {"llm": {"enabled": False, "slots": 0},
                                        "cpu": {**site["cpu"], "enabled": site_cpu and lent}},
                          "capped": [] if lent else ["cpu.enabled"], "limits": {"cpu": "hard", "memory": "hard"},
                          "memory_per_run_mb": per_run})
        e = self.registry.get(self.sid, "Bob's PC")
        e.connected(conn)
        return e, conn

    def offers(self, conn) -> list:
        return [f for f in conn.frames if f.get("t") == "job_offer"]

    def refused(self, who: str, spec: dict, why: str, *, connect: bool = True, **kw):
        """``who``'s experiment is not offered to bob's helper, saying ``why``; the job stays queued, and a machine
        that takes it gets it."""
        conn = None
        if connect:
            _e, conn = self.connect(**kw)
        self.submit(who, spec)
        d = self.dispatcher([], remotes=self.registry)
        for _ in range(3):
            d.tick()
        if conn is not None:
            self.assertEqual(self.offers(conn), [], "the helper was offered the job")
        # its game waits (a job no executor would take gets no row: its experiment says why), saying why
        self.assertTrue(all(j.state == "queued" for j in d.jobs.values()))
        waiting = d.published["experiments"][spec["name"]].get("waiting") or ""
        self.assertIn(why, waiting)
        other = Fake("sv_host", name="this server")
        d._own.append(other)
        d.tick()
        self.assertEqual(len(other.offers), 1, "a machine that takes it gets it")
        return d

    def test_offline(self):
        e, conn = self.connect()
        e.disconnected(conn)                # its helper went: the executor stays, offered nothing
        self.refused("bob", duels("off-line", 1), "its helper is not connected", connect=False)
        self.assertEqual(self.offers(conn), [])

    def test_cpu_off_on_the_site(self):
        self.refused("bob", duels("site-off", 1), "CPU work is off in its machine settings", site_cpu=False)

    def test_cpu_off_on_the_machine(self):
        self.refused("bob", duels("not-lent", 1), "not lent by this machine's owner", lent=False)

    def test_a_kind_not_opted_in(self):
        self.refused("bob", duels("not-this-kind", 1), "leave out lab games", work=("ranking_game",))

    def test_another_jobs_build(self):
        self.refused("bob", duels("other-build", 1), "its helper's is 000000000000", jobs_build="000000000000")

    def test_not_enough_memory_a_run(self):
        with mock.patch.object(store.Costs, "of", lambda self, kind, spec: {"cpu_s": 0.2, "peak_mb": 100.0}):
            self.refused("bob", duels("too-big", 1), "its job host leaves each run 50 MB", per_run=50)

    def test_a_quarantined_owner(self):
        with db.session() as s:
            s.get(User, self.ids["bob"]).trust_tier = "quarantined"
        self.refused("bob", duels("quarantined", 1), "its owner is quarantined")

    def test_a_new_machine_with_no_verifier_waits_for_one(self):
        """4-10's "trusted machines only" is gone (4-11): a new machine takes work once a verifier can check its
        results, and waits for one while none can (P4.5.8 rule 3)."""
        with db.session() as s:
            s.get(Server, self.sid).trust_tier = "new"
        servers.invalidate()
        self.refused("bob", duels("untrusted", 1), "waiting for a verifier")

    def test_its_helpers_quiet_hours(self):
        self.refused("bob", duels("quiet", 1), "its helper's own quiet hours", quiet_until=time.time() + 3600)

    def test_a_live_benchmark_lease(self):
        _e, conn = self.connect()
        lease = S.get().lease(self.sid, "dry-run", kind="benchmark", label="benchmark run")
        self.addCleanup(lease.release)
        self.refused("bob", duels("benchmark", 1), "a benchmark holds its machine", connect=False)
        self.assertEqual(self.offers(conn), [])

    def test_no_grant(self):
        self.refused("alice", duels("no-grant", 1), "You do not have permission")

    def test_a_grant_without_lab(self):
        self.grant("alice", ["game", "pooled"])
        self.refused("alice", duels("no-lab", 1), "limited to")

    def test_a_grant_with_no_purposes_never_lends_the_cpu(self):
        self.grant("alice", [])
        self.refused("alice", duels("empty-purposes", 1), "does not include lab runs")

    def test_an_administrators_own_experiment_needs_a_grant_too(self):
        self.refused("zara", duels("admins-own", 1), "You do not have permission")

    def test_a_grant_listing_lab_its_owners_own_and_the_collection_are_offered(self):
        """What is lent: a member's experiment under a grant listing ``lab``, the machine owner's own, and the server's
        own ranking games (its work lists them); the offer carries the job's figures."""
        self.grant("alice", ["lab"])
        _e, conn = self.connect(threads=3)
        self.submit("alice", duels("granted", 1))
        self.submit("bob", duels("bobs-own", 1))
        self.submit("zara", duels("ladder", 1, kind="ranking"))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        offers = self.offers(conn)
        self.assertEqual(sorted(f["spec"]["exp"] for f in offers), ["bobs-own", "granted", "ladder"])
        for offer in offers:
            self.assertEqual(offer["jobs_build"], engine_api.jobs_build())
            self.assertEqual(offer["offer_ttl_s"], 30)
            self.assertGreaterEqual(offer["deadline_s"], D.DEADLINE_MIN)
        self.assertEqual({f["kind"] for f in offers}, {"lab_game", "ranking_game"})

    def test_a_helpers_result_is_acknowledged_lined_and_credited_to_its_owner(self):
        """A helper's result into the lab and the ledger (P4.5.7): its line names the machine and ``unverified``, its
        event is given by the machine's owner and used by the experiment's owner, its CPU bounded by 1.5 times the
        reference cost; it is acknowledged once committed."""
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("credited", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        out = engine_api.run_job(offer["kind"], offer["spec"], spec_hash=offer["spec_hash"])
        e.frame({"t": "job_accept", "run": offer["run"]})
        e.frame(result_frame(offer, out, self.sid, cpu_s=3600.0))
        d.tick()
        self.led.flush()
        self.assertIn({"t": "job_result_ack", "run": offer["run"]},
                      [{k: v for k, v in f.items() if k != "ts"} for f in conn.frames])
        (line,) = lab.load_results("credited")
        self.assertEqual(line["executor"], {"machine": self.sid, "run": offer["run"], "verification": "unverified"})
        with db.session() as s:
            (ev,) = s.scalars(select(ComputeEvent).where(ComputeEvent.request_id == offer["run"])).all()
            self.assertEqual((ev.provider_id, ev.consumer_id), (self.ids["bob"], self.ids["alice"]))
            self.assertEqual(ev.verification, "unverified")
            cost = d.costs.of("lab_game", offer["spec"])["cpu_s"]
            self.assertLessEqual(ev.cpu_core_s, 1.5 * cost + 1e-6, "bounded by 1.5 x the reference cost")
            self.assertTrue(ev.flags.get("overreported_cpu") or ev.flags.get("capped_cpu"), ev.flags)
            run = s.get(CpuJobRun, offer["run"])
            self.assertEqual((run.state, run.executor, run.verdict), ("done", "remote", "unchecked"))

    def test_a_run_under_a_grant_with_a_budget_settles_its_credits_into_the_budget(self):
        """A member's run under a grant with a budget holds the budget from its offer and settles the run's credits
        into it with its event (P4.5.7), as a model step does."""
        from citar import pool
        from citar.db.models import BudgetEntry, ServerGroup
        from citar.pool import budgets
        budgets.reset_keeper()
        self.addCleanup(budgets.reset_keeper)
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, self.group_id),
                              subject_type="user", subject_handle="alice", purposes=["lab"],
                              budget={"period": "month", "amount": 1000, "unit": "credits", "behavior": "hard"})
        with db.session() as s:
            budgets.keeper().sync(s)
        e, conn = self.connect()
        self.submit("alice", duels("budgeted", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        self.assertIn(offer["run"], e.holds, "the offer holds the budget")
        out = engine_api.run_job(offer["kind"], offer["spec"], spec_hash=offer["spec_hash"])
        e.frame({"t": "job_accept", "run": offer["run"]})
        e.frame(result_frame(offer, out, self.sid))
        d.tick()
        self.led.flush()
        self.assertNotIn(offer["run"], e.holds)
        with db.session() as s:
            (ev,) = s.scalars(select(ComputeEvent).where(ComputeEvent.request_id == offer["run"])).all()
            (entry,) = s.scalars(select(BudgetEntry)).all()
            self.assertAlmostEqual(entry.actual, ev.credits, places=6)
            self.assertEqual(entry.user_id, self.ids["alice"])

    def test_a_machines_jobs_are_its_owners_and_administrators(self):
        """``GET /api/machines/{id}/jobs`` (P4.5.10, P4.7.3): a visitor 401; a stranger and a moderator "not found"; a
        member a grant lends it to 403; its owner and an administrator 200, with its runs (an experiment its owner may
        not view named "a member's experiment"), what its helper lends and its trust counters."""
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("lent", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        path = f"/api/machines/{self.sid}/jobs"

        def call(who):
            return client.get(path, headers=self.headers.get(who, {"origin": "https://citar.test"}))
        self.assertEqual(call(None).status_code, 401)
        self.assertEqual(call("mona").status_code, 404, "a moderator is a member here")
        self.assertEqual(call("alice").status_code, 403, "a grant lends it to her: she may see it, not manage it")
        self.assertEqual(client.get("/api/machines/sv_nobody/jobs", headers=self.headers["zara"]).status_code, 404)
        r = call("bob")
        self.assertEqual(r.status_code, 200, r.text)
        body = r.json()
        (run,) = body["runs"]
        self.assertEqual((run["run"], run["state"], run["kind"]), (offer["run"], "offered", "lab_game"))
        self.assertEqual(run["experiment"], "a member's experiment", "alice's experiment is not bob's to see")
        self.assertNotIn("verdict", run, "a verdict once its job settles")
        self.assertIn("abandons", body["trust"])
        admin = call("zara").json()
        self.assertEqual(admin["runs"][0]["experiment"], "lent")
        self.assertEqual(admin["runs"][0]["role"], "primary")

    def test_the_dialog_says_what_is_lent_and_the_grants_that_lend_it(self):
        """The machine settings dialog's read (P4.5.11): the CPU its helper lends in effect, each limit hard or soft,
        from its last hello and acknowledgement; "not lent" when its own switch is off; and the grants that list
        ``lab`` (one with no purposes is not among them)."""
        from citar import machine_settings as M
        from citar.server import workers as W
        self.grant("alice", [])
        with db.session() as s:
            row = s.get(Server, self.sid)
            self.assertIsNone(M.client(s, row, s.get(User, self.ids["bob"]))["cpu_helper"])
            self.assertEqual(M.client(s, row, s.get(User, self.ids["bob"]))["lab_grants"], [])
        self.grant("zara", ["lab", "game"])
        e, conn = self.connect()
        W.record_hello(self.sid, conn.hello)
        W.record_ack(self.sid, conn.ack)
        with db.session() as s:
            out = M.client(s, s.get(Server, self.sid), s.get(User, self.ids["bob"]))
        cpu = out["cpu_helper"]
        self.assertTrue(cpu["lent"] and cpu["verifiable"] and cpu["plays_jobs"])
        self.assertEqual(cpu["limits"], {"cpu": "hard", "memory": "hard"})
        self.assertEqual(cpu["memory_per_run_mb"], 500)
        self.assertEqual(cpu["effective"]["threads"], 1)
        self.assertEqual(out["lab_grants"], [{"subject": "zara", "group": "Bob's machines"}])
        _e, conn = self.connect(lent=False)
        W.record_hello(self.sid, conn.hello)
        with db.session() as s:
            self.assertFalse(M.client(s, s.get(Server, self.sid), None)["cpu_helper"]["lent"])

    def test_a_helper_that_stops_gives_its_games_back_at_no_cost(self):
        """A helper stopped by its owner fails its runs ``aborted_locally`` (PROTOCOL.md): each game is queued again
        at once, neither an attempt nor an abandon, and offered to another machine at once. The machine itself is
        offered nothing until its helper's next hello (an honest helper closes its connection as it stops; one that
        stays connected cannot take a game and give it back for ever)."""
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("stopped-helper", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        e.frame({"t": "job_failed", "run": offer["run"], "code": "aborted_locally",
                 "message": "the helper is stopping", "retryable": True})
        d.tick()
        with db.session() as s:
            run = s.get(CpuJobRun, offer["run"])
            self.assertEqual((run.state, (run.error or {}).get("code")), ("failed", "aborted_locally"))
            job = s.get(CpuJob, run.job_id)
            self.assertEqual(job.attempts or 0, 0, "not an attempt")
            self.assertEqual((s.get(Server, self.sid).trust or {}).get("abandons", 0), 0, "not an abandon")
        self.assertEqual(d.backoff(self.sid), ["stopped"])
        for _ in range(3):
            d.tick()
        self.assertEqual(len(self.offers(conn)), 1, "not offered again over the same connection")
        other = Fake("sv_host", name="this server")
        d._own.append(other)
        d.tick()
        self.assertEqual([j.ref for _r, j, _dl in other.offers], [job.ref], "offered elsewhere at once")
        d._own.remove(other)
        # its next hello clears it
        e.disconnected(conn)
        _e, conn2 = self.connect()
        self.submit("alice", duels("after-the-restart", 1))
        d.tick()
        d.tick()
        self.assertEqual([f["spec"]["exp"] for f in self.offers(conn2)], ["after-the-restart"])
        self.assertEqual(d.backoff(self.sid), [])


    # ------------------------------------------------------------------ the review's findings (4-10's fix round)
    def result(self, offer: dict) -> dict:
        """A helper's ``job_result`` for ``offer``, played here."""
        out = engine_api.run_job(offer["kind"], offer["spec"], spec_hash=offer["spec_hash"])
        return result_frame(offer, out, self.sid)

    def budget(self, amount: float) -> None:
        """Alice's grant on bob's group, listing ``lab``, with a hard monthly budget of ``amount`` credits."""
        from citar import pool
        from citar.db.models import ServerGroup
        from citar.pool import budgets
        budgets.reset_keeper()
        self.addCleanup(budgets.reset_keeper)
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, self.group_id),
                              subject_type="user", subject_handle="alice", purposes=["lab"],
                              budget={"period": "month", "amount": amount, "unit": "credits", "behavior": "hard"})
        with db.session() as s:
            budgets.keeper().sync(s)

    def trust(self) -> dict:
        with db.session() as s:
            return dict(s.get(Server, self.sid).trust or {})

    def attempts(self) -> list:
        return [j[3] for j in self.jobs()]

    def test_a_budget_with_no_room_keeps_its_owners_jobs_from_the_machine_at_no_cost(self):
        """A hard budget with room for two holds of a credit: the third job is not offered (no run is made for it, so
        nothing expires as the lender's abandon or the job's attempt), and is once the two runs settled."""
        self.budget(2.5)
        clock = Clock()
        e, conn = self.connect(threads=3)
        self.submit("alice", duels("budget-three", 3))
        d = self.dispatcher([], remotes=self.registry, clock=clock)
        with self.assertNoLogs("citar.jobs.dispatcher", level="ERROR"):
            d.tick()
        offers = self.offers(conn)
        self.assertEqual(len(offers), 2)
        self.assertEqual(len(d.runs), 2, "no run for the third")
        self.assertEqual(sorted(e.holds), sorted(o["run"] for o in offers), "each offer holds the budget")
        for o in offers:
            e.frame({"t": "job_accept", "run": o["run"]})
        d.tick()
        clock.t += D.OFFER_TTL_SECONDS + 1
        d.tick()
        self.assertEqual(len(self.offers(conn)), 2)
        self.assertFalse(self.trust().get("abandons"), "the lender is not charged for the borrower's budget")
        self.assertEqual(self.attempts(), [0, 0, 0])
        with db.session() as s:
            self.assertEqual(s.scalars(select(CpuJobRun).where(CpuJobRun.state != "running")).all(), [])
        for o in offers:
            e.frame(self.result(o))
        for _ in range(3):
            d.tick()
        self.assertEqual(len(self.offers(conn)), 3, "offered once the two runs settled a little of the budget")

    def test_an_offer_never_sent_or_lost_with_its_connection_is_nobodys_abandon(self):
        """An offer the executor could not send ends at once and its job waits again; one that went with its helper's
        connection expires as lost: neither an attempt nor an abandon. A helper that reconnects without an offered run
        has it offered again the same way."""
        self.grant("alice", ["lab"])
        clock = Clock()
        e, conn = self.connect()
        self.submit("alice", duels("unsent", 1))
        d = self.dispatcher([], remotes=self.registry, clock=clock)
        with mock.patch.object(e, "offer", side_effect=RuntimeError("its helper's connection closed")):
            d.tick()
        with db.session() as s:
            (run,) = s.scalars(select(CpuJobRun)).all()
            self.assertEqual((run.state, run.error["code"]), ("failed", "not_sent"))
        self.assertEqual(d.runs, {})
        d.tick()
        (offer,) = self.offers(conn)
        e.disconnected(conn)
        clock.t += D.OFFER_TTL_SECONDS + 1
        d.tick()
        with db.session() as s:
            row = s.get(CpuJobRun, offer["run"])
            self.assertEqual(row.state, "expired")
            self.assertIn("lost with its helper's connection", row.error["message"])
        self.assertEqual(e.in_flight, set(), "a run the dispatcher ended holds none of its helper's threads")
        _e, conn2 = self.connect()
        d.tick()
        (offer2,) = self.offers(conn2)
        e.disconnected(conn2)
        _e, conn3 = self.connect()
        self.assertEqual(d.welcome_jobs(self.sid, [], []), ([], [], []))
        d.tick()
        with db.session() as s:
            row = s.get(CpuJobRun, offer2["run"])
            self.assertEqual((row.state, row.error["code"]), ("failed", "lost"))
        self.assertEqual(len(self.offers(conn3)), 1, "offered again")
        self.assertEqual(self.attempts(), [0])
        self.assertFalse(self.trust().get("abandons"))

    def test_a_job_aborted_nobody_asked_for_is_an_abandon_and_an_attempt(self):
        """A correct helper answers ``job_aborted`` only to the server's ``job_abort``: one nobody asked for is an
        abandon on the machine and an attempt of the job; one the dispatcher asked for is neither."""
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("unasked", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        e.frame({"t": "job_aborted", "run": offer["run"]})
        d.tick()
        with db.session() as s:
            row = s.get(CpuJobRun, offer["run"])
            self.assertEqual((row.state, row.error["code"]), ("aborted", "abandoned"))
        self.assertEqual(self.attempts(), [1])
        self.assertEqual(self.trust().get("abandons"), 1)
        offer2 = self.offers(conn)[1]
        e.frame({"t": "job_accept", "run": offer2["run"]})
        d.tick()
        self.assertTrue(d.abort_run(offer2["run"], "the test's abort"))
        e.frame({"t": "job_aborted", "run": offer2["run"]})
        d.tick()
        with db.session() as s:
            self.assertEqual(s.get(CpuJobRun, offer2["run"]).error["code"], "aborted")
        self.assertEqual(self.attempts(), [1], "asked for: no attempt")
        self.assertEqual(self.trust().get("abandons"), 1, "nor an abandon")

    def test_a_helpers_bad_spec_is_its_fault_not_the_games(self):
        """The server checked the spec, and offers a job only to a helper of its jobs build: a helper's ``bad_spec``
        queues the job again (an attempt), counts a helper fault on its machine (audited the first time) and backs the
        machine off; another machine plays the game."""
        from citar.db.models import AuditLog
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("bad-spec-liar", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_reject", "run": offer["run"], "reason": "bad_spec"})
        d.tick()
        self.assertEqual([(j[1], j[3]) for j in self.jobs()], [("queued", 1)])
        trust = self.trust()
        self.assertEqual((trust["helper_faults"], trust["last_helper_fault"]["run"]), (1, offer["run"]))
        with db.session() as s:
            self.assertEqual(s.query(AuditLog).filter(AuditLog.action == "server.helper_fault").count(), 1)
        self.assertIn("bad_spec", d.backoff(self.sid))
        other = Fake("sv_host", name="this server")
        d._own.append(other)
        d.tick()
        self.assertEqual(len(other.offers), 1, "played elsewhere")

    def test_a_helpers_run_that_no_machine_could_play_is_its_fault_not_the_games(self):
        """A helper's ``job_failed`` that is not retryable fails nothing for good: the job is queued again (an attempt),
        a helper fault counted, the machine backed off."""
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("not-retryable", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        e.frame({"t": "job_failed", "run": offer["run"], "code": "internal", "message": "no machine could",
                 "retryable": False})
        d.tick()
        self.assertEqual([(j[1], j[3]) for j in self.jobs()], [("queued", 1)])
        self.assertEqual(self.trust()["helper_faults"], 1)
        self.assertIn("fault", d.backoff(self.sid))
        self.assertEqual(d.published["experiments"]["not-retryable"]["failed"], [])

    def test_a_game_its_helpers_own_pauses_ran_out_of_time_goes_to_another_machine(self):
        """A run held by its helper's own pauses past its deadline expires as no attempt and no abandon (P4.5.5), and
        that machine is not offered the game again for the hour (another is at once): its helper's word cannot keep a
        game from the others. A run a benchmark held is offered to its machine again."""
        self.grant("alice", ["lab"])
        clock = Clock()
        e, conn = self.connect()
        self.submit("alice", duels("held-by-its-helper", 1))
        d = self.dispatcher([], remotes=self.registry, clock=clock)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        clock.t += 1.0
        e.frame({"t": "job_progress", "run": offer["run"], "round": 3, "of": 30, "elapsed_s": 1.0, "cpu_s": 0.5,
                 "paused_s": 0.0, "peak_mb": 3.0, "paused": True})
        d.tick()
        clock.t += 2 * offer["deadline_s"] + D.EXPIRY_GRACE_SECONDS + 1
        d.tick()
        with db.session() as s:
            row = s.get(CpuJobRun, offer["run"])
            self.assertEqual((row.state, row.error["message"]), ("expired", "past its deadline (its pauses)"))
        self.assertEqual(self.attempts(), [0])
        self.assertFalse(self.trust().get("abandons"))
        self.assertEqual(e.free, 1, "its thread is free for other games")
        for _ in range(3):
            d.tick()
        self.assertEqual(len(self.offers(conn)), 1, "not offered to the machine whose own pauses held it")
        other = Fake("sv_host", name="this server")
        d._own.append(other)
        d.tick()
        self.assertEqual(len(other.offers), 1, "another machine plays it at once")
        other.play(other.offers[0][0])
        d.tick()
        self.assertEqual(len(self.lines("held-by-its-helper")), 1)
        d._own.remove(other)
        clock.t += D.ABANDON_WINDOW
        # a benchmark's hold is the server's own: its machine gets the game again
        self.submit("alice", duels("held-by-a-benchmark", 1))
        d.tick()
        offer2 = self.offers(conn)[1]
        self.assertEqual(offer2["spec"]["exp"], "held-by-a-benchmark")
        e.frame({"t": "job_accept", "run": offer2["run"]})
        d.tick()
        lease = S.get().lease(self.sid, "dry-run", kind="benchmark", label="benchmark run")
        try:
            d.tick()
            clock.t += 2 * offer2["deadline_s"] + D.EXPIRY_GRACE_SECONDS + 1
            d.tick()
        finally:
            lease.release()
        d.tick()
        with db.session() as s:
            self.assertEqual(s.get(CpuJobRun, offer2["run"]).state, "expired")
        self.assertEqual([f["spec"]["exp"] for f in self.offers(conn)][2:], ["held-by-a-benchmark"],
                         "offered to its machine again")

    def test_a_result_that_crossed_its_experiments_deletion_is_acknowledged_and_dropped(self):
        """PROTOCOL.md: a run whose job is cancelled while it plays is aborted; a result that crossed the abort is
        acknowledged (the helper forgets it) and dropped: no line, no event, the run ``aborted``."""
        self.grant("alice", ["lab"])
        e, conn = self.connect()
        self.submit("alice", duels("deleted-mid-run", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        (lab.QUEUE / "deleted-mid-run.json").unlink()
        d.tick()
        self.assertIn(offer["run"], [f["run"] for f in conn.frames if f.get("t") == "job_abort"])
        e.frame(self.result(offer))
        d.tick()
        self.assertIn(offer["run"], [f["run"] for f in conn.frames if f.get("t") == "job_result_ack"])
        with db.session() as s:
            row = s.get(CpuJobRun, offer["run"])
            self.assertEqual((row.state, row.error["code"], row.result_hash), ("aborted", "cancelled", None))
        self.assertEqual(self.lines("deleted-mid-run"), [])
        self.assertEqual(self.events(), [])
        self.assertFalse(self.trust().get("abandons"))

    def test_a_benchmark_that_ends_while_its_helper_is_away_leaves_nothing_paused(self):
        """A helper's ``jobs_pause`` holds for its connection (PROTOCOL.md): a lease that ended while it was away sends
        nothing at its return, and one still live pauses it again after its welcome."""
        e, conn = self.connect()
        d = self.dispatcher([], remotes=self.registry)

        def told(c):
            return [f["t"] for f in c.frames if f.get("t") in ("jobs_pause", "jobs_resume")]
        lease = S.get().lease(self.sid, "dry-run", kind="benchmark", label="benchmark run")
        d.tick()
        self.assertEqual(told(conn), ["jobs_pause"])
        e.disconnected(conn)
        lease.release()
        d.tick()
        self.assertFalse(e.paused)
        _e, conn2 = self.connect()
        d.tick()
        self.assertEqual(told(conn2), [])
        lease = S.get().lease(self.sid, "dry-run", kind="benchmark", label="benchmark run")
        self.addCleanup(lease.release)
        d.tick()
        self.assertEqual(told(conn2), ["jobs_pause"])
        e.disconnected(conn2)
        _e, conn3 = self.connect()
        self.assertEqual(told(conn3), ["jobs_pause"], "paused again while the lease lasts")

    def test_a_run_carried_across_a_restart_settles_its_credits_into_its_budget(self):
        """The budget holds were the old process's: a run its helper still plays when the server comes back settles its
        credits into its grant's budget all the same (P4.5.7)."""
        from citar.db.models import BudgetEntry
        from citar.jobs import remote
        from citar.pool import budgets
        self.budget(1000)
        e, conn = self.connect()
        self.submit("alice", duels("across-a-restart", 1))
        d = self.dispatcher([], remotes=self.registry)
        d.tick()
        (offer,) = self.offers(conn)
        e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        # the server restarts: a new process, with neither the old one's budget holds nor its executors
        d.stop()
        budgets.reset_keeper()
        self.registry = remote.Remotes()
        d2 = self.dispatcher([], remotes=self.registry)
        e2, conn2 = self.connect()
        self.assertEqual(d2.welcome_jobs(self.sid, [offer["run"]], []), ([offer["run"]], [], []))
        d2.tick()
        self.assertEqual(e2.holds, {})
        e2.frame(self.result(offer))
        d2.tick()
        self.led.flush()
        self.assertIn(offer["run"], [f["run"] for f in conn2.frames if f.get("t") == "job_result_ack"])
        with db.session() as s:
            (ev,) = s.scalars(select(ComputeEvent).where(ComputeEvent.request_id == offer["run"])).all()
            (entry,) = s.scalars(select(BudgetEntry)).all()
            self.assertAlmostEqual(entry.actual, ev.credits, places=6)
            self.assertEqual(entry.user_id, self.ids["alice"])

    def test_a_tick_with_2000_queued_jobs_and_ten_helpers_that_refuse_them(self):
        """P4.7.9's tick bound with the executors being helpers (the review's probe): ten trusted helper machines of
        bob's, each ready with a free thread, none of which may play alice's 2,000 queued jobs (no grant lends them to
        her); the host's one thread busy. Under 50 ms a tick, the median of 100 (three times that on CI)."""
        from tests.helperfake import StubConnection, hello, settings as site_settings
        cpu = {"enabled": True, "threads": 1, "cpu_percent": 50, "memory_mb": 1024, "work": ["lab_game"],
               "yield_to_llm": True}
        sids = []
        with db.session() as s:
            for k in range(10):
                row = Server(owner_id=self.ids["bob"], name=f"Bob's PC {k}", kind="owned", provider="lmstudio",
                             reach="worker", max_concurrent=1, group_id=self.group_id, trust_tier="trusted",
                             capabilities={"cpu": dict(cpu)})
                s.add(row)
                s.flush()
                sids.append(row.id)
        servers.invalidate()
        for sid in sids:
            site = site_settings(llm=False)
            site["cpu"] = dict(cpu)
            conn = StubConnection(sid, site=site)
            h = hello(hostname="bob-pc", llm=False)
            h["helper"]["jobs_build"] = engine_api.jobs_build()
            h["cpu"] = {"enabled": True, "ceiling": {"threads": 1, "cpu_percent": 50, "memory_mb": 1024,
                                                      "work": ["lab_game"]},
                        "limits": {"cpu": "hard", "memory": "hard"}, "verifiable": True, "bots": [],
                        "ruleset": "0" * 64}
            conn.hello, conn.jobs_build, conn.memory_per_run_mb = h, h["helper"]["jobs_build"], 500
            conn.acknowledge({"effective": {"llm": {"enabled": False, "slots": 0}, "cpu": dict(cpu)},
                              "capped": [], "limits": {"cpu": "hard", "memory": "hard"}, "memory_per_run_mb": 500})
            self.registry.get(sid, sid).connected(conn)
        self.submit("alice", duels("big", 2000))
        busy = Fake("sv_host", name="this server", threads=1)      # takes every job, its one thread busy
        d = self.dispatcher([busy], remotes=self.registry)
        with mock.patch.object(D, "WINDOW", 1000):
            d.tick()
        self.assertGreaterEqual(sum(1 for j in d.jobs.values() if j.state == "queued"), 1990)
        self.assertEqual(sum(1 for e in self.registry.all() if e.free > 0), 10, "ten helpers ready")
        times = []
        for _ in range(100):
            t = time.perf_counter()
            d.tick()
            times.append(time.perf_counter() - t)
        median = statistics.median(times)
        print(f"\ndispatcher tick with ten refusing helpers: median {median * 1000:.1f} ms, p95 "
              f"{sorted(times)[94] * 1000:.1f} ms over 100 ticks (2,000 jobs)")
        self.assertLess(median, 0.050 * (3 if ON_CI else 1))
