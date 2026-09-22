"""Engine and server features that exist mainly for the human (browser) player: queue editing, move orders that
capture or stall, private events, save listings and route previews."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from citar.engine.game import Game, ActionError
from citar.engine import tools, cities, movement, visibility, automation


def game(**kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "normal", "players": [{"name": "A"}, {"name": "B"}]}
    cfg.update(kw)
    return Game.new(cfg)


def found_capital(g, pid=0):
    s = next(u for u in g.player_units(pid) if u.type == "Settler")
    tools.execute(g, pid, "found_city", {"unit_id": s.id})
    return g.player_cities(pid)[0]


class QueueTests(unittest.TestCase):
    def setUp(self):
        self.g = game()
        self.city = found_capital(self.g)
        self.city.queue = []
        for item in ("Warrior", "Worker", "Monument"):
            tools.execute(self.g, 0, "set_production", {"city_id": self.city.id, "item": item, "append": True})

    def ids(self):
        return list(self.city.queue)

    def test_move_and_remove(self):
        self.assertEqual(self.ids(), ["Warrior", "Worker", "Monument"])
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 2, "action": "up"})
        self.assertEqual(self.ids(), ["Warrior", "Monument", "Worker"])
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 2, "action": "first"})
        self.assertEqual(self.ids(), ["Worker", "Warrior", "Monument"])
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 0, "action": "down"})
        self.assertEqual(self.ids(), ["Warrior", "Worker", "Monument"])
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 1, "action": "remove"})
        self.assertEqual(self.ids(), ["Warrior", "Monument"])
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "action": "clear"})
        self.assertEqual(self.ids(), [])

    def test_stored_production_carries_to_new_front_item(self):
        # progress is kept per item (UnCiv): reordering neither loses nor transfers it
        self.city.progress["Warrior"] = 12
        tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 1, "action": "first"})
        self.assertEqual(self.ids()[0], "Worker")
        self.assertEqual(cities.work_done(self.city, "Warrior"), 12)
        self.assertEqual(cities.work_done(self.city, "Worker"), 0)

    def test_bad_index_is_explained(self):
        with self.assertRaises(ActionError) as cm:
            tools.execute(self.g, 0, "change_queue", {"city_id": self.city.id, "index": 7, "action": "remove"})
        self.assertIn("0-2", str(cm.exception))


class AutoProductionTests(unittest.TestCase):
    def test_enabling_fills_empty_queue_and_turn_processing_refills_it(self):
        g = game()
        city = found_capital(g)
        city.queue = []
        res = tools.execute(g, 0, "set_auto_production", {"city_id": city.id, "enabled": True})
        self.assertTrue(res["auto_production"])
        self.assertTrue(city.queue, res)
        # the queue empties again: the next city turn picks something instead of going idle
        city.queue = []
        cities.start_turn(g, city)
        self.assertTrue(city.queue)
        self.assertTrue(any(e["type"] == "city_auto_production" for e in g.s.events))
        self.assertFalse(any(e["type"] == "city_idle" for e in g.s.events))

    def test_off_by_default(self):
        g = game()
        city = found_capital(g)
        city.queue = []
        cities.start_turn(g, city)
        self.assertFalse(city.queue)
        self.assertTrue(any(e["type"] == "city_idle" for e in g.s.events))


class MoveOrderTests(unittest.TestCase):
    def test_move_order_captures_enemy_civilian(self):
        g = game()
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        nb = next(n for n in g.grid.neighbors(w.idx) if movement.can_stand(g, 0, g.rules.units["Warrior"], n))
        g.create_unit(g.barbarian_id, "Worker", nb)
        visibility.refresh(g, force=True)
        x, y = g.grid.xy(nb)
        res = tools.execute(g, 0, "move_unit", {"unit_id": w.id, "x": x, "y": y})
        self.assertTrue(res["arrived"], res)
        captured = [u for u in g.units_at(nb) if u.type == "Worker"]   # UnCiv replaces a captured unit with a new one
        self.assertEqual([u.owner for u in captured], [0])

    def test_blocked_standing_order_waits_then_gives_up_and_reports(self):
        g = game(barbarians="off")
        city = found_capital(g)
        units = [u for u in g.player_units(0) if g.rules.units[u.type]["_military"]]
        blocker, walker = units[0], units[1] if len(units) > 1 else g.create_unit(0, "Warrior", city.idx)
        g.place_unit(blocker, city.idx)
        # right next to the city, heading into it: the only step left is the blocked one
        near = next(n for n in g.grid.neighbors(city.idx) if movement.can_stand(g, 0, g.rules.units["Warrior"], n))
        g.place_unit(walker, near)
        walker.activity, walker.goto = "goto", city.idx
        # held up: the order waits for the way to clear, for ORDER_PATIENCE turns, then gives up and says so
        for _ in range(movement.ORDER_PATIENCE - 1):
            walker.moves = movement.max_moves(g, walker)
            automation.run_unit_orders(g, 0)
            self.assertEqual((walker.activity, walker.goto), ("goto", city.idx))
            self.assertFalse(any(e["type"] == "orders_interrupted" for e in g.s.events))
        walker.moves = movement.max_moves(g, walker)
        automation.run_unit_orders(g, 0)
        self.assertIsNone(walker.activity)
        self.assertIsNone(walker.goto)
        self.assertTrue(any(e["type"] == "orders_interrupted" and "blocked" in e["text"] for e in g.s.events))

    def _long_order(self):
        """A warrior with a standing order to a tile several turns away, and its planned route."""
        g = game(barbarians="off")
        found_capital(g)
        w = next(u for u in g.player_units(0) if u.type == "Warrior")
        pl = g.player(0)
        pl.explored = [True] * len(pl.explored)       # a known route: nothing hidden in the fog to stop it
        dest = next(n for n in sorted(g.grid.within(w.idx, 7), key=lambda n: -g.grid.distance(n, w.idx))
                    if movement.find_path(g, w, n) and len(movement.find_path(g, w, n)) >= 6)
        x, y = g.grid.xy(dest)
        tools.execute(g, 0, "move_unit", {"unit_id": w.id, "x": x, "y": y})
        return g, w, dest

    def test_standing_order_follows_its_original_route(self):
        g, w, dest = self._long_order()
        route = list(w.path)
        self.assertEqual((route[-1], w.goto, w.activity), (dest, dest, "goto"))
        while w.goto is not None:
            w.moves = movement.max_moves(g, w)
            automation.run_unit_orders(g, 0)
            self.assertIn(w.idx, route, "a standing order never leaves the route it was given")
            if w.goto is not None:
                self.assertEqual(w.path, route)
        self.assertEqual(w.idx, dest)

    def test_unit_on_the_route_holds_the_order_up_instead_of_cancelling_it(self):
        g, w, dest = self._long_order()
        route = list(w.path)
        nxt = route[route.index(w.idx) + 1]
        other = next(p for p in g.majors() if p.id != 0).id
        # a neutral foreign unit steps onto the next tile of the route
        blocker = g.create_unit(other, "Warrior", nxt)
        w.moves = movement.max_moves(g, w)
        here = w.idx
        automation.run_unit_orders(g, 0)
        self.assertEqual((w.idx, w.goto, w.path), (here, dest, route), "it waits, order and route intact")
        self.assertFalse(any(e["type"] == "orders_interrupted" for e in g.s.events))
        g.remove_unit(blocker)
        g.invalidate()
        w.moves = movement.max_moves(g, w)
        automation.run_unit_orders(g, 0)
        self.assertNotEqual(w.idx, here, "and carries on along the same route once the way is clear")
        self.assertIn(w.idx, route)

    def test_a_new_order_plans_a_new_route(self):
        g, w, dest = self._long_order()
        back = w.path[0]
        w.moves = movement.max_moves(g, w)
        x, y = g.grid.xy(back)
        tools.execute(g, 0, "move_unit", {"unit_id": w.id, "x": x, "y": y})
        self.assertTrue(w.goto is None or w.path[-1] == back)


class WorkerSafetyTests(unittest.TestCase):
    def _setup(self, automated):
        g = game()
        city = found_capital(g)
        worker = g.create_unit(0, "Worker", city.idx)
        spot = next(n for n in g.grid.within(city.idx, 2) if g.grid.distance(n, city.idx) == 2
                    and movement.can_stand(g, 0, g.rules.units["Worker"], n) and not g.is_water(n))
        g.place_unit(worker, spot)
        worker.activity = "automate" if automated else "build"
        g.s.tiles[spot].build = [["Farm", 5]]
        raider_spot = next(n for n in g.grid.within(spot, 2) if g.grid.distance(n, spot) == 2
                           and movement.can_stand(g, g.barbarian_id, g.rules.units["Warrior"], n))
        g.create_unit(g.barbarian_id, "Warrior", raider_spot)
        return g, city, worker

    def test_automated_worker_abandons_build_and_retreats(self):
        g, city, worker = self._setup(automated=True)
        visibility.refresh(g, force=True)
        automation.run_unit_orders(g, 0)
        self.assertEqual(worker.activity, "automate")
        self.assertLess(g.grid.distance(worker.idx, city.idx), 2)

    def test_hand_ordered_worker_stops_and_asks(self):
        g, city, worker = self._setup(automated=False)
        visibility.refresh(g, force=True)
        automation.run_unit_orders(g, 0)
        self.assertIsNone(worker.activity)
        self.assertTrue(any(e["type"] == "unit_woke" and "stopped building" in e["text"] for e in g.s.events))


class EventPrivacyTests(unittest.TestCase):
    def test_city_production_is_not_shared_with_observers(self):
        g = game(barbarians="off")
        city = found_capital(g)
        # player 1 can see the city tile
        g.create_unit(1, "Warrior", next(n for n in g.grid.neighbors(city.idx)
                                          if g.is_land(n) and not g.units_at(n)))
        visibility.refresh(g, force=True)
        self.assertIn(city.idx, visibility.visible_tiles(g, 1))
        private = g.emit("unit_built", "Cap trained a Warrior.", [0], idx=city.idx)
        public = g.emit("unit_killed", "Something died.", [0], idx=city.idx)
        self.assertEqual(private["players"], [0])
        self.assertIn(1, public["players"])


class SaveListTests(unittest.TestCase):
    def test_save_listing_has_game_details_and_delete(self):
        from citar.server import session as sess
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(sess, "SAVE_DIR", Path(tmp)):
                m = sess.SessionManager()
                s = m.create({"map_size": "duel", "seed": 3, "barbarians": "off", "name": "Listing test"},
                             [{"type": "human"}, {"type": "bot"}])
                s.paused = True
                s.save("manual")
                saves = [x for x in m.list_saves() if x["game_id"] == s.id]
                m.delete(s.id)
                self.assertTrue(saves)
                entry = saves[0]
                self.assertEqual(entry["game_name"], s.name)
                self.assertEqual([p["seat"] for p in entry["players"]], ["human", "bot"])
                self.assertEqual(entry["turn"], 1)
                removed = m.delete_save(entry["path"], whole_game=True)
                self.assertEqual(len(removed), len(saves))
                self.assertFalse((Path(tmp) / s.id).exists())
                with self.assertRaises(KeyError):
                    m.delete_save("../outside.citar")

    def test_delete_save_when_the_save_directory_resolves_elsewhere(self):
        """A save directory whose resolved form differs from the path CITAR holds.

        This is every macOS temporary directory (/var is a symlink to /private/var), a Windows 8.3
        short path, and any save directory reached through a symlink or junction. Deleting a save
        used to raise "is not in the subpath of" on exactly those machines and nowhere else, which
        is why it reached CI before it reached anybody's laptop.

        Reproduced portably by pointing SAVE_DIR at a path with a redundant segment, so that
        `resolve()` gives a different string from the one held.
        """
        from citar.server import session as sess
        with tempfile.TemporaryDirectory() as tmp:
            indirect = Path(tmp) / "sub" / ".."
            (Path(tmp) / "sub").mkdir()
            self.assertNotEqual(str(indirect), str(indirect.resolve()), "the test needs the two to differ")
            with mock.patch.object(sess, "SAVE_DIR", indirect):
                m = sess.SessionManager()
                s = m.create({"map_size": "duel", "seed": 4, "barbarians": "off", "name": "Symlinked"},
                             [{"type": "human"}, {"type": "bot"}])
                s.paused = True
                s.save("manual")
                entry = [x for x in m.list_saves() if x["game_id"] == s.id][0]
                m.delete(s.id)
                removed = m.delete_save(entry["path"], whole_game=True)
                self.assertTrue(removed)
                self.assertFalse((indirect / s.id).exists())
                # The escape check still has to work through the indirection.
                with self.assertRaises(KeyError):
                    m.delete_save("../../outside.citar")


class PathPreviewTests(unittest.TestCase):
    def test_path_endpoint_returns_route_and_turns(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        with tempfile.TemporaryDirectory() as tmp:
            from citar.server import session as sess
            with mock.patch.object(sess, "SAVE_DIR", Path(tmp)):
                client = TestClient(appmod.app)
                s = appmod.manager.create({"map_size": "duel", "seed": 5, "barbarians": "off"}, [{"type": "human"}, {"type": "bot"}])
                try:
                    g = s.game
                    token = s.seats[0].token
                    w = next(u for u in g.player_units(0) if u.type == "Warrior")
                    dest = next(n for n in g.grid.within(w.idx, 3) if g.grid.distance(n, w.idx) == 3
                                and movement.find_path(g, w, n))
                    x, y = g.grid.xy(dest)
                    r = client.get(f"/api/games/{s.id}/path", params={"token": token, "unit_id": w.id, "x": x, "y": y}).json()
                    self.assertEqual(r["path"][0], list(g.grid.xy(w.idx)))
                    self.assertEqual(r["path"][-1], [x, y])
                    self.assertGreaterEqual(r["turns"], 1)
                    # someone else's unit: no route
                    enemy = next(u for u in g.player_units(1))
                    r2 = client.get(f"/api/games/{s.id}/path", params={"token": token, "unit_id": enemy.id, "x": x, "y": y}).json()
                    self.assertIsNone(r2["path"])
                finally:
                    appmod.manager.delete(s.id)


if __name__ == "__main__":
    unittest.main()
