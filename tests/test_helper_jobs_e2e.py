"""The real CITAR helper playing the lab's CPU jobs for the hub's dispatcher (design/phase4-helper.md P4.3.6,
P4.5.5-P4.5.7; package 4-10's gates 2 and 3).

The binary is the one ``cargo xtask develop`` builds into ``CITAR_EXT_DIR`` (``tests/test_helper_e2e.py`` says more);
without it the module skips, except on CI. The hub is the real app under uvicorn in this process, and the dispatcher
is started here with no local executor, so every job goes to a helper. Each helper runs in a home of its own whose
``helper.json`` lends the machine's CPU as its owner would: two threads, 20% of the machine and 512 MB, the lab's and
the ranking's games (the site's settings ask more, and that ceiling wins). Bob owns the machines, in a group of his
whose grant lists ``lab`` for alice, a member an administrator gave ``run_lab``; an administrator trusted them, and
their results are not sampled here (``trusted_rate`` 0: verification is ``tests/test_verification_e2e.py``'s, and this
module's dispatcher has no verifier).

- **Eight games** (gate 2): alice's experiment of eight 30-round duels plays on bob's machine: eight lines with
  ``executor.machine`` that machine (``unverified``: accepted without a replica), eight events given by bob and used
  by alice; the machine settings dialog's read shows what is in effect, each limit hard on Windows (a Job Object);
  the job host's CPU stays under its cap plus slack, and every run's round advances at least every 30 s.
- **Yield** (gate 3): while a request streams at the scripted endpoint, the game playing holds (its round reported
  the same when held and when let go), and goes on within 2 s of the stream's end, with its paused time reported and
  the server's deadline pushed on by it; with ``yield_to_llm`` off a streamed request holds nothing, and a benchmark
  lease going live on the machine sends ``jobs_pause`` (the game holds) and its end ``jobs_resume``.
- **A stop** (Linux and macOS: a signal the helper sees): its owner stopping the helper gives its game back at once,
  failed ``aborted_locally``, queued again at no cost.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from sqlalchemy import select

from citar import db, items, lab, ledger, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, ComputeEvent, CpuJob, CpuJobRun, Server, ServerGroup, User, WorkerToken
from citar.jobs import dispatcher as D, remote
from citar.pool import scheduler as S
from tests import helperfake as H

ROOT = Path(__file__).resolve().parent.parent
_TMP = Path(tempfile.mkdtemp(prefix="citar_helper_jobs_e2e_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
CI = bool(os.environ.get("CI"))
#: CI's runners are slower and busier: the timing bounds are three times as loose there.
SLACK = 3.0 if CI else 1.0


def _qa4():
    spec = importlib.util.spec_from_file_location("qa_phase4_for_jobs_e2e", ROOT / "scripts" / "qa_phase4.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa4 = _qa4()
qa3 = qa4.q3
MODEL = qa3.FakeEndpoint.MODEL

#: What the machine's owner lends in its helper's own settings (gate 2's figures).
CEILING = {"enabled": True, "threads": 2, "cpu_percent": 20, "memory_mb": 512, "work": ["lab_game", "ranking_game"]}
#: What the site's settings ask: more than that, so the helper's ceiling is what plays.
SITE = {"enabled": True, "threads": 4, "cpu_percent": 50, "memory_mb": 1024, "work": ["lab_game", "ranking_game"],
        "yield_to_llm": True}
#: A game that lasts long enough to be held and let go: about 11 s of one core here.
LONG = {"size": "huge", "maps": ["pangaea"], "turns": 300,
        "seats": [{"label": f"s{i}", "bot": "basic"} for i in range(12)]}


#: The environment this module changes, as it was: given back at its end, so a module run after it in the same process
#: finds its own database again.
_SAVED: dict = {}


def setUpModule():
    H.require_helper_binary("the helper's CPU job tests")       # skips locally without one; fails on CI
    _SAVED.update({k: os.environ.get(k) for k in ("CITAR_DATA_DIR", "CITAR_DB_URL")})
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


def tearDownModule():
    db.dispose()
    for k, v in _SAVED.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    settings.reset()
    servers.invalidate()
    shutil.rmtree(_TMP, ignore_errors=True)


def _until(check, limit: float = 10.0, step: float = 0.02):
    return H.until(check, limit, step)


def machine_settings_client(s, sid: str, viewer: str) -> dict:
    """The machine settings dialog's read of ``sid`` as ``viewer`` sees it."""
    from citar import machine_settings
    return machine_settings.client(s, s.get(Server, sid), s.get(User, viewer))


class Runs:
    """The dispatcher's runs on one machine, sampled every 20 ms on a thread of its own: (``time.time()``, run,
    state, round, paused, paused_s) each, so a test can say when a game held, at which round, and when it went on."""

    def __init__(self, d: D.Dispatcher, machine: str):
        self.d, self.machine = d, machine
        self.samples: list = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, name="runs-sampler", daemon=True)
        self._thread.start()

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                runs = list(self.d.runs.values())
            except RuntimeError:            # the dispatcher's thread changed the dict meanwhile
                runs = []
            now = time.time()
            for r in runs:
                if r.machine == self.machine:
                    self.samples.append((now, r.id, r.state, r.round, r.paused, r.paused_s))
            self._stop.wait(0.02)

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(5)

    def of(self, run: str) -> list:
        return [s for s in self.samples if s[1] == run]

    def running(self) -> list:
        """The runs seen running with a reported round."""
        return sorted({s[1] for s in self.samples if s[2] == "running" and s[3]})

    def gaps(self) -> dict:
        """Each run's longest wait between the reports that moved its round (its first sample to its last counting
        as moves): what "its round advances at least every 30 s" bounds."""
        out = {}
        for run in {s[1] for s in self.samples}:
            rows = self.of(run)
            moves = [rows[0][0]] + [b[0] for a, b in zip(rows, rows[1:]) if b[3] != a[3]] + [rows[-1][0]]
            out[run] = max(b - a for a, b in zip(moves, moves[1:])) if len(moves) > 1 else 0.0
        return out


class Lending(unittest.TestCase):
    """Bob's machines lending their CPU through the real helper, alice's experiments playing on them."""

    @classmethod
    def setUpClass(cls):
        from citar import pool
        cls.dir = Path(tempfile.mkdtemp(prefix="lab-", dir=_TMP))
        labdir = cls.dir / "saves" / "lab"
        cls.patches = [mock.patch.object(lab, name, labdir / sub if sub else labdir)
                       for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"),
                                         ("REFERENCE", "reference"))]
        for p in cls.patches:
            p.start()
        lab._dirs()
        cls.led = ledger.reset(background=False)
        remote.forget_owner_tiers()
        cls.ids = {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@jobs.cc", password="a long enough phrase",
                                            role=role, status="active", email_verified=True)
                cls.ids[handle] = user.id
            s.get(User, cls.ids["alice"]).caps = {"run_lab": True}      # an administrator gave her run_lab
            group = ServerGroup(owner_id=cls.ids["bob"], name="Bob's machines", slug="bobs-machines")
            s.add(group)
            s.flush()
            cls.group_id = group.id
            pool.create_grant(s, s.get(User, cls.ids["bob"]), group, subject_type="user", subject_handle="alice",
                              purposes=["lab"])
            from citar.jobs import trust
            trust.save_settings(s, {"trusted_rate": 0.0})
        cls.server = H.HubServer()
        cls.dispatcher = D.start(D.Dispatcher(local=False))

    @classmethod
    def tearDownClass(cls):
        D.stop()
        cls.server.stop()
        S.reset()
        for p in cls.patches:
            p.stop()
        ledger.reset(background=False)
        remote.forget_owner_tiers()

    # -------------------------------------------------------------- the pieces
    def machine(self, name: str, **cpu) -> tuple:
        """One of bob's machines in his group, trusted by an administrator, its CPU settings ``SITE`` with ``cpu``;
        (its id, a worker token)."""
        with db.session() as s:
            row = Server(owner_id=self.ids["bob"], name=name, kind="owned", provider="openai_compatible",
                         reach="worker", max_concurrent=1, config={"models": [{"key": MODEL}]}, group_id=self.group_id,
                         trust_tier="trusted", capabilities={"cpu": {**SITE, **cpu}})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="jobs e2e"))
            sid = row.id
        servers.invalidate()
        return sid, raw

    def helper(self, sid: str, token: str, endpoint) -> H.RealHelper:
        """The real helper for ``sid``, its owner's ``CEILING`` in its home's ``helper.json``; once its executor is
        ready (its job host's self-test passed)."""
        home = Path(tempfile.mkdtemp(prefix="helper-", dir=_TMP))
        (home / "helper.json").write_text(json.dumps({"cpu": CEILING}), encoding="utf-8")
        helper = H.RealHelper(self.server.base, token, endpoint.url, name=f"jobs {sid}", home=home)
        self.addCleanup(helper.stop)
        ready = _until(lambda: (lambda e: e is not None and e.state == "ready")(remote.registry().find(sid)), 90, 0.05)
        e = remote.registry().find(sid)
        self.assertTrue(ready, f"its executor is {e and e.state}: {e and e.reason}\n{helper.text()[-3000:]}")
        return helper

    def endpoint(self, **slow) -> "qa3.FakeEndpoint":
        endpoint = qa3.FakeEndpoint(delay=0.05)
        endpoint.slow = dict(slow)
        self.addCleanup(endpoint.close)
        return endpoint

    def submit(self, who: str, spec: dict) -> str:
        """Queue an experiment as ``who``'s, its item registered as the A/B route registers it."""
        exp = lab.submit(spec)
        with db.session() as s:
            items.register(s, "lab_experiment", exp["name"], s.get(User, self.ids[who]), exp["name"],
                           path=lab.QUEUE / f"{exp['name']}.json", collection=False)
        D.poke()
        return exp["name"]

    def request(self, sid: str, text: str) -> None:
        """One streamed completion through the machine's helper, as a pooled prompt asks it (the endpoint's entry
        for it records when each event went out, its last the ``[DONE]``)."""
        from citar.agents.providers import make_conversation
        ticket = S.get().ticket(sid, MODEL, cls="test", label=text)
        try:
            conv = make_conversation({"provider": "worker", "server_id": sid, "model": MODEL, "timeout": 60},
                                     "You are a test.", [], binding=ticket)
            conv.add_user_text(text)
            conv.step()
        finally:
            ticket.close()

    def long_game(self, sid: str, name: str) -> tuple:
        """Alice's one long game, playing on ``sid``: (the sampler, its run), once its first round was reported."""
        runs = Runs(self.dispatcher, sid)
        self.addCleanup(runs.stop)
        self.submit("alice", {"name": name, "games": 1, "seed": 9400, **LONG})
        self.assertTrue(_until(lambda: runs.running(), 60), "the game did not start on the machine")
        return runs, runs.running()[0]

    def finished(self, run: str, limit: float = 180.0):
        def done():
            with db.session() as s:
                row = s.get(CpuJobRun, run)
                return row if row is not None and row.state in ("done", "failed", "expired", "aborted") else None
        row = _until(done, limit * SLACK, 0.1)
        self.assertIsNotNone(row, "the run did not end")
        return row

    # -------------------------------------------------------------- gate 2
    def test_eight_games_of_a_members_experiment_on_a_lent_machine(self):
        sid, token = self.machine("lender")
        helper = self.helper(sid, token, self.endpoint())
        watch = qa4.CpuWatch(helper.proc.pid).start()
        runs = Runs(self.dispatcher, sid)
        self.addCleanup(runs.stop)
        name = self.submit("alice", {"name": "eight-on-bobs", "games": 8, "seed": 9300, "size": "duel",
                                     "maps": ["pangaea"], "turns": 30,
                                     "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}]})
        self.assertTrue(_until(lambda: len(lab.load_results(name)) >= 8, 180 * SLACK, 0.1),
                        f"{len(lab.load_results(name))} lines\n{helper.text()[-3000:]}")
        cpu = watch.stop()
        runs.stop()
        lines = lab.load_results(name)
        self.assertEqual(sorted(x["i"] for x in lines), list(range(8)))
        self.assertEqual({(x["executor"]["machine"], x["executor"]["verification"]) for x in lines},
                         {(sid, "unverified")})
        played = {x["executor"]["run"] for x in lines}
        with db.session() as s:
            rows = s.scalars(select(CpuJobRun).where(CpuJobRun.id.in_(played))).all()
            self.assertEqual({(r.executor, r.state, r.machine_id) for r in rows}, {("remote", "done", sid)})
        self.led.flush()
        events = _until(lambda: (lambda evs: evs if len(evs) == 8 else None)(self._events(sid)), 30)
        self.assertTrue(events, f"{len(self._events(sid))} events")
        self.assertEqual({(e.provider_id, e.consumer_id, e.verification) for e in events},
                         {(self.ids["bob"], self.ids["alice"], "unverified")})
        self.assertEqual({e.request_id for e in events}, played)
        # the machine settings dialog's read: what is in effect, the helper's ceiling having capped the site's figures
        with db.session() as s:
            dialog = machine_settings_client(s, sid, self.ids["bob"])
        lent = dialog["cpu_helper"]
        self.assertTrue(lent["lent"] and lent["verifiable"] and lent["plays_jobs"], lent)
        self.assertEqual({k: lent["effective"][k] for k in ("threads", "cpu_percent", "memory_mb")},
                         {"threads": 2, "cpu_percent": 20, "memory_mb": 512})
        self.assertTrue({"cpu.threads", "cpu.cpu_percent", "cpu.memory_mb"} <= set(lent["capped"]), lent)
        if sys.platform == "win32":
            self.assertEqual(lent["limits"], {"cpu": "hard", "memory": "hard"}, "a Job Object holds both")
        self.assertEqual(dialog["lab_grants"], [{"subject": "alice", "group": "Bob's machines"}])
        # the job host's CPU: under its cap plus slack, where the cap is held hard and the CPU can be read
        if lent["limits"].get("cpu") == "hard" and cpu["peak_pct"] is not None:
            self.assertLessEqual(cpu["peak_pct"], 20 + 5, cpu)
        # each run's round moved at least every 30 s
        gaps = runs.gaps()
        self.assertTrue(gaps and max(gaps.values()) <= 30.0, gaps)
        print(f"\n[jobs e2e] eight duels: CPU {json.dumps(cpu)}; longest wait between rounds "
              f"{max(gaps.values()):.2f} s", file=sys.stderr)

    def test_the_job_host_holds_the_share_in_effect(self):
        """Gate 2's CPU bound where it binds: the eight duels last a fraction of a second, and 20% of this machine is
        more than two threads use where it has ten CPUs or more, so the share is measured here at 5% of the machine
        (the site's figure, under the owner's 20%) with two long games playing: the job host's CPU stays under it plus
        slack where it is held hard. The experiment is then deleted, and its runs end aborted."""
        sid, token = self.machine("held", cpu_percent=5)
        helper = self.helper(sid, token, self.endpoint())
        e = remote.registry().find(sid)
        self.assertEqual(e.cpu_percent, 5)
        runs = Runs(self.dispatcher, sid)
        self.addCleanup(runs.stop)
        name = self.submit("alice", {"name": "held-to-its-share", "games": 2, "seed": 9500, **LONG})
        self.assertTrue(_until(lambda: len(runs.running()) == 2, 60), "both games started")
        watch = qa4.CpuWatch(helper.proc.pid).start()
        time.sleep(8.0)
        cpu = watch.stop()
        with db.session() as s:
            limits = (machine_settings_client(s, sid, self.ids["bob"])["cpu_helper"] or {}).get("limits") or {}
        if limits.get("cpu") == "hard" and cpu["peak_pct"] is not None:
            self.assertLessEqual(cpu["peak_pct"], 5 + 2, cpu)
            self.assertGreater(cpu["mean_pct"], 2.5, f"it used its share: {cpu}")
        try:
            (lab.QUEUE / f"{name}.json").unlink()
        except FileNotFoundError:
            if limits.get("cpu") == "hard":
                raise
            # A share held softly (macOS: nice and a thread cap) let both games end within the 8 s on CI's runner
            # (test.yml run 38986642828: the experiment done, its file gone), so nothing is left to abort. Where it is
            # held hard the games are still running and the deletion must find them.
            print(f"\n[jobs e2e] a share of 5% held {limits}: both games ended within the 8 s; CPU {json.dumps(cpu)}",
                  file=sys.stderr)
            return
        D.poke()
        # each run seen playing ends aborted; where the share is held softly one may have ended (done) before the
        # deletion reached it (CI, test.yml run 39078917427, macos), but not both, as the file was still queued
        ends = [self.finished(run, 30).state for run in runs.running()]
        if limits.get("cpu") == "hard":
            self.assertEqual(set(ends), {"aborted"}, ends)
        else:
            self.assertIn("aborted", ends, ends)
            self.assertLessEqual(set(ends), {"aborted", "done"}, ends)
        print(f"\n[jobs e2e] a share of 5% held {limits}: CPU {json.dumps(cpu)}", file=sys.stderr)

    @unittest.skipIf(sys.platform == "win32", "a test can end a Windows child only by TerminateProcess, which is no stop "
                     "the helper sees (Ctrl-C and its service's stop are; the dispatcher's half is test_dispatcher's)")
    def test_a_helper_stopped_by_its_owner_gives_its_game_back_at_once(self):
        """Its owner stops the helper (SIGTERM, as Ctrl-C or the service manager asks) with a game playing: the
        helper fails it ``aborted_locally`` before it goes, and the server queues the game again at once, neither an
        attempt nor an abandon, rather than waiting five minutes for its silence."""
        sid, token = self.machine("stopped")
        helper = self.helper(sid, token, self.endpoint())
        name = "given-back-by-a-stop"
        runs, run = self.long_game(sid, name)

        def gone():
            try:
                (lab.QUEUE / f"{name}.json").unlink()
            except OSError:
                pass
            D.poke()
        self.addCleanup(gone)           # its game must not wait for the next test's machine
        t0 = time.monotonic()
        helper.proc.terminate()
        self.assertIsNotNone(helper.exited(15), "it stopped")
        row = self.finished(run, 15)
        took = time.monotonic() - t0
        self.assertEqual((row.state, (row.error or {}).get("code")), ("failed", "aborted_locally"),
                         helper.text()[-2000:])
        self.assertLess(took, 10.0 * SLACK)
        with db.session() as s:
            job = s.get(CpuJob, row.job_id)
            self.assertEqual((job.state, job.attempts or 0), ("queued", 0))
            self.assertEqual((s.get(Server, sid).trust or {}).get("abandons", 0), 0)
        print(f"\n[jobs e2e] stop: its game failed aborted_locally {took:.2f} s after the signal", file=sys.stderr)

    def _events(self, sid: str) -> list:
        self.led.flush()
        with db.session() as s:
            return list(s.scalars(select(ComputeEvent).where(ComputeEvent.machine_id == sid)))

    # -------------------------------------------------------------- gate 3
    def test_a_streamed_request_holds_its_games_and_its_end_lets_them_go(self):
        sid, token = self.machine("yielding")
        endpoint = self.endpoint(**{"Reply with exactly: HOLD": 3.0})
        self.helper(sid, token, endpoint)
        runs, run = self.long_game(sid, "held-by-a-request")
        t_asked = time.time()
        self.request(sid, "Reply with exactly: HOLD")
        entry = next(r for r in endpoint.streamed() if r.get("delay") == 3.0)
        done = entry["events"][-1]
        held = [x for x in runs.of(run) if x[4] and x[0] >= t_asked]
        self.assertTrue(held, "the game was never held while the request streamed")
        self.assertLess(held[0][0], done, "it held before the stream ended")
        went_on = _until(lambda: next((x for x in runs.of(run) if x[0] > done and not x[4]), None), 5 * SLACK)
        self.assertIsNotNone(went_on, "it never went on")
        self.assertLess(went_on[0] - done, 2.0 * SLACK, "it went on within 2 s of the stream's end")
        self.assertEqual(went_on[3], held[-1][3], "held at one round: the round it went on from")
        self.assertGreater(went_on[5], 0.0, "its paused time reported")
        view = next((r for r in self.dispatcher.published.get("running") or [] if r["run"] == run), None)
        if view is not None:
            self.assertGreater(view["extended_s"], 0.0, "the server's deadline pushed on by the pause")
        row = self.finished(run)
        self.assertEqual(row.state, "done", row.error)
        self.assertGreater(row.paused_s, 1.0, "the result's paused time")
        moved = [x for x in runs.of(run) if x[0] > went_on[0] and (x[3] or 0) > went_on[3]]
        self.assertTrue(moved or row.state == "done", "its round moved again")
        print(f"\n[jobs e2e] yield: held {held[0][0] - entry['events'][0]:+.2f} s from the first event at round "
              f"{held[-1][3]}, went on {went_on[0] - done:.3f} s after [DONE], paused {row.paused_s:.2f} s",
              file=sys.stderr)

    def test_with_yield_off_a_request_holds_nothing_and_a_benchmark_pauses_them(self):
        sid, token = self.machine("benched", yield_to_llm=False)
        endpoint = self.endpoint(**{"Reply with exactly: NO HOLD": 2.0})
        self.helper(sid, token, endpoint)
        runs, run = self.long_game(sid, "held-by-a-benchmark")
        t_asked = time.time()
        self.request(sid, "Reply with exactly: NO HOLD")
        self.assertFalse([x for x in runs.of(run) if x[4] and x[0] >= t_asked], "yield_to_llm is off")
        executor = remote.registry().find(sid)
        lease = S.get().lease(sid, MODEL, kind="benchmark", label="benchmark run")
        t_lease = time.time()
        try:
            held = _until(lambda: next((x for x in runs.of(run) if x[0] >= t_lease and x[4]), None), 5 * SLACK)
            self.assertIsNotNone(held, "the benchmark's lease did not hold the game")
            self.assertTrue(executor.paused, "jobs_pause sent")
            self.assertLess(held[0] - t_lease, 2.0 * SLACK)
            time.sleep(2.0)
            still = [x for x in runs.of(run) if x[0] >= held[0]]
            self.assertTrue(all(x[4] and x[3] == held[3] for x in still), "held at one round while it lasted")
        finally:
            lease.release()
        t_end = time.time()
        went_on = _until(lambda: next((x for x in runs.of(run) if x[0] >= t_end and not x[4]), None), 5 * SLACK)
        self.assertIsNotNone(went_on, "its end did not let the game go on")
        self.assertFalse(executor.paused, "jobs_resume sent")
        self.assertLess(went_on[0] - t_end, 2.0 * SLACK)
        row = self.finished(run)
        self.assertEqual(row.state, "done", row.error)
        self.assertGreater(row.paused_s, 1.5, "the result's paused time")
        print(f"\n[jobs e2e] benchmark: held {held[0] - t_lease:.3f} s after the lease at round {held[3]}, went on "
              f"{went_on[0] - t_end:.3f} s after its end, paused {row.paused_s:.2f} s", file=sys.stderr)


if __name__ == "__main__":
    unittest.main()
