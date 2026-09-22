import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine.game import Game, ActionError, unique_colors, colors_clash
from citar.engine import tools, views
from citar.engine.state import PLAYER_COLORS


def new_game(**kw):
    cfg = {"map_size": "duel", "seed": 21, "players": [{"controller": "human"}, {"controller": "human"}]}
    cfg.update(kw)
    return Game.new(cfg)


class ResearchQueueTests(unittest.TestCase):
    def test_append_adds_to_the_end_with_missing_prerequisites(self):
        g = new_game()
        tools.execute(g, 0, "set_research", {"tech": "Pottery"})
        r = tools.execute(g, 0, "set_research", {"tech": "Mathematics", "append": True})
        q = g.player(0).research_queue
        self.assertEqual(q[0], "Pottery")
        self.assertEqual(q[-1], "Mathematics")
        self.assertIn("The Wheel", q)
        self.assertLess(q.index("The Wheel"), q.index("Mathematics"))
        self.assertEqual(r["queue"], q)
        # appending something already queued is refused rather than duplicated
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "set_research", {"tech": "Mathematics", "append": True})

    def test_plain_set_replaces_the_queue(self):
        g = new_game()
        tools.execute(g, 0, "set_research", {"tech": "Pottery"})
        tools.execute(g, 0, "set_research", {"tech": "Sailing", "append": True})
        tools.execute(g, 0, "set_research", {"tech": "Mining"})
        self.assertEqual(g.player(0).research_queue, ["Mining"])

    def test_dequeue_removes_dependents(self):
        g = new_game()
        tools.execute(g, 0, "set_research", {"tech": "Pottery"})
        tools.execute(g, 0, "set_research", {"tech": "Mathematics", "append": True})
        tools.execute(g, 0, "set_research", {"tech": "Sailing", "append": True})
        r = tools.execute(g, 0, "dequeue_research", {"tech": "The Wheel"})
        q = g.player(0).research_queue
        self.assertNotIn("The Wheel", q)
        self.assertNotIn("Mathematics", q)
        self.assertIn("Sailing", q)
        self.assertIn("Mathematics", r["removed"])
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "dequeue_research", {"tech": "Writing"})


class ColorTests(unittest.TestCase):
    def test_palette_is_distinct(self):
        self.assertEqual(len(set(PLAYER_COLORS)), len(PLAYER_COLORS))
        for i, a in enumerate(PLAYER_COLORS):
            for b in PLAYER_COLORS[i + 1:]:
                self.assertFalse(colors_clash(a, b), (a, b))

    def test_first_come_first_served(self):
        out = unique_colors(["#aa0000", "#aa0000", None, "#ab0101", "#00AA00"])
        self.assertEqual(out[0], "#aa0000")
        self.assertEqual(out[4], "#00aa00")          # an explicit request beats an earlier seat left on automatic
        self.assertEqual(len(set(out)), 5)
        for i, a in enumerate(out):
            for b in out[i + 1:]:
                self.assertFalse(colors_clash(a, b))

    def test_new_game_never_repeats_a_color(self):
        g = Game.new({"map_size": "small", "seed": 3,
                      "players": [{"controller": "human", "color": "#123456"}] * 4})
        colors = [p.color for p in g.majors()]
        self.assertEqual(colors[0], "#123456")
        self.assertEqual(len(set(colors)), 4)


class BombardViewTests(unittest.TestCase):
    def test_city_reports_whether_it_can_still_bombard(self):
        g = new_game()
        s = next(u for u in g.player_units(0) if u.type == "Settler")
        c = g.city(tools.execute(g, 0, "found_city", {"unit_id": s.id})["city_id"])
        self.assertTrue(views.city_info(g, c, 0)["can_bombard"])
        self.assertNotIn("can_bombard", views.city_info(g, c, 1))
        c.attacked = True
        self.assertFalse(views.city_info(g, c, 0)["can_bombard"])


if __name__ == "__main__":
    unittest.main()
