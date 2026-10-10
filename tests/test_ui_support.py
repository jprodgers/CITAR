"""Engine and server features that exist mainly for the human (browser) player: queue editing, move orders that
capture or stall, private events, save listings and route previews.

Through the facade (``citar.engine_api``), so they run on whichever engine is behind it: the players' tools, the
scenario operations and, where a test sets up what no player could, the rule scripts' test operations and inspect
(tests/rules/README.md), which only a build with them has."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from citar.engine_api import ActionError, EngineGame
from tests import has_test_ops

#: Turns a standing order waits on a blocked step before it gives up (movement.ORDER_PATIENCE, both engines).
ORDER_PATIENCE = 3
needs_test_ops = unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")


def game(**kw) -> EngineGame:
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "normal", "players": [{"name": "A"}, {"name": "B"}]}
    cfg.update(kw)
    return EngineGame.new(cfg)


def unit_of(g: EngineGame, pid: int, kind: str) -> dict:
    """One of the player's units of a kind, as their view shows it."""
    return next(u for u in g.view(pid)["units"] if u["owner"] == pid and u["type"] == kind)


def found_capital(g: EngineGame, pid: int = 0) -> dict:
    """The player's settler founds a city where it stands: {"city_id", "x", "y"}."""
    city = g.execute(pid, "found_city", {"unit_id": unit_of(g, pid, "Settler")["id"]})["city_id"]
    c = g.execute(pid, "get_city", {"city_id": city})
    return {"city_id": city, "x": c["x"], "y": c["y"]}


def tiles_at(g: EngineGame, x: int, y: int, distance: int, **filters) -> list:
    """The tiles exactly ``distance`` from (x, y) that pass inspect's find_tiles filters, nearest first."""
    found = g.inspect({"what": "find_tiles", "x": x, "y": y, "radius": distance, **filters})
    return [(t["x"], t["y"]) for t in found if t["distance"] == distance]


def unit(g: EngineGame, uid: int) -> dict:
    return g.inspect({"what": "unit", "unit": uid})


def events_since(g: EngineGame, mark: int, etype: str) -> list:
    return [e for e in g.events()[mark:] if e["type"] == etype]


class QueueTests(unittest.TestCase):
    def setUp(self):
        self.g = game()
        self.city = found_capital(self.g)["city_id"]
        self.g.execute(0, "change_queue", {"city_id": self.city, "action": "clear"})
        for item in ("Warrior", "Worker", "Monument"):
            self.g.execute(0, "set_production", {"city_id": self.city, "item": item, "append": True})

    def queue(self) -> list:
        return self.g.execute(0, "get_city", {"city_id": self.city})["queue"]

    def ids(self):
        return [q["item"] for q in self.queue()]

    def change(self, **args):
        return self.g.execute(0, "change_queue", {"city_id": self.city, **args})

    def test_move_and_remove(self):
        self.assertEqual(self.ids(), ["Warrior", "Worker", "Monument"])
        self.change(index=2, action="up")
        self.assertEqual(self.ids(), ["Warrior", "Monument", "Worker"])
        self.change(index=2, action="first")
        self.assertEqual(self.ids(), ["Worker", "Warrior", "Monument"])
        self.change(index=0, action="down")
        self.assertEqual(self.ids(), ["Warrior", "Worker", "Monument"])
        self.change(index=1, action="remove")
        self.assertEqual(self.ids(), ["Warrior", "Monument"])
        self.change(action="clear")
        self.assertEqual(self.ids(), [])

    def test_stored_production_carries_to_new_front_item(self):
        # progress is kept per item (UnCiv): reordering neither loses nor transfers it
        self.g.execute(0, "end_turn", {})
        self.g.execute(1, "end_turn", {})             # the round ends, and the city's production goes into the Warrior
        stored = {q["item"]: q["progress"] for q in self.queue()}
        self.assertGreater(stored["Warrior"], 0)
        self.change(index=1, action="first")
        self.assertEqual(self.ids()[0], "Worker")
        after = {q["item"]: q["progress"] for q in self.queue()}
        self.assertEqual(after["Warrior"], stored["Warrior"])
        self.assertEqual(after["Worker"], 0)

    def test_bad_index_is_explained(self):
        with self.assertRaises(ActionError) as cm:
            self.change(index=7, action="remove")
        self.assertIn("0-2", str(cm.exception))


class AutoProductionTests(unittest.TestCase):
    def round(self, g):
        """Both majors end their turns: the round ends, and the next begins with the cities' turns."""
        g.execute(0, "end_turn", {})
        g.execute(1, "end_turn", {})

    def test_enabling_fills_empty_queue_and_turn_processing_refills_it(self):
        g = game()
        city = found_capital(g)["city_id"]
        g.execute(0, "change_queue", {"city_id": city, "action": "clear"})
        res = g.execute(0, "set_auto_production", {"city_id": city, "enabled": True})
        self.assertTrue(res["auto_production"])
        self.assertTrue(g.execute(0, "get_city", {"city_id": city})["queue"], res)
        # the queue empties again: the next city turn picks something instead of going idle
        g.execute(0, "change_queue", {"city_id": city, "action": "clear"})
        mark = len(g.events())
        self.round(g)
        self.assertTrue(g.execute(0, "get_city", {"city_id": city})["queue"])
        self.assertTrue(events_since(g, mark, "city_auto_production"))
        self.assertFalse(events_since(g, mark, "city_idle"))

    def test_off_by_default(self):
        g = game()
        city = found_capital(g)["city_id"]
        g.execute(0, "change_queue", {"city_id": city, "action": "clear"})
        mark = len(g.events())
        self.round(g)
        self.assertFalse(g.execute(0, "get_city", {"city_id": city})["queue"])
        self.assertTrue(events_since(g, mark, "city_idle"))


@needs_test_ops
class MoveOrderTests(unittest.TestCase):
    def test_move_order_captures_enemy_civilian(self):
        g = game()
        w = unit_of(g, 0, "Warrior")
        x, y = tiles_at(g, w["x"], w["y"], 1, land=True, units=False)[0]
        g.test_ops([{"op": "add_barbarian", "unit": "Worker", "x": x, "y": y}, {"op": "refresh_visibility"}])
        res = g.execute(0, "move_unit", {"unit_id": w["id"], "x": x, "y": y})
        self.assertTrue(res["arrived"], res)
        captured = [u for u in g.inspect({"what": "units", "x": x, "y": y}) if u["type"] == "Worker"]
        self.assertEqual([u["owner"] for u in captured], [0])     # UnCiv replaces a captured unit with a new one

    def test_blocked_standing_order_waits_then_gives_up_and_reports(self):
        g = game(barbarians="off")
        c = found_capital(g)
        cx, cy = c["x"], c["y"]
        walker = unit_of(g, 0, "Warrior")["id"]
        # three steps out, a two-turn walk into the city: the order is kept with one step to go
        for x, y in tiles_at(g, cx, cy, 3, land=True, units=False):
            g.test_ops([{"op": "set_unit", "unit": walker, "x": x, "y": y}, {"op": "ready_unit", "unit": walker}])
            route = g.path_preview(0, walker, cx, cy)
            if route["path"] and len(route["path"]) == 4 and route["turns"] == 2:
                break
        else:
            self.fail("no tile three steps from the city with a two-turn route into it")
        res = g.execute(0, "move_unit", {"unit_id": walker, "x": cx, "y": cy})
        self.assertTrue(res["order_kept"], res)
        here = (unit(g, walker)["x"], unit(g, walker)["y"])
        # the city is taken by another military unit: the only step left is the blocked one
        g.apply_ops([{"op": "add_unit", "player": 0, "unit": "Warrior", "x": cx, "y": cy}])
        mark = len(g.events())
        # held up: the order waits for the way to clear, for ORDER_PATIENCE turns, then gives up and says so
        for _ in range(ORDER_PATIENCE - 1):
            g.test_ops([{"op": "end_round"}])
            u = unit(g, walker)
            self.assertEqual((u["activity"], u["goto"], (u["x"], u["y"])), ("goto", {"x": cx, "y": cy}, here))
            self.assertFalse(events_since(g, mark, "orders_interrupted"))
        g.test_ops([{"op": "end_round"}])
        u = unit(g, walker)
        self.assertIsNone(u["activity"])
        self.assertIsNone(u["goto"])
        self.assertTrue(any("blocked" in e["text"] for e in events_since(g, mark, "orders_interrupted")))

    def _long_order(self):
        """A warrior with a standing order to a tile several turns away, and its planned route ([x, y] steps from where
        it stands)."""
        g = game(barbarians="off")
        found_capital(g)
        w = unit_of(g, 0, "Warrior")["id"]
        g.debug("reveal")                               # a known route: nothing hidden in the fog to stop it
        at = unit(g, w)
        for distance in (7, 6):
            for x, y in tiles_at(g, at["x"], at["y"], distance, land=True, units=False):
                route = g.path_preview(0, w, x, y)
                if route["path"] and len(route["path"]) - 1 >= 6:
                    g.execute(0, "move_unit", {"unit_id": w, "x": x, "y": y})
                    return g, w, [tuple(p) for p in route["path"]], (x, y)
        self.fail("no destination six steps away")

    def test_standing_order_follows_its_original_route(self):
        g, w, route, dest = self._long_order()
        u = unit(g, w)
        self.assertEqual((u["activity"], u["goto"]), ("goto", {"x": dest[0], "y": dest[1]}))
        for _ in range(20):
            if u["goto"] is None:
                break
            g.test_ops([{"op": "end_round"}])
            u = unit(g, w)
            self.assertIn((u["x"], u["y"]), route, "a standing order never leaves the route it was given")
        self.assertEqual((u["x"], u["y"]), dest)

    def test_unit_on_the_route_holds_the_order_up_instead_of_cancelling_it(self):
        g, w, route, dest = self._long_order()
        u = unit(g, w)
        here = (u["x"], u["y"])
        nxt = route[route.index(here) + 1]
        # a neutral foreign unit steps onto the next tile of the route
        g.apply_ops([{"op": "add_unit", "player": 1, "unit": "Warrior", "x": nxt[0], "y": nxt[1]}])
        mark = len(g.events())
        g.test_ops([{"op": "end_round"}])
        u = unit(g, w)
        self.assertEqual(((u["x"], u["y"]), u["goto"]), (here, {"x": dest[0], "y": dest[1]}),
                         "it waits, order intact")
        self.assertFalse(events_since(g, mark, "orders_interrupted"))
        g.apply_ops([{"op": "remove_units", "x": nxt[0], "y": nxt[1]}])
        g.test_ops([{"op": "end_round"}])
        u = unit(g, w)
        self.assertNotEqual((u["x"], u["y"]), here, "and carries on along the same route once the way is clear")
        self.assertIn((u["x"], u["y"]), route)

    def test_a_new_order_plans_a_new_route(self):
        g, w, route, dest = self._long_order()
        u = unit(g, w)
        back = route[route.index((u["x"], u["y"])) - 1]   # the step it came by
        g.test_ops([{"op": "set_unit", "unit": w, "moves": u["max_moves"]}])
        g.execute(0, "move_unit", {"unit_id": w, "x": back[0], "y": back[1]})
        u = unit(g, w)
        self.assertTrue(u["goto"] is None or u["goto"] == {"x": back[0], "y": back[1]}, u)
        self.assertEqual((u["x"], u["y"]), back)


@needs_test_ops
class WorkerSafetyTests(unittest.TestCase):
    def _setup(self, automated):
        """A worker two tiles from the capital, automated or building a farm there, and a barbarian warrior two tiles
        beyond it; then it is the owner's turn again, without the barbarians' turn in between (the start of a turn is
        where the orders run)."""
        g = game()
        c = found_capital(g)
        cx, cy = c["x"], c["y"]
        g.apply_ops([{"op": "set_city", "city": c["city_id"], "claim_radius": 2}])    # a farm is built inside borders
        for x, y in tiles_at(g, cx, cy, 2, land=True, units=False, city=False, owner=0):
            worker = g.apply_ops([{"op": "add_unit", "player": 0, "unit": "Worker", "x": x, "y": y}])[0]["unit_ids"][0]
            try:
                if automated:
                    g.execute(0, "unit_order", {"unit_id": worker, "order": "automate"})
                else:
                    g.execute(0, "build_improvement", {"unit_id": worker, "improvement": "Farm"})
                break
            except ActionError:
                g.apply_ops([{"op": "remove_units", "x": x, "y": y}])
        else:
            self.fail("no tile two from the capital for the worker")
        rx, ry = tiles_at(g, x, y, 2, land=True, units=False, city=False)[0]
        g.test_ops([{"op": "add_barbarian", "unit": "Warrior", "x": rx, "y": ry}, {"op": "refresh_visibility"}])
        mark = len(g.events())
        g.execute(0, "end_turn", {})
        g.force_turn(0)
        return g, (cx, cy), worker, mark

    def test_automated_worker_abandons_build_and_retreats(self):
        g, (cx, cy), worker, _ = self._setup(automated=True)
        u = unit(g, worker)
        self.assertEqual(u["activity"], "automate")
        self.assertIn((u["x"], u["y"]), tiles_at(g, cx, cy, 1) + tiles_at(g, cx, cy, 0), "closer than 2 to the city")

    def test_hand_ordered_worker_stops_and_asks(self):
        g, _, worker, mark = self._setup(automated=False)
        self.assertIsNone(unit(g, worker)["activity"])
        self.assertTrue(any("stopped building" in e["text"] for e in events_since(g, mark, "unit_woke")))


@needs_test_ops
class EventPrivacyTests(unittest.TestCase):
    def test_city_production_is_not_shared_with_observers(self):
        g = game(barbarians="off")
        settler = unit_of(g, 0, "Settler")
        # player 1 can see the city tile
        x, y = tiles_at(g, settler["x"], settler["y"], 1, land=True, units=False)[0]
        g.apply_ops([{"op": "add_unit", "player": 1, "unit": "Warrior", "x": x, "y": y}])
        g.test_ops([{"op": "refresh_visibility"}])
        self.assertIn(1, g.inspect({"what": "tile", "x": settler["x"], "y": settler["y"]})["visible"])
        mark = len(g.events())
        city = found_capital(g)["city_id"]
        g.execute(0, "set_production", {"city_id": city, "item": "Warrior"})
        g.test_ops([{"op": "complete_construction", "city": city}])
        private = events_since(g, mark, "unit_built")
        public = events_since(g, mark, "city_founded")
        self.assertEqual([e["players"] for e in private], [[0]])
        self.assertTrue(public and 1 in public[0]["players"], public)


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
    @classmethod
    def setUpClass(cls):
        # The app reads the accounts database on every request. Run on its own, this module would otherwise meet an
        # empty one; the test package has already pointed CITAR_DB_URL at a temporary file, as the API modules do.
        import os
        from citar import db, settings
        settings.reset()
        db.configure(os.environ["CITAR_DB_URL"])
        db.create_all()

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
                    w = next(u for u in g.view(0)["units"] if u["owner"] == 0 and u["type"] == "Warrior")
                    # a destination three tiles off that the route can reach, found through the facade's preview
                    dest = next((w["x"] + dx, w["y"] + dy) for dx, dy in ((3, 0), (-3, 0), (2, 2), (-2, 2), (2, -2),
                                                                           (-2, -2), (1, 3), (-1, -3))
                                if (g.path_preview(0, w["id"], w["x"] + dx, w["y"] + dy)["path"] or [None])[-1]
                                == [w["x"] + dx, w["y"] + dy])
                    x, y = dest
                    r = client.get(f"/api/games/{s.id}/path", params={"token": token, "unit_id": w["id"], "x": x, "y": y}).json()
                    self.assertEqual(r["path"][0], [w["x"], w["y"]])
                    self.assertEqual(r["path"][-1], [x, y])
                    self.assertGreaterEqual(r["turns"], 1)
                    # someone else's unit: no route
                    enemy = next(u for u in g.view(None)["units"] if u["owner"] == 1)
                    r2 = client.get(f"/api/games/{s.id}/path", params={"token": token, "unit_id": enemy["id"], "x": x, "y": y}).json()
                    self.assertIsNone(r2["path"])
                    # numbers past any engine's integers name no unit or tile: no route, not a server error
                    for bad in ({"unit_id": w["id"], "x": 10 ** 12, "y": y}, {"unit_id": w["id"], "x": x, "y": -10 ** 12},
                                {"unit_id": 10 ** 30, "x": x, "y": y}, {"unit_id": -1, "x": x, "y": y}):
                        r3 = client.get(f"/api/games/{s.id}/path", params={"token": token, **bad})
                        self.assertEqual(r3.status_code, 200, bad)
                        self.assertIsNone(r3.json()["path"], bad)
                finally:
                    appmod.manager.delete(s.id)


if __name__ == "__main__":
    unittest.main()
