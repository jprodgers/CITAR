"""Hybrid seats: the bot plays the turns, a model decides the diplomacy the seat hands it (design/phase3-features.md
P3.6.2-P3.6.7, P3.6.11; package 3-14's gates 1 to 6 and 8).

- ``Settings``: a seat's ``hybrid`` block, its presets and refusals, the engine's seat it makes, its tools.
- ``Categories`` (gate 1): for each of the ten categories, a seeded 4-seat small game of 60 rounds with one hybrid seat
  whose dry-run model owns only that category: the bot takes no action of it, and the model's actions in it happen; and
  with every category the bot's, the hybrid seat's game is the bot seat's, event for event.
- ``Fallbacks`` (gate 2), ``Veto`` (gate 3), ``Triggers`` (gate 4), ``Tools`` (gate 5), ``OneMachine`` (gate 6) and
  ``SeatSites`` (gate 8: every Python row of P3.6.7).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import re
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Optional
from unittest import mock

from citar import db, engine_api, servers
from citar.agents import hybrid, responders
from citar.agents.hybrid_agent import HybridAgent
from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.db.models import Base
from citar.pool import scheduler as S
from citar.server.session import SAVE_DIR, GameSession, SessionManager

_URL = "sqlite:///" + (Path(tempfile.mkdtemp(prefix="citar_hybrid_")) / "test.db").as_posix()

#: A dry-run model that answers at once.
DRY = {"provider": "dryrun", "dry_run_delay": 0}


def setUpModule():
    """This module's own database, with the test registry's machines (``sv_gpu``: one slot)."""
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()


def until(fn, timeout: float = 15.0, step: float = 0.01):
    """Poll ``fn`` until it returns something true (returned) or ``timeout`` passes (its last value)."""
    deadline = time.monotonic() + timeout
    while True:
        value = fn()
        if value or time.monotonic() > deadline:
            return value
        time.sleep(step)


def seat(owners, *, llm=None, **more) -> dict:
    """A hybrid seat on the dry run (or ``llm``), its model owning ``owners`` (a preset or a map)."""
    return {"type": "hybrid", "llm": dict(llm or DRY),
            "hybrid": {"owners": owners, "prompt_every": more.pop("prompt_every", 5), **more}}


def only(category: str) -> dict:
    return {category: "llm"}


class Calls:
    """Every call a hybrid seat's model made through the session (``GameSession.diplomat_call``): its tool, its
    arguments, whether it went through, and, for a negotiation, the chat and how many entries it had after it."""

    def __init__(self):
        self.calls: list = []
        self.lock = threading.Lock()
        real = GameSession.diplomat_call
        calls = self

        def spy(session, pid, name, args=None, *, chat=False):
            res = real(session, pid, name, args, chat=chat)
            entry = {"pid": pid, "tool": name, "args": dict(args or {}), "ok": bool(res.get("ok")), "chat": chat,
                     "error": res.get("error")}
            if res.get("ok") and name in ("open_negotiation", "respond_negotiation"):
                nid = (res.get("result") or {}).get("negotiation_id") or (args or {}).get("negotiation_id")
                try:
                    with session.lock:
                        entry["nid"] = int(nid)
                        entry["entries"] = len(session.game.negotiation(int(nid))["history"])
                except Exception:
                    pass
            with calls.lock:
                calls.calls.append(entry)
            return res
        self.patcher = mock.patch.object(GameSession, "diplomat_call", spy)

    def ok(self, *tools) -> list:
        return [c for c in self.calls if c["ok"] and (not tools or c["tool"] in tools)]

    def entries(self) -> set:
        """The (chat, entry number) of every entry the model made."""
        return {(c["nid"], c["entries"]) for c in self.calls if c.get("nid") is not None and c["ok"]}


class Case(unittest.TestCase):
    def setUp(self):
        S.reset()
        self.addCleanup(S.reset)
        responders.reset()
        self.addCleanup(responders.reset)
        self.manager = SessionManager()
        self.calls = Calls()
        self.calls.patcher.start()
        self.addCleanup(self.calls.patcher.stop)

    def tearDown(self):
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def game(self, seats: list, start: bool = False, **config) -> GameSession:
        cfg = {"map_size": "small", "seed": 11, "barbarians": "off", "city_states": 2, **config}
        return self.manager.create(cfg, seats, track=False, start=start)

    def play_out(self, s: GameSession, timeout: float = 180.0) -> None:
        """Start the game and wait until it is over (its turn limit)."""
        s.start()
        self.assertTrue(until(lambda: s.game.phase != "playing", timeout, 0.05),
                        f"turn {s.game.turn}; errors {[e.get('where') for e in s.errors]}")
        self.assertEqual([e for e in s.errors if e.get("where") != "chat clock"], [], s.errors[:1])


# ---------------------------------------------------------------------------- settings
class Settings(unittest.TestCase):
    def test_presets_and_maps(self):
        everything = hybrid.normalize_hybrid({"owners": "everything"})
        self.assertEqual(set(everything["owners"].values()), {"llm"})
        self.assertEqual(everything["preset"], "everything")
        war = hybrid.normalize_hybrid({"owners": "all_but_war"})["owners"]
        self.assertEqual([c for c, o in war.items() if o == "bot"], ["war"])
        trade = hybrid.normalize_hybrid({"owners": "trade_only"})
        self.assertEqual(hybrid.model_categories(trade["owners"]), ["trades", "chat"])
        mine = hybrid.normalize_hybrid({"owners": {"un": "llm", "chat": "llm"}, "prompt_every": 3, "advisor": False})
        self.assertEqual(mine["owners"], {c: ("llm" if c in ("un", "chat") else "bot") for c in hybrid.CATEGORIES})
        self.assertEqual((mine["prompt_every"], mine["advisor"], mine["phase_seconds"]), (3, False, 180.0))
        self.assertNotIn("preset", mine)
        self.assertEqual(hybrid.normalize_hybrid({})["owners"], hybrid.owners_map("trade_only"))
        self.assertFalse(hybrid.normalize_hybrid({"owners": "everything"}, benchmark=True)["advisor"],
                         "the advisor is always off in a benchmark game")

    def test_refusals_are_the_engines_and_the_designs(self):
        cases = [
            ({"owners": {"trades": "model"}}, "owned by 'bot' or 'llm'"),
            ({"owners": {"bribes": "llm"}}, "Unknown diplomacy category bribes"),
            ({"owners": "nothing_at_all"}, "Unknown preset"),
            ({"owners": {}}, "nothing for its model is a bot seat"),
            ({"owners": {"trades": "bot"}}, "nothing for its model is a bot seat"),
            ({"owners": "trade_only", "prompt_every": 0}, "prompt_every"),
            ({"owners": "trade_only", "prompt_every": 51}, "prompt_every"),
            ({"owners": "trade_only", "prompt_every": 2.5}, "prompt_every"),
            ({"owners": "trade_only", "advisor": "yes"}, "advisor"),
            ({"owners": "trade_only", "phase_seconds": 5}, "phase_seconds"),
            ("trade_only", "an object"),
        ]
        for block, words in cases:
            with self.subTest(block=block):
                with self.assertRaises(ValueError) as e:
                    hybrid.normalize_hybrid(block)
                self.assertIn(words, str(e.exception))
        self.assertEqual(hybrid.check_handicap(None), "ai")
        self.assertEqual(hybrid.check_handicap("human"), "human")
        with self.assertRaises(ValueError):
            hybrid.check_handicap("god")

    def test_the_engines_seat_and_the_tools_follow_the_owners(self):
        owners = hybrid.owners_map({"un": "llm", "captured_cities": "llm", "trades": "llm"})
        self.assertEqual(hybrid.engine_auto(owners), {"un_vote": False, "conquest": False, "free_picks": True})
        self.assertEqual(hybrid.engine_auto(hybrid.owners_map("trade_only")),
                         {"un_vote": True, "conquest": True, "free_picks": True})
        self.assertEqual(set(hybrid.diplomat_tools(owners)), set(hybrid.BASE_TOOLS) | {
            "un_vote", "get_cities", "city_status", "open_negotiation", "respond_negotiation"})
        self.assertEqual(set(hybrid.diplomat_tools(hybrid.owners_map({"war": "llm"}))),
                         set(hybrid.BASE_TOOLS) | {"declare_war"})
        # a chat its bot left to it is the model's to answer, whatever it owns
        self.assertIn("respond_negotiation", hybrid.chat_tools(hybrid.owners_map({"war": "llm"})))
        self.assertNotIn("send_message", hybrid.chat_tools(hybrid.owners_map({"trades": "llm"})))
        self.assertIn("send_message", hybrid.chat_tools(hybrid.owners_map({"chat": "llm"})))


class Creation(Case):
    """The engine's controller, handicap and automatic decisions at creation, seat update and scenario launch (P3.6.2);
    the seat saved with the session record."""

    def test_a_new_game_s_hybrid_seat(self):
        s = self.game([seat({"un": "llm", "trades": "llm"}), {"type": "bot"},
                       seat("everything") | {"handicap": "human", "difficulty": "King"}])
        p0, p2 = s.game.player(0), s.game.player(2)
        self.assertEqual((p0["controller"], p0["handicap"], p0["auto"]),
                         ("hybrid", "ai", {"un_vote": False, "conquest": True, "free_picks": True}))
        self.assertEqual((p2["controller"], p2["handicap"], p2["difficulty"]), ("hybrid", "human", "King"))
        self.assertEqual(p2["auto"], {"un_vote": False, "conquest": False, "free_picks": True})
        self.assertEqual(s.seats[0].hybrid["owners"]["un"], "llm")
        self.assertTrue(s.seats[0].model_seat and s.seats[0].has_bot)
        self.assertEqual((s.seats[0].task_class, s.seats[1].task_class), ("partial", None))
        self.assertEqual(s.info()["seats"][0]["hybrid"]["prompt_every"], 5)
        with self.assertRaises(ValueError) as e:
            self.game([{"type": "bot"}, {"type": "hybrid", "llm": dict(DRY), "hybrid": {"owners": {}}}])
        self.assertIn("Seat 2", str(e.exception))
        with self.assertRaises(ValueError):
            self.game([{"type": "hybrid", "llm": dict(DRY), "handicap": "god"}])

    def test_the_seat_survives_a_save_and_a_load(self):
        s = self.game([seat("all_but_war", prompt_every=7), {"type": "bot"}])
        path = s.save("hybrid-seat")
        gid = s.id
        self.manager.delete(gid)
        back = self.manager.load(path)
        self.assertEqual(back.seats[0].type, "hybrid")
        self.assertEqual(back.seats[0].hybrid["prompt_every"], 7)
        self.assertEqual(back.seats[0].hybrid["preset"], "all_but_war")

    def test_a_seat_changed_to_hybrid_and_back(self):
        s = self.game([{"type": "human"}, {"type": "bot"}])
        s.update_seat(1, type="hybrid", llm=dict(DRY), hybrid={"owners": {"captured_cities": "llm"}},
                      handicap="human")
        p = s.game.player(1)
        self.assertEqual((p["controller"], p["handicap"], p["auto"]),
                         ("hybrid", "human", {"un_vote": True, "conquest": False, "free_picks": True}))
        self.assertIsInstance(s.get_agent(1), HybridAgent)
        s.update_seat(1, hybrid={"owners": "everything"})       # its owners change; its handicap stays
        p = s.game.player(1)
        self.assertEqual((p["handicap"], p["auto"]), ("human", {"un_vote": False, "conquest": False,
                                                                "free_picks": True}))
        self.assertEqual(s.get_agent(1).owners, hybrid.owners_map("everything"), "a new agent for the new owners")
        s.update_seat(1, type="bot")
        p = s.game.player(1)
        self.assertEqual((s.seats[1].hybrid, p["controller"], p["handicap"], p["auto"]),
                         ({}, "bot", "ai", {"un_vote": True, "conquest": True, "free_picks": True}))

    def test_a_scenario_launches_its_hybrid_seat(self):
        g = engine_api.EngineGame.new({"map_size": "duel", "seed": 21, "barbarians": "off", "city_states": 0,
                                       "players": [{"controller": "human"}, {"controller": "bot"}]})
        seats = g.normalize_seats([{"type": "human"}, seat({"denounce": "llm"}, prompt_every=4)
                                   | {"handicap": "human"}])
        self.assertEqual(seats[1]["hybrid"]["owners"], {"denounce": "llm"})
        s = self.manager.create_from_scenario({"state": g.state_dict(), "seats": seats, "name": "S"}, start=False)
        p = s.game.player(1)
        self.assertEqual((s.seats[1].type, p["controller"], p["handicap"]), ("hybrid", "hybrid", "human"))
        self.assertEqual(s.seats[1].hybrid["owners"]["denounce"], "llm")
        self.assertEqual(s.seats[1].hybrid["prompt_every"], 4)


# ---------------------------------------------------------------------------- gate 1: the categories
def neighbours(x: int, y: int) -> list:
    """The six tiles around (x, y) on the odd-r hex grid."""
    if y % 2 == 0:
        steps = [(-1, -1), (0, -1), (-1, 0), (1, 0), (-1, 1), (0, 1)]
    else:
        steps = [(0, -1), (1, -1), (-1, 0), (1, 0), (0, 1), (1, 1)]
    return [(x + dx, y + dy) for dx, dy in steps]


def proposal_categories(proposal) -> set:
    """The categories a negotiation entry's proposal ({player: items}) touches."""
    return set(engine_api.proposal_categories(proposal)) if proposal else set()


ROUNDS = 60


class Categories(Case):
    """Gate 1. Seat 0 is the hybrid seat (its model the dry-run diplomat, called every 5 turns and on its triggers),
    seats 1 to 3 bots, every civilization met from the start, 60 rounds."""

    def _play(self, category: str, setup=None, until_over: bool = False) -> GameSession:
        s = self.game([seat(only(category)), {"type": "bot"}, {"type": "bot"}, {"type": "bot"}],
                      turn_limit=ROUNDS, espionage=True)
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": "all"}, {"op": "set_player", "player": "all", "gold": 600}])
            if setup is not None:
                setup(s)
        self.play_out(s)
        if not until_over:
            self.assertEqual(s.game.turn, ROUNDS + 1)
        fallbacks = [x for x in s.metrics.data["negotiations"] if x["player"] == 0 and x["outcome"] == "fallback"]
        self.assertEqual(fallbacks, [], "the dry-run model answered every chat itself")
        return s

    def _bot_actions(self, s: GameSession) -> dict:
        """The hybrid seat's bot's actions over the game, by tool (its turns' drives and its answers)."""
        out: dict = {}
        for rec in s.metrics.data["turns"]:
            if rec["player"] == 0:
                for tool, (taken, _refused) in (rec.get("bot_actions") or {}).items():
                    out[tool] = out.get(tool, 0) + taken
        return out

    def _bot_deals(self, s: GameSession) -> list:
        """What the hybrid seat's bot proposed or accepted: each entry the seat made in a chat that its model did not
        make, with the categories its proposal touches (an open's or a counter's own, an accept's the one on the
        table)."""
        mine = self.calls.entries()
        out = []
        for n in s.game.negotiations():
            table = None
            for i, e in enumerate(n["history"], 1):
                if e["by"] == 0 and (n["id"], i) not in mine:
                    if e["action"] in ("open", "counter"):
                        out.append((n["id"], e["action"], proposal_categories(e.get("proposal"))))
                    elif e["action"] == "accept":
                        out.append((n["id"], e["action"], proposal_categories(table)))
                if e.get("proposal"):
                    table = e["proposal"]
        return out

    def _model_deals(self, category: str) -> list:
        return [c for c in self.calls.ok("open_negotiation")
                if category in hybrid.items_categories(c["args"].get("give"), c["args"].get("receive"))]

    def _deal_category(self, category: str) -> GameSession:
        s = self._play(category)
        bad = [d for d in self._bot_deals(s) if category in d[2]]
        self.assertEqual(bad, [], f"the bot proposed or accepted {category}")
        self.assertTrue(self._model_deals(category), f"the model proposed {category}: {self.calls.calls[:6]}")
        return s

    def test_trades(self):
        self._deal_category("trades")

    def test_agreements(self):
        self._deal_category("agreements")

    def test_peace(self):
        def at_war(s):
            s.game.apply_ops([{"op": "set_relation", "a": 0, "b": 1, "state": "war"},
                              {"op": "set_relation", "a": 0, "b": 2, "state": "war"}])
        s = self._play("peace", at_war)
        bad = [d for d in self._bot_deals(s) if "peace" in d[2]]
        self.assertEqual(bad, [], "the bot proposed or accepted peace")
        self.assertTrue(self._model_deals("peace"), self.calls.calls[:6])

    def test_war(self):
        s = self._play("war")
        declared = [e for e in s.game.events() if e["type"] == "war_declared" and e["data"].get("attacker") == 0]
        model = self.calls.ok("declare_war")
        self.assertTrue(model, "the model declared war")
        self.assertEqual(len(declared), len(model), "every war the seat declared was its model's")
        self.assertEqual(self._bot_actions(s).get("declare_war", 0), 0)

    def test_denounce(self):
        s = self._play("denounce")
        self.assertTrue(self.calls.ok("denounce"), "the model denounced")
        self.assertEqual(self._bot_actions(s).get("denounce", 0), 0)

    @staticmethod
    def _united_nations(s):
        settler = next(u for u in s.game.execute(1, "get_units", {}) if str(u["type"]).lower() == "settler")
        s.game.apply_ops([{"op": "found_city", "player": 1, "x": settler["x"], "y": settler["y"],
                           "buildings": ["United Nations"], "capital": True}])

    @staticmethod
    def _tallies(s) -> list:
        return [sum(e["data"]["results"]["tally"].values()) for e in s.game.events()
                if e["type"] == "un_vote" and (e.get("data") or {}).get("results")]

    def test_un(self):
        s = self._play("un", self._united_nations)
        votes = self.calls.ok("un_vote")
        self.assertGreaterEqual(len(votes), 3, "the model voted (an abstention) at each vote, on turns 15, 30 and 45")
        voted = self._tallies(s)
        self.assertGreaterEqual(len(voted), 3)
        # the same game with a model that never votes: the engine casts nothing for the seat, so every count is the
        # same as with the model's abstentions
        from citar.agents.providers import dryrun
        real = dryrun.DryRunConversation._diplomacy_orders
        silent = mock.patch.object(dryrun.DryRunConversation, "_diplomacy_orders",
                                   lambda conv: [c for c in real(conv) if c.name != "un_vote"])
        with silent:
            quiet = self._play("un", self._united_nations)
        self.assertEqual(self._tallies(quiet), voted)
        # where the bot decides the vote, the engine votes for the seat: one vote more at the first count (the
        # model's only category here, espionage, gives it nothing to do)
        control = self._play("espionage", self._united_nations, until_over=True)
        self.assertEqual(self._tallies(control)[0], voted[0] + 1)

    def test_city_states(self):
        s = self._play("city_states")
        gifts = [c for c in self.calls.ok("city_state_action") if c["args"].get("action") == "gift_gold"]
        self.assertTrue(gifts, "the model gave a city-state gold")
        self.assertEqual(self._bot_actions(s).get("city_state_action", 0), 0)

    def test_espionage(self):
        def spy(s):
            s.game.test_ops([{"op": "add_spy", "player": 0}])
        s = self._play("espionage", spy)
        self.assertTrue(self.calls.ok("move_spy"), f"the model moved its spy: {self.calls.calls[:8]}")
        acts = self._bot_actions(s)
        self.assertEqual((acts.get("move_spy", 0), acts.get("stage_coup", 0)), (0, 0))

    def test_captured_cities(self):
        captured = {}

        def capture(s):
            g = s.game
            g.apply_ops([{"op": "set_relation", "a": 0, "b": 2, "state": "war"}])
            settler = next(u for u in g.execute(2, "get_units", {}) if str(u["type"]).lower() == "settler")
            city = g.apply_ops([{"op": "found_city", "player": 2, "x": settler["x"], "y": settler["y"],
                                 "name": "Target", "pop": 1}])[0]["city_id"]
            for x, y in neighbours(settler["x"], settler["y"]):
                try:
                    g.apply_ops([{"op": "add_unit", "player": 0, "unit": "Swordsman", "x": x, "y": y, "count": 12}])
                    break
                except engine_api.ActionError:
                    continue
            swords = [u["id"] for u in g.execute(0, "get_units", {}) if u["type"] == "Swordsman"]
            for uid in swords:
                g.test_ops([{"op": "attack_as", "unit": uid, "x": settler["x"], "y": settler["y"]}])
                c = next((c for c in g.execute(0, "get_cities", {}) if c["id"] == city), None)
                if c is not None:
                    captured.update(c)
                    break
        s = self._play("captured_cities", capture)
        self.assertTrue(captured.get("puppet"), f"captured as a puppet, not annexed by the engine: {captured}")
        annexed = [c for c in self.calls.ok("city_status") if c["args"] == {"city_id": captured["id"],
                                                                             "status": "annex"}]
        self.assertTrue(annexed, "the model annexed it")
        self.assertEqual(self._bot_actions(s).get("city_status", 0), 0)
        city = next(c for c in s.game.execute(0, "get_cities", {}) if c["id"] == captured["id"])
        self.assertFalse(city["puppet"])

    def test_chat(self):
        s = self._play("chat")
        self.assertTrue(self.calls.ok("send_message"), "the model talked")
        self.assertEqual(self._bot_actions(s).get("send_message", 0), 0)

    def test_with_every_category_the_bots_the_hybrid_seat_plays_the_bot_seats_game(self):
        """A test-only configuration: a hybrid seat whose owners are all the bot's (which ``normalize_hybrid`` refuses
        to anyone else) asks its model nothing, and plays the game a bot seat with the same handicap plays."""
        def run(first: dict) -> list:
            s = self.game([first, {"type": "bot"}, {"type": "bot"}, {"type": "bot"}], turn_limit=ROUNDS)
            if first["type"] == "hybrid":
                s.seats[0].hybrid = {**s.seats[0].hybrid, "owners": hybrid.owners_map({})}
                s.game.set_controller(0, "hybrid", "ai", {"un_vote": True, "conquest": True, "free_picks": True})
            with s.lock:
                s.game.apply_ops([{"op": "meet", "a": 0, "b": "all"}])
            self.play_out(s)
            if first["type"] == "hybrid":
                self.assertEqual(s.metrics.data.get("diplomat") or [], [], "no phase")
                self.assertEqual(s.game.player(0)["controller"], "hybrid")
            return [(e["turn"], e["type"], e["text"], e.get("players"), e.get("data")) for e in s.game.events()]
        hybrid_game = run(seat("trade_only"))
        bot_game = run({"type": "bot", "handicap": "ai"})
        self.assertGreater(len(bot_game), 500)
        diff = next((i for i, (a, b) in enumerate(zip(hybrid_game, bot_game)) if a != b), None)
        self.assertIsNone(diff, (diff, hybrid_game[diff] if diff is not None else None,
                                 bot_game[diff] if diff is not None else None))
        self.assertEqual(len(hybrid_game), len(bot_game))


# ---------------------------------------------------------------------------- a model the tests script
class Fake(Conversation):
    """A model for the tests. ``mode`` (on the class) decides what a chat's conversation does: ``reply`` answers the
    chat it is told of (``answer``: accept by default), ``mute`` says something and calls no tool, ``fail`` raises,
    ``hang`` waits until its conversation is closed. A diplomat phase follows ``phase`` (a callable given the
    conversation, returning its step) when set, else stops at once; an LLM seat's turn follows ``turn`` likewise, else
    ends its turn. Each conversation is recorded (``made``): its system prompt, its tools, whether it is a phase, and
    the chat it was told of."""
    lock = threading.Lock()
    made: list = []
    mode = "reply"
    answer = "accept"
    phase = None
    turn = None

    @classmethod
    def reset(cls):
        cls.made, cls.mode, cls.answer, cls.phase, cls.turn = [], "reply", "accept", None, None

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.system = system
        self.tools = {t["name"] for t in tools}
        self.usage = {"input_tokens": 10, "output_tokens": 2}
        self.texts: list = []
        self.results: list = []
        self.nid = None
        self.pending = False
        self.steps = 0
        self.closed = threading.Event()
        self.started = threading.Event()
        self.record = {"system": system, "tools": set(self.tools), "phase": False, "nid": None, "conv": self}
        with Fake.lock:
            Fake.made.append(self.record)

    @property
    def is_phase(self) -> bool:
        return self.record["phase"]

    def add_user_text(self, text):
        self.texts.append(text)
        if "DIPLOMAT PHASE" in text:
            self.record["phase"] = True
        found = re.search(r"negotiation #(\d+)", text)
        if found:
            self.pending = True
            if self.nid is None and not self.is_phase:
                self.nid = self.record["nid"] = int(found.group(1))

    def add_tool_results(self, results):
        self.results.append(results)

    def close(self):
        self.closed.set()

    def step(self):
        self.steps += 1
        self.started.set()
        if "end_turn" in self.tools:
            if Fake.turn is not None:
                return Fake.turn(self)
            return StepResult(text="done", tool_calls=[ToolCall(f"e{self.steps}", "end_turn", {})])
        if self.is_phase:
            if Fake.phase is not None:
                return Fake.phase(self)
            return StepResult(text="nothing to do")
        if Fake.mode == "fail":
            raise RuntimeError("the model is down")
        if Fake.mode == "hang":
            self.closed.wait(30)
            raise RuntimeError("cancelled")
        if Fake.mode == "mute" or not self.pending or self.nid is None:
            return StepResult(text="hmm")
        self.pending = False
        return StepResult(text="answer", tool_calls=[ToolCall("r", "respond_negotiation", {
            "negotiation_id": self.nid, "action": Fake.answer, "message": f"model: {Fake.answer}"})])


class Scripted(Case):
    """Cases whose models are ``Fake``."""

    def setUp(self):
        super().setUp()
        Fake.reset()
        for where in ("citar.agents.llm_agent.make_conversation", "citar.agents.hybrid_agent.make_conversation"):
            patcher = mock.patch(where, Fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    @staticmethod
    def on(machine: str, owners="trade_only", **more) -> dict:
        return seat(owners, llm={"server_id": machine, "model": "dry-run"}, **more)

    def met(self, s, gold: int = 600):
        with s.lock:
            n = len(s.seats)
            s.game.apply_ops([{"op": "meet", "a": a, "b": b} for a in range(n) for b in range(a + 1, n)]
                             + [{"op": "set_player", "player": "all", "gold": gold}])

    def offer(self, s, to: int = 1, give=None, receive=None) -> int:
        """The person (seat 0) offers seat ``to`` a trade: 50 gold for its world map, by default."""
        r = s.call_tool(0, "open_negotiation", {"to": to, "message": "A trade?",
                                                "give": give if give is not None else [{"type": "gold", "amount": 50}],
                                                "receive": receive if receive is not None else [{"type": "share_map"}]})
        self.assertTrue(r["ok"], r)
        return r["result"]["negotiation_id"]

    @staticmethod
    def settled(s, nid: int, pid: int = 1, timeout: float = 15.0) -> bool:
        def moved():
            with s.lock:
                n = s.game.negotiation_head(nid)
            return n["status"] != "open" or n["awaiting"] != pid
        return bool(until(moved, timeout))

    def round(self, s, act=None) -> int:
        """The person's turn (seat 0) once it is theirs: ``act()``, then the end of the turn; back once the round's
        other seats have played and it is the person's turn again. Returns the turn the person ended."""
        turn = s.game.turn
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn, 30), (s.game.turn, s.errors))
        if act is not None:
            act()
        self.assertTrue(until(lambda: s.call_tool(0, "end_turn", {})["ok"], 30), "the person's turn ended")
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn + 1, 60), (s.game.turn, s.errors))
        return turn

    @staticmethod
    def rows(s, nid: int) -> list:
        return [(x["outcome"], x["cause"]) for x in s.metrics.data["negotiations"] if x["nid"] == nid]

    def chats_made(self, nid: Optional[int] = None) -> list:
        return [m for m in Fake.made if not m["phase"] and "end_turn" not in m["tools"]
                and (nid is None or m["nid"] == nid)]


# ---------------------------------------------------------------------------- gate 2: fallbacks
class Fallbacks(Scripted):
    def test_a_chat_ticket_refused_while_a_benchmark_holds_the_machine(self):
        s = self.game([{"type": "human"}, self.on("sv_gpu")])
        self.met(s)
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="a benchmark")
        self.addCleanup(bench.release)
        t0 = time.perf_counter()
        nid = self.offer(s)
        self.assertTrue(self.settled(s, nid), "the fallback bot answered at once")
        self.assertLess(time.perf_counter() - t0, 5)
        self.assertEqual(self.rows(s, nid), [("fallback", "queued_behind_benchmark")])
        self.assertEqual(self.chats_made(nid), [], "the model was never asked")
        last = s.game.negotiation(nid)["history"][-1]
        self.assertEqual(last["by"], 1)

    def test_a_diplomat_phase_refused_while_a_benchmark_holds_the_machine(self):
        s = self.game([{"type": "human"}, self.on("sv_gpu", prompt_every=1)])
        self.met(s)
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", label="a benchmark")
        self.addCleanup(bench.release)
        s.start()
        for _ in range(3):
            self.round(s)
        rows = s.metrics.data.get("diplomat") or []
        self.assertTrue(rows)
        self.assertEqual({(r["outcome"], r["cause"]) for r in rows}, {("refused", "queued_behind_benchmark")})
        self.assertEqual([m for m in Fake.made if m["phase"]], [], "no phase's conversation was made")
        ended = [r["end_reason"] for r in s.metrics.data["turns"] if r["player"] == 1 and r["ended"]]
        self.assertEqual(set(ended), {"end_turn"}, "every turn of the seat ended")

    def test_a_chat_ticket_that_does_not_start_before_the_chat_s_deadline(self):
        s = self.game([{"type": "human"}, self.on("sv_gpu")], chat_model_reply_seconds=2)
        self.met(s)
        holder = S.get().lease("sv_gpu", "dry-run", kind="game", label="another game")
        self.addCleanup(holder.release)
        holder.activate()
        grant = holder.acquire()                   # its request holds the one slot
        self.addCleanup(lambda: holder.finish(grant))
        nid = self.offer(s)
        self.assertTrue(self.settled(s, nid, timeout=20), "the fallback bot answered once the ticket expired")
        self.assertEqual(self.rows(s, nid), [("fallback", "timeout")])
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["by"], 1)

    def test_a_model_that_fails(self):
        Fake.mode = "fail"
        s = self.game([{"type": "human"}, self.on("sv_dryrun")])
        self.met(s)
        nid = self.offer(s)
        self.assertTrue(self.settled(s, nid))
        self.assertEqual(self.rows(s, nid), [("fallback", "failed")])
        self.assertNotIn("(no response)", [e.get("message") for e in s.game.negotiation(nid)["history"]])

    def test_the_chat_clock_s_fallback_answers_in_its_tick(self):
        from tests import chatfake
        Fake.mode = "hang"
        s = self.game([{"type": "human"}, self.on("sv_dryrun")])
        self.met(s)
        clock = chatfake.use(s)
        nid = self.offer(s)
        self.assertTrue(until(lambda: self.chats_made(nid) and self.chats_made(nid)[0]["conv"].started.is_set(), 10))
        clock.advance(599)
        self.assertFalse(self.settled(s, nid, timeout=0.2), "still the model's at 599 s")
        clock.advance(1)
        self.assertTrue(self.settled(s, nid, timeout=0.5), "answered in the tick that found it due")
        self.assertIn(("fallback", "timeout"), self.rows(s, nid))
        self.assertTrue(self.chats_made(nid)[0]["conv"].closed.is_set(), "the model's answer was cancelled")
        time.sleep(0.2)
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["by"], 1)
        self.assertNotIn("(no response)", [e.get("message") for e in s.game.negotiation(nid)["history"]])

    def test_a_hundred_rounds_with_a_model_that_never_answers(self):
        Fake.mode = "mute"
        s = self.game([self.on("sv_dryrun", "everything", prompt_every=7), {"type": "bot"}, {"type": "bot"},
                       {"type": "bot"}], turn_limit=100)
        self.met(s)
        self.play_out(s, timeout=300)
        self.assertEqual(s.game.turn, 101)
        ends = [r["end_reason"] for r in s.metrics.data["turns"] if r["player"] == 0 and r["ended"]]
        self.assertEqual(len(ends), 100)
        self.assertEqual(set(ends), {"end_turn"})
        fallbacks = [x for x in s.metrics.data["negotiations"] if x["player"] == 0]
        self.assertTrue(fallbacks, "the bots' offers came to the model")
        self.assertEqual({(x["outcome"], x["cause"]) for x in fallbacks}, {("fallback", "failed")})
        self.assertTrue(all(r["outcome"] == "done" for r in s.metrics.data["diplomat"]))


# ---------------------------------------------------------------------------- gate 3: the veto
class Veto(Scripted):
    def setUp(self):
        super().setUp()
        s = self.s = self.game([{"type": "human"}, self.on("sv_dryrun", {"trades": "llm"}), {"type": "bot"}])
        self.met(s, gold=900)
        with s.lock:
            s.game.apply_ops([{"op": "grant_tech", "player": p, "tech": "Writing"} for p in range(3)])
            for p in range(3):          # embassies go to capitals
                settler = next(u for u in s.game.execute(p, "get_units", {}) if u["type"] == "Settler")
                s.game.apply_ops([{"op": "found_city", "player": p, "x": settler["x"], "y": settler["y"],
                                   "capital": True}])
        self.agent = s.get_agent(1)

    def value(self, give, receive, other=0) -> float:
        return self.s.game.bot_evaluate(1, self.agent.bot, other, give, receive)

    def test_the_models_mixed_offer_the_bot_values_below_zero_is_refused(self):
        s = self.s
        s.game.force_turn(1)
        give, receive = [{"type": "gold", "amount": 300}, {"type": "embassy"}], [{"type": "embassy"}]
        v = self.value(give, receive)
        self.assertLess(v, 0)
        r = s.diplomat_call(1, "open_negotiation", {"to": 0, "message": "Gold and an embassy?", "give": give,
                                                     "receive": receive})
        self.assertFalse(r["ok"])
        name = s.game.player_name(1)
        self.assertEqual(r["error"], f"The bot decides agreements and values this deal at {round(v, 1):g} gold for "
                                     f"you, so it will not agree. Leave those items out, or make the deal better for "
                                     f"{name}.")
        self.assertEqual(s.game.open_negotiations(1), [], "nothing was opened")
        refusals = s.metrics.data["diplomat_refusals"]
        self.assertEqual([(x["tool"], x["why"]) for x in refusals], [("open_negotiation", "veto")])
        # valued above zero, it goes through
        give, receive = [{"type": "embassy"}], [{"type": "embassy"}, {"type": "gold", "amount": 100}]
        self.assertGreater(self.value(give, receive), 0)
        r = s.diplomat_call(1, "open_negotiation", {"to": 0, "message": "An embassy and gold?", "give": give,
                                                     "receive": receive})
        self.assertTrue(r["ok"], r)

    def test_a_deal_wholly_in_the_models_categories_is_never_vetoed(self):
        s = self.s
        s.game.force_turn(1)
        give = [{"type": "gold", "amount": 400}]
        self.assertLess(self.value(give, []), 0)
        r = s.diplomat_call(1, "open_negotiation", {"to": 2, "message": "A gift.", "give": give, "receive": []})
        self.assertTrue(r["ok"], r)
        self.assertNotIn("diplomat_refusals", s.metrics.data)

    def test_accepting_and_countering_a_mixed_deal(self):
        s = self.s
        give, receive = [{"type": "embassy"}, {"type": "gold", "amount": 300}], [{"type": "embassy"}]
        nid = self.offer(s, give=receive, receive=give)     # the person asks for gold and an embassy for an embassy
        with s.lock:
            self.assertEqual(s.game.negotiation_head(nid)["awaiting"], 1)
        responders.halt(s, 1)                               # the test answers for the seat's model
        r = s.diplomat_call(1, "respond_negotiation", {"negotiation_id": nid, "action": "accept", "message": "Yes."},
                            chat=True)
        self.assertFalse(r["ok"])
        self.assertIn("The bot decides agreements", r["error"])
        r = s.diplomat_call(1, "respond_negotiation", {
            "negotiation_id": nid, "action": "counter", "message": "This instead.",
            "give": [{"type": "gold", "amount": 300}, {"type": "embassy"}], "receive": [{"type": "embassy"}]},
            chat=True)
        self.assertFalse(r["ok"], "a counter the bot values below zero is refused too")
        r = s.diplomat_call(1, "respond_negotiation", {
            "negotiation_id": nid, "action": "counter", "message": "This instead.",
            "give": [{"type": "embassy"}], "receive": [{"type": "embassy"}, {"type": "gold", "amount": 50}]},
            chat=True)
        self.assertTrue(r["ok"], r)
        self.assertEqual([x["why"] for x in s.metrics.data["diplomat_refusals"]], ["veto", "veto"])


# ---------------------------------------------------------------------------- gate 5: the tools and the prompt
class Tools(Scripted):
    def test_a_call_outside_the_models_categories_is_refused_and_counted(self):
        s = self.game([{"type": "human"}, self.on("sv_dryrun", {"trades": "llm"})])
        self.met(s)
        s.get_agent(1)
        s.game.force_turn(1)
        for name, args in (("declare_war", {"player_id": 0}), ("un_vote", {"candidate": "abstain"}),
                           ("move_unit", {"unit_id": 1, "x": 1, "y": 1}), ("end_turn", {})):
            r = s.diplomat_call(1, name, args)
            self.assertEqual(r, {"ok": False, "error": hybrid.NOT_YOURS}, name)
        r = s.diplomat_call(1, "send_message", {"to": 0, "text": "hi"}, chat=True)
        self.assertEqual(r["error"], hybrid.NOT_YOURS, "talk is not this model's either")
        self.assertTrue(s.diplomat_call(1, "get_diplomacy", {})["ok"])
        self.assertEqual(s.game.player(0)["alive"], True)
        summary = s.metrics_report()["summary"][1]
        self.assertEqual(summary["diplomat_refused"], {"category": 5})
        self.assertEqual(len(s.metrics.data["diplomat_refusals"]), 5)

    def test_the_system_prompt_is_the_same_for_every_call_and_names_both_timeouts(self):
        calls = []

        def phase(conv):
            calls.append(conv.steps)
            if conv.steps == 1:
                return StepResult(text="looking", tool_calls=[ToolCall("x", "declare_war", {"player_id": 0}),
                                                              ToolCall("y", "get_diplomacy", {})])
            return StepResult(text="done")
        Fake.phase = phase
        s = self.game([{"type": "human"}, self.on("sv_dryrun", "trade_only", prompt_every=1)],
                      chat_model_reply_seconds=90, chat_person_reply_seconds=240)
        self.met(s)
        s.start()
        for _ in range(3):
            self.round(s, lambda: self.assertTrue(self.settled(s, self.offer(s))))
        systems = {m["system"] for m in Fake.made}
        self.assertEqual(len(systems), 1, "one system prompt for the seat's phases and chats")
        self.assertGreaterEqual(len([m for m in Fake.made if m["phase"]]), 2)
        self.assertGreaterEqual(len(self.chats_made()), 3)
        text = systems.pop()
        self.assertIn("You have 1.5 minutes to answer a chat that waits on you; the other side has 4 (a person) or "
                      "1.5 (another AI)", text)
        self.assertIn("at 30 messages", text)
        self.assertIn("- trades: gold", text)
        self.assertIn("- talk: messages", text)
        self.assertIn("The bot decides agreements, peace, war, denounce, un, city states, espionage, captured cities.",
                      text)
        refused = [r for r in s.metrics.data["diplomat_refusals"] if r["tool"] == "declare_war"]
        self.assertEqual(len(refused), len([m for m in Fake.made if m["phase"]]))
        rows = s.metrics.data["diplomat"]
        self.assertTrue(all({"tool": "declare_war", "ok": False} in r["tools"] for r in rows), rows)


# ---------------------------------------------------------------------------- gate 6: one machine, two kinds of seat
class OneMachine(Scripted):
    def test_an_llm_seat_s_trade_is_answered_by_the_hybrid_seat_s_model_on_their_one_slot(self):
        opened = []

        def turn(conv):
            if conv.steps == 1:
                return StepResult(text="trade", tool_calls=[ToolCall("o", "open_negotiation", {
                    "to": 1, "message": "Maps for gold?", "give": [{"type": "gold", "amount": 40}],
                    "receive": [{"type": "share_map"}]})])
            if not opened:
                opened.extend(conv.results[-1:])
            return StepResult(text="done", tool_calls=[ToolCall(f"e{conv.steps}", "end_turn", {})])
        Fake.turn = turn
        s = self.game([{"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run"}}, self.on("sv_gpu")],
                      map_size="duel")
        self.met(s)
        s.start()
        self.assertTrue(until(lambda: any(n["initiator"] == 0 for n in s.game.negotiations()), 30), s.errors)
        nid = next(n["id"] for n in s.game.negotiations() if n["initiator"] == 0)
        self.assertTrue(until(lambda: s.game.negotiation(nid)["status"] == "accepted", 30),
                        (s.game.negotiation(nid), s.errors))
        self.assertEqual(self.rows(s, nid), [("answered", None)], "the model's answer, no fallback")
        self.assertEqual(len(self.chats_made(nid)), 1)
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["message"], "model: accept")

    def test_a_chat_back_to_the_seat_after_its_phase_is_its_models_and_its_turn_waits_for_it(self):
        """P3.6.3's last step: the drive after the phase stops for the chat the phase opened; the person counters, the
        chat comes back to the seat on its own turn, its bot leaves it to the model, which answers it on a chat ticket
        of its own, and only then does the seat's turn end."""
        def phase(conv):
            if conv.steps == 1:
                return StepResult(text="trade", tool_calls=[ToolCall("o", "open_negotiation", {
                    "to": 0, "message": "Maps for maps?", "give": [{"type": "share_map"}],
                    "receive": [{"type": "share_map"}]})])
            return StepResult(text="done")
        Fake.phase = phase
        s = self.game([{"type": "human"}, self.on("sv_dryrun", prompt_every=1)])
        self.met(s)
        s.start()
        turn = self.round(s)
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn + 1, 30))
        self.assertTrue(s.call_tool(0, "end_turn", {})["ok"])            # the seat's phase is due on this turn
        self.assertTrue(until(lambda: any(n["initiator"] == 1 for n in s.game.negotiations()), 30), s.errors)
        nid = next(n["id"] for n in s.game.negotiations() if n["initiator"] == 1)
        self.assertTrue(until(lambda: any(r["turn"] == turn + 1 and r["outcome"] == "done"
                                          for r in s.metrics.data.get("diplomat") or []), 30), "the phase ended")
        self.assertEqual((s.game.current, s.game.turn), (1, turn + 1), "the seat's turn waits for the chat")
        r = s.call_tool(0, "respond_negotiation", {"negotiation_id": nid, "action": "counter",
                                                   "message": "Your map and 20 gold.",
                                                   "give": [{"type": "share_map"}],
                                                   "receive": [{"type": "share_map"}, {"type": "gold", "amount": 20}]})
        self.assertTrue(r["ok"], r)
        self.assertTrue(until(lambda: s.game.negotiation(nid)["status"] == "accepted", 30), s.game.negotiation(nid))
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["message"], "model: accept")
        self.assertEqual(len(self.chats_made(nid)), 1, "its model's own conversation for the chat")
        self.assertEqual(self.rows(s, nid), [("answered", None)])
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn + 2, 30), "then the turn ended")

    def test_a_chat_back_to_the_seat_during_its_phase_is_the_phase_s(self):
        opened, countered = [], threading.Event()

        def phase(conv):
            if conv.steps == 1:
                return StepResult(text="trade", tool_calls=[ToolCall("o", "open_negotiation", {
                    "to": 0, "message": "Maps for maps?", "give": [{"type": "share_map"}],
                    "receive": [{"type": "share_map"}]})])
            if conv.steps == 2:
                opened.append(conv.results[-1])
                countered.wait(15)
                return StepResult(text="look", tool_calls=[ToolCall("d", "get_diplomacy", {})])
            told = [t for t in conv.texts if t.startswith("CHAT:") or "\nCHAT:" in t]
            nid = int(re.search(r"negotiation #(\d+)", told[-1]).group(1)) if told else None
            if conv.steps == 3 and nid is not None:
                return StepResult(text="accept", tool_calls=[ToolCall("a", "respond_negotiation", {
                    "negotiation_id": nid, "action": "accept", "message": "phase: accept"})])
            return StepResult(text="done")
        Fake.phase = phase
        s = self.game([{"type": "human"}, self.on("sv_dryrun", prompt_every=1)])
        self.met(s)
        s.start()
        turn = s.game.turn
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn, 30))
        self.assertTrue(s.call_tool(0, "end_turn", {})["ok"])
        self.assertTrue(until(lambda: s.game.current == 0 and s.game.turn == turn + 1, 30))
        self.assertTrue(s.call_tool(0, "end_turn", {})["ok"])     # turn 2: the seat's phase is due
        self.assertTrue(until(lambda: any(n["initiator"] == 1 for n in s.game.negotiations()), 30), s.errors)
        nid = next(n["id"] for n in s.game.negotiations() if n["initiator"] == 1)
        r = s.call_tool(0, "respond_negotiation", {"negotiation_id": nid, "action": "counter",
                                                   "message": "Your map and 20 gold.",
                                                   "give": [{"type": "share_map"}],
                                                   "receive": [{"type": "share_map"}, {"type": "gold", "amount": 20}]})
        self.assertTrue(r["ok"], r)
        # the chat came back to the seat while its phase holds its voice: given to the phase, not to an answer of its
        # own (the phase is still in its second step, waiting for this)
        self.assertTrue(until(lambda: nid in getattr(s.agents.get(1), "_held", ()), 10), "the chat is the phase's")
        countered.set()
        self.assertTrue(until(lambda: s.game.negotiation(nid)["status"] == "accepted", 30), s.game.negotiation(nid))
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["message"], "phase: accept")
        self.assertEqual(self.chats_made(nid), [], "no chat conversation of its own: the phase answered it")
        self.assertEqual(self.rows(s, nid), [], "no chat answer, no fallback")
        row = next(r for r in s.metrics.data["diplomat"] if r["turn"] == turn + 1)
        self.assertIn({"tool": "respond_negotiation", "ok": True}, row["tools"])


# ---------------------------------------------------------------------------- gate 4: the triggers
class Triggers(Scripted):
    """Gate 4: each trigger of P3.6.4 fires one phase. A person (seat 0) plays through the API, and on its turn does
    what makes a trigger; the hybrid seat (1, every category but war its model's, ``prompt_every`` 50) plays next, and
    its phase that turn names the trigger. A phase never runs twice in a turn."""

    def setUp(self):
        super().setUp()
        s = self.s = self.game([{"type": "human"}, self.on("sv_dryrun", "all_but_war", prompt_every=50),
                                {"type": "bot"}, {"type": "bot"}], espionage=True)
        self.target = {}
        with s.lock:
            g = s.game
            g.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": "all", "gold": 900}])
            home = self.home = next(u for u in g.execute(0, "get_units", {}) if u["type"] == "Settler")
            g.apply_ops([{"op": "found_city", "player": 0, "x": home["x"], "y": home["y"], "capital": True,
                          "buildings": ["United Nations"]}])
            theirs = next(u for u in g.execute(2, "get_units", {}) if u["type"] == "Settler")
            city = g.apply_ops([{"op": "found_city", "player": 2, "x": theirs["x"], "y": theirs["y"],
                                 "name": "Target"}])[0]["city_id"]
            self.target = {"id": city, "x": theirs["x"], "y": theirs["y"]}
        s.start()

    def phases(self, turn: int) -> list:
        return [r for r in self.s.metrics.data.get("diplomat") or [] if r["turn"] == turn and r["player"] == 1]

    def fired(self, turn: int, words: str) -> list:
        """The hybrid seat's phase of ``turn``, which must exist, be the turn's only one, and name ``words``."""
        rows = self.phases(turn)
        self.assertEqual(len(rows), 1, f"one phase on turn {turn}: {rows}")
        self.assertTrue(any(words in t for t in rows[0]["triggers"]), (words, rows[0]["triggers"]))
        return rows[0]["triggers"]

    def test_each_trigger_fires_one_phase(self):
        s, g = self.s, self.s.game
        name = g.player_name
        t = self.round(s)
        self.assertEqual(self.phases(t), [], "nothing has happened yet")
        # first contact with a major civilization
        t = self.round(s, lambda: g.apply_ops([{"op": "meet", "a": 1, "b": 2}]))
        self.fired(t, f"first contact with {name(2)}")
        # war declared on the seat
        t = self.round(s, lambda: self.assertTrue(s.call_tool(0, "declare_war", {"player_id": 1})["ok"]))
        self.fired(t, f"{name(0)} declared war on you")

        # peace made (the person's peace, which the seat's model accepts)
        def peace():
            nid = self.offer(s, give=[{"type": "peace_treaty"}], receive=[{"type": "peace_treaty"}])
            self.assertTrue(until(lambda: g.negotiation(nid)["status"] == "accepted", 15), g.negotiation(nid))
        t = self.round(s, peace)
        self.fired(t, f"peace was made with {name(0)}")
        # the seat denounced
        t = self.round(s, lambda: self.assertTrue(s.call_tool(0, "denounce", {"player_id": 1})["ok"]))
        self.fired(t, f"{name(0)} denounced you")

        # a deal of the seat's that ends next turn, then expires
        def deal():
            nid = self.offer(s, give=[{"type": "gold_per_turn", "amount": 1, "turns": 2}], receive=[])
            self.assertTrue(until(lambda: g.negotiation(nid)["status"] == "accepted", 15), g.negotiation(nid))
        dealt = self.round(s, deal)
        seen = {}
        for _ in range(4):
            t = self.round(s)
            for row in self.phases(t):
                for trig in row["triggers"]:
                    seen.setdefault(trig, t)
        ending = [t for trig, t in seen.items() if "ends next turn" in trig and name(0) in trig]
        expired = [t for trig, t in seen.items() if trig == f"a deal with {name(0)} expired"]
        self.assertTrue(ending and expired, (dealt, seen))
        self.assertLess(ending[0], expired[0])

        # a deal of the seat's cut short: the person's cotton, traded for ten turns, gone
        cotton = {}

        def trade():
            g.apply_ops([{"op": "grant_tech", "player": 0, "tech": "Calendar"}])
            for x, y in neighbours(self.home["x"], self.home["y"]):
                try:
                    g.apply_ops([{"op": "set_tile", "x": x, "y": y, "resource": "Cotton", "improvement": "Plantation"}])
                except engine_api.ActionError:
                    continue
                cotton.update(x=x, y=y)
                break
            nid = self.offer(s, give=[{"type": "resource", "resource": "Cotton", "amount": 1, "turns": 10}],
                             receive=[])
            self.assertTrue(until(lambda: g.negotiation(nid)["status"] == "accepted", 15), g.negotiation(nid))
        self.round(s, trade)
        cut = self.round(s, lambda: g.apply_ops([{"op": "set_tile", "x": cotton["x"], "y": cotton["y"],
                                                  "resource": None}]))
        self.round(s)                       # the round's end cuts it: the seat's next look sees it
        rows = self.phases(cut) + self.phases(cut + 1)
        self.assertTrue(any(f"a deal with {name(0)} was cut" in x for r in rows for x in r["triggers"]), rows)
        # a city-state event about the seat (city-states are its model's)
        cs = next(p["id"] for p in g.summary()["players"] if p["kind"] != "major")
        t = self.round(s, lambda: g.apply_ops([{"op": "meet", "a": 1, "b": cs}]))
        self.fired(t, "city-states: ")
        # a spy (espionage is its model's)
        t = self.round(s, lambda: g.test_ops([{"op": "add_spy", "player": 1}]))
        self.fired(t, "espionage: ")

        # a city the seat captured (captured cities are its model's: it stays a puppet until the model decides)
        def capture():
            g.apply_ops([{"op": "set_relation", "a": 1, "b": 2, "state": "war"}])
            for x, y in neighbours(self.target["x"], self.target["y"]):
                try:
                    g.apply_ops([{"op": "add_unit", "player": 1, "unit": "Swordsman", "x": x, "y": y, "count": 12}])
                    break
                except engine_api.ActionError:
                    continue
            for u in [u for u in g.execute(1, "get_units", {}) if u["type"] == "Swordsman"]:
                # it is the person's turn: the seat's new units are given their moves
                g.test_ops([{"op": "set_unit", "unit": u["id"], "moves": 2},
                            {"op": "attack_as", "unit": u["id"], "x": self.target["x"], "y": self.target["y"]}])
                if any(c["id"] == self.target["id"] for c in g.execute(1, "get_cities", {})):
                    return
            self.fail("the city was not captured")
        t = self.round(s, capture)
        self.fired(t, "you captured")
        city = next(c for c in g.execute(1, "get_cities", {}) if c["id"] == self.target["id"])
        self.assertTrue(city["puppet"], "the engine did not annex it")
        # the seat's score changes by more than a quarter
        t = self.round(s, lambda: g.apply_ops([{"op": "grant_era", "player": 1, "era": "Medieval era"}]))
        self.fired(t, "your score went from")

        # its strength against a civilization it has met changes by more than a quarter (the turn's statistics are
        # the round's: the next turn's look sees it)
        def army():
            at = next(u for u in g.execute(1, "get_units", {}) if u["type"] == "Swordsman")
            g.apply_ops([{"op": "add_unit", "player": 1, "unit": "Swordsman", "x": at["x"], "y": at["y"],
                          "count": 30}])
        t = self.round(s, army)
        strength = [r for turn in (t, t + 1) for r in self.phases(turn)
                    if any(x.startswith("your strength against") for x in r["triggers"])]
        if not strength:
            t = self.round(s)
            strength = [r for r in self.phases(t) if any(x.startswith("your strength against") for x in r["triggers"])]
        self.assertTrue(strength, s.metrics.data["diplomat"][-3:])

        # two triggers in one turn: one phase, naming both
        other = [p["id"] for p in g.summary()["players"] if p["kind"] != "major"][1]

        def two():
            g.apply_ops([{"op": "meet", "a": 1, "b": other}])
            g.test_ops([{"op": "add_spy", "player": 1}])
        t = self.round(s, two)
        both = self.fired(t, "city-states: ")
        self.assertTrue(any(x.startswith("espionage: ") for x in both), both)
        # the United Nations' vote opens on turn 15, the turn before the vote (its builder, the person, founded it on
        # turn 1): that turn's phase names it, whatever else it names
        while g.turn <= 15:
            self.round(s)
        self.fired(15, "the United Nations vote is open")
        # never two phases in a turn
        turns = [r["turn"] for r in s.metrics.data["diplomat"]]
        self.assertEqual(len(turns), len(set(turns)), turns)


class Schedule(Scripted):
    def test_prompt_every_fires_on_schedule(self):
        s = self.game([{"type": "human"}, self.on("sv_dryrun", {"un": "llm"}, prompt_every=3), {"type": "bot"}])
        self.met(s)
        s.start()
        for _ in range(13):
            self.round(s)
        rows = [r for r in s.metrics.data["diplomat"] if r["player"] == 1]
        last = 1                                    # the seat's first turn: the schedule counts from it
        scheduled = []
        for r in rows:
            due = any(t.startswith("it is 3 turns since the last call") for t in r["triggers"])
            self.assertEqual(due, r["turn"] - last >= 3, (r["turn"], last, r["triggers"]))
            if due:
                scheduled.append(r["turn"])
            last = r["turn"]
        self.assertGreaterEqual(len(scheduled), 3, rows)
        self.assertIn(4, [r["turn"] for r in rows], "the first, three turns after the seat's first")


# ---------------------------------------------------------------------------- gate 8: every place that asks "is this an LLM seat?"
class SeatSites(Scripted):
    """Each Python row of P3.6.7, as the hybrid seat is to be seen there."""

    def test_the_seat_s_public_shape_its_agent_and_its_turns_metrics(self):
        s = self.game([{"type": "human"}, self.on("sv_dryrun", "trade_only")])
        self.met(s)
        info = s.info()["seats"][1]
        self.assertEqual((info["type"], info["llm_info"]["server_id"]), ("hybrid", "sv_dryrun"))
        self.assertEqual(info["hybrid"]["preset"], "trade_only")
        self.assertIsInstance(s.get_agent(1), HybridAgent)            # get_agent
        s.start()
        for _ in range(3):
            self.round(s)
        recs = [r for r in s.metrics.data["turns"] if r["player"] == 1 and r["ended"]]
        self.assertTrue(recs)
        self.assertEqual({(r["controller"], r["model"]) for r in recs}, {("hybrid", "dry-run")})    # _track_turn
        self.assertTrue(any(r.get("bot_actions") for r in recs), "its turns are its bot's drives")   # drive_bots
        row = s.metrics_report()["summary"][1]                                                       # metrics_report
        self.assertEqual((row["controller"], row["model"], row["hybrid"], row["bot"]),
                         ("hybrid", "dry-run", True, "standard"))
        self.assertEqual(row["owners"]["trades"], "llm")

    def test_its_chats_go_to_its_bot_first_on_the_bot_pool(self):
        """``_dispatch_negotiation_interrupts``: a chat its bot owns is answered by the bot, on the bot pool, and its
        model is never asked."""
        seen = []
        real = HybridAgent.respond_negotiation

        def spy(agent, session, pid, nid, kind="bot", epoch=None):
            seen.append((kind, threading.current_thread().name.rsplit("-", 1)[0]))
            return real(agent, session, pid, nid, kind=kind, epoch=epoch)
        s = self.game([{"type": "human"}, self.on("sv_dryrun", {"denounce": "llm"})])
        self.met(s)
        with mock.patch.object(HybridAgent, "respond_negotiation", spy):
            nid = self.offer(s)
            self.assertTrue(self.settled(s, nid))
        self.assertEqual(seen, [("bot", "bot-responder")])
        self.assertEqual(self.chats_made(), [])
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["by"], 1)

    def test_occupancy_quiet_hours_and_making_way_are_full_seats_business(self):
        """``occupied`` lists leases only, and a hybrid seat holds none; quiet hours and making way pause a game for
        its full seats only, and a hybrid seat's prompt in its machine's quiet hours is refused (the fallback bot
        answers)."""
        from datetime import datetime, timedelta
        from citar.pool import seats as pool_seats
        s = self.game([{"type": "human"}, self.on("sv_dryrun"), {"type": "bot"}])
        s.registered = True
        self.met(s)
        s.start()
        self.assertTrue(until(lambda: s.game.current == 0, 10))
        self.assertEqual(pool_seats.occupied("sv_dryrun"), [], "a hybrid seat holds no lease")
        self.assertEqual(s.leases, {})
        self.assertIsNone(s.queue_machine())
        later = datetime.now() + timedelta(hours=2)
        with mock.patch("citar.servers.restricted_now", return_value=later):
            self.assertFalse(s._quiet_hours(1, s.seats[1]), "the game is not paused for a hybrid seat's machine")
        self.assertFalse(s._make_way(1, s.seats[1]))
        with mock.patch.object(S.Machine, "_quiet", lambda m: later):
            nid = self.offer(s)
            self.assertTrue(self.settled(s, nid))
        self.assertEqual(self.rows(s, nid), [("fallback", "refused")])
        # a full seat on the same machine is listed
        other = self.game([{"type": "llm", "llm": {"server_id": "sv_dryrun", "model": "dry-run"}}, {"type": "human"}])
        other.start()
        self.assertTrue(until(lambda: pool_seats.occupied("sv_dryrun"), 10))

    def test_usage_lists_every_model_seat_with_its_class_and_tracks_a_hybrid_game(self):
        from citar import usage
        rows = []
        with mock.patch.object(usage.Tracker, "activity",
                               lambda t, act, kind, name="", parent=None, ref=None, **fields: rows.append(fields)):
            s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                    [{"type": "human"}, self.on("sv_dryrun"),
                                     {"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run"}}],
                                    start=False)
        self.assertTrue(s.usage_act, "a game whose model seat is hybrid is tracked")
        seats = rows[0]["seats"]
        self.assertEqual(seats[1], {"player": 1, "type": "hybrid", "server_id": "sv_dryrun", "model": "dry-run",
                                    "class": "partial"})
        self.assertEqual(seats[2]["class"], "full")
        self.assertEqual((seats[0]["class"], seats[0]["server_id"]), (None, None))
        self.assertEqual(usage.session_servers(s).get("sv_dryrun"), None, "a hybrid seat holds its machine never")

    def test_scoring_leaves_hybrid_seats_out_of_the_model_ranking(self):
        from citar.server import scoring
        m = {"provider": "mock", "model": "m-1"}
        s = self.game([{"type": "llm", "llm": dict(m)}, {"type": "hybrid", "llm": dict(m),
                                                         "hybrid": {"owners": "trade_only"}}, {"type": "human"}])
        for pid, controller in ((0, "llm"), (1, "hybrid")):
            for turn in (1, 2, 3):
                s.metrics.begin_turn(pid, turn, controller, "m-1")
                s.metrics.end_turn(pid, "end_turn")
        reports = scoring._reports(self.manager)
        self.assertEqual(sorted(reports[s.id]["summary"]), [0, 2])
        rows = {r["model"]: r for r in scoring.compare_models(self.manager)}
        self.assertEqual((rows["m-1"]["turns"], rows["m-1"]["games"]), (3, 1), "the LLM seat's turns only")
        # a save's digest the same
        path = s.save("scored")
        digest = scoring._digest_save(path)
        self.assertNotIn(1, digest["summary"])
        self.assertIn(0, digest["summary"])

    def test_reports_and_probes_read_model_seats(self):
        from citar.probes import ProbeRunner
        from citar.reports import data as report_data
        s = self.game([{"type": "human"}, self.on("sv_dryrun")])
        with s.lock:
            s.game.add_thought(1, "the diplomat's thought", "diplomacy")
        seats = report_data._details_of(s.id, s)["seats"]
        self.assertEqual([(x["player"], x["controller"], x["class"], x["model"]) for x in seats],
                         [(1, "hybrid", "partial", "dry-run")])
        runner = ProbeRunner(self.manager)
        runner._set_live("r1", {"session": s, "run": "r1"})
        self.assertEqual([t["text"] for t in runner.status()["live"]["thoughts"]], ["the diplomat's thought"])
