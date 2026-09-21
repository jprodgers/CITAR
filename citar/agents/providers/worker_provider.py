"""Talking to a model on somebody else's machine, through their outbound worker connection.

To the engine this is just another `Conversation`: the turn driver calls `step()` and gets a
`StepResult`, exactly as it would for a local LM Studio. The difference is that the completion
happens on a GPU behind a home router three hops away.

Conversation state stays **here**, on the server. Every request carries the whole message list, so
the worker holds nothing between calls and can be restarted, replaced or moved mid-game without the
game noticing. It also means two workers for one server are interchangeable, which is what makes a
reconnect after a dropped link seamless.

The tool-call parsing that small local models need — XML forms, JSON protocol mode, the repair pass
for malformed calls — lives on the worker side, reusing the existing OpenAI provider. This class
does not reimplement any of it; it ships messages out and unpacks a result.
"""
from __future__ import annotations

import logging
import secrets
import time
from typing import Optional

from .base import Conversation, StepResult, ToolCall

log = logging.getLogger("citar.worker_provider")


class WorkerConnectionError(ConnectionError):
    """The machine serving this seat cannot be reached right now - its worker is disconnected, reconnecting or
    busy. The name matters: the turn driver treats "Connection" errors as something to wait out."""

    def __init__(self, message: str, refusal: str = ""):
        super().__init__(message)
        self.refusal = refusal


class WorkerConversation(Conversation):
    """A conversation whose completions are executed by a remote worker."""

    def __init__(self, cfg: dict, system: str, tools: list):
        from ...worker import protocol as P

        self.cfg = cfg
        self.server_id = cfg.get("server_id") or ""
        if not self.server_id:
            raise ValueError("A worker seat needs a server_id.")
        self.model = cfg.get("model") or ""
        self.tool_mode = (cfg.get("tool_mode") or "native").lower()
        self.activity = cfg.get("activity") or ""
        self.tools = tools
        # The same shape the OpenAI provider builds, because the worker feeds it to that provider.
        self.messages: list = [{"role": "system", "content": system}]
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
        self.request_timeout: Optional[float] = None
        self._P = P

    # ------------------------------------------------------------------ message building
    def add_user_text(self, text: str):
        """Add a user message."""
        self.messages.append({"role": "user", "content": text})

    def add_tool_results(self, results: list):
        """Add the results of tool calls."""
        if self.tool_mode == "json":
            parts = [f"Result of call {i + 1} ({tid}){' [ERROR]' if err else ''}:\n{content}"
                     for i, (tid, content, err) in enumerate(results)]
            self.messages.append({"role": "user", "content": "\n\n".join(parts)})
        else:
            for tid, content, err in results:
                self.messages.append({"role": "tool", "tool_call_id": tid,
                                      "content": ("ERROR: " if err else "") + content})

    # ------------------------------------------------------------------ the round trip
    def step(self) -> StepResult:
        """Send the request down the worker's WebSocket and wait for the answer."""
        from ...server.workers import WorkerError, hub

        params = {}
        for key in ("temperature", "max_tokens", "reasoning_effort", "top_p", "seed"):
            if self.cfg.get(key) is not None:
                params[key] = self.cfg[key]

        timeout = float(self.request_timeout or self.cfg.get("timeout") or 900)
        request = self._P.Request(
            id=secrets.token_hex(8), model=self.model, messages=self.messages,
            tools=self.tools, tool_mode=self.tool_mode, params=params,
            timeout=timeout, activity=self.activity)

        started = time.time()
        try:
            answer = hub().submit(self.server_id, request, timeout=timeout)
        except WorkerError as exc:
            # A dropped or busy worker is worth waiting for: the agent retries it for the game's
            # reconnect window and then applies the disconnect policy. Anything else - a refusal, a
            # machine closed for the night - is not a model failure, and retrying it would only make
            # the agent hammer a machine that has said no.
            if exc.retryable:
                raise WorkerConnectionError(str(exc), exc.refusal) from exc
            raise RuntimeError(str(exc)) from exc

        usage = answer.get("usage") or {}
        for field in ("input_tokens", "output_tokens", "reasoning_tokens"):
            self.usage[field] = self.usage.get(field, 0) + int(usage.get(field) or 0)

        calls = [ToolCall(id=c.get("id") or secrets.token_hex(4), name=c.get("name") or "",
                          args=c.get("args") or {})
                 for c in (answer.get("tool_calls") or [])]

        # Record the assistant turn so the next request carries the full history.
        assistant: dict = {"role": "assistant", "content": answer.get("text") or ""}
        if calls and self.tool_mode != "json":
            import json as _json
            assistant["tool_calls"] = [
                {"id": c.id, "type": "function",
                 "function": {"name": c.name, "arguments": _json.dumps(c.args)}}
                for c in calls]
        self.messages.append(assistant)

        log.debug("worker step on %s: %.1fs, %d tool calls", self.server_id,
                  time.time() - started, len(calls))
        return StepResult(text=answer.get("text") or "", thinking=answer.get("thinking") or "",
                          tool_calls=calls, stop_reason=answer.get("stop_reason") or "",
                          malformed=int(answer.get("malformed") or 0))

    def context_info(self) -> Optional[dict]:
        """What the worker last reported about this model, if anything."""
        from ...server.workers import hub

        connection = hub().get(self.server_id)
        if connection is None:
            return None
        for model in connection.models:
            if model.get("key") == self.model:
                return {"state": "loaded" if model.get("loaded") else "not-loaded",
                        "max_context": model.get("context")}
        return None

    def close(self):
        # Nothing local to close: the HTTP client lives on the worker. An in-flight request is
        # cancelled by the hub when the caller gives up.
        """Release the request slot on the worker."""
        pass
