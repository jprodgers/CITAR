import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

from citar.server.metrics import Metrics


class MetricsTests(unittest.TestCase):
    def test_turn_open_at_save_is_excluded_after_load(self):
        m = Metrics()
        m.begin_turn(0, 1, "bot")
        m.tool_call(0, "set_research", {"tech": "pottery"}, "action", True, 0.001)
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


if __name__ == "__main__":
    unittest.main()
