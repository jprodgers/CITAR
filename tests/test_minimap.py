"""The minimap (design/phase3-features.md P3.7, package 3-17; gate 3).

- **The block.** A seat's view (``GET /api/games/{gid}/view`` with its token) has ``minimap``: ``stage`` from its era,
  ``capital``, and ``home``, the box of the tiles of its home continent it has explored, checked here against the view's
  own tiles (each one a tile the seat has explored) and the continents the state keeps, covered the short way round on
  a wrapping axis. The god view and a spectator's view, through a seat's eyes or not, have none (P3.7.2, P3.7.4).
- **Its cost** through the binding, as the server pays it: a seat's view with the block against the same view without
  it, alternating (P3.9.7; the engine's own timing of the block is the testkit's,
  ``crates/citar-testkit/tests/engine/minimap.rs``).
- **The debug ops route**, ``POST /api/games/{gid}/debug/ops`` (P3.7.5): not there (404) unless the server runs with
  ``CITAR_DEBUG=1``; then 200 for whoever manages the game (its owner, an administrator), 404 for another member and a
  moderator, 401 or 404 for a visitor, 403 for a member it is shared with to view (a write), and refused without the
  session's CSRF token (P3.9.1). The operations apply all or nothing, under the game's lock, and are announced as any
  action is: the version moves, and the view's ETag with it.
"""
from __future__ import annotations

import base64
import json
import os
import statistics
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_minimap_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import Base
from citar.engine_api import EngineGame
from citar.pool import scheduler as S

settings.reset()

DEBUG = {"CITAR_DEBUG": "1"}


def _stand_still(s, hold: float = 0.3, timeout: float = 15.0) -> None:
    """Wait until a paused game stands still: a bot's turn under way when the pause came runs on, its end included
    (tests/test_client_payloads.py says why). Still for ``hold`` seconds: the turn and the seat to move unchanged."""
    def mark():
        with s.lock:
            return s.game.turn, s.game.current, tuple(sorted(s.agent_status.items()))
    end = time.monotonic() + timeout
    last, since = mark(), time.monotonic()
    while time.monotonic() < end:
        time.sleep(0.02)
        now = mark()
        if now != last:
            last, since = now, time.monotonic()
        elif time.monotonic() - since >= hold:
            return
    raise AssertionError("the paused game never stood still")


def continents(state: dict) -> list:
    """Each tile's continent, as the state keeps them (a ``u16le`` column; 65535 is water)."""
    raw = base64.b64decode(state["map"]["continents"])
    return [int.from_bytes(raw[i:i + 2], "little") for i in range(0, len(raw), 2)]


def shortest_arc(marked: set, n: int, wrap: bool):
    """The shortest interval of ``0..n`` covering ``marked``, found the long way: every arc tried, the shortest kept, of
    those one that does not cross the seam, else the first to start (the engine's choice, by its longest gap)."""
    if not marked:
        return None
    if not wrap:
        return min(marked), max(marked)
    best = None
    for start in range(n):
        for length in range(1, n + 1):
            if all((i - start) % n < length for i in marked):
                key = (length, start + length > n, start)
                best = key if best is None or key < best else best
                break
    length, _, start = best
    return start, (start + length - 1) % n


def expected_home(view: dict, cont: list, anchor: tuple):
    """The home box the view's own tiles make: those of the anchor's continent, covered on each axis."""
    w, h = view["width"], view["height"]
    home = cont[anchor[1] * w + anchor[0]]
    if home == 0xFFFF:
        return None
    tiles = [t[0] for t in view["tiles"] if cont[t[0]] == home]
    xs = shortest_arc({i % w for i in tiles}, w, view["wrap_x"])
    ys = shortest_arc({i // w for i in tiles}, h, view["wrap_y"])
    if xs is None:
        return None
    return {"x0": xs[0], "y0": ys[0], "x1": xs[1], "y1": ys[1]}


def anchor_of(view: dict, pid: int):
    """Where the seat's home is read from: its capital, else its first city, else its first settler."""
    cities = sorted((c for c in view["cities"] if c["owner"] == pid and not c.get("stale")), key=lambda c: c["id"])
    capital = next((c for c in cities if c.get("capital")), None)
    if capital:
        return capital["x"], capital["y"]
    if cities:
        return cities[0]["x"], cities[0]["y"]
    settlers = sorted((u for u in view["units"] if u["owner"] == pid and u.get("type") == "Settler"),
                      key=lambda u: u["id"])
    return (settlers[0]["x"], settlers[0]["y"]) if settlers else None


class Case(unittest.TestCase):
    WHO = (("alice", "user"), ("bob", "user"), ("moe", "moderator"), ("zara", "admin"))

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        db.dispose()
        db.configure(os.environ["CITAR_DB_URL"])
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        servers.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.app = appmod
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in self.WHO:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.max_concurrent_games = 20
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
        self.games: list = []

    def tearDown(self):
        for gid in self.games:
            if self.manager.get(gid) is not None:
                self.manager.delete(gid)

    def create(self, who: str, seats: list, **config) -> dict:
        """A paused game of ``who``'s on a small map that wraps east-west (nobody plays on but the test)."""
        r = self.client.post("/api/games", headers=self.headers[who], json={
            "name": "minimap", "config": {"map_size": "small", "map_edges": "wrap_x", "seed": 17, "barbarians": "off",
                                          "city_states": 0, **config}, "seats": seats})
        self.assertEqual(r.status_code, 200, r.text)
        info = r.json()
        self.games.append(info["id"])
        s = self.manager.get(info["id"])
        s.set_paused(True)
        _stand_still(s)
        return info

    def view(self, gid: str, who=None, token=None, **params):
        r = self.client.get(f"/api/games/{gid}/view", headers=self.headers[who] if who else {},
                            params={**({"token": token} if token else {}), **params})
        self.assertEqual(r.status_code, 200, r.text)
        return r

    def ops(self, gid: str, ops, who=None, headers=None, **params):
        return self.client.post(f"/api/games/{gid}/debug/ops", json=ops, params=params,
                                headers=headers if headers is not None else (self.headers[who] if who else {}))

    def check_block(self, gid: str, token: str, pid: int) -> dict:
        """The seat's block, its home box checked against its view's own tiles and the state's continents."""
        v = json.loads(self.view(gid, token=token).content)
        s = self.manager.get(gid)
        with s.lock:
            cont = continents(s.game.state_dict())
            era = s.game.view(pid)["empire"].get("era")
        m = v["minimap"]
        self.assertEqual(set(m), {"stage", "capital", "home"})
        anchor = anchor_of(v, pid)
        self.assertEqual(m["home"], expected_home(v, cont, anchor) if anchor else None,
                         f"the explored tiles of the home continent; anchor {anchor}")
        capital = next((c for c in v["cities"] if c["owner"] == pid and c.get("capital")), None)
        self.assertEqual(m["capital"], [capital["x"], capital["y"]] if capital else None)
        return {**m, "era": era}


class Block(Case):
    """Gate 3: the block in a seat's view, and in nobody else's."""

    def test_a_seat_s_view_bounds_its_home_by_what_it_explored(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        start = self.check_block(gid, token, 0)
        self.assertEqual(start["stage"], "early")
        self.assertIsNone(start["capital"], "turn 1: a settler, no city")
        self.assertIsNotNone(start["home"], "the settler's continent")
        v = json.loads(self.view(gid, token=token).content)
        settler = next(u for u in v["units"] if u["owner"] == 0 and u["type"] == "Settler")
        with mock.patch.dict(os.environ, DEBUG):
            made = self.ops(gid, [{"op": "found_city", "player": 0, "x": settler["x"], "y": settler["y"],
                                   "capital": True}], who="alice")
        self.assertEqual(made.status_code, 200, made.text)
        city = self.check_block(gid, token, 0)
        self.assertEqual(city["capital"], [settler["x"], settler["y"]])
        # the whole map explored: the home is the whole continent, and the other continents' tiles are in the view
        with mock.patch.dict(os.environ, DEBUG):
            self.assertEqual(self.ops(gid, [{"op": "reveal", "player": 0}], who="alice").status_code, 200)
        whole = self.check_block(gid, token, 0)
        v = json.loads(self.view(gid, token=token).content)
        self.assertEqual(len(v["tiles"]), v["width"] * v["height"], "every tile explored")
        s = self.manager.get(gid)
        with s.lock:
            cont = continents(s.game.state_dict())
        home = cont[settler["y"] * v["width"] + settler["x"]]
        self.assertTrue(any(c not in (home, 0xFFFF) for c in cont), "another continent, left out of the box")
        self.assertNotEqual(whole["home"], expected_home(v, [home if c != 0xFFFF else c for c in cont],
                                                          (settler["x"], settler["y"])),
                            "all land counted as home would be another box")

    def test_the_stage_follows_the_era(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        seen = []
        for era in ("Classical era", "Renaissance era", "Industrial era", "Information era", "Future era"):
            with mock.patch.dict(os.environ, DEBUG):
                r = self.ops(gid, [{"op": "grant_era", "player": 0, "era": era}], who="alice")
            self.assertEqual(r.status_code, 200, r.text)
            seen.append(self.check_block(gid, token, 0)["stage"])
        self.assertEqual(seen, ["early", "renaissance", "renaissance", "information", "information"])

    def test_the_god_view_and_a_spectator_s_have_none(self):
        info = self.create("alice", [{"type": "bot"}, {"type": "bot"}])
        gid = info["id"]
        bot = info["seats"][0]["token"]
        self.assertIn("minimap", json.loads(self.view(gid, token=bot).content), "a bot seat's own view")
        spectator = info["spectator_token"]
        for label, r in (("the god view", self.view(gid, token=spectator)),
                         ("a spectator through a seat's eyes", self.view(gid, token=spectator, as_player=0)),
                         ("the owner watching", self.view(gid, who="alice")),
                         ("the owner through a seat's eyes", self.view(gid, who="alice", as_player=1))):
            v = json.loads(r.content)
            self.assertNotIn("minimap", v, label)
            self.assertIn("tiles", v, label)

    def test_the_block_costs_little_through_the_binding(self):
        """P3.9.7, as the server pays it: a standard map all explored, the view with its block against the same view
        without it, alternating, seven pairs of twenty; the median of the pairs' ratios within 5% (15% on CI)."""
        g = EngineGame.new({"seed": 5, "map_size": "standard", "map_edges": "wrap_x", "city_states": 0,
                            "players": [{"controller": "bot"}, {"controller": "bot"}]})
        g.apply_ops([{"op": "reveal", "player": 0}])
        self.assertIn(b'"minimap"', g.view_json(0))
        self.assertNotIn(b'"minimap"', g.view_json(0, minimap=False))
        ratios = []
        for _ in range(7):
            t = time.perf_counter()
            for _ in range(20):
                g.view_json(0)
            with_block = time.perf_counter() - t
            t = time.perf_counter()
            for _ in range(20):
                g.view_json(0, minimap=False)
            ratios.append(with_block / (time.perf_counter() - t))
        budget = 1.15 if os.environ.get("CI") else 1.05
        self.assertLessEqual(statistics.median(ratios), budget, f"ratios {[round(r, 3) for r in ratios]}")


class DebugOps(Case):
    """Gate 3: ``POST /api/games/{gid}/debug/ops`` (P3.7.5), its gate by every kind of caller (P3.9.1)."""

    def test_it_is_not_there_without_the_debug_flag(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        with mock.patch.dict(os.environ, {"CITAR_DEBUG": ""}):
            r = self.ops(info["id"], [{"op": "reveal", "player": 0}], who="alice")
        self.assertEqual(r.status_code, 404, r.text)
        with mock.patch.dict(os.environ, {"CITAR_DEBUG": ""}):
            self.assertEqual(self.ops(info["id"], [], who="zara").status_code, 404, "not for an administrator either")

    def test_who_may_apply_operations(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid = info["id"]
        gold = [{"op": "set_player", "player": 0, "gold": 777}]
        with mock.patch.dict(os.environ, DEBUG):
            self.assertEqual(self.ops(gid, gold, who="bob").status_code, 404, "a stranger: the game is private")
            self.assertEqual(self.ops(gid, gold, who="moe").status_code, 404, "a moderator")
            self.assertIn(self.ops(gid, gold).status_code, (401, 404), "a visitor")
            self.assertIn(self.ops(gid, gold, token=info["seats"][0]["token"]).status_code, (401, 403, 404),
                          "a seat token plays its seat and manages nothing")
            self.assertIn(self.ops(gid, gold, token=info["spectator_token"]).status_code, (401, 403, 404),
                          "the spectator link")
            no_csrf = {k: v for k, v in self.headers["alice"].items() if k != sessions.CSRF_HEADER}
            self.assertEqual(self.ops(gid, gold, headers=no_csrf).status_code, 403, "a cookie without its CSRF token")
            shared = self.client.post(f"/api/games/{gid}/sharing/users", headers=self.headers["alice"],
                                      json={"handle": "bob", "permission": "view"})
            self.assertEqual(shared.status_code, 200, shared.text)
            self.assertEqual(self.ops(gid, gold, who="bob").status_code, 403, "an ACL viewer may not write")
            s = self.manager.get(gid)
            with s.lock:
                self.assertNotEqual(s.game.view(0)["empire"]["gold"], 777, "none of them changed the game")
            owner = self.ops(gid, gold, who="alice")
            self.assertEqual(owner.status_code, 200, owner.text)
            self.assertEqual(owner.json()["ok"], True)
            self.assertEqual(len(owner.json()["results"]), 1, "each operation's result")
            with s.lock:
                self.assertEqual(s.game.view(0)["empire"]["gold"], 777)
            admin = self.ops(gid, [{"op": "set_player", "player": 0, "gold": 888}], who="zara")
            self.assertEqual(admin.status_code, 200, admin.text)
            with s.lock:
                self.assertEqual(s.game.view(0)["empire"]["gold"], 888, "an administrator manages every game")

    def test_operations_apply_all_or_nothing_and_are_announced(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        s = self.manager.get(gid)
        first = self.view(gid, token=token)
        tag = first.headers.get("etag")
        with s.lock:
            version, gold = s.version, s.game.view(0)["empire"]["gold"]
        with mock.patch.dict(os.environ, DEBUG):
            bad = self.ops(gid, [{"op": "set_player", "player": 0, "gold": 5}, {"op": "no_such_op"}], who="alice")
            self.assertEqual(bad.status_code, 400, bad.text)
            self.assertIn("no_such_op", bad.json()["detail"])
            with s.lock:
                self.assertEqual((s.version, s.game.view(0)["empire"]["gold"]), (version, gold), "all or nothing")
            # a refused call still counts as a command (the binding's writes), so the tag may move; the view does not
            self.assertEqual(self.view(gid, token=token).content, first.content, "a refused list changed nothing")
            many = self.ops(gid, [{"op": "set_player", "player": 0, "gold": 5}] * 201, who="alice")
            self.assertEqual(many.status_code, 400, many.text)
            for body in ({"op": "reveal"}, "reveal", [1, 2]):
                self.assertEqual(self.ops(gid, body, who="alice").status_code, 422, f"not a list of operations: {body}")
            good = self.ops(gid, [{"op": "grant_era", "player": 0, "era": "Renaissance era"}], who="alice")
            self.assertEqual(good.status_code, 200, good.text)
        with s.lock:
            self.assertEqual(s.version, version + 1, "announced as an action is")
        moved = self.client.get(f"/api/games/{gid}/view", params={"token": token}, headers={"if-none-match": tag})
        self.assertEqual(moved.status_code, 200, "the view moved")
        self.assertNotEqual(moved.headers.get("etag"), tag)
        self.assertEqual(json.loads(moved.content)["minimap"]["stage"], "renaissance")

    def test_the_route_audit_sees_its_gate(self):
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))
        import audit_routes
        found = [(route, methods) for route, path, _deps in audit_routes.all_routes(self.app.app)
                 if path == "/api/games/{gid}/debug/ops" for methods in [getattr(route, "methods", None)]]
        self.assertEqual([m for _, m in found], [{"POST"}])
        self.assertTrue(audit_routes.body_gated(found[0][0]), "its _gate( in the body")
        # registered before /debug/{action}, which would otherwise take it as an action called "ops"
        paths = [path for _, path, _ in audit_routes.all_routes(self.app.app)]
        self.assertLess(paths.index("/api/games/{gid}/debug/ops"), paths.index("/api/games/{gid}/debug/{action}"))


if __name__ == "__main__":
    unittest.main()
