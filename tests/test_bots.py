"""The balance simulator plays the scripted bot: bots that expand, research and defend on a duel map.

The bot's decisions are tested in the rule scripts (tests/rules/bot_*.toml) and crates/citar-testkit/tests/bot/."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest


class BotTests(unittest.TestCase):
    def test_bots_expand_research_and_defend(self):
        from citar.balance import play_game
        r = play_game({"seed": 7, "map_type": "pangaea", "size": "duel", "players": 2, "turns": 60, "speed": "Quick",
                       "barbarians": "normal", "seat_bots": ["basic", "basic"]})
        self.assertEqual(r["errors"], [])
        for p in r["players"].values():
            self.assertGreaterEqual(p["checkpoints"][50]["cities"], 2)
            self.assertGreaterEqual(p["techs"], 6)


if __name__ == "__main__":
    unittest.main()
