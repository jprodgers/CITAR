"""citar._engine, the Rust engine's bindings (DESIGN.md P2.6), tested through Python, since the crate has no Rust tests
of its own: every binding on a seeded duel, with what it returns decoded and its errors mapped; a poisoned game; the
GIL released for heavy calls, so two games use two cores and other threads run on; the panic test operation; run_game's
callbacks; the bot handles; the laptop's dev loop; and interpreter exit with a daemon thread inside a call.

Skipped when the extension is not built (``cargo xtask develop``, or CI's build). The facade over it is package 2-08's;
these call the module itself.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import os
import sys
import threading
import time
import unittest
from pathlib import Path

try:
    from citar import _engine as E
except ImportError:          # not built: every test here is skipped
    E = None

ROOT = Path(__file__).resolve().parent.parent
needs_engine = unittest.skipIf(E is None, "citar._engine is not built (cargo xtask develop)")
needs_test_ops = unittest.skipIf(E is None or not E.HAS_TEST_OPS, "citar._engine was built without test-ops")


def dumps(v) -> bytes:
    return json.dumps(v).encode()


def duel(**kw):
    """A seeded duel with city-states and barbarians, the second seat a bot."""
    cfg = {"seed": 21, "map_size": "duel", "map_type": "pangaea",
           "players": [{"controller": "human"}, {"controller": "bot"}]}
    cfg.update(kw)
    return E.Game.new(dumps(cfg))


def small(seed: int, turn_limit: int = 60):
    """A 4-bot small game."""
    cfg = {"seed": seed, "map_size": "small", "players": [{"controller": "bot"}] * 4, "turn_limit": turn_limit}
    return E.Game.new(dumps(cfg))


def idle_bots(n: int = 4) -> dict:
    return {p: E.Bot("idle") for p in range(n)}


@needs_engine
class ModuleFunctionTests(unittest.TestCase):
    """The functions over the process's ruleset, the tools, maps, scenarios, categories and bots."""

    def test_the_build_and_the_ruleset(self):
        info = json.loads(E.build_info())
        self.assertEqual(set(info), {"version", "build_id", "label", "rules", "engine_code", "bot_code"})
        self.assertEqual(len(info["build_id"]), 12)
        self.assertTrue(E.rules_version().startswith("2-"))
        self.assertIn("techs", json.loads(E.rules_client()))
        self.assertEqual(E.max_players(), 24)
        sizes = json.loads(E.map_sizes())
        self.assertEqual(list(sizes)[:2], ["duel", "small"])
        self.assertEqual(set(sizes["duel"]), {"name", "width", "height", "players", "city_states"})
        self.assertEqual(E.map_types(), ["continents", "pangaea", "archipelago", "inland_sea", "fractal"])
        self.assertIn("Quick", E.speeds())
        self.assertEqual(E.difficulties()[0], "Settler")
        self.assertEqual(E.resolve_name("speed", "quick"), "Quick")
        self.assertIsNone(E.resolve_name("speed", "warp"))
        self.assertIsNone(E.resolve_name("speed", None))
        with self.assertRaises(ValueError):
            E.resolve_name("spaceship", "x")
        self.assertEqual(set(json.loads(E.ruleset_counts())), {"techs", "units", "buildings", "nations", "policies"})

    def test_tools_text_and_categories(self):
        tools = json.loads(E.tool_list())
        self.assertEqual(len(tools), 61)
        queries = json.loads(E.tool_list("query"))
        self.assertTrue(queries and all(t["kind"] == "query" for t in queries))
        with self.assertRaises(ValueError):
            E.tool_list("thing")
        self.assertEqual((E.tool_kind("end_turn"), E.tool_kind("get_briefing"), E.tool_kind("nope")),
                         ("action", "query", None))
        self.assertIn("trades", E.DIPLOMACY_CATEGORIES)
        self.assertEqual(E.DEBUG_ACTIONS, ("meet_all", "reveal", "gold"))
        self.assertTrue(E.RULES_OVERVIEW and E.MAP_LEGEND)
        self.assertEqual(E.item_category(dumps({"type": "gold", "amount": 50})), "trades")
        self.assertEqual(E.item_category(dumps({"type": "peace_treaty"})), "peace")
        with self.assertRaises(ValueError):
            E.item_category(dumps({"type": "gold"}))
        self.assertEqual(E.proposal_categories(dumps(None)), [])
        terms = {"0": [{"type": "gold", "amount": 10}], "1": [{"type": "peace_treaty"}]}
        self.assertEqual(E.proposal_categories(dumps(terms)), ["trades", "peace"])

    def test_maps_and_scenarios_as_values(self):
        blank = json.loads(E.blank_map(20, 16, "Grassland", "Flat land"))
        self.assertEqual((blank["width"], blank["height"], blank["id"]), (20, 16, "flat-land"))
        with self.assertRaises(E.MapError) as e:
            E.blank_map(20, 16, "Lava")
        self.assertIsInstance(e.exception, ValueError)
        clean, fixed = E.validate_map(dumps(blank))
        self.assertEqual(json.loads(clean)["format"], "citar-map")
        self.assertIsInstance(fixed, list)
        with self.assertRaises(E.MapError):
            E.validate_map(dumps({"width": 3}))
        summary = json.loads(E.map_summary(dumps(blank)))
        self.assertEqual(summary["land_share"], 1.0)
        made = json.loads(E.generate_map(5, dumps({"map_size": "duel", "players": 2})))
        self.assertEqual((made["width"], made["height"]), (44, 28))
        self.assertEqual(made, json.loads(E.generate_map(5, dumps({"map_size": "duel", "players": 2}))))
        with self.assertRaises(ValueError):
            E.generate_map(5, dumps({"map_size": "galaxy"}))
        self.assertTrue(any(o["op"] == "set_tile" for o in json.loads(E.scenario_ops_help())))
        with self.assertRaises(E.ActionError) as e:
            E.scenario_summary(dumps({"name": "x"}))
        self.assertEqual(e.exception.code, "bad_param")

    def test_a_saved_states_summary(self):
        g = duel()
        state, _ = g.save()
        s = json.loads(E.state_summary(state))
        self.assertEqual((s["turn"], s["phase"], s["map_size"], s["map_type"]), (1, "playing", "duel", "pangaea"))
        with self.assertRaises(E.LoadError) as e:
            E.state_summary(b"{}")
        self.assertIsInstance(e.exception, ValueError)

    def test_bot_versions_schemas_and_cleaning(self):
        versions = json.loads(E.bot_versions())
        self.assertEqual([v["id"] for v in versions], ["basic-1", "idle"])
        self.assertTrue(versions[0]["latest"])
        self.assertEqual(json.loads(E.bot_schema("idle")), {"engine": "idle", "groups": []})
        with self.assertRaises(ValueError):
            E.bot_schema("frozen_0123")
        self.assertEqual(json.loads(E.bot_clean_params("idle", dumps({"anything": 1}))), {})
        self.assertEqual(json.loads(E.bot_clean_params("basic", None)), {})
        with self.assertRaises(ValueError):
            E.bot_clean_params("nobot", None)


@needs_engine
class GameTests(unittest.TestCase):
    """Every method of Game on a seeded duel."""

    def test_new_and_its_errors(self):
        with self.assertRaises(ValueError):
            E.Game.new(dumps({"map_size": "duel"}))               # no seed: the host draws one
        with self.assertRaises(ValueError):
            E.Game.new(b"not json")
        with self.assertRaises(ValueError):
            E.Game.new(dumps({"seed": 1, "map_size": "galaxy"}))
        with self.assertRaises(E.MapError):
            E.Game.new(dumps({"seed": 1, "map": {"width": 3, "height": 3, "tiles": []}}))

    def test_heads(self):
        g = duel()
        self.assertEqual((g.turn, g.current, g.phase, g.winner, g.victory), (1, 0, "playing", None, None))
        self.assertGreater(g.turn_limit, 100)
        self.assertIsNone(g.poisoned)
        rev = g.revision
        self.assertTrue(g.is_alive(1))
        with self.assertRaises(ValueError):
            g.is_alive(40)
        self.assertEqual(g.open_negotiation_heads(), [])
        g.meet(0, 1)
        self.assertGreater(g.revision, rev, "a command moves the revision")
        events = json.loads(g.execute(0, "open_negotiation", dumps({"to": 1, "message": "Hello."}))[1])
        self.assertTrue(events)
        head = g.negotiation_head(1)
        self.assertEqual(head, {"id": 1, "initiator": 0, "responder": 1, "status": "open", "awaiting": 1,
                                "entries": 1})
        self.assertEqual(g.open_negotiation_heads(1), [head])
        self.assertEqual(g.open_negotiation_heads(5), [])
        g.close_negotiation(1, "expired", "Too late.")
        self.assertEqual(g.negotiation_head(1)["status"], "expired", "a closed one is read under the lock")
        with self.assertRaises(E.ActionError) as e:
            g.negotiation_head(9)
        self.assertEqual(e.exception.code, "negotiation")

    def test_reads(self):
        g = duel()
        cfg = json.loads(g.config())
        self.assertEqual((cfg["seed"], cfg["map_size"], cfg["map_type"]), (21, "duel", "pangaea"))
        s = json.loads(g.summary())
        self.assertEqual(list(s), ["turn", "current", "phase", "winner", "victory", "turn_limit", "players"])
        self.assertEqual(json.loads(g.player(1))["controller"], "bot")
        with self.assertRaises(ValueError):
            g.player(99)
        self.assertEqual(g.player_name(0), s["players"][0]["name"])
        self.assertEqual([p["id"] for p in json.loads(g.majors())], [0, 1])
        self.assertEqual(set(json.loads(g.standing(0))), {"score", "cities", "units", "population", "techs", "gold",
                                                           "era"})
        self.assertEqual(set(json.loads(g.standings())), {"0", "1"})
        for _ in range(4):
            g.end_turn(g.current)
        self.assertEqual(g.turn, 3)
        stats = json.loads(g.stats())
        self.assertEqual(len(stats), 2)
        self.assertEqual(json.loads(g.stats(1)), stats[-1:])
        events = json.loads(g.events())
        self.assertEqual(json.loads(g.events(2)), events[-2:])
        ev = events[-1]
        self.assertEqual(json.loads(g.event_view(ev["id"])), ev)
        self.assertIn("text", json.loads(g.event_view(ev["id"], 1)))
        with self.assertRaises(ValueError):
            g.event_view(10 ** 6)
        g.add_thought(0, "Settle the river.", "thought")
        g.add_thought(1, "Hold.")
        self.assertEqual(g.thought_count(), 2)
        self.assertEqual(json.loads(g.thoughts(1)), [{"turn": 3, "player": 1, "text": "Hold.", "kind": None}])
        self.assertEqual(len(json.loads(g.thoughts(None, 1))), 1)
        emitted = json.loads(g.emit("agent_error", "Player 1's AI could not play.", None, dumps({"player": 1})))
        self.assertEqual((emitted[0]["type"], emitted[0]["data"]), ("agent_error", {"player": 1}))
        private = json.loads(g.emit("game_paused", "Paused.", [0]))
        self.assertEqual(private[0]["players"], [0])
        with self.assertRaises(ValueError):
            g.emit("x", "y", None, dumps({"weather": 1}))
        self.assertEqual(g.take_violations(), [])

    def test_reads_of_every_player_answer_without_poisoning_the_game(self):
        g = duel()
        for _ in range(4):
            g.end_turn(g.current)
        players = json.loads(g.summary())["players"]
        self.assertEqual({p["kind"] for p in players}, {"major", "city_state", "barbarian"})
        for p in players:
            pid = p["id"]
            with self.subTest(pid=pid, kind=p["kind"]):
                json.loads(g.player(pid))
                json.loads(g.standing(pid))
                json.loads(g.view_json(pid, 5))
                json.loads(g.open_negotiations(pid))
                g.end_turn_refusal(pid)
                g.has_met(0, pid)
                json.loads(g.path_preview(pid, 1, 3, 3))
                if p["kind"] == "major":
                    self.assertTrue(g.briefing(pid) and g.turn_progress(pid))
                    json.loads(g.empire_summary(pid))
                else:
                    for call in (g.briefing, g.turn_progress, g.empire_summary):
                        with self.assertRaises(ValueError):
                            call(pid)
                for ev in json.loads(g.events(5)):
                    json.loads(g.event_view(ev["id"], pid))
        self.assertIsNone(g.poisoned)

    def test_tools_views_and_text(self):
        g = duel()
        result, events = g.execute(0, "get_empire")
        self.assertIn("gold", json.loads(result))
        self.assertEqual(json.loads(events), [])
        with self.assertRaises(E.ActionError) as e:
            g.execute(1, "end_turn")
        self.assertEqual(e.exception.code, "not_your_turn")
        with self.assertRaises(E.ActionError) as e:
            g.execute(0, "no_such_tool")
        self.assertEqual(e.exception.code, "unknown_tool")
        with self.assertRaises(E.ActionError) as e:
            g.execute(300, "get_empire")
        self.assertEqual(e.exception.code, "invalid_player")
        self.assertNotIsInstance(e.exception, RuntimeError)
        view = json.loads(g.view_json(0, 20, dumps({"seat": {"player": 0}, "version": 7})))
        self.assertEqual((view["you"], view["seat"], view["version"]), (0, {"player": 0}, 7))
        god = json.loads(g.view_json(None))
        self.assertIsNone(god["you"])
        with self.assertRaises(ValueError):
            g.view_json(0, 20, dumps({"turn": 5}))           # the view's own key
        self.assertIn("TURN 1", g.briefing(0))
        self.assertTrue(g.turn_progress(0))
        es = json.loads(g.empire_summary(0))
        self.assertTrue({"cities", "at_war_with", "notes"} <= set(es))
        self.assertIsNone(g.end_turn_refusal(0))
        path = json.loads(g.path_preview(0, 10 ** 6, 1, 1))
        self.assertEqual(path, {"path": None})
        unit = json.loads(g.execute(0, "get_units")[0])
        uid, x, y = unit[0]["id"], unit[0]["x"], unit[0]["y"]
        route = json.loads(g.path_preview(0, uid, x + 2, y))
        self.assertEqual(route["path"][0], [x, y])
        self.assertEqual(route["path"][-1], [x + 2, y])
        self.assertGreaterEqual(route["turns"], 1)
        self.assertEqual(json.loads(g.path_preview(1, uid, x + 2, y)), {"path": None}, "not player 1's unit")

    def test_negotiations(self):
        g = duel()
        g.meet(0, 1)
        g.debug("gold")
        opened, events = g.open_negotiation_as(1, 0, "Trade?", dumps([{"type": "gold", "amount": 50}]), None)
        nid = json.loads(opened)["negotiation_id"]
        self.assertTrue(json.loads(events))
        n = json.loads(g.negotiation(nid))
        self.assertEqual((n["initiator"], n["status"]), (1, "open"))
        self.assertEqual([x["id"] for x in json.loads(g.open_negotiations(0))], [nid])
        self.assertEqual(json.loads(g.negotiations()), json.loads(g.open_negotiations()))
        self.assertIn("summary", json.dumps(json.loads(g.negotiation_view(nid, 0))))
        self.assertGreater(g.max_chat_messages(), 1)
        self.assertIsNone(g.deal(1))
        self.assertEqual(g.describe_items(dumps([{"type": "gold", "amount": 50}])), "50 gold")
        terms = {"0": [{"type": "gold", "amount": 1}], "1": []}
        g.validate_items(0, 1, dumps([{"type": "gold", "amount": 1}]), dumps(terms))
        with self.assertRaises(E.ActionError):
            g.validate_items(0, 1, dumps([{"type": "gold", "amount": 10 ** 6}]), dumps(terms))
        closed, events = g.close_negotiation(nid, "rejected", "No.", 0)
        self.assertEqual(json.loads(closed)["status"], "rejected")
        with self.assertRaises(E.ActionError):
            g.close_negotiation(nid, "rejected", "Again.")
        with self.assertRaises(E.ActionError):
            g.negotiation(42)
        # A deal concluded: the other side accepts a gift.
        opened, _ = g.execute(0, "open_negotiation", dumps({"to": 1, "message": "A gift.",
                                                             "give": [{"type": "gold", "amount": 40}]}))
        gift = json.loads(opened)["negotiation_id"]
        g.execute(1, "respond_negotiation", dumps({"negotiation_id": gift, "action": "accept", "message": "Thanks."}))
        deal_id = json.loads(g.negotiation(gift))["deal_id"]
        deal = json.loads(g.deal(deal_id))
        self.assertEqual((deal["id"], deal["parties"]), (deal_id, [0, 1]))
        self.assertIn("40 gold", deal["summary"])

    def test_seats_scenario_map_and_debug(self):
        g = duel()
        g.set_controller(1, "llm", "ai", dumps({"conquest": False}))
        row = json.loads(g.player(1))
        self.assertEqual((row["controller"], row["handicap"], row["auto"]["conquest"]), ("llm", "ai", False))
        with self.assertRaises(ValueError):
            g.set_controller(1, "robot")
        with self.assertRaises(ValueError):
            g.set_controller(1, "bot", "godlike")
        self.assertTrue(g.set_difficulty(0, "deity"))
        self.assertFalse(g.set_difficulty(0, "impossible"))
        done, events = g.apply_ops(dumps([{"op": "set_player", "player": 0, "gold": 321}]))
        self.assertEqual(len(json.loads(done)), 1)
        self.assertEqual(json.loads(g.standing(0))["gold"], 321)
        with self.assertRaises(E.ActionError):
            g.apply_ops(dumps([{"op": "no_such_op"}]))
        self.assertIn("players", json.loads(g.scenario_overview()))
        seats = json.loads(g.default_seats())
        self.assertEqual(len(seats), 2)
        self.assertEqual(len(json.loads(g.normalize_seats(dumps(seats)))), 2)
        exported = json.loads(g.export_map("From a test"))
        self.assertEqual((exported["name"], exported["width"]), ("From a test", 44))
        self.assertFalse(g.has_met(0, 1))
        self.assertTrue(json.loads(g.meet(0, 1)))
        self.assertTrue(g.has_met(0, 1))
        g.force_turn(1)
        self.assertEqual(g.current, 1)
        with self.assertRaises(E.ActionError):
            g.end_turn(0)
        g.end_turn(1)
        json.loads(g.debug("gold"))
        with self.assertRaises(ValueError):
            g.debug("money")

    def test_saves_and_the_replay(self):
        g = duel()
        for _ in range(6):
            g.end_turn(g.current)
        state, history = g.save()
        self.assertIsNotNone(history)
        h, report = E.Game.load(state, [history])
        self.assertEqual(json.loads(report)["chronicle_incomplete"], False)
        self.assertEqual(h.digest(), g.digest())
        self.assertEqual(json.loads(h.events()), json.loads(g.events()))
        partial, report = E.Game.load(state)
        self.assertTrue(json.loads(report)["chronicle_incomplete"], "a save without its history plays on")
        self.assertEqual(partial.digest(), g.digest())
        with self.assertRaises(E.LoadError):
            E.Game.load(b'{"format": "nonsense"}')
        self.assertEqual(json.loads(g.state_json())["format"], json.loads(state)["format"])
        replay = json.loads(g.replay_data())
        self.assertEqual(replay["format"], "full")
        self.assertEqual(json.loads(g.replay_data("delta"))["format"], "delta")
        with self.assertRaises(ValueError):
            g.replay_data("mpeg")
        served = json.loads(g.replay_json(dumps({"id": "g1", "name": "Duel", "seats": [{"type": "human"}]})))
        self.assertEqual(list(served)[:2], ["id", "name"])
        self.assertEqual((served["players"][0]["seat"], served["players"][1]["seat"]), ({"type": "human"}, None))

    def test_drive_answer_and_advice(self):
        g = duel()
        bots = {1: E.Bot("idle")}
        stop, events, actions = g.drive(bots, 0)
        self.assertEqual(json.loads(stop), {"stop": "external", "player": 0, "negotiations": []})
        self.assertEqual(json.loads(actions), {"1": {}})
        with self.assertRaises(TypeError):
            g.drive({1: object()})
        # A bot keyed to a player that is no major civilization is a mistake of the host's, refused as run_game
        # refuses it, before anything moves.
        players = json.loads(g.summary())["players"]
        city_state = next(p["id"] for p in players if p["kind"] == "city_state")
        rev = g.revision
        with self.assertRaises(ValueError):
            g.drive({city_state: E.Bot("idle")})
        with self.assertRaises(ValueError):
            g.drive({1: E.Bot("idle"), 77: E.Bot("idle")})
        self.assertEqual((g.revision, g.poisoned), (rev, None))
        g.execute(0, "end_turn")
        stop, events, _ = g.drive(bots, 1)
        self.assertIn(json.loads(stop)["stop"], ("seat_limit", "external"))
        self.assertTrue(json.loads(events))
        g.meet(0, 1)
        g.execute(0, "open_negotiation", dumps({"to": 1, "message": "Peace?"}))
        outcome, events, actions = g.answer(1, 1, E.Bot("idle"))
        self.assertEqual(outcome, "done")
        self.assertEqual(json.loads(g.negotiation(1))["status"], "rejected")
        with self.assertRaises(E.ActionError):
            g.answer(1, 1, E.Bot("idle"))                      # no longer waiting on player 1
        advice = json.loads(g.bot_advice(1, E.Bot("basic")))
        self.assertEqual(set(advice), {"deal_value", "war_readiness", "spare_luxuries", "wants"})


@needs_test_ops
class TestOperationTests(unittest.TestCase):
    """inspect and test_ops, including the operations that replace the Python tests' pokes."""

    def test_inspect_and_the_operations(self):
        g = duel()
        self.assertEqual(json.loads(g.inspect(dumps({"what": "player", "player": 0})))["id"], 0)
        with self.assertRaises(E.ActionError):
            g.inspect(dumps({"what": "nothing"}))
        done, _ = g.test_ops(dumps([{"op": "set_turn", "turn": 5}]))
        self.assertEqual(json.loads(done), [{"turn": 5}])
        self.assertEqual(g.turn, 5, "the heads follow a test operation")
        done, events = g.test_ops(dumps([{"op": "eliminate", "player": 1}]))
        self.assertEqual(json.loads(done)[0]["eliminated"], 1)
        self.assertFalse(g.is_alive(1))
        self.assertEqual((g.phase, g.winner), ("over", 0), "the last one standing")
        h = duel()
        h.test_ops(dumps([{"op": "end_game"}]))
        self.assertEqual((h.phase, h.winner), ("over", None))
        h = duel()
        h.test_ops(dumps([{"op": "end_game", "winner": 1, "victory": "Time"}]))
        self.assertEqual((h.winner, h.victory), (1, "Time"))
        h = duel()
        h.set_checks(True)
        for _ in range(4):
            h.end_turn(h.current)
        self.assertEqual(h.take_violations(), [])

    def test_a_panic_poisons_the_game_never_its_lock(self):
        g = duel()
        before = g.revision
        with self.assertRaises(E.EngineCrash) as e:
            g.test_ops(dumps([{"op": "panic"}]))
        crash = e.exception
        self.assertIsInstance(crash, RuntimeError)
        self.assertNotIsInstance(crash, E.ActionError)
        self.assertIn("panic test operation", str(crash))
        self.assertIn("testops.rs", str(crash), "where it happened")
        self.assertIsNotNone(g.poisoned)
        self.assertGreaterEqual(g.revision, before)
        # The game is poisoned and its lock is not: the panic was caught inside it. (Reads alone cannot tell, since
        # the binding reads through a poisoned lock.)
        self.assertFalse(g._lock_poisoned())
        # Every command is refused with EngineCrash, never ActionError: a crash is not a refusal.
        for call in (lambda: g.execute(0, "end_turn"), lambda: g.end_turn(0), lambda: g.meet(0, 1),
                     lambda: g.drive({1: E.Bot("idle")}), lambda: g.test_ops(dumps([{"op": "set_turn", "turn": 2}])),
                     lambda: g.add_thought(0, "x"), lambda: g.emit("x", "y")):
            with self.assertRaises(E.EngineCrash):
                call()
        # The lock is not poisoned: reads still answer, and so do saves.
        self.assertEqual(json.loads(g.summary())["turn"], 1)
        self.assertEqual(json.loads(g.view_json(0))["you"], 0)
        self.assertIn("frames", json.loads(g.replay_json()))
        state, _ = g.save()
        self.assertTrue(state)
        # The process lives, and another game plays on.
        h = duel()
        h.end_turn(0)
        self.assertEqual(h.current, 1)


@needs_engine
class BotHandleTests(unittest.TestCase):
    def test_a_handle_and_its_fingerprint(self):
        b = E.Bot("basic", None, 0.7)
        self.assertEqual((b.version, b.aggression, b.fixed_aggression), ("basic-1", 0.7, None))
        self.assertEqual(json.loads(b.params), {})
        with self.assertRaises(ValueError):
            E.Bot("frozen_abc")
        fixed_a, fixed_b = E.Bot("basic", None, 0.2, 0.5), E.Bot("basic", None, 0.9, 0.5)
        self.assertEqual((fixed_a.aggression, fixed_b.aggression), (0.5, 0.5))
        self.assertEqual(fixed_a.fingerprint(), fixed_b.fingerprint(), "the profile's aggression, not the seat's")
        self.assertEqual(E.bot_fingerprint(fixed_a), fixed_a.fingerprint())
        self.assertNotEqual(E.Bot("basic", None, 0.2, 0.6).fingerprint(), fixed_a.fingerprint())
        self.assertNotEqual(E.Bot("idle").fingerprint(), E.Bot("basic").fingerprint())
        self.assertEqual(E.Bot("basic", None, None, 2.0).aggression, 1.0)

    def test_set_diplomacy_changes_the_handle_in_place(self):
        g = duel()
        g.meet(0, 1)
        g.execute(0, "open_negotiation", dumps({"to": 1, "message": "Hello."}))
        chat = g.negotiation(1)
        bot = E.Bot("basic")
        same = bot
        self.assertTrue(bot.owns_negotiation(chat))
        bot.set_diplomacy(dumps({"chat": "llm"}))
        self.assertFalse(same.owns_negotiation(chat), "the same handle, changed in place")
        self.assertEqual(json.loads(same.owners)["chat"], "llm")
        # A seat that holds the handle follows it: basic-1 leaves a chat its model owns to the host.
        outcome, _, _ = g.answer(1, 1, same)
        self.assertEqual(outcome, "deferred")
        self.assertEqual(json.loads(g.negotiation(1))["status"], "open")
        bot.set_diplomacy(None)
        self.assertTrue(same.owns_negotiation(chat))
        with self.assertRaises(ValueError):
            bot.set_diplomacy(dumps({"wars": "llm"}))
        offer = {"id": 2, "proposal": {"0": [{"type": "gold", "amount": 5}], "1": [{"type": "peace_treaty"}]}}
        bot.set_diplomacy(dumps({"peace": "llm"}))
        self.assertFalse(bot.owns_negotiation(dumps(offer)))
        self.assertTrue(bot.owns_negotiation(chat), "a chat with no proposal is the bot's while it owns chat")


@needs_engine
class RunGameTests(unittest.TestCase):
    def spec(self, **kw) -> bytes:
        spec = {"config": {"seed": 3, "map_size": "duel", "map_type": "pangaea",
                           "players": [{"controller": "bot"}, {"controller": "bot"}], "turn_limit": 25}}
        spec.update(kw)
        return dumps(spec)

    def test_every_event_once_in_order_and_a_round_call_per_turn(self):
        seen, rounds = [], []
        out = json.loads(E.run_game(self.spec(), {0: E.Bot("idle"), 1: E.Bot("basic")},
                                    on_turn=rounds.append, on_event=seen.append))
        self.assertEqual((out["phase"], out["turn_limit"], out["errors"]), ("over", 25, []))
        self.assertEqual(set(out), {"turn", "turns", "phase", "winner", "victory", "turn_limit", "stats", "players",
                                    "errors"})
        ids = [e["id"] for e in seen]
        self.assertEqual(ids, sorted(set(ids)), "each once, in order")
        self.assertTrue(ids)
        # Every event after the game's creation, to the last: the same game played as the runner plays it, one
        # driven seat a step through Game.drive, holds exactly these in its chronicle, the last step's included.
        g = E.Game.new(json.dumps(json.loads(self.spec())["config"]).encode())
        created = len(json.loads(g.events()))
        replay_bots = {0: E.Bot("idle"), 1: E.Bot("basic")}
        while g.phase == "playing":
            stop = json.loads(g.drive(replay_bots, 1)[0])["stop"]
            self.assertIn(stop, ("seat_limit", "game_over"), "both seats have bots: nothing waits on the host")
        chronicle = json.loads(g.events())
        self.assertEqual(json.loads(g.stats()), out["stats"], "the same game")
        self.assertEqual(ids[0], created + 1)
        self.assertEqual(seen, chronicle[created:])
        turns = [r["turn"] for r in rounds]
        self.assertEqual(turns, list(range(1, 27)), "the first turn, each new one, and the one it ended on")
        self.assertEqual(rounds[-1]["phase"], "over")
        self.assertEqual(set(rounds[0]), {"turn", "phase", "turn_limit", "last_stats"})
        again = json.loads(E.run_game(self.spec(), {0: E.Bot("idle"), 1: E.Bot("basic")}))
        self.assertEqual(again, out, "the same spec plays the same game")

    def test_errors_from_the_spec_and_the_bots(self):
        with self.assertRaises(ValueError):
            E.run_game(dumps({}), {})
        with self.assertRaises(TypeError):
            E.run_game(self.spec(), {0: "a bot"})
        with self.assertRaises(ValueError):
            E.run_game(self.spec(), {7: E.Bot("idle")})
        self.assertEqual(json.loads(E.run_game(self.spec(max_errors=3), {}))["phase"], "over",
                         "max_errors is read and ignored; seats with no bot pass their turns")

        def boom(_):
            raise KeyError("from the hook")
        with self.assertRaises(KeyError):
            E.run_game(self.spec(), {0: E.Bot("idle")}, on_turn=boom)

    def test_a_listener_that_raises_hears_every_event_and_the_game_plays_on(self):
        # on_event is a listener, as headless.play made it: Game.emit swallowed a listener's Exception, so the lab's
        # listen, which indexes each event's data, never ended a game over one event. Rust reports it instead.
        quiet = []
        E.run_game(self.spec(), {0: E.Bot("idle"), 1: E.Bot("idle")}, on_event=lambda ev: quiet.append(ev["id"]))
        heard, reported = [], []

        def listen(ev):
            heard.append(ev["id"])
            if ev["id"] % 2:
                raise KeyError("era")

        hook = sys.unraisablehook
        sys.unraisablehook = reported.append
        try:
            out = json.loads(E.run_game(self.spec(), {0: E.Bot("idle"), 1: E.Bot("idle")}, on_event=listen))
        finally:
            sys.unraisablehook = hook
        self.assertEqual(out["phase"], "over")
        self.assertEqual(heard, quiet, "every event, after each one that raised too")
        self.assertEqual(len(reported), sum(1 for i in heard if i % 2))
        self.assertTrue(all(isinstance(r.exc_value, KeyError) and r.object is listen for r in reported))

        class Stop(BaseException):
            pass

        def stop(_):
            raise Stop()
        with self.assertRaises(Stop, msg="a BaseException that is no Exception ends the run"):
            E.run_game(self.spec(), {0: E.Bot("idle"), 1: E.Bot("idle")}, on_event=stop)

    def test_ctrl_c_stops_a_run_with_no_hooks_within_a_step(self):
        # balance.py's games and citar sim's long ones: no Python runs between steps, so the run checks for signals.
        import _thread
        import signal

        class Interrupted(Exception):
            pass

        fired, sent, done = [], [], threading.Event()

        def handler(signum, frame):
            fired.append(time.perf_counter())
            raise Interrupted()

        def interrupt():
            # Once the run is stepping (its steps are calls in flight), and a little more.
            while E.calls_in_flight() == 0 and not done.is_set():
                time.sleep(0.0005)
            time.sleep(0.05)
            if not done.is_set():
                sent.append(time.perf_counter())
                _thread.interrupt_main(signal.SIGINT)

        # 500 rounds of a 4-bot small game: about a second on the laptop, so a run that did not check would hear
        # the signal only at its end.
        spec = dumps({"config": {"seed": 3, "map_size": "small", "players": [{"controller": "bot"}] * 4,
                                 "turn_limit": 500}})
        old = signal.signal(signal.SIGINT, handler)
        t = threading.Thread(target=interrupt)
        try:
            t.start()
            with self.assertRaises(Interrupted):
                E.run_game(spec, idle_bots())
        finally:
            done.set()
            t.join(30)
            signal.signal(signal.SIGINT, old)
        self.assertEqual(len(sent), 1)
        self.assertLess(fired[0] - sent[0], 0.1, "the run stopped within a step of the signal")

    @needs_test_ops
    def test_a_crash_is_recorded_or_raised(self):
        spec = self.spec(labels={"0": "careless"}, test_panic={"player": 0, "turn": 4})
        out = json.loads(E.run_game(spec, {0: E.Bot("idle"), 1: E.Bot("idle")}))
        self.assertEqual(out["phase"], "playing")
        self.assertEqual(len(out["errors"]), 1)
        self.assertTrue(out["errors"][0].startswith("T4 P0 careless: panic: the test bot panics on turn 4"),
                        out["errors"][0])
        raising = self.spec(raise_errors=True, test_panic={"player": 1, "turn": 2})
        with self.assertRaises(E.EngineCrash) as e:
            E.run_game(raising, {0: E.Bot("idle"), 1: E.Bot("idle")})
        self.assertIn("T2 P1", str(e.exception))


@needs_engine
class ParallelismTests(unittest.TestCase):
    """The GIL is released for every heavy call (DESIGN.md P2.6.2, package 2-06a's gate 2)."""

    def test_two_games_on_two_threads_use_two_cores(self):
        # Each thread plays 60-round games of its own, a round (four seats) per call, until a second has passed, so the
        # CPU clock's tick (16 ms on Windows) is small beside the window. Both stop at the same moment, within a call
        # of it (about 1.3 ms on the laptop): a thread still finishing a game alone would count wall time on one core.
        # A round rather than a seat per call takes the GIL back a quarter as often, so a slow wake-up on a virtual
        # runner costs the measure less.
        # The best of five such seconds is the measure: a shared CI runner (macOS's has three cores) can lend a core
        # away for part of one (1.58 once; 1.33, 1.57 and 1.21 in three running), while a drive that held the GIL
        # would stay near 1.0 in every one.
        def worker(seed, until):
            bots = idle_bots()
            while time.perf_counter() < until:
                g = small(seed)
                while g.phase == "playing" and time.perf_counter() < until:
                    g.drive(bots, 4)
                seed += 2

        def one_second():
            cpu0, wall0 = time.process_time(), time.perf_counter()
            threads = [threading.Thread(target=worker, args=(s, wall0 + 1.0)) for s in (101, 102)]
            for t in threads:
                t.start()
            for t in threads:
                t.join(120)
            return time.process_time() - cpu0, time.perf_counter() - wall0

        tries = []
        while len(tries) < 5 and not any(cpu / wall >= 1.6 for cpu, wall in tries):
            tries.append(one_second())
        cpu, wall = max(tries, key=lambda t: t[0] / t[1])
        self.assertGreaterEqual(cpu / wall, 1.6, f"CPU {cpu:.2f} s in {wall:.2f} s of wall time, the best of "
                                                 f"{', '.join(f'{c / w:.2f}' for c, w in tries)}")

    def test_a_counting_thread_keeps_running_while_another_drives(self):
        def count(seconds: float) -> float:
            n, until = 0, time.perf_counter() + seconds
            while time.perf_counter() < until:
                n += 1
            return n / seconds

        solo = count(0.4)
        stop = threading.Event()

        def drive():
            # A whole game in each call (about 0.1 s): a drive that held the GIL would starve the counter for all of
            # it, not for one switch interval at a time.
            seed, bots = 200, idle_bots()
            while not stop.is_set():
                small(seed).drive(bots, 0)
                seed += 1

        t = threading.Thread(target=drive)
        t.start()
        try:
            time.sleep(0.05)
            shared = count(0.4)
        finally:
            stop.set()
            t.join(60)
        self.assertGreaterEqual(shared, solo / 2, f"{shared:.0f} counts a second beside a drive, {solo:.0f} alone")

    def test_cheap_reads_never_wait_for_a_drive(self):
        g = small(300, turn_limit=330)
        done = threading.Event()

        def drive():
            try:
                g.drive(idle_bots(), 0)                         # the whole game in one call
            finally:
                done.set()

        t = threading.Thread(target=drive)
        t.start()
        while E.calls_in_flight() == 0 and not done.is_set():
            time.sleep(0.0005)
        polls, turns = [], set()
        while len(polls) < 1000 and not done.is_set():
            t0 = time.perf_counter()
            turns.add(g.turn)
            polls.append(time.perf_counter() - t0)
        during = not done.is_set()
        t.join(120)
        self.assertFalse(t.is_alive(), "the drive ended: nothing deadlocked")
        self.assertTrue(during and len(polls) == 1000, f"{len(polls)} polls while the drive ran")
        self.assertLess(max(polls), 0.001, f"the slowest poll took {max(polls) * 1000:.3f} ms")
        self.assertEqual(g.phase, "over")


@needs_engine
class StubTests(unittest.TestCase):
    def test_the_stubs_name_what_the_module_has(self):
        import ast
        tree = ast.parse((ROOT / "citar" / "_engine.pyi").read_text(encoding="utf-8"))
        top, classes = set(), {}
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
                top.add(node.name)
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                top.add(node.target.id)
            if isinstance(node, ast.ClassDef):
                classes[node.name] = {n.name for n in node.body if isinstance(n, ast.FunctionDef)}
        public = {n for n in dir(E) if not n.startswith("_")}
        self.assertEqual(top, public)
        test_only = {"inspect", "test_ops", "set_checks", "_lock_poisoned"}
        for cls in ("Game", "Bot"):
            have = {n for n in dir(getattr(E, cls)) if not n.startswith("__")}
            stubbed = classes[cls] - {"__init__"}
            if not E.HAS_TEST_OPS:
                stubbed -= test_only
            self.assertEqual(stubbed, have, cls)


@needs_engine
class LifecycleTests(unittest.TestCase):
    def test_the_dev_loop_imports_the_extension_from_its_folder(self):
        ext = os.environ.get("CITAR_EXT_DIR")
        if not ext:
            self.skipTest("CITAR_EXT_DIR is not set: the extension was not built by cargo xtask develop")
        self.assertEqual(Path(E.__file__).resolve().parent, Path(ext).resolve())
        self.assertEqual(sorted(p.name for p in (ROOT / "citar").glob("_engine*.pyd")), [],
                         "nothing built into the checkout")
        self.assertEqual(sorted(p.name for p in (ROOT / "citar").glob("_engine*.so")), [])

    def test_no_call_is_in_flight_between_tests(self):
        self.assertEqual(E.calls_in_flight(), 0)

    def test_the_shutdown_parks_other_threads_and_lets_its_own_go_on(self):
        from tests import engine_exit_child as child
        ok, line = child.run("barred")
        self.assertTrue(ok, line)

    def test_the_interpreter_exits_cleanly_with_a_daemon_thread_inside_a_call(self):
        # Each child exits mid-drive and opens the window during finalization, a finalizer that releases the GIL
        # while any call ends; it passes only if that window opened, with no call in flight in it, and exit code 0.
        from tests import engine_exit_child as child
        for mode in ("long", "short"):
            with self.subTest(mode=mode):
                ok, line = child.run(mode)
                self.assertTrue(ok, line)


if __name__ == "__main__":
    unittest.main()
