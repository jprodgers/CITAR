"""Bot parameters, profiles, fingerprints, the lab's use of profiles, and the ratings fit."""
import tests  # noqa: F401  (temporary saves folder; must be imported before citar)
import ast
import json
import unittest
from pathlib import Path
from types import SimpleNamespace

from citar.bots import basic, profiles, ratings


class ParameterTests(unittest.TestCase):
    def test_every_parameter_the_code_reads_is_declared(self):
        """P["x"] / self.p["x"] anywhere in the bot must be a declared parameter, or a profile can't reach it."""
        tree = ast.parse(Path(basic.__file__).read_text(encoding="utf-8"))
        used = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "get" \
                    and isinstance(node.func.value, ast.Attribute) and node.func.value.attr == "p" and node.args \
                    and isinstance(node.args[0], ast.Constant):
                used.add(node.args[0].value)
            if isinstance(node, ast.Subscript) and isinstance(node.slice, ast.Constant) \
                    and isinstance(node.slice.value, str):
                v = node.value
                if (isinstance(v, ast.Name) and v.id == "P") or \
                        (isinstance(v, ast.Attribute) and v.attr == "p" and isinstance(v.value, ast.Name)
                         and v.value.id == "self"):
                    used.add(node.slice.value)
        missing = used - set(basic.DEFAULT_PARAMS)
        self.assertFalse(missing, f"used but not declared: {sorted(missing)}")
        dynamic = {k for k in basic.DEFAULT_PARAMS if k.startswith(("tw_", "w_", "u_"))}
        unused = set(basic.DEFAULT_PARAMS) - used - dynamic - {"policy_order_peaceful", "policy_order_aggressive",
                                                                 "target_cities"}
        self.assertFalse(unused, f"declared but never read: {sorted(unused)}")

    def test_specs_are_well_formed(self):
        keys = [s["key"] for s in basic.PARAM_SPECS]
        self.assertEqual(len(keys), len(set(keys)))
        for s in basic.PARAM_SPECS:
            with self.subTest(key=s["key"]):
                self.assertTrue(s["label"])
                d = s["default"]
                if s["type"] == "bool":
                    self.assertIsInstance(d, bool)
                elif s["type"] == "int":
                    self.assertIsInstance(d, int)
                elif s["type"] == "float":
                    self.assertIsInstance(d, float)
                elif s["type"] == "choice":
                    self.assertIn(d, s["choices"])
                if s["type"] in ("int", "float"):
                    if s.get("min") is not None:
                        self.assertLessEqual(s["min"], d)
                    if s.get("max") is not None:
                        self.assertGreaterEqual(s["max"], d)
                if s["type"] in ("order", "list") and isinstance(d, list):
                    self.assertTrue(set(d) <= set(s["options"]), set(d) - set(s["options"]))

    def test_a_parameter_changes_the_bot(self):
        bot = profiles.make_bot({"profile": None, "bot": "basic", "params": {"war_prep_rate": 3.0}}, seed=1)
        self.assertEqual(bot.p["war_prep_rate"], 3.0)
        self.assertEqual(bot.p["u_food"], basic.DEFAULT_PARAMS["u_food"])


class ProfileTests(unittest.TestCase):
    def tearDown(self):
        for p in profiles.list_profiles():
            if not p["builtin"]:
                profiles.delete(p["id"])

    def test_clean_params_coerces_drops_defaults_and_refuses_unknown(self):
        out = profiles.clean_params("basic", {"war_prep_rate": "2", "u_food": 3.6, "prep_gather": "true",
                                              "site_candidates": 8.0})
        self.assertEqual(out, {"war_prep_rate": 2.0, "prep_gather": True, "site_candidates": 8})
        with self.assertRaises(profiles.ProfileError):
            profiles.clean_params("basic", {"no_such_knob": 1})
        with self.assertRaises(profiles.ProfileError):
            profiles.clean_params("basic", {"prod_mode": "sideways"})

    def test_save_revisions_and_history(self):
        p = profiles.save({"name": "Warlike", "engine": "basic", "aggression": 0.8, "params": {"war_prep_rate": 2}},
                          user="tester", note="first")
        self.assertEqual((p["id"], p["rev"]), ("warlike", 1))
        same = profiles.save({**p, "description": "only words changed"})
        self.assertEqual(same["rev"], 1, "a description edit is not a new revision")
        p2 = profiles.save({**p, "params": {"war_prep_rate": 3}}, note="more")
        self.assertEqual(p2["rev"], 2)
        self.assertEqual([h["note"] for h in p2["history"]], ["first", "more"])
        with self.assertRaises(profiles.ProfileError):
            profiles.save({"id": "standard", "name": "x"})

    def test_fork_and_delete(self):
        f = profiles.fork("v1", name="My v1")
        self.assertEqual((f["engine"], f["parent"], f["builtin"]), ("frozen_7149efb1", "v1", False))
        profiles.delete(f["id"])
        with self.assertRaises(profiles.ProfileError):
            profiles.get(f["id"])
        with self.assertRaises(profiles.ProfileError):
            profiles.delete("standard")

    def test_fingerprint_is_what_plays(self):
        a = profiles.fingerprint("basic", {"war_prep_rate": 2.0}, None)
        self.assertEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2}, None))
        self.assertEqual(a, profiles.fingerprint(profiles.code_id("basic"), {"war_prep_rate": 2.0}, None),
                         "the live bot and its frozen copy are the same code")
        self.assertNotEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2.5}, None))
        self.assertNotEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2.0}, 0.5))
        self.assertEqual(profiles.fingerprint("basic", {"u_food": basic.DEFAULT_PARAMS["u_food"]}, None),
                         profiles.fingerprint("basic", {}, None), "a default value is not an override")

    def test_resolve_seat_aggression(self):
        p = profiles.save({"name": "Calm", "engine": "basic", "aggression": 0.1, "params": {}})
        self.assertEqual(profiles.make_bot(p["id"], aggression=0.9).aggression, 0.1, "the profile's wins")
        self.assertEqual(profiles.make_bot("standard", aggression=0.9).aggression, 0.9, "the seat's when open")
        self.assertEqual(type(profiles.make_bot("idle")).__name__, "IdleBot")

    def test_old_snapshots_have_an_inferred_schema(self):
        sch = profiles.schema("frozen_7149efb1")
        keys = {p["key"] for g in sch["groups"] for p in g["params"]}
        self.assertIn("w_food", keys)

    def test_engines_list_the_live_bot_snapshots_and_idle(self):
        ids = [e["id"] for e in profiles.engines()]
        self.assertEqual((ids[0], ids[-1]), ("basic", "idle"))
        self.assertIn("frozen_7149efb1", ids)


class LabProfileTests(unittest.TestCase):
    def test_profile_seats_are_frozen_into_the_experiment(self):
        from citar import lab
        p = profiles.save({"name": "Lab seat", "engine": "frozen_d95d50cb", "params": {"war_prep_rate": 2.0}})
        try:
            spec = lab.normalize({"name": "t", "seats": [{"profile": p["id"]}, {"profile": "v1"}]})
            a, b = spec["seats"]
            self.assertEqual((a["bot"], a["params"], a["label"], a["profile_rev"]),
                             ("frozen_d95d50cb", {"war_prep_rate": 2.0}, "Lab seat", 1))
            self.assertEqual(a["fingerprint"], profiles.fingerprint("frozen_d95d50cb", {"war_prep_rate": 2.0}, None))
            self.assertEqual(b["bot"], "frozen_7149efb1")
            # a later edit does not change the queued experiment
            profiles.save({**p, "params": {"war_prep_rate": 3.0}})
            self.assertEqual(lab.game_spec(spec, 0)["seats"][0]["params"], {"war_prep_rate": 2.0})
            bot = lab.make_bot(lab.game_spec(spec, 0)["seats"][0], 1, 0.5)
            self.assertEqual(bot.p["war_prep_rate"], 2.0)
        finally:
            profiles.delete(p["id"])

    def test_api_queues_an_ab_experiment(self):
        from citar import lab
        from citar.server import bots_api
        admin = SimpleNamespace(handle="tester")
        spec = bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["v1", "snapshot-0922"], games=2,
                                                                 name="api-ab"), user=admin)
        try:
            self.assertEqual([s["profile"] for s in spec["seats"]], ["v1", "snapshot-0922", "v1", "snapshot-0922"])
            self.assertTrue((lab.QUEUE / "api-ab.json").exists())
            again = bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["v1"], name="api-ab"), user=admin)
            self.assertEqual(again["name"], "api-ab-2", "names never overwrite an experiment")
            (lab.QUEUE / "api-ab-2.json").unlink()
            from fastapi import HTTPException
            with self.assertRaises(HTTPException):
                bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["v1"], maps=["mars"]), user=admin)
        finally:
            (lab.QUEUE / "api-ab.json").unlink(missing_ok=True)


class BestBotTests(unittest.TestCase):
    def test_best_is_pinned_when_a_game_is_created(self):
        from unittest import mock
        from citar.bots import ratings as rt
        from citar.server.session import SessionManager
        with mock.patch.object(rt, "best_profile", return_value="v1"):
            m = SessionManager()
            s = m.create({"map_size": "duel", "seed": 5, "barbarians": "off"},
                         [{"type": "human"}, {"type": "bot", "bot": {"profile": "best", "aggression": 0.4}}],
                         track=False)
            try:
                self.assertEqual(s.seats[1].bot, {"profile": "v1", "aggression": 0.4, "chosen_as": "best"})
                self.assertEqual(s.get_agent(1).profile["engine"], "frozen_7149efb1")
            finally:
                m.delete(s.id)

    def test_best_skips_idle_and_prefers_a_rated_current_revision(self):
        def entry(profile, fp, rating):
            return {"profile": profile, "fingerprint": fp, "difficulty": "Prince", "rated": True, "rating": rating}
        idle_fp = profiles.fingerprint("idle", {}, None)
        board = [entry("idle", idle_fp, 2000), entry("standard", "old-code", 1700),
                 entry("v1", profiles.fingerprint("frozen_7149efb1", {}, None), 1500)]
        self.assertEqual(ratings.best_profile(board), "v1", "Standard's rating is from older code")
        order = [p["id"] for p in ratings.ranked_profiles(board)]
        self.assertEqual(order[:3], ["idle", "standard", "v1"])
        self.assertEqual(ratings.best_profile([]), "standard")


class RatingTests(unittest.TestCase):
    @staticmethod
    def _game(order, n=None):
        """A game in which the entries in `order` finish in that order (shares descending)."""
        n = n or len(order)
        return {"seats": [{"entry": e, "share": (n - i) / 10} for i, e in enumerate(order)]}

    def test_the_stronger_side_rates_higher_and_ratings_are_symmetric(self):
        games = [self._game(["a", "b"]) for _ in range(8)] + [self._game(["b", "a"]) for _ in range(2)]
        r = ratings.fit(ratings.comparisons(games))
        self.assertGreater(r["a"][0], r["b"][0])
        self.assertAlmostEqual(r["a"][0] - 1500, 1500 - r["b"][0], places=6)
        # 8-2 is 4:1 odds (241 Elo); the prior pulls both towards 1500
        self.assertTrue(150 < r["a"][0] - r["b"][0] < 241, r)

    def test_ties_and_prior(self):
        tie = {"seats": [{"entry": "a", "share": 0.5}, {"entry": "b", "share": 0.5}]}
        r = ratings.fit(ratings.comparisons([tie] * 5))
        self.assertAlmostEqual(r["a"][0], 1500, places=6)
        one = ratings.fit(ratings.comparisons([self._game(["a", "b"])]))
        self.assertLess(one["a"][0] - one["b"][0], 200, "one game must not make a runaway rating")

    def test_multiplayer_pairs_are_weighted(self):
        pairs = ratings.comparisons([self._game(["a", "b", "c", "d"])])
        self.assertAlmostEqual(pairs[("a", "b")][1], 1 / 3)
        self.assertAlmostEqual(sum(n for _, n in pairs.values()), 2.0, "4 seats x 3 pairs / 2 x 1/3")

    def test_rankings_from_lab_results(self):
        from citar import lab
        lab._dirs()
        spec = lab.normalize({"name": "rate-me", "seats": [{"profile": "v1"}, {"profile": "v0"}], "games": 3})
        (lab.DONE / "rate-me.json").write_text(json.dumps(spec), encoding="utf-8")
        rows = []
        for i in range(3):
            players = {}
            for k, seat in enumerate(lab.game_spec(spec, i)["seats"]):
                share = 0.7 if seat["profile"] == "v1" else 0.3
                players[str(k)] = {"label": seat["label"], "bot": seat["bot"], "difficulty": "Prince", "alive": True,
                                   "score_share": share, "rank": 0 if share > 0.5 else 1, "fingerprint": seat["fingerprint"],
                                   "profile": seat["profile"], "profile_rev": 1, "techs": 50, "cities": 5, "events": {}}
            rows.append(json.dumps({"exp": "rate-me", "i": i, "turns": 100, "winner": None, "victory": None,
                                    "players": players, "finished": f"2026-09-2{i}T10:00:00"}))
        (lab.RESULTS / "rate-me.jsonl").write_text("\n".join(rows) + "\n", encoding="utf-8")
        try:
            board = ratings.rankings()["entries"]
            names = [b["name"] for b in board if "rate-me" in b["experiments"]]
            self.assertEqual(names[:2], ["v1 (18 Sep)", "v0 (18 Sep)"])
            v1 = next(b for b in board if b["name"] == "v1 (18 Sep)")
            self.assertEqual((v1["seats"], v1["profile"]), (3, "v1"))
            hist = ratings.rankings()["history"][v1["id"]]
            self.assertEqual([h[0] for h in hist], ["2026-09-20", "2026-09-21", "2026-09-22"])
        finally:
            (lab.DONE / "rate-me.json").unlink()
            (lab.RESULTS / "rate-me.jsonl").unlink()


if __name__ == "__main__":
    unittest.main()
