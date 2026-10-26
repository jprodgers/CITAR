"""The diplomacy leaderboard (design/phase3-features.md P3.6.10; package 3-16's gate 3).

- ``Lines``: pair lines (``pair_line``) and the per-model aggregates (``leaderboard``: the mean score difference, the
  90% bootstrap interval with its fixed seed, deals per game, the mean deal value, the acceptance rate, wars, captures
  and losses) on fixture results equal hand-computed values; fallbacks a benchmark's queue or a refusal caused are not
  counted against the model (``fallbacks_of``).
- ``Route``: ``GET /api/models/diplomacy`` for members, an administrator and an anonymous caller: a run the caller
  cannot see is left out, another member's pooled pairs are counted, never named, and withheld under the
  minimum-contributor rule, an account that opted out of pooling feeds nobody else's lines.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import os
import random
import statistics
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_diplomacy_board_"))
DB_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = DB_URL
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, diplomacy_stats as D, items
from citar.auth import accounts, policy, sessions
from citar.db.models import Base, Game as GameRow, User


def arm(deals=0, total=0.0, valued=None, made=0, accepted=0, wars=0, captures=0, losses=0) -> dict:
    """One arm's figures as a pair line carries them."""
    valued = deals if valued is None else valued
    return {"deals": deals, "deals_valued": valued, "deal_value_total": total,
            "deal_value_mean": round(total / valued, 2) if valued else None, "proposals_made": made,
            "accepted_own": accepted, "acceptance_rate": round(accepted / made, 4) if made else None,
            "wars_declared": wars, "captures": captures, "losses": losses}


def pair(model, owners, diff, hybrid: dict, control_arm: dict, fallbacks=None, **more) -> dict:
    line = {"score_diff": {"end": diff}, **hybrid, "wars": [], "control": control_arm}
    return {"model": model, "owners": owners, "line": line, "fallbacks": fallbacks or {"model": 0, "other": {}},
            **more}


def bootstrap(values, seed=D.BOOTSTRAP_SEED, resamples=D.BOOTSTRAP_RESAMPLES):
    """The interval written out again from its definition: resamples of n draws by randrange, their means sorted, the
    5th and 95th percentile positions of the 2,000."""
    rng = random.Random(seed)
    n = len(values)
    means = sorted(statistics.fmean([values[rng.randrange(n)] for _ in range(n)]) for _ in range(resamples))
    return [round(means[100], 2), round(means[1899], 2)]


class Lines(unittest.TestCase):
    def test_pair_lines_take_hybrid_minus_control_where_both_games_reached_the_turn(self):
        hybrid = {**arm(deals=3, total=12.0, made=4, accepted=3), "wars": [{"with": 2}], "wars_declared_on": 0,
                  "peace": 0, "proposals_received": 1,
                  "final": {"score": 180, "won": True, "alive": True, "scores_at": {"100": 50, "200": 90}}}
        control = {**arm(deals=1, total=-4.0, made=2, accepted=1), "wars": [], "wars_declared_on": 1, "peace": 0,
                   "proposals_received": 2,
                   "final": {"score": 150, "won": False, "alive": True, "scores_at": {"100": 40}}}
        line = D.pair_line(hybrid, control)
        self.assertEqual(line["score_diff"], {"end": 30, "100": 10})
        self.assertEqual((line["deals"], line["deal_value_total"], line["deal_value_mean"], line["acceptance_rate"]),
                         (3, 12.0, 4.0, 0.75))
        self.assertEqual((line["won"], line["control"]["won"], line["control"]["deals"]), (True, False, 1))
        self.assertEqual(line["wars"], [{"with": 2}])

    def test_the_lines_equal_hand_computed_values(self):
        pairs = [
            pair("m1", "all_but_war", 10, arm(2, 30.0, made=4, accepted=2, wars=1, captures=1),
                 arm(1, 5.0), {"model": 1, "other": {"queued_behind_benchmark": 2}}),
            pair("m1", "all_but_war", -4, arm(0, 0.0, made=1, accepted=0, losses=2), arm(1, -5.0)),
            pair("m1", "all_but_war", 6, arm(4, -10.0, made=5, accepted=3, wars=2), arm(2, 10.0),
                 {"model": 0, "other": {"refused": 1}}),
            pair("m1", {"trades": "llm"}, 2, arm(1, 7.0, made=1, accepted=1), arm(0, 0.0)),
            pair("m2", "trade_only", 5, arm(1, 3.0, made=2, accepted=1), arm(1, 1.0)),
            pair("m2", "trade_only", 5, arm(3, 6.0, made=3, accepted=2), arm(1, 2.0)),
        ]
        lines = {(x["model"], x["owners"]): x for x in D.leaderboard(pairs)}
        self.assertEqual(set(lines), {("m1", "all_but_war"), ("m1", "custom: trades"), ("m2", "trade_only")})
        m1 = lines[("m1", "all_but_war")]
        self.assertEqual({k: m1[k] for k in ("pairs", "mean_score_diff", "deals_per_game", "mean_deal_value",
                                             "acceptance_rate", "wars_declared_per_game", "captures_per_game",
                                             "losses_per_game", "control_deals_per_game", "control_mean_deal_value",
                                             "fallbacks_per_game", "other_fallbacks")},
                         {"pairs": 3, "mean_score_diff": 4.0, "deals_per_game": 2.0,
                          "mean_deal_value": round(20.0 / 6, 2), "acceptance_rate": 0.5,
                          "wars_declared_per_game": 1.0, "captures_per_game": 0.33, "losses_per_game": 0.67,
                          "control_deals_per_game": 1.33, "control_mean_deal_value": 2.5,
                          "fallbacks_per_game": 0.33,
                          "other_fallbacks": {"queued_behind_benchmark": 2, "refused": 1}})
        self.assertEqual(m1["ci90"], bootstrap([10.0, -4.0, 6.0]))
        lo, hi = m1["ci90"]
        self.assertTrue(-4.0 <= lo <= 4.0 <= hi <= 10.0)
        m2 = lines[("m2", "trade_only")]
        self.assertEqual((m2["mean_score_diff"], m2["ci90"], m2["deals_per_game"], m2["mean_deal_value"],
                          m2["acceptance_rate"]), (5.0, [5.0, 5.0], 2.0, 2.25, 0.6))
        self.assertEqual(D.leaderboard(pairs)[0]["model"], "m2", "best mean score difference first")
        self.assertEqual(D.leaderboard(pairs), D.leaderboard(list(reversed(pairs))), "the same with every read")

    def test_the_bootstrap_interval_is_its_definition_with_its_fixed_seed(self):
        diffs = [3.0, -7.0, 12.0, 0.0, 5.0, -2.0, 9.0, 1.0, -5.0, 4.0]
        ci = D.bootstrap_interval(diffs)
        self.assertEqual(ci, bootstrap(diffs))
        self.assertEqual(ci, D.bootstrap_interval(list(diffs)), "the same every time it is read")
        self.assertNotEqual(ci, D.bootstrap_interval(diffs, coverage=0.8), "a 90% interval, not an 80% one")
        self.assertNotEqual(ci, D.bootstrap_interval(diffs, seed=D.BOOTSTRAP_SEED + 1), "its seed is fixed")
        lo, hi = ci
        self.assertTrue(min(diffs) < lo < statistics.fmean(diffs) < hi < max(diffs), ci)
        line = D.leaderboard([pair("m", "trade_only", d, arm(), arm()) for d in diffs])[0]
        self.assertEqual((line["mean_score_diff"], line["ci90"]), (2.0, ci))
        self.assertIsNone(D.bootstrap_interval([]))

    def test_fallbacks_the_model_did_not_cause_are_not_counted_against_it(self):
        data = {"negotiations": [
            {"player": 1, "outcome": "fallback", "cause": "timeout"},
            {"player": 1, "outcome": "fallback", "cause": "queued_behind_benchmark"},
            {"player": 1, "outcome": "fallback", "cause": "refused"},
            {"player": 1, "outcome": "fallback", "cause": "failed"},
            {"player": 1, "outcome": "fallback", "cause": "not_started"},    # its time ran out before its prompt began
            {"player": 1, "outcome": "answered", "cause": None},
            {"player": 2, "outcome": "fallback", "cause": "timeout"}],
            "diplomat": [{"player": 1, "outcome": "skipped", "cause": "queued_behind_benchmark"},
                         {"player": 1, "outcome": "failed", "cause": "failed"},
                         {"player": 1, "outcome": "skipped", "cause": "timeout"},     # its prompt never started
                         {"player": 1, "outcome": "timeout", "cause": None},          # ran out of its budget: ran
                         {"player": 1, "outcome": "done", "cause": None}]}
        fb = D.fallbacks_of(data, 1)
        self.assertEqual(fb["model"], 3, "two timeouts or failures of its chats and one failed phase")
        self.assertEqual(fb["other"], {"not_started": 2, "queued_behind_benchmark": 2, "refused": 1})
        self.assertEqual((fb["chats"]["not_started"], fb["phases"]["not_started"]), (1, 1))
        line = D.leaderboard([pair("m", "trade_only", 0, arm(), arm(), fb)])[0]
        self.assertEqual((line["fallbacks_per_game"], line["other_fallbacks"]),
                         (3.0, {"not_started": 2, "queued_behind_benchmark": 2, "refused": 1}))


#: The runs: (run id, owner, visibility, game id, model, score difference).
RUNS = [("r_alice", "alice", "private", "g_alice", "m-shared", 10),
        ("r_bob", "bob", "private", "g_bob", "m-shared", -20),
        ("r_carol", "carol", "server", "g_carol", "m-shared", 4),
        ("r_dave", "dave", "server", "g_dave", "m-shared", 6),
        ("r_dave2", "dave", "server", "g_dave2", "m-dave", 1)]


class Route(unittest.TestCase):
    """``GET /api/models/diplomacy`` over made-up pairs (the scheduler's ``pairs`` is a stub that honours its
    ``visible`` as the real one does), with real rows for the runs' visibility and the games' pooling."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        cls.client = TestClient(appmod.app, base_url="https://citar.test")
        pairs = [pair(model, "all_but_war", diff, arm(1, 2.0, made=1, accepted=1), arm(1, 1.0), run_id=rid,
                      run=f"run {rid}", job_id=f"j_{rid}", game_id=gid, scenario="duel", seed=7, control="ctl_x")
                 for rid, _owner, _vis, gid, model, diff in RUNS]
        cls.patch = mock.patch.object(appmod, "_scheduler", lambda: SimpleNamespace(
            pairs=lambda visible=None: [p for p in pairs if visible is None or p["run_id"] in visible]))
        cls.patch.start()

    @classmethod
    def tearDownClass(cls):
        cls.patch.stop()
        cls.client.close()

    def setUp(self):
        db.dispose()
        db.configure(DB_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("carol", "user"), ("dave", "user"),
                                 ("erin", "user"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            for rid, owner, vis, gid, _model, _diff in RUNS:
                items.register(s, "benchmark_run", rid, self.ids[owner], f"run {rid}").visibility = vis
                s.add(GameRow(id=gid, owner_id=self.ids[owner], name=f"game {gid}", kind="benchmark",
                              visibility=vis, data_sharing="pool", status="finished"))

    def tearDown(self):
        policy.invalidate()

    def board(self, who: str) -> dict:
        r = self.client.get("/api/models/diplomacy", headers=self.headers[who])
        self.assertEqual(r.status_code, 200, r.text)
        return {(x["model"], x["owners"]): x for x in r.json()["lines"]} | {"_raw": r.json()}

    def test_an_anonymous_caller_gets_401(self):
        self.assertEqual(self.client.get("/api/models/diplomacy").status_code, 401)

    def test_a_member_sees_their_own_pairs_and_the_pooled_ones_of_runs_they_may_see(self):
        policy.set("aggregate_min_contributors", 2)
        policy.invalidate()
        got = self.board("alice")
        shared = got[("m-shared", "all_but_war")]
        # bob's private run is not alice's to see: left out; carol's and dave's are shared with members and pool
        self.assertEqual(shared["pairs"], 3)
        self.assertEqual(shared["mean_score_diff"], round((10 + 4 + 6) / 3, 2))
        self.assertEqual([g["run_id"] for g in shared["games"]], ["r_alice"], "only her own pair is named")
        self.assertEqual(shared["pooled_pairs"], 2)
        self.assertEqual(got["_raw"]["scope"]["own"], 1)
        self.assertNotIn("r_bob", str(got["_raw"]))
        # dave's own model: one account behind it, under the threshold of two
        self.assertNotIn(("m-dave", "all_but_war"), got)
        self.assertEqual([(w["model"], w["contributors"]) for w in got["_raw"]["withheld"]], [("m-dave", 1)])

    def test_pooled_pairs_under_the_minimum_are_withheld(self):
        got = self.board("alice")                   # the default threshold: three accounts
        shared = got[("m-shared", "all_but_war")]
        self.assertEqual((shared["pairs"], shared["pooled_pairs"]), (1, 0), "carol and dave are two accounts")
        self.assertEqual(sorted((w["model"], w["contributors"]) for w in got["_raw"]["withheld"]),
                         [("m-dave", 1), ("m-shared", 2)])
        erin = self.board("erin")                   # nothing of her own: every pooled line withheld
        self.assertEqual([k for k in erin if k != "_raw"], [])

    def test_an_account_that_opted_out_feeds_nobody_elses_lines(self):
        policy.set("aggregate_min_contributors", 1)
        policy.invalidate()
        with db.session() as s:
            s.get(User, self.ids["dave"]).data_sharing = "private"
        got = self.board("alice")
        self.assertEqual(got[("m-shared", "all_but_war")]["pairs"], 2, "her own and carol's")
        self.assertNotIn(("m-dave", "all_but_war"), got)
        dave = self.board("dave")
        self.assertEqual(dave[("m-dave", "all_but_war")]["games"][0]["run_id"], "r_dave2", "his own are his")

    def test_an_administrator_sees_every_pair_named(self):
        got = self.board("zara")
        shared = got[("m-shared", "all_but_war")]
        self.assertEqual(shared["pairs"], 4)
        self.assertEqual(sorted(g["run_id"] for g in shared["games"]),
                         ["r_alice", "r_bob", "r_carol", "r_dave"])
        self.assertEqual(shared["pooled_pairs"], 0)
        self.assertNotIn("scope", got["_raw"])
        self.assertEqual(got["_raw"]["bootstrap"], {"resamples": 2000, "seed": D.BOOTSTRAP_SEED, "coverage": 0.9})


if __name__ == "__main__":
    unittest.main()
