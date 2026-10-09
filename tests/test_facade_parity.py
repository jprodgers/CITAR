"""The facade's two backends give the same shapes (crates/citar-engine/DESIGN.md P2.6.6, risk 5 of P2.11).

Both backend modules are imported side by side: ``citar.engine.facade`` (the Python engine) and
``citar._facade_rust`` (the Rust engine, skipped when the extension is not built). Each plays the same seeded duel and
the same small game, and every original name of the facade and every EngineGame method is called on each with the same
arguments; each answer is compared with the same call's answer on the other backend, by shape under type classes
(tests/parity_shapes.py): int and float are one number class, None matches any type, an empty collection matches any
element type, records keep their key sets, maps their key kind. A key some records of one answer lack is optional in
that answer alone, and only for the side that has it; the keys a record has only in some states (a unit's own
detail) are listed in CONDITIONAL, each with why. An answer that is vacant (None, empty, or holding only those) on one
backend and not on the other would compare nothing, so that fails too unless VACANT lists it with why. The two
engines play different games from one seed, so values are never compared, except where a behaviour test pins them: a
bot's diplomacy switch and whom it then answers, a crashing bot's record and ``raise_errors``, and the events a game
hands its subscribers, in order and each once.

What differs on purpose is listed in DOCUMENTED, each with where it is decided; an entry that no longer differs fails
the test, so the list cannot go stale. What a save holds (``to_save``, ``state_dict``) is each engine's own format and
is compared through ``state_summary``, the only reading of it the facade promises.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import copy
import inspect
import re
import unittest

from citar.engine import facade as PY
from tests import rulescript
from tests.parity_shapes import compare, record_paths, shape, vacancies

try:
    from citar import _facade_rust as RS
except ImportError:          # the extension is not built
    RS = None

#: Differences the backends have on purpose: the path of the difference, and where it is decided. A path is the
#: battery's key with the place inside the value (``found_city.at``).
DOCUMENTED = {
    "found_city.at": "intended unit-results-give-tiles: Python gave the keys of the tile's coordinates, ['x', 'y']",
    "rules_version": "DESIGN.md 5.2 and P2.6.1: the ruleset's format and the start of its id ('2-0123456789ab'), a "
                     "str as the facade's annotation always said; Python returned the bare format number, 2",
}
#: The keys a record has only in some states, by the end of its path (as tests/parity_shapes.py names it), with why:
#: either side may have them in all, some or none of its records. "*" makes every key so: a map by snake-case name.
CONDITIONAL = {
    ".units[]": ({"moves", "max_moves", "activity", "xp", "promotions", "can_promote", "promotion_ready",
                  "attacks_made", "embarked", "fortified_turns", "return_offer", "status", "building", "goto",
                  "religious_strength", "great_person", "religion"},
                 "a unit's own detail, which another civilization's unit lacks, and what only some units have: a "
                 "fortified unit's fortified_turns, a great person's flag, a worker's building, a religion "
                 "(citar/engine/views.py unit_info)"),
    ".per_turn_breakdown{}": ("*", "a source's yields by name, a zero left out (views.py empire_info)"),
    "[type=negotiation].data": ({"awaiting", "status"},
                                "a negotiation's event names whom it awaits while open and its status once closed "
                                "(citar/engine/diplomacy.py)"),
}
#: Answers (by battery key, or a record's field below one) vacant on one backend alone, and why: their shapes
#: compare nothing there. An entry that is no longer vacant on one side alone fails the test, so the list cannot go
#: stale.
VACANT: dict = {}
#: The facade's names and EngineGame's members this test does not call by shape, and why.
NOT_BY_SHAPE = {
    "EngineGame.python_game": "the Python engine's own game: the Rust backend has none, by design (BackendError)",
    "EngineGame.to_save": "each engine's own save format; compared through state_summary, and loaded back",
    "EngineGame.state_dict": "each engine's own state layout; compared through state_summary, and loaded back",
    "EngineGame.subscribe": "a behaviour: test_events_reach_subscribers_in_order_and_once",
    "EngineGame.unsubscribe": "a behaviour: test_events_reach_subscribers_in_order_and_once",
    "bot_instance": "an opaque handle; what it plays is compared through the games it plays",
    "EngineGame.inspect": "the rule scripts' reads, held equal on both engines by tests/rules (test_rule_scripts)",
    "EngineGame.test_ops": "the rule scripts' operations, held equal on both engines by tests/rules",
}
#: The names Phase 2 added, which the Python backend refuses: not compared, only checked to refuse.
PHASE2 = {"EngineCrash", "BackendError", "build_info", "bot_versions", "bot_schema", "bot_clean_params",
          "bot_fingerprint", "EngineGame.drive", "EngineGame.answer", "EngineGame.view_json",
          "EngineGame.replay_json"}

DUEL = {"map_size": "duel", "seed": 21, "players": [{"controller": "human"}, {"controller": "bot"}]}
SMALL = {"map_size": "small", "seed": 7, "turn_limit": 12, "players": [{"controller": "bot"}] * 4}


def original_names() -> set:
    """Every name of the Python backend before Phase 2, and EngineGame's members, as "name" or "EngineGame.name"."""
    names = set(PY.__all__) - PHASE2
    for name, member in vars(PY.EngineGame).items():
        if not name.startswith("_") and (callable(member) or isinstance(member, (property, classmethod))):
            names.add(f"EngineGame.{name}")
    return names - PHASE2


def _path_target(g, pid: int):
    """One of ``pid``'s units and a tile two steps east or west it has a route to, for path_preview."""
    view = g.view(pid)
    for u in sorted((u for u in view["units"] if u.get("owner") == pid), key=lambda u: u["id"]):
        for dx in (2, -2, 1, -1):
            x, y = u["x"] + dx, u["y"]
            if 0 <= x < view["width"] and g.path_preview(pid, u["id"], x, y)["path"]:
                return u["id"], x, y
    raise AssertionError("no unit with a route two tiles away")


def duel_battery(B) -> dict:
    """The seeded duel on backend B: a human seat (0) and a bot seat (1). Returns {key: answer}; a key is the name
    called (``EngineGame.name`` for a method), with ``:case`` when one name is called more than once, or a few words
    for an answer of its own kind (a tool's result). Each key's answer is compared with the same key's on the other
    backend."""
    out = {}
    g = B.EngineGame.new(dict(DUEL))
    g.meet(0, 1)
    out["EngineGame.new"] = g.summary()
    out["EngineGame.meet"] = g.has_met(0, 1)
    out["EngineGame.has_met"] = g.has_met(0, 1)
    v = g.view(0)
    settler = next(u for u in v["units"] if u["type"] == "Settler" and u["owner"] == 0)
    out["found_city"] = g.execute(0, "found_city", {"unit_id": settler["id"]})
    out["EngineGame.execute"] = g.execute(0, "set_research", {"tech": "Pottery"})
    out["execute get_empire"] = g.execute(0, "get_empire", {})
    out["EngineGame.apply_ops"] = g.apply_ops([{"op": "set_player", "player": 0, "gold": 200}])
    nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Gold for peace of mind.",
                                           "give": [{"type": "gold", "amount": 30}]})["negotiation_id"]
    out["EngineGame.negotiation:open"] = g.negotiation(nid)
    out["EngineGame.negotiation_view"] = g.negotiation_view(nid, 0)
    out["EngineGame.negotiation_head"] = g.negotiation_head(nid)
    out["EngineGame.open_negotiation_heads"] = g.open_negotiation_heads(1)
    out["EngineGame.open_negotiations"] = g.open_negotiations(1)
    out["EngineGame.end_turn_refusal"] = g.end_turn_refusal(1)
    out["EngineGame.validate_items"] = g.validate_items(0, 1, [{"type": "gold", "amount": 10}],
                                                        g.negotiation(nid)["proposal"])
    bot = B.bot_instance("basic", seed=1)
    out["EngineGame.bot_advice"] = g.bot_advice(1, bot, nid)
    out["EngineGame.bot_respond"] = g.bot_respond(1, nid, bot)
    out["EngineGame.negotiation:answered"] = g.negotiation(nid)
    probe = g.open_negotiation_as(1, 0, "A word, out of turn.")
    out["EngineGame.open_negotiation_as"] = probe
    out["EngineGame.close_negotiation"] = g.close_negotiation(probe["negotiation_id"], "expired", "(no reply in time)")
    # A deal both engines conclude alike: seat 1 accepts a second gift itself (the bot accepted the first).
    gift = g.execute(0, "open_negotiation", {"to": 1, "message": "A gift between neighbours.",
                                            "give": [{"type": "gold", "amount": 20}]})["negotiation_id"]
    out["execute respond_negotiation accept"] = g.execute(1, "respond_negotiation", {
        "negotiation_id": gift, "action": "accept", "message": "Done."})
    out["EngineGame.negotiation:accepted"] = g.negotiation(gift)
    for _ in range(6):
        if g.current == 0:
            g.execute(0, "end_turn")
        if g.current == 1:
            out["EngineGame.play_bot_turn"] = g.play_bot_turn(1, bot, end_turn=True)
    out["EngineGame.emit"] = g.emit("agent_error", "Player 0's AI: a test of the host's events.", None, player=0)
    out["EngineGame.add_thought"] = g.add_thought(0, "Expand to the east.", "thought")
    out["EngineGame.set_difficulty"] = [g.set_difficulty(0, "deity"), g.set_difficulty(0, "impossible")]
    out["EngineGame.set_controller"] = g.set_controller(1, "llm", None, {"un_vote": True})
    out["EngineGame.debug"] = g.debug("gold")
    out["EngineGame.summary"] = g.summary()
    out["EngineGame.config"] = g.config
    out["EngineGame.player"] = g.player(1)
    out["EngineGame.player_name"] = g.player_name(1)
    out["EngineGame.is_alive"] = g.is_alive(1)
    out["EngineGame.majors"] = g.majors(alive_only=False)
    out["EngineGame.standing"] = g.standing(0)
    out["EngineGame.standings"] = g.standings()
    out["EngineGame.stats"] = g.stats()
    out["EngineGame.events"] = g.events()
    out["EngineGame.event_view"] = [g.event_view(e, 1) for e in g.events(40)]
    out["EngineGame.thoughts"] = g.thoughts()
    out["EngineGame.thought_count"] = g.thought_count()
    out["EngineGame.view:player"] = g.view(0)
    out["EngineGame.view:god"] = g.view(None, event_limit=30)
    out["EngineGame.briefing"] = g.briefing(0)
    out["EngineGame.turn_progress"] = g.turn_progress(0)
    out["EngineGame.empire_summary"] = g.empire_summary(0)
    out["EngineGame.negotiations"] = g.negotiations()
    out["EngineGame.max_chat_messages"] = g.max_chat_messages()
    deals = [n["deal_id"] for n in g.negotiations() if n.get("deal_id") is not None]
    out["EngineGame.deal"] = [g.deal(d) for d in deals]
    out["EngineGame.deal:none"] = g.deal(9999)
    out["EngineGame.describe_items"] = g.describe_items([{"type": "gold", "amount": 50},
                                                         {"type": "open_borders", "turns": 30}])
    out["EngineGame.scenario_overview"] = g.scenario_overview()
    out["EngineGame.default_seats"] = g.default_seats()
    out["EngineGame.normalize_seats"] = g.normalize_seats([{"type": "hybrid", "handicap": "human",
                                                            "auto": {"un_vote": False}}])
    out["EngineGame.export_map"] = g.export_map("Parity map")
    unit, x, y = _path_target(g, 0)
    out["EngineGame.path_preview"] = [g.path_preview(0, unit, x, y), g.path_preview(1, unit, x, y)]
    out["EngineGame.force_turn"] = g.force_turn(0)
    out["EngineGame.replay_data"] = g.replay_data()
    for prop in ("turn", "current", "phase", "winner", "victory", "turn_limit"):
        out[f"EngineGame.{prop}"] = getattr(g, prop)
    save = g.to_save()
    out["state_summary"] = B.state_summary(save["state"])
    out["EngineGame.from_save"] = B.EngineGame.from_save(save).summary()
    out["EngineGame.from_state"] = B.EngineGame.from_state(g.state_dict()).summary()
    out["EngineGame.save_scenario"] = g.save_scenario(f"parity-{B.__name__.rsplit('.', 1)[-1].strip('_')}",
                                                      "Parity", "Both backends", [{"type": "llm"}])
    return out


def small_battery(B) -> dict:
    """The small game, four bot seats: played seat by seat for a few rounds, then read; and the same settings as a
    whole headless game."""
    out = {}
    g = B.EngineGame.new(dict(SMALL))
    bots = {p: B.bot_instance("basic", seed=p, aggression=0.25 + 0.15 * p) for p in range(4)}
    for _ in range(4 * 6):
        if g.phase != "playing":
            break
        g.play_bot_turn(g.current, bots[g.current], end_turn=True)
    out["EngineGame.summary:small"] = g.summary()
    out["EngineGame.view:small god"] = g.view(None, event_limit=60)
    out["EngineGame.view:small player"] = g.view(2)
    out["EngineGame.events:small"] = g.events()
    out["EngineGame.standings:small"] = g.standings()
    out["EngineGame.stats:small"] = g.stats(2)
    out["EngineGame.empire_summary:small"] = g.empire_summary(3)
    out["EngineGame.scenario_overview:small"] = g.scenario_overview()
    out["EngineGame.replay_data:small"] = g.replay_data()
    out["EngineGame.negotiations:small"] = g.negotiations()
    turns, events = [], []
    r = B.run_game({"config": dict(SMALL), "bots": {p: B.bot_instance("basic", seed=p) for p in range(4)},
                    "labels": {0: "first"}}, on_turn=turns.append, on_event=events.append)
    out["run_game"] = r
    out["run_game on_turn"] = turns
    out["run_game on_event"] = events
    return out


def module_battery(B) -> dict:
    """The names that need no game: the ruleset, the tools, maps and scenarios, the diplomacy categories."""
    out = {}
    for name in ("rules_version", "rules_client", "max_players", "map_sizes", "map_types", "speeds", "difficulties",
                 "ruleset_counts", "scenario_ops_help", "tool_list"):
        out[name] = getattr(B, name)()
    out["tool_list:query"] = B.tool_list("query")
    out["resolve_name"] = [B.resolve_name("speed", "quick"), B.resolve_name("tech", "pottery"),
                           B.resolve_name("speed", "warp"), B.resolve_name("speed", None)]
    out["tool_kind"] = [B.tool_kind("end_turn"), B.tool_kind("get_briefing"), B.tool_kind("no_such_tool")]
    for name in ("RULES_OVERVIEW", "MAP_LEGEND", "DEBUG_ACTIONS", "DIPLOMACY_CATEGORIES"):
        out[name] = getattr(B, name)
    blank = B.blank_map(20, 16, "Grassland", "Flat land")
    out["blank_map"] = blank
    out["validate_map"] = B.validate_map(blank)
    out["map_summary"] = B.map_summary(blank)
    out["generate_map"] = B.generate_map(30, 20, "pangaea", 2, 1, seed=5, name="Generated")
    tag = B.__name__.rsplit(".", 1)[-1].strip("_")
    saved = B.save_map(dict(blank, id="", name=f"Parity {tag}"))
    out["save_map"] = saved
    out["load_map"] = B.load_map(saved[0]["id"])
    out["list_maps"] = [m for m in B.list_maps() if m["id"] == saved[0]["id"]]
    out["delete_map"] = B.delete_map(saved[0]["id"])
    g = B.EngineGame.new(dict(DUEL))
    sid = g.save_scenario(f"parity-module-{tag}", "Parity", "", None)["id"]
    out["list_scenarios"] = [s for s in B.list_scenarios() if s["id"] == sid]
    scenario = B.load_scenario(sid)
    out["load_scenario"] = {k: v for k, v in scenario.items() if k != "state"}
    out["scenario_summary"] = B.scenario_summary(scenario)
    out["delete_scenario"] = B.delete_scenario(sid)
    out["item_category"] = [B.item_category({"type": "gold", "amount": 5}), B.item_category({"type": "peace_treaty"})]
    out["proposal_categories"] = [B.proposal_categories({"0": [{"type": "gold", "amount": 5}], "1": []}),
                                  B.proposal_categories(None)]
    bot = B.bot_instance("basic", seed=1)
    out["bot_set_diplomacy"] = B.bot_set_diplomacy(bot, {"trades": "llm"})
    out["bot_owns_negotiation"] = B.bot_owns_negotiation(bot, {"proposal": None})
    for exc in ("ActionError", "MapError"):
        out[exc] = issubclass(getattr(B, exc), Exception)
    out["EngineGame"] = inspect.isclass(B.EngineGame)
    return out


CONDITIONAL_KEYS = {end: keys for end, (keys, _why) in CONDITIONAL.items()}


def shape_differences(py: dict, rs: dict) -> list[str]:
    """Where the two backends' answers differ in shape, key by key, as "path: what" lines."""
    return [d for key in sorted(py) for d in compare(py[key], rs[key], key, CONDITIONAL_KEYS)]


def vacant_differences(py: dict, rs: dict) -> list[str]:
    """Where one backend's answer is vacant and the other's is not, as "path: what" lines."""
    return [d for key in sorted(py) for d in vacancies(py[key], rs[key], key)]


def _paths_of(lines: list[str]) -> set:
    # a difference reads "path: what"; a battery key may hold a colon, never a colon and a space
    return {d.split(": ", 1)[0] for d in lines}


@unittest.skipIf(RS is None, "citar._engine is not built (cargo xtask develop)")
class ParityTests(unittest.TestCase):
    """Both backends, the same calls, the same shapes."""

    @classmethod
    def setUpClass(cls):
        cls.answers = {}
        for B in (PY, RS):
            cls.answers[B] = {**module_battery(B), **duel_battery(B), **small_battery(B)}

    def test_every_answer_has_the_same_shape(self):
        self.assertEqual(sorted(self.answers[RS]), sorted(self.answers[PY]))
        found = shape_differences(self.answers[PY], self.answers[RS])
        undocumented = [d for d in found if d.split(": ", 1)[0] not in DOCUMENTED]
        self.assertEqual(undocumented, [], "the backends' answers differ in shape:\n" + "\n".join(undocumented))
        stale = sorted(set(DOCUMENTED) - _paths_of(found))
        self.assertEqual(stale, [], "documented differences that no longer differ: remove them")

    def test_no_answer_is_vacant_on_one_side_alone(self):
        found = vacant_differences(self.answers[PY], self.answers[RS])
        unlisted = [d for d in found if d.split(": ", 1)[0] not in VACANT]
        self.assertEqual(unlisted, [], "answers whose shapes compare nothing on one backend:\n" + "\n".join(unlisted))
        stale = sorted(set(VACANT) - _paths_of(found))
        self.assertEqual(stale, [], "VACANT entries that are no longer vacant on one side alone: remove them")

    def test_each_conditional_entry_names_records_the_battery_has(self):
        paths = {p for B in (PY, RS) for key, answer in self.answers[B].items()
                 for p in record_paths(shape(answer), key)}
        for end in CONDITIONAL:
            self.assertTrue(any(p.endswith(end) for p in paths), f"CONDITIONAL {end!r} names no record compared")

    def test_faults_put_back_are_caught(self):
        # Faults a Rust backend could have, put into its answers: the comparison must find each where it is.
        def faulty(change) -> set:
            rs = copy.deepcopy(self.answers[RS])
            change(rs)
            return _paths_of(shape_differences(self.answers[PY], rs)) - set(DOCUMENTED)

        def drop(keys, *answers):
            def change(rs):
                for a in answers:
                    for k in keys:
                        rs[a].pop(k)
            return change

        def drop_from_one_unit(rs):
            rs["EngineGame.view:god"]["units"][0].pop("hp")

        def drop_from_every_unit(rs):
            for u in rs["EngineGame.view:player"]["units"]:
                u.pop("hp")

        def ids_as_text(rs):
            rs["EngineGame.standings"] = {str(k): v for k, v in rs["EngineGame.standings"].items()}
        faults = {
            "a player view's own keys": (drop(("empire", "diplomacy", "alerts", "notes"), "EngineGame.view:player",
                                              "EngineGame.view:small player"), "EngineGame.view:player"),
            "a god view's own keys": (drop(("empires", "stats", "negotiations", "messages", "thoughts"),
                                           "EngineGame.view:god", "EngineGame.view:small god"), "EngineGame.view:god"),
            "a key of one unit": (drop_from_one_unit, "EngineGame.view:god.units[]"),
            "a key of every unit": (drop_from_every_unit, "EngineGame.view:player.units[]"),
            "ids as text": (ids_as_text, "EngineGame.standings"),
        }
        for name, (change, where) in faults.items():
            with self.subTest(fault=name):
                self.assertIn(where, faulty(change))

    def test_every_original_name_is_compared(self):
        called = {k.split(":")[0] for k in self.answers[PY]}
        missing = original_names() - called - set(NOT_BY_SHAPE)
        self.assertEqual(missing, set(), "names of the facade the battery never calls")
        self.assertEqual(set(NOT_BY_SHAPE) - original_names(), set(), "NOT_BY_SHAPE names no name of the facade")

    def test_documented_differences_cite_what_decided_them(self):
        ids = rulescript.intended_ids()
        for path, why in DOCUMENTED.items():
            m = re.match(r"intended ([a-z0-9-]+)", why)
            if m:
                self.assertIn(m.group(1), ids, path)
            else:
                self.assertRegex(why, r"DESIGN\.md", path)

    def test_the_constant_lists_are_equal(self):
        for name in ("DEBUG_ACTIONS", "DIPLOMACY_CATEGORIES", "map_types", "speeds", "difficulties"):
            self.assertEqual(list(self.answers[PY][name]), list(self.answers[RS][name]), name)
        for name in ("resolve_name", "tool_kind", "item_category", "proposal_categories", "max_players"):
            self.assertEqual(self.answers[PY][name], self.answers[RS][name], name)

    def test_the_phase2_names_refuse_on_python(self):
        g = PY.EngineGame.new(dict(DUEL))
        calls = [lambda: PY.build_info(), lambda: PY.bot_versions(), lambda: PY.bot_schema("basic"),
                 lambda: PY.bot_clean_params("basic", {}), lambda: PY.bot_fingerprint(PY.bot_instance("idle")),
                 lambda: g.drive({1: PY.bot_instance("idle")}), lambda: g.answer(1, 1, PY.bot_instance("idle")),
                 lambda: g.view_json(0), lambda: g.replay_json({})]
        for call in calls:
            with self.assertRaises(PY.BackendError):
                call()
        self.assertIs(PY.BackendError, RS.BackendError)
        with self.assertRaises(RS.BackendError):
            _ = RS.EngineGame.new(dict(DUEL)).python_game


@unittest.skipIf(RS is None, "citar._engine is not built (cargo xtask develop)")
class BehaviourTests(unittest.TestCase):
    """What both backends must do alike, value for value."""

    def test_the_diplomacy_switch_then_who_owns_a_negotiation(self):
        for B in (PY, RS):
            with self.subTest(backend=B.__name__):
                g = B.EngineGame.new(dict(DUEL))
                g.meet(0, 1)
                g.apply_ops([{"op": "set_player", "player": 0, "gold": 100}])
                trade = g.negotiation(g.execute(0, "open_negotiation", {
                    "to": 1, "message": "Gold for you.", "give": [{"type": "gold", "amount": 30}]})["negotiation_id"])
                g.close_negotiation(trade["id"], "expired", "(no reply in time)")   # one chat a pair at a time
                talk = g.negotiation(g.open_negotiation_as(1, 0, "Hello there.")["negotiation_id"])
                bot = B.bot_instance("basic", seed=1)

                def owns(B=B, bot=bot, trade=trade, talk=talk):
                    return B.bot_owns_negotiation(bot, trade), B.bot_owns_negotiation(bot, talk)
                self.assertEqual(owns(), (True, True))
                B.bot_set_diplomacy(bot, {"trades": "llm"})
                self.assertEqual(owns(), (False, True))
                B.bot_set_diplomacy(bot, {"chat": "llm"})
                self.assertEqual(owns(), (True, False))
                B.bot_set_diplomacy(bot, None)
                self.assertEqual(owns(), (True, True))
                for bad in ({"trade": "llm"}, {"trades": "model"}):
                    with self.assertRaises(ValueError):
                        B.bot_set_diplomacy(bot, bad)

    def test_raise_errors_raises_a_crash_and_otherwise_records_it(self):
        # Each engine's way to crash a bot: a Python bot that raises, a Rust bot that panics (test operations).
        class Crasher:
            def play_turn(self, g, pid, end_turn=False):
                raise RuntimeError("simulated bot bug")

            def respond(self, g, pid, nid):
                raise RuntimeError("simulated bot bug")

        if not RS._E.HAS_TEST_OPS:
            self.skipTest("the Rust bot's panic needs a build with the test operations")
        config = {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 8,
                  "players": [{"controller": "bot"}, {"controller": "bot"}]}
        specs = {PY: {"config": config, "bots": {0: Crasher(), 1: PY.bot_instance("idle")}},
                 RS: {"config": config, "bots": {0: RS.bot_instance("basic"), 1: RS.bot_instance("idle")},
                      "test_panic": {"player": 0, "turn": 1}}}
        for B, spec in specs.items():
            with self.subTest(backend=B.__name__):
                r = B.run_game(dict(spec, labels={0: "careless"}))
                self.assertTrue(r["errors"], "the crash is recorded")
                self.assertTrue(all(re.match(r"T\d+ P0 careless: ", e) for e in r["errors"]), r["errors"])
                with self.assertRaises(RuntimeError) as e:
                    B.run_game(dict(spec, raise_errors=True))
                self.assertNotIsInstance(e.exception, B.ActionError)

    def test_events_reach_subscribers_in_order_and_once(self):
        types = {}
        for B in (PY, RS):
            with self.subTest(backend=B.__name__):
                g = B.EngineGame.new(dict(DUEL))
                mark = len(g.events())
                heard, other = [], []
                g.subscribe(heard.append)
                g.subscribe(other.append)
                g.meet(0, 1)
                g.apply_ops([{"op": "set_player", "player": 0, "gold": 100}])
                nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Gold for you.",
                                                        "give": [{"type": "gold", "amount": 30}]})["negotiation_id"]
                g.execute(1, "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                     "message": "No, thank you."})
                g.emit("game_paused", "Game paused: a test.", None, player=0)
                g.unsubscribe(other.append)
                g.close_negotiation(g.open_negotiation_as(1, 0, "Again?")["negotiation_id"], "expired", "(gone)")
                g.execute(0, "end_turn")
                after = g.events()[mark:]
                self.assertEqual(heard, after, "each event once, in the order the game recorded them")
                self.assertEqual(len({e["id"] for e in heard}), len(heard))
                self.assertLess(len(other), len(heard), "an unsubscribed listener hears no more")
                self.assertEqual(other, heard[:len(other)])
                types[B] = [e["type"] for e in heard if e["turn"] == 1 and e["type"] != "turn_start"]
        self.assertEqual(types[PY], types[RS], "the same calls record the same kinds of event, in the same order")

    def test_a_raising_subscriber_does_not_stop_the_others(self):
        for B in (PY, RS):
            with self.subTest(backend=B.__name__):
                g = B.EngineGame.new(dict(DUEL))
                heard = []

                def bad(ev):
                    raise ValueError("a broken listener")
                g.subscribe(bad)
                g.subscribe(heard.append)
                with self.assertLogs("citar.engine", "ERROR") if B is RS else _nothing():
                    g.meet(0, 1)
                self.assertTrue(heard)


class ShapeTests(unittest.TestCase):
    """The comparison itself: what it lets pass, and what it catches."""

    def test_type_classes(self):
        self.assertEqual(compare({"a": 1}, {"a": 2.5}), [], "int and float are one number class")
        self.assertEqual(compare({"a": None}, {"a": "x"}), [], "None matches any type")
        self.assertEqual(compare({"a": []}, {"a": [{"b": 1}]}), [], "an empty collection matches any element")
        self.assertEqual(compare({"a": {}}, {"a": {"1": 2}}), [])
        self.assertTrue(compare({"a": True}, {"a": 1}), "a bool is no number")
        self.assertTrue(compare({"a": "1"}, {"a": 1}))
        self.assertTrue(compare([1, 2], {"x": 1}))

    def test_records_and_maps(self):
        self.assertTrue(compare({"a": 1, "b": 2}, {"a": 1}), "a key one side lacks")
        self.assertTrue(compare({1: {"s": 1}}, {"1": {"s": 1}}), "ids as ints against ids as text")
        self.assertEqual(compare({1: {"s": 1}}, {2: {"s": 3}, 5: {"s": 0.5}}), [], "a map's ids are its data")
        self.assertTrue(compare({1: {"s": 1}}, {2: {"t": 1}}), "a map's values are records")
        self.assertEqual(compare({"Base": 1, "Nation": 2}, {"Base": 3}), [], "a map keyed by names")
        self.assertEqual(compare([{"a": 1, "f": 2}, {"a": 1}], [{"a": 3}]), [], "a key some records lack is optional")
        self.assertEqual(compare([{"a": 3}], [{"a": 1, "f": 2}, {"a": 1}]), [], "... on the side that has it")
        self.assertTrue(compare([{"a": 1, "f": 2}], [{"a": 3}]), "a key every record has is not")
        self.assertTrue(compare([{"id": 1, "x": 1}, {"id": 2, "x": 2}], [{"id": 1, "x": 1}, {"id": 2}]),
                        "a key dropped from some records")
        self.assertEqual(compare([{"a": 1, "f": 2}, {"a": 1}], [{"a": 1, "f": 3}, {"a": 2}]), [])

    def test_each_answer_is_compared_on_its_own(self):
        # Merged into one collection, a player view's own key reads as optional (the god view lacks it), so the
        # battery compares each view with the same view on the other backend: there, a dropped key is a difference.
        player, god, bare = {"units": [], "empire": {"gold": 1}}, {"units": [], "empires": {"Rome": 1}}, {"units": []}
        self.assertEqual(compare([player, god], [bare, god]), [], "what merging the two views would let pass")
        self.assertEqual(compare(player, player), [])
        self.assertTrue(compare(player, bare), "a player view without its empire")
        self.assertTrue(compare(god, bare), "a god view without its empires")

    def test_conditional_keys(self):
        own, outside = {"id": 1, "moves": 2}, {"id": 2}
        self.assertTrue(compare([own, outside], [own]))
        self.assertEqual(compare([own, outside], [own], conditional={"[]": {"moves"}}), [])
        self.assertEqual(compare([own], [outside], conditional={"[]": {"moves"}}), [])
        self.assertTrue(compare([own], [{"x": 1}], conditional={"[]": {"moves"}}), "only the keys it names")
        self.assertTrue(compare({"u": [own]}, {"u": [outside]}, conditional={".v[]": {"moves"}}), "only where")
        self.assertEqual(compare({"y": {"gold": 1}}, {"y": {"science": 2}}, conditional={".y": "*"}), [])

    def test_vacancies(self):
        self.assertEqual(vacancies([None, {"a": 1}], [{"a": 2}]), [])
        self.assertTrue(vacancies([None, {"a": 1}], [None]), "a list of None alone compares nothing")
        self.assertTrue(vacancies({"deal_value": 3, "wants": []}, {"deal_value": None, "wants": []}))
        self.assertEqual(vacancies({"a": None, "b": 1}, {"a": None, "b": 2}), [])
        self.assertEqual(vacancies({"a": {"b": None}, "c": 1}, {"a": {"b": 2}, "c": 1}),
                         ["$.a: vacant on the first alone"])
        self.assertEqual(vacancies([1, None], [None, 1]), [], "not into a collection's elements")

    def test_tagged_records(self):
        ev = lambda t, **d: {"type": t, "data": d}  # noqa: E731
        self.assertEqual(compare([ev("a", x=1), ev("b", y=1)], [ev("a", x=2), ev("c", z=1)]), [],
                         "only the kinds both have are compared")
        self.assertTrue(compare([ev("a", x=1)], [ev("a", y=1)]))
        self.assertTrue(compare([{"type": "Warrior", "x": 1}], [{"type": "Scout", "y": 1}]),
                        "a ruleset name in type is data, not a tag")


class _nothing:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


if __name__ == "__main__":
    unittest.main()
