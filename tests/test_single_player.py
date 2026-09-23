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


class RecapturedCivilianTests(unittest.TestCase):
    def setUp(self):
        from citar.engine import units as unitmod, diplomacy
        self.unitmod, self.diplomacy = unitmod, diplomacy
        g = self.g = new_game(barbarians="normal")
        # a worker of player 1's, taken by barbarians, then freed by player 0
        w = g.create_unit(1, "Worker", next(u for u in g.player_units(1) if u.type == "Settler").idx)
        barb = g.create_unit(g.barbarian_id, "Brute", g.player_units(0)[0].idx)
        unitmod.capture_civilian(g, barb, w)
        self.held = next(u for u in g.player_units(g.barbarian_id) if u.type == "Worker")
        self.captor = next(u for u in g.player_units(0) if u.type == "Warrior")
        unitmod.capture_civilian(g, self.captor, self.held)
        self.freed = next(u for u in g.player_units(0) if u.type == "Worker")

    def test_offer_is_made_and_shown(self):
        g = self.g
        self.assertEqual(self.freed.return_offer, 1)
        self.assertTrue(any(e["type"] == "civilian_recaptured" for e in g.s.events))
        from citar.engine.briefing import alert_items
        self.assertTrue(any(a["type"] == "return_civilian" and a["unit"] == self.freed.id for a in alert_items(g, 0)))
        self.assertIn("return_offer", views.unit_info(g, self.freed, 0))

    def test_return_gives_it_back_with_goodwill(self):
        g = self.g
        before = self.diplomacy.opinion(g, 1, 0)
        r = tools.execute(g, 0, "return_civilian", {"unit_id": self.freed.id})
        self.assertEqual(r["returned"], "Worker")
        self.assertIsNone(g.unit(self.freed.id))
        self.assertTrue(any(u.type == "Worker" for u in g.player_units(1)))
        self.assertGreater(self.diplomacy.opinion(g, 1, 0), before)

    def test_keep_or_ignore_keeps_it(self):
        g = self.g
        tools.execute(g, 0, "return_civilian", {"unit_id": self.freed.id, "keep": True})
        self.assertEqual(g.unit(self.freed.id).owner, 0)
        self.assertIsNone(g.unit(self.freed.id).return_offer)
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "return_civilian", {"unit_id": self.freed.id})

    def test_offer_lapses_at_end_of_turn(self):
        g = self.g
        tools.execute(g, 0, "end_turn", {})
        self.assertIsNone(g.unit(self.freed.id).return_offer)

    def test_own_civilian_comes_back_without_a_question(self):
        g = self.g
        w = g.create_unit(0, "Worker", self.captor.idx)
        barb = g.create_unit(g.barbarian_id, "Brute", self.captor.idx)
        self.unitmod.capture_civilian(g, barb, w)
        held = next(u for u in g.player_units(g.barbarian_id) if u.type == "Worker" and u.original_owner == 0)
        self.unitmod.capture_civilian(g, self.captor, held)
        self.assertTrue(all(u.return_offer is None for u in g.player_units(0) if u.id != self.freed.id))


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
