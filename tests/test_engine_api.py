"""The engine facade: what it hands out is plain data, saves round-trip through it, and headless games run on it."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar import engine_api
from citar.engine_api import ActionError, EngineGame


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


class SaveTests(unittest.TestCase):
    def test_a_save_round_trips(self):
        g = duel()
        g.execute(0, "end_turn")
        data = g.to_save()
        back = EngineGame.from_save(data)
        self.assertEqual(back.summary(), g.summary())
        self.assertEqual(back.view(0)["tiles"], g.view(0)["tiles"])

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
        self.assertEqual(type(engine_api.bot_instance("idle")).__name__, "IdleBot")
        bot = engine_api.bot_instance("basic", seed=3, aggression=0.7, params={"counter_rounds": 1})
        self.assertEqual((bot.aggression, bot.p["counter_rounds"]), (0.7, 1))
        for bad in ("os", "citar.engine", "../basic", ""):
            with self.assertRaises(ValueError):
                engine_api.bot_instance(bad)

    def test_the_diplomacy_switch(self):
        bot = engine_api.bot_instance("basic", seed=1)
        engine_api.bot_set_diplomacy(bot, {"trades": "llm"})
        self.assertFalse(engine_api.bot_owns_negotiation(bot, {"proposal": {"0": [{"type": "gold", "amount": 5}],
                                                                           "1": []}}))
        self.assertTrue(engine_api.bot_owns_negotiation(bot, {"proposal": None}))
        with self.assertRaises(ValueError):
            engine_api.bot_set_diplomacy(bot, {"trade": "llm"})
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


if __name__ == "__main__":
    unittest.main()
