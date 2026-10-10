"""Negotiations as chats in a live session: the session's driver and the LLM agent close the chats that time out, so
that a game never stalls on one, and an agent waiting for an answer hears it as news.

The engine's rules for chats (every entry carries a message and a number, action names are normalised, a message cap
closes a chat that goes nowhere, no one ends a turn while a chat they are in is open) are tested in the rule scripts
(tests/rules/negotiation_*.toml)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import threading
import time
import unittest
from unittest import mock

from citar.server.session import SessionManager
from tests.test_session import ScriptedConversation


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
        self._wait(s, lambda: s.game.current == 1)
        self.assertEqual(s.game.current, 1, s.errors)
        n = s.game.negotiations()[0]
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
                s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": 0, "gold": 50}])
            t0 = time.time()
            s.start()
            self._wait(s, lambda: s.game.current == 1)
        self.assertEqual(s.game.current, 1, s.errors)
        self.assertGreaterEqual(time.time() - t0, 0.5)     # the open waited...
        n = s.game.negotiations()[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", "(no reply in time)"))
        first = next(r for r in s.metrics.data["turns"] if r["player"] == 0)
        self.assertEqual(first["end_reason"], "end_turn")
        # ...and end_turn, with nothing left of that wait, closed the chat and ended the turn in one call that the
        # metrics do not hold against the model
        self.assertEqual((first["errors"], first["by_tool"]["end_turn"]["count"]), (0, 1))
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
                s.game.meet(0, 1)
                # the human opened a chat on its own turn, which is waiting on the model when its turn comes
                s.game.open_negotiation_as(1, 0, "Peace and friendship?")
            s.start()
            self._wait(s, lambda: s.game.current == 1)
        self.assertEqual(s.game.current, 1, s.errors)
        _id, text, is_error = captured["refusal"]
        self.assertTrue(is_error)
        self.assertIn(f"Answer {s.game.player_name(1)} in negotiation #1 first", text)
        self.assertEqual(s.game.negotiations()[0]["status"], "rejected")


class LLMEndTurnTests(unittest.TestCase):
    """LLMAgent._end_turn: the wait at end_turn for chats the model opened, driven directly (the model's own
    open_negotiation wait is covered above), with the other side answering from another thread."""
    def setUp(self):
        self.manager = SessionManager()

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def _session(self, wait: float, others: int = 1):
        seats = [{"type": "llm", "llm": {"provider": "mock", "negotiation_wait_seconds": wait}}]
        seats += [{"type": "human"} for _ in range(others)]
        s = self.manager.create({"map_size": "duel" if others == 1 else "small", "seed": 3, "barbarians": "off"},
                                seats, track=False, start=False)
        with s.lock:
            for q in range(1, others + 1):
                s.game.meet(0, q)
        self.session = s
        return s, s.get_agent(0)

    def _open(self, s, to=1):
        with s.lock:
            return s.game.execute(0, "open_negotiation", {"to": to, "message": "Shall we talk?"})["negotiation_id"]

    def _later(self, seconds, pid, nid, action, message):
        """The other side answers from another thread, as a person on the game screen would."""
        args = {"negotiation_id": nid, "action": action, "message": message}
        threading.Timer(seconds, lambda: self.session.call_tool(pid, "respond_negotiation", args)).start()

    def test_a_counter_during_the_wait_goes_back_to_the_model_as_news(self):
        s, agent = self._session(wait=10)
        nid = self._open(s)
        self._later(0.3, 1, nid, "reply", "Tell me more.")
        t0 = time.time()
        res = agent._end_turn(s, 0)
        self.assertLess(time.time() - t0, 5)
        self.assertFalse(res["ok"])
        self.assertIn(f"Answer {s.game.player_name(1)} in negotiation #{nid} first", res["error"])
        self.assertIn("While you waited, they answered", res["error"])
        self.assertIn("Tell me more.", res["error"])
        self.assertEqual(s.game.current, 0)
        rec = s.metrics.current(0)
        self.assertEqual((rec["errors"], rec["by_tool"]["end_turn"]["count"]), (0, 1))

    def test_an_answer_that_settles_the_chat_ends_the_turn(self):
        s, agent = self._session(wait=10)
        with s.lock:
            s.game.apply_ops([{"op": "set_player", "player": 0, "gold": 50}])
            nid = s.game.execute(0, "open_negotiation",
                                 {"to": 1, "message": "Your map for 5 gold?", "give": [{"type": "gold", "amount": 5}],
                                  "receive": [{"type": "share_map"}]})["negotiation_id"]
        self._later(0.3, 1, nid, "accept", "Deal.")
        t0 = time.time()
        res = agent._end_turn(s, 0)
        self.assertLess(time.time() - t0, 5)
        self.assertTrue(res["ok"], res)
        self.assertEqual((s.game.current, s.game.negotiation(nid)["status"]), (1, "accepted"))

    def test_each_chat_gets_what_is_left_of_its_own_wait(self):
        s, agent = self._session(wait=10, others=2)
        slow, quick = self._open(s, 1), self._open(s, 2)
        agent._chat_left[(slow, 1)] = 0.3             # most of its wait was spent inside open_negotiation
        self._later(0.8, 2, quick, "reject", "No, thank you.")
        t0 = time.time()
        res = agent._end_turn(s, 0)
        took = time.time() - t0
        self.assertTrue(res["ok"], res)
        self.assertTrue(0.7 <= took < 5, took)         # it waited for the answer, not for the rest of the ten seconds
        a, b = s.game.negotiation(slow), s.game.negotiation(quick)
        self.assertEqual((a["status"], a["history"][-1]["note"]), ("expired", "(no reply in time)"))
        self.assertEqual(b["status"], "rejected")

    def _end_turn_in_thread(self, s, agent):
        out = {}

        def run():
            try:
                out["result"] = agent._end_turn(s, 0)
            except Exception as e:
                out["raised"] = e
        t = threading.Thread(target=run, daemon=True)
        t.start()
        return t, out

    def test_a_seat_change_during_the_wait_hands_the_turn_over_at_once(self):
        from citar.agents.llm_agent import _Halted
        s, agent = self._session(wait=60)
        nid = self._open(s)
        t, out = self._end_turn_in_thread(s, agent)
        time.sleep(0.3)
        t0 = time.time()
        s.update_seat(0, type="human")
        t.join(5)
        self.assertFalse(t.is_alive())
        self.assertLess(time.time() - t0, 3)
        self.assertIsInstance(out.get("raised"), _Halted)
        self.assertEqual(s.game.negotiation(nid)["status"], "open")   # the new controller's to settle

    def test_a_pause_stops_the_clock_on_the_wait(self):
        s, agent = self._session(wait=0.5)
        nid = self._open(s)
        s.set_paused(True)
        t, out = self._end_turn_in_thread(s, agent)
        time.sleep(1.5)
        self.assertTrue(t.is_alive())                   # well past the wait, but the game is paused
        self.assertEqual((s.game.negotiation(nid)["status"], s.game.current), ("open", 0))
        s.set_paused(False)
        t.join(5)
        self.assertFalse(t.is_alive())
        self.assertTrue(out["result"]["ok"], out)
        self.assertEqual(s.game.negotiation(nid)["status"], "expired")
        self.assertEqual(s.game.current, 1)


class WaitForTurnTests(unittest.TestCase):
    """An agent playing over HTTP or MCP waits for the other side's answer with wait_for_turn, not by polling."""
    def setUp(self):
        self.manager = SessionManager()

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def test_on_your_own_turn_it_waits_for_the_answer(self):
        s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                [{"type": "mcp"}, {"type": "human"}], track=False, start=False)
        with s.lock:
            s.game.meet(0, 1)

        def later(action, message):
            threading.Timer(0.3, lambda: s.call_tool(1, "respond_negotiation", {
                "negotiation_id": nid, "action": action, "message": message})).start()

        self.assertEqual(s.wait_for_turn(0, 1)["status"], "your_turn")          # nothing open: at once
        nid = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Hello?"})["result"]["negotiation_id"]
        t0 = time.time()
        res = s.wait_for_turn(0, 1)
        self.assertGreaterEqual(time.time() - t0, 0.9)
        self.assertEqual((res["status"], res["negotiation_ids"], res["your_turn"]), ("waiting_for_reply", [nid], True))
        self.assertIn("wait_for_turn again", res["note"])
        later("reply", "Hello yourself.")                                     # an answer that wants one back
        self.assertEqual(s.wait_for_turn(0, 10)["status"], "negotiation")
        s.call_tool(0, "respond_negotiation", {"negotiation_id": nid, "action": "reply", "message": "Trade?"})
        later("reject", "No.")                                                # an answer that ends it
        res = s.wait_for_turn(0, 10)
        self.assertEqual((res["status"], res["negotiation_ids"], res["your_turn"]), ("negotiation_update", [nid], True))
        self.assertTrue(s.call_tool(0, "end_turn", {})["ok"])


if __name__ == "__main__":
    unittest.main()
