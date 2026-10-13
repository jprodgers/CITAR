"""The client's payloads (design/phase3-features.md P3.5.8, package 3-13; gates 2 to 4).

- **The replay is `Delta`.** ``/api/games/{gid}/replay`` and ``EngineGame.replay_data`` send the frames as the journal
  stores them (keyframes and deltas, base64), the binding refuses the retired ``full`` format, and a reloaded game's
  replay is the live one's byte for byte (tests/test_saves.py compares the same through a session's autosave chain).
- **View ETags.** Two answers with one ETag have the same bytes; a move, a message (history only, which the engine's
  revision does not count), a pause, an agent's status, a seat's change and another perspective each move it; a
  request whose ``If-None-Match`` names the current tag gets 304 without the view being built (a counter on
  ``view_json``), and only once the caller has passed the game's gate.
- **Trade options on demand.** No seat's view carries ``trade_options`` (four civilizations, all met);
  ``GET /api/games/{gid}/trade-options/{other}`` gives them to a seat token for its own seat and to a spectator of a
  game whose god view is allowed (``as_player``), with what ``get_diplomacy`` lists; it 404s for a civilization not
  met, for itself, for one the game lacks, and for everyone else (P3.9.1: a stranger, a visitor, the owner and an
  administrator while a person plays, a moderator, the spectator link while a person plays), and an ACL viewer and
  the owner read an AI game's.
"""
from __future__ import annotations

import base64
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_client_payloads_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import _engine as E, db, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import Base
from citar.engine_api import EngineGame
from citar.pool import scheduler as S

settings.reset()


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

    def create(self, who: str, seats: list, meet=None, **config) -> dict:
        """A paused game (nobody plays on but the test), with the pairs in ``meet`` met (every pair when None)."""
        r = self.client.post("/api/games", headers=self.headers[who], json={
            "name": "payloads", "config": {"map_size": "small", "seed": 9, "barbarians": "off", "city_states": 0,
                                           **config}, "seats": seats})
        self.assertEqual(r.status_code, 200, r.text)
        info = r.json()
        self.games.append(info["id"])
        s = self.manager.get(info["id"])
        s.set_paused(True)
        n = len(seats)
        pairs = [(a, b) for a in range(n) for b in range(a + 1, n)] if meet is None else meet
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": a, "b": b} for a, b in pairs])
        return info

    def view(self, gid: str, who=None, token=None, etag=None, **params):
        headers = dict(self.headers[who]) if who else {}
        if etag is not None:
            headers["if-none-match"] = etag
        return self.client.get(f"/api/games/{gid}/view", headers=headers,
                               params={**({"token": token} if token else {}), **params})

    def options(self, gid: str, other: int, who=None, token=None, **params):
        headers = self.headers[who] if who else {}
        return self.client.get(f"/api/games/{gid}/trade-options/{other}", headers=headers,
                               params={**({"token": token} if token else {}), **params})

    def tool(self, gid: str, token: str, tool: str, args: dict) -> dict:
        r = self.client.post(f"/api/games/{gid}/tool", params={"token": token}, json={"tool": tool, "args": args})
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()


class Replay(Case):
    """Gate 2: the replay is the journal's frames as stored; ``full`` is gone."""

    def test_the_route_and_the_facade_send_the_stored_records(self):
        info = self.create("alice", [{"type": "bot"}, {"type": "bot"}])
        gid = info["id"]
        s = self.manager.get(gid)
        with s.lock:
            for _ in range(6):
                s.game.execute(s.game.current, "end_turn", {})
        r = self.client.get(f"/api/games/{gid}/replay", headers=self.headers["alice"])
        self.assertEqual(r.status_code, 200, r.text)
        data = json.loads(r.content)
        self.assertEqual(data["format"], "delta")
        frames = data["frames"]
        self.assertGreaterEqual(len(frames), 3)
        self.assertTrue(frames[0]["keyframe"] and not any(f["keyframe"] for f in frames[1:]), "a keyframe, then deltas")
        for f in frames:
            self.assertEqual(set(f), {"turn", "keyframe", "bytes"})
            raw = base64.b64decode(f["bytes"])
            self.assertEqual(raw[:4], b"CF\x01" + (b"K" if f["keyframe"] else b"D"), "a version 1 frame record")
        with s.lock:
            facade = s.game.replay_data()
        self.assertEqual(facade["frames"], frames, "the facade's frames are the route's")
        self.assertEqual(facade["config"], s.game.config)

    def test_a_call_for_full_frames_fails(self):
        g = E.Game.new(json.dumps({"seed": 5, "map_size": "duel", "players": [{"controller": "bot"}] * 2}).encode())
        for _ in range(3):
            g.end_turn(g.current)
        with self.assertRaisesRegex(ValueError, "'full' replay format was retired"):
            g.replay_data("full")
        with self.assertRaisesRegex(ValueError, "Unknown replay format"):
            g.replay_data("mpeg")
        self.assertEqual(json.loads(g.replay_data())["format"], "delta")
        self.assertEqual(json.loads(g.replay_data("delta")), json.loads(g.replay_data()))

    def test_a_reloaded_game_sends_the_same_bytes(self):
        g = EngineGame.new({"seed": 7, "map_size": "duel", "players": [{"controller": "bot"}] * 2})
        for _ in range(8):
            g.execute(g.current, "end_turn", {})
        back = EngineGame.from_save(g.to_save())
        self.assertEqual(back.replay_data()["frames"], g.replay_data()["frames"], "the Delta bytes, record for record")


class ViewETags(Case):
    """Gate 3: what moves a view's ETag, and the 304 that builds nothing."""

    def tag_and_body(self, gid, **kw):
        r = self.view(gid, **kw)
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.headers.get("cache-control"), "private, no-cache")
        return r.headers.get("etag"), r.content

    def test_the_same_tag_is_the_same_body_and_every_change_moves_it(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}, {"type": "human"}])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        s = self.manager.get(gid)
        seen: dict = {}

        def look(label, token=tokens[0], **params):
            tag, body = self.tag_and_body(gid, token=token, **params)
            self.assertTrue(tag and tag.startswith('"') and tag.endswith('"'), tag)
            if tag in seen:
                self.assertEqual(body, seen[tag], f"{label}: one tag, one body")
            seen[tag] = body
            return tag

        first = look("the first look")
        self.assertEqual(look("again, nothing changed"), first)
        changes = [("the first look", first)]
        # a move: an action through the tool route
        self.assertTrue(self.tool(gid, tokens[0], "set_research", {"tech": "Pottery"})["ok"])
        changes.append(("a move", look("after a move")))
        # a message, which is history only: the engine's revision does not move, the game's writes do
        before = s.game._g.revision
        self.assertTrue(self.tool(gid, tokens[0], "send_message", {"to": 1, "text": "Hello there."})["ok"])
        self.assertEqual(s.game._g.revision, before, "a message is no write to the state")
        changes.append(("a message", look("after a message")))
        # a thought, which the view does not show, still moves it (a command): never a wrong 304
        with s.lock:
            s.game.add_thought(1, "thinking", "note")
        changes.append(("a thought", look("after a thought")))
        # a pause
        r = self.client.post(f"/api/games/{gid}/control", headers=self.headers["alice"], json={"paused": False})
        self.assertEqual(r.status_code, 200, r.text)
        changes.append(("a resume", look("after the resume")))
        s.set_paused(True)
        changes.append(("a pause", look("after the pause")))
        # an agent's status
        s.agent_status[1] = "thinking"
        changes.append(("an agent status", look("after an agent status")))
        # a seat's change
        r = self.client.post(f"/api/games/{gid}/seats/2", headers=self.headers["alice"], json={"name": "Carol"})
        self.assertEqual(r.status_code, 200, r.text)
        changes.append(("a seat change", look("after a seat change")))
        for (was, before_tag), (now, after_tag) in zip(changes, changes[1:]):
            self.assertNotEqual(after_tag, before_tag, f"{now} moved it from {was}'s")
        tags = [t for _, t in changes]
        # paused again with nothing else changed is the view as it was after the thought: the same body, the same tag
        self.assertEqual(dict(changes)["a pause"], dict(changes)["a thought"])
        self.assertEqual(len(set(tags)), len(tags) - 1, f"the rest all differ: {changes}")
        # another perspective: the other person's seat, at the same moment
        other = look("the other seat", token=tokens[2])
        self.assertNotIn(other, tags, "another seat's view has its own tag")
        self.assertEqual(look("the first seat again"), tags[-1])

    def test_a_matching_tag_gets_304_without_the_view_being_built(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        s = self.manager.get(gid)
        tag, body = self.tag_and_body(gid, token=token)
        real = s.game.view_json
        built = []

        def counted(*a, **k):
            built.append(1)
            return real(*a, **k)

        with mock.patch.object(s.game, "view_json", side_effect=counted):
            for header in (tag, f"W/{tag}", f'"nothing", {tag}', "*"):
                r = self.view(gid, token=token, etag=header)
                self.assertEqual((r.status_code, r.content), (304, b""), header)
                self.assertEqual(r.headers.get("etag"), tag)
            self.assertEqual(built, [], "no view was built for a 304")
            stale = self.view(gid, token=token, etag='"v-something-else"')
            self.assertEqual((stale.status_code, stale.content), (200, body))
            self.assertEqual(len(built), 1)
            # the gate comes first: a stranger with the tag is told nothing
            r = self.view(gid, who="bob", etag=tag)
            self.assertEqual(r.status_code, 404)
            # a spectator link has its own perspective, and so its own tag
            self.assertEqual(self.view(gid, token=info["spectator_token"], etag=tag).status_code, 403,
                             "the spectator view is refused while a person plays, tag or not")
        # a command that lands while the view is built: the body may be the game after it, so no tag
        def racing(*a, **k):
            s.game.add_thought(0, "meanwhile", "note")
            return real(*a, **k)

        with mock.patch.object(s.game, "view_json", side_effect=racing):
            r = self.view(gid, token=token)
        self.assertEqual(r.status_code, 200)
        self.assertNotIn("etag", {k.lower() for k in r.headers}, "no tag for a view a command overtook")
        # a loaded game is a new game object: its counts start again, under another name
        again = EngineGame.from_save(s.game.to_save())
        self.assertNotEqual(again.instance, s.game.instance)
        self.assertLessEqual(again.writes, s.game.writes)

    def test_a_query_through_the_tool_route_leaves_the_tag(self):
        """The browser asks for the selected unit (``get_unit``) or city after every refresh, and models query freely: a
        query changes nothing (property P8), so the next refresh with the tag in hand is still a 304. An action, even a
        refused one, moves the tag."""
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}])
        gid, token = info["id"], info["seats"][0]["token"]
        s = self.manager.get(gid)
        # the seat's first tool call marks it connected (the seat's public part, in the view): done before the tag
        self.assertTrue(self.tool(gid, token, "get_empire", {})["ok"])
        tag, body = self.tag_and_body(gid, token=token)
        unit = next(u["id"] for u in json.loads(body)["units"] if u["owner"] == 0)
        writes, revision = s.game.writes, s.game._g.revision
        real = s.game.view_json
        built = []

        def counted(*a, **k):
            built.append(1)
            return real(*a, **k)

        with mock.patch.object(s.game, "view_json", side_effect=counted):
            for name, args in (("get_unit", {"unit_id": unit}), ("get_diplomacy", {}), ("get_empire", {}),
                               ("get_tile", {"x": 0, "y": 0})):
                r = self.tool(gid, token, name, args)
                self.assertTrue(r["ok"], r)
                again = self.view(gid, token=token, etag=tag)
                self.assertEqual((again.status_code, again.content), (304, b""), f"after {name}")
                self.assertEqual(again.headers.get("etag"), tag)
            # a refused query is a read too
            self.assertFalse(self.tool(gid, token, "get_unit", {"unit_id": 99999})["ok"])
            self.assertEqual(self.view(gid, token=token, etag=tag).status_code, 304, "after a refused query")
            self.assertEqual(built, [], "no view was built")
            self.assertEqual((s.game.writes, s.game._g.revision), (writes, revision), "a query counts as no command")
            # a refused action is a command (the count moves, never wrongly a 304), and so is an action
            self.assertFalse(self.tool(gid, token, "found_city", {"unit_id": 99999})["ok"])
            self.assertEqual(s.game.writes, writes + 1)
            refused = self.view(gid, token=token, etag=tag)
            self.assertEqual((refused.status_code, refused.content), (200, body), "the same body under a new tag")
            self.assertNotEqual(refused.headers.get("etag"), tag)
            self.assertTrue(self.tool(gid, token, "set_research", {"tech": "Pottery"})["ok"])
            moved = self.view(gid, token=token, etag=refused.headers.get("etag"))
            self.assertEqual(moved.status_code, 200)
            self.assertNotEqual(moved.content, body)
            self.assertEqual(len(built), 2)

    def test_an_ai_game_viewer_through_one_civilizations_eyes(self):
        info = self.create("alice", [{"type": "bot"}, {"type": "bot"}])
        gid = info["id"]
        a, _ = self.tag_and_body(gid, who="alice", as_player=0)
        b, _ = self.tag_and_body(gid, who="alice", as_player=1)
        god, god_body = self.tag_and_body(gid, who="alice")
        spectator, _ = self.tag_and_body(gid, token=info["spectator_token"])
        admin, admin_body = self.tag_and_body(gid, who="zara")
        self.assertEqual(len({a, b, god, spectator}), 4,
                         "each civilization's eyes and the whole map their own tags; the spectator link's controls differ")
        self.assertEqual((admin, admin_body), (god, god_body),
                         "an administrator and the owner see the same god view, managing it: one body, one tag")
        self.assertEqual(self.view(gid, who="alice", etag=a, as_player=0).status_code, 304)
        self.assertEqual(self.view(gid, who="alice", etag=a, as_player=1).status_code, 200)


class TradeOptions(Case):
    """Gate 4: trade options left the views and come from their own route."""

    def test_no_seats_view_carries_them_and_the_tool_still_does(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}, {"type": "bot"}, {"type": "human"}])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        for pid, token in enumerate(tokens):
            r = self.view(gid, token=token)
            self.assertEqual(r.status_code, 200, r.text)
            listed = r.json()["diplomacy"]["players"]
            self.assertEqual(sorted(p["id"] for p in listed), [q for q in range(4) if q != pid], "all met")
            self.assertTrue(all("trade_options" not in p for p in listed), f"seat {pid}'s view")
            self.assertNotIn(b"trade_options", r.content)
        tool = self.tool(gid, tokens[0], "get_diplomacy", {})
        self.assertTrue(tool["ok"], tool)
        by_id = {p["id"]: p["trade_options"] for p in tool["result"]["players"]}
        self.assertEqual(sorted(by_id), [1, 2, 3], "get_diplomacy lists them for each civilization met")
        for other in (1, 2, 3):
            r = self.options(gid, other, token=tokens[0])
            self.assertEqual(r.status_code, 200, r.text)
            self.assertEqual(r.json(), {"player": 0, "with": other, "trade_options": by_id[other]},
                             "the route gives what get_diplomacy lists")

    def test_who_gets_them_while_a_person_plays(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}, {"type": "human"}], meet=[(0, 1)])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        r = self.options(gid, 1, token=tokens[0])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertIn("they_could_give", r.json()["trade_options"])
        self.assertEqual(self.options(gid, 0, token=tokens[1]).status_code, 200, "the bot's seat, its own side")
        for label, r in (("a civilization not met", self.options(gid, 2, token=tokens[0])),
                         ("itself", self.options(gid, 0, token=tokens[0])),
                         ("one the game lacks", self.options(gid, 7, token=tokens[0])),
                         ("past a player id", self.options(gid, 300, token=tokens[0])),
                         ("a seat that met nobody", self.options(gid, 0, token=tokens[2])),
                         ("a seat's token cannot borrow another's eyes",
                          self.options(gid, 1, token=tokens[2], as_player=0)),
                         ("a stranger", self.options(gid, 1, who="bob", as_player=0)),
                         ("a moderator", self.options(gid, 1, who="moe", as_player=0)),
                         ("the owner as a viewer while a person plays", self.options(gid, 1, who="alice", as_player=0)),
                         ("an administrator while a person plays", self.options(gid, 1, who="zara", as_player=0)),
                         ("the spectator link while a person plays",
                          self.options(gid, 1, token=info["spectator_token"], as_player=0))):
            self.assertEqual(r.status_code, 404, f"{label}: {r.status_code} {r.text}")
        self.assertIn(self.options(gid, 1, as_player=0).status_code, (401, 404), "a visitor")

    def test_spectators_of_an_ai_game_read_them_through_one_civilizations_eyes(self):
        info = self.create("alice", [{"type": "mcp"}, {"type": "bot"}, {"type": "bot"}], meet=[(0, 1)])
        gid = info["id"]
        owner = self.options(gid, 1, who="alice", as_player=0)
        self.assertEqual(owner.status_code, 200, owner.text)
        self.assertEqual(owner.json()["player"], 0)
        self.assertEqual(self.options(gid, 0, who="alice", as_player=1).status_code, 200, "from the other side")
        self.assertEqual(self.options(gid, 2, who="alice", as_player=0).status_code, 404, "not met")
        self.assertEqual(self.options(gid, 1, who="alice").status_code, 404, "a spectator names whose eyes")
        self.assertEqual(self.options(gid, 1, who="alice", as_player=9).status_code, 404, "no such seat")
        self.assertEqual(self.options(gid, 1, who="zara", as_player=0).status_code, 200, "an administrator")
        self.assertEqual(self.options(gid, 1, token=info["spectator_token"], as_player=0).status_code, 200)
        self.assertEqual(self.options(gid, 1, who="bob", as_player=0).status_code, 404, "a stranger: the game is private")
        self.assertEqual(self.options(gid, 1, who="moe", as_player=0).status_code, 404, "a moderator")
        self.assertIn(self.options(gid, 1, as_player=0).status_code, (401, 404), "a visitor")
        shared = self.client.post(f"/api/games/{gid}/sharing/users", headers=self.headers["alice"],
                                  json={"handle": "bob", "permission": "view"})
        self.assertEqual(shared.status_code, 200, shared.text)
        self.assertEqual(self.options(gid, 1, who="bob", as_player=0).status_code, 200, "an ACL viewer")
        public = self.client.put(f"/api/games/{gid}/sharing", headers=self.headers["zara"], json={"visibility": "global"})
        self.assertEqual(public.status_code, 200, public.text)
        self.assertEqual(self.options(gid, 1, who="moe", as_player=0).status_code, 200, "published: a moderator reads")
        self.assertEqual(self.options(gid, 1, as_player=0).status_code, 200, "published: a visitor reads")

    def test_the_facade_gives_none_where_the_view_would_list_nothing(self):
        g = EngineGame.new({"seed": 3, "map_size": "duel", "city_states": 1,
                            "players": [{"controller": "bot"}, {"controller": "bot"}]})
        self.assertIsNone(g.trade_options(0, 1), "not met")
        g.meet(0, 1)
        self.assertIn("agreements_possible_now", g.trade_options(0, 1))
        for pid, other in ((0, 0), (0, 2), (0, 99), (99, 0), (-1, 1), (0, -1)):
            self.assertIsNone(g.trade_options(pid, other), (pid, other))


if __name__ == "__main__":
    unittest.main()
