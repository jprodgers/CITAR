"""Hybrid seats in the lobby, the seats dialog and scenarios (design/phase3-features.md P3.6.8, P3.6.2; package 3-15's
gates 1 and 2).

- **The lobby's payloads** (gate 1): each hybrid seat of ``citar/web/tests/fixtures/hybrid-seats.json`` (the seats
  ``seatPayload`` makes from the lobby's form, which ``citar/web/tests/seats.test.mjs`` checks: each preset and a grid of
  its own, each handicap) makes a game, as a member on a machine shared with her for pooled prompts, whose seat is the
  engine's hybrid controller with the handicap, the automatic decisions and the owners the form chose, its level and
  civilization too.
- **The seat route** (gate 1): ``POST /api/games/{gid}/seats/{pid}`` takes ``hybrid`` and ``handicap``. A seat made
  hybrid and back to a bot works both ways, and every change that makes it a model seat is admitted again by its task
  class (P3.3.7): refused once her grant is gone, and an LLM seat (a grant for games) refused her all along, each
  refusal leaving the seat and its engine controller as they were. Settings that are not allowed are a 400 with the
  reason; "Best bot" is pinned; the route answers a stranger 404, an anonymous caller 401 or 404, an ACL viewer and a
  moderator on a published game 403, the owner and an administrator 200 (P3.9.1), and a member's hybrid seat that
  describes a machine dials nothing (P3.2.9).
- **Scenarios** (gate 2): a scenario saved with a hybrid seat (its settings checked when saved, 400 with the seat named)
  launches with it, from its defaults or from the launch dialog's seats; launched as a bot, the seat keeps none of the
  hybrid seat's handicap; a scenario made from a running game keeps its hybrid seat's settings.
"""
from __future__ import annotations

import json
import os
import secrets
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401  (the temporary saves folder and registry; before citar)
from tests.test_seat_blocks import NoOutbound

_TMP = Path(tempfile.mkdtemp(prefix="citar_hybrid_lobby_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, engine_api, pool, servers, settings
from citar.agents import hybrid
from citar.auth import access, accounts, policy, sessions
from citar.db.models import AccessGrant, Base, Game as GameRow, Server, ServerGroup, User
from citar.pool import scheduler as S
from citar.pool import seats as pool_seats

settings.reset()

FIXTURE = json.loads((Path(__file__).resolve().parent.parent / "citar" / "web" / "tests" / "fixtures"
                      / "hybrid-seats.json").read_text(encoding="utf-8"))
#: The lobby's model block for a seat on the shared dry-run machine (the fixture's, as llmForm makes it).
LLM = FIXTURE["llm"]
CONFIG = {"map_size": "duel", "seed": 15, "barbarians": "off", "city_states": 0}


def expected_owners(form_owners: dict) -> dict:
    """The whole owners map the form held (its ten categories, the bot's where it did not say the model's)."""
    return {c: ("llm" if form_owners.get(c) == "llm" else "bot") for c in hybrid.CATEGORIES}


def expected_auto(owners: dict) -> dict:
    """The engine's automatic decisions for a hybrid seat with these owners (P3.6.2), worked out here, not by the code
    under test: it votes and annexes for the seat where the bot decides those, and always makes its free picks."""
    return {"un_vote": owners["un"] == "bot", "conquest": owners["captured_cities"] == "bot", "free_picks": True}


class World(unittest.TestCase):
    """A fresh database with the suite's machines; alice, a member whose grant on the site's dry-run machine covers
    pooled prompts only (a hybrid seat, not an LLM seat); bob, another member; rita, who will be given a view of
    alice's game; maggie, a moderator; zara, an administrator."""

    WHO = (("alice", "user"), ("bob", "user"), ("rita", "user"), ("maggie", "moderator"), ("zara", "admin"))

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        db.dispose()
        db.configure(os.environ["CITAR_DB_URL"])
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers, self.ids = {"anon": {"origin": "https://citar.test"}}, {}
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
            zara = s.get(User, self.ids["zara"])
            group = pool.create_group(s, zara, name="The site's dry run", visibility="server")
            s.get(Server, LLM["server_id"]).group_id = group.id
            self.grant_id = pool.create_grant(s, zara, group, subject_type="user", subject_handle="alice",
                                              purposes=["pooled"]).id
            self.group_id = group.id
        servers.invalidate()
        self.games: list = []

    def tearDown(self):
        for gid in self.games:
            if self.manager.get(gid) is not None:
                self.manager.delete(gid)

    def call(self, who, method, path, body=None):
        return self.client.request(method, path, json=body, headers=self.headers[who])

    def create(self, who: str, seats: list, **config):
        r = self.call(who, "POST", "/api/games", {"name": "hybrid lobby", "config": {**CONFIG, **config},
                                                  "seats": seats})
        if r.status_code == 200:
            gid = r.json()["id"]
            self.games.append(gid)
            self.manager.get(gid).set_paused(True)          # nothing plays on but the test
        return r

    def engine_seat(self, gid: str, pid: int) -> dict:
        s = self.manager.get(gid)
        with s.lock:
            p = s.game.player(pid)
        return {"controller": p["controller"], "handicap": p["handicap"], "auto": dict(p["auto"]),
                "difficulty": p["difficulty"], "nation": p["nation"]}

    def revoke(self):
        with db.session() as s:
            s.delete(s.get(AccessGrant, self.grant_id))
        servers.invalidate()

    def regrant(self, purposes):
        with db.session() as s:
            zara = s.get(User, self.ids["zara"])
            self.grant_id = pool.create_grant(s, zara, s.get(ServerGroup, self.group_id), subject_type="user",
                                              subject_handle="alice", purposes=purposes).id
        servers.invalidate()


# ---------------------------------------------------------------------------- gate 1: the lobby's payloads

class LobbyPayloads(World):
    def test_the_fixture_covers_each_preset_a_grid_and_each_handicap(self):
        kinds = {c["payload"]["hybrid"]["owners"] if isinstance(c["payload"]["hybrid"]["owners"], str) else "grid"
                 for c in FIXTURE["cases"]}
        self.assertEqual(kinds, {"trade_only", "all_but_war", "everything", "grid"})
        self.assertEqual({c["payload"]["handicap"] for c in FIXTURE["cases"]}, {"ai", "human"})

    def test_each_lobby_payload_starts_with_its_controller_handicap_automatic_decisions_and_owners(self):
        for case in FIXTURE["cases"]:
            with self.subTest(case=case["name"]):
                payload = {**case["payload"], "llm": dict(LLM)}
                r = self.create("alice", [{"type": "human"}, payload, {"type": "bot"}])
                self.assertEqual(r.status_code, 200, r.text)
                gid = r.json()["id"]
                owners = expected_owners(case["seat"]["hybrid"]["owners"])
                handicap = case["seat"].get("handicap") or "ai"
                eng = self.engine_seat(gid, 1)
                self.assertEqual((eng["controller"], eng["handicap"], eng["auto"]),
                                 ("hybrid", handicap, expected_auto(owners)))
                self.assertEqual(eng["difficulty"], case["seat"]["difficulty"] or "Prince")
                if case["seat"]["nation"] != "random":
                    self.assertEqual(eng["nation"], case["seat"]["nation"])
                seat = self.manager.get(gid).seats[1]
                self.assertEqual(seat.type, "hybrid")
                self.assertEqual(seat.hybrid["owners"], owners)
                self.assertEqual(seat.hybrid.get("preset"), hybrid.preset_of(owners))
                self.assertEqual((seat.hybrid["advisor"], seat.hybrid["prompt_every"]),
                                 (case["seat"]["hybrid"]["advisor"], case["seat"]["hybrid"]["prompt_every"]))
                self.assertEqual(seat.bot.get("profile"), "standard")
                self.assertEqual((seat.llm["server_id"], seat.task_class, seat.engine), (LLM["server_id"], "partial", {}))
                # the bot and the person keep their own controllers
                self.assertEqual(self.engine_seat(gid, 2)["controller"], "bot")
                self.assertEqual(self.engine_seat(gid, 0)["controller"], "human")
                # what the lobby lists back: the seat's settings, and whose numbers it plays with
                info = self.call("alice", "GET", f"/api/games/{gid}").json()
                self.assertEqual(info["seats"][1]["hybrid"]["owners"], owners)
                self.assertEqual(info["players"][1]["handicap"], handicap)

    def test_a_member_without_a_grant_for_pooled_prompts_is_refused_the_lobby_s_seat(self):
        self.revoke()
        r = self.create("alice", [{"type": "human"}, {**FIXTURE["cases"][0]["payload"], "llm": dict(LLM)}])
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self.games, [])


# ---------------------------------------------------------------------------- gate 1: the seat route

class SeatRoute(World):
    HYBRID = {"type": "hybrid", "llm": LLM, "bot": {"profile": "standard"}, "handicap": "human",
              "hybrid": {"owners": "all_but_war", "advisor": True, "prompt_every": 4}}

    def game(self) -> str:
        r = self.create("alice", [{"type": "human"}, {"type": "bot"}, {"type": "bot"}])
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()["id"]

    def seat(self, gid: str, body: dict, who: str = "alice"):
        return self.call(who, "POST", f"/api/games/{gid}/seats/1", body)

    def test_a_seat_made_hybrid_and_back_to_a_bot_is_admitted_each_time_it_plays_a_model(self):
        gid = self.game()
        s = self.manager.get(gid)
        admitted: list = []
        real = pool_seats.authorize

        def watch(session, user, seats, **kw):
            admitted.append([pool_seats.seat_class(x) for x in seats])
            return real(session, user, seats, **kw)

        with mock.patch.object(pool_seats, "authorize", watch):
            r = self.seat(gid, self.HYBRID)
            self.assertEqual(r.status_code, 200, r.text)
            owners = hybrid.owners_map("all_but_war")
            self.assertEqual(self.engine_seat(gid, 1)["controller"], "hybrid")
            self.assertEqual((self.engine_seat(gid, 1)["handicap"], self.engine_seat(gid, 1)["auto"]),
                             ("human", expected_auto(owners)))
            self.assertEqual((s.seats[1].type, s.seats[1].hybrid["preset"], s.seats[1].hybrid["prompt_every"]),
                             ("hybrid", "all_but_war", 4))
            self.assertEqual(r.json()["seats"][1]["hybrid"]["owners"], owners)
            self.assertEqual(r.json()["players"][1]["handicap"], "human")
            self.assertEqual(admitted, [["partial"]], "admitted as a hybrid seat: pooled prompts")

            r = self.seat(gid, {"type": "bot"})
            self.assertEqual(r.status_code, 200, r.text)
            self.assertEqual(self.engine_seat(gid, 1)["controller"], "bot")
            self.assertEqual((self.engine_seat(gid, 1)["handicap"], self.engine_seat(gid, 1)["auto"]),
                             ("ai", {"un_vote": True, "conquest": True, "free_picks": True}),
                             "nothing of the hybrid seat stays behind")
            self.assertEqual((s.seats[1].type, s.seats[1].hybrid), ("bot", {}))
            self.assertEqual(len(admitted), 1, "a bot plays no model: nothing to admit")

            # her grant is gone: making it hybrid again is admitted again, and refused, leaving the bot as it was
            self.revoke()
            r = self.seat(gid, self.HYBRID)
            self.assertEqual(r.status_code, 403, r.text)
            self.assertEqual((s.seats[1].type, self.engine_seat(gid, 1)["controller"]), ("bot", "bot"))
            # a grant for pooled prompts again: hybrid once more (an LLM seat is refused her: no grant for games)
            self.regrant(["pooled"])
            r = self.seat(gid, {"type": "llm", "llm": LLM})
            self.assertEqual(r.status_code, 403, r.text)
            self.assertEqual((s.seats[1].type, self.engine_seat(gid, 1)["controller"]), ("bot", "bot"))
            r = self.seat(gid, {**self.HYBRID, "handicap": "ai", "hybrid": {"owners": "trade_only"}})
            self.assertEqual(r.status_code, 200, r.text)
            self.assertEqual((self.engine_seat(gid, 1)["controller"], self.engine_seat(gid, 1)["handicap"]),
                             ("hybrid", "ai"))
            self.assertEqual(s.seats[1].hybrid["preset"], "trade_only")
            self.assertEqual(admitted, [["partial"], ["partial"], ["full"], ["partial"]],
                             "every change that made it a model seat was admitted by its class")

    def test_settings_and_handicap_change_through_the_route_and_bad_ones_are_refused_with_the_reason(self):
        gid = self.game()
        s = self.manager.get(gid)
        self.assertEqual(self.seat(gid, self.HYBRID).status_code, 200)
        # a grid of its own: the United Nations and captured cities the model's, so the engine no longer decides them
        grid = {"un": "llm", "captured_cities": "llm", "chat": "llm"}
        r = self.seat(gid, {"hybrid": {"owners": grid, "advisor": False, "prompt_every": 7}})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.engine_seat(gid, 1)["auto"], expected_auto(expected_owners(grid)))
        self.assertEqual((s.seats[1].hybrid["owners"], s.seats[1].hybrid["advisor"], s.seats[1].hybrid.get("preset")),
                         (expected_owners(grid), False, None))
        self.assertEqual(self.engine_seat(gid, 1)["handicap"], "human", "the handicap stays what it was")
        r = self.seat(gid, {"handicap": "ai"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.engine_seat(gid, 1)["handicap"], "ai")
        before = (dict(s.seats[1].hybrid), self.engine_seat(gid, 1))
        for body, words in (({"hybrid": {"owners": {}}}, "nothing for its model"),
                            ({"hybrid": {"owners": {"bribes": "llm"}}}, "bribes"),
                            ({"hybrid": {"owners": "trade_only", "prompt_every": 0}}, "prompt_every"),
                            ({"hybrid": {"owners": "trade_only", "advisor": "yes"}}, "advisor"),
                            ({"handicap": "god"}, "handicap")):
            with self.subTest(body=body):
                r = self.seat(gid, body)
                self.assertEqual(r.status_code, 400, r.text)
                self.assertIn(words, r.json()["detail"])
                self.assertEqual((dict(s.seats[1].hybrid), self.engine_seat(gid, 1)), before, "nothing changed")
        r = self.seat(gid, {"type": "robot"})
        self.assertEqual(r.status_code, 400, r.text)
        self.assertIn("Unknown seat type", r.json()["detail"])

    def test_a_handicap_for_another_seat_is_its_own(self):
        gid = self.game()
        r = self.call("alice", "POST", f"/api/games/{gid}/seats/2", {"type": "bot", "handicap": "human"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.engine_seat(gid, 2)["handicap"], "human")
        self.assertEqual(self.manager.get(gid).seats[2].engine, {"handicap": "human"})

    def test_best_bot_is_pinned_when_a_seat_is_changed_as_when_it_is_made(self):
        gid = self.game()
        r = self.seat(gid, {**self.HYBRID, "bot": {"profile": "best"}})
        self.assertEqual(r.status_code, 200, r.text)
        bot = self.manager.get(gid).seats[1].bot
        self.assertNotEqual(bot.get("profile"), "best")
        self.assertEqual(bot.get("chosen_as"), "best")
        summary = self.call("alice", "GET", f"/api/games/{gid}/summary").json()
        row = next(p for p in summary["players"] if p["id"] == 1)
        self.assertEqual(row["label"], f"Hybrid: {bot['profile']} (best) + dry-run")
        # the stats screen labels it the same way (seats.js controllerLabel reads bot and bot_best)
        metrics = self.call("alice", "GET", f"/api/games/{gid}/metrics").json()["summary"]["1"]
        self.assertEqual((metrics["controller"], metrics["bot"], metrics["bot_best"]), ("hybrid", bot["profile"], True))

    def test_who_may_change_a_seat_to_hybrid(self):
        """P3.9.1 on the changed route: another member 404, an anonymous caller 401 or 404, an ACL viewer 403, a
        moderator on a published game 403, the owner and an administrator 200; a refusal changes nothing."""
        gid = self.game()
        s = self.manager.get(gid)
        with db.session() as ss:
            row = ss.get(GameRow, gid)
            access.grant(ss, row, ss.get(User, self.ids["rita"]), access.VIEW, actor=ss.get(User, self.ids["alice"]))
        for who, want in (("bob", (404,)), ("anon", (401, 404)), ("rita", (403,))):
            with self.subTest(who=who):
                r = self.seat(gid, self.HYBRID, who=who)
                self.assertIn(r.status_code, want, r.text)
                self.assertEqual(s.seats[1].type, "bot")
        self.assertEqual(self.call("rita", "GET", f"/api/games/{gid}").status_code, 200, "the viewer reads")
        with db.session() as ss:
            ss.get(GameRow, gid).visibility = "global"
        r = self.seat(gid, self.HYBRID, who="maggie")
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(s.seats[1].type, "bot")
        self.assertEqual(self.seat(gid, self.HYBRID, who="alice").status_code, 200)
        r = self.seat(gid, {**self.HYBRID, "llm": {"provider": "dryrun", "model": "dry-run"},
                            "hybrid": {"owners": "everything"}}, who="zara")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(s.seats[1].hybrid["preset"], "everything")

    def test_a_member_s_hybrid_seat_that_describes_a_machine_dials_nothing(self):
        gid = self.game()
        with NoOutbound() as watch:
            for llm in ({"base_url": "http://127.0.0.1:9/v1", "model": "x"},
                        {"provider": "anthropic", "api_key_env": "CITAR_TEST_SEAT_SECRET", "model": "x"}):
                with self.subTest(llm=llm):
                    r = self.seat(gid, {**self.HYBRID, "llm": llm})
                    self.assertEqual(r.status_code, 400, r.text)
                    self.assertIn("Pick a machine", r.json()["detail"])
        self.assertEqual(watch.calls, [])
        self.assertEqual(self.manager.get(gid).seats[1].type, "bot")


# ---------------------------------------------------------------------------- gate 2: scenarios

class Scenarios(World):
    SEAT = {"type": "hybrid", "bot": {"profile": "standard", "aggression": 0.6}, "handicap": "human",
            "hybrid": {"owners": "all_but_war", "advisor": False, "prompt_every": 6}}

    def editor(self, who="alice", players=None) -> str:
        r = self.call(who, "POST", "/api/scenario-editor", {
            "source": "generate", "config": {"map_size": "duel", "map_type": "pangaea", "city_states": 0,
                                             "barbarians": "off"},
            "players": players or [{"type": "human"}, {"type": "hybrid"}]})
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()["editor_id"]

    def save(self, eid, seats, name="Hybrid scenario", who="alice"):
        # a name of its own: the scenarios folder outlives each test's database, and a file without its row is an
        # administrator's
        return self.call(who, "POST", f"/api/scenario-editor/{eid}/save",
                         {"name": f"{name} {secrets.token_hex(3)}", "seats": seats})

    def launch(self, sid, seats, who="alice"):
        r = self.call(who, "POST", f"/api/scenarios/{sid}/launch", {"seats": seats})
        if r.status_code == 200:
            self.games.append(r.json()["id"])
            self.manager.get(r.json()["id"]).set_paused(True)
        return r

    def test_a_scenario_saved_with_a_hybrid_seat_launches_with_it(self):
        eid = self.editor()
        r = self.save(eid, [{"type": "human"}, dict(self.SEAT)])
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        saved = engine_api.load_scenario(sid)["seats"][1]
        owners = hybrid.owners_map("all_but_war")
        self.assertEqual((saved["type"], saved["hybrid"]["owners"], saved["hybrid"]["prompt_every"], saved["handicap"]),
                         ("hybrid", owners, 6, "human"))
        listed = self.call("alice", "GET", f"/api/scenarios/{sid}").json()["seats"][1]
        self.assertEqual((listed["type"], listed["hybrid"]["preset"]), ("hybrid", "all_but_war"))
        # from its defaults, the model's machine named at launch
        for seats in ([{}, {"llm": dict(LLM)}],
                      # the launch dialog's seats: the scenario's, as the dialog sends them back (scenarioSeat)
                      [{"type": "human"}, {**listed, "llm": dict(LLM), "hybrid": {"owners": "all_but_war",
                                                                                   "advisor": False,
                                                                                   "prompt_every": 6}}]):
            with self.subTest(seats=seats):
                r = self.launch(sid, seats)
                self.assertEqual(r.status_code, 200, r.text)
                gid = r.json()["id"]
                seat = self.manager.get(gid).seats[1]
                self.assertEqual((seat.type, seat.hybrid["owners"], seat.hybrid["advisor"], seat.hybrid["prompt_every"]),
                                 ("hybrid", owners, False, 6))
                self.assertEqual((seat.bot.get("profile"), seat.bot.get("aggression"), seat.llm["server_id"]),
                                 ("standard", 0.6, LLM["server_id"]))
                self.assertEqual(self.engine_seat(gid, 1)["controller"], "hybrid")
                self.assertEqual((self.engine_seat(gid, 1)["handicap"], self.engine_seat(gid, 1)["auto"]),
                                 ("human", expected_auto(owners)))
        # the dialog's other choices: a grid of its own and the AI's bonuses
        r = self.launch(sid, [{}, {"llm": dict(LLM), "handicap": "ai", "hybrid": {"owners": {"un": "llm"}}}])
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.assertEqual(self.engine_seat(gid, 1)["handicap"], "ai")
        self.assertEqual(self.engine_seat(gid, 1)["auto"]["un_vote"], False)

    def test_a_hybrid_seat_launched_as_a_bot_keeps_none_of_its_handicap(self):
        eid = self.editor()
        sid = self.save(eid, [{"type": "human"}, dict(self.SEAT)]).json()["id"]
        r = self.launch(sid, [{}, {"type": "bot"}])
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.assertEqual(self.engine_seat(gid, 1)["controller"], "bot")
        self.assertEqual((self.engine_seat(gid, 1)["handicap"], self.engine_seat(gid, 1)["auto"]),
                         ("ai", {"un_vote": True, "conquest": True, "free_picks": True}))
        self.assertEqual((self.manager.get(gid).seats[1].hybrid, self.manager.get(gid).seats[1].engine), ({}, {}))
        # a handicap the launch names is the bot's own
        r = self.launch(sid, [{}, {"type": "bot", "handicap": "human"}])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.engine_seat(r.json()["id"], 1)["handicap"], "human")

    def test_hybrid_settings_that_are_not_allowed_are_refused_when_saved(self):
        eid = self.editor()
        before = {s["id"] for s in engine_api.list_scenarios()}
        for seat, words in (({**self.SEAT, "hybrid": {"owners": {}}}, "nothing for its model"),
                            ({**self.SEAT, "hybrid": {"owners": "trade_only", "prompt_every": 99}}, "prompt_every"),
                            ({**self.SEAT, "hybrid": {"owners": {"bribes": "llm"}}}, "bribes"),
                            ({**self.SEAT, "handicap": "god"}, "god")):
            with self.subTest(seat=seat):
                r = self.save(eid, [{"type": "human"}, seat])
                self.assertEqual(r.status_code, 400, r.text)
                self.assertIn(words, r.json()["detail"])
                self.assertIn("Seat ", r.json()["detail"], "the seat is named")
        self.assertEqual({s["id"] for s in engine_api.list_scenarios()}, before, "nothing saved")

    def test_a_member_launching_a_hybrid_seat_names_a_machine_she_may_use(self):
        eid = self.editor()
        sid = self.save(eid, [{"type": "human"}, dict(self.SEAT)]).json()["id"]
        r = self.launch(sid, [{}, {}])
        self.assertEqual(r.status_code, 400, r.text)
        self.assertIn("Pick a machine", r.json()["detail"])
        self.revoke()
        r = self.launch(sid, [{}, {"llm": dict(LLM)}])
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self.games, [])

    def test_a_scenario_made_from_a_running_game_keeps_its_hybrid_seat(self):
        r = self.create("alice", [{"type": "human"}, {**FIXTURE["cases"][1]["payload"], "llm": dict(LLM)}])
        self.assertEqual(r.status_code, 200, r.text)
        opened = self.call("alice", "POST", "/api/scenario-editor", {"source": "game", "game_id": r.json()["id"]})
        self.assertEqual(opened.status_code, 200, opened.text)
        seat = opened.json()["meta"]["seats"][1]
        self.assertEqual((seat["type"], seat["hybrid"]["preset"], seat["hybrid"]["advisor"], seat["handicap"]),
                         ("hybrid", "all_but_war", False, "human"))
        self.assertNotIn("llm", seat, "the launch names the machine")
        sid = self.save(opened.json()["editor_id"], opened.json()["meta"]["seats"], name="From a game").json()["id"]
        r = self.launch(sid, [{}, {"llm": dict(LLM)}])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((self.engine_seat(r.json()["id"], 1)["controller"],
                          self.engine_seat(r.json()["id"], 1)["handicap"]), ("hybrid", "human"))


if __name__ == "__main__":
    unittest.main()
