"""The server's smoke test on the Rust engine (package 2-09): the website's routes, end to end over HTTP.

A signed-in administrator creates a game of a human and two bots; the human plays turns through the tool route and
waits for its turn back, opens a chat that a bot answers, saves and loads the game and plays on; the views, the
summary, the metrics, the path preview, the debug shortcuts and the map export answer; the recap is refused while the
human plays. An all-bot game plays on its own and serves the god view and the recap to its spectator. Maps, the
scenario editor and a scenario launched as a game round it off. Every engine call behind these routes is the Rust
engine's (CITAR_ENGINE=rust, the default since this package).
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import os
import shutil
import time
import unittest
from unittest import mock

from citar.server import session as sess
from tests.backends import rust_only


def wait(cond, timeout: float) -> bool:
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return True
        time.sleep(0.05)
    return bool(cond())


@rust_only
class ServerSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from citar import db, settings
        from citar.auth import accounts, sessions
        from citar.server import app as appmod
        from fastapi.testclient import TestClient
        settings.reset()
        db.configure(os.environ["CITAR_DB_URL"])
        db.create_all()
        with db.session() as s:
            handle = f"smoke{os.urandom(3).hex()}"
            user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                        password="a long enough phrase", role="admin", status="active",
                                        email_verified=True)
            row, raw = sessions.start(s, user)
            cls.headers = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token}
        cls.app = appmod
        cls.client = TestClient(appmod.app, base_url=settings.get().public_origin or "http://testserver")
        cls.games = []

    @classmethod
    def tearDownClass(cls):
        for gid in cls.games:
            cls.app.manager.delete(gid)
            shutil.rmtree(sess.SAVE_DIR / gid, ignore_errors=True)
        cls.client.close()

    # ------------------------------------------------------------------ helpers
    def req(self, method: str, path: str, *, token=None, ok=True, **kw):
        r = self.client.request(method, path, headers=self.headers, params={"token": token} if token else None, **kw)
        if ok:
            self.assertLess(r.status_code, 300, f"{method} {path}: {r.status_code} {r.text[:500]}")
        return r

    def tool(self, gid: str, token: str, name: str, args=None, wait_seconds: float = 0.0) -> dict:
        r = self.req("POST", f"/api/games/{gid}/tool", token=token,
                     json={"tool": name, "args": args or {}, "wait_seconds": wait_seconds}).json()
        self.assertTrue(r["ok"], f"{name}: {r}")
        return r["result"]

    def view(self, gid: str, token: str) -> dict:
        r = self.req("GET", f"/api/games/{gid}/view", token=token)
        self.assertEqual(r.headers["content-type"], "application/json")
        return json.loads(r.content)

    def create(self, name: str, config: dict, seats: list) -> dict:
        info = self.req("POST", "/api/games", json={"name": name, "config": config, "seats": seats}).json()
        self.games.append(info["id"])
        return info

    def my_turn(self, gid: str, token: str, timeout: float = 60.0) -> dict:
        r = None
        deadline = time.time() + timeout
        while time.time() < deadline:
            r = self.client.get(f"/api/games/{gid}/wait", headers=self.headers,
                                params={"token": token, "timeout": 5}).json()
            if r["status"] in ("your_turn", "game_over", "eliminated"):
                return r
            if r["status"] == "negotiation":
                return r
        self.fail(f"the human's turn did not come back within {timeout} s: {r}")

    # ------------------------------------------------------------------ the human's game
    def test_a_human_plays_with_two_bots(self):
        info = self.create("Smoke: human and bots", {"map_size": "duel", "seed": 4242, "barbarians": "off"},
                           [{"type": "human"}, {"type": "bot"}, {"type": "bot"}])
        gid = info["id"]
        human = next(s for s in info["seats"] if s["type"] == "human")["token"]
        s = self.app.manager.get(gid)
        v = self.view(gid, human)
        self.assertEqual((v["you"], v["seat"]["player"], v["session"]["id"]), (0, 0, gid))
        settler = next(u for u in v["units"] if u["owner"] == 0 and u["type"] == "Settler")
        city = self.tool(gid, human, "found_city", {"unit_id": settler["id"]})["city_id"]
        self.tool(gid, human, "set_production", {"city_id": city, "item": "Warrior"})
        tech = next(t["name"] for t in self.tool(gid, human, "get_tech_tree")["techs"] if t["status"] == "available")
        self.tool(gid, human, "set_research", {"tech": tech})
        self.assertTrue(self.tool(gid, human, "get_briefing"), "a briefing")
        # the route preview of a unit
        warrior = next(u for u in v["units"] if u["owner"] == 0 and u["type"] == "Warrior")
        preview = self.client.get(f"/api/games/{gid}/path", headers=self.headers,
                                  params={"token": human, "unit_id": warrior["id"], "x": warrior["x"] + 1,
                                          "y": warrior["y"]})
        self.assertEqual(preview.status_code, 200)
        self.assertIn("path", preview.json())
        # five turns: end the turn, the bots play theirs through the session's driver, the human's comes back
        for _ in range(5):
            turn = s.game.turn
            self.tool(gid, human, "end_turn")
            self.my_turn(gid, human)
            self.assertGreater(s.game.turn, turn)
        self.assertGreaterEqual(s.game.turn, 6)
        # the recap waits for the end of a game somebody plays
        self.assertEqual(self.client.get(f"/api/games/{gid}/replay", headers=self.headers,
                                         params={"token": human}).status_code, 403)
        # a chat with a bot: it answers through its responder while the human waits for the reply
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": 0, "gold": 200}])
        r = self.tool(gid, human, "open_negotiation",
                      {"to": 1, "message": "Your map for 20 gold?", "give": [{"type": "gold", "amount": 20}],
                       "receive": [{"type": "share_map"}]}, wait_seconds=30)
        nid = r["negotiation_id"]
        self.assertTrue(wait(lambda: s.game.negotiation_head(nid)["awaiting"] != 1, 30))
        self.assertGreater(len(s.game.negotiation(nid)["history"]), 1, "the bot answered")
        if s.game.negotiation_head(nid)["status"] == "open":
            self.tool(gid, human, "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                          "message": "No, thank you."})
        # the summary, the metrics and the map export
        summary = self.req("GET", f"/api/games/{gid}/summary").json()
        self.assertEqual((summary["id"], summary["crashed"]), (gid, None))
        self.assertEqual(len(summary["players"]), 3)
        metrics = self.req("GET", f"/api/games/{gid}/metrics").json()
        bots = [r for r in metrics["turns"] if r["player"] in (1, 2)]
        self.assertTrue(bots and all(r["controller"] == "bot" for r in bots))
        self.assertTrue(any(r.get("bot_actions") for r in bots))
        self.assertIn("bot_actions", self.req("GET", f"/api/games/{gid}/metrics.csv").text.splitlines()[0])
        exported = self.req("POST", f"/api/games/{gid}/export_map").json()
        self.assertEqual((exported["width"], exported["height"]), (v["width"], v["height"]))
        # the debug shortcuts, on a server run with --debug
        with mock.patch.dict(os.environ, {"CITAR_DEBUG": "1"}):
            gold = s.game.standing(0)["gold"]
            self.req("POST", f"/api/games/{gid}/debug/gold")
            self.assertEqual(s.game.standing(0)["gold"], gold + 500)
            self.assertEqual(self.req("POST", f"/api/games/{gid}/debug/nonsense", ok=False).status_code, 400)
        # a named save, listed, loaded back over the live game, and played on
        turn = s.game.turn
        saved = self.req("POST", f"/api/games/{gid}/save", json={"name": "smoke"}).json()["saved"]
        listed = [x for x in self.req("GET", "/api/saves").json() if x["path"] == saved]
        self.assertEqual(len(listed), 1)
        self.assertEqual((listed[0]["turn"], listed[0].get("unreadable")), (turn, None))
        loaded = self.req("POST", "/api/saves/load", json={"path": saved}).json()
        self.assertEqual((loaded["id"], loaded["turn"]), (gid, turn))
        s2 = self.app.manager.get(gid)
        self.assertIsNot(s2, s)
        self.req("POST", f"/api/games/{gid}/control", json={"paused": False})
        self.tool(gid, human, "end_turn")
        self.my_turn(gid, human)
        self.assertGreater(s2.game.turn, turn)
        self.assertEqual(s2.errors, [])

    # ------------------------------------------------------------------ an all-bot game
    def test_an_all_bot_game_plays_itself_and_is_watched(self):
        info = self.create("Smoke: all bots", {"map_size": "small", "seed": 4343}, [{"type": "bot"}] * 3)
        gid, spectator = info["id"], info["spectator_token"]
        s = self.app.manager.get(gid)
        self.assertTrue(wait(lambda: s.game.turn >= 50, 120), f"turn {s.game.turn}: {s.errors}")
        god = self.view(gid, spectator)
        self.assertTrue(god["spectator"])
        self.assertEqual(sorted(int(k) for k in god["empires"]), [0, 1, 2])
        self.assertTrue(god["units"])
        as_one = self.client.get(f"/api/games/{gid}/view", headers=self.headers,
                                 params={"token": spectator, "as_player": 1})
        self.assertEqual(json.loads(as_one.content)["you"], 1)
        replay = self.req("GET", f"/api/games/{gid}/replay", token=spectator)
        data = json.loads(replay.content)
        self.assertEqual((data["id"], data["name"]), (gid, "Smoke: all bots"))
        self.assertGreaterEqual(len(data["frames"]), 40)
        self.assertTrue(all(p["seat"]["type"] == "bot" for p in data["players"] if p["seat"]))
        lobby = [g for g in self.req("GET", "/api/games").json() if g["id"] == gid]
        self.assertEqual(len(lobby), 1)
        self.assertIsNone(lobby[0]["crashed"])
        self.req("POST", f"/api/games/{gid}/control", json={"paused": True})
        self.assertEqual(s.errors, [])
        self.req("DELETE", f"/api/games/{gid}")
        self.assertIsNone(self.app.manager.get(gid))

    # ------------------------------------------------------------------ maps and scenarios
    def test_maps_the_scenario_editor_and_a_launch(self):
        m = self.req("POST", "/api/maps/generate", json={"map_size": "duel", "map_type": "pangaea", "seed": 77}).json()
        m["id"] = f"smoke-{os.urandom(2).hex()}"
        m["name"] = "Smoke map"
        saved = self.req("PUT", f"/api/maps/{m['id']}", json=m).json()
        self.assertEqual(saved["map"]["width"], m["width"])
        self.assertIn(m["id"], [x["id"] for x in self.req("GET", "/api/maps").json()])
        ed = self.req("POST", "/api/scenario-editor",
                      json={"source": "map", "map": m["id"], "players": [{"type": "human"}, {"type": "bot"}]}).json()
        eid = ed["editor_id"]
        start = next(u for u in ed["view"]["units"] if u["owner"] == 0 and u["type"] == "Settler")
        done = self.req("POST", f"/api/scenario-editor/{eid}/ops",
                        json={"ops": [{"op": "found_city", "player": 0, "x": start["x"], "y": start["y"],
                                       "name": "Smoketown"}, {"op": "set_player", "player": 1, "gold": 321}]}).json()
        self.assertEqual(len(done["results"]), 2)
        self.assertEqual(self.req("POST", f"/api/scenario-editor/{eid}/ops", ok=False,
                                  json={"ops": [{"op": "no_such_op"}]}).status_code, 400)
        self.assertTrue(self.req("GET", f"/api/scenario-editor/{eid}").json()["can_undo"])
        sid = f"smoke-scn-{os.urandom(2).hex()}"
        summ = self.req("POST", f"/api/scenario-editor/{eid}/save", json={"id": sid, "name": "Smoke scenario"}).json()
        self.assertEqual(summ["id"], sid)
        undone = self.req("POST", f"/api/scenario-editor/{eid}/undo").json()
        self.assertFalse([c for c in undone["view"]["cities"] if c["name"] == "Smoketown"])
        self.req("DELETE", f"/api/scenario-editor/{eid}")
        launched = self.req("POST", f"/api/scenarios/{sid}/launch",
                            json={"name": "Smoke launch", "seats": [{"type": "human"}, {"type": "bot"}]}).json()
        self.games.append(launched["id"])
        human = launched["seats"][0]["token"]
        cities = self.view(launched["id"], human)["cities"]
        self.assertIn("Smoketown", [c["name"] for c in cities])
        self.req("DELETE", f"/api/scenarios/{sid}")
        self.req("DELETE", f"/api/maps/{m['id']}")


if __name__ == "__main__":
    unittest.main()
