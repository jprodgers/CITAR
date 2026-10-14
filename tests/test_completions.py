"""The raw-completion corpus (design/phase4-helper.md P4.5.2; package 4-02's gate 1).

`tests/data/completions/*.json` holds raw completions in the shape of protocol 2's `done` (the message's content,
reasoning and tool calls, the finish reason, the usage), each with what `OpenAIConversation.step` made of it when the
corpus was recorded, before the parsing moved out of it: the `StepResult`, the assistant entry it stored, and the
usage it counted. The ids the parsers generate for calls written as text (``call_<8 hex>``) are random, so they are
compared as ``<gen:N>``, numbered by first appearance across the step's calls and then the entry's: a step whose entry
named other ids than its calls would not match. `tools.json` holds the tool lists the cases parse against, as they were
when the corpus was recorded, so a new game tool cannot change what an old completion parses to.

Every case goes both ways and must come out the same: through `OpenAIConversation` with a fake SDK response, and
straight through `completion.parse_completion` on the raw message. Package 4-05's parity gate plays the same corpus
through `WorkerConversation` and a fake helper.
"""
from __future__ import annotations

import json
import re
import unittest
from pathlib import Path
from types import SimpleNamespace as NS
from unittest import mock

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

from citar.agents.providers import completion, openai_provider

CORPUS = Path(__file__).resolve().parent / "data" / "completions"
TOOLS_FILE = json.loads((CORPUS / "tools.json").read_text(encoding="utf-8"))
TOOLSETS = TOOLS_FILE["sets"]
PROMPTS = TOOLS_FILE["json_prompts"]
CASES = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(CORPUS.glob("*.json")) if p.name != "tools.json"]
GENERATED = re.compile(r"^call_[0-9a-f]{8}$")


def normalized(step: dict, entry: dict, given: set) -> tuple[dict, dict]:
    """``step`` and ``entry`` with every generated call id (one the completion did not carry) as ``<gen:N>``."""
    names: dict = {}

    def name(i):
        if isinstance(i, str) and GENERATED.match(i) and i not in given:
            names.setdefault(i, f"<gen:{len(names) + 1}>")
            return names[i]
        return i
    step = dict(step, tool_calls=[dict(c, id=name(c["id"])) for c in step["tool_calls"]])
    if "tool_calls" in entry:
        entry = dict(entry, tool_calls=[dict(c, id=name(c["id"])) for c in entry["tool_calls"]])
    return step, entry


def step_dict(result) -> dict:
    """A ``StepResult`` as the corpus writes it."""
    return {"text": result.text, "thinking": result.thinking,
            "tool_calls": [{"id": c.id, "name": c.name, "args": c.args} for c in result.tool_calls],
            "stop_reason": result.stop_reason, "malformed": result.malformed}


def given_ids(case: dict) -> set:
    return {c["id"] for c in case["completion"]["message"]["tool_calls"]}


def sdk_response(completion: dict):
    """The OpenAI SDK's response for a corpus completion, as `OpenAIConversation.step` reads it."""
    message, use = completion["message"], completion["usage"]
    calls = [NS(id=c["id"], type=c["type"], function=NS(name=c["function"]["name"],
                                                        arguments=c["function"]["arguments"]))
             for c in message["tool_calls"]] or None
    usage = None
    if use is not None:
        usage = NS(prompt_tokens=use["prompt_tokens"], completion_tokens=use["completion_tokens"],
                   completion_tokens_details=(NS(reasoning_tokens=use["reasoning_tokens"])
                                              if use["reasoning_tokens"] is not None else None),
                   prompt_tokens_details=(NS(cached_tokens=use["cached_tokens"])
                                          if use["cached_tokens"] is not None else None))
    return NS(usage=usage, choices=[NS(finish_reason=completion["finish_reason"],
                                       message=NS(content=message["content"], reasoning_content=message["reasoning"],
                                                  tool_calls=calls))])


def fake_client(resp, sent: list):
    """An OpenAI client whose one completion is ``resp``, recording each request's arguments in ``sent``."""
    def create(**kw):
        sent.append(kw)
        return resp
    return NS(chat=NS(completions=NS(create=create)), close=lambda: None)


class Corpus(unittest.TestCase):
    def test_the_corpus_holds_every_shape_the_parser_reads(self):
        """The shapes P4.5.2 names, each by at least one case: native calls, invalid JSON arguments, merged XML
        repaired, XML, GLM, plain and Gemma 4 text calls, the JSON protocol (and its fallbacks), reasoning, no call."""
        names = {c["name"] for c in CASES}
        for prefix in ("native-one-call", "native-invalid-json-arguments", "native-merged-xml-repaired",
                       "text-xml-", "text-glm-", "text-plain-", "text-gemma4-", "json-fenced", "json-glm-fallback",
                       "native-calls-with-reasoning", "native-no-tool-call"):
            self.assertTrue(any(n.startswith(prefix) for n in names), prefix)
        self.assertGreaterEqual(len(CASES), 40)
        # each file is named after its case, so a case is found by its name
        self.assertEqual(sorted(names), sorted(p.stem for p in CORPUS.glob("*.json") if p.name != "tools.json"))
        for case in CASES:
            self.assertEqual(set(case), {"name", "note", "mode", "tools", "completion", "expect"}, case["name"])
            self.assertIn(case["mode"], ("native", "json"))
            self.assertIn(case["tools"], TOOLSETS)

    def test_the_completions_are_done_frames_messages(self):
        """Every completion is what a protocol 2 helper sends in `done` (its message, finish reason and usage), so
        4-05 can replay the corpus through a fake helper unchanged."""
        from citar.server import helper_protocol as HP
        for case in CASES:
            with self.subTest(case=case["name"]):
                c = case["completion"]
                frame = HP.helper_frame("done", id="r1", message=c["message"], finish_reason=c["finish_reason"],
                                        usage=c["usage"], gen_s=1.5, ttft_s=0.25, streamed=True)
                parsed = HP.parse_helper_frame(frame)
                self.assertEqual((parsed["t"], parsed["message"], parsed["usage"]), ("done", c["message"], c["usage"]))

    def test_every_case_through_openai_conversation(self):
        """Each completion, through `OpenAIConversation.step` with a fake SDK response, gives its recorded step,
        stored entry and counted usage."""
        for case in CASES:
            with self.subTest(case=case["name"]):
                sent: list = []
                client = fake_client(sdk_response(case["completion"]), sent)
                cfg = {"model": "m", "base_url": "http://127.0.0.1:9/v1", "tool_mode": case["mode"]}
                with mock.patch.object(openai_provider, "OpenAI", return_value=client):
                    conv = openai_provider.OpenAIConversation(cfg, "sys", TOOLSETS[case["tools"]])
                conv.add_user_text("go")
                before = len(conv.messages)
                result = conv.step()
                self.assertEqual(len(conv.messages), before + 1)
                step, entry = normalized(step_dict(result), conv.messages[-1], given_ids(case))
                self.assertEqual(step, case["expect"]["step"])
                self.assertEqual(entry, case["expect"]["entry"])
                self.assertEqual(conv.usage, case["expect"]["usage"])
                # the request: tools only in native mode and only when there are any
                self.assertEqual("tools" in sent[0], case["mode"] == "native" and bool(TOOLSETS[case["tools"]]))

    def test_every_case_through_parse_completion(self):
        """Each raw message, given straight to `parse_completion` (as the server will give it a helper's `done.message`
        from 4-05), gives the same step and entry."""
        for case in CASES:
            with self.subTest(case=case["name"]):
                c = case["completion"]
                result, entry = completion.parse_completion(c["message"], c["finish_reason"], case["mode"],
                                                            completion.openai_tools(TOOLSETS[case["tools"]]))
                step, entry = normalized(step_dict(result), entry, given_ids(case))
                self.assertEqual(step, case["expect"]["step"])
                self.assertEqual(entry, case["expect"]["entry"])

    def test_parse_completion_leaves_its_input_alone(self):
        """The message is read, never changed: the hub keeps a `done` frame's message as it came."""
        for case in CASES:
            with self.subTest(case=case["name"]):
                c = case["completion"]
                message = json.loads(json.dumps(c["message"]))
                tools = completion.openai_tools(TOOLSETS[case["tools"]])
                before = json.dumps(tools)
                completion.parse_completion(message, c["finish_reason"], case["mode"], tools)
                self.assertEqual((message, json.dumps(tools)), (c["message"], before))


class Prompts(unittest.TestCase):
    """The request side the parser's JSON protocol relies on, moved with it."""

    def test_json_mode_appends_the_protocol_and_the_listing(self):
        tools = TOOLSETS["small"]
        prompt = completion.system_prompt("You play Rome.", tools, "json")
        self.assertTrue(prompt.startswith("You play Rome.\n" + completion.JSON_PROTOCOL))
        self.assertIn('- get_tile: Tile params={"x": {"type": "integer"}, "y": {"type": "integer"}}', prompt)
        self.assertEqual(prompt.count("\n- "), len(tools))
        self.assertEqual(completion.system_prompt("You play Rome.", tools, "native"), "You play Rome.")

    def test_openai_conversation_sends_what_the_helpers_will(self):
        """`OpenAIConversation` builds its system prompt and tools with the same two functions WorkerConversation will
        use (4-05), so a json-mode model gets the protocol whichever way its request goes."""
        tools = TOOLSETS["game"]
        for mode in ("native", "json"):
            with self.subTest(mode=mode), mock.patch.object(openai_provider, "OpenAI"):
                conv = openai_provider.OpenAIConversation({"model": "m", "base_url": "http://127.0.0.1:9/v1",
                                                           "tool_mode": mode}, "sys", tools)
                self.assertEqual(conv.messages, [{"role": "system",
                                                  "content": completion.system_prompt("sys", tools, mode)}])
                self.assertEqual(conv.tools, completion.openai_tools(tools))
                self.assertEqual(conv.tools[0], {"type": "function", "function": {
                    "name": tools[0]["name"], "description": tools[0]["description"],
                    "parameters": tools[0]["input_schema"]}})

    def test_the_json_mode_prompt_is_the_one_models_were_given(self):
        """The json-mode system prompt is byte for byte what `OpenAIConversation` sent at 8e7db41, before the request
        side moved (`tools.json`'s `json_prompts`), through `system_prompt` and as the conversation's first message:
        the target 4-05's WorkerConversation is held to."""
        system = PROMPTS["system"]
        for name in ("small", "game"):
            with self.subTest(tools=name):
                self.assertEqual(completion.system_prompt(system, TOOLSETS[name], "json"), PROMPTS[name])
                with mock.patch.object(openai_provider, "OpenAI"):
                    conv = openai_provider.OpenAIConversation({"model": "m", "base_url": "http://127.0.0.1:9/v1",
                                                               "tool_mode": "json"}, system, TOOLSETS[name])
                self.assertEqual(conv.messages[0]["content"], PROMPTS[name])


if __name__ == "__main__":
    unittest.main()
