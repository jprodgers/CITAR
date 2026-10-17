"""Proof of work: helpers' results verified, trust tiers, quarantine and withdrawal (design/phase4-helper.md P4.5.8,
P4.4.4; package 4-11's gates 2 to 7, with the dispatcher, its local verifier and helpers in this process).

Each helper is a machine row with the fake helpers' throwaway key registered, connected through a stub (no socket) as
its remote executor; the test plays every offer it gets, honestly or with one of P4.4.6's faults played in Python as
``--test-fault`` plays it in the real helper (``tests/test_verification_e2e.py`` runs the real one): ``result`` (one
score raised and the result hashed again, signed), ``digest:R`` (round R's digest changed and the chain folded again
from it, signed), ``sig`` (signed with another key), ``abandon`` (accepted, never answered). This server's job host is
a ``Fake`` that plays what it is offered at once, honestly: in most tests it verifies only (its primaries would take
the helpers' games), so that every game goes to the helpers and the order of their jobs is the test's.

- **Caught while new** (gate 2): ``result`` quarantines the machine at its first job with "same chain, different
  result: tampering or an incompatible build", its result never in the results file, its event rejected with no
  credits, its owner quarantined and refused CPU work on a second machine; ``digest:12``'s evidence names round 12 and
  quarantines the machine alone; ``sig`` is a bad signature, at once; an abandoned job is queued again and counted.
- **Caught by sampling, everything withdrawn** (gate 3): a selective cheater reaches ``provisional`` after five
  matches; its job 6 (altered) is accepted ``unverified``, an administrator's ``replicate 1`` makes job 7 a verified
  match, job 8 is accepted ``unverified``, and with the rate at 1 job 9 is replicated and mismatches: the machine is
  quarantined, jobs 6 and 8 leave the results file and are played again elsewhere, their events are rejected with
  their budget settlements reversed, and the ratings no longer count them.
- **Visibility** (gate 4): while a primary is in flight its job's listings are the same whether or not it will be
  sampled; its replica is made only once its result is committed.
- **Collusion and load** (gate 5), **promotion** (gate 6), **quarantine and release** (gate 7).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path

from sqlalchemy import select

import tests.test_dispatcher as TD
from citar import _engine, db, engine_api, lab, ledger, servers, settings
from citar.bots import ratings
from citar.db.models import AuditLog, BudgetEntry, ComputeEvent, CpuJob, CpuJobRun, Server, ServerGroup, User
from citar.jobs import dispatcher as D, remote, trust as T
from tests import has_test_ops
from tests.helperfake import claim_of, sign_claim, test_key, TEST_KEY_SEED

duels = TD.duels

#: This module's own folder and database: ``TD.Lab`` makes each test's lab folders in ``TD._TMP``, which
#: ``tests.test_dispatcher``'s own ``tearDownModule`` removes when that module ran first in this process.
_TMP = Path(tempfile.mkdtemp(prefix="citar_verification_"))
_SAVED: dict = {}


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


def altered_score(out: dict, kind: str) -> dict:
    """``out`` with one score raised and its result hashed again (``result``)."""
    out = json.loads(json.dumps(out))
    first = next(iter(out["result"]["players"].values()))
    first["score"] += 1
    out["result_hash"] = engine_api.job_result_hash(kind, out["result"])
    return out


def altered_digest(out: dict, spec_hash: str, round_: int) -> dict:
    """``out`` with round ``round_``'s digest changed and the chain folded again from it (``digest:R``)."""
    out = json.loads(json.dumps(out))
    digests = out["chain"]["digests"]
    d = digests[round_ - 1]
    digests[round_ - 1] = d[:-1] + ("1" if d[-1] == "0" else "0")
    out["chain"]["head"] = _engine.jobs.test_fold(spec_hash, json.dumps(digests).encode())
    return out


class VerifierOnly(TD.Fake):
    """This server's job host as a verifier alone: it plays verification runs at once and refuses primaries, so the
    helpers get every game and the test the order of their jobs."""

    only_verifies = True

    def refuses(self, job):
        if self.only_verifies and job.state != "settling":
            return "this test's host plays verification runs only"
        return super().refuses(job)


class Lender:
    """A helper machine of ``owner``'s in their group (its key the fake helpers' throwaway one), connected through a
    stub, lending ``threads`` threads to lab and ranking games. ``play`` answers its offers as the test says; ``n``
    counts the jobs it played (the fault modes' count)."""

    def __init__(self, t: "Verify", name: str, owner: str, *, tier: str = "new", threads: int = 1,
                 seed: int = TEST_KEY_SEED, work: tuple = ("lab_game", "ranking_game")):
        from tests.helperfake import StubConnection, hello, settings as site_settings
        self.t, self.name, self.owner, self.seed = t, name, owner, seed
        with db.session() as s:
            group = s.scalar(select(ServerGroup).where(ServerGroup.owner_id == t.ids[owner]))
            if group is None:
                group = ServerGroup(owner_id=t.ids[owner], name=f"{owner}'s machines", slug=f"{owner}-machines")
                s.add(group)
                s.flush()
            row = Server(owner_id=t.ids[owner], name=name, kind="owned", provider="lmstudio", reach="worker",
                         max_concurrent=1, group_id=group.id, trust_tier=tier, public_key=test_key(seed),
                         capabilities={"cpu": {"enabled": True, "threads": threads, "cpu_percent": 50,
                                               "memory_mb": 1024, "work": list(work),
                                               "yield_to_llm": True}})
            s.add(row)
            s.flush()
            self.sid, self.group_id = row.id, group.id
        servers.invalidate()
        site = site_settings(llm=False)
        site["cpu"] = {"enabled": True, "threads": threads, "cpu_percent": 50, "memory_mb": 1024,
                       "work": list(work), "yield_to_llm": True}
        conn = StubConnection(self.sid, site=site)
        h = hello(hostname=name, llm=False, key=test_key(seed))
        h["helper"]["jobs_build"] = engine_api.jobs_build()
        h["cpu"] = {"enabled": True, "ceiling": {"threads": threads, "cpu_percent": 50, "memory_mb": 1024,
                                                 "work": ["lab_game", "ranking_game"]},
                    "limits": {"cpu": "hard", "memory": "hard"}, "verifiable": True, "bots": [], "ruleset": "0" * 64}
        conn.hello, conn.jobs_build, conn.memory_per_run_mb = h, h["helper"]["jobs_build"], 500
        conn.acknowledge({"effective": {"llm": {"enabled": False, "slots": 0}, "cpu": dict(site["cpu"])},
                          "capped": [], "limits": {"cpu": "hard", "memory": "hard"}, "memory_per_run_mb": 500})
        self.conn = conn
        self.e = t.registry.get(self.sid, name)
        self.e.connected(conn)
        self.answered: set = set()
        self.n = 0
        self.runs: list = []            # (n, run, job) as it played them

    def offers(self) -> list:
        """Its offers not answered yet."""
        return [f for f in self.conn.frames if f.get("t") == "job_offer" and f["run"] not in self.answered]

    def play(self, offer: dict, fault: str = "") -> None:
        """Accepts ``offer`` and sends its result, honest or as ``fault`` says."""
        self.answered.add(offer["run"])
        self.n += 1
        self.runs.append((self.n, offer["run"], offer["job"]))
        self.e.frame({"t": "job_accept", "run": offer["run"]})
        if fault == "abandon":
            return
        out = engine_api.run_job(offer["kind"], offer["spec"], spec_hash=offer["spec_hash"])
        if fault == "result":
            out = altered_score(out, offer["kind"])
        elif fault.startswith("digest:"):
            out = altered_digest(out, offer["spec_hash"], int(fault.split(":")[1]))
        claim = claim_of(offer, out, self.sid, seed=self.seed)
        signature = sign_claim(claim, self.seed + 1 if fault == "sig" else self.seed)
        self.e.frame({"t": "job_result", "run": offer["run"],
                      "outcome": {"result": out["result"], "result_hash": out["result_hash"], "chain": out["chain"],
                                  "cpu_s": out["cpu_s"], "wall_s": out["wall_s"], "paused_s": 0.0, "peak_mb": 3.0},
                      "claim": claim, "signature": signature})

    def row(self) -> Server:
        with db.session() as s:
            row = s.get(Server, self.sid)
            s.expunge(row)
            return row


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (the throwaway keys)")
class Verify(TD.Lab):
    """A fresh database, the lab's folders, alice and bob (members), mona (a moderator), zara (an administrator), and
    the verification settings at their defaults (each test sets what it needs)."""

    THREADS = 1

    def setUp(self):
        super().setUp()
        remote.forget_owner_tiers()
        self.addCleanup(remote.forget_owner_tiers)
        T.forget()
        self.addCleanup(T.forget)
        self.registry = remote.Remotes()
        with db.session() as s:
            from citar.auth import accounts
            user = accounts.create_user(s, handle="carol", email="carol@example.com",
                                        password="a long enough phrase", role="user", status="active",
                                        email_verified=True)
            self.ids["carol"] = user.id

    # -------------------------------------------------------------- the pieces
    def settings(self, **values) -> None:
        with db.session() as s:
            T.save_settings(s, values)

    def verifier(self, **kw) -> VerifierOnly:
        return VerifierOnly("sv_host", name="this server", threads=kw.pop("threads", 1), auto=True, **kw)

    def make(self, executors, **kw) -> D.Dispatcher:
        return self.dispatcher(executors, remotes=self.registry, **kw)

    def drive(self, d: D.Dispatcher, lenders, fault=lambda lender, n: "", until=lambda: False, ticks: int = 400):
        """Ticks ``d``, each lender playing every offer it has after each tick (its ``n``-th job as ``fault(lender,
        n)`` says), until ``until()`` holds after a tick (then nothing more is played: an offer made in that tick
        waits for the next drive), or ``ticks`` pass."""
        for _ in range(ticks):
            d.tick()
            if until():
                return True
            for lender in lenders:
                for offer in lender.offers():
                    lender.play(offer, fault(lender, lender.n + 1))
        return False

    def runs(self, job_id: str | None = None) -> list:
        with db.session() as s:
            q = select(CpuJobRun).order_by(CpuJobRun.offered_at)
            if job_id is not None:
                q = q.where(CpuJobRun.job_id == job_id)
            out = list(s.scalars(q))
            for r in out:
                s.expunge(r)
            return out

    def job_of(self, run: str) -> CpuJob:
        with db.session() as s:
            job = s.get(CpuJob, s.get(CpuJobRun, run).job_id)
            s.expunge(job)
            return job

    def event(self, run: str) -> ComputeEvent:
        self.led.drain()
        with db.session() as s:
            ev = s.scalar(select(ComputeEvent).where(ComputeEvent.request_id == run))
            if ev is not None:
                s.expunge(ev)
            return ev

    def owner_tier(self, who: str) -> str:
        with db.session() as s:
            return s.get(User, self.ids[who]).trust_tier

    def audits(self, action: str, object_id: str | None = None) -> list:
        with db.session() as s:
            q = select(AuditLog).where(AuditLog.action == action)
            if object_id is not None:
                q = q.where(AuditLog.object_id == object_id)
            return [dict(a.detail or {}, actor=a.actor_label) for a in s.scalars(q.order_by(AuditLog.id))]


class CaughtWhileNew(Verify):
    """Gate 2: a new machine's every result is replicated on this server, and a cheat is caught at once."""

    def test_an_altered_result_quarantines_the_machine_and_its_owner_at_its_first_job(self):
        bob = Lender(self, "Bob's cheat", "bob")
        self.submit("bob", duels("cheat", 3))
        host = self.verifier()
        d = self.make([host])
        self.assertTrue(self.drive(d, [bob], fault=lambda lender, n: "result",
                                   until=lambda: bob.row().trust_tier == "quarantined"))
        ((n, run, job_id),) = bob.runs
        self.assertEqual(n, 1, "caught at its first job")
        row = bob.row()
        ev = row.trust["last_mismatch"]
        self.assertEqual((ev["verdict"], ev["words"]),
                         ("result", "same chain, different result: tampering or an incompatible build"))
        self.assertEqual((ev["run"], ev["against"]), (run, "this server"))
        primary = next(r for r in self.runs(job_id) if r.id == run)
        self.assertEqual((primary.verdict, primary.error["verdict"]), ("mismatch", "result"))
        replica = next(r for r in self.runs(job_id) if r.role == "replica")
        self.assertEqual((replica.machine_id, replica.verdict, replica.offered_at >= primary.ended_at),
                         ("sv_host", "reference", True))
        self.assertNotIn(run, {(x.get("executor") or {}).get("run") for x in lab.load_results("cheat")},
                         "its result never reaches the results file")
        event = self.event(run)
        self.assertEqual((event.verification, event.credits, event.flags.get("rejected")),
                         ("rejected", 0.0, ev["words"]))
        self.assertEqual(self.owner_tier("bob"), "quarantined", "a tampering verdict quarantines its owner")
        self.assertTrue(self.audits("user.trust", self.ids["bob"]))
        self.assertTrue(self.audits("server.trust", bob.sid))
        # the job is played again elsewhere: never by a quarantined machine, nor by another machine of its owner's
        second = Lender(self, "Bob's other", "bob", seed=TEST_KEY_SEED + 7)
        d.tick()
        d.tick()
        refusals = {second.e.refuses(j) for j in d.jobs.values() if j.state == "queued"}
        self.assertEqual(refusals, {"its owner is quarantined"})
        self.assertEqual({bob.e.refuses(j) for j in d.jobs.values() if j.state == "queued"}, {"it is quarantined"})
        self.assertEqual(second.offers(), [])
        self.assertEqual(self.job_of(run).state, "withdrawn")

    def test_a_changed_digest_names_its_round_and_quarantines_the_machine_alone(self):
        bob = Lender(self, "Bob's broken", "bob")
        self.submit("bob", duels("broken", 2))
        d = self.make([self.verifier()])
        self.assertTrue(self.drive(d, [bob], fault=lambda lender, n: "digest:12",
                                   until=lambda: bob.row().trust_tier == "quarantined"))
        ev = bob.row().trust["last_mismatch"]
        self.assertEqual((ev["verdict"], ev["round"]), ("chain", 12))
        self.assertIn("parted at round 12", ev["words"])
        self.assertIn("hardware", ev["words"])
        self.assertEqual(self.owner_tier("bob"), "new", "a chain verdict quarantines the machine alone")

    def test_a_bad_signature_is_caught_at_once(self):
        bob = Lender(self, "Bob's forger", "bob")
        self.submit("bob", duels("forged", 2))
        d = self.make([self.verifier()])
        self.assertTrue(self.drive(d, [bob], fault=lambda lender, n: "sig",
                                   until=lambda: bob.row().trust_tier == "quarantined"))
        ((_n, run, job_id),) = bob.runs
        ev = bob.row().trust["last_mismatch"]
        self.assertEqual(ev["verdict"], "signature")
        self.assertIn("a bad signature", ev["words"])
        self.assertEqual([r.role for r in self.runs(job_id)], ["primary"], "no replica was needed")
        self.assertEqual(self.owner_tier("bob"), "quarantined")
        self.assertIn({"t": "job_result_ack", "run": run},
                      [{k: v for k, v in f.items() if k != "ts"} for f in bob.conn.frames], "acknowledged: decided")
        self.assertEqual(self.event(run).verification, "rejected")

    def test_an_abandoned_job_is_queued_again_and_counted(self):
        clock = TD.Clock()
        bob = Lender(self, "Bob's quitter", "bob")
        self.submit("bob", duels("quit", 1))
        d = self.make([self.verifier()], clock=clock)
        self.drive(d, [bob], fault=lambda lender, n: "abandon", until=lambda: bool(bob.runs))
        ((_n, run, job_id),) = bob.runs
        d.tick()
        clock.t += D.SILENT_SECONDS + 1
        d.tick()
        (row,) = [r for r in self.runs(job_id) if r.id == run]
        self.assertEqual((row.state, row.error["message"]), ("expired", "no progress for 5 minutes"))
        self.assertEqual(self.job_of(run).attempts, 1, "queued again, an attempt")
        self.assertEqual(len([r for r in self.runs(job_id) if r.role == "primary"]), 2, "offered again")
        self.assertEqual(bob.row().trust.get("abandons"), 1)
        self.assertEqual(bob.row().trust_tier, "new", "an abandon is no mismatch")


class CaughtBySampling(Verify):
    """Gate 3: a selective cheater, provisional, caught by sampling; everything unverified it gave withdrawn."""

    def test_every_unverified_result_is_withdrawn_one_from_before_its_verified_match_included(self):
        from citar import pool
        from citar.pool import budgets
        budgets.reset_keeper()
        self.addCleanup(budgets.reset_keeper)
        self.settings(provisional_rate=0.0, new_streak=5)
        bob = Lender(self, "Bob's selective", "bob")
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, bob.group_id),
                              subject_type="user", subject_handle="alice", purposes=["lab"],
                              budget={"period": "month", "amount": 1_000_000, "unit": "credits", "behavior": "hard"})
        with db.session() as s:
            budgets.keeper().sync(s)
        self.submit("alice", duels("sampled", 12))
        host = self.verifier()
        d = self.make([host])
        cheats = {6, 9}

        def fault(lender, n):
            return "result" if n in cheats else ""
        # jobs 1-5: replicated (new) and matched: provisional
        self.assertTrue(self.drive(d, [bob], fault, until=lambda: bob.row().trust_tier == "provisional"))
        # promoted by job 5's match; job 6 may be offered in the tick that promoted it
        self.assertIn(bob.n, (5, 6))
        self.assertEqual((bob.row().trust["verified"], bob.row().trust["streak"]), (5, 5))
        self.assertTrue(self.audits("server.trust", bob.sid))
        # job 6 (altered): provisional at rate 0, accepted unverified
        self.assertTrue(self.drive(d, [bob], fault, until=lambda: bob.n >= 6 and self._line(bob.runs[5][1])))
        six = bob.runs[5][1]
        self.assertEqual(self._line(six)["executor"]["verification"], "unverified")
        # an administrator asks for its next result's replica: job 7 (honest) a verified match
        self.assertEqual(D.replicate_next(bob.sid, 1, actor_id=self.ids["zara"]), {"replicate": 1})
        self.assertTrue(self.drive(d, [bob], fault, until=lambda: bob.n >= 7 and self._line(bob.runs[6][1])))
        seven = bob.runs[6][1]
        self.assertEqual(self._line(seven)["executor"]["verification"], "verified")
        self.assertEqual(bob.row().trust["replicate"], 0)
        self.assertEqual(bob.row().trust["verified"], 6)
        # job 8: unverified
        self.assertTrue(self.drive(d, [bob], fault, until=lambda: bob.n >= 8 and self._line(bob.runs[7][1])))
        eight = bob.runs[7][1]
        self.assertEqual(self._line(eight)["executor"]["verification"], "unverified")
        self.led.drain()
        settled = {r: self.event(r).credits for r in (six, eight)}
        self.assertTrue(all(c > 0 for c in settled.values()), settled)
        before = self._budget_spent()
        games_before, _ = ratings.collect()
        # the rate at 1: job 9 is replicated, and mismatches
        self.settings(provisional_rate=1.0)
        self.assertTrue(self.drive(d, [bob], fault, until=lambda: bob.row().trust_tier == "quarantined"))
        self.assertGreaterEqual(bob.n, 9, "a provisional machine goes on while its result is checked")
        ev = bob.row().trust["last_mismatch"]
        self.assertEqual((ev["verdict"], ev["run"]), ("result", bob.runs[8][1]))
        withdrawn = {w["run"] for w in ev["withdrawn_jobs"]}
        self.assertTrue({six, eight} <= withdrawn, (withdrawn, six, eight))
        self.assertNotIn(seven, withdrawn, "a verified match is not withdrawn")
        lines = lab.load_results("sampled")
        runs_lined = {(x.get("executor") or {}).get("run") for x in lines}
        self.assertFalse({six, eight} & runs_lined, "they leave the results file")
        self.assertIn(seven, runs_lined)
        for r in (six, eight):
            event = self.event(r)
            self.assertEqual((event.verification, event.credits), ("rejected", 0.0))
            self.assertAlmostEqual(event.flags["rejected_credits"], settled[r], places=6)
        after = self._budget_spent()
        self.assertAlmostEqual(before - after, sum(settled.values()), places=5, msg="budget settlements reversed")
        self.assertAlmostEqual(budgets.keeper().account(self._budget_id())["spent"], after, places=5)
        games_after, _ = ratings.collect()
        self.assertEqual(len(games_after), len(games_before) - 2, "the ratings no longer count them")
        # played again elsewhere and accepted: this server plays primaries now
        host.only_verifies = False
        jobs6 = {self.job_of(six).ref, self.job_of(eight).ref}
        self.assertTrue(self.drive(d, [], until=lambda: (lab.DONE / "sampled.json").exists(), ticks=600))
        lines = lab.load_results("sampled")
        self.assertEqual(sorted(x["i"] for x in lines), list(range(12)), "each game once")
        again = [x for x in lines if f"lab:sampled:{x['i']}" in jobs6]
        self.assertEqual({x["executor"]["machine"] for x in again}, {"sv_host"})

    def _line(self, run: str):
        return next((x for x in lab.load_results("sampled") if (x.get("executor") or {}).get("run") == run), None)

    def _budget_id(self) -> str:
        with db.session() as s:
            return s.scalar(select(BudgetEntry.budget_id))

    def _budget_spent(self) -> float:
        self.led.drain()
        with db.session() as s:
            return sum(e.actual for e in s.scalars(select(BudgetEntry)))


class Visibility(Verify):
    """Gate 4: nothing tells a helper's owner, or an experiment's, which of its jobs a replica will check."""

    def listings(self, d, lender, exp: str) -> dict:
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        bob = self.headers["bob"]
        out = {"lab_jobs": client.get(f"/api/lab/jobs?exp={exp}", headers=bob).json(),
               "machine_jobs": client.get(f"/api/machines/{lender.sid}/jobs", headers=bob).json(),
               "queue": client.get("/api/queue", headers=bob).json()["cpu"]}
        return _normal(out)

    def scenario(self, rate: float, name: str) -> tuple:
        self.settings(trusted_rate=rate)
        bob = Lender(self, f"Bob's {name}", "bob", tier="trusted")
        self.submit("bob", duels(name, 1))
        hooked: list = []
        d = self.make([self.verifier()])

        def replicated(job_id, run_id):
            # the primary's result is committed (another session sees it) and no replica exists yet
            with db.session() as s:
                primary = s.get(CpuJobRun, run_id)
                replicas = s.scalars(select(CpuJobRun).where(CpuJobRun.job_id == job_id,
                                                             CpuJobRun.role != "primary")).all()
                hooked.append((primary.state, primary.result_hash is not None, len(replicas)))
        d.hooks["replicated"] = replicated
        for _ in range(3):
            d.tick()
        (offer,) = bob.offers()
        bob.answered.add(offer["run"])
        bob.e.frame({"t": "job_accept", "run": offer["run"]})
        d.tick()
        in_flight = self.listings(d, bob, name)
        bob.answered.discard(offer["run"])
        bob.play(offer)
        self.drive(d, [bob], until=lambda: (lab.DONE / f"{name}.json").exists())
        bob.e.disconnected(bob.conn)
        self.registry.clear()               # the next scenario's machine is the only helper
        return in_flight, hooked, offer["run"]

    def test_listings_while_a_primary_plays_are_the_same_whether_or_not_it_will_be_sampled(self):
        sampled, hooked, run = self.scenario(1.0, "watched")
        self.assertEqual(hooked, [("done", True, 0)], "replicated once its result was committed, before any replica")
        replica = next(r for r in self.runs() if r.role == "replica")
        primary = next(r for r in self.runs() if r.id == run)
        self.assertGreaterEqual(replica.offered_at, primary.ended_at)
        self.assertEqual(self.job_of(run).state, "accepted")
        unsampled, hooked2, _run = self.scenario(0.0, "unwatched")
        self.assertEqual(hooked2, [])
        self.assertEqual(json.dumps(sampled, sort_keys=True).replace("watched", "X").replace("unX", "X"),
                         json.dumps(unsampled, sort_keys=True).replace("unwatched", "X"))

    def test_a_member_sees_a_settling_job_done_and_no_replica(self):
        """Rule 2: once its primary's result is in, a job is ``done`` to everyone but administrators until it settles;
        its replica and the ``settling`` state are administrators' alone."""
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        bob = Lender(self, "Bob's new", "bob")
        self.submit("bob", duels("settling", 1))
        host = self.verifier()
        host.auto = False
        d = self.make([host])
        self.drive(d, [bob], until=lambda: bool(host.offers))
        member = client.get("/api/lab/jobs?exp=settling", headers=self.headers["bob"]).json()
        (job,) = member["jobs"]
        self.assertEqual(job["state"], "done")
        self.assertIsNone(job["verification"])
        self.assertNotIn("runs", job)
        admin = client.get("/api/lab/jobs?exp=settling", headers=self.headers["zara"]).json()
        (ajob,) = admin["jobs"]
        self.assertEqual(ajob["state"], "settling")
        self.assertEqual(sorted(r["role"] for r in ajob["runs"]), ["primary", "replica"])
        lab_view = client.get("/api/lab", headers=self.headers["bob"]).json()
        self.assertEqual([r for r in lab_view["running"] if r.get("exp") == "settling"], [], "no replica in sight")
        (exp,) = [e for e in lab_view["experiments"] if e["name"] == "settling"]
        self.assertEqual(exp["done"], 1)
        queue = client.get("/api/queue", headers=self.headers["bob"]).json()["cpu"]
        self.assertEqual([g for e in queue["executors"] for g in e["games"]], [])
        self.assertEqual({e["name"]: e["running"] for e in queue["executors"]}.get("this server"), 0)
        machine = client.get(f"/api/machines/{bob.sid}/jobs", headers=self.headers["bob"]).json()
        self.assertEqual([r.get("role") for r in machine["runs"]], [None], "its own run alone, with no role")
        self.assertNotIn("verdict", machine["runs"][0])
        host.play(host.offers[0][0])
        d.tick()
        member = client.get("/api/lab/jobs?exp=settling", headers=self.headers["bob"]).json()
        self.assertEqual((member["jobs"][0]["state"], member["jobs"][0]["verification"]), ("done", "verified"))


def _normal(value):
    """A listing with its ids and times taken out (what differs between any two runs)."""
    if isinstance(value, dict):
        return {k: _normal(v) for k, v in value.items()
                if k not in ("job", "run", "offered", "started", "ended", "minutes", "cpu_s", "wall_s", "peak_mb",
                             "id", "machine", "load", "cpus", "submitted", "finished", "eta_minutes",
                             "remaining_minutes", "minutes_per_game")}
    if isinstance(value, list):
        return [_normal(v) for v in value]
    if isinstance(value, str) and value.startswith(("cj_", "cr_", "sv_")):
        return "<id>"
    return value


class CollusionAndLoad(Verify):
    """Gate 5: who verifies, the caps on new machines, this server's share, and a quarantined verifier."""

    def test_a_trusted_machine_that_is_no_verifier_never_gets_a_replica(self):
        self.settings(provisional_rate=1.0)
        carol = Lender(self, "Carol's trusted", "carol", tier="trusted", threads=2)
        newbie = Lender(self, "Bob's new", "bob")
        prov = Lender(self, "Bob's provisional", "bob", tier="provisional", seed=TEST_KEY_SEED + 3)
        self.submit("bob", duels("checked", 6))
        d = self.make([])                      # no verifier on this server: carol's is the only other machine
        self.drive(d, [carol, newbie, prov], ticks=40)
        roles = {(r.machine_id, r.role) for r in self.runs()}
        self.assertNotIn((carol.sid, "replica"), roles, "a trusted machine that is no verifier never replicates")
        self.assertFalse([r for r in self.runs() if r.machine_id == newbie.sid], "no verifier for a new machine")
        self.assertEqual(newbie.e and d._available(newbie.e, d.clock()),
                         "waiting for a verifier: this server's job host does not verify now")
        # carol's machine marked a verifier: it replicates the provisional machine's sampled results (not the new
        # one's, which this server alone checks), and those matches count
        D.change_trust(carol.sid, verifier=True, actor_id=self.ids["zara"])
        self.drive(d, [carol, newbie, prov],
                   until=lambda: any(r.machine_id == carol.sid and r.role == "replica" and r.verdict == "match"
                                     for r in self.runs()))
        replicated = [r for r in self.runs() if r.role == "replica"]
        self.assertTrue(replicated)
        self.assertTrue(all(self.job_of(r.id).id and r.machine_id == carol.sid for r in replicated))
        self.assertGreaterEqual(prov.row().trust.get("verified", 0), 1, "a match against a verifier counts")

    def test_a_third_new_machine_of_one_owner_waits(self):
        lenders = [Lender(self, f"Bob's new {k}", "bob", seed=TEST_KEY_SEED + k) for k in range(3)]
        self.submit("bob", duels("many", 6))
        host = self.verifier()
        host.auto = False                      # their results wait for their replicas: work in hand
        d = self.make([host])
        for _ in range(4):
            d.tick()
            for lender in lenders:
                for offer in lender.offers():
                    lender.play(offer)
        played = [lender for lender in lenders if lender.runs]
        self.assertEqual(len(played), 2)
        (third,) = [lender for lender in lenders if not lender.runs]
        self.assertIn("its owner's other new machines have work in hand", d._available(third.e, d.clock()))

    def test_one_local_thread_alternates_new_replicas_with_other_work(self):
        """Rule 3: with both a new machine's replicas and other work waiting, this server's one thread takes every
        other run of each (``new_verify_share`` a half)."""
        self.settings(new_verify_share=0.5)
        host = TD.Fake("sv_host", name="this server", threads=1)
        bob = Lender(self, "Bob's new", "bob", threads=3, work=("lab_game",))
        self.submit("bob", duels("theirs", 4))
        self.submit("zara", duels("ours", 8, kind="ranking"))
        d = self.make([host])
        kinds, seen = [], set()
        for _ in range(80):
            d.tick()
            for offer in bob.offers():
                bob.play(offer)
            for run, job, _dl in list(host.offers):
                if run not in seen:
                    seen.add(run)
                    kinds.append("replica" if job.state == "settling" else "primary")
                    host.play(run)
            if len(kinds) >= 7:
                break
        # its first run came before any replica waited; from the second on, both waited
        self.assertEqual(kinds[1:7], ["primary", "replica"] * 3, kinds)

    def test_a_quarantined_verifier_reopens_its_matches_and_takes_back_a_promotion(self):
        self.settings(provisional_rate=1.0, new_streak=2)
        verifier = Lender(self, "Carol's verifier", "carol", tier="trusted", seed=TEST_KEY_SEED + 5)
        D.change_trust(verifier.sid, verifier=True, actor_id=self.ids["zara"])
        prov = Lender(self, "Bob's provisional", "bob", tier="provisional")
        with db.session() as s:
            row = s.get(Server, prov.sid)
            row.trust = {"verified": 1, "streak": 1}
        self.submit("bob", duels("vouched", 2))
        host = self.verifier()
        host.only_verifies = True
        host.state = "off"                     # this server verifies nothing now: carol's machine does
        d = self.make([host])
        self.drive(d, [prov, verifier], until=lambda: len([r for r in self.runs()
                                                           if r.role == "replica" and r.verdict == "match"]) >= 2)
        vouched = [r for r in self.runs() if r.role == "replica" and r.verdict == "match"]
        self.assertEqual({r.machine_id for r in vouched}, {verifier.sid})
        jobs = {r.job_id for r in vouched}
        lines = lab.load_results("vouched")
        self.assertEqual({x["executor"]["verification"] for x in lines}, {"verified"})
        self.assertEqual((prov.row().trust_tier, prov.row().trust["streak"]), ("provisional", 3))
        # the verifier quarantined by an administrator: its matches re-opened, the provisional machine's taken back
        out = D.change_trust(verifier.sid, tier="quarantined", actor_id=self.ids["zara"])
        self.assertEqual(out["tier"], "quarantined")
        d.tick()
        with db.session() as s:
            states = {s.get(CpuJob, j).state for j in jobs}
        self.assertEqual(states, {"settling"})
        lines = lab.load_results("vouched")
        self.assertEqual({x["executor"]["verification"] for x in lines}, {"unverified"})
        self.led.drain()
        primaries = [self.job_of(r.id).accepted_run for r in vouched]
        self.assertEqual({self.event(p).verification for p in primaries}, {"unverified"})
        self.assertEqual(prov.row().trust_tier, "new", "below its tier's threshold: back to new")
        # a fresh replica on this server re-verifies them
        host.state = "ready"
        self.drive(d, [prov], until=lambda: (lab.DONE / "vouched.json").exists())
        lines = lab.load_results("vouched")
        self.assertEqual({x["executor"]["verification"] for x in lines}, {"verified"})
        fresh = [r for r in self.runs() if r.role == "replica" and r.machine_id == "sv_host"]
        self.assertEqual({r.job_id for r in fresh}, jobs)


class Tiebreak(Verify):
    """Rule 5: a remote verifier that disagrees with a primary asks this server for a tiebreak, and every machine that
    disagrees with it is quarantined (a verifier can be wrong too)."""

    def scenario(self, cheat: str) -> tuple:
        self.settings(provisional_rate=1.0)
        prov = Lender(self, "Bob's provisional", "bob", tier="provisional")
        verifier = Lender(self, "Carol's verifier", "carol", tier="trusted", seed=TEST_KEY_SEED + 5)
        D.change_trust(verifier.sid, verifier=True, actor_id=self.ids["zara"])
        self.submit("bob", duels("tied", 1))
        host = self.verifier()
        host.state = "off"                     # its replica goes to carol's verifier
        d = self.make([host])
        cheater = prov if cheat == "primary" else verifier
        self.drive(d, [prov, verifier], fault=lambda lender, n: "result" if lender is cheater else "",
                   until=lambda: any(r.role == "replica" and r.state == "done" for r in self.runs()))
        (replica,) = [r for r in self.runs() if r.role == "replica"]
        self.assertEqual((replica.machine_id, replica.verdict), (verifier.sid, "mismatch"))
        self.assertEqual(d.jobs[replica.job_id].need, "tiebreak")
        self.assertEqual([r for r in self.runs() if r.role == "tiebreak"], [], "this server breaks it, once it can")
        host.state = "ready"
        self.drive(d, [prov, verifier], until=lambda: any(r.role == "tiebreak" and r.state == "done"
                                                          for r in self.runs()) and replica.job_id not in d.jobs)
        (tie,) = [r for r in self.runs() if r.role == "tiebreak"]
        self.assertEqual((tie.machine_id, tie.verdict), ("sv_host", "reference"))
        return prov, verifier, replica

    def test_a_verifier_that_disagrees_with_this_servers_tiebreak_is_quarantined(self):
        prov, verifier, replica = self.scenario("verifier")
        self.assertEqual(verifier.row().trust_tier, "quarantined")
        self.assertEqual(verifier.row().trust["last_mismatch"]["against"], "this server (a tiebreak)")
        self.assertEqual(prov.row().trust_tier, "provisional")
        self.assertEqual(prov.row().trust["verified"], 1, "its match against this server's tiebreak counts")
        job = self.job_of(replica.id)
        self.assertEqual(job.state, "accepted")
        (line,) = lab.load_results("tied")
        self.assertEqual(line["executor"]["verification"], "verified")
        self.assertEqual(self.event(replica.id).verification, "rejected", "the wrong replica earns nothing")

    def test_a_primary_that_disagrees_with_the_tiebreak_is_quarantined_and_its_verifier_kept(self):
        prov, verifier, replica = self.scenario("primary")
        self.assertEqual(prov.row().trust_tier, "quarantined")
        self.assertEqual(verifier.row().trust_tier, "trusted")
        self.assertEqual(self.event(replica.id).verification, "verified", "the right replica")
        self.assertEqual(self.job_of(replica.id).state, "withdrawn")
        self.assertEqual(lab.load_results("tied"), [], "played again elsewhere")


class Restart(Verify):
    """A job waiting for its replica when the server stops waits for it again after the start (its primary's
    ``replicating`` mark says what it waits for), and a re-opened job keeps its line."""

    def test_a_settling_job_is_settled_after_a_restart(self):
        bob = Lender(self, "Bob's new", "bob")
        self.submit("bob", duels("restarted", 1))
        host = self.verifier()
        host.auto = False
        d = self.make([host])
        self.drive(d, [bob], until=lambda: bool(host.offers))
        (run, job, _dl) = host.offers[0]
        self.assertEqual((job.state, job.need, job.local_only, job.of_new), ("settling", "replica", True, True))
        d.stop()                                # the server stops: its replica run goes with it
        D.install(None)
        again = self.verifier()
        d2 = self.make([again])
        self.drive(d2, [bob], until=lambda: (lab.DONE / "restarted.json").exists())
        (line,) = lab.load_results("restarted")
        self.assertEqual(line["executor"]["verification"], "verified")
        roles = [(r.role, r.state, r.machine_id) for r in self.runs()]
        self.assertIn(("replica", "aborted", "sv_host"), roles, "the one in flight at the stop")
        self.assertIn(("replica", "done", "sv_host"), roles, "a fresh one after the start")


class Promotion(Verify):
    """Gate 6: an honest helper climbs new -> provisional -> trusted, an audit row each."""

    def test_an_honest_helper_is_promoted_with_lowered_thresholds(self):
        self.settings(new_streak=3, promote_verified=6, provisional_rate=1.0)
        bob = Lender(self, "Bob's honest", "bob")
        self.submit("bob", duels("climb", 8))
        d = self.make([self.verifier()])
        tiers = []
        self.assertTrue(self.drive(d, [bob], until=lambda: tiers.append(bob.row().trust_tier) or
                                   bob.row().trust_tier == "trusted"))
        self.assertEqual(bob.row().trust["verified"], 6)
        changes = [a["changed"] for a in self.audits("server.trust", bob.sid)]
        self.assertEqual(changes, [{"from": "new", "to": "provisional"}, {"from": "provisional", "to": "trusted"}])
        self.assertEqual({a["actor"] for a in self.audits("server.trust", bob.sid)}, {"system"})
        self.assertEqual(tiers[0], "new")


class QuarantineAndRelease(Verify):
    """Gate 7: a quarantined machine serves nobody but its owner; a release returns it to new with ``since`` reset;
    a key reset keeps its tier."""

    def test_admission_refuses_a_quarantined_machine_to_another_member_and_serves_its_owner(self):
        from citar.pool import admission
        bob = Lender(self, "Bob's", "bob", tier="trusted")
        with db.session() as s:
            from citar import pool
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, bob.group_id),
                              subject_type="user", subject_handle="alice", purposes=["pooled"])
        with db.session() as s:
            server = s.get(Server, bob.sid)
            server.capabilities = {**server.capabilities, "llm": {"enabled": True}}
            self.assertTrue(admission.check(s, s.get(User, self.ids["alice"]), server, "pooled").allowed)
        D.change_trust(bob.sid, tier="quarantined", actor_id=self.ids["zara"])
        with db.session() as s:
            server = s.get(Server, bob.sid)
            d = admission.check(s, s.get(User, self.ids["alice"]), server, "pooled")
            self.assertEqual((d.allowed, d.code), (False, "quarantined"))
            self.assertEqual(admission.check(s, s.get(User, self.ids["zara"]), server, "pooled").code, "quarantined",
                             "an administrator is not its owner")
            self.assertTrue(admission.check(s, s.get(User, self.ids["bob"]), server, "pooled").allowed,
                            "its owner's own use")
        # released: new, its counters reset and since now
        out = D.change_trust(bob.sid, tier="new", actor_id=self.ids["zara"])
        self.assertEqual(out["tier"], "new")
        row = bob.row()
        self.assertEqual((row.trust["verified"], row.trust["mismatches"], row.trust["verifier"]), (0, 0, False))
        self.assertIsNotNone(T.since(row.trust))
        with db.session() as s:
            self.assertTrue(admission.check(s, s.get(User, self.ids["alice"]), s.get(Server, bob.sid),
                                            "pooled").allowed)

    def test_a_pooled_ticket_from_another_member_is_refused_through_the_route(self):
        from fastapi.testclient import TestClient
        from citar import pool
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        bob = Lender(self, "Bob's quarantined", "bob", tier="trusted")
        with db.session() as s:
            pool.create_grant(s, s.get(User, self.ids["bob"]), s.get(ServerGroup, bob.group_id),
                              subject_type="user", subject_handle="alice", purposes=[])
            row = s.get(Server, bob.sid)
            row.capabilities = {**row.capabilities, "llm": {"enabled": True}}
        D.change_trust(bob.sid, tier="quarantined", actor_id=self.ids["zara"])
        r = client.post(f"/api/pool/servers/{bob.sid}/test", headers=self.headers["alice"])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((r.json()["stage"], r.json()["code"]), ("admission", "quarantined"))
        mine = client.post(f"/api/pool/servers/{bob.sid}/test", headers=self.headers["bob"]).json()
        self.assertNotEqual(mine.get("code"), "quarantined", "its owner's own test passes admission")

    def test_a_key_reset_keeps_the_tier_and_the_counters(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        bob = Lender(self, "Bob's provisional", "bob", tier="provisional")
        with db.session() as s:
            s.get(Server, bob.sid).trust = {"verified": 7, "streak": 7}
        r = client.post(f"/api/machines/{bob.sid}/key/reset", headers=self.headers["bob"])
        self.assertEqual(r.status_code, 200, r.text)
        row = bob.row()
        self.assertEqual((row.trust_tier, row.trust["verified"], row.public_key), ("provisional", 7, None))


class Routes(Verify):
    """Gate 8's access: the trust and replicate routes are administrators' (a member and a moderator "not found", a
    visitor 401), and their bounds."""

    def test_access_and_bounds(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        bob = Lender(self, "Bob's", "bob", tier="trusted")
        trust_path, rep_path = f"/api/admin/machines/{bob.sid}/trust", f"/api/admin/machines/{bob.sid}/replicate"
        anon = {"origin": "https://citar.test"}
        for path, method, body in ((trust_path, "PUT", {"verifier": True}), (rep_path, "POST", {"n": 3})):
            self.assertEqual(client.request(method, path, json=body, headers=anon).status_code, 401)
            for who in ("bob", "alice", "mona"):
                self.assertEqual(client.request(method, path, json=body, headers=self.headers[who]).status_code,
                                 404, (path, who))
            self.assertEqual(client.request(method, path, json=body, headers=self.headers["zara"]).status_code, 200,
                             path)
        self.assertTrue(bob.row().trust["verifier"])
        self.assertEqual(bob.row().trust["replicate"], 3)
        bad = [(trust_path, "PUT", {}), (trust_path, "PUT", {"trust_tier": "royal"}),
               (trust_path, "PUT", {"withdraw": True}), (trust_path, "PUT", {"trust_tier": "new", "verifier": True}),
               (trust_path, "PUT", {"verifier": "yes"}), (trust_path, "PUT", {"x": 1}),
               (rep_path, "POST", {"n": 0}), (rep_path, "POST", {"n": 101}), (rep_path, "POST", {"n": True}),
               (rep_path, "POST", {"count": 3})]
        for path, method, body in bad:
            self.assertEqual(client.request(method, path, json=body, headers=self.headers["zara"]).status_code, 400,
                             body)
        self.assertEqual(client.put("/api/admin/machines/sv_nobody/trust", json={"verifier": False},
                                    headers=self.headers["zara"]).status_code, 404)
        # a quarantine by hand with its withdrawal
        r = client.put(trust_path, json={"trust_tier": "quarantined", "withdraw": True}, headers=self.headers["zara"])
        self.assertEqual((r.status_code, bob.row().trust_tier), (200, "quarantined"))
        self.assertFalse(bob.row().trust["verifier"], "a quarantined machine verifies nobody")
        self.assertEqual(self.audits("server.trust", bob.sid)[-1]["actor"], "zara (admin)")


if __name__ == "__main__":
    unittest.main()
