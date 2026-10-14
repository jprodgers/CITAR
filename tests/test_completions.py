"""The raw-completion corpus (design/phase4-helper.md P4.5.2; package 4-02's gate 1).

`tests/data/completions/*.json` holds raw completions in the shape of protocol 2's `done` (the message's content,
reasoning and tool calls, the finish reason, the usage), each with what `OpenAIConversation.step` made of it when the
corpus was recorded, before the parsing moved out of it: the `StepResult`, the assistant entry it stored, and the
usage it counted. The ids the parsers generate for calls written as text (``call_<8 hex>``) are random, so they are
compared as ``<gen:N>``, numbered by first appearance across the step's calls and then the entry's: a step whose entry
named other ids than its calls would not match. `tools.json` holds the tool lists the cases parse against, as they were
when the corpus was recorded, so a new game tool cannot change what an old completion parses to.

Package 4-05's parity gate plays the same corpus through `WorkerConversation` and a fake helper.
"""
from __future__ import annotations

import json
import re
import unittest
from pathlib import Path
from types import SimpleNamespace as NS
from unittest import mock

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

from citar.agents.providers import openai_provider

CORPUS = Path(__file__).resolve().parent / "data" / "completions"
TOOLSETS = json.loads((CORPUS / "tools.json").read_text(encoding="utf-8"))["sets"]
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
                resp = sdk_response(case["completion"])
                sent = []

                def create(**kw):
                    sent.append(kw)
                    return resp
                client = NS(chat=NS(completions=NS(create=create)), close=lambda: None)
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


if __name__ == "__main__":
    unittest.main()
