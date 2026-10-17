"""Members name machines; they never describe them (design/phase3-features.md P3.2.9; package 3-01's gate 5).

A seat's model block decides which address the server dials and which key it sends. From a member it may only name
a machine (`servers.member_llm`): a block with a base URL, a key variable or a provider of its own and no machine is
refused on every route that takes one (a new game, a seat change, a scenario's seats and its launch), a machine of
the registry is an administrator's until it is shared, and a pooled machine needs admission. While these requests
run, every way out is watched (`NoOutbound`: the network transports, the OpenAI and Anthropic clients, processes
started (the `lms` command line of an LM Studio machine), and reads of the key variables), and nothing may be used.
An administrator's inline block is still accepted.
"""
from __future__ import annotations

import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_seats_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

import anthropic
import httpx

from citar import db, engine_api, servers, settings
from citar.agents.providers import openai_provider
from citar.auth import accounts, policy, sessions
from citar.db.models import Base, Server, ServerGroup, User

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

#: The key variable a member's block names: it holds a secret of the server's.
SECRET_VAR = "CITAR_TEST_SEAT_SECRET"
WATCHED_VARS = {SECRET_VAR, "OPENAI_API_KEY", "ANTHROPIC_API_KEY"}
CONFIG = {"map_size": "duel", "seed": 21, "barbarians": "off"}

#: Blocks that describe a machine instead of naming one.
INLINE = {
    "base_url": {"model": "x", "base_url": "http://127.0.0.1:9/v1"},
    "api_key_env": {"model": "x", "provider": "anthropic", "api_key_env": SECRET_VAR},
    "openai_compatible": {"model": "x", "provider": "openai_compatible"},
}


class _WatchedEnviron(dict):
    """A copy of the environment that records every read of a watched variable."""

    def __init__(self, base, log):
        super().__init__(base)
        self._log = log

    def _note(self, key):
        if key in WATCHED_VARS:
            self._log.append(("env", key))

    def get(self, key, default=None):
        self._note(key)
        return super().get(key, default)

    def __getitem__(self, key):
        self._note(key)
        return super().__getitem__(key)

    def __contains__(self, key):
        self._note(key)
        return super().__contains__(key)


class NoOutbound:
    """Records, and refuses, every way a request could leave the server or reach its keys: httpx's network
    transports (the test client's own transport is not one), constructing the OpenAI or Anthropic client, starting
    a process (``subprocess.run`` and ``subprocess.Popen``, which the LM Studio command line goes through), and
    reading a watched environment variable. ``calls`` is what was tried."""

    def __init__(self):
        self.calls: list = []
        self._patches = []

    def _refuse(self, what, method=True):
        def refused(*args, **kwargs):
            self.calls.append((what, args[1:2] if method else args[:1]))
            raise RuntimeError(f"{what} was called")
        return refused

    def __enter__(self):
        os.environ[SECRET_VAR] = "sk-the-servers-own-secret"
        self._patches = [
            mock.patch.object(httpx.HTTPTransport, "handle_request", self._refuse("httpx")),
            mock.patch.object(httpx.AsyncHTTPTransport, "handle_async_request", self._refuse("httpx async")),
            mock.patch.object(openai_provider, "OpenAI", self._refuse("OpenAI client")),
            mock.patch.object(anthropic, "Anthropic", self._refuse("Anthropic client")),
            mock.patch.object(subprocess, "run", self._refuse("subprocess.run", method=False)),
            mock.patch.object(subprocess, "Popen", self._refuse("subprocess.Popen", method=False)),
            mock.patch.object(os, "environ", _WatchedEnviron(os.environ, self.calls)),
        ]
        for p in self._patches:
            p.start()
        return self

    def __exit__(self, *exc):
        for p in reversed(self._patches):
            p.stop()
        os.environ.pop(SECRET_VAR, None)
        return False


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class SeatCase(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        _reset()
        self.app = appmod
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("member", "user"), ("lender", "user"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.max_concurrent_games = 20
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            # every machine is a row (P3.2.8), the site's own included: the administrator shares the dry run with
            # every member, as a site that wants members to try it does
            self.share(s, "sv_dryrun", "everyone")
        self.games: list = []
        self.scenarios: list = []
        self.watch = NoOutbound().__enter__()

    def tearDown(self):
        self.watch.__exit__(None, None, None)
        for gid in self.games:
            s = self.manager.get(gid)
            if s is not None:
                self.manager.delete(gid)
                if s._driver is not None:
                    s._driver.join(10)
        for sid in self.scenarios:
            try:
                engine_api.delete_scenario(sid)
            except Exception:
                pass
        self.client.close()
        self.assertEqual(self.watch.calls, [], "something was dialled or a key variable read")

    def share(self, s, server_id: str, subject: str) -> None:
        """Put one of the site's machines in a group of the administrator's and grant it to ``subject`` (a handle,
        or "everyone"), in session ``s``."""
        from citar import pool
        zara = s.get(User, self.ids["zara"])
        group = pool.create_group(s, zara, name=f"shared {server_id}", visibility="server")
        s.get(Server, server_id).group_id = group.id
        pool.create_grant(s, zara, group, subject_type="everyone" if subject == "everyone" else "user",
                          subject_handle=None if subject == "everyone" else subject)

    def post(self, who, path, body):
        return self.client.post(path, json=body, headers=self.headers[who])

    def create(self, who, llm, expect, *, first_human=True):
        seats = [{"type": "human"}, {"type": "llm", "llm": llm}] if first_human else [{"type": "llm", "llm": llm}]
        r = self.post(who, "/api/games", {"name": "seats", "config": CONFIG, "seats": seats})
        self.assertEqual(r.status_code, expect, r.text)
        if r.status_code == 200:
            self.games.append(r.json()["id"])
            return r.json()
        return r.json()


class NewGames(SeatCase):
    def test_a_members_inline_block_is_refused(self):
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                detail = self.create("member", llm, 400)["detail"]
                self.assertIn("Pick a machine", detail)
        self.assertEqual(self.manager.sessions.keys() & set(self.games), set())

    def test_a_site_machine_is_an_admins_until_shared(self):
        """Since the registry merge (3-05) the site's machines are rows like any other, and admission decides: a
        member may seat the API only once its owner, the administrator, grants it."""
        self.assertEqual(servers.find("sv_api")["connection"]["provider"], "anthropic")
        detail = self.create("member", {"server_id": "sv_api", "model": "claude-opus-5"}, 403)["detail"]
        self.assertNotIn("Test API", detail, "a machine the member cannot see is not named")
        game = self.create("zara", {"server_id": "sv_api", "model": "claude-opus-5"}, 200)
        self.assertEqual(game["seats"][1]["llm"]["server_id"], "sv_api")
        with db.session() as s:
            self.share(s, "sv_api", "member")
        game = self.create("member", {"server_id": "sv_api", "model": "claude-opus-5", "api_key_env": SECRET_VAR}, 200)
        self.assertEqual(game["seats"][1]["llm"], {"server_id": "sv_api", "model": "claude-opus-5"})
        self.create("lender", {"server_id": "sv_api", "model": "claude-opus-5"}, 403)

    def test_a_dry_run_is_anyones(self):
        game = self.create("member", {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0,
                                      "base_url": "http://127.0.0.1:9/v1", "api_key_env": SECRET_VAR}, 200)
        # what a member's block keeps: the dry run, never the address or the key variable
        self.assertEqual(game["seats"][1]["llm"], {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0})
        game = self.create("member", {"server_id": "sv_dryrun", "model": "dry-run", "base_url": "http://x/",
                                      "provider": "anthropic", "server": "Dry run", "persona": "calm"}, 200)
        self.assertEqual(game["seats"][1]["llm"],
                         {"server_id": "sv_dryrun", "model": "dry-run", "persona": "calm"})
        # the site's dry-run machine is shared here (setUp); unshared, it is the administrator's like any machine
        with db.session() as s:
            s.get(Server, "sv_dryrun").group_id = None
        self.create("member", {"server_id": "sv_dryrun", "model": "dry-run"}, 403)

    def test_an_admins_inline_block_is_accepted(self):
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                game = self.create("zara", llm, 200)
                self.assertEqual(game["seats"][1]["llm"], llm)

    def test_a_members_model_seat_must_name_a_machine(self):
        for llm in ({}, None, {"server_id": None, "model_id": None, "model": "", "profile_id": None,
                                "max_tool_calls_per_turn": 150, "persona": ""}):
            with self.subTest(block=llm):
                self.assertIn("Pick a machine", self.create("member", llm, 400)["detail"])
        # a bot seat needs no machine, and an empty block on it is nothing
        r = self.post("member", "/api/games", {"name": "bots", "config": CONFIG,
                                               "seats": [{"type": "human"}, {"type": "bot", "llm": {}}]})
        self.assertEqual(r.status_code, 200, r.text)
        self.games.append(r.json()["id"])

    def test_an_unknown_machine_is_refused(self):
        self.assertIn("no such machine", self.create("member", {"server_id": "sv_nowhere"}, 400)["detail"].lower())

    def test_a_seat_that_plays_no_model_is_never_refused_for_its_block(self):
        """The lobby sends ``llm: s.llm`` for every seat, so a seat switched to a model seat and back to a bot carries
        the lobby's default block, which names no machine. Nothing plays it: a member's is dropped (or cleaned to the
        machine it names, which admission checks when the seat is made a model seat), an administrator's kept."""
        lobby_default = {"server_id": None, "model_id": None, "model": "", "profile_id": None,
                         "max_tool_calls_per_turn": 150, "persona": ""}
        for seats, first, kept in (
                ([{"type": "human"}, {"type": "bot", "llm": lobby_default}], {}, {}),
                ([{"type": "human", "llm": {"server_id": "sv_api", "base_url": "u"}}, {"type": "bot"}],
                 {"server_id": "sv_api"}, None),
                ([{"type": "human", "llm": {"server_id": "sv_nowhere"}}, {"type": "bot"}], {}, None),
                ([{"type": "human"}, {"type": "bot", "llm": INLINE["api_key_env"]}], {}, {}),
                ([{"type": "human"}, {"type": "bot", "llm": {"server_id": "sv_dryrun", "base_url": "u"}}],
                 {}, {"server_id": "sv_dryrun"})):
            with self.subTest(seats=seats):
                r = self.post("member", "/api/games", {"name": "no model", "config": CONFIG, "seats": seats})
                self.assertEqual(r.status_code, 200, r.text)
                self.games.append(r.json()["id"])
                s = self.manager.get(r.json()["id"])
                self.assertEqual([seat.type for seat in s.seats], ["human", "bot"])
                if kept is not None:
                    self.assertEqual(dict(s.seats[1].llm), kept)
                self.assertEqual(dict(s.seats[0].llm), first)
        r = self.post("zara", "/api/games", {"name": "admin's bot", "config": CONFIG,
                                             "seats": [{"type": "human"}, {"type": "bot", "llm": INLINE["base_url"]}]})
        self.assertEqual(r.status_code, 200, r.text)
        self.games.append(r.json()["id"])
        self.assertEqual(dict(self.manager.get(r.json()["id"]).seats[1].llm), INLINE["base_url"])

    def test_a_block_that_is_not_an_object_is_refused_for_anyone(self):
        for who in ("member", "zara"):
            for block in ("x", 5, [1], True):
                for stype in ("llm", "bot"):
                    with self.subTest(who=who, block=block, seat=stype):
                        r = self.post(who, "/api/games", {"name": "bad", "config": CONFIG,
                                                          "seats": [{"type": "human"}, {"type": stype, "llm": block}]})
                        self.assertEqual(r.status_code, 400, r.text)
                        self.assertIn("object", r.json()["detail"])
        self.assertEqual([g for g in self.manager.sessions.values() if g.name == "bad"], [])


class SeatChanges(SeatCase):
    def _bot_game(self):
        r = self.post("member", "/api/games", {"name": "a bot seat", "config": CONFIG,
                                               "seats": [{"type": "human"}, {"type": "bot"}]})
        self.assertEqual(r.status_code, 200, r.text)
        self.games.append(r.json()["id"])
        return r.json()["id"]

    def _seat(self, gid, pid=1):
        seat = self.manager.get(gid).seats[pid]
        return seat.type, dict(seat.llm)

    def test_a_members_inline_block_is_refused_and_the_seat_kept(self):
        gid = self._bot_game()
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                r = self.post("member", f"/api/games/{gid}/seats/1", {"type": "llm", "llm": llm})
                self.assertEqual(r.status_code, 400, r.text)
                self.assertEqual(self._seat(gid), ("bot", {}))

    def test_a_model_seat_without_a_machine_is_refused_and_the_seat_kept(self):
        gid = self._bot_game()
        r = self.post("member", f"/api/games/{gid}/seats/1", {"type": "llm"})
        self.assertEqual(r.status_code, 400, r.text)
        self.assertEqual(self._seat(gid), ("bot", {}))

    def test_a_pooled_machine_without_a_grant_is_refused_and_the_seat_kept(self):
        with db.session() as s:
            group = ServerGroup(owner_id=self.ids["lender"], name="Lender's box", visibility="private")
            s.add(group)
            s.flush()
            s.add(Server(id="sv_lent", owner_id=self.ids["lender"], group_id=group.id, name="Lender's GPU",
                         kind="owned", provider="worker", reach="worker", config={"models": [{"key": "m"}]}))
        gid = self._bot_game()
        r = self.post("member", f"/api/games/{gid}/seats/1", {"type": "llm", "llm": {"server_id": "sv_lent",
                                                                                     "model": "m"}})
        self.assertEqual(r.status_code, 403, r.text)
        self.assertNotIn("Lender", r.json()["detail"], "a machine the member cannot see is not named")
        self.assertEqual(self._seat(gid), ("bot", {}))

    def test_changing_only_the_type_checks_the_block_the_seat_has(self):
        with db.session() as s:
            s.add(Server(id="sv_lent2", owner_id=self.ids["lender"], name="Unshared", kind="owned",
                         provider="worker", reach="worker", config={}))
        gid = self._bot_game()
        self.manager.get(gid).seats[1].llm = {"server_id": "sv_lent2", "model": "m"}
        r = self.post("member", f"/api/games/{gid}/seats/1", {"type": "llm"})
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self.manager.get(gid).seats[1].type, "bot")

    def test_renaming_a_model_seat_checks_nothing(self):
        """A new name leaves the seat's block as it was admitted: a machine outside its window or over its budget must
        not stop its seat being renamed, and an ACL manager may rename an administrator's inline seat."""
        from citar.pool import seats as pool_seats
        r = self.post("member", "/api/games", {"name": "renamed", "config": CONFIG, "seats": [
            {"type": "human"}, {"type": "llm", "llm": {"server_id": "sv_dryrun", "model": "dry-run"}}]})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.games.append(gid)

        def closed(*_a, **_k):
            raise ValueError("That machine is outside its hours.")
        with mock.patch.object(pool_seats, "authorize", closed):
            for body in ({"name": "Napoleon"}, {"type": "llm", "name": "Josephine"}):
                with self.subTest(body=body):
                    r = self.post("member", f"/api/games/{gid}/seats/1", body)
                    self.assertEqual(r.status_code, 200, r.text)
            self.assertEqual(self.manager.get(gid).seats[1].name, "Josephine")
            # a new block is admitted again
            r = self.post("member", f"/api/games/{gid}/seats/1",
                          {"llm": {"server_id": "sv_dryrun", "model": "dry-run", "persona": "bold"}})
            self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self._seat(gid), ("llm", {"server_id": "sv_dryrun", "model": "dry-run"}))

        from citar.auth import access
        from citar.db.models import Game as GameRow
        r = self.post("zara", "/api/games", {"name": "admin's inline", "config": CONFIG,
                                             "seats": [{"type": "human"}, {"type": "llm", "llm": INLINE["base_url"]}]})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.games.append(gid)
        with db.session() as s:
            access.grant(s, s.get(GameRow, gid), s.get(User, self.ids["member"]), access.MANAGE,
                         actor=s.get(User, self.ids["zara"]))
        r = self.post("member", f"/api/games/{gid}/seats/1", {"name": "Wellington"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self._seat(gid), ("llm", INLINE["base_url"]))
        # the manager's own block is a member's
        r = self.post("member", f"/api/games/{gid}/seats/1", {"llm": INLINE["openai_compatible"]})
        self.assertEqual(r.status_code, 400, r.text)

    def test_a_dry_run_seat_change_is_accepted(self):
        gid = self._bot_game()
        r = self.post("member", f"/api/games/{gid}/seats/1",
                      {"type": "llm", "llm": {"provider": "dryrun", "model": "dry-run", "base_url": "http://x/"}})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self._seat(gid), ("llm", {"provider": "dryrun", "model": "dry-run"}))

    def test_an_admin_may_change_a_seat_to_an_inline_block(self):
        r = self.post("zara", "/api/games", {"name": "admin's", "config": CONFIG,
                                             "seats": [{"type": "human"}, {"type": "bot"}]})
        gid = r.json()["id"]
        self.games.append(gid)
        r = self.post("zara", f"/api/games/{gid}/seats/1", {"type": "llm", "llm": INLINE["base_url"]})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self._seat(gid), ("llm", INLINE["base_url"]))


class Scenarios(SeatCase):
    def _editor(self, who):
        r = self.post(who, "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                    "players": [{"type": "human"}, {"type": "llm"}]})
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()["editor_id"]

    def _seats(self, llm):
        return [{"type": "human"}, {"type": "llm", "llm": llm}]

    def test_a_members_scenario_save_with_an_inline_block_is_refused(self):
        eid = self._editor("member")
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                r = self.post("member", f"/api/scenario-editor/{eid}/save",
                              {"name": f"members {name}", "seats": self._seats(llm)})
                self.assertEqual(r.status_code, 400, r.text)
                r = self.post("member", f"/api/scenario-editor/{eid}/meta", {"seats": self._seats(llm)})
                self.assertEqual(r.status_code, 400, r.text)
        r = self.post("member", f"/api/scenario-editor/{eid}/save",
                      {"name": "members dry run", "seats": self._seats({"server_id": "sv_dryrun", "model": "dry-run"})})
        self.assertEqual(r.status_code, 200, r.text)
        self.scenarios.append(r.json()["id"])
        # a scenario's model seat may leave its machine to the launch
        r = self.post("member", f"/api/scenario-editor/{eid}/save",
                      {"id": "members-open", "name": "members open seat", "seats": [{"type": "human"}, {"type": "llm"}]})
        self.assertEqual(r.status_code, 200, r.text)
        self.scenarios.append(r.json()["id"])
        r = self.post("member", f"/api/scenarios/{r.json()['id']}/launch", {"name": "no machine"})
        self.assertEqual(r.status_code, 400, r.text)
        self.assertIn("Pick a machine", r.json()["detail"])

    def test_an_admins_inline_scenario_launches_for_a_member_only_once_it_names_a_machine(self):
        eid = self._editor("zara")
        r = self.post("zara", f"/api/scenario-editor/{eid}/save",
                      {"name": "admins inline", "seats": self._seats(INLINE["api_key_env"])})
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        self.scenarios.append(sid)
        r = self.client.put(f"/api/items/scenario/{sid}/sharing", json={"visibility": "server"},
                            headers=self.headers["zara"])
        self.assertEqual(r.status_code, 200, r.text)
        # the member's save of it, as it stands, is refused too
        eid2 = self.post("member", "/api/scenario-editor", {"source": "scenario", "scenario": sid}).json()["editor_id"]
        self.assertEqual(self.post("member", f"/api/scenario-editor/{eid2}/save", {"name": "copy"}).status_code, 400)
        r = self.post("member", f"/api/scenarios/{sid}/launch", {"name": "as it is"})
        self.assertEqual(r.status_code, 400, r.text)
        self.assertEqual([g for g in self.manager.sessions.values() if g.name == "as it is"], [])
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                r = self.post("member", f"/api/scenarios/{sid}/launch",
                              {"name": "with mine", "seats": [{}, {"type": "llm", "llm": llm}]})
                self.assertEqual(r.status_code, 400, r.text)
        r = self.post("member", f"/api/scenarios/{sid}/launch",
                      {"name": "named", "seats": [{}, {"type": "llm", "llm": {"server_id": "sv_dryrun",
                                                                                "model": "dry-run"}}]})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.games.append(gid)
        s = self.manager.get(gid)
        self.assertEqual(s.owner_id, self.ids["member"])
        self.assertEqual(s.seats[1].llm, {"server_id": "sv_dryrun", "model": "dry-run"})
        # an administrator launches it as it is
        r = self.post("zara", f"/api/scenarios/{sid}/launch", {"name": "admin's launch"})
        self.assertEqual(r.status_code, 200, r.text)
        self.games.append(r.json()["id"])

    def test_a_scenarios_bot_seat_with_a_block_is_saved_and_launched(self):
        lobby_default = {"server_id": None, "model": "", "persona": ""}
        eid = self._editor("member")
        seats = [{"type": "human"}, {"type": "bot", "llm": lobby_default}]
        r = self.post("member", f"/api/scenario-editor/{eid}/meta", {"seats": seats})
        self.assertEqual(r.status_code, 200, r.text)
        r = self.post("member", f"/api/scenario-editor/{eid}/save",
                      {"name": "members bots", "seats": [{"type": "human"}, {"type": "bot", "llm": INLINE["base_url"]}]})
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        self.scenarios.append(sid)
        r = self.post("member", f"/api/scenarios/{sid}/launch",
                      {"name": "bots launched", "seats": [{}, {"type": "bot", "llm": lobby_default}]})
        self.assertEqual(r.status_code, 200, r.text)
        self.games.append(r.json()["id"])
        s = self.manager.get(r.json()["id"])
        self.assertEqual((s.seats[1].type, dict(s.seats[1].llm)), ("bot", {}))
        r = self.post("member", f"/api/scenarios/{sid}/launch",
                      {"name": "bad block", "seats": [{}, {"type": "bot", "llm": "x"}]})
        self.assertEqual(r.status_code, 400, r.text)

    def test_a_refused_launch_leaves_nothing_behind(self):
        from citar.server import session as sess
        with db.session() as s:
            s.add(Server(id="sv_unshared", owner_id=self.ids["lender"], name="Unshared box", kind="owned",
                         provider="worker", reach="worker", config={}))
        eid = self._editor("member")
        r = self.post("member", f"/api/scenario-editor/{eid}/save", {"name": "members open", "seats": self._seats({})})
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        self.scenarios.append(sid)
        def folders():
            return {p.name for p in sess.SAVE_DIR.iterdir()} if sess.SAVE_DIR.exists() else set()
        before = folders()
        live = set(self.manager.sessions)
        r = self.post("member", f"/api/scenarios/{sid}/launch",
                      {"name": "refused", "seats": [{}, {"type": "llm", "llm": {"server_id": "sv_unshared", "model": "m"}}]})
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(set(self.manager.sessions), live)
        self.assertEqual(folders(), before)


class Agents(SeatCase):
    """Whatever a seat's block says, a member's game dials nothing it describes (``resolve_llm``'s passthrough)."""

    def test_an_inline_block_in_a_members_game_plays_no_model(self):
        # The second seat starts human, so the game waits at its first turn: two bots alone could play on past
        # turn 3, or to the end, before the test pauses them, and the model seat would never get a turn to watch.
        r = self.post("member", "/api/games", {"name": "agents", "config": CONFIG,
                                               "seats": [{"type": "bot"}, {"type": "human"}]})
        gid = r.json()["id"]
        self.games.append(gid)
        s = self.manager.get(gid)
        with s.lock:
            s.paused = True
            start = s.game.turn
        # put there behind the routes' back, as a game restored from an old save might have it
        s.update_seat(1, type="llm", llm=dict(INLINE["api_key_env"]))
        agent = s.get_agent(1)
        from citar.agents.llm_agent import NoMachineAgent
        self.assertIsInstance(agent, NoMachineAgent)
        s.resume()
        end = time.time() + 30
        texts: list[str] = []
        while time.time() < end:
            with s.lock:
                turn = s.game.turn
                texts = [e["text"] for e in s.game.events(last=500) if e.get("type") == "agent_error"]
            if turn >= start + 2 and texts:
                break
            time.sleep(0.05)
        self.assertGreaterEqual(turn, start + 2, "the seat's turns were skipped, the game went on")
        self.assertTrue(any("no machine" in t for t in texts), texts)

    def test_an_admins_game_keeps_its_inline_block(self):
        with db.session() as s:
            admin = s.get(User, self.ids["zara"])
        game = self.create("zara", {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0, "persona": "p"}, 200)
        s = self.manager.get(game["id"])
        self.assertEqual(s.owner_id, admin.id)
        self.assertTrue(s._inline_llm_allowed(INLINE["base_url"]))
        member_game = self.create("member", {"provider": "dryrun", "model": "dry-run"}, 200)
        self.assertFalse(self.manager.get(member_game["id"])._inline_llm_allowed(INLINE["base_url"]))
        cfg = servers.resolve_llm({"provider": "dryrun", "model": "dry-run", "base_url": "http://x/"},
                                  inline_ok=False)
        self.assertEqual(cfg, {"provider": "dryrun", "model": "dry-run"})


class Watch(SeatCase):
    def test_the_watch_sees_each_way_out(self):
        """The recorder these tests rely on records what it should: a request on the network, either client, and
        a read of a watched variable (and lets the test client's own requests through)."""
        with self.assertRaises(RuntimeError):
            httpx.get("http://127.0.0.1:9/models", timeout=1)
        with self.assertRaises(RuntimeError):
            openai_provider.OpenAI(base_url="http://127.0.0.1:9/v1", api_key="k")
        with self.assertRaises(RuntimeError):
            anthropic.Anthropic(api_key="k")
        with self.assertRaises(RuntimeError):
            subprocess.run(["lms", "ls", "--json"], capture_output=True)
        with self.assertRaises(RuntimeError):
            subprocess.Popen(["lms", "ls"])
        from citar.server import lmstudio
        with self.assertRaises(RuntimeError):
            lmstudio._run(["lms", "ls"], timeout=1)     # the way the server reaches the LM Studio command line
        self.assertEqual(os.environ.get(SECRET_VAR), "sk-the-servers-own-secret")
        self.assertEqual(self.client.get("/api/rules").status_code, 200)
        self.assertEqual([c[0] for c in self.watch.calls],
                         ["httpx", "OpenAI client", "Anthropic client", "subprocess.run", "subprocess.Popen",
                          "subprocess.run", "env"])
        self.watch.calls.clear()


class Gate(unittest.TestCase):
    """``servers.member_llm`` on its own."""

    def setUp(self):
        self.member = User(id="u1", handle="m", role="user", status="active")
        self.admin = User(id="u2", handle="a", role="admin", status="active")

    def test_admins_keep_everything(self):
        block = {"provider": "openai_compatible", "base_url": "http://h/v1", "api_key_env": "X", "timeout": 5}
        self.assertEqual(servers.member_llm(block, self.admin), block)

    def test_a_suspended_admin_is_a_member(self):
        self.admin.status = "suspended"
        with self.assertRaises(servers.SeatBlockError):
            servers.member_llm({"base_url": "http://h/v1"}, self.admin)

    def test_members_keep_the_machine_the_model_and_the_overrides_only(self):
        block = {"server_id": "sv_dryrun", "model": "m", "model_id": "mid", "profile_id": "p", "persona": "x",
                 "max_tokens": 10, "api_key": "k", "api_key_env": "K", "base_url": "u", "provider": "anthropic",
                 "timeout": 3, "surprise": 1}
        self.assertEqual(servers.member_llm(block, self.member),
                         {"server_id": "sv_dryrun", "model": "m", "model_id": "mid", "profile_id": "p",
                          "persona": "x", "max_tokens": 10})

    def test_empty_blocks_and_anonymous_callers(self):
        self.assertEqual(servers.member_llm(None, self.member), {})
        self.assertEqual(servers.member_llm({}, None), {})
        with self.assertRaises(servers.SeatBlockError) as caught:
            servers.member_llm({"model": "x"}, None)
        self.assertEqual(caught.exception.status, 400)


if __name__ == "__main__":
    unittest.main()
