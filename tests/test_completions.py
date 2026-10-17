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

From 4-05 a helper's `done.message` is chosen by whoever runs the helper, so the fix round of 4-02 added `Hostile`
(linear time on the shapes that were quadratic, no exception on any message protocol 2 accepts, the bounds on calls and
nesting) and `Oracle` (the rewritten scans give what the code before them gave, `tests/completion_oracle.py`).
"""
from __future__ import annotations

import json
import random
import re
import time
import unittest
from pathlib import Path
from types import SimpleNamespace as NS
from unittest import mock

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

from citar.agents.providers import completion, openai_provider
from tests import completion_oracle as oracle

CORPUS = Path(__file__).resolve().parent / "data" / "completions"
TOOLS_FILE = json.loads((CORPUS / "tools.json").read_text(encoding="utf-8"))
TOOLSETS = TOOLS_FILE["sets"]
PROMPTS = TOOLS_FILE["json_prompts"]
CASES = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(CORPUS.glob("*.json")) if p.name != "tools.json"]
GENERATED = re.compile(r"^call_[0-9a-f]{8}$")
GAME = completion.openai_tools(TOOLSETS["game"])
Q = '<|"|>'


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


# ---------------------------------------------------------------------------------------------------------------------
# Hostile replies (4-02's fix round)

BIG = "9" * 4301  # one digit more than Python converts by default (sys.get_int_max_str_digits)


def message(content=None, calls=(), reasoning=None) -> dict:
    return {"content": content, "reasoning": reasoning, "tool_calls": list(calls)}


def call(id_, name, arguments) -> dict:
    return {"id": id_, "type": "function", "function": {"name": name, "arguments": arguments}}


def repeat(unit: str, size: int, head: str = "", tail: str = "") -> str:
    return head + unit * max(1, (size - len(head) - len(tail)) // len(unit)) + tail


def hostile_replies(size: int):
    """``(label, mode, message)``: replies of about ``size`` characters in every shape that took time quadratic in
    their length before the fix round (the first 13 and the Gemma 4 ones), or raised, or made too many calls."""
    spaces = " " * size
    yield "think blocks never closed", "native", message(repeat("<think>", size))
    yield "think blocks never closed, json mode", "json", message(repeat("<think>", size))
    yield "fences opening on a brace", "json", message(repeat("```{", size))
    yield "an XML value's trailing spaces", "native", message("<function=a><parameter=b>a" + spaces + "b")
    yield "merged XML with trailing spaces", "native", message(calls=[call("c1", "set_research", json.dumps(
        {"tech": "x</parameter><parameter=b>a" + spaces + "b"}))])
    yield "GLM keys never closed", "native", message(repeat("<arg_key>", size, head="<tool_call>x"))
    yield "a GLM key's trailing spaces", "native", message("<tool_call>x<arg_key>a" + spaces + "b")
    yield "GLM keys with no value", "native", message(repeat("<arg_key>a</arg_key>", size, head="<tool_call>x"))
    yield "Gemma 4 words", "native", message(repeat("a ", size, head="set_research {"))
    yield "Gemma 4 objects left open", "native", message(repeat("end_turn{\n", size))
    yield "Gemma 4 objects left open after a parenthesis", "native", message(repeat("set_research({\n", size))
    yield "Gemma 4 strings left open", "native", message(repeat("end_turn{" + Q + "\n", size))
    yield "Gemma 4 objects closed far away", "native", message(repeat("end_turn{\n", size // 2) + "}" * (size // 20))
    yield "a line of prefix marks", "native", message(repeat("-", size) + "x")
    yield "a line of spaces", "native", message("x\n" + spaces + "y")
    yield "fences closing on a brace", "json", message(repeat("}```", size, head="```{"))
    yield "XML parameters", "native", message(repeat("<parameter=a>", size, head="<function=x>"))
    yield "GLM values never closed", "native", message(repeat("<arg_key>a</arg_key><arg_value>b", size,
                                                              head="<tool_call>x"))
    yield "plain calls", "native", message(repeat("end_turn\n", size))
    yield "XML calls", "native", message(repeat("<function=end_turn>", size))
    yield "GLM calls", "native", message(repeat("<tool_call>end_turn</tool_call>", size))
    yield "JSON-protocol calls", "json", message(repeat('{"tool": "end_turn", "args": {}},', size,
                                                        head='{"calls": [', tail='{"tool": "end_turn"}]}'))
    yield "arguments nested past Python's limit", "native", message(calls=[call("c1", "end_turn", "[" * size)])
    yield "arguments nested deep", "native", message(calls=[call("c1", "end_turn", '{"a":' * (size // 6) + "1"
                                                                 + "}" * (size // 6))])
    yield "a JSON-protocol reply nested deep", "json", message('{"calls": [{"tool": "end_turn", "args": '
                                                               + '{"a":' * (size // 6) + "1" + "}" * (size // 6) + "}]}")
    yield "a plain call's object nested deep", "native", message("set_research " + '{"a":' * (size // 6) + "1"
                                                                 + "}" * (size // 6))
    yield "a huge number in arguments", "native", message(calls=[call("c1", "end_turn", '{"n": ' + "1" * size + "}")])
    yield "a huge number in an XML parameter", "native", message("<function=a><parameter=n>" + "1" * size)


#: Messages protocol 2 accepts that the code before the fix round raised on, or read into the wrong types.
HOSTILE_TEXTS = [
    '{"calls": 5}', '{"calls": true}', '{"calls": {"tool": "end_turn"}}', '{"calls": "end_turn"}',
    '{"calls": [{"tool": "end_turn", "args": 5}, {"tool": "get_tile", "args": [1, 2]}]}',
    '{"tool": "set_research", "args": "pottery"}', '{"thoughts": {"a": 1}, "calls": []}', '{"thoughts": [1]}',
    '{"calls": [{"tool": "end_turn", "args": {"x": NaN, "y": -Infinity}}]}',
    "[" * 20000, '{"a":' * 5000 + "1" + "}" * 5000, '{"calls": ' + "[" * 20000, BIG, '{"n": ' + BIG + "}",
    'get_tile {"x": ' + BIG + "}", "get_tile " + "[" * 20000, "get_tile {" + '"a":' * 20000,
    'get_tile {"a": ' + "[" * 20000 + "]" * 20000 + "}", "end_turn{" + "[" * 20000,
    "<function=a><parameter=n>" + BIG, "<function=a><parameter=n>" + "[" * 20000,
    "<function=a><parameter=n>" + "[" * 100 + "]" * 100, "<tool_call>x<arg_key>a</arg_key><arg_value>" + "{" * 20000,
    "```json\n" + "[" * 20000 + "}\n```", "```json\n{" + '"a":[' * 5000 + "}\n```",
    f"set_research{{tech:{Q}" + "[" * 20000 + f"{Q}}}", "set_research{a:" + "[" * 20000 + "}",
]
NAMES = ["end_turn", "get_tile", "set_research", "log_thought", "found_city", "unit_order"]
SOUP = ["<think>", "</think>", "```", "```json", "{", "}", "[", "]", "(", ")", ":", ",", '"', " ", "\n", "\n", "\t", "a",
        "x.y", "-", "*", ">", "`", "1", "-2", "true", "null", Q, "call:", "<|tool_call>", "<function=",
        "<function=end_turn>", "</function>", "<parameter=x>", "</parameter>", "<tool_call>", "</tool_call>",
        "<arg_key>", "</arg_key>", "<arg_value>", "</arg_value>", '"tool"', '"calls"', '"args"', '"thoughts"', "é",
        " ", *NAMES]


def random_json(rng: random.Random, depth: int = 0):
    """A random JSON value at most five levels deep, its words drawn from what the parsers look for."""
    r = rng.random()
    if depth > 4 or r < 0.4:
        return rng.choice([None, True, False, 0, -3, 2.5, 1e308, float("nan"), "", "x", "pottery", "</parameter>", Q,
                           "<function=end_turn>", "\n", *NAMES])
    if r < 0.7:
        return [random_json(rng, depth + 1) for _ in range(rng.randint(0, 4))]
    return {rng.choice(["tool", "args", "calls", "thoughts", "x", "tech", "unit_id"]): random_json(rng, depth + 1)
            for _ in range(rng.randint(0, 4))}


def random_text(rng: random.Random, hostile: bool = True) -> str:
    """Model output: token soup, JSON, the JSON protocol (perhaps fenced, among prose), or a hostile text."""
    r = rng.random()
    if r < 0.3:
        return "".join(rng.choice(SOUP) for _ in range(rng.randint(0, 30)))
    if r < 0.55:
        return json.dumps(random_json(rng))
    if r < 0.85 or not hostile:
        calls = [{"tool": rng.choice([*NAMES, 5, None]), "args": random_json(rng, 1)} for _ in range(rng.randint(0, 3))]
        reply = json.dumps({"thoughts": random_json(rng, 3), "calls": calls if rng.random() < 0.8 else
                            random_json(rng, 2)})
        return rng.choice([reply, f"Sure.\n```json\n{reply}\n```", f"<think>hmm</think>{reply} done"])
    return rng.choice(HOSTILE_TEXTS)


def random_message(rng: random.Random, hostile: bool = True) -> dict:
    """A `done.message` protocol 2 accepts (checked), drawn from model output of every kind."""
    calls = [call(f"call_{i}", rng.choice([*NAMES, "", "x" * 200]), random_text(rng, hostile))
             for i in range(rng.choice([0, 0, 0, 1, 2, 4]))]
    return message(None if rng.random() < 0.1 else random_text(rng, hostile), calls,
                   rng.choice([None, "", "Let me think.", "".join(rng.choice(SOUP) for _ in range(9))]))


def depth(value) -> int:
    """How many objects and arrays deep ``value`` nests, without recursion."""
    level, n = [value], 0
    while True:
        level = [v for v in level if isinstance(v, (dict, list))]
        if not level:
            return n
        n += 1
        level = [x for c in level for x in (c.values() if isinstance(c, dict) else c)]


class Hostile(unittest.TestCase):
    """From 4-05 a helper sends `done.message` and the server parses it, so whoever runs the helper chooses the text
    (up to 8 MiB a frame). Parsing must take time linear in it, never raise on a message protocol 2 accepts, and
    yield at most `MAX_CALLS` calls whose arguments are objects at most `MAX_DEPTH` deep (P4.6)."""

    def assert_well_formed(self, step, entry):
        """What the turn loop relies on: text, ids and names that are strings, arguments that are shallow objects
        (``"__invalid_json__" in args`` and the call's signature work), and an entry whose calls are the step's."""
        from citar.server.metrics import call_signature
        self.assertTrue(all(isinstance(v, str) for v in (step.text, step.thinking, step.stop_reason)))
        self.assertLessEqual(len(step.tool_calls), completion.MAX_CALLS)
        for c in step.tool_calls:
            self.assertTrue(isinstance(c.id, str) and isinstance(c.name, str) and isinstance(c.args, dict), c)
            self.assertLessEqual(depth(c.args), completion.MAX_DEPTH)
            call_signature(c.name, c.args)
        self.assertEqual((entry["role"], type(entry["content"])), ("assistant", str))
        if "tool_calls" in entry:
            self.assertEqual([e["id"] for e in entry["tool_calls"]], [c.id for c in step.tool_calls])
        json.dumps(entry)

    def test_every_hostile_shape_parses_in_linear_time(self):
        """Each shape at 1 MiB: under 0.4 s on the owner's laptop (Python 3.14), where the code before the fix round
        took seconds at 20 KB and hours at this size. The bound is 1 s, times 3 for CI (p3-notes.md)."""
        for label, mode, msg in hostile_replies(1 << 20):
            with self.subTest(shape=label):
                started = time.perf_counter()
                step, entry = completion.parse_completion(msg, "stop", mode, GAME)
                took = time.perf_counter() - started
                self.assertLess(took, 3.0)
                self.assert_well_formed(step, entry)

    def test_many_lines_cost_linear_time(self):
        """Replies of many lines that name a tool, each a call to read (before the fix round each one copied the rest
        of the reply, a cost quadratic in its length with a small factor that 1 MiB does not show): eight times the
        text costs at most 24 times the time, best of a few runs, where quadratic costs 64 times."""
        def best(unit, size, runs):
            msg = message(repeat(unit, size))
            took = []
            for _ in range(runs):
                started = time.perf_counter()
                completion.parse_completion(msg, "stop", "native", GAME)
                took.append(time.perf_counter() - started)
            return min(took)
        for unit in ("end_turn{\n", f"end_turn{{{Q}\n"):
            with self.subTest(line=unit):
                small, big = best(unit, 256 << 10, 3), best(unit, 2 << 20, 2)
                self.assertLess(big, 24 * small, (small, big))

    def test_any_done_message_parses(self):
        """2,000 random messages protocol 2 accepts (model output of every kind, random JSON in the text and the
        arguments, and the hostile texts), in both tool modes and against every tool set: no exception, and a
        well-formed step and entry."""
        from citar.server import helper_protocol as HP
        rng = random.Random(402)
        for i in range(2000):
            msg = random_message(rng)
            frame = HP.helper_frame("done", id="r1", message=msg, finish_reason=rng.choice([None, "stop", "length"]),
                                    usage=None, gen_s=1.0, ttft_s=None, streamed=False)
            self.assertEqual(HP.parse_helper_frame(frame)["message"], msg)
            for mode in ("native", "json"):
                for tools in ("game", "small", "none"):
                    with self.subTest(i=i, mode=mode, tools=tools):
                        step, entry = completion.parse_completion(msg, "stop", mode,
                                                                  completion.openai_tools(TOOLSETS[tools]))
                        self.assert_well_formed(step, entry)

    def test_a_reply_yields_at_most_max_calls(self):
        """However the calls were written, a reply yields the first `MAX_CALLS` (protocol 2's bound on a `done`
        frame's native calls), and the stored entry names exactly those."""
        n = completion.MAX_CALLS + 88

        def leak(functions):
            return json.dumps({"tech": "x</parameter></function>" + "<function=end_turn></function>" * functions})
        replies = [
            ("plain", "native", message("end_turn\n" * n)),
            ("XML", "native", message("<function=end_turn></function>" * n)),
            ("GLM", "native", message("<tool_call>end_turn</tool_call>" * n)),
            ("JSON protocol", "json", message(json.dumps({"calls": [{"tool": "end_turn", "args": {}}] * n}))),
            ("native (an endpoint's, past protocol 2's bound)", "native",
             message(calls=[call(f"c{i}", "end_turn", "{") for i in range(n)])),
            ("repaired from one call", "native", message(calls=[call("c1", "set_research", leak(n))])),
            ("repaired from two calls", "native", message(calls=[call("c1", "set_research", leak(n // 2)),
                                                                 call("c2", "set_research", leak(n // 2))])),
        ]
        for label, mode, msg in replies:
            with self.subTest(written=label):
                step, entry = completion.parse_completion(msg, "stop", mode, GAME)
                self.assertEqual(len(step.tool_calls), completion.MAX_CALLS)
                self.assert_well_formed(step, entry)
        # the native calls past the bound are not read at all: their invalid arguments count nowhere
        step, _ = completion.parse_completion(replies[4][2], "stop", "native", GAME)
        self.assertEqual([c.id for c in step.tool_calls], [f"c{i}" for i in range(completion.MAX_CALLS)])
        self.assertEqual(step.malformed, completion.MAX_CALLS)

    def test_arguments_nest_at_most_max_depth(self):
        """JSON nested `MAX_DEPTH` levels is read, one level more is not: native arguments are kept as text for the
        tool's error, a plain call is not read, a JSON-protocol reply is not read as the protocol (its own three
        levels count), and an XML parameter stays text."""
        deepest = completion.MAX_DEPTH

        def obj(levels):
            return '{"a": ' + "[" * (levels - 1) + "]" * (levels - 1) + "}"
        for levels, read in ((deepest, True), (deepest + 1, False)):
            with self.subTest(levels=levels):
                text = obj(levels)
                step, _ = completion.parse_completion(message(calls=[call("c1", "get_tile", text)]), "tool_calls",
                                                      "native", GAME)
                self.assertEqual(step.tool_calls[0].args, json.loads(text) if read else {"__invalid_json__": text})
                self.assertEqual(step.malformed, 0 if read else 1)
                step, _ = completion.parse_completion(message("get_tile " + text), "stop", "native", GAME)
                self.assertEqual([c.args for c in step.tool_calls], [json.loads(text)] if read else [])
                reply = '{"calls": [{"tool": "get_tile", "args": ' + obj(levels - 3) + "}]}"
                step, _ = completion.parse_completion(message(reply), "stop", "json", GAME)
                self.assertEqual([c.args for c in step.tool_calls], [json.loads(obj(levels - 3))] if read else [])
                nested = "[" * levels + "]" * levels
                step, _ = completion.parse_completion(message(f"<function=a><parameter=p>{nested}</parameter>"),
                                                      "stop", "native", GAME)
                self.assertEqual(step.tool_calls[0].args, {"p": json.loads(nested) if read else nested})


# ---------------------------------------------------------------------------------------------------------------------
# The rewritten scans against the code before them


def soup(rng: random.Random, tokens: list, most: int) -> str:
    return "".join(rng.choice(tokens) for _ in range(rng.randint(0, most)))


def plain_reply(rng: random.Random) -> str:
    """Lines as models write plain and Gemma 4 calls: a tool name (after the marks a line may start with) and an
    object, often left open, whose strings may hold line breaks and tool names; a bare tool name; or prose."""
    body = ["k:", "x :", "n: 3", ",", Q, f"{Q}t\n{Q}", f"{Q}a\nend_turn{{{Q}", "{", "}", "}", "[", "]", " ", "\n",
            "true", "1e5", '"', "v", "\n:", "end_turn", "\nend_turn\n:", "-", '"tech": "p"']

    def line():
        r = rng.random()
        mark = rng.choice(["", " ", "-", "* ", "> ", "`", "<|tool_call>call:", "call:", "\t"])
        if r < 0.6:
            return (mark + rng.choice(["end_turn", "get_tile", "log_thought", "set_research"])
                    + rng.choice(["", " ", "(", "`", "( "]) + "{" + soup(rng, body, 10))
        if r < 0.8:
            return mark + rng.choice(["end_turn", "get_tile", "log_thought", "set_research"])
        return soup(rng, [*body, "text", ":"], 6)
    return "\n".join(line() for _ in range(rng.randint(1, 6)))


class Oracle(unittest.TestCase):
    """The scans 4-02's fix round rewrote give what the code before them gave (`tests/completion_oracle.py`, the
    parser at 9585349) on random text built from what each one looks for."""

    def check(self, tokens, most, new, old, n=5000, seed=0):
        rng = random.Random(seed)
        for _ in range(n):
            text = soup(rng, tokens, most)
            self.assertEqual(new(text), old(text), text)

    def test_think_blocks(self):
        self.check(["<think>", "</think>", "a", " ", "\n", "x</think>", "<think>y", "<", ">", "think"], 14,
                   completion._strip_think, lambda t: re.sub(r"<think>.*?</think>", "", t, flags=re.S))

    def test_fenced_objects(self):
        fence = re.compile(r"```(?:json)?\s*(\{.*\})\s*```", re.S)
        self.check(["```", "```json", "json", "{", "}", " ", "\n", "a", "`", '"', "``"], 14, completion._fenced_object,
                   lambda t: m.group(1) if (m := fence.search(t)) else None)

    def test_xml_parameters(self):
        """The values keep their trailing whitespace now (`_coerce` strips it); otherwise the same."""
        self.check(["<parameter=a>", "<parameter=b.c>", "</parameter>", "</function>", "<tool_call>", "</tool_call>",
                    " ", "\n", "\t", "x", "1", "<", "<parameter=", ">", " ", " "], 14,
                   lambda t: [(k, v.rstrip()) for k, v in completion._PARAM_RE.findall(t)], oracle._PARAM_RE.findall)

    def test_glm_pairs(self):
        self.check(["<arg_key>", "</arg_key>", "<arg_value>", "</arg_value>", " ", "\n", "k", "v", "<", ">", " ",
                    "a b"], 16, completion._glm_args, oracle._GLM_ARG_RE.findall)

    def test_gemma_objects(self):
        self.check(["{", "}", "[", "]", Q, ":", ",", " ", "\n", "a", "key", "x.y-z", "1", "e5", "-", "true", "null",
                    '"', "<", "|", "é", "_k"], 16, lambda t: completion.gemma_json("{" + t),
                   lambda t: oracle.gemma_json("{" + t))

    def test_parsers_on_their_own_text(self):
        tools = completion.openai_tools(TOOLSETS["small"])
        for label, parse, old, tokens in (
                ("xml", completion.parse_xml_calls, oracle.parse_xml_calls,
                 ["<function=end_turn>", "<function=get_tile>", "<function=", "</function>", "<parameter=x>",
                  "</parameter>", "<tool_call>", " ", "\n", "1", "[1, 2]", '{"a": 1}', "true", "-3", "x"]),
                ("glm", completion.parse_glm_calls, oracle.parse_glm_calls,
                 ["<tool_call>", "</tool_call>", "end_turn", "get_tile", "<arg_key>", "</arg_key>", "<arg_value>",
                  "</arg_value>", " ", "\n", "x", "1", "true", "[1]"]),
                ("plain", lambda t: completion.parse_plain_calls(t, tools), lambda t: oracle.parse_plain_calls(t, tools),
                 [*SOUP, "\n", "\n", "{}", "()", '{"x": 1}'])):
            with self.subTest(parser=label):
                self.check(tokens, 24, lambda t, parse=parse: [(c.name, c.args) for c in parse(t)],
                           lambda t, old=old: [(c.name, c.args) for c in old(t)], n=3000)
        self.check(["{", "}", "[", "]", '"calls"', '"tool"', '"args"', '"thoughts"', '"end_turn"', ":", ",", " ", "```",
                    "```json", "<think>", "</think>", "1", '"t"', "null", "{}", "[]", "\n"], 24,
                   lambda t: ([(c.name, c.args) for c in completion.parse_json_calls(t)[0]],
                              completion.parse_json_calls(t)[1]),
                   lambda t: ([(c.name, c.args) for c in oracle.parse_json_calls(t)[0]], oracle.parse_json_calls(t)[1]),
                   n=3000)

    def test_plain_and_gemma_calls_stop_at_the_next_call(self):
        """An object left open stops at the next line's tool name (`gemma_json`'s stops) with the same calls as the
        code that read on to the end: 5,000 replies of plain and Gemma 4 lines, the stop taken hundreds of times."""
        tools = completion.openai_tools(TOOLSETS["small"]) + [{"type": "function", "function": {
            "name": "set_research", "parameters": {"type": "object", "properties": {"tech": {"type": "string"}},
                                                   "required": ["tech"]}}}]
        stopped = 0
        whole = completion.gemma_json

        def gemma_json(text, start=0, stops=frozenset()):
            nonlocal stopped
            got = whole(text, start, stops)
            stopped += got is None and whole(text, start) is not None
            return got
        rng = random.Random(7)
        with mock.patch.object(completion, "gemma_json", gemma_json):
            for _ in range(5000):
                text = plain_reply(rng)
                self.assertEqual([(c.name, c.args) for c in completion.parse_plain_calls(text, tools)],
                                 [(c.name, c.args) for c in oracle.parse_plain_calls(text, tools)], text)
        self.assertGreater(stopped, 100)

    def test_parse_completion(self):
        """Whole messages: the same step and entry, except where the fix round changed the reading on purpose (a
        call's arguments that are not an object, thoughts that are not text: the old step shows them)."""
        rng = random.Random(4)
        compared = 0
        for _ in range(1500):
            msg = random_message(rng, hostile=False)
            for mode in ("native", "json"):
                given = {c["id"] for c in msg["tool_calls"]}
                try:
                    old_step, old_entry = oracle.parse_completion(msg, "stop", mode, GAME)
                except Exception:
                    continue  # the old code raised: it gives no reference
                if not isinstance(old_step.text, str) or any(not isinstance(c.args, dict) for c in old_step.tool_calls):
                    continue
                step, entry = completion.parse_completion(msg, "stop", mode, GAME)
                # as JSON text, so a NaN read from the reply compares equal to itself
                self.assertEqual(json.dumps(normalized(step_dict(step), entry, given)),
                                 json.dumps(normalized(step_dict(old_step), old_entry, given)), msg)
                compared += 1
        self.assertGreater(compared, 2500)


if __name__ == "__main__":
    unittest.main()
