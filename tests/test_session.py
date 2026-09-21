"""Session-level tests: AI seats, negotiation interrupts between agents, saves. Uses a scripted mock model."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import time
import unittest
from unittest import mock

from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.server.session import SessionManager, GameSession, load_save_file


class ScriptedConversation(Conversation):
    """Plays a fixed script of tool calls, then ends the turn."""
    scripts: dict = {}

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.tools = {t["name"] for t in tools}
        self.steps = list(self.scripts.get(cfg.get("script"), []))
        self.results = []
        self.usage = {"input_tokens": 1, "output_tokens": 1}
        self.user_texts = []

    def add_user_text(self, text):
        self.user_texts.append(text)

    def add_tool_results(self, results):
        self.results.extend(results)

    def step(self):
        if self.steps:
            fn = self.steps.pop(0)
            calls = fn(self)
            return StepResult(text="thinking about it", tool_calls=[ToolCall(f"c{i}", n, a) for i, (n, a) in enumerate(calls)])
        if "end_turn" in self.tools:
            return StepResult(text="done", tool_calls=[ToolCall("end", "end_turn", {})])
        return StepResult(text="ok", tool_calls=[])


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.manager = SessionManager()

    def tearDown(self):
        import shutil
        from citar.server.session import SAVE_DIR
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def test_llm_seat_plays_and_negotiates_with_bot(self):
        captured = {}

        def first(conv):
            return [("get_briefing", {})]

        def act(conv):
            return [("set_civ_name", {"name": "Mockonia", "leader": "Test Model"})]

        def negotiate(conv):
            return [("open_negotiation", {"to": 1, "message": "Your map for 5 gold?",
                                          "give": [{"type": "gold", "amount": 5}], "receive": [{"type": "share_map"}]})]

        def record(conv):
            captured["results"] = list(conv.results)
            return [("log_thought", {"text": "Negotiated with the bot."})]

        ScriptedConversation.scripts = {"p0": [first, act, negotiate, record]}
        with mock.patch("citar.agents.llm_agent.make_conversation", ScriptedConversation):
            s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                    [{"type": "llm", "llm": {"provider": "mock", "script": "p0", "negotiation_wait_seconds": 20}},
                                     {"type": "bot"}])
            s.ai_delay = 0
            with s.lock:
                s.game.meet(0, 1)
                s.game.player(0).gold = 50       # UnCiv civs start with no gold
            # Wait for what the assertions below actually need, not for a turn number. The turn
            # counter moves when the turn ends, which is not the same moment as the script for
            # that turn having run: on a fast machine this loop saw turn 3 and paused the game
            # before the negotiation script had been called at all, and the test failed with an
            # empty negotiation list.
            def ready():
                negotiations = s.game.s.negotiations
                return (s.game.turn >= 3 and negotiations and negotiations[0]["status"] != "open"
                        and captured.get("results"))

            deadline = time.time() + 60
            while time.time() < deadline and not ready():
                time.sleep(0.1)
            s.paused = True
        self.assertGreaterEqual(s.game.turn, 3, s.errors)
        self.assertEqual(s.game.player(0).name, "Mockonia")
        negs = s.game.s.negotiations
        self.assertTrue(negs, "negotiation should have been opened")
        self.assertIn(negs[0]["status"], ("accepted", "rejected", "expired"))
        results_text = " ".join(r[1] for r in captured.get("results", []))
        self.assertIn("negotiation", results_text)
        self.assertTrue(any(t["player"] == 0 for t in s.game.s.thoughts))
        self.assertEqual(s.errors, [])

    def test_loop_guard_and_metrics(self):
        """A model that repeats itself gets blocked repeats, a stall nudge and a forced end; metrics record it all."""
        def found(conv):
            return [("set_civ_name", {"name": "Loopia"}), ("found_city", {"unit_id": 1, "name": "Loop City"})]

        def again(conv):
            return [("set_civ_name", {"name": "Loopia"}), ("found_city", {"unit_id": 1, "name": "Loop City"})]

        def look(conv):
            return [("get_briefing", {})]

        script = [found, again, again] + [look] * 30
        ScriptedConversation.scripts = {"loop": script}
        with mock.patch("citar.agents.llm_agent.make_conversation", ScriptedConversation):
            s = self.manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                    [{"type": "llm", "llm": {"provider": "mock", "script": "loop", "stall_steps": 6}},
                                     {"type": "bot"}])
            deadline = time.time() + 30
            while time.time() < deadline and s.game.turn < 2:
                time.sleep(0.1)
            s.paused = True
        rep = s.metrics_report()
        seat = rep["summary"][0]
        self.assertGreaterEqual(seat["turns"], 1, rep)
        first = rep["turns"][0]
        self.assertEqual(first["end_reason"], "stalled")
        self.assertEqual(first["blocked_repeats"], 4)      # both repeated actions blocked on each of the 2 retries
        self.assertGreaterEqual(first["repeats"], 4)
        self.assertEqual(first["stall_nudges"], 1)
        self.assertGreater(seat["tools"]["get_briefing"]["count"], 3)
        self.assertEqual(s.game.player(0).name, "Loopia")
        self.assertEqual(len(s.game.player_cities(0)), 1)

    def test_closing_game_aborts_ai_turn(self):
        import threading
        started = threading.Event()

        class SlowConversation(ScriptedConversation):
            def __init__(self, *a, **k):
                super().__init__(*a, **k)
                self.closed = threading.Event()

            def step(self):
                started.set()
                if self.closed.wait(30):   # simulates a long model request that close() aborts
                    raise RuntimeError("client closed")
                return super().step()

            def close(self):
                self.closed.set()

        ScriptedConversation.scripts = {}
        with mock.patch("citar.agents.llm_agent.make_conversation", SlowConversation):
            s = self.manager.create({"map_size": "duel", "seed": 4}, [{"type": "llm", "llm": {"provider": "mock"}}, {"type": "bot"}])
            self.assertTrue(started.wait(10), "AI turn should have started")
            t0 = time.time()
            self.manager.delete(s.id)
            s._driver.join(10)
            self.assertFalse(s._driver.is_alive(), "driver thread should exit promptly")
            self.assertLess(time.time() - t0, 5)
        self.assertEqual(s.game.turn, 1)
        self.assertEqual(s.errors, [])
        self.assertFalse(s.call_tool(0, "end_turn", {})["ok"])
        import shutil
        from citar.server.session import SAVE_DIR
        shutil.rmtree(SAVE_DIR / s.id, ignore_errors=True)

    def test_turn_time_limit_bounds_a_hanging_model_request(self):
        seen = []

        class APITimeoutError(Exception):
            pass

        class HangingConversation(ScriptedConversation):
            def step(self):
                seen.append(self.request_timeout)
                time.sleep(self.request_timeout)   # the model is still generating when the request times out
                raise APITimeoutError("Request timed out.")

        ScriptedConversation.scripts = {}
        with mock.patch("citar.agents.llm_agent.make_conversation", HangingConversation):
            t0 = time.time()
            s = self.manager.create({"map_size": "duel", "seed": 4},
                                    [{"type": "llm", "llm": {"provider": "mock", "max_turn_seconds": 7}}, {"type": "bot"}])
            while time.time() - t0 < 30 and s.game.turn < 2:
                time.sleep(0.1)
            self.assertGreaterEqual(s.game.turn, 2, "the turn should end when its time budget runs out")
            self.assertLess(time.time() - t0, 15, "a timed-out request must not be retried past the turn limit")
        self.manager.delete(s.id)                   # stop its next turn, which has already started
        self.assertLessEqual(seen[0], 7)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec["end_reason"], "time_limit")
        self.assertEqual(s.errors, [])

    def _wait(self, cond, timeout=30):
        t0 = time.time()
        while time.time() - t0 < timeout and not cond():
            time.sleep(0.1)
        return cond()

    def test_short_disconnect_is_ridden_out(self):
        """A server that drops for a few seconds costs a wait, not a turn."""
        up_at = time.time() + 2.5

        class FlakyConversation(ScriptedConversation):
            def step(self):
                if time.time() < up_at:
                    raise ConnectionError("connection refused")
                return super().step()

        ScriptedConversation.scripts = {}
        with mock.patch("citar.agents.llm_agent.make_conversation", FlakyConversation):
            s = self.manager.create({"map_size": "duel", "seed": 4, "on_disconnect": "skip", "reconnect_seconds": 30},
                                    [{"type": "llm", "llm": {"provider": "mock"}}, {"type": "bot"}])
            self.assertTrue(self._wait(lambda: s.game.turn >= 2), "the turn should finish once the server is back")
        self.manager.delete(s.id)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec["end_reason"], "end_turn")
        self.assertFalse(any(e["type"] == "agent_error" for e in s.game.s.events))

    def test_disconnect_skip_policy_skips_after_the_window(self):
        class DownConversation(ScriptedConversation):
            def step(self):
                raise ConnectionError("connection refused")

        ScriptedConversation.scripts = {}
        with mock.patch("citar.agents.llm_agent.make_conversation", DownConversation):
            t0 = time.time()
            s = self.manager.create({"map_size": "duel", "seed": 4, "on_disconnect": "skip", "reconnect_seconds": 2},
                                    [{"type": "llm", "llm": {"provider": "mock"}}, {"type": "bot"}])
            self.assertTrue(self._wait(lambda: s.game.turn >= 2))
            self.assertGreaterEqual(time.time() - t0, 2, "the turn must not be skipped before the window is over")
        self.manager.delete(s.id)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec["end_reason"], "disconnected")
        self.assertTrue(any(e["type"] == "agent_error" and "unreachable" in e["text"] for e in s.game.s.events))

    def test_disconnect_pause_policy_pauses_and_resumes(self):
        server_up = {"v": False}

        class SwitchableConversation(ScriptedConversation):
            def step(self):
                if not server_up["v"]:
                    raise ConnectionError("connection refused")
                return super().step()

        ScriptedConversation.scripts = {}
        with mock.patch("citar.agents.llm_agent.make_conversation", SwitchableConversation), \
                mock.patch("citar.agents.llm_agent.reachable", lambda cfg, timeout=5.0: server_up["v"]), \
                mock.patch.object(GameSession, "RECONNECT_POLL_SECONDS", 0.3):
            s = self.manager.create({"map_size": "duel", "seed": 4, "on_disconnect": "pause", "reconnect_seconds": 1},
                                    [{"type": "llm", "llm": {"provider": "mock"}}, {"type": "bot"}])
            self.assertTrue(self._wait(lambda: s.paused), "the game should pause when the server stays down")
            self.assertEqual(s.info()["pause_reason"]["kind"], "disconnect")
            time.sleep(1.5)
            self.assertEqual(s.game.turn, 1, "nobody - bots included - plays while the game is paused")
            server_up["v"] = True
            self.assertTrue(self._wait(lambda: not s.paused), "the game should resume when the server is back")
            self.assertTrue(self._wait(lambda: s.game.turn >= 2), "the interrupted turn is replayed and finished")
        self.manager.delete(s.id)
        self.assertIsNone(s.info()["pause_reason"])
        self.assertTrue(any(e["type"] == "game_resumed" for e in s.game.s.events))

    def test_save_and_load(self):
        s = self.manager.create({"map_size": "duel", "seed": 9}, [{"type": "human"}, {"type": "bot"}])
        pid = 0
        r = s.call_tool(pid, "end_turn", {})
        self.assertTrue(r["ok"], r)
        deadline = time.time() + 20
        while time.time() < deadline and s.game.s.current != 0:
            time.sleep(0.1)
        path = s.save("unit-test")
        data = load_save_file(path)
        s2 = GameSession.from_save(data)
        self.assertEqual(s2.game.turn, s.game.turn)
        self.assertEqual(s2.seats[0].token, s.seats[0].token)
        path.unlink()

    def test_wait_for_turn(self):
        s = self.manager.create({"map_size": "duel", "seed": 2}, [{"type": "mcp"}, {"type": "bot"}])
        self.assertEqual(s.wait_for_turn(0, 1)["status"], "your_turn")
        s.call_tool(0, "end_turn", {})
        res = s.wait_for_turn(0, 20)
        self.assertEqual(res["status"], "your_turn")


if __name__ == "__main__":
    unittest.main()
