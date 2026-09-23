"""Negotiations as chats: every entry carries a message and a number, action names are normalised, a message cap
closes a chat that goes nowhere, and no one ends a turn while a chat they are in is open - with the session and the
LLM agent closing chats that time out so that a game never stalls on one."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import time
import unittest
from unittest import mock

from citar.engine.game import Game, ActionError
from citar.engine import diplomacy as D, tools
from citar.server.session import SessionManager
from tests.test_session import ScriptedConversation


def two_civs(**kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off", "players": [{"name": "Avalon"}, {"name": "Brigadoon"}]}
    cfg.update(kw)
    g = Game.new(cfg)
    g.meet(0, 1)
    g.player(0).gold = g.player(1).gold = 200
    return g


def open_chat(g, give=None, receive=None, message="Shall we talk?"):
    """Player 0 opens a negotiation with player 1 on its own turn."""
    args = {"to": 1, "message": message}
    if give is not None or receive is not None:
        args.update(give=give or [], receive=receive or [])
    return tools.execute(g, 0, "open_negotiation", args)["negotiation_id"]


def respond(g, pid, nid, action, message="Fine.", **items):
    return tools.execute(g, pid, "respond_negotiation",
                         {"negotiation_id": nid, "action": action, "message": message, **items})


class MessageTests(unittest.TestCase):
    def test_every_entry_needs_a_message_and_the_refusal_names_the_negotiation(self):
        g = two_civs()
        with self.assertRaises(ActionError):
            tools.execute(g, 0, "open_negotiation", {"to": 1, "message": "  "})
        nid = open_chat(g, give=[{"type": "gold", "amount": 20}])
        for action in ("accept", "reject", "counter", "reply"):
            with self.subTest(action=action):
                with self.assertRaises(ActionError) as cm:
                    tools.execute(g, 1, "respond_negotiation", {"negotiation_id": nid, "action": action,
                                                                "receive": [{"type": "gold", "amount": 5}]})
                self.assertIn(f"#{nid}", str(cm.exception))
                self.assertIn("message", str(cm.exception))
        self.assertEqual(D.get_negotiation(g, nid)["status"], "open")

    def test_aliases_are_stored_as_reject(self):
        for alias in ("decline", "withdraw", "end", "Reject"):
            with self.subTest(alias=alias):
                g = two_civs()
                nid = open_chat(g)
                self.assertEqual(respond(g, 1, nid, alias, "Not today.")["status"], "rejected")
                n = D.get_negotiation(g, nid)
                self.assertEqual((n["status"], n["history"][-1]["action"]), ("rejected", "reject"))

    def test_an_unknown_action_is_refused_with_the_valid_ones(self):
        g = two_civs()
        nid = open_chat(g)
        with self.assertRaises(ActionError) as cm:
            respond(g, 1, nid, "haggle")
        self.assertIn("accept, counter, reject, reply", str(cm.exception))

    def test_an_empty_counter_is_refused(self):
        g = two_civs()
        nid = open_chat(g, give=[{"type": "gold", "amount": 20}])
        with self.assertRaises(ActionError) as cm:
            respond(g, 1, nid, "counter", "Hmm.", give=[], receive=[])
        self.assertEqual(str(cm.exception), "A counter-offer needs at least one item; use reply to send only a message.")

    def test_entries_are_numbered_in_order(self):
        g = two_civs()
        nid = open_chat(g, give=[{"type": "gold", "amount": 20}], receive=[{"type": "share_map"}])
        respond(g, 1, nid, "counter", "More gold.", give=[{"type": "share_map"}], receive=[{"type": "gold", "amount": 30}])
        respond(g, 0, nid, "reply", "That is steep.")
        respond(g, 1, nid, "reply", "It is a good map.")
        respond(g, 0, nid, "accept", "Very well.")
        n = D.get_negotiation(g, nid)
        self.assertEqual([h["seq"] for h in n["history"]], [1, 2, 3, 4, 5])
        self.assertEqual([h["action"] for h in n["history"]], ["open", "counter", "reply", "reply", "accept"])
        self.assertTrue(all(h["message"] for h in n["history"]))
        view = D.negotiation_view(g, n, 1)
        self.assertEqual([h["seq"] for h in view["history"]], [1, 2, 3, 4, 5])
        self.assertEqual((view["messages"], view["max_messages"]), (5, 30))

    def test_either_side_may_withdraw_but_only_the_side_to_move_may_answer(self):
        g = two_civs()
        nid = open_chat(g, give=[{"type": "gold", "amount": 20}])
        with self.assertRaises(ActionError):
            respond(g, 0, nid, "reply", "Hello?")             # it is Brigadoon's move
        respond(g, 0, nid, "reject", "Never mind.")          # but the opener may withdraw
        self.assertEqual(D.get_negotiation(g, nid)["status"], "rejected")


class MessageCapTests(unittest.TestCase):
    def test_the_ruleset_cap_is_30_and_a_game_can_set_its_own(self):
        self.assertEqual(D.max_chat_messages(two_civs()), 30)
        self.assertEqual(D.max_chat_messages(two_civs(diplomacy={"max_chat_messages": 4})), 4)

    def test_a_chat_that_would_pass_the_cap_closes_as_expired(self):
        g = two_civs(diplomacy={"max_chat_messages": 4})
        nid = open_chat(g)
        respond(g, 1, nid, "reply", "Talk is cheap.")
        respond(g, 0, nid, "reply", "So it is.")
        respond(g, 1, nid, "reply", "Indeed.")             # four entries: at the cap, still open
        n = D.get_negotiation(g, nid)
        self.assertEqual((n["status"], len(n["history"])), ("open", 4))
        respond(g, 0, nid, "reply", "Well then.")          # the fifth would pass it
        self.assertEqual((n["status"], n["awaiting"]), ("expired", None))
        self.assertIn("limit of 4 messages", n["history"][-1]["note"])
        self.assertEqual(D.negotiation_view(g, n, 0)["history"][-1]["note"], n["history"][-1]["note"])
        with self.assertRaises(ActionError):
            respond(g, 1, nid, "reply", "Hello?")


class EndTurnTests(unittest.TestCase):
    def test_end_turn_waits_for_the_other_sides_answer(self):
        g = two_civs()
        nid = open_chat(g)
        with self.assertRaises(ActionError) as cm:
            tools.execute(g, 0, "end_turn", {})
        self.assertEqual(str(cm.exception),
                         f"You are waiting for Brigadoon to answer negotiation #{nid}. End your turn after they reply, "
                         f"or withdraw it with respond_negotiation(negotiation_id={nid}, action='reject', message=...).")
        self.assertEqual(g.s.current, 0)
        respond(g, 0, nid, "withdraw", "Another time.")
        tools.execute(g, 0, "end_turn", {})
        self.assertEqual(g.s.current, 1)

    def test_end_turn_waits_for_your_own_answer(self):
        g = two_civs()
        nid = open_chat(g, give=[{"type": "gold", "amount": 20}])
        respond(g, 1, nid, "counter", "Double it.", receive=[{"type": "gold", "amount": 40}])
        with self.assertRaises(ActionError) as cm:
            tools.execute(g, 0, "end_turn", {})
        self.assertTrue(str(cm.exception).startswith(f"Answer Brigadoon in negotiation #{nid} first"), cm.exception)
        respond(g, 0, nid, "accept", "Done.")
        tools.execute(g, 0, "end_turn", {})
        self.assertEqual(g.s.current, 1)

    def test_the_responder_cannot_end_its_turn_on_an_open_chat_either(self):
        g = two_civs()
        nid = open_chat(g)
        g.s.current = 1                                 # as if the opener's turn had ended without the tool
        with self.assertRaises(ActionError) as cm:
            tools.execute(g, 1, "end_turn", {})
        self.assertIn(f"Answer Avalon in negotiation #{nid} first", str(cm.exception))

    def test_game_end_turn_still_expires_the_openers_chats(self):
        """Headless runners call Game.end_turn directly: it keeps the old safety net rather than refusing."""
        g = two_civs()
        nid = open_chat(g)
        g.end_turn(0)
        n = D.get_negotiation(g, nid)
        self.assertEqual((g.s.current, n["status"]), (1, "expired"))
        self.assertEqual(n["history"][-1]["action"], "close")


class CloseNegotiationTests(unittest.TestCase):
    def test_close_records_a_note_and_tells_both_sides(self):
        g = two_civs()
        nid = open_chat(g)
        seen = []
        g.listeners.append(seen.append)
        D.close_negotiation(g, nid, "expired", "(no reply in time)")
        n = D.get_negotiation(g, nid)
        self.assertEqual((n["status"], n["awaiting"]), ("expired", None))
        last = n["history"][-1]
        self.assertEqual((last["seq"], last["by"], last["action"], last["note"]), (2, None, "close", "(no reply in time)"))
        ev = [e for e in seen if e["type"] == "negotiation"][-1]
        self.assertEqual((ev["players"], ev["data"]["negotiation"], ev["data"]["status"]), ([0, 1], nid, "expired"))
        self.assertIn("(no reply in time)", ev["text"])
        tools.execute(g, 0, "end_turn", {})            # nothing open any more
        with self.assertRaises(ActionError):
            D.close_negotiation(g, nid, "expired", "again")

    def test_only_closed_statuses_are_allowed(self):
        g = two_civs()
        nid = open_chat(g)
        with self.assertRaises(ActionError):
            D.close_negotiation(g, nid, "accepted", "no")
        self.assertEqual(D.get_negotiation(g, nid)["status"], "open")

    def test_war_cancels_the_chat_with_a_note(self):
        g = two_civs()
        nid = open_chat(g)
        tools.execute(g, 0, "declare_war", {"player_id": 1})
        n = D.get_negotiation(g, nid)
        self.assertEqual(n["status"], "cancelled")
        self.assertIn("declared war", n["history"][-1]["note"])


class _ChatOpener:
    """An agent that opens a chat with the next seat and returns without ending its turn."""
    cancelled = False
    last_error = None

    def play_turn(self, session, pid):
        session.call_tool(pid, "open_negotiation", {"to": 1, "message": "Anyone there?"})


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.manager = SessionManager()

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def _wait(self, s, done, seconds=30):
        deadline = time.time() + seconds
        while time.time() < deadline and not done():
            time.sleep(0.05)
        s.paused = True

    def test_the_driver_closes_an_ai_seats_open_chats_before_forcing_its_turn_to_end(self):
        s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                [{"type": "bot"}, {"type": "human"}], track=False, start=False)
        s.ai_delay = 0
        s.agents[0] = _ChatOpener()
        with s.lock:
            s.game.meet(0, 1)
        s.start()
        self._wait(s, lambda: s.game.s.current == 1)
        self.assertEqual(s.game.s.current, 1, s.errors)
        n = s.game.s.negotiations[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", "(no reply in time)"))
        self.assertEqual(s.errors, [])

    def test_an_llm_waits_for_the_answer_then_closes_the_chat_and_ends_its_turn(self):
        def opener(conv):
            return [("open_negotiation", {"to": 1, "message": "Your map for 5 gold?",
                                          "give": [{"type": "gold", "amount": 5}], "receive": [{"type": "share_map"}]})]

        ScriptedConversation.scripts = {"opener": [opener]}
        with mock.patch("citar.agents.llm_agent.make_conversation", ScriptedConversation):
            s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                    [{"type": "llm", "llm": {"provider": "mock", "script": "opener",
                                                             "negotiation_wait_seconds": 0.5}},
                                     {"type": "human"}], track=False, start=False)
            s.ai_delay = 0
            with s.lock:
                s.game.meet(0, 1)
                s.game.player(0).gold = 50
            t0 = time.time()
            s.start()
            self._wait(s, lambda: s.game.s.current == 1)
        self.assertEqual(s.game.s.current, 1, s.errors)
        self.assertGreaterEqual(time.time() - t0, 1.0)     # the open waited, and so did end_turn
        n = s.game.s.negotiations[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", "(no reply in time)"))
        first = next(r for r in s.metrics.turn_rows() if r["player"] == 0)
        self.assertEqual(first["end_reason"], "end_turn")
        self.assertEqual(s.errors, [])

    def test_a_chat_waiting_on_the_llm_goes_back_to_the_model(self):
        captured = {}

        def end_first(conv):
            return [("end_turn", {})]

        def answer(conv):
            captured["refusal"] = conv.results[-1]
            return [("respond_negotiation", {"negotiation_id": 1, "action": "reject", "message": "No, thank you."})]

        ScriptedConversation.scripts = {"answerer": [end_first, answer]}
        with mock.patch("citar.agents.llm_agent.make_conversation", ScriptedConversation):
            s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                    [{"type": "llm", "llm": {"provider": "mock", "script": "answerer",
                                                             "negotiation_wait_seconds": 0.5}},
                                     {"type": "human"}], track=False, start=False)
            s.ai_delay = 0
            with s.lock:
                g = s.game
                g.meet(0, 1)
                g.s.current = 1                         # the human opens a chat on its own turn...
                tools.execute(g, 1, "open_negotiation", {"to": 0, "message": "Peace and friendship?"})
                g.s.current = 0                         # ...which is waiting on the model when its turn comes
            s.start()
            self._wait(s, lambda: s.game.s.current == 1)
        self.assertEqual(s.game.s.current, 1, s.errors)
        _id, text, is_error = captured["refusal"]
        self.assertTrue(is_error)
        self.assertIn(f"Answer {s.game.player(1).name} in negotiation #1 first", text)
        self.assertEqual(s.game.s.negotiations[0]["status"], "rejected")


if __name__ == "__main__":
    unittest.main()
