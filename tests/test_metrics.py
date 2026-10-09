import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.server.metrics import Metrics


class MetricsTests(unittest.TestCase):
    def test_turn_open_at_save_is_excluded_after_load(self):
        m = Metrics()
        m.begin_turn(0, 1, "bot")
        m.bot_actions(0, {"set_research": [1, 0]})
        m.end_turn(0)
        m.begin_turn(0, 2, "bot")            # game saved while this turn is in progress
        saved = {"turns": [dict(r) for r in m.data["turns"]], "negotiations": []}

        loaded = Metrics(saved)               # server restarted, game reloaded later
        loaded.data["turns"][1]["started"] -= 2000  # simulate downtime between save and load
        loaded.begin_turn(0, 2, "bot")        # the turn is replayed
        loaded.end_turn(0)

        players = {0: {"name": "Bot", "controller": "bot", "model": None}}
        s = loaded.summary(players)[0]
        self.assertEqual(s["turns"], 2)                      # turn 1 + the replayed turn 2
        self.assertLess(s["max_turn_s"], 5)                  # downtime not counted
        self.assertEqual(loaded.data["turns"][1]["end_reason"], "interrupted")

    def test_repeats_and_blocked_counts(self):
        m = Metrics()
        m.begin_turn(1, 1, "llm", "some-model")
        for _ in range(3):
            m.tool_call(1, "get_briefing", {}, "query", True, 0.01)
        m.tool_call(1, "set_civ_name", {"name": "X"}, "action", True, 0.01)
        m.tool_call(1, "set_civ_name", {"name": "X"}, "action", False, 0.0, "blocked", blocked_repeat=True)
        m.model_step(1, 2.0, input_tokens=5000, output_tokens=100, malformed=1)
        m.end_turn(1, "end_turn")
        s = m.summary({1: {"name": "A", "controller": "llm", "model": "some-model"}})[1]
        self.assertEqual(s["avg_repeats"], 3)                # 2 repeated briefings + 1 repeated rename
        self.assertEqual(s["blocked_repeats"], 1)
        self.assertEqual(s["avg_errors"], 0)                 # blocked repeats are not counted as errors
        self.assertEqual(s["malformed_calls"], 1)
        self.assertEqual(s["peak_prompt_tokens"], 5000)
        self.assertEqual(s["tools"]["get_briefing"]["max_in_turn"], 3)

    def test_a_bot_turn_has_its_actions_and_no_tool_rows(self):
        """A bot's actions never pass through the session's tool calls (they are a drive's, DESIGN.md P2.7.1): its
        turn rows carry them as bot_actions, and the summary, the rows, the CSV text and the model comparison take a
        seat that has no tool rows at all."""
        from citar.server.metrics import bot_actions_text
        from citar.server.scoring import compare_models
        m = Metrics()
        m.bot_actions(0, {"move_unit": [1, 0]})            # no turn open: nothing to add it to
        m.begin_turn(0, 1, "bot")
        m.bot_actions(0, {"move_unit": [3, 1], "set_research": [1, 0]})
        m.bot_actions(0, {"move_unit": [2, 0]})            # a second drive of the same turn adds to the first
        m.bot_actions(0, None)
        m.end_turn(0)
        m.begin_turn(0, 2, "bot")
        m.bot_actions(0, {"unit_action": [1, 0]})
        m.end_turn(0)
        rows = m.turn_rows()
        self.assertEqual(rows[0]["bot_actions"], {"move_unit": [5, 1], "set_research": [1, 0]})
        self.assertEqual(rows[0]["top_tools"], "move_unitx5, set_researchx1")
        self.assertEqual((rows[0]["tool_calls"], rows[0]["end_reason"]), (0, "end_turn"))
        self.assertEqual(bot_actions_text(rows[0]["bot_actions"]), "move_unit 5 (1 refused), set_research 1")
        self.assertEqual(bot_actions_text(None), "")
        s = m.summary({0: {"name": "Bot", "controller": "bot", "model": None}})[0]
        self.assertEqual(s["turns"], 2)
        self.assertEqual(s["tools"], {})
        self.assertEqual(s["bot_actions"]["move_unit"], {"taken": 5, "refused": 1, "max_in_turn": 5, "per_turn": 2.5})
        self.assertEqual(list(s["bot_actions"]), ["move_unit", "set_research", "unit_action"])
        self.assertEqual(s["avg_bot_actions"], 3.5)
        self.assertEqual(s["bot_refusal_rate"], round(1 / 8, 3))
        self.assertEqual(s["error_rate"], 0)
        compared = compare_models(None, {"g": {"name": "G", "turn": 3, "summary": {0: s}}})
        self.assertEqual([(r["model"], r["turns"]) for r in compared], [("bot", 2)])


if __name__ == "__main__":
    unittest.main()
