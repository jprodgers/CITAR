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

#: The hub's grace beyond a request's timeout (``WorkerHub.submit``): an abandoned request drains at most this long
#: past its own timeout.
DRAIN_GRACE_SECONDS = 15.0


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
        #: The id the next request goes out under: the scheduler's (``ScheduledConversation``), so a late frame for it
        #: frees the slot it drains; a fresh one otherwise.
        self.next_request_id: Optional[str] = None
        #: The last step's generation time as the helper reported it (``Done.duration``) and as the server observed it
        #: (from sending the request frame to its ``Done``): the scheduler meters the smaller (P3.3.4).
        self.last_reported_s: Optional[float] = None
        self.last_observed_s: Optional[float] = None
        self.last_request_id: Optional[str] = None
        #: (request id, seconds left) when the last step ended without the helper's answer (cancelled, or the hub's
        #: wait ran out): the helper may still be generating it, so its slot drains (P3.3.3).
        self.unfinished: Optional[tuple] = None
        self._inflight: Optional[str] = None
        self._closed = False

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
        rid, self.next_request_id = self.next_request_id or secrets.token_hex(8), None
        request = self._P.Request(
            id=rid, model=self.model, messages=self.messages,
            tools=self.tools, tool_mode=self.tool_mode, params=params,
            timeout=timeout, activity=self.activity)
        self.unfinished = None
        self.last_reported_s = self.last_observed_s = None
        self.last_request_id = rid
        if self._closed:
            raise RuntimeError("The request was cancelled.")

        started = time.time()
        self._inflight = rid
        try:
            answer = hub().submit(self.server_id, request, timeout=timeout)
        except WorkerError as exc:
            if getattr(exc, "unfinished", False):
                # sent, then given up (cancelled, or the hub's wait ran out): the helper may still be generating it
                self.unfinished = (rid, max(0.0, started + timeout - time.time()) + DRAIN_GRACE_SECONDS)
            # A dropped worker is worth waiting for: the agent retries it for the game's reconnect window and then
            # applies the disconnect policy. A "busy" refusal is the scheduler's to handle (it lowers the machine's
            # capacity and queues the request again; P3.3.3). Anything else - a refusal, a machine closed for the
            # night - is not a model failure, and retrying it would only make the agent hammer a machine that has
            # said no.
            if exc.retryable:
                raise WorkerConnectionError(str(exc), exc.refusal) from exc
            raise RuntimeError(str(exc)) from exc
        finally:
            self._inflight = None

        self.last_observed_s = answer.get("observed_s")
        try:
            self.last_reported_s = float(answer.get("duration")) if answer.get("duration") is not None else None
        except (TypeError, ValueError):
            self.last_reported_s = None
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
        """Cancel the request in flight, if any (non-blocking): the hub sends the helper ``CANCEL`` and ends the wait
        with an error. Today's helper ignores ``CANCEL`` and finishes the completion, so the scheduler keeps its slot
        draining until the helper's late answer (``unfinished``; design/phase3-features.md P3.3.3)."""
        self._closed = True
        rid = self._inflight
        if rid:
            from ...server.workers import hub
            hub().cancel(self.server_id, rid)
