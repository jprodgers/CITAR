"""One parser for every model completion the server receives (design/phase4-helper.md P4.5.2).

A model's reply reaches the server as an OpenAI-format message: from an endpoint the server calls itself
(`OpenAIConversation`), and from package 4-05 on from a helper's `done` frame, which carries the completion exactly as
the endpoint returned it (protocol 2 parses and repairs nothing on the helper). `parse_completion` turns that message
into the turn loop's `StepResult` and the assistant entry the conversation keeps, whoever produced it: native tool
calls, arguments that are not JSON, XML calls a local server merged into one call's arguments, calls a model wrote as
text (Qwen/Hermes XML, GLM, plain names, Gemma 4's own syntax), and the JSON protocol of tool mode ``json``. With one
parser, a model's quirk fixed once is fixed for helpers too: the 0.1.5 worker parsed on the helper, so every fix had
to be shipped to every machine (and the JSON protocol's prompt never reached its models at all).

The request side of the JSON protocol lives here too (`JSON_PROTOCOL`, `system_prompt`), so the server builds the
same prompt whichever way the request goes out.

Moved without change of behaviour from `openai_provider.py` (the parsing half of `OpenAIConversation.step`, its lines
105-152 at 8e7db41, and the parsers after it). `tests/data/completions` holds what the code gave before the move, and
`tests/test_completions.py` holds both paths to it.
"""
from __future__ import annotations

import json
import re
import uuid
from typing import Optional

from .base import StepResult, ToolCall

JSON_PROTOCOL = """
TOOL PROTOCOL (important): you cannot call functions natively. Reply ONLY with a JSON object, no other text:
{"thoughts": "<brief reasoning>", "calls": [{"tool": "<tool name>", "args": {<arguments>}}, ...]}
Use 1-8 calls per reply. Results come back in the next message. Available tools (name: description; parameters):
"""


def openai_tools(tools: list[dict]) -> list[dict]:
    """CITAR's tools (``{name, description, input_schema}``) as OpenAI function definitions: what a request offers
    and what `parse_completion` reads names and parameters from."""
    return [{"type": "function", "function": {"name": t["name"], "description": t["description"],
                                              "parameters": t["input_schema"]}} for t in tools]


def system_prompt(system: str, tools: list[dict], mode: str) -> str:
    """The system prompt a conversation in tool mode ``mode`` sends: in ``json`` mode, ``system`` followed by the JSON
    protocol and a listing of CITAR's ``tools`` (the model gets no native tools then); otherwise ``system`` as it is."""
    if mode != "json":
        return system
    listing = "\n".join(f"- {t['name']}: {t['description']} params={json.dumps(t['input_schema'].get('properties', {}))}"
                        for t in tools)
    return system + "\n" + JSON_PROTOCOL + listing


def _call_entries(calls: list) -> list[dict]:
    """``calls`` as the assistant entry stores them: with their own ids, so every tool result has a matching call."""
    return [{"id": c.id, "type": "function", "function": {"name": c.name, "arguments": json.dumps(c.args)}}
            for c in calls]


def parse_completion(message: dict, finish_reason: Optional[str], mode: str,
                     tools: list[dict]) -> tuple[StepResult, dict]:
    """One completion read: ``message`` is ``{content, reasoning, tool_calls: [{id, type, function: {name,
    arguments}}]}`` (protocol 2's `done.message`, the shape of an OpenAI-compatible endpoint's message), ``mode`` the
    conversation's tool mode (``native`` or ``json``) and ``tools`` the OpenAI function definitions it offered
    (`openai_tools`), which text calls are read against. Returns the step and the assistant entry the conversation
    appends to its messages.

    `StepResult.malformed` counts the calls that had to be repaired or were read from text, and the arguments that
    were not JSON (kept as ``{"__invalid_json__": text}`` so the tool answers with an error the model can read).
    """
    text = message.get("content") or ""
    thinking = message.get("reasoning") or ""
    stop_reason = finish_reason or ""
    if mode == "json":
        # the tool-call channel is not used in json mode: the reply's text holds the calls
        calls, thoughts = parse_json_calls(text)
        malformed = 0
        if not calls and text.strip():
            # ignored the JSON protocol but wrote calls in its own trained format (e.g. GLM-style tags)
            calls = parse_text_calls(text, tools)
            malformed = len(calls)
        return (StepResult(text=thoughts or text, thinking=thinking, tool_calls=calls, stop_reason=stop_reason,
                           malformed=malformed),
                {"role": "assistant", "content": text})
    native = message.get("tool_calls") or []
    calls = []
    for tc in native:
        function = tc.get("function") or {}
        arguments = function.get("arguments")
        try:
            args = json.loads(arguments or "{}")
            if not isinstance(args, dict):
                args = {}
        except json.JSONDecodeError:
            args = {"__invalid_json__": arguments}
        calls.append(ToolCall(tc.get("id"), function.get("name"), args))
    malformed = sum(1 for c in calls if "__invalid_json__" in c.args)
    calls, repaired = repair_tool_calls(calls)
    malformed += repaired
    if not calls and text.strip():
        # the model wrote tool calls as text instead of using the tool-call channel
        calls = parse_text_calls(text, tools)
        malformed += len(calls)
        if calls:
            return (StepResult(text=text, thinking=thinking, tool_calls=calls, stop_reason=stop_reason,
                               malformed=malformed),
                    {"role": "assistant", "content": text, "tool_calls": _call_entries(calls)})
    entry = {"role": "assistant", "content": text}
    if native:
        # the repaired calls, so every tool result has a matching call id
        entry["tool_calls"] = _call_entries(calls)
    return (StepResult(text=text, thinking=thinking, tool_calls=calls, stop_reason=stop_reason, malformed=malformed),
            entry)


_LEAK_MARKERS = ("</parameter>", "<parameter=", "<function=", "</function>", "<tool_call>", "</tool_call>")
_FUNC_RE = re.compile(r"<function=([\w.\-]+)>(.*?)(?=<function=|$)", re.S)
_PARAM_RE = re.compile(r"<parameter=([\w.\-]+)>\s*(.*?)\s*(?=</parameter>|<parameter=|</function>|</tool_call>|<tool_call>|$)", re.S)


def _coerce(v: str):
    """Coerce a model's argument into the type the schema asks for.

    Models send ``"3"`` for integers and comma-separated strings for arrays constantly. Refusing them
    would measure formatting rather than play.
    """
    v = v.strip()
    if re.fullmatch(r"-?\d+", v):
        return int(v)
    if v.lower() in ("true", "false"):
        return v.lower() == "true"
    if v[:1] in "[{":
        try:
            return json.loads(v)
        except json.JSONDecodeError:
            pass
    return v


def parse_xml_calls(text: str) -> list:
    """Parse Qwen/Hermes-style <function=name><parameter=k>v</parameter></function> tool calls from text."""
    calls = []
    for m in _FUNC_RE.finditer(text):
        name, body = m.group(1), m.group(2)
        args = {k: _coerce(v) for k, v in _PARAM_RE.findall(body)}
        calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, args))
    return calls


_GLM_CALL_RE = re.compile(r"<tool_call>\s*([\w.\-]+)\s*(.*?)(?=</tool_call>|<tool_call>|$)", re.S)
_GLM_ARG_RE = re.compile(r"<arg_key>\s*(.*?)\s*</arg_key>\s*<arg_value>(.*?)(?:</arg_value>|(?=<arg_key>)|$)", re.S)


def parse_glm_calls(text: str) -> list:
    """Parse GLM-style tool calls written as text (seen with Laguna):
    <tool_call>found_city<arg_key>unit_id</arg_key><arg_value>1</arg_value></tool_call>"""
    calls = []
    for m in _GLM_CALL_RE.finditer(text):
        args = {k: _coerce(v) for k, v in _GLM_ARG_RE.findall(m.group(2))}
        calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", m.group(1), args))
    return calls


def parse_text_calls(text: str, tools: list[dict]) -> list:
    """Tool calls a model wrote into its reply text instead of the tool-call channel, in any format we recognize."""
    if "<function=" in text:
        return parse_xml_calls(text)
    if "<arg_key>" in text or re.search(r"<tool_call>\s*[\w.\-]+\s*(?:<arg_key>|</tool_call>)", text):
        return parse_glm_calls(text)
    return parse_plain_calls(text, tools)


#: Gemma 4's string delimiter in its own tool-call syntax (``call:name{key:<|"|>text<|"|>,n:3}``).
_GEMMA_QUOTE = '<|"|>'
_GEMMA_WORD = re.compile(r"[A-Za-z_][\w.\-]*")


def gemma_json(text: str) -> str | None:
    """The object at the start of ``text`` in Gemma 4's own tool-call syntax, which LM Studio sometimes leaves in the
    reply's text (``respond_negotiation{action:<|"|>reply<|"|>,message:<|"|>Agreed.<|"|>,negotiation_id:3}``), as
    JSON text: its keys are bare words and its strings sit between ``<|"|>`` marks. None when it is not one."""
    if text[:1] != "{":
        return None
    out, i, depth = [], 0, 0
    while i < len(text):
        if text.startswith(_GEMMA_QUOTE, i):
            end = text.find(_GEMMA_QUOTE, i + len(_GEMMA_QUOTE))
            if end < 0:
                return None
            out.append(json.dumps(text[i + len(_GEMMA_QUOTE):end]))
            i = end + len(_GEMMA_QUOTE)
            continue
        word = _GEMMA_WORD.match(text, i)
        if word:
            after = text[word.end():].lstrip()
            # a key (followed by its colon) is quoted; a bare value (true, false, null) is kept as it is
            out.append(json.dumps(word.group(0)) if after[:1] == ":" else word.group(0))
            i = word.end()
            continue
        ch = text[i]
        out.append(ch)
        i += 1
        if ch in "{[":
            depth += 1
        elif ch in "}]":
            depth -= 1
            if depth == 0:
                return "".join(out)
    return None


def parse_plain_calls(text: str, tools: list[dict]) -> list:
    """Parse tool calls a model wrote as plain text instead of using the tool-call channel (seen with Gemma), e.g.
    `end_turn{}`, `set_research {"tech": "pottery"}`, `log_thought` followed by the note on the next lines, or Gemma 4's
    own syntax (``call:name{key:<|"|>text<|"|>}``, ``gemma_json``). Only lines that start with a known tool name count,
    so prose that merely mentions a tool is ignored."""
    schemas = {t["function"]["name"]: t["function"].get("parameters") or {} for t in tools}
    t = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    names = "|".join(re.escape(n) for n in sorted(schemas, key=len, reverse=True))
    if not names:
        return []
    calls = []
    decoder = json.JSONDecoder()
    for m in re.finditer(rf"^[ \t>*`-]*(?:<\|tool_call>)?(?:call:)?({names})`?[ \t]*(?=[({{]|$)", t, flags=re.M):
        name, rest = m.group(1), t[m.end():]
        if rest[:1] == "(":
            rest = rest[1:].lstrip()
        if rest[:1] == "{":
            try:
                args, _ = decoder.raw_decode(rest)
            except json.JSONDecodeError:
                converted = gemma_json(rest)
                try:
                    args = json.loads(converted) if converted is not None else None
                except json.JSONDecodeError:
                    args = None
                if args is None:
                    continue
            if isinstance(args, dict):
                calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, args))
            continue
        if rest[:2] in ("()", ")"):
            rest = ""
        schema = schemas[name]
        required = schema.get("required") or []
        if not required:
            calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, {}))
        elif not calls and m.start() == 0 and len(required) == 1 \
                and (schema.get("properties") or {}).get(required[0], {}).get("type") == "string":
            # a bare tool name on the first line, followed by the value of its only (string) parameter
            body = re.split(rf"^[ \t>*`-]*(?:{names})`?[ \t]*(?:[({{]|$)", rest, maxsplit=1, flags=re.M)[0].strip()
            if body:
                calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, {required[0]: body}))
    return calls


def repair_tool_calls(calls: list) -> tuple[list, int]:
    """Some local servers merge several XML-format tool calls into one call's JSON arguments, e.g.
    {"tech": "pottery</parameter>\\n</function>\\n<tool_call>\\n<function=unit_order>..."}.
    Rebuild the XML stream from the arguments and re-parse it into separate calls."""
    out, repaired = [], 0
    for c in calls:
        if not any(isinstance(v, str) and any(mk in v for mk in _LEAK_MARKERS) for v in c.args.values()):
            out.append(c)
            continue
        repaired += 1
        stream = f"<function={c.name}>" + "".join(f"<parameter={k}>\n{v if isinstance(v, str) else json.dumps(v)}"
                                                  + ("" if isinstance(v, str) and "</parameter>" in v else "</parameter>")
                                                  for k, v in c.args.items())
        parsed = parse_xml_calls(stream)
        if parsed:
            parsed[0].id = c.id  # keep the original id so the tool result still matches the native call
            out.extend(parsed)
        else:
            out.append(c)
    return out, repaired


def parse_json_calls(text: str) -> tuple[list, str]:
    """Extract {"thoughts", "calls"} from a model reply (tolerates code fences and surrounding prose)."""
    t = re.sub(r"<think>.*?</think>", "", text, flags=re.S)
    m = re.search(r"```(?:json)?\s*(\{.*\})\s*```", t, flags=re.S)
    candidate = m.group(1) if m else None
    if candidate is None:
        start, end = t.find("{"), t.rfind("}")
        candidate = t[start:end + 1] if start != -1 and end > start else ""
    try:
        data = json.loads(candidate)
    except json.JSONDecodeError:
        return [], text
    if isinstance(data, dict) and "tool" in data and "calls" not in data:
        data = {"calls": [data]}
    calls = []
    for c in (data.get("calls") or []) if isinstance(data, dict) else []:
        if isinstance(c, dict) and isinstance(c.get("tool"), str):
            calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", c["tool"], c.get("args") or {}))
    return calls, (data.get("thoughts") or "") if isinstance(data, dict) else ""
