"""Bot behaviour, the balance simulator, and the harness helpers built for AI players (alerts, exploration)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine.game import Game
from citar.engine import tools, cities, briefing, automation, movement, visibility


def game(**kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off",
           "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    return Game.new(cfg)


def found_capital(g, pid=0):
    s = next(u for u in g.player_units(pid) if u.type == "Settler")
    tools.execute(g, pid, "found_city", {"unit_id": s.id})
    return g.player_cities(pid)[0]


def free_tile(g, center, pid, dist):
    for n in g.grid.within(center, dist):
        if g.grid.distance(n, center) == dist and movement.can_stand(g, pid, g.rules.units["Archer"], n):
            return n
    raise AssertionError("no free tile")


class AlertTests(unittest.TestCase):
    def test_threatened_city_gets_bombard_hint(self):
        g = game(barbarians="normal")
        city = found_capital(g)
        spot = free_tile(g, city.idx, g.barbarian_id, 2)
        g.create_unit(g.barbarian_id, "Archer", spot)
        visibility.refresh(g, force=True)
        alerts = briefing.alerts(g, 0)
        x, y = g.grid.xy(spot)
        self.assertTrue(any("threatened" in a and f"city_attack city_id={city.id} x={x} y={y}" in a for a in alerts), alerts)
        self.assertIn("city_attack", briefing.turn_progress(g, 0))
        self.assertIn("ALERTS", briefing.briefing(g, 0))

    def test_falling_gold_alert(self):
        g = game()
        found_capital(g)
        cap = g.player_cities(0)[0].idx
        spots = [n for n in g.grid.within(cap, 6) if movement.can_stand(g, 0, g.rules.units["Warrior"], n)]
        for n in spots[:30]:
            g.create_unit(0, "Warrior", n)
        g.invalidate()
        items = briefing.alert_items(g, 0)
        self.assertTrue(any(a["type"] == "gold" for a in items), items)


class ExploreTests(unittest.TestCase):
    def test_explorer_heads_for_distant_frontier(self):
        g = game()
        scout = next(u for u in g.player_units(0) if u.type == "Warrior")
        # everything within 16 tiles is already mapped: the old radius-limited search would give up
        near = set(g.grid.within(scout.idx, 16))
        for i in near:
            g.player(0).explored[i] = 1
        target = automation.explore_target(g, scout)
        self.assertIsNotNone(target)
        self.assertTrue(any(not g.player(0).explored[n] for n in g.grid.neighbors(target)))

    def test_explorer_avoids_hostile_units(self):
        g = game(barbarians="normal")
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        scout = g.create_unit(0, "Scout", w.idx)     # healthy military units ignore danger; recon units don't
        g.remove_unit(w)
        target = automation.explore_target(g, scout)
        spot = free_tile(g, target, g.barbarian_id, 1)
        g.create_unit(g.barbarian_id, "Warrior", spot)
        visibility.refresh(g, force=True)
        visibility.visible_tiles(g, 0).add(spot)      # as if the scout could see it
        new_target = automation.explore_target(g, scout)
        self.assertNotEqual(new_target, target)


class CityHealTests(unittest.TestCase):
    def test_city_heals_20_per_turn(self):
        g = game()
        city = found_capital(g)
        city.health = 100
        cities.end_turn(g, city)
        self.assertEqual(city.health, 120)


class BotTests(unittest.TestCase):
    def test_bots_expand_research_and_defend(self):
        from citar.balance import play_game
        r = play_game({"seed": 7, "map_type": "pangaea", "size": "duel", "players": 2, "turns": 60, "speed": "Quick",
                       "barbarians": "normal", "seat_bots": ["basic", "basic"]})
        self.assertEqual(r["errors"], [])
        for p in r["players"].values():
            self.assertGreaterEqual(p["checkpoints"][50]["cities"], 2)
            self.assertGreaterEqual(p["techs"], 6)

    def test_aggressive_bot_conquers_a_defenceless_neighbour(self):
        # Aggression 0.9, because since the v2 defaults a middling bot (0.5) out-expands its neighbour on this
        # map instead of attacking it: 19 cities by T200 and no war declared at all. See docs/research/BOT_TUNING.md.
        from citar.bots.basic import BasicBot
        from citar.balance import IdleBot
        from citar.sim import resolve_negotiations
        g = Game.new({"map_type": "pangaea", "map_size": "duel", "seed": 1001, "barbarians": "normal",
                      "players": [{"controller": "bot"}, {"controller": "bot"}], "turn_limit": 200, "speed": "Quick"})
        bots = {0: BasicBot(aggression=0.9, seed=1), 1: IdleBot()}
        while g.s.phase == "playing":
            pid = g.s.current
            bots[pid].play_turn(g, pid, end_turn=False)
            resolve_negotiations(g, bots)
            if g.s.phase == "playing" and g.s.current == pid:
                g.end_turn(pid)
        self.assertEqual((g.s.winner, g.s.victory), (0, "Domination"))


if __name__ == "__main__":
    unittest.main()
