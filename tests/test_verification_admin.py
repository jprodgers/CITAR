"""The verification routes for administrators (design/phase4-helper.md P4.5.10; package 4-12's gate 3) and what they
show, with the dispatcher, its local verifier and helpers in this process (``tests.test_verification``'s pieces).

- ``GET /api/admin/verification`` and ``PUT /api/admin/verification``: a visitor 401, a member (a machine's owner
  too), a moderator "not found", an administrator 200; the settings' bounds (rates and the share 0 to 1, counts 1 to
  10,000: an unknown name, a value past a bound, a fraction for a count, a boolean or a string, an empty body, and
  numbers no float holds (infinity, a 400-digit count, not a number), 400 and nothing changed), one audit row naming
  what changed, the dispatcher reading them at once.
- What it shows once a cheat is caught: the quarantined machine with its evidence and withdrawn jobs, its quarantined
  owner with why, the mismatch, the withdrawal, the verifiers (this server's job host and a machine an administrator
  marked), the machines the credits' bounds flagged.
- ``GET /api/admin/jobs``' verifiers and the caps on ``new`` machines; ``GET /api/machines/{id}/jobs``' flags and
  measured speed for administrators; the Servers page's card's trust for a machine's owner and an administrator;
  a tick that holds its lock never holds ``GET /api/admin/jobs`` (the caps are the last tick's, the queued jobs' wait
  bounded: 503).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import os
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from sqlalchemy import select

import tests.test_dispatcher as TD
import tests.test_verification as TV
from citar import db, ledger, servers, settings
from citar.db.models import AuditLog
from citar.jobs import dispatcher as D, trust as T
from tests import has_test_ops

duels = TD.duels
Lender = TV.Lender

_TMP = Path(tempfile.mkdtemp(prefix="citar_verification_admin_"))
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


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (the throwaway keys)")
class AdminVerification(TV.Verify):

    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)

    def call(self, who, method: str, path: str, **kw):
        headers = self.headers[who] if who else {"origin": "https://citar.test"}
        return self.client.request(method, path, headers=headers, **kw)

    def test_the_routes_are_administrators(self):
        Lender(self, "Bob's", "bob")
        for method, path, body in (("GET", "/api/admin/verification", None),
                                   ("PUT", "/api/admin/verification", {"trusted_rate": 0.05})):
            kw = {"json": body} if body is not None else {}
            self.assertEqual(self.call(None, method, path, **kw).status_code, 401, (method, path))
            for who in ("bob", "alice", "mona"):
                self.assertEqual(self.call(who, method, path, **kw).status_code, 404, (method, path, who))
            self.assertEqual(self.call("zara", method, path, **kw).status_code, 200, (method, path))
        self.assertEqual(T.settings(fresh=True)["trusted_rate"], 0.05, "only the administrator's change was made")

    def test_the_settings_and_their_bounds(self):
        got = self.call("zara", "GET", "/api/admin/verification").json()
        self.assertEqual(got["defaults"], T.DEFAULTS)
        self.assertEqual(got["bounds"]["trusted_rate"], [0.0, 1.0])
        self.assertEqual(got["bounds"]["new_streak"], [1, 10_000])
        self.assertEqual(got["settings"]["trusted_rate"], 0.0, "the suite's Lab pins the rates at 0")
        bad = [{}, {"x": 1}, {"trusted_rate": 1.5}, {"trusted_rate": -0.01}, {"new_verify_share": 2},
               {"new_streak": 0}, {"new_streak": 10_001}, {"new_streak": 2.5}, {"promote_verified": True},
               {"provisional_rate": "0.1"}, {"overreport_days": None}, {"trusted_rate": 0.1, "nope": 1}]
        before = T.settings(fresh=True)
        for body in bad:
            self.assertEqual(self.call("zara", "PUT", "/api/admin/verification", json=body).status_code, 400, body)
        # numbers JSON reads as no float holds them (4-12's review: these answered 500): infinity, a 400-digit count,
        # not a number, a count past every float's whole numbers
        raw = ['{"new_streak": 1e999}', '{"new_streak": -1e999}', '{"new_streak": 1' + "0" * 400 + "}",
               '{"promote_verified": NaN}', '{"trusted_rate": Infinity}', '{"trusted_rate": 1' + "0" * 400 + "}",
               '{"overreport_days": 1e300}', '{"new_per_owner": 4.000000000000001}']
        for body in raw:
            r = self.client.put("/api/admin/verification", content=body,
                                headers={**self.headers["zara"], "content-type": "application/json"})
            self.assertEqual(r.status_code, 400, (body[:40], r.text[:200]))
            self.assertLess(len(r.text), 200, "the refusal does not quote a huge number")
        self.assertEqual(self.call("zara", "PUT", "/api/admin/verification", json=[1, 2]).status_code, 422,
                         "not an object at all")
        self.assertEqual(T.settings(fresh=True), before, "a refused change changes nothing")
        edges = {"trusted_rate": 1.0, "provisional_rate": 0.0, "new_verify_share": 1.0, "new_streak": 10_000,
                 "promote_verified": 1, "overreport_days": 1, "new_per_owner": 10_000}
        r = self.call("zara", "PUT", "/api/admin/verification", json=edges)
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual({k: r.json()["settings"][k] for k in edges}, edges)
        self.assertEqual({k: T.settings(fresh=True)[k] for k in edges}, edges, "in force at once")
        with db.session() as s:
            rows = list(s.scalars(select(AuditLog).where(AuditLog.action == "verification.settings")))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0].detail["changed"]["new_streak"], {"from": 5, "to": 10_000})
        self.assertEqual(rows[0].actor_label, "zara (admin)")
        # the same again changes nothing and writes no audit row
        self.assertEqual(self.call("zara", "PUT", "/api/admin/verification", json=edges).status_code, 200)
        with db.session() as s:
            self.assertEqual(len(list(s.scalars(select(AuditLog).where(
                AuditLog.action == "verification.settings")))), 1)

    def test_what_it_shows_once_a_cheat_is_caught(self):
        verifier = Lender(self, "Carol's verifier", "carol", tier="trusted", seed=TV.TEST_KEY_SEED + 5)
        D.change_trust(verifier.sid, verifier=True, actor_id=self.ids["zara"])
        bob = Lender(self, "Bob's cheat", "bob", seed=TV.TEST_KEY_SEED + 9)
        self.submit("bob", duels("caught", 2))
        d = self.make([self.verifier()])
        self.assertTrue(self.drive(d, [bob], fault=lambda lender, n: "result",
                                   until=lambda: bob.row().trust_tier == "quarantined"))
        ((_n, run, job),) = bob.runs
        got = self.call("zara", "GET", "/api/admin/verification").json()
        (q,) = got["quarantined"]["machines"]
        self.assertEqual((q["machine"], q["name"], q["owner"]["handle"]), (bob.sid, "Bob's cheat", "bob"))
        self.assertEqual(q["evidence"]["words"], "same chain, different result: tampering or an incompatible build")
        self.assertEqual([w["run"] for w in q["evidence"]["withdrawn_jobs"]], [run])
        (owner,) = got["quarantined"]["owners"]
        self.assertEqual((owner["handle"], owner["why"], owner["run"]),
                         ("bob", "same chain, different result: tampering or an incompatible build", run))
        (m,) = got["mismatches"]
        self.assertEqual((m["run"], m["machine"], m["exp"], m["i"], m["verdict"], m["against"]),
                         (run, bob.sid, "caught", 0, "result", "this server"))
        (w,) = got["withdrawals"]
        self.assertEqual((w["job"], w["run"], w["machine"], w["exp"], w["i"]), (job, run, bob.sid, "caught", 0))
        # the Lab page counts the experiment's withdrawn game (its owner sees it played again, not the verdict)
        (exp,) = [e for e in self.call("bob", "GET", "/api/lab").json()["experiments"] if e["name"] == "caught"]
        self.assertEqual(exp["withdrawn"], 1)
        kinds = {(v["kind"], v["machine"]) for v in got["verifiers"]}
        self.assertEqual(kinds, {("local", "sv_host"), ("remote", verifier.sid)})
        self.assertEqual(got["tiers"].get("quarantined"), 1)

    def test_a_tick_holding_its_lock_never_holds_the_administrators_request(self):
        """4-12's review (P3.9.2: every wait has a timeout): ``GET /api/admin/jobs`` read the caps under the tick's lock
        with no timeout. The caps are the last tick's figures now, and the queued jobs' wait is bounded: busy, 503."""
        Lender(self, "Bob's", "bob")
        self.submit("bob", duels("busy", 1))
        d = self.make([self.verifier()])
        d.tick()
        held, release = threading.Event(), threading.Event()

        def tick_stuck():
            with d._tick_lock:
                held.set()
                release.wait(10)
        t = threading.Thread(target=tick_stuck, daemon=True)
        t.start()
        self.addCleanup(t.join, 10)
        self.addCleanup(release.set)
        self.assertTrue(held.wait(5))
        t0 = time.monotonic()
        self.assertIn("in_hand", d.hands_view(), "the last tick's figures, at once")
        with mock.patch.object(D, "VIEW_WAIT_SECONDS", 0.2):
            r = self.call("zara", "GET", "/api/admin/jobs")
        self.assertEqual(r.status_code, 503, r.text)
        self.assertIn("busy", r.json()["detail"])
        self.assertLess(time.monotonic() - t0, 5.0)
        release.set()
        t.join(10)
        self.assertEqual(self.call("zara", "GET", "/api/admin/jobs").status_code, 200)

    def test_the_dispatchers_verifiers_and_caps_and_a_machines_flags(self):
        self.settings(new_per_owner=1)
        bob = Lender(self, "Bob's one", "bob")
        bob2 = Lender(self, "Bob's two", "bob", seed=TV.TEST_KEY_SEED + 4)
        self.submit("bob", duels("capped", 4))
        host = self.verifier()
        host.state = "off"          # replicas wait: the new machines' work stays in hand
        d = self.make([host])
        self.assertTrue(self.drive(d, [bob, bob2], until=lambda: bool(bob.runs or bob2.runs)))
        d.tick()
        jobs = self.call("zara", "GET", "/api/admin/jobs").json()
        caps = jobs["caps"]
        self.assertEqual(caps["new_per_owner"], 1)
        by = {m["machine"]: m for m in caps["new_machines"]}
        self.assertEqual(set(by), {bob.sid, bob2.sid})
        busy = bob if bob.runs else bob2
        idle = bob2 if busy is bob else bob
        self.assertEqual(by[busy.sid]["in_hand"], 1)
        self.assertTrue(by[busy.sid]["held"].startswith("waiting for a verifier"), by[busy.sid])
        self.assertTrue(by[idle.sid]["held"].startswith("its owner's other new machines"), by[idle.sid])
        self.assertEqual({v["kind"] for v in jobs["verifiers"]}, {"local"})
        # an administrator's view of a machine: its flags and measured speed (none yet)
        with db.session() as s:
            from citar.db.models import Server
            row = s.get(Server, busy.sid)
            row.flags = {**(row.flags or {}), "capped_cpu": {"count": 2, "last": "2026-10-17T00:00:00+00:00"}}
        mine = self.call("bob", "GET", f"/api/machines/{busy.sid}/jobs").json()
        self.assertNotIn("flags", mine["trust"], "a machine's owner is not shown what an administrator is")
        admin = self.call("zara", "GET", f"/api/machines/{busy.sid}/jobs").json()
        self.assertEqual(admin["trust"]["flags"]["capped_cpu"]["count"], 2)
        self.assertIsNone(admin["trust"]["speed"])
        # the Servers page's card: the tier, verified count and streak to its owner; more to an administrator
        card = next(m for m in self.call("bob", "GET", "/api/pool/servers").json()["servers"] if m["id"] == busy.sid)
        self.assertEqual(card["trust"], {"tier": "new", "verified": 0, "streak": 0, "mismatches": 0})
        card = next(m for m in self.call("zara", "GET", "/api/pool/servers").json()["servers"] if m["id"] == busy.sid)
        self.assertEqual((card["trust"]["tier"], card["trust"]["verifier"]), ("new", False))
        self.assertIn("cpu_class", card["trust"])
        other = next((m for m in self.call("alice", "GET", "/api/pool/servers").json()["servers"] if m["id"] == busy.sid),
                     None)
        self.assertTrue(other is None or "trust" not in other, "nobody else is told its trust")


if __name__ == "__main__":
    unittest.main()
