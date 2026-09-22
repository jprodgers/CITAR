import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.engine import cities, tools
from citar.engine.briefing import briefing as turn_briefing
from citar.engine.game import Game
from citar.engine.views import client_view


def new_game():
    return Game.new({"map_size": "duel", "seed": 21, "players": [{"controller": "human"}, {"controller": "human"}]})


def found(g, pid, name):
    s = next(u for u in g.player_units(pid) if u.type == "Settler")
    return g.city(tools.execute(g, pid, "found_city", {"unit_id": s.id, "name": name})["city_id"])


def unmet(g, a, b):
    """Forget any contact between two players (the duel map may start them within sight of each other)."""
    if b in g.player(a).met:
        g.player(a).met.remove(b)
    if a in g.player(b).met:
        g.player(b).met.remove(a)


class EventScrubbingTest(unittest.TestCase):
    def setUp(self):
        self.g = g = new_game()
        found(g, 0, "Alpha")
        g.end_turn(0)
        self.city = found(g, 1, "Omega")
        unmet(g, 0, 1)
        self.rome = g.player(1).name
        cities.complete_construction(g, self.city, "The Pyramids")
        self.ev = next(e for e in reversed(g.s.events) if e["type"] == "wonder_built")

    def text_for(self, pid):
        return next(e for e in self.g.events_for(pid) if e["id"] == self.ev["id"])

    def test_unmet_civ_is_unknown(self):
        ev = self.text_for(0)
        self.assertNotIn(self.rome, ev["text"])
        self.assertNotIn("Omega", ev["text"])
        self.assertIn("Unknown Civilization", ev["text"])
        self.assertIn("Pyramids", ev["text"])
        self.assertIsNone(ev["idx"])
        self.assertNotIn("x", ev)
        self.assertIsNone(ev["data"]["player"])
        # the stored event is untouched
        self.assertIn(self.rome, self.ev["text"])
        self.assertEqual(self.ev["data"]["player"], 1)

    def test_own_civ_is_named(self):
        ev = self.text_for(1)
        self.assertIn(self.rome, ev["text"])
        self.assertIn("Omega", ev["text"])
        self.assertEqual(ev["idx"], self.city.idx)

    def test_named_once_met(self):
        self.g.meet(0, 1)
        ev = self.text_for(0)
        self.assertIn(self.rome, ev["text"])
        self.assertNotIn("Unknown", ev["text"])
        self.assertEqual(ev["idx"], self.city.idx)

    def test_spectator_sees_everything(self):
        ev = self.text_for(None)
        self.assertIn(self.rome, ev["text"])
        self.assertEqual(ev["data"]["player"], 1)

    def test_views_briefings_and_tools_are_scrubbed(self):
        g = self.g
        view_text = " ".join(e["text"] for e in client_view(g, 0)["events"])
        self.assertIn("Unknown Civilization", view_text)
        self.assertNotIn(self.rome, view_text.replace(g.player(0).name, ""))
        tool_text = " ".join(e["text"] for e in tools.execute(g, 0, "get_events", {}))
        self.assertNotIn(self.rome, tool_text.replace(g.player(0).name, ""))
        self.assertIn("Unknown Civilization", turn_briefing(g, 0))
        self.assertIn(self.rome, " ".join(e["text"] for e in client_view(g, None)["events"]))

    def test_possessive_and_sentence_start(self):
        g = self.g
        g.player(1).name = "Aztecs"
        ev = g.emit("combat", "Aztecs's Warrior attacked at (3,4). Omega is burning.", None, player=1)
        self.assertEqual(ev["text"], "Aztecs' Warrior attacked at (3,4). Omega is burning.")
        seen = g.event_view(ev, 0)["text"]
        self.assertEqual(seen, "Unknown Civilization's Warrior attacked at an unknown location. "
                               "An unknown city is burning.")

    def test_city_state_is_unknown_city_state(self):
        g = self.g
        cs = next((p for p in g.s.players if p.kind == "city_state"), None)
        if cs is None:
            self.skipTest("no city-state on this map")
        if cs.id in g.player(0).met:
            g.player(0).met.remove(cs.id)
        ev = g.emit("era", f"{cs.name} has entered the Classical era.", None, player=cs.id)
        self.assertEqual(g.event_view(ev, 0)["text"], "Unknown City-State has entered the Classical era.")

    def test_renamed_civ_old_name_is_hidden(self):
        g = self.g
        g.s.current = 1
        tools.execute(g, 1, "set_civ_name", {"name": "Newland"})
        ev = next(e for e in reversed(g.s.events) if e["type"] == "civ_renamed")
        self.assertNotIn(self.rome, g.event_view(ev, 0)["text"])
        self.assertNotIn("Newland", g.event_view(ev, 0)["text"])


if __name__ == "__main__":
    unittest.main()
