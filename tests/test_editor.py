"""Map editor files, scenarios (operations, save/load, launch) and probes (with the dry-run model)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine import maps, scenario as S
from citar.engine.game import Game, ActionError
from citar.engine.rules import get_rules
from citar import probes as P
from citar.server.session import SessionManager

DRY = {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0}


def small_map(name="unit-map", starts=2):
    m = maps.generated_map(get_rules(), 40, 26, "pangaea", starts, 2, seed=11, name=name)
    return m


class MapTests(unittest.TestCase):
    def test_big_sizes_exist(self):
        R = get_rules()
        self.assertEqual(R.const["map_sizes"]["huge"]["players"], 12)
        self.assertEqual(R.const["map_sizes"]["gargantuan"]["players"], 24)
        self.assertGreaterEqual(R.const["max_players"], 24)

    def test_validate_fixes_and_errors(self):
        R = get_rules()
        m = maps.blank_map(12, 10, "Grassland")
        m["tiles"][0][1] = ["Forest", "Hill"]           # reordered: hills first
        m["tiles"][1][1] = ["Jungle", "Nonsense"]       # unknown feature dropped
        m["tiles"][2][0] = "Coast"
        m["tiles"][2][1] = ["Forest"]                   # forest can't be on coast
        m["tiles"][3][6] = "Farm"
        m["tiles"][3][3] = 1                            # river to the east: mirrored onto the neighbour
        m["starts"] = [2, 5, 5]                         # water start dropped, duplicate dropped
        clean, warnings = maps.validate(R, m)
        self.assertEqual(clean["tiles"][0][1], ["Hill", "Forest"])
        self.assertEqual(clean["tiles"][2][1], [])
        self.assertEqual(clean["starts"], [5])
        self.assertTrue(clean["tiles"][4][3] & (1 << 3))
        self.assertTrue(any("Nonsense" in w for w in warnings))
        m["tiles"][0][0] = "Lava"
        with self.assertRaises(maps.MapError):
            maps.validate(R, m)

    def test_game_on_custom_map_fills_missing_starts(self):
        m = small_map(starts=2)
        m["starts"] = m["starts"][:1]
        g = Game.new({"map": m, "players": [{"controller": "bot"}] * 3, "city_states": 2, "seed": 3})
        self.assertEqual(g.s.config["map_type"], "custom")
        self.assertEqual(len(g.majors()), 3)
        starts = [p.flags["start"] for p in g.majors()]
        self.assertEqual(starts[0], m["starts"][0])
        self.assertEqual(len(set(starts)), 3)

    def test_save_list_load(self):
        R = get_rules()
        clean, _ = maps.save_map(R, dict(small_map(), id="unit-test-map"))
        self.assertIn("unit-test-map", [x["id"] for x in maps.list_maps()])
        self.assertEqual(maps.load_map("unit-test-map")["width"], 40)
        g = Game.new({"map": "unit-test-map", "seed": 1})
        self.assertEqual(g.s.config["map"], "unit-test-map")
        maps.delete_map("unit-test-map")


class ScenarioTests(unittest.TestCase):
    def make(self):
        R = get_rules()
        maps.save_map(R, dict(small_map(), id="scn-map"))
        return Game.new({"map": "scn-map", "players": [{"controller": "human"}, {"controller": "llm"}], "seed": 2,
                         "speed": "Quick"})

    def test_operations(self):
        g = self.make()
        x0, y0 = g.grid.xy(g.player(0).flags["start"])
        x1, y1 = g.grid.xy(g.player(1).flags["start"])
        res = S.apply_ops(g, [
            {"op": "grant_era", "player": "all", "era": "Medieval era"},
            {"op": "found_city", "player": 0, "x": x0, "y": y0, "pop": 6, "buildings": ["Library"]},
            {"op": "found_city", "player": 1, "x": x1, "y": y1},
            {"op": "add_unit", "player": 1, "unit": "Swordsman", "x": x1, "y": y1, "count": 2},
            {"op": "set_player", "player": 1, "gold": 777},
            {"op": "set_relation", "a": 0, "b": 1, "embassies": True, "friends": True},
            {"op": "adopt_policy", "player": 0, "policy": "Aristocracy"},
        ])
        self.assertEqual(len(res), 7)
        ov = S.overview(g)
        self.assertEqual(ov["players"][1]["gold"], 777)
        self.assertEqual(ov["players"][0]["era"], "Medieval era")
        self.assertIn("Aristocracy", g.player(0).policies)
        city = next(c for c in g.s.cities.values() if c.owner == 0)
        self.assertEqual(city.pop, 6)
        self.assertIn("Library", city.buildings)
        rel = ov["relations"][0]
        self.assertTrue(rel["embassies"] and rel["friends"] and rel["met"])
        with self.assertRaises(ActionError):
            S.apply_ops(g, [{"op": "found_city", "player": 0, "x": x1, "y": y1}])
        with self.assertRaises(ActionError):
            S.apply_ops(g, [{"op": "no_such_op"}])

    def test_save_load_launch(self):
        g = self.make()
        x0, y0 = g.grid.xy(g.player(0).flags["start"])
        S.apply_ops(g, [{"op": "found_city", "player": 0, "x": x0, "y": y0, "name": "Keep"}])
        summ = S.save_scenario(g, "unit-scn", "Unit scenario", "", [{"type": "script"}, {"type": "bot"}])
        self.assertEqual([s["type"] for s in summ["seats"]], ["script", "bot"])
        data = S.load_scenario("unit-scn")
        g2 = S.game_from_state(data["state"])
        self.assertEqual([c.name for c in g2.s.cities.values()], ["Keep"])
        mgr = SessionManager()
        s = mgr.create_from_scenario(data, [{"type": "human"}, {"type": "bot"}], register=False, start=False)
        self.assertEqual([seat.type for seat in s.seats], ["human", "bot"])
        self.assertEqual(s.game.player(1)["controller"], "bot")
        # the scenario itself is untouched by the game
        s.game.apply_ops([{"op": "set_player", "player": 0, "gold": 12345}])
        self.assertNotEqual(S.game_from_state(S.load_scenario("unit-scn")["state"]).player(0).gold, 12345)
        S.delete_scenario("unit-scn")


class ProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        R = get_rules()
        maps.save_map(R, dict(small_map(), id="probe-map"))
        g = Game.new({"map": "probe-map", "players": [{"controller": "human"}, {"controller": "llm"}], "seed": 5,
                      "speed": "Quick"})
        pts = [g.grid.xy(g.player(i).flags["start"]) for i in (0, 1)]
        S.apply_ops(g, [{"op": "found_city", "player": i, "x": x, "y": y} for i, (x, y) in enumerate(pts)] +
                    [{"op": "set_player", "player": "all", "gold": 500}, {"op": "meet", "a": 0, "b": 1}])
        S.save_scenario(g, "probe-scn", "Probe scenario", "", [{"type": "script"}, {"type": "llm"}])
        cls.probe = P.validate({"id": "unit-probe", "scenario": "probe-scn", "subject": 1, "counterparty": 0, "cases": [
            {"id": "gift", "kind": "offer", "give": [{"type": "gold", "amount": 100}], "receive": [], "expect": "accept"},
            {"id": "demand", "kind": "offer", "give": [], "receive": [{"type": "gold", "amount": 300}], "expect": "reject"},
            {"id": "too-much", "kind": "offer", "give": [], "receive": [{"type": "gold", "amount": 5000}]},
            {"id": "hello", "kind": "message", "message": "Greetings."},
            {"id": "turn", "kind": "turn", "expect_tools": ["end_turn"]}]})
        cls.scn = S.load_scenario("probe-scn")
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

    def test_each_case_starts_fresh(self):
        before = self.scn["state"]["players"][1]["gold"]
        self.case("gift", dry_run_negotiation="accept")
        self.assertEqual(S.load_scenario("probe-scn")["state"]["players"][1]["gold"], before)

    def test_validation(self):
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "probe-scn", "subject": 1, "counterparty": 1, "cases": [{"kind": "turn"}]})
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "probe-scn", "subject": 1, "counterparty": 0, "cases": [{"kind": "offer"}]})
        with self.assertRaises(P.ProbeError):
            P.validate({"scenario": "missing", "subject": 1, "cases": [{"kind": "turn"}]})


if __name__ == "__main__":
    unittest.main()
