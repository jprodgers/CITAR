"""OpenAI-compatible chat completions (OpenAI, Ollama, LM Studio, llama.cpp server, vLLM, ...).

tool_mode "native" uses the endpoint's function calling. tool_mode "json" is a fallback for local models with weak
tool calling: the model replies with a JSON object {"thoughts": "...", "calls": [{"tool": name, "args": {...}}]}.
"""
from __future__ import annotations

import json
import os
import re
import uuid

from openai import OpenAI

from .base import Conversation, StepResult, ToolCall

JSON_PROTOCOL = """
TOOL PROTOCOL (important): you cannot call functions natively. Reply ONLY with a JSON object, no other text:
{"thoughts": "<brief reasoning>", "calls": [{"tool": "<tool name>", "args": {<arguments>}}, ...]}
Use 1-8 calls per reply. Results come back in the next message. Available tools (name: description; parameters):
"""


class OpenAIConversation(Conversation):
    """A conversation with any OpenAI-compatible endpoint.

    Covers LM Studio, Ollama, llama.cpp, vLLM and the rest. Two tool modes: native calls where the
    model does them well, and a JSON mode where the model replies with a list of calls that this parses
    - which is what makes weaker local models usable at all.
    """
    def __init__(self, cfg: dict, system: str, tools: list[dict]):
        self.cfg = cfg
        self.mode = (cfg.get("tool_mode") or "native").lower()
        base_url = cfg.get("base_url") or None
        key = cfg.get("api_key") or (os.environ.get(cfg["api_key_env"]) if cfg.get("api_key_env") else None)
        if not key:
            key = os.environ.get("OPENAI_API_KEY") or ("not-needed" if base_url else None)
        self.client = OpenAI(base_url=base_url, api_key=key, timeout=float(cfg.get("timeout") or 900), max_retries=0)
        # No SDK-level retries: the agent retries connection problems itself, and silently re-sending a request that
        # timed out would restart a long generation from scratch past the turn's time limit.
        self.model = cfg.get("model") or "gpt-4o"
        self.tools = [{"type": "function", "function": {"name": t["name"], "description": t["description"],
                                                         "parameters": t["input_schema"]}} for t in tools]
        if self.mode == "json":
            listing = "\n".join(f"- {t['name']}: {t['description']} params={json.dumps(t['input_schema'].get('properties', {}))}"
                                for t in tools)
            system = system + "\n" + JSON_PROTOCOL + listing
        self.messages: list = [{"role": "system", "content": system}]
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}

    def context_info(self) -> dict | None:
        """LM Studio only: load state and context length of this conversation's model."""
        import httpx
        base = str(self.client.base_url).rstrip("/")
        root = base[:-3] if base.endswith("/v1") else base
        try:
            r = httpx.get(f"{root}/api/v0/models", timeout=3)
            if r.status_code != 200:
                return None
            for m in r.json().get("data", []):
                if m.get("id") == self.model:
                    return {"state": m.get("state"), "loaded_context": m.get("loaded_context_length"),
                            "max_context": m.get("max_context_length"),
                            "loaded_models": [x["id"] for x in r.json()["data"] if x.get("state") == "loaded"]}
        except Exception:
            return None
        return None

    def add_user_text(self, text: str):
        """Add a user message."""
        self.messages.append({"role": "user", "content": text})

    def add_tool_results(self, results: list[tuple[str, str, bool]]):
        """Add the results of tool calls, in whichever form this mode uses."""
        if self.mode == "json":
            parts = [f"Result of call {i + 1} ({tid}){' [ERROR]' if err else ''}:\n{content}"
                     for i, (tid, content, err) in enumerate(results)]
            self.messages.append({"role": "user", "content": "\n\n".join(parts)})
        else:
            for tid, content, err in results:
                self.messages.append({"role": "tool", "tool_call_id": tid, "content": ("ERROR: " if err else "") + content})

    def step(self) -> StepResult:
        """Take one step, parsing tool calls out of whatever the model produced."""
        params = {"model": self.model, "messages": self.messages}
        if self.cfg.get("temperature") is not None:
            params["temperature"] = float(self.cfg["temperature"])
        if self.cfg.get("max_tokens"):
            params["max_tokens"] = int(self.cfg["max_tokens"])
        if self.cfg.get("reasoning_effort"):
            # honoured by LM Studio and OpenAI reasoning models; ignored by servers that don't support it
            params["extra_body"] = {"reasoning_effort": self.cfg["reasoning_effort"]}
        if self.mode != "json":
            params["tools"] = self.tools
        if self.request_timeout:
            cap = float(self.cfg["timeout"]) if self.cfg.get("timeout") else None
            params["timeout"] = min(self.request_timeout, cap) if cap else self.request_timeout
        resp = self.client.chat.completions.create(**params)
        if resp.usage:
            self.usage["input_tokens"] += resp.usage.prompt_tokens or 0
            self.usage["output_tokens"] += resp.usage.completion_tokens or 0
            details = getattr(resp.usage, "completion_tokens_details", None)
            self.usage["reasoning_tokens"] += (getattr(details, "reasoning_tokens", 0) or 0) if details else 0
        choice = resp.choices[0]
        msg = choice.message
        text = msg.content or ""
        thinking = getattr(msg, "reasoning_content", None) or ""
        if self.mode == "json":
            self.messages.append({"role": "assistant", "content": text})
            calls, thoughts = parse_json_calls(text)
            malformed = 0
            if not calls and text.strip():
                # ignored the JSON protocol but wrote calls in its own trained format (e.g. GLM-style tags)
                calls = parse_text_calls(text, self.tools)
                malformed = len(calls)
            return StepResult(text=thoughts or text, thinking=thinking, tool_calls=calls, stop_reason=choice.finish_reason or "",
                              malformed=malformed)
        calls = []
        stored_calls = []
        for tc in msg.tool_calls or []:
            try:
                args = json.loads(tc.function.arguments or "{}")
                if not isinstance(args, dict):
                    args = {}
            except json.JSONDecodeError:
                args = {"__invalid_json__": tc.function.arguments}
            calls.append(ToolCall(tc.id, tc.function.name, args))
            stored_calls.append({"id": tc.id, "type": "function",
                                 "function": {"name": tc.function.name, "arguments": tc.function.arguments or "{}"}})
        malformed = sum(1 for c in calls if "__invalid_json__" in c.args)
        calls, repaired = repair_tool_calls(calls)
        malformed += repaired
        if not calls and text.strip():
            # the model wrote tool calls as text instead of using the tool-call channel
            calls = parse_text_calls(text, self.tools)
            malformed += len(calls)
            if calls:
                text_calls = [{"id": c.id, "type": "function", "function": {"name": c.name, "arguments": json.dumps(c.args)}}
                              for c in calls]
                self.messages.append({"role": "assistant", "content": text, "tool_calls": text_calls})
                return StepResult(text=text, thinking=thinking, tool_calls=calls, stop_reason=choice.finish_reason or "",
                                  malformed=malformed)
        entry = {"role": "assistant", "content": text}
        if stored_calls:
            # store the repaired calls so every tool result has a matching call id
            stored_calls = [{"id": c.id, "type": "function", "function": {"name": c.name, "arguments": json.dumps(c.args)}}
                            for c in calls]
            entry["tool_calls"] = stored_calls
        self.messages.append(entry)
        return StepResult(text=text, thinking=thinking, tool_calls=calls, stop_reason=choice.finish_reason or "",
                          malformed=malformed)


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


def parse_plain_calls(text: str, tools: list[dict]) -> list:
    """Parse tool calls a model wrote as plain text instead of using the tool-call channel (seen with Gemma), e.g.
    `end_turn{}`, `set_research {"tech": "pottery"}` or `log_thought` followed by the note on the next lines.
    Only lines that start with a known tool name count, so prose that merely mentions a tool is ignored."""
    schemas = {t["function"]["name"]: t["function"].get("parameters") or {} for t in tools}
    t = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    names = "|".join(re.escape(n) for n in sorted(schemas, key=len, reverse=True))
    if not names:
        return []
    calls = []
    decoder = json.JSONDecoder()
    for m in re.finditer(rf"^[ \t>*`-]*({names})`?[ \t]*(?=[({{]|$)", t, flags=re.M):
        name, rest = m.group(1), t[m.end():]
        if rest[:1] == "(":
            rest = rest[1:].lstrip()
        if rest[:1] == "{":
            try:
                args, _ = decoder.raw_decode(rest)
            except json.JSONDecodeError:
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
