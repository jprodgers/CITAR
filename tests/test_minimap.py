"""The minimap (design/phase3-features.md P3.7, package 3-17; gate 3).

- **The block.** A seat's view (``GET /api/games/{gid}/view`` with its token) has ``minimap``: ``stage`` from its era,
  ``capital``, and ``home``, the box of the tiles of its home continent it has explored and seen joined to home, checked
  here against a search from the anchor through the view's own tiles (each one a tile the seat has explored) of the
  anchor's continent as the state keeps them, covered the short way round on a wrapping axis. Land of the home
  continent a scout explored far away, which nothing explored joins to home, stays out of the box: the box would tell
  the seat that land is home, which its view does not. The god view and a spectator's view, through a seat's eyes or
  not, have none (P3.7.2, P3.7.4).
- **The binding** leaves the block out when asked (``view_json(..., minimap=False)``, a spectator's look through a seat's
  eyes), and nothing else. The block's cost (P3.9.7) is a testkit timing of the block itself against the seat view
  (``crates/citar-testkit/tests/engine/minimap.rs``): through the binding it is a fraction of a percent of a view, below
  this laptop's run-to-run noise.
- **The debug ops route**, ``POST /api/games/{gid}/debug/ops`` (P3.7.5): not there (404) unless the server runs with
  ``CITAR_DEBUG=1``, whoever calls and whatever the body; then 200 for whoever manages the game (its owner, an administrator), 404 for another member and a
  moderator, 401 or 404 for a visitor, 403 for a member it is shared with to view (a write) and, the game published,
  for a moderator (control) and a visitor, and refused without the session's CSRF token (P3.9.1), each before its
  body is judged (a body not a list of objects is a 422 for the manager only). The operations apply all or nothing, under the game's lock, and are announced as any action is: the version moves, and the view's
  ETag with it.
"""
from __future__ import annotations

import base64
import json
import os
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


def neighbours(i: int, w: int, h: int, wrap_x: bool, wrap_y: bool):
    """The six tiles next to tile ``i``, from its offset coordinates (odd rows half a tile east)."""
    x, y = i % w, i // w
    steps = ((1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)) if y & 1 else \
        ((1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1))
    for dx, dy in steps:
        nx, ny = x + dx, y + dy
        nx, ny = nx % w if wrap_x else nx, ny % h if wrap_y else ny
        if 0 <= nx < w and 0 <= ny < h:
            yield ny * w + nx


def box_of(view: dict, tiles) -> dict | None:
    """The box of ``tiles`` (indexes), covered the short way round on a wrapping axis."""
    w, h = view["width"], view["height"]
    xs = shortest_arc({i % w for i in tiles}, w, view["wrap_x"])
    ys = shortest_arc({i // w for i in tiles}, h, view["wrap_y"])
    return None if xs is None else {"x0": xs[0], "y0": ys[0], "x1": xs[1], "y1": ys[1]}


def home_tiles(view: dict, cont: list, anchor: tuple) -> set:
    """The tiles of the anchor's continent the view's own tiles join to it: a search from the anchor through them."""
    w, h = view["width"], view["height"]
    start = anchor[1] * w + anchor[0]
    shown = {t[0] for t in view["tiles"]}
    if cont[start] == 0xFFFF or start not in shown:
        return set()
    seen, todo = {start}, [start]
    while todo:
        for n in neighbours(todo.pop(), w, h, view["wrap_x"], view["wrap_y"]):
            if n in shown and cont[n] == cont[start] and n not in seen:
                seen.add(n)
                todo.append(n)
    return seen


def expected_home(view: dict, cont: list, anchor: tuple):
    """The home box the view's own tiles make: those of the anchor's continent joined to it through them."""
    return box_of(view, home_tiles(view, cont, anchor))


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
        self.assertEqual(whole["home"], box_of(v, [i for i, c in enumerate(cont) if c == home]),
                         "everything explored: the whole home continent")
        self.assertNotEqual(whole["home"], box_of(v, [i for i, c in enumerate(cont) if c != 0xFFFF]),
                            "all land counted as home would be another box")

    def test_far_land_of_the_home_continent_stays_out_until_explored_land_joins_it(self):
        """A scout put down far away on the seat's own continent explores land there, which nothing the seat explored
        joins to its home: the box stays as it was, though counting every explored tile of the continent would widen
        it. Revealed, the land between is explored, and the box is the whole continent."""
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        before = self.check_block(gid, token, 0)
        v = json.loads(self.view(gid, token=token).content)
        w = v["width"]
        settler = next(u for u in v["units"] if u["owner"] == 0 and u["type"] == "Settler")
        s = self.manager.get(gid)
        with s.lock:
            cont = continents(s.game.state_dict())
        home = cont[settler["y"] * w + settler["x"]]

        def apart(i: int) -> int:
            dx = abs(i % w - settler["x"])
            return min(dx, w - dx) + abs(i // w - settler["y"])
        far = max((i for i, c in enumerate(cont) if c == home), key=apart)
        self.assertGreater(apart(far), 12, "the home continent reaches far from the start")
        with mock.patch.dict(os.environ, DEBUG):
            r = self.ops(gid, [{"op": "add_unit", "player": 0, "unit": "Scout", "x": far % w, "y": far // w}],
                         who="alice")
        self.assertEqual(r.status_code, 200, r.text)
        after = self.check_block(gid, token, 0)
        v = json.loads(self.view(gid, token=token).content)
        explored_home = [t[0] for t in v["tiles"] if cont[t[0]] == home]
        self.assertIn(far, explored_home, "the scout's tile is explored, and on the home continent")
        self.assertNotIn(far, home_tiles(v, cont, (settler["x"], settler["y"])), "nothing explored joins it to home")
        self.assertEqual(after["home"], before["home"], "the far land stays out of the box")
        self.assertNotEqual(box_of(v, explored_home), after["home"],
                            "every explored tile of the continent counted would have widened it")
        with mock.patch.dict(os.environ, DEBUG):
            self.assertEqual(self.ops(gid, [{"op": "reveal", "player": 0}], who="alice").status_code, 200)
        whole = self.check_block(gid, token, 0)
        self.assertEqual(whole["home"], box_of(v, [i for i, c in enumerate(cont) if c == home]),
                         "revealed: the whole continent")

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

    def test_the_binding_leaves_the_block_out_when_asked(self):
        """``view_json(..., minimap=False)``, as the view route asks for a spectator looking through a seat's eyes: the
        same view, its block alone left out; the god view has none either way."""
        g = EngineGame.new({"seed": 5, "map_size": "small", "map_edges": "wrap_x", "city_states": 0,
                            "players": [{"controller": "bot"}, {"controller": "bot"}]})
        seat = json.loads(g.view_json(0))
        without = json.loads(g.view_json(0, minimap=False))
        self.assertEqual(set(seat["minimap"]), {"stage", "capital", "home"})
        self.assertNotIn("minimap", without)
        seat.pop("minimap")
        self.assertEqual(seat, without, "nothing else differs")
        self.assertNotIn("minimap", json.loads(g.view_json(None)), "the god view")
        self.assertIn("minimap", g.view(1), "the facade's view, a seat's own")
        with self.assertRaises(ValueError):
            g.view_json(0, {"minimap": 1})          # a host may not splice a key of the view's own


class DebugOps(Case):
    """Gate 3: ``POST /api/games/{gid}/debug/ops`` (P3.7.5), its gate by every kind of caller (P3.9.1)."""

    def test_it_is_not_there_without_the_debug_flag(self):
        """Without the flag the route is not there, whoever calls and whatever JSON the body holds: the flag is checked
        before the caller is resolved and before the body is judged. So is ``/debug/{action}``."""
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid = info["id"]
        bodies = ([{"op": "reveal", "player": 0}], [], {"op": "reveal"}, "x", [1, 2], None, 7)
        with mock.patch.dict(os.environ, {"CITAR_DEBUG": ""}):
            for who in (None, "bob", "alice", "zara"):
                for body in bodies:
                    r = self.ops(gid, body, who=who)
                    self.assertEqual(r.status_code, 404, f"{who or 'a visitor'}, {body!r}: {r.text}")
                    self.assertEqual(r.json(), {"detail": "Debug endpoints are disabled."}, "nothing of the body")
            anon = self.client.post("/api/games/nosuchgame/debug/ops", content=b"")
            self.assertEqual(anon.status_code, 404, f"a visitor, no game, no body: {anon.text}")
            text = self.client.post(f"/api/games/{gid}/debug/ops", content=b"{not json",
                                    headers={"content-type": "text/plain"})
            self.assertEqual(text.status_code, 404, f"text that is not JSON: {text.text}")
            # text declared JSON that does not parse is FastAPI's 422 on any route that takes a body, before the flag
            bad = self.client.post(f"/api/games/{gid}/debug/ops", content=b"{not json",
                                   headers={"content-type": "application/json"})
            self.assertEqual((bad.status_code, bad.json()["detail"][0]["type"]), (422, "json_invalid"), bad.text)
            self.assertNotIn("not json", bad.text, "the body is not echoed")
            for who in (None, "alice"):
                r = self.client.post(f"/api/games/{gid}/debug/reveal", headers=self.headers[who] if who else {})
                self.assertEqual(r.status_code, 404, f"the named actions, {who or 'a visitor'}: {r.text}")
        s = self.manager.get(gid)
        with s.lock:
            seat = json.loads(s.game.view_json(0))
        self.assertLess(len(seat["tiles"]), seat["width"] * seat["height"], "nothing was revealed")

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
            # a body that is not a list of operations is judged only once the gate let the caller through
            for body in ({"op": "reveal"}, "x", [1, 2], None):
                self.assertEqual(self.ops(gid, body, who="bob").status_code, 404, f"a stranger's {body!r}")
                self.assertIn(self.ops(gid, body).status_code, (401, 404), f"a visitor's {body!r}")
                self.assertEqual(self.ops(gid, body, headers=no_csrf).status_code, 403, f"no CSRF token, {body!r}")
            shared = self.client.post(f"/api/games/{gid}/sharing/users", headers=self.headers["alice"],
                                      json={"handle": "bob", "permission": "view"})
            self.assertEqual(shared.status_code, 200, shared.text)
            self.assertEqual(self.ops(gid, gold, who="bob").status_code, 403, "an ACL viewer may not write")
            public = self.client.put(f"/api/games/{gid}/sharing", headers=self.headers["zara"],
                                     json={"visibility": "global"})
            self.assertEqual(public.status_code, 200, public.text)
            self.assertEqual(self.ops(gid, gold, who="moe").status_code, 403, "published: a moderator may not control")
            self.assertIn(self.ops(gid, gold).status_code, (401, 403), "published: a visitor may not either")
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
            for body in ({"op": "reveal"}, "reveal", [1, 2], None, 7):
                r = self.ops(gid, body, who="alice")
                self.assertEqual(r.status_code, 422, f"not a list of operations: {body!r}")
                self.assertIn("list of operations", r.json()["detail"])
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
