"""Targeted tests for mechanics that bot simulations rarely reach."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine.game import Game, ActionError
from citar.engine import (tools, cities, movement, research, units as unitmod, victory, workers, economy, policies,
                          religion, city_states, espionage, visibility)


def game(**kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off", "city_states": 2,
           "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    return Game.new(cfg)


def found_capitals(g):
    for pid in (0, 1):
        s = next(u for u in g.player_units(pid) if u.type == "Settler")
        cities.found_city(g, pid, s.idx, f"Cap{pid}", unit=s)


def free_land_near(g, idx, pid, utype="Warrior", exclude=()):
    for n in g.grid.within(idx, 4)[1:]:
        if n in exclude:
            continue
        if movement.can_stand(g, pid, g.rules.units[utype], n):
            return n
    raise AssertionError("no free tile")


def grant(g, pid, *techs):
    for t in techs:
        for x in research.path_to(g, pid, t) or [t]:
            research.add_tech_silently(g, pid, x)


class CaptureTests(unittest.TestCase):
    def test_capture_city_and_domination(self):
        g = game()
        found_capitals(g)
        g.meet(0, 1)
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        target = g.player_cities(1)[0]
        for u in list(g.player_units(1)):
            g.remove_unit(u)
        grant(g, 0, "Iron Working")
        spot = next(n for n in g.grid.neighbors(target.idx) if movement.can_stand(g, 0, g.rules.units["Warrior"], n))
        u = g.create_unit(0, "Warrior", spot)
        u.moves = movement.max_moves(g, u)
        target.health = 1
        x, y = g.grid.xy(target.idx)
        res = tools.execute(g, 0, "attack", {"unit_id": u.id, "x": x, "y": y})
        self.assertEqual(res.get("captured_city"), "Cap1")
        self.assertEqual(target.owner, 0)
        self.assertTrue(target.puppet or res.get("result") == "recaptured")
        self.assertFalse(g.player(1).alive)
        self.assertEqual(g.s.phase, "over")
        self.assertEqual(g.s.victory, "Domination")

    def test_puppet_annex_and_raze_rules(self):
        g = game()
        found_capitals(g)
        own = g.player_cities(0)[0]
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "city_status", {"city_id": own.id, "status": "raze"})
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "city_status", {"city_id": own.id, "status": "annex"})


class ScienceVictoryTests(unittest.TestCase):
    def test_spaceship(self):
        g = game()
        found_capitals(g)
        for t in g.rules.techs:
            research.add_tech_silently(g, 0, t)
        cap = g.player_cities(0)[0]
        self.assertTrue(cities.rejection_reasons(g, cap, "SS Booster"))      # needs the Apollo Program first
        cities.complete_construction(g, cap, "Apollo Program")
        self.assertIn("Apollo Program", cap.buildings)
        for part in victory.required_parts(g).elements():
            u = g.create_unit(0, part, cap.idx)
            u.moves = movement.max_moves(g, u)
            tools.execute(g, 0, "unit_action", {"unit_id": u.id, "action": "add_to_spaceship"})
        self.assertEqual(g.s.victory, "Scientific")


class UnitTests(unittest.TestCase):
    def test_upgrade_and_promotion(self):
        g = game()
        found_capitals(g)
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        cap = g.player_cities(0)[0]
        g.place_unit(w, cap.idx)
        grant(g, 0, "Iron Working")
        target, err, cost = unitmod.check_upgrade(g, w)
        self.assertEqual(target, "Swordsman")
        self.assertIn("Iron", err or "")         # no iron yet
        w.xp = 12
        opts = unitmod.available_promotions(g, w)
        self.assertIn("Shock I", opts)
        tools.execute(g, 0, "promote_unit", {"unit_id": w.id, "promotion": "Shock I"})
        self.assertIn("Shock I", w.promotions)
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "promote_unit", {"unit_id": w.id, "promotion": "Drill I"})

    def test_embark_requires_optics(self):
        g = game()
        found_capitals(g)
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        water = next(i for i in range(g.grid.size) if g.s.tiles[i].terrain == "Coast" and not g.s.tiles[i].features
                     and any(movement.can_stand(g, 0, g.rules.units["Warrior"], n) for n in g.grid.neighbors(i)))
        land = next(n for n in g.grid.neighbors(water) if movement.can_stand(g, 0, g.rules.units["Warrior"], n))
        g.place_unit(w, land)
        w.moves = movement.max_moves(g, w)
        self.assertIsNotNone(movement.step(g, w, water))
        grant(g, 0, "Optics")
        g.clear_static()
        self.assertIsNone(movement.step(g, w, water))
        self.assertTrue(movement.is_embarked(g, w))


class WorkerTests(unittest.TestCase):
    def test_build_road_pillage_and_repair(self):
        g = game()
        found_capitals(g)
        cap = g.player_cities(0)[0]
        grant(g, 0, "The Wheel")
        spot = next(n for n in g.grid.neighbors(cap.idx) if movement.can_stand(g, 0, g.rules.units["Worker"], n)
                    and g.s.tiles[n].owner == 0)
        wk = g.create_unit(0, "Worker", spot)
        wk.moves = movement.max_moves(g, wk)
        res = tools.execute(g, 0, "build_improvement", {"unit_id": wk.id, "improvement": "Road"})
        for _ in range(res["turns"]):
            wk.moves = 60
            workers.progress_builds(g, 0)
        self.assertEqual(g.s.tiles[spot].route, "Road")
        g.meet(0, 1)
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        g.remove_unit(wk)
        raider = g.create_unit(1, "Warrior", spot)
        raider.moves = movement.max_moves(g, raider)
        workers.pillage(g, raider)
        self.assertTrue(g.s.tiles[spot].route_pillaged)
        g.remove_unit(raider)
        wk = g.create_unit(0, "Worker", spot)
        wk.moves = 60
        res = tools.execute(g, 0, "build_improvement", {"unit_id": wk.id, "improvement": "repair"})
        for _ in range(res["turns"]):
            wk.moves = 60
            workers.progress_builds(g, 0)
        self.assertFalse(g.s.tiles[spot].route_pillaged)

    def test_forest_needs_removal_before_farm(self):
        g = game()
        found_capitals(g)
        grant(g, 0, "Agriculture", "Mining")
        cap = g.player_cities(0)[0]
        forest = next((i for i in cities.city_tiles(g, cap) if g.s.tiles[i].features == ["Forest"]
                       and not g.s.tiles[i].resource and not g.units_at(i)), None)
        if forest is None:
            self.skipTest("no forest near the capital for this seed")
        wk = g.create_unit(0, "Worker", forest)
        wk.moves = 60
        res = tools.execute(g, 0, "build_improvement", {"unit_id": wk.id, "improvement": "Farm"})
        self.assertEqual(res["queue"], ["Remove Forest", "Farm"])


class EconomyTests(unittest.TestCase):
    def test_buy_and_happiness(self):
        g = game()
        found_capitals(g)
        cap = g.player_cities(0)[0]
        g.player(0).gold = 1000
        tools.execute(g, 0, "buy", {"city_id": cap.id, "item": "Monument"})
        self.assertIn("Monument", cap.buildings)
        self.assertLess(g.player(0).gold, 1000)
        g.invalidate()
        h = economy.happiness(g, 0)
        self.assertEqual(h["total"], int(round(sum(h["breakdown"].values()))))
        self.assertIn("Cities", h["breakdown"])

    def test_turn_limit_time_victory(self):
        g = game(turn_limit=3)
        found_capitals(g)
        while g.s.phase == "playing":
            tools.execute(g, g.s.current, "end_turn", {})
        self.assertEqual(g.s.victory, "Time")
        self.assertEqual(len(g.s.stats), 3)
        self.assertEqual(len(g.frames), 3)


class PolicyTests(unittest.TestCase):
    def test_adopt_branch_and_policy(self):
        g = game()
        found_capitals(g)
        cap = g.player_cities(0)[0]
        base = cities.city_stats(g, cap)["total"]["culture"]
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "adopt_policy", {"policy": "Tradition"})
        g.player(0).culture = 1000
        tools.execute(g, 0, "adopt_policy", {"policy": "Tradition"})
        g.invalidate()
        self.assertGreater(cities.city_stats(g, cap)["total"]["culture"], base)
        cost1 = policies.culture_cost(g, 0)
        tools.execute(g, 0, "adopt_policy", {"policy": "Aristocracy"})
        self.assertIn("Aristocracy", g.player(0).policies)
        self.assertGreater(policies.culture_cost(g, 0), cost1)
        with self.assertRaises(ActionError):       # Monarchy needs Legalism... any unmet requirement is refused
            tools.execute(g, 0, "adopt_policy", {"policy": "Freedom"})


class ReligionTests(unittest.TestCase):
    def test_pantheon_and_religion(self):
        g = game()
        found_capitals(g)
        p = g.player(0)
        p.faith = religion.faith_for_pantheon(g, 0)
        belief = religion.beliefs_available(g, "Pantheon")[0]
        tools.execute(g, 0, "found_pantheon", {"belief": belief})
        self.assertEqual(p.religion_state, "pantheon")
        cap = g.player_cities(0)[0]
        prophet = g.create_unit(0, "Great Prophet", cap.idx)
        prophet.moves = 60
        need = religion.beliefs_to_choose(g, 0, enhancing=False)
        beliefs = religion.ai_choose_beliefs(g, 0, need)
        tools.execute(g, 0, "unit_action", {"unit_id": prophet.id, "action": "found_religion", "name": "The Way",
                                            "beliefs": beliefs})
        self.assertEqual(p.religion_state, "religion")
        self.assertEqual(religion.display_name(g, religion.majority_religion(g, cap)), "The Way")
        self.assertTrue(religion.is_holy_city(g, cap))


class GreatPeopleTests(unittest.TestCase):
    def test_scientist_and_artist(self):
        g = game()
        found_capitals(g)
        cap = g.player_cities(0)[0]
        tools.execute(g, 0, "set_research", {"tech": "Writing"})
        sci = g.create_unit(0, "Great Scientist", cap.idx)
        sci.moves = 60
        g.player(0).flags["science_last8"] = [40] * 8
        before = len(g.player(0).techs)
        tools.execute(g, 0, "unit_action", {"unit_id": sci.id, "action": "hurry_research"})
        self.assertIsNone(g.unit(sci.id))
        self.assertGreater(len(g.player(0).techs), before)
        art = g.create_unit(0, "Great Artist", cap.idx)
        art.moves = 60
        act = next(a for a in tools.execute(g, 0, "get_unit", {"unit_id": art.id})["actions"] if a["id"].startswith("trigger:"))
        tools.execute(g, 0, "unit_action", {"unit_id": art.id, "action": act["id"]})
        self.assertGreater(g.player(0).golden_age_turns, 0)


class CityStateTests(unittest.TestCase):
    def test_gift_gold_makes_friends(self):
        g = game()
        found_capitals(g)
        cs = g.city_states()[0]
        g.meet(0, cs.id)
        g.player(0).gold = 2000
        before = city_states.influence(g, cs.id, 0)
        tools.execute(g, 0, "city_state_action", {"player_id": cs.id, "action": "gift_gold", "amount": 1000})
        self.assertGreater(city_states.influence(g, cs.id, 0), before + 30)
        self.assertIn(city_states.relationship(g, cs.id, 0), ("Friend", "Ally"))


class EspionageTests(unittest.TestCase):
    def test_spy_counter_intelligence(self):
        g = game()
        found_capitals(g)
        spy = espionage.add_spy(g, 0)
        cap = g.player_cities(0)[0]
        tools.execute(g, 0, "move_spy", {"spy": spy["name"], "city_id": cap.id})
        espionage.end_turn(g, 0)
        self.assertEqual(spy["action"], "Counter-intelligence")


class DiplomaticVictoryTests(unittest.TestCase):
    def test_united_nations_vote(self):
        g = game()
        found_capitals(g)
        cap = g.player_cities(0)[0]
        for t in g.rules.techs:
            research.add_tech_silently(g, 0, t)
        cities.complete_construction(g, cap, "United Nations")
        un = victory._un(g)
        self.assertIsNotNone(un["next_vote"])
        for q in g.s.players:
            if q.kind != "barbarian":
                victory._un(g)["votes"][str(q.id)] = 0
        g.s.turn = un["next_vote"]
        victory.end_round(g)
        self.assertIn(0, un["won"])
        self.assertEqual(g.s.victory, "Diplomatic")


class NaturalWonderTests(unittest.TestCase):
    def test_discovery(self):
        g = game(map_size="small", players=[{}, {}, {}, {}])
        w = next((i for i, t in enumerate(g.s.tiles) if t.wonder), None)
        if w is None:
            self.skipTest("no natural wonder on this map")
        u = g.player_units(0)[0]
        g.place_unit(u, next(n for n in g.grid.within(w, 2) if movement.can_stand(g, 0, g.rules.units[u.type], n)))
        visibility.refresh(g, force=True)
        self.assertIn(g.s.tiles[w].wonder, g.player(0).natural_wonders)


class RuleGapTests(unittest.TestCase):
    def test_paratrooper_paradrop(self):
        g = game()
        found_capitals(g)
        grant(g, 0, "Radar")
        cap = g.player_cities(0)[0]
        para = g.create_unit(0, "Paratrooper", cap.idx)
        para.moves = movement.max_moves(g, para)
        ids = [a["id"] for a in tools.execute(g, 0, "get_unit", {"unit_id": para.id})["actions"]]
        self.assertIn("paradrop", ids)
        for i in range(g.grid.size):
            g.player(0).explored[i] = 1
        far = next(n for n in g.grid.within(cap.idx, 5) if g.grid.distance(n, cap.idx) == 5
                   and movement.can_stand(g, 0, g.rules.units["Paratrooper"], n) and g.is_land(n))
        too_far = next(n for n in g.grid.within(cap.idx, 6) if g.grid.distance(n, cap.idx) == 6 and g.is_land(n))
        x, y = g.grid.xy(too_far)
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "unit_action", {"unit_id": para.id, "action": "paradrop", "x": x, "y": y})
        x, y = g.grid.xy(far)
        tools.execute(g, 0, "unit_action", {"unit_id": para.id, "action": "paradrop", "x": x, "y": y})
        self.assertEqual(para.idx, far)
        self.assertEqual(para.moves, 0)

    def test_fountain_of_youth_promotes_adjacent_units(self):
        g = game()
        spot = next(i for i in range(g.grid.size) if g.is_land(i) and not g.units_at(i)
                    and g.s.tiles[i].owner is None and g.grid.distance(i, g.player_units(0)[0].idx) > 3)
        g.s.tiles[spot].wonder = "Fountain of Youth"
        g.clear_static()
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        nb = next(n for n in g.grid.neighbors(spot) if movement.can_stand(g, 0, g.rules.units["Warrior"], n))
        g.place_unit(w, nb)
        movement.on_enter_tile(g, w, nb)
        self.assertIn("Rejuvenation", w.promotions)

    def test_later_start_era_disables_religion(self):
        self.assertTrue(game().religion_enabled)
        self.assertFalse(game(starting_era="Industrial era").religion_enabled)


class DifficultyTests(unittest.TestCase):
    def _game(self, **kw):
        cfg = {"map_size": "duel", "seed": 21, "barbarians": "normal", "city_states": 0}
        cfg.update(kw)
        return Game.new(cfg)

    def test_default_is_prince_everywhere(self):
        g = self._game(players=[{"controller": "bot"}, {"controller": "human"}])
        self.assertEqual([p.difficulty for p in g.majors()], ["Prince", "Prince"])
        self.assertEqual(g.s.config["barbarian_difficulty"], "Prince")

    def test_bot_seats_get_their_own_ai_bonuses(self):
        g = self._game(players=[{"controller": "bot", "difficulty": "Deity"}, {"controller": "bot", "difficulty": "Chieftain"}])
        deity, chief = g.player(0), g.player(1)
        self.assertEqual((deity.difficulty, chief.difficulty), ("Deity", "Chieftain"))
        for t in g.rules.difficulties["Deity"]["aiFreeTechs"]:
            self.assertIn(t, deity.techs)
            self.assertNotIn(t, chief.techs)
        self.assertGreater(len(g.player_units(0)), len(g.player_units(1)))       # Deity bonus starting units
        self.assertLess(cities.production_cost(g, 0, "Warrior"), cities.production_cost(g, 1, "Warrior"))
        self.assertLess(cities.production_cost(g, 0, "Monument"), cities.production_cost(g, 1, "Monument"))

    def test_human_seats_get_player_values(self):
        g = self._game(players=[{"controller": "human", "difficulty": "Settler"},
                                {"controller": "llm", "difficulty": "Deity"}])
        self.assertGreater(economy.difficulty(g, 0)["baseHappiness"], economy.difficulty(g, 1)["baseHappiness"])
        self.assertLess(research.tech_cost(g, 0, "Pottery"), research.tech_cost(g, 1, "Pottery"))
        # AI bonuses are for bots only
        self.assertEqual(len(g.player(1).techs), len(g.player(0).techs))

    def test_barbarian_difficulty(self):
        from citar.engine import combat
        g = self._game(players=[{"controller": "human"}, {"controller": "human"}], barbarian_difficulty="Chieftain")
        found_capitals(g)
        cap = g.player_cities(0)[0]
        inside = next(n for n in g.grid.neighbors(cap.idx) if g.s.tiles[n].owner == 0 and g.is_land(n))
        self.assertFalse(g.can_enter_territory(g.barbarian_id, inside))    # Chieftain: not before turn 60
        g.s.turn = 61
        self.assertTrue(g.can_enter_territory(g.barbarian_id, inside))
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        spot = next(n for n in g.grid.within(w.idx, 3) if movement.can_stand(g, g.barbarian_id, g.rules.units["Warrior"], n)
                    and g.grid.distance(n, w.idx) == 1)
        g.create_unit(g.barbarian_id, "Warrior", spot)
        visibility.refresh(g, force=True)
        self.assertIn("Difficulty +50%", combat.preview(g, w, spot)["attacker_modifiers"])

    def test_seat_difficulty_survives_save(self):
        from citar.engine.state import GameState
        g = self._game(players=[{"controller": "bot", "difficulty": "King"}, {"controller": "human"}])
        g2 = Game(GameState.from_dict(g.s.to_dict()))
        self.assertEqual(g2.player(0).difficulty, "King")


if __name__ == "__main__":
    unittest.main()


class PerCivLimitTests(unittest.TestCase):
    """Items "Limited to [n] per Civilization" (spaceship parts, the Recycling Center)."""

    def _space_ready_city(self):
        from unittest import mock
        g = game()
        s = next(u for u in g.player_units(0) if u.type == "Settler")
        tools.execute(g, 0, "found_city", {"unit_id": s.id})
        city = g.player_cities(0)[0]
        p = g.player(0)
        for t in g.rules.techs:
            p.techs.add(t) if isinstance(p.techs, set) else p.techs.append(t)
        cities.add_building(g, city, "Apollo Program")
        city.queue = []
        g.invalidate()
        patch = mock.patch.object(type(g), "resource_amount", lambda self, pid, res: 9)
        patch.start()
        self.addCleanup(patch.stop)
        return g, city

    def test_the_last_allowed_part_stays_in_the_queue(self):
        g, city = self._space_ready_city()
        # limited to 1 and none built: it used to count its own place in the queue and be dropped at once
        tools.execute(g, 0, "set_production", {"city_id": city.id, "item": "SS Cockpit"})
        cities.validate_queue(g, city)
        self.assertEqual(city.queue[:1], ["SS Cockpit"])
        # limited to 3 with two built: the third one may be built
        for _ in range(2):
            g.create_unit(0, "SS Booster", city.idx)
        tools.execute(g, 0, "set_production", {"city_id": city.id, "item": "SS Booster"})
        cities.validate_queue(g, city)
        self.assertIn("SS Booster", city.queue)
        self.assertEqual(cities.count_constructed(g, 0, "SS Booster"), 3)
        self.assertEqual(cities.count_constructed(g, 0, "SS Booster", exclude=city), 2)

    def test_the_limit_still_holds(self):
        g, city = self._space_ready_city()
        for _ in range(3):
            g.create_unit(0, "SS Booster", city.idx)
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "set_production", {"city_id": city.id, "item": "SS Booster"})
