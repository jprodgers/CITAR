"""OpenAI-compatible chat completions (OpenAI, Ollama, LM Studio, llama.cpp server, vLLM, ...).

tool_mode "native" uses the endpoint's function calling. tool_mode "json" is a fallback for local models with weak
tool calling: the model replies with a JSON object {"thoughts": "...", "calls": [{"tool": name, "args": {...}}]}.

What a reply means (tool calls in either mode, and every malformed shape models produce) is `completion.py`'s, the one
parser a helper's completions also go through (design/phase4-helper.md P4.5.2): this module only talks to the endpoint.
"""
from __future__ import annotations

import os

from openai import OpenAI

from .base import Conversation, StepResult
from .completion import openai_tools, parse_completion, system_prompt


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
        self.tools = openai_tools(tools)
        self.messages: list = [{"role": "system", "content": system_prompt(system, tools, self.mode)}]
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
        if self.mode != "json" and self.tools:
            # a plain completion (a report's narrative) sends none: the API refuses an empty list
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
        result, entry = parse_completion(message_dict(choice.message), choice.finish_reason, self.mode, self.tools)
        self.messages.append(entry)
        return result


def message_dict(msg) -> dict:
    """The SDK's message as `parse_completion` reads it, the shape of protocol 2's `done.message`: its text, its
    reasoning (``reasoning_content``, where LM Studio and DeepSeek put it) and its tool calls as the endpoint sent
    them."""
    return {"content": msg.content, "reasoning": getattr(msg, "reasoning_content", None),
            "tool_calls": [{"id": tc.id, "type": "function",
                            "function": {"name": tc.function.name, "arguments": tc.function.arguments}}
                           for tc in msg.tool_calls or []]}
