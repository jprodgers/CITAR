"""Bot profiles on the engine's bot versions, fingerprints, the Bots page's API, the lab's records and the ratings
fit (crates/citar-engine/DESIGN.md P2.7.3, P2.8.5-P2.8.7).

Versions, their schemas, cleaning, fingerprints and the build id are the Rust engine's (``engine_api``'s Phase 2
names), so the tests of them are ``rust_only``. The rest run on either backend: storage and the ratings fit need no
engine, and a profile still plays in a lobby seat on the Python backend until package 2-12 deletes it.
"""
import tests  # noqa: F401  (temporary saves folder; must be imported before citar)
import ast
import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from citar import engine_api
from citar.bots import profiles, ratings
from tests.backends import RUST, python_engine_only, rust_only


def _clear_profiles():
    for p in profiles.list_profiles():
        if not p.get("builtin"):
            profiles.delete(p["id"])


def _latest() -> str:
    return next(v["id"] for v in engine_api.bot_versions() if v["latest"])


class ParameterTests(unittest.TestCase):
    @python_engine_only("a_key_missing_from_either_side_fails")
    def test_every_parameter_the_code_reads_is_declared(self):
        """P["x"] / self.p["x"] anywhere in the bot must be a declared parameter, or a profile can't reach it."""
        from citar.bots import basic
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

    @python_engine_only("the_defaults_deserialize_into_params")
    def test_specs_are_well_formed(self):
        from citar.bots import basic
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

    @rust_only
    def test_a_parameter_changes_the_bot(self):
        bot = profiles.make_bot({"profile": None, "bot": "basic", "params": {"war_prep_rate": 3.0}})
        self.assertEqual(json.loads(bot.params), {"war_prep_rate": 3.0})
        self.assertEqual(engine_api.bot_fingerprint(bot), profiles.fingerprint("basic", {"war_prep_rate": 3}, None))
        self.assertNotEqual(engine_api.bot_fingerprint(bot), profiles.fingerprint("basic", {}, None))


class StorageTests(unittest.TestCase):
    """What needs no engine: the built-ins, the files, refusing what a profile may not name."""

    def tearDown(self):
        _clear_profiles()

    def test_the_built_ins_are_standard_classic_production_and_idle(self):
        self.assertEqual([(p["id"], p["engine"], p["params"]) for p in profiles.BUILTIN],
                         [("standard", "basic", {}), ("classic-production", "basic", {"prod_mode": "classic"}),
                          ("idle", "idle", {})])
        self.assertEqual([p["id"] for p in profiles.list_profiles()], ["standard", "classic-production", "idle"])
        for gone in ("v1", "v0", "snapshot-0922"):
            with self.assertRaises(profiles.ProfileError):
                profiles.get(gone)

    def test_an_engine_is_basic_a_version_or_idle_and_a_frozen_one_is_archived(self):
        for ok in ("basic", "basic-1", "basic-12", "idle"):
            self.assertEqual(profiles.check_engine(ok), ok)
        with self.assertRaises(profiles.ProfileError) as e:
            profiles.check_engine("frozen_7149efb1")
        self.assertIn("archived with 0.1.5", str(e.exception))
        for bad in ("live", "basic-", "basic-x", "os", "", None, "../basic"):
            with self.assertRaises(profiles.ProfileError):
                profiles.check_engine(bad)

    def test_an_aggression_is_a_number_held_to_0_1_or_none(self):
        for given, kept in ((None, None), ("", None), (0.3, 0.3), ("0.6", 0.6), (1.7, 1.0), (-2, 0.0),
                            (float("inf"), 1.0)):
            self.assertEqual(profiles.clean_aggression(given), kept, given)
        for bad in ("high", float("nan"), True, [0.5], {}):
            with self.assertRaises(profiles.ProfileError, msg=bad):
                profiles.clean_aggression(bad)

    def test_a_stored_profile_on_a_frozen_engine_is_listed_but_never_plays(self):
        # A laptop's profiles from 0.1.5 may name a snapshot: the history stays readable, the profile refuses to
        # play, and a lobby seat that names it plays Standard (BotAgent).
        profiles.PROFILES_DIR.mkdir(parents=True, exist_ok=True)
        (profiles.PROFILES_DIR / "old-tuned.json").write_text(json.dumps(
            {"id": "old-tuned", "name": "Old tuned", "engine": "frozen_d95d50cb", "aggression": None,
             "params": {"war_prep_rate": 2.0}, "rev": 3, "history": [], "builtin": False}), encoding="utf-8")
        self.assertIn("old-tuned", [p["id"] for p in profiles.list_profiles()])
        for use in (lambda: profiles.resolve("old-tuned"), lambda: profiles.make_bot("old-tuned")):
            with self.assertRaises(profiles.ProfileError) as e:
                use()
            self.assertIn("archived with 0.1.5", str(e.exception))
        from citar.agents.bot_agent import BotAgent
        self.assertEqual(BotAgent(profile="old-tuned").profile["profile"], "standard")


class EitherBackendTests(unittest.TestCase):
    def test_a_profile_plays_in_a_lobby_seat(self):
        """On Rust a profile resolves to the version that plays and its fingerprint; on the Python backend (until
        2-12) it still plays, with the stored overrides, and has neither."""
        r = profiles.resolve("classic-production")
        self.assertEqual((r["engine"], r["params"], r["profile"], r["profile_rev"]),
                         ("basic", {"prod_mode": "classic"}, "classic-production", 1))
        bot = profiles.make_bot("classic-production", seed=3, aggression=0.7)
        self.assertEqual(bot.aggression, 0.7)
        if RUST:
            self.assertEqual((r["version"], bot.version), (_latest(), _latest()))
            self.assertEqual(r["fingerprint"], engine_api.bot_fingerprint(bot))
        else:
            self.assertEqual((r["version"], r["fingerprint"], bot.p["prod_mode"]), (None, None, "classic"))
            with self.assertRaises(engine_api.BackendError):
                profiles.resolve("standard", pin=True)


@rust_only
class ProfileTests(unittest.TestCase):
    def tearDown(self):
        _clear_profiles()

    def test_clean_params_coerces_drops_defaults_and_refuses_unknown(self):
        default_food = profiles.defaults("basic")["u_food"]
        out = profiles.clean_params("basic", {"war_prep_rate": "2", "u_food": default_food, "war_overseas": "true",
                                              "site_candidates": 8.0})
        self.assertEqual(out, {"site_candidates": 8, "war_overseas": True, "war_prep_rate": 2.0})
        self.assertEqual(list(out), sorted(out), "keys sorted")
        for bad in ({"no_such_knob": 1}, {"prod_mode": "sideways"}, {"site_candidates": 8.5}):
            with self.assertRaises(profiles.ProfileError, msg=bad):
                profiles.clean_params("basic", bad)
        self.assertEqual(profiles.clean_params("idle", {"anything": 1}), {})

    def test_save_revisions_and_history(self):
        p = profiles.save({"name": "Warlike", "engine": "basic", "aggression": 0.8, "params": {"war_prep_rate": 2}},
                          user="tester", note="first")
        self.assertEqual((p["id"], p["rev"], p["params"]), ("warlike", 1, {"war_prep_rate": 2.0}))
        same = profiles.save({**p, "description": "only words changed"})
        self.assertEqual(same["rev"], 1, "a description edit is not a new revision")
        p2 = profiles.save({**p, "params": {"war_prep_rate": 3}}, note="more")
        self.assertEqual(p2["rev"], 2)
        self.assertEqual([h["note"] for h in p2["history"]], ["first", "more"])
        with self.assertRaises(profiles.ProfileError):
            profiles.save({"id": "standard", "name": "x"})
        for agg in ("loud", float("nan")):
            with self.assertRaises(profiles.ProfileError):
                profiles.save({"name": "Odd", "aggression": agg})
        self.assertEqual(profiles.save({"name": "Hot", "aggression": 7})["aggression"], 1.0)

    def test_a_saved_profile_round_trips_through_bot_clean_params(self):
        raw = {"war_prep_rate": "2.5", "site_candidates": "8", "war_overseas": "yes", "tech_mode": "potential",
               "policy_order_peaceful": "liberty_first"}
        p = profiles.save({"name": "Round trip", "engine": "basic-1", "params": raw})
        version = _latest()
        self.assertEqual(p["params"], engine_api.bot_clean_params(version, raw))
        self.assertEqual(engine_api.bot_clean_params(version, p["params"]), p["params"], "clean is idempotent")
        back = profiles.get(p["id"])
        self.assertEqual(back["params"], p["params"])
        again = profiles.save(back)
        self.assertEqual((again["rev"], again["params"]), (1, p["params"]), "saving it as read is no new revision")
        self.assertEqual(json.loads(profiles.make_bot(p["id"]).params), p["params"])

    def test_fork_and_delete(self):
        f = profiles.fork("classic-production", name="My production")
        self.assertEqual((f["engine"], f["params"], f["parent"], f["builtin"]),
                         ("basic", {"prod_mode": "classic"}, "classic-production", False))
        self.assertEqual(f["tags"], [], "the control tag is the built-in's")
        profiles.delete(f["id"])
        with self.assertRaises(profiles.ProfileError):
            profiles.get(f["id"])
        with self.assertRaises(profiles.ProfileError):
            profiles.delete("standard")

    def test_frozen_engines_are_refused_with_the_archive_message(self):
        old = "frozen_7149efb1"
        for use in (lambda: profiles.save({"name": "Old", "engine": old}), lambda: profiles.schema(old),
                    lambda: profiles.resolve({"bot": old}), lambda: profiles.fingerprint(old, {}, None),
                    lambda: profiles.make_bot({"bot": old})):
            with self.assertRaises(profiles.ProfileError) as e:
                use()
            self.assertEqual(str(e.exception), profiles.ARCHIVED.format(engine=old))
        with self.assertRaises(profiles.ProfileError) as e:
            profiles.save({"name": "Future", "engine": "basic-99"})
        self.assertIn("not a bot version", str(e.exception))

    def test_engines_are_basic_then_the_versions(self):
        versions = engine_api.bot_versions()
        engines = profiles.engines()
        self.assertEqual([e["id"] for e in engines], ["basic"] + [v["id"] for v in versions])
        self.assertIn("idle", [e["id"] for e in engines])
        self.assertEqual(engines[0]["code"], _latest())
        for e in engines:
            self.assertEqual(set(e), {"id", "label", "code", "latest", "created", "description"})

    def test_basic_resolves_to_the_latest_version_at_use(self):
        r = profiles.resolve("standard")
        self.assertEqual((r["engine"], r["version"]), ("basic", _latest()))
        pinned = profiles.resolve("standard", pin=True)
        self.assertEqual((pinned["engine"], pinned["version"]), (_latest(), _latest()))
        self.assertEqual(pinned["fingerprint"], r["fingerprint"], "basic and the version it names play alike")
        self.assertEqual(profiles.resolve({"bot": "live"})["engine"], "basic", "0.1.5's live bot is the latest")

    def test_fingerprint_is_what_plays(self):
        a = profiles.fingerprint("basic", {"war_prep_rate": 2.0}, None)
        self.assertRegex(a, r"^[0-9a-f]{12}$")
        self.assertEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2}, None))
        self.assertEqual(a, profiles.fingerprint(_latest(), {"war_prep_rate": "2"}, None),
                         "basic and the version it names are the same code")
        self.assertNotEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2.5}, None))
        self.assertNotEqual(a, profiles.fingerprint("basic", {"war_prep_rate": 2.0}, 0.5))
        default_food = profiles.defaults("basic")["u_food"]
        self.assertEqual(profiles.fingerprint("basic", {"u_food": default_food}, None),
                         profiles.fingerprint("basic", {}, None), "a default value is not an override")
        self.assertNotEqual(profiles.fingerprint("idle", {}, None), profiles.fingerprint("basic", {}, None))

    def test_the_seats_aggression_is_not_the_profiles(self):
        """One profile is one entry however a seat plays it: the fingerprint hashes the profile's fixed aggression,
        never the seat's (DESIGN.md P2.8.6)."""
        p = profiles.save({"name": "Calm", "engine": "basic", "aggression": 0.1, "params": {}})
        calm = profiles.make_bot(p["id"], aggression=0.9)
        self.assertEqual((calm.aggression, calm.fixed_aggression), (0.1, 0.1), "the profile's wins")
        low, high = profiles.make_bot("standard", aggression=0.2), profiles.make_bot("standard", aggression=0.8)
        self.assertEqual((low.aggression, high.aggression, low.fixed_aggression), (0.2, 0.8, None),
                         "the seat's when open")
        self.assertEqual(engine_api.bot_fingerprint(low), engine_api.bot_fingerprint(high))
        self.assertEqual(engine_api.bot_fingerprint(low), profiles.resolve("standard")["fingerprint"])
        self.assertEqual(engine_api.bot_fingerprint(calm), profiles.fingerprint("basic", {}, 0.1))
        self.assertEqual(profiles.make_bot("idle").version, "idle")


@rust_only
class BotsPageTests(unittest.TestCase):
    """The Bots page's API (citar/server/bots_api.py) in the shapes the page reads."""

    admin = SimpleNamespace(handle="tester")

    def tearDown(self):
        _clear_profiles()

    def test_engines_and_the_schema(self):
        from citar.server import bots_api
        engines = bots_api.list_engines()["engines"]
        self.assertEqual([e["id"] for e in engines], ["basic"] + [v["id"] for v in engine_api.bot_versions()])
        schema = bots_api.param_schema("basic")
        self.assertEqual(list(schema), ["engine", "groups"])
        self.assertEqual(schema["engine"], "basic-1")
        self.assertEqual(len(schema["groups"]), 17)
        self.assertEqual(sum(len(g["params"]) for g in schema["groups"]), 373)
        self.assertEqual(bots_api.param_schema("basic-1"), schema)
        self.assertEqual(bots_api.param_schema("idle"), {"engine": "idle", "groups": []})

    def test_a_frozen_engine_is_refused_with_the_archive_message(self):
        from fastapi import HTTPException
        from citar.server import bots_api
        with self.assertRaises(HTTPException) as e:
            bots_api.param_schema("frozen_7149efb1")
        self.assertEqual((e.exception.status_code, e.exception.detail),
                         (404, profiles.ARCHIVED.format(engine="frozen_7149efb1")))
        with self.assertRaises(HTTPException) as e:
            bots_api.create_profile(bots_api.ProfileBody(name="Old", engine="frozen_7149efb1"), user=self.admin)
        self.assertEqual((e.exception.status_code, e.exception.detail),
                         (400, profiles.ARCHIVED.format(engine="frozen_7149efb1")))

    def test_a_saved_profile_round_trips(self):
        from citar.server import bots_api
        body = bots_api.ProfileBody(name="Page made", engine="basic", aggression=0.6,
                                    params={"war_prep_rate": "3", "lux_buy": "on"}, note="from the page")
        made = bots_api.create_profile(body, user=self.admin)
        self.assertEqual(made["params"], engine_api.bot_clean_params("basic", body.params))
        shown = bots_api.get_profile(made["id"])["profile"]
        self.assertEqual((shown["params"], shown["aggression"], shown["rev"]), (made["params"], 0.6, 1))
        self.assertEqual(shown["fingerprint"], profiles.fingerprint("basic", made["params"], 0.6))
        edited = bots_api.update_profile(made["id"], bots_api.ProfileBody(
            name="Page made", engine="basic", aggression=0.6, params=shown["params"]), user=self.admin)
        self.assertEqual(edited["rev"], 1, "the page sending back what it read is no new revision")


@rust_only
class LabTests(unittest.TestCase):
    def tearDown(self):
        _clear_profiles()

    def test_profile_seats_are_pinned_into_the_experiment(self):
        from citar import lab
        p = profiles.save({"name": "Lab seat", "engine": "basic", "params": {"war_prep_rate": 2.0}})
        spec = lab.normalize({"name": "t", "seats": [{"profile": p["id"]}, {"profile": "classic-production"}]})
        a, b = spec["seats"]
        self.assertEqual((a["bot"], a["params"], a["label"], a["profile_rev"], a["profile_name"]),
                         (_latest(), {"war_prep_rate": 2.0}, "Lab seat", 1, "Lab seat"))
        self.assertEqual((b["bot"], b["params"], b["label"]), (_latest(), {"prod_mode": "classic"},
                                                              "Classic production"))
        # a later edit does not change the queued experiment
        profiles.save({**p, "params": {"war_prep_rate": 3.0}})
        seat = lab.game_spec(spec, 0)["seats"][0]
        self.assertEqual(seat["params"], {"war_prep_rate": 2.0})
        bot = lab.make_bot(seat, 0.5)
        self.assertEqual((bot.version, json.loads(bot.params)), (_latest(), {"war_prep_rate": 2.0}))
        self.assertEqual(engine_api.bot_fingerprint(bot), profiles.fingerprint(_latest(), {"war_prep_rate": 2}, None))

    def test_raw_seats_pin_their_version(self):
        from citar import lab
        spec = lab.normalize({"name": "raw", "seats": [{"bot": "basic", "params": {"counter_rounds": "2"}},
                                                        {"bot": "live"}, {"bot": "idle"}, {}]})
        self.assertEqual([s["bot"] for s in spec["seats"]], [_latest(), "basic", "idle", _latest()])
        self.assertEqual(spec["seats"][0]["params"], {"counter_rounds": 2})
        self.assertEqual(spec["seats"][0]["label"], _latest())
        for bad in ({"bot": "frozen_d95d50cb"}, {"bot": "basic", "params": {"no_such": 1}}, {"bot": "basic-99"}):
            with self.assertRaises(profiles.ProfileError, msg=bad):
                lab.normalize({"name": "bad", "seats": [bad]})

    def test_factorial_experiments_pin_and_check_their_levels(self):
        from citar import lab
        s = lab.normalize({"name": "f", "factors": {"war_prep_rate": [1.0, 2.0]}, "profile": "classic-production"})
        self.assertEqual((s["bot"], s["base_params"], s["profile_rev"]), (_latest(), {"prod_mode": "classic"}, 1))
        seats = lab.game_spec(s, 0)["seats"]
        self.assertEqual({x["params"]["war_prep_rate"] for x in seats}, {1.0, 2.0})
        self.assertEqual({x["profile_name"] for x in seats}, {"Classic production"})
        with self.assertRaises(profiles.ProfileError):
            lab.normalize({"name": "f2", "factors": {"site_candidates": [8, 8.5]}})

    def test_a_best_seat_is_queued_as_the_profile_it_stands_for(self):
        """Ratings attribute a game to the profile its results record, so "best" is recorded as the profile it
        stood for when the experiment was queued, not as "best" (which is no profile)."""
        from citar import lab
        with mock.patch.object(ratings, "best_profile", return_value="classic-production"):
            spec = lab.normalize({"name": "best", "seats": [{"profile": "best"}, {"profile": "standard"}]})
            factorial = lab.normalize({"name": "best-f", "factors": {"war_prep_rate": [1.0, 2.0]},
                                       "profile": "best"})
        self.assertEqual([(s["profile"], s["label"], s["params"]) for s in spec["seats"]],
                         [("classic-production", "Classic production", {"prod_mode": "classic"}),
                          ("standard", "Standard", {})])
        self.assertEqual(factorial["profile"], "classic-production")
        self.assertEqual({s["profile"] for s in lab.game_spec(factorial, 0)["seats"]}, {"classic-production"})

    def test_a_seats_aggression_is_held_to_0_1_when_queued(self):
        from citar import lab
        spec = lab.normalize({"name": "agg", "seats": [{"bot": "basic", "aggression": 1.7},
                                                       {"profile": "standard", "aggression": -2},
                                                       {"bot": "basic", "aggression": "0.3"}, {"bot": "basic"}]})
        self.assertEqual([s["aggression"] for s in spec["seats"]], [1.0, 0.0, 0.3, None])
        factorial = lab.normalize({"name": "agg-f", "factors": {"war_prep_rate": [1.0, 2.0]}, "aggression": 3})
        self.assertEqual(factorial["aggression"], 1.0)
        for bad in ("high", float("nan")):
            with self.assertRaises(profiles.ProfileError, msg=bad):
                lab.normalize({"name": "bad", "seats": [{"bot": "basic", "aggression": bad}]})
            with self.assertRaises(profiles.ProfileError, msg=bad):
                lab.normalize({"name": "bad", "seats": [{"profile": "standard", "aggression": bad}]})

    def test_api_queues_an_ab_experiment(self):
        from citar import lab
        from citar.server import bots_api
        admin = SimpleNamespace(handle="tester")
        spec = bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["standard", "classic-production"],
                                                                 games=2, name="api-ab"), user=admin)
        try:
            self.assertEqual([s["profile"] for s in spec["seats"]],
                             ["standard", "classic-production", "standard", "classic-production"])
            self.assertEqual({s["bot"] for s in spec["seats"]}, {_latest()})
            self.assertTrue((lab.QUEUE / "api-ab.json").exists())
            again = bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["standard"], name="api-ab"),
                                              user=admin)
            self.assertEqual(again["name"], "api-ab-2", "names never overwrite an experiment")
            (lab.QUEUE / "api-ab-2.json").unlink()
            from fastapi import HTTPException
            with self.assertRaises(HTTPException):
                bots_api.queue_experiment(bots_api.ExperimentBody(profiles=["standard"], maps=["mars"]), user=admin)
        finally:
            (lab.QUEUE / "api-ab.json").unlink(missing_ok=True)

    def test_the_engine_hash_is_the_build_id(self):
        from citar import lab
        self.assertEqual(lab.engine_hash(), engine_api.build_info()["build_id"])

    def test_a_played_game_records_what_played(self):
        """Each seat's result names the build, the version, the profile, its revision and the fingerprint, taken
        at play time; a profile's seats at different positions share one fingerprint."""
        from citar import lab
        spec = lab.normalize({"name": "records", "games": 2, "size": "duel", "maps": ["pangaea"], "turns": 12,
                              "barbarians": "off", "seats": [{"profile": "standard"},
                                                             {"profile": "classic-production"}]})
        build = engine_api.build_info()["build_id"]
        fp = {"standard": profiles.resolve("standard")["fingerprint"],
              "classic-production": profiles.resolve("classic-production")["fingerprint"]}
        seen = {}
        for i in range(2):
            r = lab.play(lab.game_spec(spec, i))
            self.assertEqual((r["engine"], r["errors"]), (build, []))
            for k, pl in r["players"].items():
                self.assertEqual((pl["build"], pl["version"], pl["profile_rev"]), (build, _latest(), 1))
                self.assertEqual(pl["fingerprint"], fp[pl["profile"]])
                self.assertEqual(pl["fixed_aggression"], None)
                seen.setdefault(pl["profile"], []).append((int(k), pl["aggression"], pl["fingerprint"]))
        positions = seen["standard"]
        self.assertEqual(sorted(p for p, _, _ in positions), [0, 1], "standard played both positions")
        self.assertNotEqual(positions[0][1], positions[1][1], "with each position's aggression")
        self.assertEqual(len({f for _, _, f in positions}), 1, "and one fingerprint")

    def test_a_played_seat_records_the_aggression_that_played(self):
        """A seat's fixed aggression is recorded as its bot played it and its fingerprint hashed it, even when the
        queued value was out of range (an experiment queued before seats were held to 0..1)."""
        from citar import lab
        spec = lab.normalize({"name": "agg-played", "games": 1, "size": "duel", "maps": ["pangaea"], "turns": 3,
                              "barbarians": "off", "seats": [{"bot": "basic"}, {"bot": "basic"}]})
        game = lab.game_spec(spec, 0)
        game["seats"][0]["aggression"] = 1.7
        r = lab.play(game)
        fixed, open_ = r["players"]["0"], r["players"]["1"]
        self.assertEqual((fixed["fixed_aggression"], fixed["aggression"]), (1.0, 1.0))
        self.assertEqual(fixed["fingerprint"], profiles.fingerprint(_latest(), {}, 1.0))
        self.assertIsNone(open_["fixed_aggression"])
        self.assertEqual(open_["fingerprint"], profiles.fingerprint(_latest(), {}, None))


class LabCommandTests(unittest.TestCase):
    def test_run_and_submit_need_the_rust_engine(self):
        """On a backend without a build id (the Python engine, until 2-12) the lab stops at once with the facade's
        message, rather than queueing what no game can play or starting each queued game three times to crash."""
        import contextlib
        import io
        import tempfile
        from citar import lab
        refusal = engine_api.BackendError("build_info: Rust backend only (set CITAR_ENGINE=rust).")
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "spec.json"
            path.write_text(json.dumps({"name": "never-queued", "seats": [{"bot": "basic"}, {"bot": "basic"}]}),
                            encoding="utf-8")
            for argv in (["submit", str(path)], ["run", "--exit-when-idle"]):
                err = io.StringIO()
                with mock.patch.object(engine_api, "build_info", side_effect=refusal), \
                        mock.patch.object(lab, "run") as run, contextlib.redirect_stderr(err):
                    with self.assertRaises(SystemExit) as stop:
                        lab.main(argv)
                self.assertEqual(stop.exception.code, 2, argv)
                self.assertIn("needs the Rust engine", err.getvalue())
                self.assertIn("CITAR_ENGINE=rust", err.getvalue())
                run.assert_not_called()
        self.assertFalse((lab.QUEUE / "never-queued.json").exists())


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
        self.assertAlmostEqual(sum(n for _, n in pairs.values()), 2.0, msg="4 seats x 3 pairs / 2 x 1/3")


class LadderTests(unittest.TestCase):
    """Entries keyed and named from the results' own records (DESIGN.md P2.8.7), on either backend: nothing here
    asks the engine."""

    def setUp(self):
        from citar import lab
        lab._dirs()
        self.written = []

    def tearDown(self):
        for p in self.written:
            p.unlink(missing_ok=True)
        _clear_profiles()

    def _experiment(self, name, rows):
        """An experiment and its results, as the lab writes them."""
        from citar import lab
        spec = {**lab.DEFAULTS, "name": name, "games": len(rows), "submitted": "2026-10-01T00:00:00",
                "seats": [{"label": "A"}, {"label": "B"}]}
        for path, text in ((lab.DONE / f"{name}.json", json.dumps(spec)),
                           (lab.RESULTS / f"{name}.jsonl", "\n".join(json.dumps(r) for r in rows) + "\n")):
            path.write_text(text, encoding="utf-8")
            self.written.append(path)

    @staticmethod
    def _seat(fp, share, rank, profile=None, rev=1, name=None, label=None, build="b1", version="basic-1"):
        return {"label": label or name or profile, "bot": version, "difficulty": "Prince", "alive": True,
                "score_share": share, "rank": rank, "fingerprint": fp, "build": build, "version": version,
                "profile": profile, "profile_rev": rev, "profile_name": name, "params": {},
                "fixed_aggression": None, "techs": 50, "cities": 5, "events": {}}

    def _game(self, i, a, b, day):
        return {"exp": "x", "i": i, "turns": 100, "winner": None, "victory": None,
                "players": {"0": a, "1": b}, "finished": f"2026-10-0{day}T10:00:00"}

    def test_rankings_from_lab_results(self):
        rows = [self._game(i, self._seat("f-std", 0.7, 0, "standard", name="Standard"),
                           self._seat("f-cls", 0.3, 1, "classic-production", name="Classic production"), i + 1)
                for i in range(3)]
        self._experiment("rate-me", rows)
        board = [b for b in ratings.rankings()["entries"] if "rate-me" in b["experiments"]]
        self.assertEqual([b["name"] for b in board], ["Standard", "Classic production"])
        std = board[0]
        self.assertEqual((std["seats"], std["profile"], std["rev"], std["code"], std["build"]),
                         (3, "standard", 1, "basic-1", "b1"))
        hist = ratings.rankings()["history"][std["id"]]
        self.assertEqual([h[0] for h in hist], ["2026-10-01", "2026-10-02", "2026-10-03"])

    def test_entries_are_named_from_their_records(self):
        rows = [
            # one profile on two builds: told apart by build
            self._game(0, self._seat("std-b1", 0.6, 0, "standard", name="Standard"),
                       self._seat("std-b2", 0.4, 1, "standard", name="Standard", build="b2"), 1),
            # a deleted profile keeps the name it played under; a later revision says so; no profile: the label
            self._game(1, self._seat("gone", 0.6, 0, "deleted-one", rev=2, name="Since deleted"),
                       self._seat("raw", 0.4, 1, None, label="raw seat"), 2),
            # a seat that recorded no fingerprint (0.1.5's oldest results): the game is not rated
            self._game(2, self._seat("std-b1", 0.6, 0, "standard", name="Standard"),
                       {**self._seat(None, 0.4, 1, "standard"), "fingerprint": None}, 3),
        ]
        self._experiment("names", rows)
        board = {b["fingerprint"]: b for b in ratings.rankings()["entries"] if "names" in b["experiments"]}
        self.assertEqual(set(board), {"std-b1", "std-b2", "gone", "raw"})
        self.assertEqual(board["std-b1"]["name"], "Standard · basic-1, build b1")
        self.assertEqual(board["std-b2"]["name"], "Standard · basic-1, build b2")
        self.assertEqual(board["gone"]["name"], "Since deleted r2")
        self.assertEqual(board["raw"]["name"], "raw seat")
        self.assertEqual(board["std-b1"]["seats"], 1, "the unrated game is not counted")
        p = profiles.PROFILES_DIR
        p.mkdir(parents=True, exist_ok=True)
        (p / "deleted-one.json").write_text(json.dumps({"id": "deleted-one", "name": "Back again", "engine": "basic",
                                                        "params": {}, "aggression": None, "rev": 2}),
                                            encoding="utf-8")
        renamed = {b["fingerprint"]: b for b in ratings.rankings()["entries"]}
        self.assertEqual(renamed["gone"]["name"], "Back again r2", "a profile is called what it is called now")

    def test_results_of_0_1_5_are_not_rated(self):
        """The ladder of 0.1.6 starts empty (DESIGN.md P2.8.7): an upgraded install keeps 0.1.5's results, whose
        seats record a fingerprint of the Python bot (frozen snapshots among them) but no build or version, and
        none of their games is rated, nor can they make a 0.1.5 profile the best bot."""
        def old(fp, share, rank, profile, name, bot):
            return {"label": name, "bot": bot, "difficulty": "Prince", "alive": True, "levels": None,
                    "profile": profile, "profile_rev": 1, "fingerprint": fp, "aggression": 0.25,
                    "score_share": share, "rank": rank, "techs": 50, "cities": 5, "events": {}}
        p = profiles.PROFILES_DIR
        p.mkdir(parents=True, exist_ok=True)
        (p / "v2-candidate-a.json").write_text(json.dumps(
            {"id": "v2-candidate-a", "name": "v2 candidate A", "engine": "frozen_4f6e040a", "params": {},
             "aggression": None, "rev": 1}), encoding="utf-8")
        rows = [{**self._game(i, old("8c1f0e2a9b3d", 0.8, 0, "v2-candidate-a", "v2 candidate A", "frozen_4f6e040a"),
                              old("1d2c3b4a5f6e", 0.2, 1, "standard", "Standard", "frozen_de161146"), i + 1),
                 "engine": "e7427f9dc8"} for i in range(4)]
        # a seat of 0.1.6 does not make a game with a seat of 0.1.5 rated
        rows.append(self._game(4, self._seat("new-std", 0.6, 0, "standard", name="Standard"),
                               old("1d2c3b4a5f6e", 0.4, 1, "standard", "Standard", "frozen_de161146"), 5))
        self._experiment("league-0-1-5", rows)
        r = ratings.rankings()
        self.assertEqual([b for b in r["entries"] if "league-0-1-5" in b["experiments"]], [])
        self.assertEqual(ratings.best_profile(), "standard")


class BestBotTests(unittest.TestCase):
    def tearDown(self):
        _clear_profiles()

    def test_best_is_pinned_when_a_game_is_created(self):
        from citar.bots import ratings as rt
        from citar.server.session import SessionManager
        with mock.patch.object(rt, "best_profile", return_value="classic-production"):
            m = SessionManager()
            s = m.create({"map_size": "duel", "seed": 5, "barbarians": "off"},
                         [{"type": "human"}, {"type": "bot", "bot": {"profile": "best", "aggression": 0.4}}],
                         track=False)
            try:
                self.assertEqual(s.seats[1].bot, {"profile": "classic-production", "aggression": 0.4,
                                                  "chosen_as": "best"})
                agent = s.get_agent(1).profile
                self.assertEqual((agent["engine"], agent["params"]), ("basic", {"prod_mode": "classic"}))
            finally:
                m.delete(s.id)

    def test_best_skips_idle_and_prefers_a_rating_of_the_current_settings(self):
        def entry(profile, fp, rating, params=None):
            return {"profile": profile, "fingerprint": fp, "difficulty": "Prince", "rated": True, "rating": rating,
                    "params": {} if params is None else params, "aggression": None, "last": "2026-10-02T00:00:00"}
        board = [entry("idle", "idle-fp", 2000), entry("standard", "earlier-build", 1700),
                 entry("classic-production", "cls-earlier", 1500, params={"prod_mode": "classic"})]
        self.assertEqual(ratings.best_profile(board), "standard",
                         "the same settings on an earlier build still describe Standard")
        order = [p["id"] for p in ratings.ranked_profiles(board)]
        self.assertEqual(order[:3], ["idle", "standard", "classic-production"])
        p = profiles.PROFILES_DIR
        p.mkdir(parents=True, exist_ok=True)
        (p / "tuned.json").write_text(json.dumps({"id": "tuned", "name": "Tuned", "engine": "basic", "rev": 1,
                                                  "params": {"war_prep_rate": 2.0}, "aggression": None}),
                                      encoding="utf-8")
        board.append(entry("tuned", "tuned-old-settings", 1900, params={"war_prep_rate": 3.0}))
        self.assertEqual(ratings.best_profile(board), "standard", "a rating of different settings doesn't count")
        board[1]["params"] = None
        self.assertEqual(ratings.best_profile(board), "classic-production",
                         "a result that recorded no overrides describes no settings: the best that does wins")
        self.assertEqual(ratings.best_profile([]), "standard")

    def test_best_skips_a_profile_that_cannot_play(self):
        # a saved profile on a frozen snapshot of 0.1.5 is refused wherever it would play, so it is never the best
        p = profiles.PROFILES_DIR
        p.mkdir(parents=True, exist_ok=True)
        (p / "old-tuned.json").write_text(json.dumps({"id": "old-tuned", "name": "Old tuned", "rev": 1,
                                                      "engine": "frozen_d95d50cb", "params": {}, "aggression": None}),
                                          encoding="utf-8")
        # neither rating describes its profile's current settings, so the highest-rated profile that plays wins
        board = [{"profile": "old-tuned", "fingerprint": "old", "difficulty": "Prince", "rated": True,
                  "rating": 2100, "params": {}, "aggression": None, "last": "2026-10-02T00:00:00"},
                 {"profile": "classic-production", "fingerprint": "cls", "difficulty": "Prince", "rated": True,
                  "rating": 1600, "params": None, "aggression": None, "last": "2026-10-02T00:00:00"}]
        self.assertEqual(ratings.best_profile(board), "classic-production")

    @rust_only
    def test_the_current_revision_is_flagged(self):
        fp = profiles.resolve("classic-production")["fingerprint"]
        board = [{"profile": "classic-production", "fingerprint": fp, "difficulty": "Prince", "rated": True,
                  "rating": 1600, "params": {"prod_mode": "classic"}, "aggression": None, "last": "2026-10-02"}]
        p = ratings.profile_rating(profiles.get("classic-production"), board)
        self.assertEqual((p["fingerprint"], p["rating_is_current"], p["rating"]["rating"]), (fp, True, 1600))


class NegotiationTests(unittest.TestCase):
    @python_engine_only("tests/rules/bot_counter_capped_by_treasury.toml")
    def test_a_refused_counter_still_ends_the_bots_move(self):
        """A live game waited 90 s on a bot whose counter-offer the rules refused (2026-09-22)."""
        from citar.engine.game import Game
        from citar.engine import tools
        g = Game.new({"map_size": "duel", "seed": 21, "barbarians": "off",
                      "players": [{"controller": "bot"}, {"controller": "bot"}]})
        a, b = 0, 1
        g.player(a).met.add(b) if isinstance(g.player(a).met, set) else g.player(a).met.append(b)
        g.player(b).met.add(a) if isinstance(g.player(b).met, set) else g.player(b).met.append(a)
        g.player(a).gold = 50
        n = tools.execute(g, a, "open_negotiation", {"to": b, "message": "Friends?",
                                                     "give": [{"type": "gold", "amount": 40}], "receive": []})
        nid = n.get("negotiation_id") or n.get("id") or g.s.negotiations[-1]["id"]
        g.s.current = b
        # B asks A for more gold than A has: any counter A builds from that proposal is refused
        tools.execute(g, b, "respond_negotiation", {"negotiation_id": nid, "action": "counter", "message": "More.",
                                                    "give": [], "receive": [{"type": "gold", "amount": 45}]})
        g.s.current = a
        g.player(b).gold = 1000                   # B can pay, so A counters rather than rejecting outright...
        g.player(a).gold = 30                     # ...but A can no longer pay the 45 on the table: the counter is refused
        bot = profiles.make_bot("standard", seed=1)
        bot.p["counter_max_gap"] = 10 ** 6         # make it try to counter whatever the value
        bot.respond(g, a, nid)
        neg = next(x for x in g.s.negotiations if x["id"] == nid)
        self.assertNotEqual((neg["status"], neg["awaiting"]), ("open", a), neg)


if __name__ == "__main__":
    unittest.main()
