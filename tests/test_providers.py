"""Offline tests for the provider adapters using fake SDK clients (no network)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest
from types import SimpleNamespace as NS
from unittest import mock

from citar.agents.providers.anthropic_provider import AnthropicConversation
from citar.agents.providers.openai_provider import OpenAIConversation, parse_json_calls

TOOLS = [{"name": "end_turn", "description": "End turn", "input_schema": {"type": "object", "properties": {}}},
         {"name": "get_tile", "description": "Tile", "input_schema": {"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}}}}]


class FakeAnthropicMessages:
    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def create(self, **kw):
        self.calls.append(kw)
        return self.responses.pop(0)


def usage():
    return NS(input_tokens=10, output_tokens=5, cache_read_input_tokens=3, cache_creation_input_tokens=0)


class AnthropicTests(unittest.TestCase):
    def test_tool_loop_shapes(self):
        r1 = NS(stop_reason="tool_use", usage=usage(), content=[
            NS(type="thinking", thinking="Let me look."), NS(type="text", text="Checking a tile."),
            NS(type="tool_use", id="tu_1", name="get_tile", input={"x": 1, "y": 2})])
        r2 = NS(stop_reason="tool_use", usage=usage(), content=[NS(type="tool_use", id="tu_2", name="end_turn", input={})])
        fake = FakeAnthropicMessages([r1, r2])
        with mock.patch("anthropic.Anthropic") as A:
            A.return_value = NS(messages=fake, beta=NS(messages=fake))
            conv = AnthropicConversation({"model": "claude-opus-5"}, "system prompt", TOOLS)
        conv.add_user_text("Your turn")
        s1 = conv.step()
        self.assertEqual(s1.tool_calls[0].name, "get_tile")
        self.assertEqual(s1.thinking, "Let me look.")
        call = fake.calls[0]
        self.assertEqual(call["model"], "claude-opus-5")
        self.assertEqual(call["thinking"]["type"], "adaptive")
        self.assertEqual(call["cache_control"], {"type": "ephemeral"})
        self.assertEqual(call["fallbacks"], "default")
        self.assertIn("server-side-fallback-2026-07-01", call["betas"])
        conv.add_tool_results([("tu_1", '{"terrain": "plains"}', False)])
        s2 = conv.step()
        self.assertEqual(s2.tool_calls[0].name, "end_turn")
        msgs = fake.calls[1]["messages"]
        self.assertEqual(msgs[1]["role"], "assistant")
        self.assertIs(msgs[1]["content"], r1.content)  # full content echoed back unchanged
        self.assertEqual(msgs[2]["content"][0]["type"], "tool_result")
        self.assertEqual(msgs[2]["content"][0]["tool_use_id"], "tu_1")
        self.assertEqual(conv.usage["input_tokens"], 20)

    def test_non_fallback_model_uses_plain_create(self):
        r = NS(stop_reason="end_turn", usage=usage(), content=[NS(type="text", text="hi")])
        plain = FakeAnthropicMessages([r])
        beta = FakeAnthropicMessages([])
        with mock.patch("anthropic.Anthropic") as A:
            A.return_value = NS(messages=plain, beta=NS(messages=beta))
            conv = AnthropicConversation({"model": "claude-sonnet-5", "effort": "medium"}, "sys", TOOLS)
        conv.add_user_text("x")
        conv.step()
        self.assertEqual(len(plain.calls), 1)
        self.assertEqual(plain.calls[0]["output_config"], {"effort": "medium"})
        self.assertNotIn("fallbacks", plain.calls[0])

    def test_max_tokens_discards_partial(self):
        r = NS(stop_reason="max_tokens", usage=usage(), content=[NS(type="tool_use", id="t", name="end_turn", input={})])
        fake = FakeAnthropicMessages([r])
        with mock.patch("anthropic.Anthropic") as A:
            A.return_value = NS(messages=fake, beta=NS(messages=fake))
            conv = AnthropicConversation({}, "sys", TOOLS)
        conv.add_user_text("x")
        s = conv.step()
        self.assertEqual(s.tool_calls, [])
        self.assertEqual(conv.messages[-1]["role"], "user")


class OpenAITests(unittest.TestCase):
    def fake_client(self, responses):
        calls = []

        def create(**kw):
            calls.append(kw)
            return responses.pop(0)
        return NS(chat=NS(completions=NS(create=create))), calls

    def test_native_tool_calls(self):
        resp = NS(usage=NS(prompt_tokens=7, completion_tokens=3), choices=[NS(finish_reason="tool_calls", message=NS(
            content="", tool_calls=[NS(id="c1", function=NS(name="get_tile", arguments='{"x": 3, "y": 4}'))]))])
        client, calls = self.fake_client([resp])
        with mock.patch("citar.agents.providers.openai_provider.OpenAI", return_value=client):
            conv = OpenAIConversation({"model": "qwen", "base_url": "http://localhost:11434/v1"}, "sys", TOOLS)
        conv.add_user_text("go")
        s = conv.step()
        self.assertEqual(s.tool_calls[0].args, {"x": 3, "y": 4})
        self.assertEqual(calls[0]["tools"][0]["type"], "function")
        conv.add_tool_results([("c1", "ok", False)])
        self.assertEqual(conv.messages[-1], {"role": "tool", "tool_call_id": "c1", "content": "ok"})
        self.assertEqual(conv.messages[-2]["tool_calls"][0]["function"]["name"], "get_tile")

    def test_json_mode(self):
        text = 'Sure!\n```json\n{"thoughts": "explore", "calls": [{"tool": "get_tile", "args": {"x": 1, "y": 1}}, {"tool": "end_turn", "args": {}}]}\n```'
        resp = NS(usage=None, choices=[NS(finish_reason="stop", message=NS(content=text, tool_calls=None))])
        client, calls = self.fake_client([resp])
        with mock.patch("citar.agents.providers.openai_provider.OpenAI", return_value=client):
            conv = OpenAIConversation({"model": "tiny", "tool_mode": "json", "base_url": "http://x/v1"}, "sys", TOOLS)
        self.assertIn("TOOL PROTOCOL", conv.messages[0]["content"])
        conv.add_user_text("go")
        s = conv.step()
        self.assertEqual([c.name for c in s.tool_calls], ["get_tile", "end_turn"])
        self.assertNotIn("tools", calls[0])

    def test_repairs_leaked_xml_tool_calls(self):
        from citar.agents.providers.openai_provider import repair_tool_calls
        from citar.agents.providers.base import ToolCall
        calls, n = repair_tool_calls([
            ToolCall("a", "set_research", {"tech": "pottery</parameter>\n</function>\n</tool_call>\n<tool_call>\n<function=unit_order>\n<parameter=unit_id>\n9</parameter>\n<parameter=order>\nexplore"}),
            ToolCall("b", "end_turn", {})])
        self.assertEqual(n, 1)
        self.assertEqual([(c.name, c.args) for c in calls],
                         [("set_research", {"tech": "pottery"}), ("unit_order", {"unit_id": 9, "order": "explore"}), ("end_turn", {})])
        self.assertEqual(calls[0].id, "a")

    def test_xml_calls_in_text_content(self):
        text = "I'll do it.\n<tool_call>\n<function=found_city>\n<parameter=unit_id>\n7\n</parameter>\n</function>\n</tool_call>"
        resp = NS(usage=None, choices=[NS(finish_reason="stop", message=NS(content=text, tool_calls=None))])
        client, calls = self.fake_client([resp])
        with mock.patch("citar.agents.providers.openai_provider.OpenAI", return_value=client):
            conv = OpenAIConversation({"model": "m", "base_url": "http://x/v1"}, "sys", TOOLS)
        conv.add_user_text("go")
        s = conv.step()
        self.assertEqual((s.tool_calls[0].name, s.tool_calls[0].args), ("found_city", {"unit_id": 7}))
        self.assertEqual(s.malformed, 1)
        self.assertEqual(conv.messages[-1]["tool_calls"][0]["id"], s.tool_calls[0].id)

    def test_plain_text_tool_calls(self):
        from citar.agents.providers.openai_provider import parse_plain_calls
        from citar.engine.tools import tool_list
        tools = [{"type": "function", "function": {"name": t["name"], "parameters": t["input_schema"]}} for t in tool_list()]

        def parsed(text):
            return [(c.name, c.args) for c in parse_plain_calls(text, tools)]

        # shapes Gemma produced in place of native tool calls
        self.assertEqual(parsed("end_turn{}"), [("end_turn", {})])
        self.assertEqual(parsed("log_thought\nKeep exploring; the capital builds a warrior."),
                         [("log_thought", {"text": "Keep exploring; the capital builds a warrior."})])
        self.assertEqual(parsed('set_research {"tech": "pottery"}\nunit_order({"unit_id": 3, "order": "explore"})\nend_turn'),
                         [("set_research", {"tech": "pottery"}), ("unit_order", {"unit_id": 3, "order": "explore"}),
                          ("end_turn", {})])
        # prose that only mentions tools is not a call
        self.assertEqual(parsed("I will call end_turn once the city has production set."), [])
        self.assertEqual(parsed("set_research needs a tech name, so let me think."), [])

        resp = NS(usage=None, choices=[NS(finish_reason="stop", message=NS(content="end_turn{}", tool_calls=None))])
        client, _ = self.fake_client([resp])
        with mock.patch("citar.agents.providers.openai_provider.OpenAI", return_value=client):
            conv = OpenAIConversation({"model": "m", "base_url": "http://x/v1"}, "sys", TOOLS)
        conv.add_user_text("go")
        s = conv.step()
        self.assertEqual(([c.name for c in s.tool_calls], s.malformed), (["end_turn"], 1))

    def test_glm_style_calls_in_json_mode(self):
        # Laguna ignored the JSON protocol and wrote its own trained tool-call format
        text = ("Let me execute these decisions:<tool_call>set_civ_name<arg_key>name</arg_key><arg_value>Aurelia</arg_value>"
                "<arg_key>leader</arg_key><arg_value>Consul</arg_value></tool_call><tool_call>found_city<arg_key>unit_id"
                "</arg_key><arg_value>1</arg_value><arg_key>name</arg_key><arg_value>Roma</arg_value></tool_call>"
                "<tool_call>end_turn</tool_call>")
        resp = NS(usage=None, choices=[NS(finish_reason="stop", message=NS(content=text, tool_calls=None))])
        client, _ = self.fake_client([resp])
        with mock.patch("citar.agents.providers.openai_provider.OpenAI", return_value=client):
            conv = OpenAIConversation({"model": "m", "tool_mode": "json", "base_url": "http://x/v1"}, "sys", TOOLS)
        conv.add_user_text("go")
        s = conv.step()
        self.assertEqual([(c.name, c.args) for c in s.tool_calls],
                         [("set_civ_name", {"name": "Aurelia", "leader": "Consul"}),
                          ("found_city", {"unit_id": 1, "name": "Roma"}), ("end_turn", {})])
        self.assertEqual(s.malformed, 3)

    def test_parse_json_calls_tolerant(self):
        calls, thoughts = parse_json_calls('<think>hmm</think>{"tool": "end_turn", "args": {}}')
        self.assertEqual(calls[0].name, "end_turn")
        calls, _ = parse_json_calls("no json here")
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
