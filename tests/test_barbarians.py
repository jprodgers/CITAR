"""Barbarian behaviour: sacking, aggression, pillage priority, civilian capture, and the aggression config."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine.game import Game
from citar.engine import barbarians, cities, combat, movement, visibility, units as unitmod


def game(barbs="normal", **kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": barbs, "city_states": 0,
           "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    g = Game.new(cfg)
    for pid in (0, 1):
        s = next(u for u in g.player_units(pid) if u.type == "Settler")
        cities.found_city(g, pid, s.idx, f"Cap{pid}", unit=s)
    g.s.turn = 200                     # past the turn barbarians may enter civilizations' land
    for u in list(g.player_units(0)) + list(g.player_units(1)):
        g.remove_unit(u)               # no defenders in the way unless a test puts them there
    visibility.refresh(g, force=True)
    return g


def unit(g, pid, utype, idx):
    """A unit ready to act this turn (new units start with no movement)."""
    u = g.create_unit(pid, utype, idx)
    unitmod.start_turn(g, u)
    visibility.refresh(g, force=True)
    return u


def land_ring(g, center, dist, ud="Warrior", pid=None, exclude=()):
    """Free land tiles at exactly this distance from a tile."""
    pid = g.barbarian_id if pid is None else pid
    out = [n for n in g.grid.within(center, dist) if g.grid.distance(n, center) == dist and n not in exclude
           and g.is_land(n) and movement.can_stand(g, pid, g.rules.units[ud], n) and g.city_at(n) is None]
    if not out:
        raise AssertionError("no free tile")
    return out


class SackTests(unittest.TestCase):
    def test_barbarians_sack_instead_of_capturing(self):
        g = game()
        city = g.player_cities(0)[0]
        cities.add_population(g, city, 3)
        g.player(0).gold = 400
        spot = land_ring(g, city.idx, 1)[0]
        b = unit(g, g.barbarian_id, "Warrior", spot)
        city.health = 1
        res = combat.attack(g, b, city.idx)
        self.assertEqual(city.owner, 0)
        self.assertIs(g.city(city.id), city)
        self.assertEqual(res.get("sacked_city"), city.name)
        self.assertGreater(res["gold_stolen"], 0)
        self.assertEqual(g.player(0).gold, 400 - res["gold_stolen"])
        self.assertGreater(city.health, 1)
        self.assertNotIn("captured_city", res)
        self.assertTrue(any(e["type"] == "city_sacked" for e in g.s.events))

    def test_a_sacked_city_is_left_alone_for_a_while(self):
        g = game(barbarian_aggression=100)
        city = g.player_cities(0)[0]
        g.player(0).gold = 400
        self.assertEqual(barbarians.sack_cooldown(g), 5)
        first = barbarians.sack_city(g, city)
        self.assertEqual(first["sacked_city"], city.name)
        gold = g.player(0).gold
        again = barbarians.sack_city(g, city)
        self.assertIsNone(again["sacked_city"])
        self.assertEqual(g.player(0).gold, gold)
        # the barbarian AI does not even try: the city is no target while picked clean
        city.health = 1
        b = unit(g, g.barbarian_id, "Warrior", land_ring(g, city.idx, 1)[0])
        self.assertFalse(any(g.city_at(t[1]) is city for t in barbarians._attack_targets(g, b)))
        g.s.turn += 5
        self.assertTrue(any(g.city_at(t[1]) is city for t in barbarians._attack_targets(g, b)))

    def test_sack_never_burns_wonders_or_the_palace(self):
        g = game(barbarian_aggression=100)
        city = g.player_cities(0)[0]
        cities.add_population(g, city, 4)
        wonder = next(n for n, b in g.rules.buildings.items() if b.get("isWonder"))
        city.buildings.append(wonder)
        for turn in range(10):
            g.s.turn = 200 + turn
            barbarians.sack_city(g, city)
        self.assertIn(wonder, city.buildings)
        self.assertEqual(g.player(0).capital, city.id)
        self.assertEqual(city.owner, 0)
        self.assertGreaterEqual(city.pop, 1)


class AggressionTests(unittest.TestCase):
    def test_config_and_defaults(self):
        g = game()
        self.assertEqual(barbarians.aggression(g), g.rules.const["barbarians"]["levels"]["normal"]["aggression"])
        self.assertGreater(barbarians.aggression(game("raging")), barbarians.aggression(g))
        self.assertEqual(barbarians.aggression(game(barbarian_aggression=150)), 100)
        self.assertEqual(barbarians.aggression(game(barbarian_aggression=0)), 0)
        with self.assertRaises(ValueError):
            Game.new({"map_size": "duel", "seed": 21, "barbarian_aggression": "lots"})
        self.assertEqual(g.rules.to_client()["barbarian_aggression"]["raging"],
                         g.rules.const["barbarians"]["levels"]["raging"]["aggression"])

    def test_knobs_grow_with_aggression(self):
        lo, hi = game(barbarian_aggression=0), game(barbarian_aggression=100)
        self.assertLess(barbarians.seek_radius(lo), barbarians.seek_radius(hi))
        self.assertGreater(barbarians.attack_bar(lo), barbarians.attack_bar(hi))
        self.assertGreaterEqual(barbarians._siege_size(lo), barbarians._siege_size(hi))
        self.assertLess(barbarians._max_near_camp(lo), barbarians._max_near_camp(hi))

    def _city_attack(self, aggr):
        g = game(barbarian_aggression=aggr)
        city = g.player_cities(0)[0]
        spot = land_ring(g, city.idx, 1)[0]
        b = unit(g, g.barbarian_id, "Warrior", spot)
        hp = city.health
        barbarians._automate(g, b)
        return city.health < hp

    def test_aggression_makes_barbarians_storm_cities(self):
        self.assertFalse(self._city_attack(0))
        self.assertTrue(self._city_attack(100))

    def test_aggressive_barbarians_hunt_from_afar(self):
        g = game(barbarian_aggression=100)
        city = g.player_cities(0)[0]
        w = g.create_unit(0, "Worker", land_ring(g, city.idx, 2, "Worker", pid=0)[0])
        spot = land_ring(g, w.idx, 5)[0]
        b = unit(g, g.barbarian_id, "Warrior", spot)
        before = g.grid.distance(b.idx, w.idx)
        barbarians._automate(g, b)
        self.assertTrue(g.unit(w.id) is None or g.grid.distance(b.idx, w.idx) < before)


class PillageTests(unittest.TestCase):
    def test_resource_improvements_first(self):
        g = game()
        lux = next(r for r, d in g.rules.resources.items() if d["resourceType"] == "Luxury")
        bid = g.barbarian_id
        b = None
        spots = [(c, i) for c in g.player_cities(0) + g.player_cities(1) for d in (1, 2) for i in land_ring(g, c.idx, d)]
        for c, spot in spots:
            b = unit(g, bid, "Warrior", spot)
            reach = movement.reachable_this_turn(g, b)
            near = sorted(n for n, left in reach.items() if left > 0 and g.is_land(n) and g.city_at(n) is None
                          and g.s.tiles[n].owner == c.owner and not g.units_at(n))
            if len(near) >= 2:
                break
            g.remove_unit(b)
        farm, plant = near[0], near[1]
        for i in [spot] + near:
            g.s.tiles[i].improvement = None
            g.s.tiles[i].resource = None
            g.s.tiles[i].route = None
        g.s.tiles[farm].improvement = "Farm"
        g.s.tiles[plant].improvement = "Plantation"
        g.s.tiles[plant].resource = lux
        g.invalidate()
        self.assertEqual(barbarians.pillage_value(g, g.barbarian_id, plant), 3)
        self.assertEqual(barbarians.pillage_value(g, g.barbarian_id, farm), 2)
        b.hp = 40
        self.assertTrue(barbarians._try_pillage(g, b))
        self.assertTrue(g.s.tiles[plant].pillaged)
        self.assertFalse(g.s.tiles[farm].pillaged)
        self.assertGreater(b.hp, 40)            # pillaging heals


class CivilianCaptureTests(unittest.TestCase):
    def test_capture_and_carry_to_camp(self):
        g = game(barbarian_aggression=85)
        bid = g.barbarian_id
        city = g.player_cities(0)[0]
        wspot = land_ring(g, city.idx, 2, "Worker", pid=0)[0]
        w = g.create_unit(0, "Worker", wspot)
        b = unit(g, bid, "Warrior", land_ring(g, wspot, 1)[0])
        barbarians._automate(g, b)
        self.assertIsNone(g.unit(w.id))
        captive = next(u for u in g.player_units(bid) if u.type == "Worker")
        # a camp a few tiles away: the captive heads there
        camp_idx = next(i for i in land_ring(g, captive.idx, 4) if g.s.tiles[i].owner is None
                        and not g.s.tiles[i].improvement)
        barbarians.create_camp(g, camp_idx)
        before = g.grid.distance(captive.idx, camp_idx)
        unitmod.start_turn(g, captive)
        barbarians._automate(g, captive)
        self.assertLess(g.grid.distance(captive.idx, camp_idx), before)


class TurnTests(unittest.TestCase):
    def test_raging_barbarians_play_turns(self):
        g = Game.new({"map_size": "duel", "seed": 21, "barbarians": "raging", "city_states": 0,
                      "players": [{"controller": "bot"}, {"controller": "bot"}]})
        moved = False
        for _ in range(3):
            g.s.turn += 10
            barbarians.update_camps(g)
            start = {u.id: u.idx for u in g.player_units(g.barbarian_id)}
            for u in g.player_units(g.barbarian_id):
                unitmod.start_turn(g, u)
            barbarians.take_turn(g)
            moved = moved or any(g.unit(i) is not None and g.unit(i).idx != idx for i, idx in start.items())
        self.assertTrue(moved)


if __name__ == "__main__":
    unittest.main()
