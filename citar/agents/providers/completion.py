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

**Hostile replies** (4-02's fix round; P4.6). From 4-05 every helper's `done.message` comes through here, so a modified
helper chooses the input: up to 8 MiB a frame. Parsing therefore takes time linear in the reply (the patterns that
rescanned the rest of the text from every candidate start are replaced by scans that never go back: `_strip_think`,
`_fenced_object`, `_glm_args`, `_PARAM_RE` without its trailing ``\\s*``, `gemma_json`'s stops), never raises on a
message protocol 2 accepts (JSON that Python refuses, by its nesting or a number's digits, is JSON that is not read),
and yields at most `MAX_CALLS` calls whose arguments are always objects nested at most `MAX_DEPTH` deep.
`tests/completion_oracle.py` keeps the code as it was, which the rewritten scans are checked against.
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

#: The most calls one completion yields, however the model wrote them: protocol 2's own bound on a `done` frame's
#: `tool_calls` (PROTOCOL.md, `done`), so calls written as text, or repaired out of one call's arguments, cannot
#: multiply what a reply costs the turn loop either. The JSON protocol asks for 1-8.
MAX_CALLS = 512
#: The deepest a call's arguments (or a JSON-protocol reply) may nest. Deeper JSON is not read: Python may decode
#: deeper than it encodes (3.14 decodes 14,000 levels and more, `json.dumps` fails past about 9,000; 3.12 stops near
#: 3,000), so such arguments would fail later, in the stored entry or the tool's signature, and differently on each
#: version. No tool's arguments come near 64.
MAX_DEPTH = 64

#: What `_loads` gives for text that is not read as JSON.
_UNREAD = object()


def _shallow(value) -> bool:
    """Whether ``value`` (decoded JSON) nests at most `MAX_DEPTH` objects and arrays deep; level by level, so a deep
    value costs no recursion and a wide one is walked once."""
    level = [value] if isinstance(value, (dict, list)) else []
    for _ in range(MAX_DEPTH):
        if not level:
            return True
        level = [v for c in level for v in (c.values() if isinstance(c, dict) else c) if isinstance(v, (dict, list))]
    return not level


def _loads(text: str):
    """``text`` decoded as JSON, or `_UNREAD`: when it is not JSON, when Python will not decode it (a number of more
    than `sys.get_int_max_str_digits` digits is a ``ValueError``, nesting past the interpreter's limit a
    ``RecursionError``), or when it nests deeper than `MAX_DEPTH`."""
    try:
        value = json.loads(text)
    except (ValueError, RecursionError):
        return _UNREAD
    return value if _shallow(value) else _UNREAD


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
    Arguments that are JSON but not an object are no arguments. At most `MAX_CALLS` calls are kept.
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
    for tc in native[:MAX_CALLS]:
        function = tc.get("function") or {}
        arguments = function.get("arguments")
        args = _loads(arguments or "{}")
        if args is _UNREAD:
            args = {"__invalid_json__": arguments}
        elif not isinstance(args, dict):
            args = {}
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
# A value's trailing whitespace stays in the match (`_coerce` strips it): a lazy value followed by ``\s*`` and the
# lookahead retried the whole run of spaces from each of its positions, quadratic in the run.
_PARAM_RE = re.compile(r"<parameter=([\w.\-]+)>\s*(.*?)(?=</parameter>|<parameter=|</function>|</tool_call>|<tool_call>|$)", re.S)
_WS = re.compile(r"\s*")


def _coerce(v: str):
    """Coerce a model's argument into the type the schema asks for.

    Models send ``"3"`` for integers and comma-separated strings for arrays constantly. Refusing them
    would measure formatting rather than play. A value Python will not convert (more digits than
    `sys.get_int_max_str_digits`, JSON `_loads` does not read) stays the text it was.
    """
    v = v.strip()
    if re.fullmatch(r"-?\d+", v):
        try:
            return int(v)
        except ValueError:
            return v
    if v.lower() in ("true", "false"):
        return v.lower() == "true"
    if v[:1] in "[{":
        value = _loads(v)
        if value is not _UNREAD:
            return value
    return v


def parse_xml_calls(text: str) -> list:
    """Parse Qwen/Hermes-style <function=name><parameter=k>v</parameter></function> tool calls from text."""
    calls = []
    for m in _FUNC_RE.finditer(text):
        name, body = m.group(1), m.group(2)
        args = {k: _coerce(v) for k, v in _PARAM_RE.findall(body)}
        calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, args))
        if len(calls) == MAX_CALLS:
            break
    return calls


_GLM_CALL_RE = re.compile(r"<tool_call>\s*([\w.\-]+)\s*(.*?)(?=</tool_call>|<tool_call>|$)", re.S)
_GLM_VALUE = re.compile(r"(.*?)(?:</arg_value>|(?=<arg_key>)|$)", re.S)


def _glm_args(body: str) -> list[tuple[str, str]]:
    """The ``(key, value)`` pairs of a GLM call's body, as ``re.findall(r"<arg_key>\\s*(.*?)\\s*</arg_key>\\s*
    <arg_value>(.*?)(?:</arg_value>|(?=<arg_key>)|$)", body, re.S)`` gives them (the code before 4-02's fix round),
    in one pass: a key runs to the first ``</arg_key>`` that a value follows, and that pattern looked for it again
    from every ``<arg_key>`` left open, and across every run of spaces before one."""
    pairs, pos = [], 0
    while (start := body.find("<arg_key>", pos)) >= 0:
        key_at = _WS.match(body, start + len("<arg_key>")).end()
        close = body.find("</arg_key>", key_at)
        while close >= 0:
            value_at = _WS.match(body, close + len("</arg_key>")).end()
            if body.startswith("<arg_value>", value_at):
                break
            close = body.find("</arg_key>", close + len("</arg_key>"))
        if close < 0:
            # no value follows any later </arg_key>, so no later <arg_key> starts a pair either
            break
        value = _GLM_VALUE.match(body, value_at + len("<arg_value>"))
        pairs.append((body[key_at:close].rstrip(), value.group(1)))
        pos = value.end()
    return pairs


def parse_glm_calls(text: str) -> list:
    """Parse GLM-style tool calls written as text (seen with Laguna):
    <tool_call>found_city<arg_key>unit_id</arg_key><arg_value>1</arg_value></tool_call>"""
    calls = []
    for m in _GLM_CALL_RE.finditer(text):
        args = {k: _coerce(v) for k, v in _glm_args(m.group(2))}
        calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", m.group(1), args))
        if len(calls) == MAX_CALLS:
            break
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
_GEMMA_KEY = re.compile(r"\s*:")
_JSON_WORDS = frozenset(("true", "false", "null", "NaN", "Infinity"))


def gemma_json(text: str, start: int = 0, stops=frozenset()) -> str | None:
    """The object at ``text[start:]`` in Gemma 4's own tool-call syntax, which LM Studio sometimes leaves in the
    reply's text (``respond_negotiation{action:<|"|>reply<|"|>,message:<|"|>Agreed.<|"|>,negotiation_id:3}``), as
    JSON text: its keys are bare words and its strings sit between ``<|"|>`` marks. None when it is not one.

    ``stops`` are positions where a bare word ends the object unread (None): `parse_plain_calls` passes the tool names
    that begin its later lines. Such a word, outside the object's strings and not a key, would stand bare in the JSON
    after a raw line break, which no decoder reads, so the result is the one it was; but an object left open above
    many calls no longer reads every one of them to the end of the reply."""
    if text[start:start + 1] != "{":
        return None
    out, i, depth = [], start, 0
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
            # a key (followed by its colon) is quoted; a bare value (true, false, null) is kept as it is
            key = _GEMMA_KEY.match(text, word.end()) is not None
            if not key and i in stops and word.group(0) not in _JSON_WORDS:
                return None
            out.append(json.dumps(word.group(0)) if key else word.group(0))
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


def _strip_think(text: str) -> str:
    """``text`` without its ``<think>...</think>`` blocks, as ``re.sub(r"<think>.*?</think>", "", text, flags=re.S)``
    gives it, in one pass: once a ``<think>`` has no ``</think>`` after it, no later one has either, where that
    pattern looked for one from every later ``<think>`` again."""
    out, i = [], 0
    while (start := text.find("<think>", i)) >= 0 and (end := text.find("</think>", start + len("<think>"))) >= 0:
        out.append(text[i:start])
        i = end + len("</think>")
    out.append(text[i:])
    return "".join(out)


def _decode_start(decoder: json.JSONDecoder, text: str):
    """The JSON value at the start of ``text`` (what follows it is ignored), or `_UNREAD` as `_loads` says."""
    try:
        value, _ = decoder.raw_decode(text)
    except (ValueError, RecursionError):
        return _UNREAD
    return value if _shallow(value) else _UNREAD


def parse_plain_calls(text: str, tools: list[dict]) -> list:
    """Parse tool calls a model wrote as plain text instead of using the tool-call channel (seen with Gemma), e.g.
    `end_turn{}`, `set_research {"tech": "pottery"}`, `log_thought` followed by the note on the next lines, or Gemma 4's
    own syntax (``call:name{key:<|"|>text<|"|>}``, ``gemma_json``). Only lines that start with a known tool name count,
    so prose that merely mentions a tool is ignored."""
    schemas = {t["function"]["name"]: t["function"].get("parameters") or {} for t in tools}
    t = _strip_think(text).strip()
    names = "|".join(re.escape(n) for n in sorted(schemas, key=len, reverse=True))
    if not names:
        return []
    lines = list(re.finditer(rf"^[ \t>*`-]*(?:<\|tool_call>)?(?:call:)?({names})`?[ \t]*(?=[({{]|$)", t, flags=re.M))
    # Each line's tool name ends a Gemma 4 object left open above it (gemma_json), and a JSON object never reaches
    # past the next line's tool name (a bare word, or a raw line break inside a string), so it is decoded from its line
    # to the next one's start: a decoding error's position is counted from the start of the text it was given. The
    # work stays linear in the reply however many lines name a tool.
    stops = frozenset(m.start(1) for m in lines)
    calls = []
    decoder = json.JSONDecoder()
    for n, m in enumerate(lines):
        if len(calls) == MAX_CALLS:
            break
        name, at = m.group(1), m.end()
        if t.startswith("(", at):
            at = _WS.match(t, at + 1).end()
        if t.startswith("{", at):
            args = _decode_start(decoder, t[at:lines[n + 1].start() if n + 1 < len(lines) else len(t)])
            if args is _UNREAD:
                converted = gemma_json(t, at, stops)
                args = _UNREAD if converted is None else _loads(converted)
                if args is _UNREAD:
                    continue
            if isinstance(args, dict):
                calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, args))
            continue
        schema = schemas[name]
        required = schema.get("required") or []
        if not required:
            calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", name, {}))
        elif not calls and m.start() == 0 and len(required) == 1 \
                and (schema.get("properties") or {}).get(required[0], {}).get("type") == "string":
            # a bare tool name on the first line, followed by the value of its only (string) parameter
            rest = "" if t[at:at + 2] in ("()", ")") else t[at:]
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
        if len(out) >= MAX_CALLS:
            break
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
    return out[:MAX_CALLS], repaired


_FENCE_END = re.compile(r"\}\s*```")
_FENCE_START = re.compile(r"```(?:json)?\s*\{")


def _fenced_object(t: str) -> str | None:
    """The object in a code fence, as ``re.search(r"```(?:json)?\\s*(\\{.*\\})\\s*```", t, re.S)`` finds it, in one pass:
    from the brace of the first fence that opens on one to the last brace that closes a fence after it. (That pattern
    ran to the end of the text and back from every fence that opened on a brace.)"""
    end = None
    for end in _FENCE_END.finditer(t):
        pass
    if end is None:
        return None
    start = _FENCE_START.search(t, 0, end.start())
    return t[start.end() - 1:end.start() + 1] if start else None


def parse_json_calls(text: str) -> tuple[list, str]:
    """Extract {"thoughts", "calls"} from a model reply (tolerates code fences and surrounding prose).

    Only a list of calls is read; a call's ``args`` that are not an object are no arguments (as a native call's), and
    ``thoughts`` that are not text are none."""
    t = _strip_think(text)
    candidate = _fenced_object(t)
    if candidate is None:
        start, end = t.find("{"), t.rfind("}")
        candidate = t[start:end + 1] if start != -1 and end > start else ""
    data = _loads(candidate)
    if data is _UNREAD:
        return [], text
    if isinstance(data, dict) and "tool" in data and "calls" not in data:
        data = {"calls": [data]}
    if not isinstance(data, dict):
        return [], ""
    listed = data.get("calls")
    calls = []
    for c in listed if isinstance(listed, list) else []:
        if isinstance(c, dict) and isinstance(c.get("tool"), str):
            args = c.get("args")
            calls.append(ToolCall(f"call_{uuid.uuid4().hex[:8]}", c["tool"], args if isinstance(args, dict) else {}))
            if len(calls) == MAX_CALLS:
                break
    thoughts = data.get("thoughts")
    return calls, thoughts if isinstance(thoughts, str) else ""
