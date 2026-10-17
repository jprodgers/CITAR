"""Model results by their machine's tier (design/phase4-helper.md P4.5.8 rule 11; package 4-12's gate 2).

A model's results from a member's machine count toward the global leaderboards only when the machine was ``trusted``
when they ran (or is now: trusting it brings its earlier results in); a site machine's and an administrator's always
count. The others carry an "unverified machine" badge and are left out of every board but their owner's own figures.

- **Benchmark games** (dry runs played to their end, then scored as a real model's): one on a ``provisional``
  member's machine is left out of the administrators' board and of another member's, counted in ``unverified``, and
  badged in its owner's; one on an administrator's machine and one on a ``trusted`` member's count; trusting the
  provisional machine brings its game in. The game records its machine's standing (``machines``), in its save too,
  and its benchmark job carries it (the runs' listing badges the job).
- **The diplomacy board** leaves out the pairs of an untrusted machine but its owner's own (marked).
- **Probe runs** started on an untrusted machine carry the badge.
- **The ledger:** a model step's event carries its machine's tier when it ran (``flags.tier``).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import shutil
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from sqlalchemy import select

from citar import aggregate, db, diplomacy_stats, ledger, servers as REG
from citar.auth import accounts
from citar.db.models import Base, Server, User
from citar.jobs import trust as T
from citar.server.benchmarks import BenchmarkScheduler
from citar.server.session import SAVE_DIR, SessionManager

_DIR = Path(tempfile.mkdtemp(prefix="citar_model_tiers_"))
_URL = "sqlite:///" + (_DIR / "test.db").as_posix()


def setUpModule():
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    REG.invalidate()


def tearDownModule():
    db.dispose()
    REG.invalidate()
    shutil.rmtree(_DIR, ignore_errors=True)


def wait_for(pred, sch, timeout=120.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        sch.tick()
        if pred():
            return True
        time.sleep(0.1)
    return False


class Tiers(unittest.TestCase):
    """Zara (an administrator), alice and bob (members); a dry-run machine of each kind the rule names: bob's
    ``provisional`` one, zara's own, alice's ``trusted`` one; each plays one benchmark game of its own model."""

    def setUp(self):
        T.forget_vouched()
        self.addCleanup(T.forget_vouched)
        self.ids = {}
        with db.session() as s:
            for handle, role in (("zara", "admin"), ("alice", "user"), ("bob", "user")):
                u = s.scalar(select(User).where(User.handle == handle))
                if u is None:
                    u = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                             password="a long enough phrase", role=role, status="active",
                                             email_verified=True)
                self.ids[handle] = u.id
        self.machines = {}
        for who, tier in (("bob", "provisional"), ("zara", "new"), ("alice", "trusted")):
            sid = f"sv_tier_{who}"
            with db.session() as s:
                actor = s.get(User, self.ids[who])
                REG.upsert({"id": sid, "name": f"{who}'s box", "kind": "test",
                            "connection": {"provider": "dryrun", "max_parallel": 1},
                            "models": [{"id": f"m_tier_{who}", "key": f"tier-{who}"}]}, actor=actor, session=s)
                s.get(Server, sid).trust_tier = tier
            self.machines[who] = sid
        REG.invalidate()
        self.dir = Path(tempfile.mkdtemp(prefix="citar-tiers-", dir=_DIR))
        self.manager = SessionManager()
        self.addCleanup(self._close)

    def _close(self):
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def play(self, who: str) -> dict:
        """One benchmark job of ``who``'s model on ``who``'s machine, played to its end (3 turns); its game then scored
        as a real model's (a dry run never counts)."""
        sch = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.addCleanup(sch.stop)
        sid = self.machines[who]
        run = sch.create_run({"name": f"{who}'s run", "mode": "sequential",
                              "servers": [{"server_id": sid, "models": [{"model_id": f"m_tier_{who}"}]}],
                              "scenarios": [{"name": "Tiny duel", "map_size": "duel", "map_type": "pangaea",
                                             "seed": 5, "opponents": 1, "turn_limit": 3, "barbarians": "off",
                                             "max_turn_minutes": 5}]}, owner_id=self.ids[who])
        self.assertTrue(wait_for(lambda: run["status"] == "done", sch), [j.get("error") for j in run["jobs"]])
        job = run["jobs"][0]
        self.manager.get(job["game_id"]).seats[0].llm["provider"] = "openai_compatible"
        return job

    def scores(self, scope=None) -> dict:
        from citar.server.scoring import model_scores
        return model_scores(self.manager, {"benchmark": 0.6, "reliability": 0.25, "speed": 0.15}, scope=scope)

    def scope(self, viewer: str, own: set, games: set) -> aggregate.Scope:
        owner_of = dict(games)
        return aggregate.Scope(viewer=None, game_ids={g for g, _ in games}, own_ids=set(own),
                               pooled_ids={g for g, _ in games} - set(own),
                               contributors={v for _, v in games}, owner_of=owner_of)

    def test_benchmark_results_count_by_their_machines_tier(self):
        jobs = {who: self.play(who) for who in ("bob", "zara", "alice")}
        games = {who: j["game_id"] for who, j in jobs.items()}
        models = {who: j["model"] for who, j in jobs.items()}
        # the game and its job record its machine's standing when it ran
        bob_game = self.manager.get(games["bob"])
        self.assertEqual(bob_game.machines[self.machines["bob"]], {"tier": "provisional", "vouched": False})
        self.assertEqual(jobs["bob"]["standing"], {"tier": "provisional", "vouched": False})
        self.assertEqual(jobs["zara"]["standing"], {"tier": "new", "vouched": True}, "an administrator's machine")
        self.assertEqual(jobs["alice"]["standing"], {"tier": "trusted", "vouched": True})
        from citar.server.scoring import _digest_save
        saved = _digest_save(bob_game.save("tiers"))
        self.assertEqual(saved["unvouched"], {0: self.machines["bob"]}, "its save keeps the record")
        # the administrators' board: bob's game left out and counted, the others in
        out = self.scores()
        rows = {r["model"]: r for r in out["models"]}
        self.assertNotIn(models["bob"], rows)
        self.assertEqual(rows[models["zara"]]["benchmark_games"], 1)
        self.assertEqual(rows[models["alice"]]["benchmark_games"], 1)
        # (other modules' saved games share the suite's saves folder: only this test's models are looked at)
        left = {u["model"]: u["games"] for u in out["unverified"]}
        self.assertEqual({m: left.get(m) for m in models.values()}, {models["bob"]: 1, models["zara"]: None,
                                                                    models["alice"]: None})
        # another member's board leaves it out too (enough contributors to publish the rest)
        everyone = {(games[w], self.ids[w]) for w in games}
        with mock.patch.object(aggregate, "min_contributors", return_value=1):
            out = self.scores(self.scope("alice", {games["alice"]}, everyone))
            rows = {r["model"]: r for r in out["models"]}
            self.assertNotIn(models["bob"], rows)
            self.assertIn(models["zara"], rows)
            self.assertEqual([u["model"] for u in out["unverified"]], [models["bob"]], "its scope: these games")
            # its owner's own figures keep it, badged
            out = self.scores(self.scope("bob", {games["bob"]}, everyone))
            rows = {r["model"]: r for r in out["models"]}
            (game,) = rows[models["bob"]]["games"]
            self.assertTrue(game["unverified_machine"])
            self.assertEqual(out["unverified"], [])
        # an administrator trusts bob's machine: its earlier game now counts
        with db.session() as s:
            s.get(Server, self.machines["bob"]).trust_tier = "trusted"
        T.forget_vouched()
        out = self.scores()
        rows = {r["model"]: r for r in out["models"]}
        self.assertEqual(rows[models["bob"]]["benchmark_games"], 1)
        self.assertNotIn(models["bob"], {u["model"] for u in out["unverified"]})

    def test_the_runs_listing_and_probe_runs_carry_the_badge(self):
        from citar.server.app import _badge_benchmark_runs, _probe_unverified
        bob, alice = self.machines["bob"], self.machines["alice"]
        runs = [{"jobs": [{"server_id": bob, "standing": {"tier": "provisional", "vouched": False}}]},
                {"jobs": [{"server_id": alice, "standing": {"tier": "trusted", "vouched": True}}]},
                {"jobs": [{"server_id": bob}]}]                      # a job from before rule 11: no standing
        _badge_benchmark_runs(runs)
        self.assertEqual([r.get("unverified_machine") for r in runs], [True, None, None])
        probe = {"llm": {"server_id": bob}, "standing": {"tier": "provisional", "vouched": False}}
        self.assertTrue(_probe_unverified(probe, T.not_counted([(bob, probe["standing"])])))
        with db.session() as s:
            s.get(Server, bob).trust_tier = "trusted"
        T.forget_vouched()
        self.assertFalse(_probe_unverified(probe, T.not_counted([(bob, probe["standing"])])), "trusted since")

    def test_the_runs_route_badges_copies_and_drops_the_badge_once_the_machine_is_trusted(self):
        """4-12's review: the listing once badged the scheduler's own job dicts, so the badge was saved to the run's
        file and outlived the machine's trusting. Through ``GET /api/benchmarks/runs``: listed, trusted, listed again;
        the scheduler's job never carries the key, and a key a run's file kept is not believed."""
        from fastapi.testclient import TestClient
        from citar.auth import sessions
        from citar.server import app as appmod
        sch = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.addCleanup(sch.stop)
        sid = self.machines["bob"]
        with db.session() as s:
            bob = s.get(User, self.ids["bob"])
            row, raw = sessions.start(s, bob)
            headers = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token,
                       "origin": "https://citar.test"}
            run = sch.create_run({"name": "bob's listed run", "mode": "sequential",
                                  "servers": [{"server_id": sid, "models": [{"model_id": "m_tier_bob"}]}],
                                  "scenarios": [{"name": "Tiny duel", "map_size": "duel", "map_type": "pangaea",
                                                 "seed": 5, "opponents": 1, "turn_limit": 3, "barbarians": "off",
                                                 "max_turn_minutes": 5}]}, owner_id=bob.id, session=s)
        self.assertTrue(wait_for(lambda: run["status"] == "done", sch), [j.get("error") for j in run["jobs"]])
        client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(client.close)
        live = sch.runs[run["id"]]["jobs"][0]

        def listed() -> tuple:
            with mock.patch.object(appmod, "_scheduler", return_value=sch):
                r = client.get("/api/benchmarks/runs", headers=headers)
            self.assertEqual(r.status_code, 200, r.text)
            (mine,) = [x for x in r.json() if x["id"] == run["id"]]
            return mine.get("unverified_machine"), mine["jobs"][0].get("unverified_machine")
        self.assertEqual(listed(), (True, True), "provisional when it ran: badged")
        self.assertNotIn("unverified_machine", live, "the scheduler's own job is never marked")
        with db.session() as s:
            s.get(Server, sid).trust_tier = "trusted"
        T.forget_vouched()
        self.assertEqual(listed(), (None, None), "trusted since: neither the run nor its job is badged")
        live["unverified_machine"] = True           # as a run's file written before the fix keeps it
        self.assertEqual(listed(), (None, None))

    def test_the_diplomacy_board_leaves_out_an_untrusted_machines_pairs(self):
        bob, zara = self.machines["bob"], self.machines["zara"]
        line = {"score_diff": {"end": 5.0}, "deals": 1, "proposals_made": 1, "accepted_own": 1}

        def pair(gid, sid, vouched, owner):
            return {"model": "dip", "owners": None, "line": dict(line), "fallbacks": {}, "game_id": gid,
                    "run": "r", "run_id": "r", "job_id": gid, "scenario": "s", "seed": 1, "control": None,
                    "server_id": sid, "standing": {"tier": "x", "vouched": vouched}, "owner_id": owner}
        pairs = [pair("g1", bob, False, self.ids["bob"]), pair("g2", zara, True, self.ids["zara"])]
        out = diplomacy_stats.board(list(pairs))
        (only,) = out["lines"]
        self.assertEqual(only["pairs"], 1, "the administrators' board: bob's pair left out")
        self.assertEqual([(u["model"], u["pairs"]) for u in out["unverified"]], [("dip", 1)])
        scope = aggregate.Scope(viewer=None, game_ids={"g1", "g2"}, own_ids={"g1"}, pooled_ids={"g2"},
                                contributors={self.ids["bob"], self.ids["zara"]},
                                owner_of={"g1": self.ids["bob"], "g2": self.ids["zara"]})
        with mock.patch.object(aggregate, "min_contributors", return_value=1):
            out = diplomacy_stats.board(list(pairs), scope)
        (only,) = out["lines"]
        self.assertEqual(only["pairs"], 2, "its owner's own pair counts in their figures")
        self.assertEqual([g.get("unverified_machine") for g in only["games"]], [True])

    def test_a_model_steps_event_carries_its_machines_tier(self):
        led = ledger.Ledger(background=False)
        ev = SimpleNamespace(machine=self.machines["bob"], model="tier-bob", usage={}, gen_s=1.0, flags={},
                             at=time.time())
        self.assertEqual(led.price(ev)["flags"]["tier"], "provisional")
        ev.machine = self.machines["alice"]
        self.assertEqual(led.price(ev)["flags"]["tier"], "trusted")


if __name__ == "__main__":
    unittest.main()
