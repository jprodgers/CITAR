"""The real CITAR helper built to cheat on purpose (``citar-helper-faults``: the ``test-faults`` feature,
``--test-fault MODE``, design/phase4-helper.md P4.4.6) against the real hub and this server's real job host as its
verifier (package 4-11's gates 2 and 3, end to end).

The binary is the one ``cargo xtask develop`` builds beside the helper (``CITAR_EXT_DIR``; ``CITAR_HELPER_FAULTS_BIN``
on CI); without it the module skips, except on CI. The hub is the real app under uvicorn in this process; the job
dispatcher is this test's, ticked by hand, with this server's job host (the real one, ``python -m citar.jobs host``)
as its local executor, set to play verification runs only so that every game goes to the helper and the order of its
jobs is the test's. Each helper runs in a home of its own (``--home``) and lends one thread, 20% of the machine and 512
MB in its own ``helper.json``.

- **Caught while new** (gate 2): ``result:1`` is quarantined at its first job with "same chain, different result:
  tampering or an incompatible build", its result never in the results file, its event rejected with no credits, its
  owner quarantined and a second machine of theirs refused CPU work; ``digest:12``'s evidence names round 12 and
  quarantines the machine alone; ``sig``'s is a bad signature; ``abandon:1``'s job is queued again and counted as an
  abandon.
- **Caught by sampling** (gate 3): ``result-at:6,9`` reaches ``provisional`` after five matches; job 6 (altered) is
  accepted unverified, an administrator's ``replicate 1`` makes job 7 a verified match, job 8 is accepted unverified,
  and with the rate at 1 job 9 is replicated and mismatches: the machine is quarantined, and jobs 6 and 8 leave the
  results file, their events rejected.
- **Credits** (package 4-12's gate 1, P4.5.7): ``cpu:3`` (300-round games, the helper at up to two cores' worth) is
  credited at most 1.5 x its replica's CPU, flags ``overreported_cpu`` and stays ``provisional`` past the matches that
  would make it ``trusted``; ``hold:2`` is credited at most 1.5 x its replica's CPU, flags ``capped_cpu`` only and is
  promoted; an honest helper capped to a quarter of a core is credited its own CPU, flags nothing that blocks it, and is
  promoted.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import json
import os
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from sqlalchemy import select

from citar import db, items, lab, ledger, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, ComputeEvent, CpuJob, CpuJobRun, Server, ServerGroup, User, WorkerToken
from citar.jobs import dispatcher as D, host as JH, local as L, remote, trust as T
from citar.pool import scheduler as S
from tests import helperfake as H

_TMP = Path(tempfile.mkdtemp(prefix="citar_verification_e2e_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
CI = bool(os.environ.get("CI"))
SLACK = 3.0 if CI else 1.0
#: What the machine's owner lends in its helper's own settings.
CEILING = {"enabled": True, "threads": 1, "cpu_percent": 20, "memory_mb": 512, "work": ["lab_game"]}
SITE = {"enabled": True, "threads": 1, "cpu_percent": 20, "memory_mb": 512, "work": ["lab_game"],
        "yield_to_llm": True}

_SAVED: dict = {}


def setUpModule():
    H.require_faults_binary("the verification tests with the real helper")
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


class VerifyingHost(L.LocalExecutor):
    """This server's real job host, playing verification runs only (its primaries would take the helper's games)."""

    def refuses(self, job):
        if job.state != "settling":
            return "this test's host plays verification runs only"
        return super().refuses(job)


def two_cores() -> int:
    return max(1, min(100, 200 // JH.machine_cpus()))


class Faulty(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = Path(tempfile.mkdtemp(prefix="lab-", dir=_TMP))
        labdir = cls.dir / "saves" / "lab"
        cls.patches = [mock.patch.object(lab, name, labdir / sub if sub else labdir)
                       for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"),
                                         ("REFERENCE", "reference"))]
        for p in cls.patches:
            p.start()
        lab._dirs()
        cls.led = ledger.reset(background=False)
        cls.ids = {}
        with db.session() as s:
            for handle, role in (("zara", "admin"),):
                cls.ids[handle] = accounts.create_user(s, handle=handle, email=f"{handle}@verify.cc",
                                                       password="a long enough phrase", role=role, status="active",
                                                       email_verified=True).id
        cls.server = H.HubServer()

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        S.reset()
        for p in cls.patches:
            p.stop()
        ledger.reset(background=False)
        remote.forget_owner_tiers()

    def setUp(self):
        T.forget()
        remote.forget_owner_tiers()
        remote.registry().clear()
        self.offset = 0.0
        host = VerifyingHost(
            host_factory=lambda threads, cpu_percent, memory_mb: JH.JobHost(threads, cpu_percent, memory_mb,
                                                                            play_unverifiable=True),
            settings=lambda: ("sv_host", "this server", {"cpu": {"enabled": True, "threads": 1,
                                                                 "cpu_percent": two_cores(), "memory_mb": 400,
                                                                 "work": ["lab_game"]}}))
        self.d = D.Dispatcher([host], clock=lambda: time.monotonic() + self.offset, remotes=remote.registry())
        old = D.install(self.d)
        self.addCleanup(D.install, old)
        self.addCleanup(self.d.stop)
        self.owner = f"owner{len(self.ids)}"
        with db.session() as s:
            self.ids[self.owner] = accounts.create_user(s, handle=self.owner, email=f"{self.owner}@verify.cc",
                                                        password="a long enough phrase", role="user",
                                                        status="active", email_verified=True).id
        with db.session() as s:
            T.save_settings(s, dict(T.DEFAULTS))       # each test from the defaults

    def settings(self, **values) -> None:
        with db.session() as s:
            from citar.db.models import Setting
            row = s.get(Setting, T.SETTINGS_KEY)
            if row is None:
                s.add(Setting(key=T.SETTINGS_KEY, value=T.clean(values)))
            else:
                row.value = {**(row.value or {}), **T.clean(values)}
        T.forget()

    # -------------------------------------------------------------- the pieces
    def machine(self, name: str, tier: str = "new") -> tuple:
        """A machine of this test's owner in their group, at ``tier``: (its id, a worker token)."""
        with db.session() as s:
            group = s.scalar(select(ServerGroup).where(ServerGroup.owner_id == self.ids[self.owner]))
            if group is None:
                group = ServerGroup(owner_id=self.ids[self.owner], name=f"{self.owner}'s", slug=f"{self.owner}-m")
                s.add(group)
                s.flush()
            row = Server(owner_id=self.ids[self.owner], name=name, kind="owned", provider="openai_compatible",
                         reach="worker", max_concurrent=1, config={"models": []}, group_id=group.id,
                         trust_tier=tier, capabilities={"cpu": dict(SITE)})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="verify e2e"))
            sid = row.id
        servers.invalidate()
        return sid, raw

    def helper(self, sid: str, token: str, *modes: str, ceiling: dict | None = None) -> H.RealHelper:
        """The real helper for ``sid`` (the cheating build, playing ``modes``; the honest one without), once its
        executor is ready; ``ceiling`` what its owner lends in its own settings (``CEILING`` by default)."""
        home = Path(tempfile.mkdtemp(prefix="helper-", dir=_TMP))
        (home / "helper.json").write_text(json.dumps({"cpu": ceiling or CEILING}), encoding="utf-8")
        extra = tuple(x for m in modes for x in ("--test-fault", m))
        binary = H.faults_binary() if modes else None
        helper = H.RealHelper(self.server.base, token, "http://127.0.0.1:9/v1", name=f"verify {sid}", home=home,
                              extra=("--no-llm", *extra), binary=binary)
        self.addCleanup(helper.stop)

        def ready():
            self.d.tick()
            e = remote.registry().find(sid)
            return e is not None and e.state == "ready"
        self.assertTrue(H.until(ready, 90 * SLACK, 0.1), helper.text()[-3000:])
        return helper

    def submit(self, name: str, games: int, turns: int = 30) -> str:
        exp = lab.submit({"name": name, "games": games, "seed": 9500, "size": "duel", "maps": ["pangaea"],
                          "turns": turns, "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}]})
        with db.session() as s:
            items.register(s, "lab_experiment", exp["name"], s.get(User, self.ids[self.owner]), exp["name"],
                           path=lab.QUEUE / f"{exp['name']}.json", collection=False)
        return exp["name"]

    def tick_until(self, cond, limit: float = 120.0, step: float = 0.05):
        deadline = time.perf_counter() + limit * SLACK
        while time.perf_counter() < deadline:
            self.d.tick()
            got = cond()
            if got:
                return got
            time.sleep(step)
        self.fail(f"not within {limit} s; the lab log:\n{lab._tail(lab.LAB / 'lab.log', 3000)}")

    def row(self, sid: str) -> Server:
        with db.session() as s:
            row = s.get(Server, sid)
            s.expunge(row)
            return row

    def primaries(self, sid: str) -> list:
        """The machine's primary runs that gave a result, in the order they were offered."""
        with db.session() as s:
            out = list(s.scalars(select(CpuJobRun).where(CpuJobRun.machine_id == sid, CpuJobRun.role == "primary",
                                                         CpuJobRun.state == "done")
                                 .order_by(CpuJobRun.offered_at)))
            for r in out:
                s.expunge(r)
            return out

    def event(self, run: str):
        self.led.drain()
        with db.session() as s:
            ev = s.scalar(select(ComputeEvent).where(ComputeEvent.request_id == run))
            if ev is not None:
                s.expunge(ev)
            return ev

    def lined(self, name: str) -> dict:
        return {(x.get("executor") or {}).get("run"): x for x in lab.load_results(name)}

    # -------------------------------------------------------------- gate 2
    def test_an_altered_result_is_caught_at_its_first_job(self):
        sid, token = self.machine("cheat e2e")
        helper = self.helper(sid, token, "result:1")
        name = self.submit("e2e-cheat", 3)
        self.tick_until(lambda: self.row(sid).trust_tier == "quarantined")
        (first,) = self.primaries(sid)
        ev = self.row(sid).trust["last_mismatch"]
        self.assertEqual((ev["verdict"], ev["words"], ev["run"]),
                         ("result", "same chain, different result: tampering or an incompatible build", first.id))
        self.assertNotIn(first.id, self.lined(name), "its result never reaches the results file")
        event = self.event(first.id)
        self.assertEqual((event.verification, event.credits), ("rejected", 0.0))
        with db.session() as s:
            self.assertEqual(s.get(User, self.ids[self.owner]).trust_tier, "quarantined")
        self.assertIn("TEST FAULTS", helper.text())
        # a second machine of that owner (an honest helper) is refused CPU work
        sid2, token2 = self.machine("cheat's other e2e")
        self.helper(sid2, token2)
        e2 = remote.registry().find(sid2)
        waiting = [j for j in self.d.jobs.values() if j.state == "queued"]
        self.assertTrue(waiting)
        self.assertEqual({e2.refuses(j) for j in waiting}, {"its owner is quarantined"})

    def test_a_changed_digest_names_round_12_and_quarantines_the_machine_alone(self):
        sid, token = self.machine("broken e2e")
        self.helper(sid, token, "digest:12")
        self.submit("e2e-broken", 2)
        self.tick_until(lambda: self.row(sid).trust_tier == "quarantined")
        ev = self.row(sid).trust["last_mismatch"]
        self.assertEqual((ev["verdict"], ev["round"]), ("chain", 12))
        with db.session() as s:
            self.assertEqual(s.get(User, self.ids[self.owner]).trust_tier, "new")

    def test_another_keys_signature_is_a_bad_signature(self):
        sid, token = self.machine("forger e2e")
        self.helper(sid, token, "sig")
        self.submit("e2e-forged", 2)
        self.tick_until(lambda: self.row(sid).trust_tier == "quarantined")
        ev = self.row(sid).trust["last_mismatch"]
        self.assertEqual(ev["verdict"], "signature")
        self.assertIn("a bad signature", ev["words"])

    def test_an_abandoned_job_is_queued_again_and_counted(self):
        sid, token = self.machine("quitter e2e")
        self.helper(sid, token, "abandon:1")
        self.submit("e2e-quit", 1)

        def running():
            with db.session() as s:
                return s.scalar(select(CpuJobRun).where(CpuJobRun.machine_id == sid,
                                                        CpuJobRun.state == "running"))
        run = self.tick_until(running)
        time.sleep(1.0)                     # nothing more of it comes
        self.d.tick()
        self.offset += D.SILENT_SECONDS + 1
        self.d.tick()
        with db.session() as s:
            row = s.get(CpuJobRun, run.id)
            self.assertEqual((row.state, row.error["message"]), ("expired", "no progress for 5 minutes"))
            self.assertEqual(s.get(CpuJob, row.job_id).attempts, 1)
        self.assertEqual(self.row(sid).trust.get("abandons"), 1)

    # -------------------------------------------------------------- gate 3
    def test_a_selective_cheater_is_caught_by_sampling_and_everything_unverified_withdrawn(self):
        self.settings(provisional_rate=0.0, new_streak=5)
        sid, token = self.machine("selective e2e")
        self.helper(sid, token, "result-at:6,9")
        name = self.submit("e2e-selective", 12)
        self.tick_until(lambda: self.row(sid).trust_tier == "provisional")
        self.assertEqual(self.row(sid).trust["verified"], 5)
        self.tick_until(lambda: len(self.primaries(sid)) >= 6 and self.primaries(sid)[5].id in self.lined(name))
        six = self.primaries(sid)[5].id
        self.assertEqual(self.lined(name)[six]["executor"]["verification"], "unverified")
        D.replicate_next(sid, 1, actor_id=self.ids["zara"])
        self.tick_until(lambda: len(self.primaries(sid)) >= 7 and self.primaries(sid)[6].id in self.lined(name))
        seven = self.primaries(sid)[6].id
        self.assertEqual(self.lined(name)[seven]["executor"]["verification"], "verified")
        self.tick_until(lambda: len(self.primaries(sid)) >= 8 and self.primaries(sid)[7].id in self.lined(name))
        eight = self.primaries(sid)[7].id
        self.assertEqual(self.lined(name)[eight]["executor"]["verification"], "unverified")
        self.settings(provisional_rate=1.0)
        self.tick_until(lambda: self.row(sid).trust_tier == "quarantined")
        ev = self.row(sid).trust["last_mismatch"]
        nine = self.primaries(sid)[8].id
        self.assertEqual((ev["verdict"], ev["run"]), ("result", nine))
        withdrawn = {w["run"] for w in ev["withdrawn_jobs"]}
        self.assertTrue({six, eight} <= withdrawn, withdrawn)
        self.assertNotIn(seven, withdrawn)
        lined = self.lined(name)
        self.assertFalse({six, eight, nine} & set(lined))
        self.assertIn(seven, lined)
        for r in (six, eight):
            self.assertEqual(self.event(r).verification, "rejected")


    # -------------------------------------------------------------- package 4-12's gate 1: credits
    def lending(self, sid: str, cpu_percent: int) -> None:
        """The site's figures for ``sid``: one thread at ``cpu_percent`` of the machine."""
        with db.session() as s:
            s.get(Server, sid).capabilities = {"cpu": {**SITE, "cpu_percent": cpu_percent}}
        servers.invalidate()

    def settled(self, sid: str, n: int) -> list:
        """The machine's first ``n`` primary runs' events once each is settled (``verified``), or None."""
        runs = self.primaries(sid)
        if len(runs) < n:
            return None
        events = [self.event(r.id) for r in runs[:n]]
        return events if all(e is not None and e.verification == "verified" for e in events) else None

    def test_cpu_reported_three_times_over_is_capped_flagged_and_blocks_promotion(self):
        """``cpu:3`` on 300-round duels (about a second of CPU each, at up to two cores' worth, so the server observes
        about what it spent): each is credited at most 1.5 x its replica's CPU and over-reported; its matches make it
        provisional, never trusted."""
        self.settings(new_streak=1, promote_verified=2, provisional_rate=1.0)
        sid, token = self.machine("over e2e")
        self.lending(sid, two_cores())
        self.helper(sid, token, "cpu:3", ceiling={**CEILING, "cpu_percent": two_cores()})
        self.submit("e2e-over", 4, turns=300)
        events = self.tick_until(lambda: self.settled(sid, 3), limit=180)
        row = self.row(sid)
        self.assertEqual(row.trust_tier, "provisional", "three verified matches, promote_verified 2: blocked")
        self.assertGreaterEqual(int(row.trust["verified"]), 3)
        self.assertGreaterEqual(row.flags["overreported_cpu"]["count"], 1)
        replicas = self._replica_cpu(sid)
        over = [e for e in events if e.flags.get("overreported_cpu")]
        self.assertTrue(over, [(e.reported_s, e.observed_s) for e in events])
        for e in events:
            ref = replicas[e.request_id]
            self.assertLessEqual(e.cpu_core_s, 1.5 * ref + 1e-6, (e.cpu_core_s, ref))
            self.assertLessEqual(e.cpu_core_s, e.observed_s * 1.05 + 1 + 1e-6)
            self.assertAlmostEqual(e.flags.get("reference_s"), ref, places=5)

    def test_holding_results_is_capped_at_the_reference_and_still_promoted(self):
        """``hold:2``: each result held two seconds, the held time reported as CPU (the server observes the hold):
        credited at most 1.5 x its replica's CPU, ``capped_cpu`` only, and promoted to trusted."""
        self.settings(new_streak=1, promote_verified=2, provisional_rate=1.0)
        sid, token = self.machine("holder e2e")
        self.helper(sid, token, "hold:2")
        self.submit("e2e-hold", 3)
        self.tick_until(lambda: self.row(sid).trust_tier == "trusted", limit=180)
        events = self.tick_until(lambda: self.settled(sid, 2))
        row = self.row(sid)
        self.assertNotIn("overreported_cpu", row.flags or {})
        self.assertGreaterEqual(row.flags["capped_cpu"]["count"], 2)
        replicas = self._replica_cpu(sid)
        for e in events:
            self.assertGreaterEqual(e.reported_s, 2.0)
            self.assertTrue(e.flags.get("capped_cpu"))
            self.assertNotIn("overreported_cpu", e.flags)
            self.assertAlmostEqual(e.cpu_core_s, 1.5 * replicas[e.request_id], places=5)

    def test_an_honest_helper_capped_to_a_quarter_core_is_promoted(self):
        """An honest helper held to a quarter of a core by its job host's hard limit: its 300-round games (about a
        second of CPU) take about four times as long as their CPU where the limit is hard (Windows' Job Object always;
        Linux's systemd scope where it has one; a soft limit, macOS's, does not hold the share and is only reported),
        its CPU is its own (the limit adds waiting, not CPU time), nothing blocks it, and it is promoted. 4-12's
        review: the test once passed the same way had the job host ignored the share."""
        import sys
        from citar import machine_settings
        self.settings(new_streak=1, promote_verified=2, provisional_rate=1.0)
        sid, token = self.machine("quarter e2e")
        cpus = JH.machine_cpus()
        quarter = max(1, round(25 / cpus))
        share = min(1.0, quarter * cpus / 100.0)        # of a core, for its one thread
        self.lending(sid, quarter)
        self.helper(sid, token, ceiling={**CEILING, "cpu_percent": quarter})
        with db.session() as s:
            lent = machine_settings.client(s, s.get(Server, sid), s.get(User, self.ids[self.owner]))["cpu_helper"]
        limits = lent.get("limits") or {}
        if sys.platform == "win32":
            self.assertEqual(limits.get("cpu"), "hard", f"a Job Object holds the share: {limits}")
        self.submit("e2e-quarter", 3, turns=300)
        self.tick_until(lambda: self.row(sid).trust_tier == "trusted", limit=180)
        events = self.tick_until(lambda: self.settled(sid, 2))
        row = self.row(sid)
        self.assertNotIn("overreported_cpu", row.flags or {})
        replicas = self._replica_cpu(sid)
        for e in events:
            self.assertNotIn("overreported_cpu", e.flags)
            self.assertLessEqual(e.cpu_core_s, 1.5 * replicas[e.request_id] + 1e-6)
            if limits.get("cpu") == "hard":
                # held to ``share`` of a core: its wall time is about its CPU over that share (3.4 to 3.9 times its
                # CPU at 0.24 of a core here, where 4.2 is the share's own: a Job Object lets a run's first moments go
                # before its rate bites); unheld, about its CPU. Asserted at 0.6 of the share's figure
                self.assertGreaterEqual(e.observed_s, 0.6 * e.reported_s / share,
                                        (e.observed_s, e.reported_s, share, limits))
        print(f"\n[verify e2e] a quarter of a core ({quarter}% of {cpus} CPUs: {share:.2f} of a core, held "
              f"{limits}): " + ", ".join(f"{e.reported_s:.2f} s of CPU over {e.observed_s:.2f} s" for e in events),
              file=sys.stderr)

    def _replica_cpu(self, sid: str) -> dict:
        """Each of the machine's primary runs' replica's CPU seconds (this server's job host: the reference)."""
        out = {}
        with db.session() as s:
            for p in s.scalars(select(CpuJobRun).where(CpuJobRun.machine_id == sid, CpuJobRun.role == "primary")):
                r = s.scalar(select(CpuJobRun).where(CpuJobRun.job_id == p.job_id, CpuJobRun.role == "replica",
                                                     CpuJobRun.verdict == "reference"))
                if r is not None:
                    out[p.id] = float(r.cpu_s)
        return out


if __name__ == "__main__":
    unittest.main()
