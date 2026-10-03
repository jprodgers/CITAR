"""The engine facade: what it hands out is plain data, saves round-trip through it, and headless games run on it."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar import engine_api
from citar.engine_api import ActionError, EngineGame
from tests.backends import RUST, has_test_ops, python_engine_only, rust_only


def duel(**kw) -> EngineGame:
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off", "players": [{"controller": "human"}, {"controller": "bot"}]}
    cfg.update(kw)
    return EngineGame.new(cfg)


class PlainDataTests(unittest.TestCase):
    def test_negotiations_are_copies(self):
        g = duel()
        g.meet(0, 1)
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Hello."})["negotiation_id"]
        n = g.negotiation(nid)
        n["status"] = "accepted"
        n["history"].clear()
        g.open_negotiations()[0]["history"].append({"forged": True})
        again = g.negotiation(nid)
        self.assertEqual((again["status"], len(again["history"])), ("open", 1))
        self.assertEqual([m["id"] for m in g.open_negotiations(1)], [nid])
        self.assertEqual(g.open_negotiations(1), g.negotiations(1))
        with self.assertRaises(ActionError):
            g.negotiation(99)

    def test_player_rows_and_the_controller(self):
        g = duel()
        row = g.player(1)
        self.assertEqual((row["controller"], row["handicap"], row["auto"]["conquest"]), ("bot", "ai", True))
        row["auto"]["conquest"] = False                   # a copy: the game is untouched
        self.assertTrue(g.player(1)["auto"]["conquest"])
        g.set_controller(1, "llm")
        self.assertEqual((g.player(1)["controller"], g.player(1)["handicap"]), ("llm", "human"))
        self.assertEqual([p["id"] for p in g.majors()], [0, 1])
        self.assertTrue(g.set_difficulty(0, "deity"))
        self.assertEqual(g.player(0)["difficulty"], "Deity")
        self.assertFalse(g.set_difficulty(0, "impossible"))

    def test_summary_and_standings(self):
        g = duel()
        s = g.summary()
        self.assertEqual((s["turn"], s["current"], s["phase"]), (1, 0, "playing"))
        self.assertEqual(s["turn_limit"], g.turn_limit)
        st = g.standings()
        self.assertEqual(set(st), {0, 1})
        self.assertEqual(set(st[0]), {"score", "cities", "units", "population", "techs", "gold", "era"})

    def test_config_is_a_copy(self):
        g = duel()
        g.config["seed"] = 999
        self.assertEqual(g.config["seed"], 21)

    def test_negotiation_views_and_the_empire_are_copies(self):
        g = duel()
        g.meet(0, 1)
        g.apply_ops([{"op": "set_player", "player": 0, "gold": 100}])
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Gold for you.",
                                                "give": [{"type": "gold", "amount": 30}]})["negotiation_id"]
        view = g.negotiation_view(nid, 0)
        view["current_proposal"]["you_give"][0]["amount"] = 1
        self.assertEqual(g.negotiation(nid)["proposal"]["0"][0]["amount"], 30)
        emp = g.empire_summary(0)
        total = emp["happiness"]["total"]
        emp["happiness"]["total"] = -99
        self.assertEqual(g.empire_summary(0)["happiness"]["total"], total)

    def test_negotiation_heads(self):
        g = duel()
        g.meet(0, 1)
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Hello."})["negotiation_id"]
        head = {"id": nid, "initiator": 0, "responder": 1, "status": "open", "awaiting": 1, "entries": 1}
        self.assertEqual(g.negotiation_head(nid), head)
        self.assertEqual(g.open_negotiation_heads(), [head])
        self.assertEqual(g.open_negotiation_heads(1), [head])
        g.close_negotiation(nid, "expired", "(no reply in time)")
        self.assertEqual(g.open_negotiation_heads(), [])
        self.assertEqual((g.negotiation_head(nid)["status"], g.negotiation_head(nid)["entries"]), ("expired", 2))
        with self.assertRaises(ActionError):
            g.negotiation_head(99)


class SaveTests(unittest.TestCase):
    def test_a_save_round_trips(self):
        g = duel()
        g.execute(0, "end_turn")
        data = g.to_save()
        back = EngineGame.from_save(data)
        self.assertEqual(back.summary(), g.summary())
        self.assertEqual(back.view(0)["tiles"], g.view(0)["tiles"])

    def test_state_summary_reads_a_save(self):
        g = duel()
        for _ in range(4):
            g.execute(g.current, "end_turn")
        summ = engine_api.state_summary(g.to_save()["state"])
        self.assertEqual((summ["turn"], summ["phase"], summ["map_size"]), (g.turn, "playing", "duel"))
        self.assertEqual((summ["winner"], summ["winner_id"]), (None, None))
        self.assertEqual(summ["names"][1], g.player_name(1))
        self.assertEqual([m["id"] for m in summ["majors"]], [0, 1])
        last = g.stats(1)[0]["players"]
        self.assertEqual(summ["scores"], {pid: last[str(pid)]["score"] for pid in (0, 1)})
        self.assertEqual(engine_api.state_summary(duel().to_save()["state"])["scores"], {}, "no stats row yet")

    def test_state_dict_is_independent(self):
        g = duel()
        copy = EngineGame.from_state(g.state_dict())
        copy.apply_ops([{"op": "set_player", "player": 0, "gold": 777}])
        self.assertEqual(copy.standing(0)["gold"], 777)
        self.assertNotEqual(g.standing(0)["gold"], 777)


class ToolAndOpTests(unittest.TestCase):
    def test_tool_kinds_and_schemas(self):
        self.assertEqual((engine_api.tool_kind("end_turn"), engine_api.tool_kind("get_briefing")), ("action", "query"))
        self.assertIsNone(engine_api.tool_kind("no_such_tool"))
        names = {t["name"] for t in engine_api.tool_list()}
        self.assertIn("respond_negotiation", names)
        self.assertTrue(all("input_schema" in t for t in engine_api.tool_list("query")))

    def test_debug_and_path_preview(self):
        g = duel()
        with self.assertRaises(ValueError):
            g.debug("free_victory")
        g.debug("gold")
        self.assertEqual(g.standing(0)["gold"], 500)
        theirs = g.view(1)["units"][0]
        self.assertEqual(g.path_preview(0, theirs["id"], 0, 0), {"path": None})

    def test_a_probe_opens_a_negotiation_out_of_turn(self):
        g = duel()
        g.meet(0, 1)
        r = g.open_negotiation_as(1, 0, "Peace?")
        self.assertEqual(g.current, 0, "the turn is only lent for the call")
        self.assertEqual(g.negotiation(r["negotiation_id"])["initiator"], 1)


class BotTests(unittest.TestCase):
    def test_bot_instances(self):
        idle = engine_api.bot_instance("idle")
        bot = engine_api.bot_instance("basic", seed=3, aggression=0.7, params={"counter_rounds": 1})
        self.assertEqual(bot.aggression, 0.7)
        if RUST:
            self.assertEqual((idle.version, bot.version), ("idle", "basic-1"))
        else:
            self.assertEqual((type(idle).__name__, bot.p["counter_rounds"]), ("IdleBot", 1))
        for bad in ("os", "citar.engine", "../basic", ""):
            with self.assertRaises(ValueError):
                engine_api.bot_instance(bad)

    @rust_only
    def test_bots_are_compiled_versions(self):
        # A bot is a version and its parameters (DESIGN.md P2.8.5): "basic" names the latest, which carries the
        # overrides cleaned against its schema; the frozen snapshots were archived with 0.1.5.
        versions = engine_api.bot_versions()
        latest = next(v["id"] for v in versions if v["latest"])
        bot = engine_api.bot_instance("basic", params={"counter_rounds": 1})
        self.assertEqual(bot.version, latest)
        self.assertEqual(engine_api.bot_instance(latest).version, latest)
        self.assertEqual(engine_api.bot_clean_params(latest, {"counter_rounds": 1}), {"counter_rounds": 1})
        self.assertEqual(engine_api.bot_schema()["engine"], latest)
        for frozen in ("frozen_d95d50cb", "snapshot-0922"):
            with self.assertRaises(ValueError):
                engine_api.bot_instance(frozen)
        with self.assertRaises(ValueError):
            engine_api.bot_instance("basic", params={"no_such_parameter": 1})
        fp = engine_api.bot_fingerprint(bot)
        self.assertRegex(fp, r"^[0-9a-f]{12}$")
        self.assertNotEqual(fp, engine_api.bot_fingerprint(engine_api.bot_instance("basic")))
        info = engine_api.build_info()
        self.assertEqual(set(info), {"version", "build_id", "label", "rules", "engine_code", "bot_code"})

    def test_the_diplomacy_switch(self):
        bot = engine_api.bot_instance("basic", seed=1)
        engine_api.bot_set_diplomacy(bot, {"trades": "llm"})
        self.assertFalse(engine_api.bot_owns_negotiation(bot, {"proposal": {"0": [{"type": "gold", "amount": 5}],
                                                                           "1": []}}))
        self.assertTrue(engine_api.bot_owns_negotiation(bot, {"proposal": None}))
        with self.assertRaises(ValueError):
            engine_api.bot_set_diplomacy(bot, {"trade": "llm"})

    @python_engine_only("tests.test_engine_api.BotTests.test_bots_are_compiled_versions")
    def test_the_frozen_bots_predate_the_switch(self):
        frozen = engine_api.bot_instance("frozen_d95d50cb", seed=1)
        self.assertTrue(engine_api.bot_owns_negotiation(frozen, {"proposal": None}))
        with self.assertRaises(ValueError):
            engine_api.bot_set_diplomacy(frozen, {"trades": "llm"})

    def test_a_headless_game_runs_on_the_facade(self):
        turns, events = [], []
        r = engine_api.run_game({"config": {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 12,
                                            "players": [{"controller": "bot"}, {"controller": "bot"}]},
                                 "bots": {0: engine_api.bot_instance("basic", seed=1),
                                          1: engine_api.bot_instance("idle")}},
                                on_turn=lambda info: turns.append(info["turn"]), on_event=events.append)
        self.assertEqual((r["phase"], r["errors"]), ("over", []))
        self.assertEqual(r["turns"], r["turn"] - 1)
        self.assertEqual(turns, sorted(set(turns)))
        self.assertEqual(turns[0], 1)
        self.assertTrue(any(e["type"] == "city_founded" for e in events))
        majors = [p for p in r["players"] if p["kind"] == "major"]
        self.assertEqual(len(majors), 2)
        self.assertTrue(all(p["cities"] >= 1 and p["score"] > 0 for p in majors))
        self.assertEqual(len(r["stats"]), r["turns"])

    @python_engine_only("tests.test_engine_api.BotTests.test_a_panicking_bot_is_recorded_or_raised")
    def test_a_crashing_bot_is_recorded_or_raised(self):
        class Crasher:
            def play_turn(self, g, pid, end_turn=False):
                raise RuntimeError("simulated bot bug")

            def respond(self, g, pid, nid):
                raise RuntimeError("simulated bot bug")

        def spec(**kw):
            return {"config": {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 4,
                               "players": [{"controller": "bot"}, {"controller": "bot"}]},
                    "bots": {0: Crasher(), 1: engine_api.bot_instance("idle")}, **kw}

        r = engine_api.run_game(spec())
        self.assertEqual(r["phase"], "over", "a crash costs the bot its turn, not the game")
        self.assertTrue(r["errors"] and all("simulated bot bug" in e for e in r["errors"]), r["errors"])
        with self.assertRaisesRegex(RuntimeError, "simulated bot bug"):
            engine_api.run_game(spec(raise_errors=True))

    @rust_only
    def test_a_panicking_bot_is_recorded_or_raised(self):
        # The Rust bot does not raise: a panic is its crash, recorded as the runner's crash line, or raised as
        # EngineCrash with raise_errors (never an ActionError: a crash is no refusal). Only Python bots are refused.
        if not has_test_ops():
            self.skipTest("the panic needs a build with the test operations")

        def spec(**kw):
            return {"config": {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 8,
                               "players": [{"controller": "bot"}, {"controller": "bot"}]},
                    "bots": {0: engine_api.bot_instance("basic"), 1: engine_api.bot_instance("idle")},
                    "test_panic": {"player": 0, "turn": 3}, **kw}

        r = engine_api.run_game(spec(labels={0: "careless"}))
        self.assertEqual(r["phase"], "playing", "a crash ends a Rust game where it stands")
        self.assertEqual(len(r["errors"]), 1, r["errors"])
        self.assertTrue(r["errors"][0].startswith("T3 P0 careless: panic: "), r["errors"])
        with self.assertRaises(engine_api.EngineCrash) as e:
            engine_api.run_game(spec(raise_errors=True))
        self.assertNotIsInstance(e.exception, ActionError)
        with self.assertRaises(TypeError):
            engine_api.run_game({"config": spec()["config"], "bots": {0: object()}})


if __name__ == "__main__":
    unittest.main()
