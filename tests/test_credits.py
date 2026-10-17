"""Credits for CPU work bounded by the job's reference cost, and the class check (design/phase4-helper.md P4.5.7;
package 4-12's gate 1), with the dispatcher, its local verifier and helpers in this process.

Each helper is ``tests.test_verification``'s ``Lender`` (a machine row with the fake helpers' throwaway key, its
results signed), its outcomes reporting the CPU seconds a test gives and seen by the server for as long as it says
(``--test-fault cpu:F`` and ``hold:S`` played in Python as the real helper plays them: ``tests/test_verification_e2e``
runs the real one). This server's job host verifies only, each of its replicas reporting a fixed reference: one second
of CPU.

- **Over-reporting** (``cpu:3``): a game of one second reported as three is credited at most 1.5 x its replica's
  CPU, sets ``flags.overreported_cpu`` on its machine and keeps it from ``trusted`` however many matches it makes.
- **Holding results** (``hold:S``): the held time reported as CPU is credited at most 1.5 x the reference and sets
  ``capped_cpu`` only; the machine is promoted.
- **A slow honest machine** (a quarter as fast, at a quarter of a core): ``capped_cpu`` only, promoted.
- **The class check:** two machines of one CPU class measured more than 2x apart: the slow one ``class_suspect``;
  pinned to a class of its own, the flag goes at its next measured job.
- **The replica's reference:** a run bounded at first by the cost table's figure for its shape is bounded again by its
  replica's own CPU once that reproduces it, its budget settlement moved by the difference.
- **An older jobs build** (4-11's leftover): a result that waited for its replica when the server stopped, of a jobs
  build the server no longer plays, is accepted ``unverified`` at the start.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import os
import shutil
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from sqlalchemy import select

import tests.test_dispatcher as TD
import tests.test_verification as TV
from citar import db, lab, ledger, servers, settings
from citar.db.models import AuditLog, BudgetEntry, CpuJob, CpuJobRun, HardwareClass, Server, ServerGroup, User
from citar.jobs import dispatcher as D, trust as T
from tests import has_test_ops

duels = TD.duels
Lender = TV.Lender

#: This module's own folder and database (``TD.Lab`` makes its folders in ``TD._TMP``; see ``tests.test_verification``).
_TMP = Path(tempfile.mkdtemp(prefix="citar_credits_"))
_SAVED: dict = {}

#: What this server's replicas report: the reference cost of every job here.
REFERENCE = 1.0


def setUpModule():
    _SAVED.update({k: os.environ.get(k) for k in ("CITAR_DATA_DIR", "CITAR_DB_URL")})
    _SAVED["_tmp"] = TD._TMP
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
    TD._TMP = _TMP
    settings.reset()


def tearDownModule():
    ledger.reset()
    db.dispose()
    TD._TMP = _SAVED.pop("_tmp")
    for k, v in _SAVED.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    _SAVED.clear()
    settings.reset()
    servers.invalidate()
    shutil.rmtree(_TMP, ignore_errors=True)


class Reference(TV.VerifierOnly):
    """This server's job host verifying only, each replica reporting ``cpu_s`` CPU seconds (the reference)."""

    def __init__(self, cpu_s: float = REFERENCE, threads: int = 1):
        super().__init__("sv_host", name="this server", threads=threads, auto=True)
        self.reference = cpu_s

    def play(self, run: str, *, peak: float = 1.0, **figures) -> None:
        figures.setdefault("cpu_s", self.reference)
        super().play(run, peak=peak, **figures)


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (the throwaway keys)")
class Credits(TV.Verify):
    """Gate 1: a helper's CPU seconds credited at most 1.5 x its job's reference cost and at most what the server
    observed; ``overreported_cpu`` blocks promotion, ``capped_cpu`` does not."""

    def lend(self, d: D.Dispatcher, lenders: dict, until, ticks: int = 400) -> bool:
        """Ticks ``d``, each lender in ``lenders`` (lender -> (cpu_s, observed)) playing every offer it has with those
        figures, until ``until()`` holds after a tick."""
        for _ in range(ticks):
            d.tick()
            if until():
                return True
            for lender, (cpu_s, observed) in lenders.items():
                for offer in lender.offers():
                    lender.play(offer, cpu_s=cpu_s, observed=observed)
        return False

    def primaries(self, lender: Lender) -> list:
        """Its primary runs' events, once settled (``verified``)."""
        out = []
        for _n, run, _job in lender.runs:
            ev = self.event(run)
            if ev is not None and ev.verification == "verified":
                out.append(ev)
        return out

    def audit_count(self, action: str, sid: str) -> int:
        with db.session() as s:
            return len(list(s.scalars(select(AuditLog).where(AuditLog.action == action,
                                                             AuditLog.object_id == sid))))

    def test_cpu_reported_three_times_over_is_capped_flagged_and_never_promoted_to_trusted(self):
        """``cpu:3``: a one-second game (one second observed) reported as three seconds."""
        self.settings(new_streak=2, promote_verified=4, provisional_rate=1.0)
        bob = Lender(self, "Bob's cpu:3", "bob")
        self.submit("bob", duels("over", 10))
        d = self.make([Reference()])
        self.assertTrue(self.lend(d, {bob: (3.0 * REFERENCE, REFERENCE)},
                                  until=lambda: int((bob.row().trust or {}).get("verified") or 0) >= 6))
        row = bob.row()
        self.assertEqual(row.trust_tier, "provisional", "six verified matches past promote_verified 4, and still not "
                                                        "trusted: its over-reporting blocks it")
        self.assertTrue(T.overreported(row.flags, datetime.now(timezone.utc), 30))
        flag = row.flags["overreported_cpu"]
        self.assertGreaterEqual(flag["count"], 6)
        self.assertEqual(flag["reported_s"], 3.0)
        self.assertEqual(self.audit_count("server.overreported_cpu", bob.sid), 1, "one audit row, the first")
        events = self.primaries(bob)
        self.assertGreaterEqual(len(events), 6)
        for ev in events:
            self.assertLessEqual(ev.cpu_core_s, 1.5 * REFERENCE + 1e-6, "at most 1.5 x the reference cost")
            self.assertAlmostEqual(ev.cpu_core_s, 1.5 * REFERENCE, places=6)
            self.assertEqual(ev.reported_s, 3.0)
            self.assertTrue(ev.flags.get("overreported_cpu"))
            self.assertEqual(ev.flags.get("reference_s"), REFERENCE, "bounded by its replica's own CPU")
            self.assertGreater(ev.credits, 0.0)
        self.assertAlmostEqual(T.speed_of(row.trust), round(1 / 3, 3), places=3)

    def test_holding_results_is_capped_at_the_reference_and_flags_capped_only(self):
        """``hold:S``: each result held 4 s and the held time reported as CPU (the server observes the hold too)."""
        self.settings(new_streak=2, promote_verified=4, provisional_rate=1.0)
        bob = Lender(self, "Bob's hold:4", "bob")
        self.submit("bob", duels("held", 10))
        d = self.make([Reference()])
        held = REFERENCE + 4.0
        self.assertTrue(self.lend(d, {bob: (held, held)}, until=lambda: bob.row().trust_tier == "trusted"))
        row = bob.row()
        self.assertNotIn("overreported_cpu", row.flags or {})
        self.assertGreaterEqual(row.flags["capped_cpu"]["count"], 4)
        self.assertEqual(self.audit_count("server.capped_cpu", bob.sid), 1)
        events = self.primaries(bob)
        self.assertGreaterEqual(len(events), 4)
        for ev in events:
            self.assertAlmostEqual(ev.cpu_core_s, 1.5 * REFERENCE, places=6)
            self.assertTrue(ev.flags.get("capped_cpu"))
            self.assertNotIn("overreported_cpu", ev.flags)

    def test_an_honest_machine_at_a_quarter_core_sets_capped_only_and_is_promoted(self):
        """A machine a quarter as fast as the reference, at a quarter of a core: each game's four seconds of CPU take
        sixteen, all honest. The reference cap holds its credits; nothing blocks its promotion."""
        self.settings(new_streak=2, promote_verified=4, provisional_rate=1.0)
        bob = Lender(self, "Bob's slow", "bob")
        self.submit("bob", duels("slow", 10))
        d = self.make([Reference()])
        self.assertTrue(self.lend(d, {bob: (4.0 * REFERENCE, 16.0 * REFERENCE)},
                                  until=lambda: bob.row().trust_tier == "trusted"))
        row = bob.row()
        self.assertEqual(sorted(k for k in row.flags or {} if k in ("overreported_cpu", "capped_cpu")),
                         ["capped_cpu"])
        self.assertAlmostEqual(T.speed_of(row.trust), 0.25, places=3)
        changes = [a["changed"] for a in self.audits("server.trust", bob.sid)]
        self.assertEqual(changes, [{"from": "new", "to": "provisional"}, {"from": "provisional", "to": "trusted"}])
        for ev in self.primaries(bob):
            self.assertAlmostEqual(ev.cpu_core_s, 1.5 * REFERENCE, places=6)
            self.assertNotIn("overreported_cpu", ev.flags)

    def test_a_class_whose_speeds_differ_by_more_than_2x_flags_the_slow_one(self):
        """Two machines of one CPU class (the default one): alice's as fast as the reference, bob's at 0.4 of it. Bob's
        is ``class_suspect`` (under half the median of the other's), alice's is not; pinned by an administrator to a
        class of its own, bob's loses the flag at its next measured job."""
        self.settings(new_streak=1000, provisional_rate=1.0)
        alice = Lender(self, "Alice's fast", "alice")
        bob = Lender(self, "Bob's slow", "bob", seed=TV.TEST_KEY_SEED + 3)
        self.submit("alice", duels("fast", 6))
        self.submit("bob", duels("slow-class", 8))
        d = self.make([Reference(threads=2)])
        figures = {alice: (REFERENCE, REFERENCE), bob: (2.5 * REFERENCE, 2.5 * REFERENCE)}

        def measured():
            a, b = alice.row(), bob.row()
            return len((a.trust or {}).get("speeds") or []) >= 2 and len((b.trust or {}).get("speeds") or []) >= 2
        self.assertTrue(self.lend(d, figures, until=measured))
        a, b = alice.row(), bob.row()
        self.assertAlmostEqual(T.speed_of(a.trust), 1.0, places=3)
        self.assertAlmostEqual(T.speed_of(b.trust), 0.4, places=3)
        self.assertNotIn("class_suspect", a.flags or {})
        flag = b.flags["class_suspect"]
        self.assertEqual((flag["speed"], flag["class_median"]), (0.4, 1.0))
        self.assertEqual(self.audit_count("server.class_suspect", bob.sid), 1)
        # an administrator pins bob's machine to a slower class of its own: alone in it, it is no longer out of line
        with db.session() as s:
            # a match no machine's hardware fits: only a pin puts a machine in it
            slow = HardwareClass(kind="cpu", name="Slow CPU", rate=0.4, match={"cpu_name": "^a CPU nobody has$"},
                                 priority=0, is_default=False)
            s.add(slow)
            s.flush()
            row = s.get(Server, bob.sid)
            row.cpu_class_id, row.class_override = slow.id, True
        servers.invalidate()
        ledger.get().invalidate_classes()
        n = len(bob.row().trust["speeds"])
        self.assertTrue(self.lend(d, figures, until=lambda: len(bob.row().trust["speeds"]) > n))
        self.assertNotIn("class_suspect", bob.row().flags or {})

    def test_the_replica_gives_the_jobs_own_reference_and_the_budget_follows(self):
        """A 30-round duel's cost-table figure is a few hundredths of a second: a primary reporting a second is capped
        there while it waits (``pending``); once this server's replica reproduces it with its own second, the event is
        bounded by that instead: the whole second, no cap, and the budget's settlement moves by the difference."""
        from citar import pool
        from citar.pool import budgets
        budgets.reset_keeper()
        self.addCleanup(budgets.reset_keeper)
        self.settings(new_streak=1000)
        bob = Lender(self, "Bob's lender", "bob")
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, bob.group_id),
                              subject_type="user", subject_handle="alice", purposes=["lab"],
                              budget={"period": "month", "amount": 1_000_000, "unit": "credits", "behavior": "hard"})
        with db.session() as s:
            budgets.keeper().sync(s)
        self.submit("alice", duels("rebound", 1))
        host = Reference()
        host.state = "off"          # its replica waits: the primary's event is seen pending first
        d = self.make([host])
        self.assertTrue(self.lend(d, {bob: (REFERENCE, REFERENCE)}, until=lambda: bool(bob.runs) and any(
            r.state == "done" for r in self.runs() if r.role == "primary")))
        ((_n, run, _job),) = bob.runs
        pending = self.event(run)
        job = self.job_of(run)
        table = d.costs.of(job.kind, job.spec)["cpu_s"]
        self.assertEqual(pending.verification, "pending")
        self.assertAlmostEqual(pending.cpu_core_s, 1.5 * table, places=6, msg="capped by the cost table at first")
        self.assertTrue(pending.flags.get("capped_cpu"))
        spent_before = self._spent()
        host.state = "ready"
        self.assertTrue(self.lend(d, {}, until=lambda: (lab.DONE / "rebound.json").exists()))
        settled = self.event(run)
        self.assertEqual(settled.verification, "verified")
        self.assertAlmostEqual(settled.cpu_core_s, REFERENCE, places=6, msg="its own second, within 1.5 x its replica")
        self.assertNotIn("capped_cpu", settled.flags)
        self.assertEqual(settled.flags.get("reference_s"), REFERENCE)
        self.assertAlmostEqual(self._spent() - spent_before, settled.credits - pending.credits, places=5,
                               msg="the budget's settlement moved by the difference")
        account = budgets.keeper().account(self._budget_id())
        self.assertAlmostEqual(account["spent"], self._spent(), places=5)

    def _spent(self) -> float:
        self.led.drain()
        with db.session() as s:
            return sum(e.actual for e in s.scalars(select(BudgetEntry)))

    def _budget_id(self) -> str:
        with db.session() as s:
            return s.scalar(select(BudgetEntry.budget_id))


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (the throwaway keys)")
class OlderJobsBuild(TV.Verify):
    """A result that waits for its replica when the server is upgraded (another jobs build) is accepted unverified at
    the next start: no verifier would ever reproduce it."""

    def test_a_settling_job_of_an_older_jobs_build_is_accepted_unverified_at_the_start(self):
        bob = Lender(self, "Bob's new", "bob")
        self.submit("bob", duels("upgraded", 1))
        host = self.verifier()
        host.state = "off"
        d = self.make([host])
        self.assertTrue(self.drive(d, [bob], until=lambda: any(
            r.role == "primary" and r.state == "done" for r in self.runs())))
        ((_n, run, job_id),) = bob.runs
        with db.session() as s:
            self.assertEqual(s.get(CpuJob, job_id).state, "settling")
            s.get(CpuJob, job_id).jobs_build = "0" * 12          # the jobs build the server played before it was upgraded
        self.led.drain()            # its event written (pending), as the ledger's writer does within seconds
        d.stop()
        D.install(None)
        again = self.verifier()
        d2 = self.make([again])
        self.assertTrue(self.drive(d2, [bob], until=lambda: (lab.DONE / "upgraded.json").exists()))
        with db.session() as s:
            job = s.get(CpuJob, job_id)
            self.assertEqual((job.state, job.accepted_run), ("accepted", run))
            self.assertEqual(s.get(CpuJobRun, run).verdict, "unchecked")
            self.assertEqual([r.role for r in s.scalars(select(CpuJobRun).where(CpuJobRun.job_id == job_id))],
                             ["primary"], "no replica was ever offered")
        (line,) = lab.load_results("upgraded")
        self.assertEqual((line["executor"]["run"], line["executor"]["verification"]), (run, "unverified"))
        self.assertEqual(self.event(run).verification, "unverified")
        self.assertEqual(int((bob.row().trust or {}).get("verified") or 0), 0, "it counts no match")


if __name__ == "__main__":
    unittest.main()
