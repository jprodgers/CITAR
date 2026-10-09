"""Map editor files, scenarios (operations, save/load, launch) and probes (with the dry-run model), through the facade
(``citar.engine_api``), so they run on whichever engine is behind it."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar import engine_api as E
from citar import probes as P
from citar.engine_api import ActionError, EngineGame
from citar.server.session import SessionManager
from tests.backends import rust_only

DRY = {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0}


def small_map(name="unit-map", starts=2):
    return E.generate_map(40, 26, "pangaea", starts, 2, seed=11, name=name)


def starts_of(g: EngineGame) -> dict:
    """Each major's start, as the tile its settler stands on as the game begins: {player: (x, y)}."""
    majors = {p["id"] for p in g.majors()}
    out = {}
    for u in g.view(None)["units"]:
        if u["type"] == "Settler" and u["owner"] in majors and u["owner"] not in out:
            out[u["owner"]] = (u["x"], u["y"])
    return out


def city_names(g: EngineGame) -> list:
    return [c["name"] for c in g.view(None)["cities"]]


class MapTests(unittest.TestCase):
    def test_big_sizes_exist(self):
        sizes = E.map_sizes()
        self.assertEqual(sizes["huge"]["players"], 12)
        self.assertEqual(sizes["gargantuan"]["players"], 24)
        self.assertGreaterEqual(E.max_players(), 24)

    def test_validate_fixes_and_errors(self):
        m = E.blank_map(12, 10, "Grassland")
        m["tiles"][0][1] = ["Forest", "Hill"]           # reordered: hills first
        m["tiles"][1][1] = ["Jungle", "Nonsense"]       # unknown feature dropped
        m["tiles"][2][0] = "Coast"
        m["tiles"][2][1] = ["Forest"]                   # forest can't be on coast
        m["tiles"][3][6] = "Farm"
        m["tiles"][3][3] = 1                            # river to the east: mirrored onto the neighbour
        m["starts"] = [2, 5, 5]                         # water start dropped, duplicate dropped
        clean, warnings = E.validate_map(m)
        self.assertEqual(clean["tiles"][0][1], ["Hill", "Forest"])
        self.assertEqual(clean["tiles"][2][1], [])
        self.assertEqual(clean["starts"], [5])
        self.assertTrue(clean["tiles"][4][3] & (1 << 3))
        self.assertTrue(any("Nonsense" in w for w in warnings))
        m["tiles"][0][0] = "Lava"
        with self.assertRaises(E.MapError):
            E.validate_map(m)

    def test_game_on_custom_map_fills_missing_starts(self):
        m = small_map(starts=2)
        m["starts"] = m["starts"][:1]
        g = EngineGame.new({"map": m, "players": [{"controller": "bot"}] * 3, "city_states": 2, "seed": 3})
        self.assertEqual(g.config["map_type"], "custom")
        self.assertEqual(len(g.majors()), 3)
        starts = starts_of(g)
        self.assertEqual(sorted(starts), [0, 1, 2])
        x, y = starts[0]
        self.assertEqual(y * m["width"] + x, m["starts"][0])
        self.assertEqual(len(set(starts.values())), 3)

    def test_save_list_load(self):
        clean, _ = E.save_map(dict(small_map(), id="unit-test-map"))
        self.assertEqual(clean["id"], "unit-test-map")
        self.assertIn("unit-test-map", [x["id"] for x in E.list_maps()])
        self.assertEqual(E.load_map("unit-test-map")["width"], 40)
        g = EngineGame.new({"map": "unit-test-map", "seed": 1})
        self.assertEqual(g.config["map"], "unit-test-map")
        E.delete_map("unit-test-map")
        self.assertNotIn("unit-test-map", [x["id"] for x in E.list_maps()])


class ScenarioTests(unittest.TestCase):
    def make(self):
        E.save_map(dict(small_map(), id="scn-map"))
        return EngineGame.new({"map": "scn-map", "players": [{"controller": "human"}, {"controller": "llm"}], "seed": 2,
                               "speed": "Quick"})

    def test_operations(self):
        g = self.make()
        (x0, y0), (x1, y1) = (starts_of(g)[p] for p in (0, 1))
        res = g.apply_ops([
            {"op": "grant_era", "player": "all", "era": "Medieval era"},
            {"op": "found_city", "player": 0, "x": x0, "y": y0, "pop": 6, "buildings": ["Library"]},
            {"op": "found_city", "player": 1, "x": x1, "y": y1},
            {"op": "add_unit", "player": 1, "unit": "Swordsman", "x": x1, "y": y1, "count": 2},
            {"op": "set_player", "player": 1, "gold": 777},
            {"op": "set_relation", "a": 0, "b": 1, "embassies": True, "friends": True},
            {"op": "adopt_policy", "player": 0, "policy": "Aristocracy"},
        ])
        self.assertEqual(len(res), 7)
        ov = g.scenario_overview()
        self.assertEqual(ov["players"][1]["gold"], 777)
        self.assertEqual(ov["players"][0]["era"], "Medieval era")
        self.assertIn("Aristocracy", ov["players"][0]["policies"])
        city = next(c for c in g.view(None)["cities"] if c["owner"] == 0)
        self.assertEqual(city["pop"], 6)
        self.assertIn("Library", city["buildings"])
        rel = ov["relations"][0]
        self.assertTrue(rel["embassies"] and rel["friends"] and rel["met"])
        with self.assertRaises(ActionError):
            g.apply_ops([{"op": "found_city", "player": 0, "x": x1, "y": y1}])
        with self.assertRaises(ActionError):
            g.apply_ops([{"op": "no_such_op"}])

    def test_save_load_launch(self):
        g = self.make()
        x0, y0 = starts_of(g)[0]
        g.apply_ops([{"op": "found_city", "player": 0, "x": x0, "y": y0, "name": "Keep"}])
        summ = g.save_scenario("unit-scn", "Unit scenario", "", [{"type": "script"}, {"type": "bot"}])
        self.assertEqual([s["type"] for s in summ["seats"]], ["script", "bot"])
        data = E.load_scenario("unit-scn")
        self.assertIn("unit-scn", [x["id"] for x in E.list_scenarios()])
        g2 = EngineGame.from_state(data["state"])
        self.assertEqual(city_names(g2), ["Keep"])
        mgr = SessionManager()
        s = mgr.create_from_scenario(data, [{"type": "human"}, {"type": "bot"}], register=False, start=False)
        self.assertEqual([seat.type for seat in s.seats], ["human", "bot"])
        self.assertEqual(s.game.player(1)["controller"], "bot")
        # the scenario itself is untouched by the game
        s.game.apply_ops([{"op": "set_player", "player": 0, "gold": 12345}])
        fresh = EngineGame.from_state(E.load_scenario("unit-scn")["state"])
        self.assertNotEqual(fresh.scenario_overview()["players"][0]["gold"], 12345)
        E.delete_scenario("unit-scn")
        with self.assertRaises(ActionError):
            E.load_scenario("unit-scn")


class ProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        E.save_map(dict(small_map(), id="probe-map"))
        g = EngineGame.new({"map": "probe-map", "players": [{"controller": "human"}, {"controller": "llm"}], "seed": 5,
                            "speed": "Quick"})
        pts = starts_of(g)
        g.apply_ops([{"op": "found_city", "player": i, "x": pts[i][0], "y": pts[i][1]} for i in (0, 1)] +
                    [{"op": "set_player", "player": "all", "gold": 500}, {"op": "meet", "a": 0, "b": 1}])
        g.save_scenario("probe-scn", "Probe scenario", "", [{"type": "script"}, {"type": "llm"}])
        cls.probe = P.validate({"id": "unit-probe", "scenario": "probe-scn", "subject": 1, "counterparty": 0, "cases": [
            {"id": "gift", "kind": "offer", "give": [{"type": "gold", "amount": 100}], "receive": [], "expect": "accept"},
            {"id": "demand", "kind": "offer", "give": [], "receive": [{"type": "gold", "amount": 300}], "expect": "reject"},
            {"id": "too-much", "kind": "offer", "give": [], "receive": [{"type": "gold", "amount": 5000}]},
            {"id": "hello", "kind": "message", "message": "Greetings."},
            {"id": "turn", "kind": "turn", "expect_tools": ["end_turn"]}]})
        cls.scn = E.load_scenario("probe-scn")
        cls.mgr = SessionManager()

    def case(self, cid, **llm):
        c = next(c for c in self.probe["cases"] if c["id"] == cid)
        return P.run_case(self.mgr, self.scn, self.probe, c, {**DRY, **llm})

    def test_offer_outcomes(self):
        self.assertEqual(self.case("gift", dry_run_negotiation="accept")["outcome"], "accept")
        rec = self.case("demand", dry_run_negotiation="reject")
        self.assertEqual(rec["outcome"], "reject")
        self.assertTrue(rec["passed"])
        rec = self.case("demand", dry_run_negotiation="counter")
        self.assertEqual(rec["outcome"], "counter")
        self.assertEqual(len(rec["counter_offers"]), 1)
        self.assertFalse(rec["passed"])

    def test_invalid_case_and_message_and_turn(self):
        rec = self.case("too-much", dry_run_negotiation="accept")
        self.assertEqual(rec["outcome"], "invalid_case")
        self.assertIn("gold", rec["error"])
        self.assertEqual(self.case("hello")["outcome"], "reply")
        rec = self.case("turn")
        self.assertEqual(rec["outcome"], "end_turn")
        self.assertTrue(rec["passed"])
        self.assertIn("end_turn", [c["tool"] for c in rec["tool_calls"]])

    @rust_only
    def test_the_bot_as_the_subject(self):
        """The scripted bot as the subject (the probes' baseline): its answers through EngineGame.answer, its turn as
        a drive of its seat."""
        bot = {"provider": "bot", "aggression": 0.4}
        rec = self.case("gift", **bot)
        self.assertEqual(rec["outcome"], "accept", rec)
        rec = self.case("turn", **bot)
        self.assertEqual((rec["outcome"], rec["error"]), ("end_turn", None), rec)

    def test_each_case_starts_fresh(self):
        def gold(state):
            return EngineGame.from_state(state).scenario_overview()["players"][1]["gold"]
        before = gold(self.scn["state"])
        self.case("gift", dry_run_negotiation="accept")
        self.assertEqual(gold(E.load_scenario("probe-scn")["state"]), before)

    def test_validation(self):
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "probe-scn", "subject": 1, "counterparty": 1, "cases": [{"kind": "turn"}]})
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "probe-scn", "subject": 1, "counterparty": 0, "cases": [{"kind": "offer"}]})
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "missing", "subject": 1, "cases": [{"kind": "turn"}]})


if __name__ == "__main__":
    unittest.main()
