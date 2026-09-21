"""Claude via the official Anthropic SDK: manual tool-use loop with prompt caching and adaptive thinking."""
from __future__ import annotations


import anthropic

from .base import Conversation, StepResult, ToolCall

DEFAULT_MODEL = "claude-opus-5"
FALLBACK_MODELS = ("claude-opus-5", "claude-fable-5-1")


class AnthropicConversation(Conversation):
    """A conversation with a Claude model through the Anthropic API.

    Uses extended thinking, with the summaries recorded as the seat's visible reasoning, and prompt
    caching - which matters here more than usual, because a turn briefing repeats most of its content
    from turn to turn and the cached prefix is most of the request.
    """
    def __init__(self, cfg: dict, system: str, tools: list[dict]):
        self.cfg = cfg
        self.model = cfg.get("model") or DEFAULT_MODEL
        kwargs = {}
        if cfg.get("api_key"):
            kwargs["api_key"] = cfg["api_key"]
        elif cfg.get("api_key_env"):
            import os
            key = os.environ.get(cfg["api_key_env"])
            if key:
                kwargs["api_key"] = key
        if cfg.get("base_url"):
            kwargs["base_url"] = cfg["base_url"]
        self.client = anthropic.Anthropic(max_retries=4, **kwargs)
        self.system = system
        # Deterministic tool order keeps the cached prefix stable across requests and turns.
        self.tools = [{"name": t["name"], "description": t["description"], "input_schema": t["input_schema"]} for t in tools]
        self.messages: list = []
        self.pending_results: list = []
        self.usage = {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}

    def add_user_text(self, text: str):
        """Add a user message."""
        content = list(self.pending_results) + [{"type": "text", "text": text}]
        self.pending_results = []
        self.messages.append({"role": "user", "content": content})

    def add_tool_results(self, results: list[tuple[str, str, bool]]):
        """Add the results of the tool calls the model asked for."""
        blocks = [{"type": "tool_result", "tool_use_id": tid, "content": content, **({"is_error": True} if err else {})}
                  for tid, content, err in results]
        self.messages.append({"role": "user", "content": blocks})

    def step(self) -> StepResult:
        """Take one step: send what we have, return what the model said and asked for."""
        params = {
            "model": self.model,
            "max_tokens": int(self.cfg.get("max_tokens") or 16000),
            "system": self.system,
            "tools": self.tools,
            "messages": self.messages,
            "cache_control": {"type": "ephemeral"},
            "thinking": {"type": "adaptive", "display": "summarized"},
        }
        if self.cfg.get("effort"):
            params["output_config"] = {"effort": self.cfg["effort"]}
        client = self.client
        if self.request_timeout and hasattr(client, "with_options"):
            # the turn's remaining time bounds this request; a timed-out request is not retried past that
            client = client.with_options(timeout=self.request_timeout, max_retries=0)
        use_fallbacks = self.model in FALLBACK_MODELS and self.cfg.get("fallbacks", True)
        if use_fallbacks:
            response = client.beta.messages.create(
                betas=["server-side-fallback-2026-07-01"], fallbacks="default", **params)
        else:
            response = client.messages.create(**params)

        u = response.usage
        for k in self.usage:
            self.usage[k] += getattr(u, k, 0) or 0
        # with server-side fallbacks another model may have served (and billed) this request
        self.served_model = getattr(response, "model", None) or self.model

        texts, thinking, calls = [], [], []
        for block in response.content:
            if block.type == "text":
                texts.append(block.text)
            elif block.type == "thinking" and getattr(block, "thinking", ""):
                thinking.append(block.thinking)
            elif block.type == "tool_use":
                calls.append(ToolCall(block.id, block.name, block.input if isinstance(block.input, dict) else {}))

        if response.stop_reason == "max_tokens":
            # A truncated response may contain an incomplete tool call; don't keep it.
            self.messages.append({"role": "user", "content": [{"type": "text", "text":
                "(Your previous response was cut off by the output limit and was discarded. Be more concise and make "
                "fewer tool calls per response.)"}]})
            return StepResult(text="\n".join(texts), thinking="\n".join(thinking), tool_calls=[], stop_reason="max_tokens")

        # Append the full content (including thinking blocks) unchanged: append-only history.
        self.messages.append({"role": "assistant", "content": response.content})
        return StepResult(text="\n".join(texts), thinking="\n".join(thinking), tool_calls=calls,
                          stop_reason=response.stop_reason or "")
