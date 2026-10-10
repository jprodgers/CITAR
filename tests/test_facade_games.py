"""Engine behaviours checked through the facade, on whichever backend runs (crates/citar-engine/DESIGN.md P2.7.4).

These are the successors of Python engine tests that read the engine's internals and had no rule script to take
their place: what each pinned down is asked here through ``citar.engine_api`` alone, so the same test holds the
Python engine today and the Rust engine from package 2-08 on, and outlives the Python engine (package 2-12).
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar import engine_api
from citar.engine_api import EngineGame


def duel(**kw) -> EngineGame:
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "normal", "city_states": 0,
           "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    return EngineGame.new(cfg)


class RulesetTests(unittest.TestCase):
    def test_the_benchmark_civilization_has_no_abilities(self):
        # tests/test_engine.py RulesTests: a seat that names no nation plays BenchmarkCiv, whose
        # abilities would leak into every rule script and benchmark otherwise
        rules = engine_api.rules_client()
        self.assertIn("BenchmarkCiv", rules["major_nations"])
        self.assertFalse(rules["nations"]["BenchmarkCiv"].get("uniques"))
        for table in ("units", "buildings"):
            mine = [name for name, row in rules[table].items() if row.get("uniqueTo") == "BenchmarkCiv"]
            self.assertEqual(mine, [], table)


class SettingsTests(unittest.TestCase):
    def test_a_speed_sets_the_turn_limit_and_the_calendar_starts_in_4000_bc(self):
        # tests/test_engine.py TurnTests.test_year_and_speed
        g = duel(speed="Quick")
        self.assertEqual(g.turn_limit, 330)
        self.assertEqual(g.view(0)["year"], "4000 BC")
        self.assertGreater(duel().turn_limit, 330, "the standard speed has more turns")

    def test_a_later_starting_era_disables_religion(self):
        # tests/test_mechanics.py RuleGapTests: an era whose unique says so starts a game without religion
        self.assertTrue(duel().execute(0, "get_religion", {})["enabled"])
        self.assertFalse(duel(starting_era="Industrial era").execute(0, "get_religion", {})["enabled"])

    def test_the_default_difficulty_is_prince_everywhere(self):
        # tests/test_mechanics.py DifficultyTests
        g = duel(players=[{"controller": "bot"}, {"controller": "human"}])
        self.assertEqual([p["difficulty"] for p in g.majors()], ["Prince", "Prince"])
        self.assertEqual((g.config["difficulty"], g.config["barbarian_difficulty"]), ("Prince", "Prince"))

    def test_bot_seats_get_their_own_difficultys_ai_bonuses(self):
        # tests/test_mechanics.py DifficultyTests: Deity's free techs and starting units go to that seat alone
        g = duel(players=[{"controller": "bot", "difficulty": "Deity"},
                          {"controller": "bot", "difficulty": "Chieftain"}])
        self.assertEqual([p["difficulty"] for p in g.majors()], ["Deity", "Chieftain"])
        deity, chieftain = g.standing(0), g.standing(1)
        self.assertGreater(deity["techs"], chieftain["techs"])
        self.assertGreater(deity["units"], chieftain["units"])
        human = duel(players=[{"controller": "human", "difficulty": "Deity"},
                              {"controller": "human", "difficulty": "Chieftain"}])
        self.assertEqual(human.standing(0)["techs"], human.standing(1)["techs"], "the AI's bonuses are for bots")

    def test_a_seats_difficulty_survives_a_save(self):
        # tests/test_mechanics.py DifficultyTests.test_seat_difficulty_survives_save
        g = duel(players=[{"controller": "bot", "difficulty": "King"}, {"controller": "human"}])
        back = EngineGame.from_save(g.to_save())
        self.assertEqual([p["difficulty"] for p in back.majors()], ["King", "Prince"])

    def test_a_new_game_never_repeats_a_colour(self):
        # tests/test_single_player.py ColorTests: the first seat keeps the colour it asked for, the others get
        # colours of their own
        g = EngineGame.new({"map_size": "small", "seed": 3,
                            "players": [{"controller": "human", "color": "#123456"}] * 4})
        colours = [p["color"] for p in g.majors()]
        self.assertEqual(colours[0], "#123456")
        self.assertEqual(len(set(colours)), 4)


class HeadlessTests(unittest.TestCase):
    def test_a_bot_game_runs(self):
        # tests/test_engine.py SimulationTests, through citar sim on the facade's runner
        from citar import sim
        r = sim.run(players=3, turns=60, map_size="duel", seed=4, verbose=False)
        self.assertEqual(r["errors"], [])
        self.assertEqual(r["phase"], "over")
        self.assertTrue(all(p["cities"] >= 1 for p in r["players"] if p["kind"] == "major" and p["alive"]))

    def test_one_seed_plays_one_game(self):
        # citar sim and citar balance play the same game for the same seed: the bots draw from the game's seed
        from citar import balance, sim
        runs = [sim.run(players=3, turns=25, map_size="duel", seed=4, verbose=False) for _ in range(2)]
        self.assertEqual(runs[0]["stats"], runs[1]["stats"])
        self.assertEqual(runs[0]["players"], runs[1]["players"])
        spec = {"seed": 4, "map_type": "continents", "size": "duel", "players": 3, "turns": 25, "speed": "Quick",
                "barbarians": "normal", "seat_bots": ["standard", "classic-production"]}
        games = [balance.play_game(spec) for _ in range(2)]
        for g in games:
            self.assertEqual(g.pop("errors"), [])
            g.pop("seconds")
        self.assertEqual(games[0], games[1])


if __name__ == "__main__":
    unittest.main()
