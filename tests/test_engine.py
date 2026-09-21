import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine.game import Game, ActionError
from citar.engine import tools, cities, combat, research, movement, uniques
from citar.engine.state import GameState
from citar.engine.hexmap import HexGrid
from citar.engine.mapgen import MAP_TYPES
from citar.engine.rules import get_rules


def new_game(**kw):
    cfg = {"map_size": "duel", "seed": 21, "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    return Game.new(cfg)


def settler_of(g, pid):
    return next(u for u in g.player_units(pid) if u.type == "Settler")


def found(g, pid, name=None):
    s = settler_of(g, pid)
    return g.city(tools.execute(g, pid, "found_city", {"unit_id": s.id, "name": name})["city_id"])


def end_round(g):
    """End the turn of every human seat until it is player 0's turn again."""
    start = g.turn
    while True:
        tools.execute(g, g.s.current, "end_turn", {})
        if g.s.current == 0 and g.turn > start:
            return


class HexTests(unittest.TestCase):
    def test_neighbors_are_distance_one(self):
        grid = HexGrid(20, 15)
        for idx in range(grid.size):
            for n in grid.neighbors(idx):
                self.assertEqual(grid.distance(idx, n), 1)
            self.assertEqual(len(grid.ring(idx, 1)), len(grid.neighbors(idx)))

    def test_line_endpoints(self):
        grid = HexGrid(20, 15)
        line = grid.line(grid.idx(2, 3), grid.idx(10, 9))
        self.assertEqual(line[0], grid.idx(2, 3))
        self.assertEqual(line[-1], grid.idx(10, 9))
        for a, b in zip(line, line[1:]):
            self.assertEqual(grid.distance(a, b), 1)


class RulesTests(unittest.TestCase):
    def test_ruleset_loaded(self):
        R = get_rules()
        self.assertIn("Warrior", R.units)
        self.assertIn("Granary", R.buildings)
        self.assertIn("Tradition", R.policy_branches)
        self.assertIn("BenchmarkCiv", R.major_nations)
        self.assertGreater(len(R.major_nations), 20)
        self.assertGreater(len(R.city_state_nations), 20)
        self.assertEqual(R.max_turns["Quick"], 330)

    def test_benchmark_civ_has_no_abilities(self):
        R = get_rules()
        self.assertEqual(R.nations["BenchmarkCiv"].get("uniques", []), [])
        self.assertEqual(R.unique_units.get("BenchmarkCiv", []), [])
        self.assertEqual(R.unique_buildings.get("BenchmarkCiv", []), [])

    def test_resolve_names_and_ids(self):
        R = get_rules()
        self.assertEqual(R.resolve("tech", "bronze_working"), "Bronze Working")
        self.assertEqual(R.resolve("tech", "Bronze Working"), "Bronze Working")
        self.assertIsNone(R.resolve("tech", "Warp Drive"))

    def test_unique_placeholders(self):
        u = uniques.Unique("[+15]% Strength <when attacking>", "test", "x")
        self.assertEqual(u.ph, "[]% Strength")
        self.assertEqual(u.n(0), 15)
        self.assertEqual(u.mods[0].ph, "when attacking")

    def test_multi_filter(self):
        self.assertTrue(uniques.multi_filter("{Military} {Land}", lambda s: s in ("Military", "Land")))
        self.assertFalse(uniques.multi_filter("{Military} {Water}", lambda s: s in ("Military", "Land")))
        self.assertTrue(uniques.multi_filter("non-[Water]", lambda s: s == "Land"))


class MapTests(unittest.TestCase):
    def test_all_map_types_generate(self):
        for mt in MAP_TYPES:
            g = new_game(map_type=mt, map_size="small", players=[{}, {}, {}, {}])
            self.assertEqual(len(g.majors()), 4)
            for p in g.majors():
                self.assertTrue(any(u.type == "Settler" for u in g.player_units(p.id)), (mt, p.name))
            terrains = {t.terrain for t in g.s.tiles}
            self.assertTrue({"Ocean", "Coast"} <= terrains)
            self.assertTrue(terrains & {"Grassland", "Plains"})

    def test_rivers_resources_wonders_ruins(self):
        g = new_game(map_size="small", players=[{}, {}, {}, {}])
        self.assertTrue(any(t.river for t in g.s.tiles))
        kinds = {g.rules.resources[t.resource]["resourceType"] for t in g.s.tiles if t.resource}
        self.assertEqual(kinds, {"Bonus", "Strategic", "Luxury"})
        self.assertTrue(any(t.resource_amount for t in g.s.tiles if t.resource and
                            g.rules.resources[t.resource]["resourceType"] == "Strategic"))
        self.assertTrue(any(t.improvement == "Ancient ruins" for t in g.s.tiles))

    def test_river_edges_are_symmetric(self):
        g = new_game(map_size="small", players=[{}, {}, {}, {}])
        for i, t in enumerate(g.s.tiles):
            for d in range(6):
                if t.river & (1 << d):
                    n = g.grid.neighbor_in_dir(i, d)
                    self.assertIsNotNone(n)
                    self.assertTrue(g.s.tiles[n].river & (1 << ((d + 3) % 6)))

    def test_deterministic_seed(self):
        a, b = new_game(), new_game()
        self.assertEqual([t.to_list() for t in a.s.tiles], [t.to_list() for t in b.s.tiles])

    def test_city_states_and_nations(self):
        g = new_game(map_size="small", players=[{"nation": "Rome"}, {"nation": "BenchmarkCiv"}, {}, {}])
        self.assertEqual(g.player(0).nation, "Rome")
        self.assertEqual(g.player(1).nation, "BenchmarkCiv")
        self.assertTrue(g.city_states())
        self.assertEqual(len({p.nation for p in g.majors() if p.nation != "BenchmarkCiv"}), 3)


class TurnTests(unittest.TestCase):
    def test_found_city_and_production(self):
        g = new_game()
        c = found(g, 0, "Alpha")
        self.assertEqual(c.name, "Alpha")
        self.assertIn("Palace", c.buildings)
        self.assertFalse(any(u.type == "Settler" for u in g.player_units(0)))
        tools.execute(g, 0, "set_production", {"city_id": c.id, "item": "Warrior"})
        tools.execute(g, 0, "set_research", {"tech": "Pottery"})
        for _ in range(16):
            end_round(g)
        self.assertGreaterEqual(len([u for u in g.player_units(0) if u.type == "Warrior"]), 2)
        self.assertIn("Pottery", g.player(0).techs)
        self.assertGreater(c.pop, 1)

    def test_year_and_speed(self):
        g = new_game(speed="Quick")
        self.assertEqual(g.total_turns(), 330)
        self.assertEqual(g.year_text(), "4000 BC")

    def test_move_without_moves_is_an_error(self):
        g = new_game()
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        tools.execute(g, 0, "unit_order", {"unit_id": w.id, "order": "explore"})
        w.moves = 0
        x, y = g.grid.xy(g.grid.neighbors(w.idx)[0])
        with self.assertRaises(ActionError) as ctx:
            tools.execute(g, 0, "move_unit", {"unit_id": w.id, "x": x, "y": y})
        self.assertIn("no moves left", str(ctx.exception))
        self.assertEqual(w.activity, "explore")

    def test_not_your_turn(self):
        g = new_game()
        with self.assertRaises(ActionError):
            tools.execute(g, 1, "end_turn", {})

    def test_city_min_distance(self):
        g = new_game()
        s = settler_of(g, 0)
        cities.found_city(g, 0, s.idx, "One", unit=s)
        for i in g.grid.ring(s.idx, 2):
            if g.is_land(i):
                self.assertIsNotNone(cities.found_check(g, 0, i))

    def test_save_roundtrip(self):
        g = new_game()
        found(g, 0)
        tools.execute(g, 0, "end_turn", {})
        d = g.s.to_dict()
        g2 = Game(GameState.from_dict(d))
        self.assertEqual(g2.s.to_dict(), d)
        tools.execute(g2, g2.s.current, "end_turn", {})

    def test_worker_builds_farm(self):
        g = new_game()
        c = found(g, 0)
        research.add_tech(g, 0, "Agriculture")
        w = g.create_unit(0, "Worker", c.idx)
        spot = next((i for i in g.grid.neighbors(c.idx) if g.s.tiles[i].terrain in ("Grassland", "Plains")
                     and not g.s.tiles[i].features and not g.s.tiles[i].resource), None)
        if spot is None:
            self.skipTest("no plain tile next to the city")
        g.place_unit(w, spot)
        res = tools.execute(g, 0, "build_improvement", {"unit_id": w.id, "improvement": "Farm"})
        self.assertGreater(res["turns"], 0)
        for _ in range(res["turns"] + 1):
            end_round(g)
            w.moves = 60
        self.assertEqual(g.s.tiles[spot].improvement, "Farm")


class CombatTests(unittest.TestCase):
    def setup_war(self):
        g = new_game(barbarians="off")
        a = next(u for u in g.player_units(0) if u.type == "Warrior")
        spot = next(n for n in g.grid.neighbors(a.idx) if movement.can_stand(g, 1, g.rules.units["Warrior"], n))
        b = g.create_unit(1, "Warrior", spot)
        g.meet(0, 1)
        return g, a, b

    def test_requires_war(self):
        g, a, b = self.setup_war()
        x, y = g.grid.xy(b.idx)
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "attack", {"unit_id": a.id, "x": x, "y": y})
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        pv = tools.execute(g, 0, "preview_attack", {"unit_id": a.id, "x": x, "y": y})
        self.assertGreater(pv["damage_to_defender"][1], 0)
        res = tools.execute(g, 0, "attack", {"unit_id": a.id, "x": x, "y": y})
        self.assertIn("damage_to_defender", res)
        self.assertTrue(b.hp < 100 or res.get("defender_killed"))

    def test_stronger_attacker_deals_more(self):
        g, a, b = self.setup_war()
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        A, B = combat.Combatant(unit=a), combat.Combatant(unit=b)
        even = combat.damage_to_defender(g, A, B, a.idx, 0.5)
        a.type = "Swordsman"
        g.clear_static()
        strong = combat.damage_to_defender(g, A, B, a.idx, 0.5)
        self.assertGreater(strong, even)


class DiplomacyTests(unittest.TestCase):
    def test_negotiation_and_deal(self):
        g = new_game()
        g.meet(0, 1)
        g.player(0).gold = 100
        before = g.player(1).gold
        r = tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "Maps for gold?",
                                                     "give": [{"type": "gold", "amount": 30}], "receive": [{"type": "share_map"}]})
        nid = r["negotiation_id"]
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "respond_negotiation", {"negotiation_id": nid, "action": "accept"})
        r2 = tools.execute(g, 1, "respond_negotiation", {"negotiation_id": nid, "action": "counter", "message": "40",
                                                         "give": [{"type": "share_map"}], "receive": [{"type": "gold", "amount": 40}]})
        self.assertEqual(r2["status"], "open")
        r3 = tools.execute(g, 0, "respond_negotiation", {"negotiation_id": nid, "action": "accept", "message": "Fine."})
        self.assertEqual(r3["status"], "accepted")
        self.assertEqual(g.player(0).gold, 60)
        self.assertEqual(g.player(1).gold, before + 40)

    def test_tech_trade(self):
        g = new_game()
        g.meet(0, 1)
        research.add_tech_silently(g, 0, "Pottery")
        research.add_tech_silently(g, 0, "Calendar")
        research.add_tech_silently(g, 1, "Pottery")
        r = tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "tech", "give": [{"type": "tech", "tech": "Calendar"}]})
        tools.execute(g, 1, "respond_negotiation", {"negotiation_id": r["negotiation_id"], "action": "accept"})
        self.assertIn("Calendar", g.player(1).techs)

    def test_embassies_and_friendship(self):
        g = new_game()
        g.meet(0, 1)
        with self.assertRaises(ActionError):      # embassies need Writing
            tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "hi", "give": [{"type": "embassy"}]})
        for pid in (0, 1):
            research.add_tech_silently(g, pid, "Pottery")
            research.add_tech_silently(g, pid, "Writing")
        found(g, 0)
        tools.execute(g, 0, "end_turn", {})
        found(g, 1)
        tools.execute(g, 1, "end_turn", {})
        r = tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "Embassies?", "give": [{"type": "embassy"}],
                                                     "receive": [{"type": "embassy"}, {"type": "declaration_of_friendship"}]})
        tools.execute(g, 1, "respond_negotiation", {"negotiation_id": r["negotiation_id"], "action": "accept"})
        from citar.engine import diplomacy
        self.assertTrue(diplomacy.shared_embassies(g, 0, 1))
        self.assertTrue(diplomacy.is_friends(g, 0, 1))

    def test_war_and_peace(self):
        g = new_game()
        g.meet(0, 1)
        tools.execute(g, 0, "declare_war", {"player_id": 1, "message": "For glory!"})
        self.assertTrue(g.at_war(0, 1))
        r = tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "peace?", "give": [{"type": "peace_treaty"}]})
        tools.execute(g, 1, "respond_negotiation", {"negotiation_id": r["negotiation_id"], "action": "accept"})
        self.assertFalse(g.at_war(0, 1))
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "declare_war", {"player_id": 1})


class VisibilityTests(unittest.TestCase):
    def test_fog_hides_other_units(self):
        from citar.engine.views import client_view
        g = new_game()
        v = client_view(g, 0)
        others = [u for u in v["units"] if u["owner"] != 0]
        self.assertEqual(others, [])
        self.assertLess(len(v["tiles"]), g.grid.size // 2)

    def test_hills_see_further(self):
        from citar.engine import visibility
        g = new_game()
        flat = next(i for i, t in enumerate(g.s.tiles) if t.terrain == "Grassland" and not t.features
                    and all(not g.s.tiles[n].features and g.s.tiles[n].terrain != "Mountain" for n in g.grid.within(i, 3)))
        seen = visibility.viewable_from(g, flat, 2)
        self.assertEqual(len(seen), len(g.grid.within(flat, 2)))


class SimulationTests(unittest.TestCase):
    def test_bot_game_runs(self):
        from citar import sim
        g = sim.run(players=3, turns=60, map_size="duel", seed=4, verbose=False)
        self.assertEqual(g.s.phase, "over")
        self.assertTrue(all(len(g.player_cities(p.id)) >= 1 for p in g.majors()))


if __name__ == "__main__":
    unittest.main()
