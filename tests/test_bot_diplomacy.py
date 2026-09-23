"""The bot's diplomacy switches (a hybrid seat's language model can own any category), its separate diplomacy random
stream, its counter-offers and the advice it can give a language model."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import random
import time
import unittest
from unittest import mock

from citar.balance import IdleBot
from citar.bots.basic import BasicBot
from citar.engine import diplomacy as D, tools
from citar.engine.game import Game

# Settings under which the bot prepares and declares war as soon as it can (a control for the war switch)
EAGER_WAR = {"war_min_turn": 0, "war_chance": 1.0, "war_power_ratio": 0, "war_power_ratio_aggr": 0,
             "war_max_threat": 100, "declare_ratio": 0, "declare_ratio_aggr": 0, "war_need_min": 1, "war_need_max": 1,
             "war_need_base": 1, "prep_gather": False}


def two_civs(**kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off", "players": [{"controller": "bot"}, {"controller": "bot"}]}
    cfg.update(kw)
    g = Game.new(cfg)
    g.meet(0, 1)
    g.player(0).gold = g.player(1).gold = 300
    return g


def offer(g, give, receive, message="A proposal."):
    """Player 0 (on its turn) proposes a deal to player 1."""
    return tools.execute(g, 0, "open_negotiation", {"to": 1, "message": message, "give": give,
                                                    "receive": receive})["negotiation_id"]


def play(g, bots, until_turn, handle=None):
    """Play bot turns headlessly. ``handle`` answers negotiations after each turn (default: every bot answers
    everything, as the lab does)."""
    from citar.sim import resolve_negotiations
    while g.s.phase == "playing" and g.turn < until_turn:
        pid = g.s.current
        bots[pid].play_turn(g, pid, end_turn=False)
        (handle or resolve_negotiations)(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)


class CategoryTests(unittest.TestCase):
    def test_every_item_type_has_a_category(self):
        self.assertEqual(set(D.ITEM_TYPES), set(D.ITEM_CATEGORY))
        self.assertTrue(set(D.ITEM_CATEGORY.values()) <= set(D.CATEGORIES))
        self.assertEqual(D.item_category({"type": "open_borders"}), "agreements")
        self.assertEqual(D.proposal_categories({"0": [{"type": "gold", "amount": 5}], "1": [{"type": "peace_treaty"}]}),
                         {"trades", "peace"})
        self.assertEqual(D.proposal_categories(None), set())

    def test_the_switch_is_checked(self):
        with self.assertRaises(ValueError):
            BasicBot(diplomacy={"trade": "llm"})
        with self.assertRaises(ValueError):
            BasicBot(diplomacy={"trades": "model"})
        bot = BasicBot(diplomacy={"trades": "llm"})
        self.assertEqual(bot.diplomacy["trades"], "llm")
        self.assertEqual(bot.diplomacy["war"], "bot")

    def test_who_owns_a_negotiation(self):
        bot = BasicBot(diplomacy={"trades": "llm"})
        gold = {"proposal": {"0": [{"type": "gold", "amount": 5}], "1": [{"type": "embassy"}]}}
        embassy = {"proposal": {"0": [{"type": "embassy"}], "1": []}}
        self.assertFalse(bot.owns_negotiation(gold))            # one LLM-owned item makes the deal the model's
        self.assertTrue(bot.owns_negotiation(embassy))
        self.assertTrue(bot.owns_negotiation({"proposal": None}))
        self.assertFalse(BasicBot(diplomacy={"chat": "llm"}).owns_negotiation({"proposal": None}))
        # a proposal of nothing on either side is talk, which the model owns with chat
        self.assertFalse(BasicBot(diplomacy={"chat": "llm"}).owns_negotiation({"proposal": {"0": [], "1": []}}))


class TradeSwitchTests(unittest.TestCase):
    def test_no_trade_offers_when_the_model_owns_trades(self):
        g = two_civs()
        params = {"lux_trade_every": 1, "lux_buy": True}
        for owners, expected in (({}, True), ({"trades": "llm"}, False)):
            with self.subTest(owners=owners):
                bot = BasicBot(seed=1, params=params, diplomacy=owners)
                bot.ex = lambda *a, **k: None
                with mock.patch.object(bot, "trade_luxuries") as lux, mock.patch.object(bot, "_buy_luxury") as buy:
                    bot.consider_diplomacy(g, 0)
                self.assertEqual(lux.called, expected)
                buy.assert_not_called()

    def test_trade_negotiations_are_left_to_the_model(self):
        for owners, answered in (({}, True), ({"trades": "llm"}, False)):
            with self.subTest(owners=owners):
                g = two_civs()
                nid = offer(g, [{"type": "gold", "amount": 10}], [{"type": "share_map"}])
                BasicBot(seed=1, diplomacy=owners).handle_negotiations(g, 1)
                n = D.get_negotiation(g, nid)
                self.assertEqual(len(n["history"]) > 1, answered)

    def test_no_gold_counter_when_the_model_owns_trades(self):
        """A counter that asks for gold is a trade: with trades owned by the model, the bot accepts or rejects."""
        for owners, action in (({}, "counter"), ({"trades": "llm"}, "reject")):
            with self.subTest(owners=owners):
                g = two_civs()
                nid = offer(g, [{"type": "declaration_of_friendship"}], [])
                bot = BasicBot(seed=1, diplomacy=owners)
                bot._war_prep[1] = {"player": 0, "since": g.turn}     # it plans war on them: friendship is costly
                bot.respond(g, 1, nid)
                self.assertEqual(D.get_negotiation(g, nid)["history"][-1]["action"], action)

    def test_a_game_with_trades_owned_by_the_model_has_no_bot_trades(self):
        g = two_civs(seed=5)
        params = {"lux_trade_every": 1, "lux_buy": True, "lux_spare_at": 1}
        hybrid = BasicBot(seed=1, params=params, diplomacy={"trades": "llm"})
        bots = {0: hybrid, 1: BasicBot(seed=2, params=params)}
        calls = []
        real = hybrid.ex

        def recording(g_, pid, tool, **args):
            calls.append((tool, args))
            return real(g_, pid, tool, **args)
        hybrid.ex = recording

        def handle(g_, bots_):
            # every bot answers only what it owns, as live seats do
            for pid, bot in bots_.items():
                bot.handle_negotiations(g_, pid)
        play(g, bots, 30, handle)
        offered = [D.item_category(it) for tool, a in calls if tool in ("open_negotiation", "respond_negotiation")
                   for it in (a.get("give") or []) + (a.get("receive") or [])]
        self.assertNotIn("trades", offered)
        for n in g.s.negotiations:
            if "trades" in D.proposal_categories(n["proposal"]):
                self.assertFalse(any(h["by"] == 0 and h["action"] == "accept" for h in n["history"]), n)


class BotAgentTests(unittest.TestCase):
    """BotAgent is how a bot answers in a live game, and a hybrid seat's model relies on it leaving the model's chats
    alone, both when a negotiation interrupts and in the wait at the end of the bot's turn."""
    def setUp(self):
        from citar.server.session import SessionManager
        self.manager = SessionManager()
        self.s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                     [{"type": "human"}, {"type": "bot"}], track=False, start=False)
        with self.s.lock:
            self.s.game.meet(0, 1)
            self.s.game.player(0).gold = 300

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def _open(self, give, receive):
        """The human opens a negotiation with the bot, straight through the engine (no interrupt thread)."""
        with self.s.lock:
            g = self.s.game
            current, g.s.current = g.s.current, 0
            nid = offer(g, give, receive)
            g.s.current = current
            return nid

    def test_an_interrupt_skips_what_the_model_owns(self):
        from citar.agents.bot_agent import BotAgent
        agent = BotAgent(seed=1)
        agent.bot.set_diplomacy({"trades": "llm"})
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        agent.respond_negotiation(self.s, 1, trade)
        self.assertEqual(len(D.get_negotiation(self.s.game, trade)["history"]), 1)
        with self.s.lock:
            tools.execute(self.s.game, 0, "respond_negotiation", {"negotiation_id": trade, "action": "reject",
                                                                  "message": "Never mind."})
        talk = self._open(None, None)                   # no proposal, and the bot still owns chat
        agent.respond_negotiation(self.s, 1, talk)
        self.assertGreater(len(D.get_negotiation(self.s.game, talk)["history"]), 1)

    def test_the_end_of_turn_wait_skips_what_the_model_owns(self):
        from citar.agents.bot_agent import BotAgent
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        with self.s.lock:
            self.s.game.s.current = 1
        agent = BotAgent(seed=1)
        agent.bot.set_diplomacy({"trades": "llm"})
        t0 = time.time()
        # only the turn's own wait loop is under test: the session's interrupt would answer (and, for a bot seat, reject
        # what nobody answered) on its own thread
        with mock.patch.object(self.s, "_dispatch_negotiation_interrupts"):
            agent.play_turn(self.s, 1)
        self.assertLess(time.time() - t0, 60)           # it did not sit out its 90-second wait on the model's chat
        n = D.get_negotiation(self.s.game, trade)
        self.assertEqual((n["status"], len(n["history"])), ("open", 1))

    def test_a_frozen_bot_answers_everything(self):
        """The archived bots predate the switch: BotAgent falls back to answering every negotiation."""
        from citar.agents.bot_agent import BotAgent
        agent = BotAgent(profile="snapshot-0922", seed=1)
        self.assertFalse(hasattr(agent.bot, "owns_negotiation"))
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        agent.respond_negotiation(self.s, 1, trade)
        self.assertGreater(len(D.get_negotiation(self.s.game, trade)["history"]), 1)


class WarSwitchTests(unittest.TestCase):
    def test_the_bot_never_declares_war_but_fights_the_war_it_is_given(self):
        def game():
            return Game.new({"map_type": "pangaea", "map_size": "duel", "seed": 1001, "barbarians": "off",
                             "players": [{"controller": "bot"}, {"controller": "bot"}], "turn_limit": 200,
                             "speed": "Quick"})
        control = game()
        play(control, {0: BasicBot(aggression=1.0, seed=1, params=EAGER_WAR), 1: IdleBot()}, 60)
        self.assertTrue(any(e["type"] == "war_declared" for e in control.s.events), "the control should go to war")

        g = game()
        bot = BasicBot(aggression=1.0, seed=1, params=EAGER_WAR, diplomacy={"war": "llm"})
        play(g, {0: bot, 1: IdleBot()}, 60)
        self.assertFalse(any(e["type"] == "war_declared" for e in g.s.events))
        self.assertEqual(bot._war_prep, {})
        # the model declares; the bot fights
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        mark = len(g.s.events)
        play(g, {0: bot, 1: IdleBot()}, 110)
        fought = [e for e in g.s.events[mark:] if e["type"] in ("combat", "city_captured")]
        self.assertTrue(fought, "the bot should attack in a war its model declared")


class RandomStreamTests(unittest.TestCase):
    def test_diplomacy_has_its_own_stream(self):
        bot = BasicBot(seed=5)
        self.assertIsInstance(bot.rng_diplo, random.Random)
        self.assertIsNot(bot.rng_diplo, bot.rng)
        self.assertEqual(bot.rng_diplo.random(), random.Random(5 * 7919 + 13).random())
        self.assertEqual(bot.rng.random(), random.Random(5).random())

    def test_handing_diplomacy_to_the_model_leaves_the_other_draws_alone(self):
        g = two_civs()
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        D.relation(g, 0, 1)["since"] = g.turn - 60                  # a long war: the bot rolls for a peace offer
        both = [BasicBot(seed=3), BasicBot(seed=3, diplomacy={"peace": "llm"})]
        for bot in both:
            bot.ex = lambda *a, **k: None
            bot.consider_diplomacy(g, 0)
        self.assertEqual(both[0].rng.getstate(), both[1].rng.getstate())
        self.assertNotEqual(both[0].rng_diplo.getstate(), both[1].rng_diplo.getstate())


class CounterTests(unittest.TestCase):
    def test_a_counter_adds_to_the_gold_already_on_the_table(self):
        g = two_civs()
        # from the bot's side: 10 gold for its map, which it values at 20
        nid = offer(g, [{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        BasicBot(seed=1).respond(g, 1, nid)
        n = D.get_negotiation(g, nid)
        self.assertEqual(n["history"][-1]["action"], "counter")
        gold = [it for it in n["proposal"]["0"] if it["type"] == "gold"]
        self.assertEqual(gold, [{"type": "gold", "amount": 30}])        # 10 + (shortfall 10 + margin 10)
        self.assertTrue(n["history"][-1]["message"])

    def test_a_counter_never_asks_for_more_gold_than_they_hold(self):
        """The rules check only the counterer's side of a counter, so an ask the other side cannot pay would fail only
        when they accepted it. The bot asks for what closes the gap within their treasury, or rejects."""
        for treasury, asked in ((200, 130), (125, 125), (100, None)):
            with self.subTest(treasury=treasury):
                g = two_civs()
                g.player(0).gold = treasury
                nid = offer(g, [{"type": "gold", "amount": 80}], [{"type": "share_map"}])
                bot = BasicBot(seed=1)
                with mock.patch.object(bot, "evaluate", return_value=-40):     # 40 short; the margin makes it 50
                    bot.respond(g, 1, nid)
                n = D.get_negotiation(g, nid)
                if asked is None:
                    self.assertEqual((n["status"], n["history"][-1]["action"]), ("rejected", "reject"))
                    continue
                self.assertEqual(n["history"][-1]["action"], "counter")
                self.assertEqual(n["proposal"]["0"], [{"type": "gold", "amount": asked}])
                tools.execute(g, 0, "respond_negotiation", {"negotiation_id": nid, "action": "accept", "message": "Done."})
                self.assertEqual(n["status"], "accepted")

    def test_the_bot_stops_countering_after_counter_rounds(self):
        g = two_civs()
        bot = BasicBot(seed=1, params={"counter_rounds": 2})
        nid = offer(g, [{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        for _ in range(2):
            bot.respond(g, 1, nid)
            self.assertEqual(D.get_negotiation(g, nid)["history"][-1]["action"], "counter")
            tools.execute(g, 0, "respond_negotiation", {"negotiation_id": nid, "action": "counter", "message": "No.",
                                                        "give": [{"type": "gold", "amount": 10}],
                                                        "receive": [{"type": "share_map"}]})
        bot.respond(g, 1, nid)
        n = D.get_negotiation(g, nid)
        self.assertEqual((n["status"], n["history"][-1]["action"]), ("rejected", "reject"))
        self.assertTrue(n["history"][-1]["message"])

    def test_our_offer_stands_once_then_the_bot_rejects(self):
        g = two_civs()
        bot = BasicBot(seed=1)
        nid = offer(g, [{"type": "gold", "amount": 10}], [{"type": "share_map"}], "From the bot.")
        g.s.current = 1                         # the other side answers with words alone, twice
        tools.execute(g, 1, "respond_negotiation", {"negotiation_id": nid, "action": "reply", "message": "Hmm."})
        bot.respond(g, 0, nid)
        self.assertEqual(D.get_negotiation(g, nid)["history"][-1]["message"], "Our offer stands.")
        tools.execute(g, 1, "respond_negotiation", {"negotiation_id": nid, "action": "reply", "message": "Hmm!"})
        bot.respond(g, 0, nid)
        n = D.get_negotiation(g, nid)
        self.assertEqual((n["status"], n["history"][-1]["action"]), ("rejected", "reject"))

    def test_the_bot_settles_its_chats_before_ending_its_own_turn(self):
        g = two_civs()
        bot = BasicBot(seed=1)
        nid = offer(g, [{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        bot._settle_chats(g, 0)
        self.assertEqual(D.get_negotiation(g, nid)["status"], "rejected")
        tools.execute(g, 0, "end_turn", {})


class AdviceTests(unittest.TestCase):
    def test_advice_is_plain_data_and_changes_nothing(self):
        g = two_civs()
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        g.s.current = 1
        nid = tools.execute(g, 1, "open_negotiation", {"to": 0, "message": "Peace?", "give": [{"type": "peace_treaty"}],
                                                       "receive": [{"type": "gold", "amount": 50}]})["negotiation_id"]
        bot = BasicBot(seed=1)
        before = json.dumps(g.s.to_dict(), sort_keys=True, default=str)
        advice = bot.advice(g, 0, nid)
        self.assertEqual(json.dumps(g.s.to_dict(), sort_keys=True, default=str), before)
        self.assertEqual((bot._war_prep, bot._war_plan), ({}, {}))
        json.dumps(advice)                                    # plain data all the way down
        self.assertEqual(set(advice), {"deal_value", "war_readiness", "spare_luxuries", "wants"})
        n = D.get_negotiation(g, nid)
        expected = bot.evaluate(g, 0, 1, n["proposal"]["0"], n["proposal"]["1"])
        self.assertAlmostEqual(advice["deal_value"], expected, places=1)
        self.assertEqual([w["player"] for w in advice["war_readiness"]], [1])
        self.assertTrue(advice["war_readiness"][0]["at_war"])
        self.assertIsInstance(advice["war_readiness"][0]["power_ratio"], float)
        self.assertIsNone(bot.advice(g, 0)["deal_value"])


if __name__ == "__main__":
    unittest.main()
