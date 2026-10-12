"""What package 3-11 tells clients of chats (design/phase3-features.md P3.5.4, P3.5.5, P3.9.1; gates 6 and 8).

- ``negotiation_status`` on the game socket: a seat's socket gets those of the chats its seat is in (``queued`` with
  its place, ``thinking``, ``waiting`` with a deadline, ``closed``); a third seat's socket, and a spectator's while a
  person plays, get none; a spectator of a game whose god view is allowed gets them.
- ``GET /api/games/{gid}/negotiations/{nid}``: a seat token reads the chats its seat is in; a viewer of the game reads
  any while the god view is allowed; a stranger, a visitor, a viewer while a person plays, a seat not in the chat and a
  chat the game does not have all get 404 (P3.9.1: by a stranger, an anonymous caller, the owner, an administrator,
  an ACL viewer and a moderator).
- MCP's ``wait_for_turn`` (``GET /api/games/{gid}/wait``) gives every chat its seat is in with its deadline.
"""
from __future__ import annotations

import os
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_chat_routes_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, servers, settings
from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.auth import accounts, policy, sessions
from citar.db.models import Base
from citar.pool import scheduler as S

settings.reset()


def until(fn, timeout: float = 15.0, step: float = 0.01):
    deadline = time.monotonic() + timeout
    while True:
        value = fn()
        if value or time.monotonic() > deadline:
            return value
        time.sleep(step)


class Replies(Conversation):
    """A model that replies to each message of a chat it is asked about."""

    def __init__(self, cfg, system, tools):
        self.tools = {t["name"] for t in tools}
        self.usage = {"input_tokens": 1, "output_tokens": 1}
        self.nid = None
        self.pending = False

    def add_user_text(self, text):
        import re
        found = re.search(r"negotiation #(\d+)", text)
        if found:
            self.nid = int(found.group(1))
        self.pending = True

    def add_tool_results(self, results):
        pass

    def step(self):
        if self.pending and self.nid is not None and "respond_negotiation" in self.tools:
            self.pending = False
            return StepResult(text="answer", tool_calls=[ToolCall("r", "respond_negotiation", {
                "negotiation_id": self.nid, "action": "reply", "message": "Tell me more."})])
        if "end_turn" in self.tools:
            return StepResult(text="done", tool_calls=[ToolCall("e", "end_turn", {})])
        return StepResult(text="nothing")


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
        r = self.client.post("/api/games", headers=self.headers[who], json={
            "name": "chats", "config": {"map_size": "small", "seed": 9, "barbarians": "off", "city_states": 0,
                                        **config}, "seats": seats})
        self.assertEqual(r.status_code, 200, r.text)
        info = r.json()
        self.games.append(info["id"])
        s = self.manager.get(info["id"])
        s.set_paused(True)                 # nobody plays on but the tests: the chats stay where they put them
        with s.lock:
            n = len(seats)
            s.game.apply_ops([{"op": "meet", "a": a, "b": b} for a in range(n) for b in range(a + 1, n)])
        return info

    def tool(self, gid: str, token: str, tool: str, args: dict) -> dict:
        r = self.client.post(f"/api/games/{gid}/tool", params={"token": token}, json={"tool": tool, "args": args})
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()

    def read(self, gid: str, nid: int, who=None, token=None, **params):
        headers = self.headers[who] if who else {}
        return self.client.get(f"/api/games/{gid}/negotiations/{nid}", headers=headers,
                               params={**({"token": token} if token else {}), **params})


class NegotiationRoute(Case):
    """``GET /api/games/{gid}/negotiations/{nid}`` by every kind of caller."""

    def test_a_game_a_person_plays(self):
        info = self.create("alice", [{"type": "human"}, {"type": "bot"}, {"type": "human"}])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        s = self.manager.get(gid)
        opened = self.tool(gid, tokens[0], "open_negotiation", {"to": 1, "message": "Friends?"})
        nid = opened["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10), "the bot answered")
        mine = self.read(gid, nid, token=tokens[0])
        self.assertEqual(mine.status_code, 200, mine.text)
        body = mine.json()
        self.assertEqual((body["id"], body["with"], body["your_move"]), (nid, 1, True))
        self.assertEqual((body["clock"]["waiting_on"], body["clock"]["kind"]), (0, "person"))
        self.assertTrue(body["clock"]["paused"], "the game is paused, so its clock stands still")
        theirs = self.read(gid, nid, token=tokens[1])
        self.assertEqual((theirs.status_code, theirs.json()["with"]), (200, 0), "the bot's seat reads it from its side")
        for label, r in (("a seat not in it", self.read(gid, nid, token=tokens[2])),
                         ("no such chat", self.read(gid, 999, token=tokens[0])),
                         ("a stranger", self.read(gid, nid, who="bob")),
                         ("a moderator", self.read(gid, nid, who="moe")),
                         ("the owner as a viewer while a person plays", self.read(gid, nid, who="alice")),
                         ("an administrator while a person plays", self.read(gid, nid, who="zara")),
                         ("the spectator link while a person plays", self.read(gid, nid, token=info["spectator_token"]))):
            self.assertEqual(r.status_code, 404, f"{label}: {r.status_code} {r.text}")
        visitor = self.read(gid, nid)
        self.assertIn(visitor.status_code, (401, 404))

    def test_a_game_no_person_plays_is_read_by_its_viewers(self):
        info = self.create("alice", [{"type": "mcp"}, {"type": "bot"}])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        s = self.manager.get(gid)
        nid = self.tool(gid, tokens[0], "open_negotiation", {"to": 1, "message": "Friends?"})["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))
        owner = self.read(gid, nid, who="alice")
        self.assertEqual((owner.status_code, owner.json().get("with")), (200, 1), "the opener's side")
        as_bot = self.read(gid, nid, who="alice", as_player=1)
        self.assertEqual(as_bot.json()["with"], 0, "through as_player's eyes when it is in the chat")
        self.assertEqual(self.read(gid, nid, who="zara").status_code, 200)
        self.assertEqual(self.read(gid, nid, token=info["spectator_token"]).status_code, 200)
        self.assertEqual(self.read(gid, nid, who="bob").status_code, 404, "a stranger: the game is private")
        self.assertEqual(self.read(gid, nid, who="moe").status_code, 404)
        self.assertIn(self.read(gid, nid).status_code, (401, 404))
        # an ACL viewer reads it
        shared = self.client.post(f"/api/games/{gid}/sharing/users", headers=self.headers["alice"],
                                  json={"handle": "bob", "permission": "view"})
        self.assertEqual(shared.status_code, 200, shared.text)
        self.assertEqual(self.read(gid, nid, who="bob").status_code, 200)
        # published to everyone (by an administrator: members do not publish to the whole web by default): a
        # moderator and a visitor read it
        public = self.client.put(f"/api/games/{gid}/sharing", headers=self.headers["zara"],
                                 json={"visibility": "global"})
        self.assertEqual(public.status_code, 200, public.text)
        self.assertEqual(self.read(gid, nid, who="moe").status_code, 200)
        self.assertEqual(self.read(gid, nid).status_code, 200)


class StatusEvents(Case):
    """Gate 6: who hears a chat's ``negotiation_status`` on the game socket."""

    END = {"type": "test_end"}

    def collect(self, sockets: dict, s) -> dict:
        """Everything each socket received until the test's end marker (broadcast to all)."""
        s._broadcast(dict(self.END))
        out = {}
        for name, ws in sockets.items():
            got = []
            while True:
                msg = ws.receive_json()
                if msg.get("type") == "test_end":
                    break
                got.append(msg)
            out[name] = [m for m in got if m.get("type") == "negotiation_status"]
        return out

    def test_the_two_sides_hear_every_state_and_nobody_else_does(self):
        from contextlib import ExitStack
        with mock.patch("citar.agents.llm_agent.make_conversation", Replies):
            info = self.create("zara", [{"type": "human"},
                                        {"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run"}},
                                        {"type": "human"}])
            gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
            s = self.manager.get(gid)
            s.set_paused(False)
            # the machine's one slot is busy with someone else's request: the model's answer queues behind it
            sched = S.get()
            m = sched.machine("sv_gpu")
            other = sched.lease("sv_gpu", "dry-run", kind="game", label="someone else")
            held = m.take(m.submit(other))
            self.assertIsNotNone(held)
            with ExitStack() as stack:
                sockets = {}
                for name, token in (("p0", tokens[0]), ("p1", tokens[1]), ("p2", tokens[2]),
                                    ("spectator", info["spectator_token"])):
                    ws = stack.enter_context(self.client.websocket_connect(
                        f"/ws/games/{gid}?token={token}", headers={"origin": "https://citar.test"}))
                    self.assertEqual(ws.receive_json()["type"], "hello")
                    sockets[name] = ws
                nid = self.tool(gid, tokens[0], "open_negotiation", {"to": 1, "message": "Maps?"})["result"][
                    "negotiation_id"]
                self.assertTrue(until(lambda: m.status()["waiting"] == 1, 10), m.status())
                time.sleep(0.3)                          # its place reaches the chat, on the notifier thread
                m.finish(held, gen_s=1.0)
                other.release()
                self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))
                self.tool(gid, tokens[0], "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                                  "message": "No."})
                got = self.collect(sockets, s)
        for side in ("p0", "p1"):
            mine = [x for x in got[side] if x["negotiation"] == nid]
            states = [x["state"] for x in mine]
            self.assertEqual({"queued", "thinking", "waiting", "closed"}, set(states), f"{side}: {states}")
            queued = next(x for x in mine if x["state"] == "queued")
            self.assertEqual((queued["position"], queued["waiting_on"], queued["kind"]), (1, 1, "model"))
            waiting = [x for x in mine if x["state"] == "waiting"]
            self.assertTrue(all(x["deadline"] and x["players"] == [0, 1] for x in waiting), waiting)
            self.assertEqual({x["waiting_on"] for x in waiting}, {0, 1}, "first the model, then the person")
            self.assertEqual(mine[-1]["state"], "closed")
            self.assertEqual(mine[-1]["status"], "rejected")
        self.assertEqual(got["p2"], [], "a third seat hears nothing of it")
        self.assertEqual(got["spectator"], [], "nor does a spectator while a person plays")

    def test_a_spectator_of_a_game_no_person_plays_hears_it(self):
        info = self.create("zara", [{"type": "mcp"}, {"type": "bot"}])
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        s = self.manager.get(gid)
        with self.client.websocket_connect(f"/ws/games/{gid}?token={info['spectator_token']}",
                                           headers={"origin": "https://citar.test"}) as ws:
            self.assertEqual(ws.receive_json()["type"], "hello")
            nid = self.tool(gid, tokens[0], "open_negotiation", {"to": 1, "message": "Friends?"})["result"][
                "negotiation_id"]
            self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))
            self.tool(gid, tokens[0], "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                              "message": "Never mind."})
            got = self.collect({"spectator": ws}, s)
        states = [x["state"] for x in got["spectator"] if x["negotiation"] == nid]
        self.assertIn("waiting", states)
        self.assertEqual(states[-1], "closed")


class DryRunKnobs(Case):
    """The dry run's knobs (how it answers chats, when it proposes) ride on a member's seat only on a dry-run machine:
    they dial nothing (``servers.member_llm``); anywhere else they go, as everything not a machine's name does."""

    def test_a_dry_run_machines_seat_keeps_them_and_another_machines_does_not(self):
        from citar.db.models import User
        with db.session() as s:
            alice = s.get(User, self.ids["alice"])
            kept = servers.member_llm({"server_id": "sv_dryrun", "model": "dry-run", "dry_run_propose_every": 3,
                                       "dry_run_negotiation": "accept", "base_url": "http://127.0.0.1:9/v1"}, alice)
            gone = servers.member_llm({"server_id": "sv_api", "model": "claude-opus-5", "dry_run_propose_every": 3},
                                      alice)
        self.assertEqual(kept, {"server_id": "sv_dryrun", "model": "dry-run", "dry_run_propose_every": 3,
                                "dry_run_negotiation": "accept"})
        self.assertEqual(gone, {"server_id": "sv_api", "model": "claude-opus-5"})
        cfg = servers.resolve_llm(kept)
        self.assertEqual((cfg["provider"], cfg["dry_run_propose_every"], cfg["dry_run_negotiation"]),
                         ("dryrun", 3, "accept"))


class WaitForTurnDeadlines(Case):
    """Gate 8: MCP's wait_for_turn gives each chat its deadline."""

    def test_the_wait_lists_each_chat_with_its_deadline(self):
        info = self.create("alice", [{"type": "human"}, {"type": "mcp"}], chat_model_reply_seconds=120)
        gid, tokens = info["id"], [seat["token"] for seat in info["seats"]]
        s = self.manager.get(gid)
        s.set_paused(False)
        nid = self.tool(gid, tokens[0], "open_negotiation", {"to": 1, "message": "Friends?"})["result"][
            "negotiation_id"]
        s.chat.note()
        r = self.client.get(f"/api/games/{gid}/wait", params={"token": tokens[1], "timeout": 1})
        self.assertEqual(r.status_code, 200, r.text)
        body = r.json()
        self.assertEqual((body["status"], body["negotiation_ids"]), ("negotiation", [nid]))
        chat = body["chats"][0]
        self.assertEqual((chat["negotiation_id"], chat["with"], chat["waiting_on"]), (nid, 0, "you"))
        self.assertTrue(0 < chat["seconds_left"] <= 120, chat)
        self.assertAlmostEqual(chat["deadline"], time.time() + chat["seconds_left"], delta=5)
        mine = self.client.get(f"/api/games/{gid}/wait", params={"token": tokens[0], "timeout": 1}).json()
        self.assertEqual(mine["chats"][0]["waiting_on"], "them")

    def test_the_mcp_instructions_say_a_chat_runs_out(self):
        from citar.agents import mcp_server
        self.assertIn("A chat waits only as long as the game allows", mcp_server.INSTRUCTIONS)
        wait_tool = next(t for t in mcp_server.EXTRA_TOOLS if t["name"] == "wait_for_turn")
        self.assertIn("seconds_left", wait_tool["description"])


if __name__ == "__main__":
    unittest.main()
