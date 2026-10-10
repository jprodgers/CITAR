"""A bot seat's diplomacy in a live session: BotAgent answers through ``EngineGame.answer`` and leaves alone what the
seat's language model owns (a hybrid seat's switches), both when a negotiation interrupts and at the end of its turn.

The bot's own diplomacy (its switches, counters, offers, advice and random streams) is tested in the rule scripts
(tests/rules/bot_*.toml) and crates/citar-testkit/tests/bot/."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import time
import unittest
from unittest import mock


class BotAgentTests(unittest.TestCase):
    """BotAgent is how a bot answers in a live game, and a hybrid seat's model relies on it leaving the model's chats
    alone, both when a negotiation interrupts and in the wait at the end of the bot's turn."""
    def setUp(self):
        from citar.server.session import SessionManager
        self.manager = SessionManager()
        self.s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                     [{"type": "human"}, {"type": "bot"}], track=False, start=False)
        with self.s.lock:
            self.s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": 0, "gold": 300}])

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def _open(self, give, receive):
        """The human opens a negotiation with the bot, straight through the engine (no interrupt thread)."""
        with self.s.lock:
            return self.s.game.open_negotiation_as(0, 1, "A proposal.", give, receive)["negotiation_id"]

    def test_an_interrupt_skips_what_the_model_owns(self):
        from citar import engine_api
        from citar.agents.bot_agent import BotAgent
        agent = BotAgent()
        engine_api.bot_set_diplomacy(agent.bot, {"trades": "llm"})
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        version = self.s.version
        agent.respond_negotiation(self.s, 1, trade)       # EngineGame.answer: left to the model, so nothing moved
        self.assertEqual(len(self.s.game.negotiation(trade)["history"]), 1)
        self.assertEqual(self.s.version, version)
        with self.s.lock:
            self.s.game.execute(0, "respond_negotiation", {"negotiation_id": trade, "action": "reject",
                                                           "message": "Never mind."})
        talk = self._open(None, None)                   # no proposal, and the bot still owns chat
        agent.respond_negotiation(self.s, 1, talk)
        self.assertGreater(len(self.s.game.negotiation(talk)["history"]), 1)
        self.assertGreater(self.s.version, version, "an answer goes through the session's side effects")
        # an answer to a chat that no longer waits on the seat is refused by the engine, which is no error
        agent.respond_negotiation(self.s, 1, talk)

    def test_the_end_of_turn_wait_skips_what_the_model_owns(self):
        from citar import engine_api
        from citar.agents.bot_agent import BotAgent
        with self.s.lock:
            self.s.game.execute(0, "end_turn", {})         # the bot's turn
        self.assertEqual(self.s.game.current, 1)
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        agent = BotAgent()
        # the model owns the trade, and the agreements, so that the bot opens no chat of its own with the human
        engine_api.bot_set_diplomacy(agent.bot, {"trades": "llm", "agreements": "llm"})
        self.s.agents[1] = agent
        t0 = time.time()
        # only the turn's own wait loop is under test: the session's interrupt would answer (and, for a bot seat, reject
        # what nobody answered) on its own thread
        with mock.patch.object(self.s, "_dispatch_negotiation_interrupts"):
            agent.play_turn(self.s, 1)
        self.assertLess(time.time() - t0, 60)           # it did not sit out its 90-second wait on the model's chat
        n = self.s.game.negotiation(trade)
        self.assertEqual((n["status"], len(n["history"])), ("open", 1))
        # the drive played the bot's turn and stopped on the chat it left to the host: the turn is still the bot's,
        # for the session's driver to close the chat and end
        self.assertEqual((self.s.game.current, self.s.game.end_turn_refusal(1) is not None), (1, True))
        rec = self.s.metrics.current(1)
        self.assertIsNotNone(rec)
        self.assertTrue(rec.get("bot_actions"), "the drive's action counts are on the bot's turn row")

    def test_the_driver_ends_a_turn_its_models_chat_held_open(self):
        """The session's driver closes the chat the bot left to its model and ends the turn, as for any turn an agent
        leaves open (a session seat has no model of its own until Phase 3's hybrid seats)."""
        from citar import engine_api
        with self.s.lock:
            self.s.game.execute(0, "end_turn", {})
        trade = self._open([{"type": "gold", "amount": 10}], [{"type": "share_map"}])
        engine_api.bot_set_diplomacy(self.s.get_agent(1).bot, {"trades": "llm", "agreements": "llm"})
        with mock.patch.object(self.s, "_dispatch_negotiation_interrupts"):
            self.s.start()
            t0 = time.time()
            while time.time() - t0 < 30 and (self.s.game.turn, self.s.game.current) != (2, 0):
                time.sleep(0.05)
            self.s.set_paused(True)
        self.assertEqual((self.s.game.turn, self.s.game.current), (2, 0), self.s.errors)
        n = self.s.game.negotiation(trade)
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", "(no reply in time)"))
        rec = next(r for r in self.s.metrics.data["turns"] if (r["player"], r["turn"]) == (1, 1))
        self.assertEqual(rec["end_reason"], "end_turn")
        self.assertEqual(self.s.errors, [])


if __name__ == "__main__":
    unittest.main()
