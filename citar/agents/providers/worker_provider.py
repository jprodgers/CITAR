"""Talking to a model on somebody else's machine, through the CITAR helper's outbound connection (protocol 2).

To the engine this is just another `Conversation`: the turn driver calls `step()` and gets a `StepResult`, exactly as
it would for a local LM Studio. The difference is that the completion happens on a GPU behind a home router three hops
away, behind the helper (``crates/citar-helper``), which proxies it to its own model endpoint.

Conversation state stays **here**, on the server. Every request carries the whole message list, so the helper holds
nothing between calls and can be restarted, replaced or moved mid-game without the game noticing.

**One parser** (design/phase4-helper.md P4.5.2): the helper returns the completion raw, as its endpoint gave it, and
``completion.parse_completion`` reads its tool calls here, exactly as ``OpenAIConversation`` reads a completion it
asked for itself: native calls, the repair of malformed ones, calls written as XML, GLM, plain or Gemma 4 text, and the
JSON protocol. The request side is the same too: OpenAI function definitions (``completion.openai_tools``), and in
``json`` tool mode the system prompt with the JSON protocol and the tool listing (``completion.system_prompt``) and no
tools, which fixes protocol 1's "JSON tool mode is broken through helpers" (its helper parsed the reply itself and
never sent the protocol to the model).

Replaces: protocol 1's ``WorkerConversation``, which shipped ``tool_mode`` to the Python worker and unpacked the calls
it had parsed there.
"""
from __future__ import annotations

import logging
import secrets
import time
from typing import Optional

from .base import Conversation, StepResult
from .completion import openai_tools, parse_completion, system_prompt

log = logging.getLogger("citar.worker_provider")

#: The hub's grace beyond a request's timeout (``WorkerHub.submit``): an abandoned request drains at most this long
#: past its own timeout.
DRAIN_GRACE_SECONDS = 15.0
#: The sampling parameters a request may carry (PROTOCOL.md, "Completions"); the helper drops any other.
PARAMS = ("temperature", "max_tokens", "top_p", "seed", "reasoning_effort")


class WorkerConnectionError(ConnectionError):
    """The machine serving this seat cannot take the request right now: its helper is disconnected, reconnecting,
    busy, in its own quiet hours or stopping. The name matters: the turn driver treats "Connection" errors as something
    to wait out, and the scheduler queues a ``busy`` request again (P3.3.3)."""

    def __init__(self, message: str, refusal: str = ""):
        super().__init__(message)
        self.refusal = refusal


def usage_of(usage: Optional[dict]) -> dict:
    """A ``done`` frame's usage on the server's keys (P4.5.2): ``input_tokens`` (the prompt's, less what the endpoint
    read from its cache), ``cache_read_input_tokens``, ``output_tokens`` and ``reasoning_tokens``. OpenAI's
    ``prompt_tokens`` counts cached tokens too, while the ledger's and the scheduler's ``input_tokens`` follow
    Anthropic's meaning, which leaves them out (4-02's note), so they are moved, not counted twice."""
    if not usage:
        return {}
    prompt = int(usage.get("prompt_tokens") or 0)
    cached = min(prompt, int(usage.get("cached_tokens") or 0))
    out = {"input_tokens": prompt - cached, "output_tokens": int(usage.get("completion_tokens") or 0),
           "reasoning_tokens": int(usage.get("reasoning_tokens") or 0)}
    if cached:
        out["cache_read_input_tokens"] = cached
    return out


class WorkerConversation(Conversation):
    """A conversation whose completions are run by a helper machine's model, through its helper."""

    def __init__(self, cfg: dict, system: str, tools: list):
        self.cfg = cfg
        self.server_id = cfg.get("server_id") or ""
        if not self.server_id:
            raise ValueError("A seat on a helper machine needs a server_id.")
        self.model = cfg.get("model") or ""
        self.mode = (cfg.get("tool_mode") or "native").lower()
        self.activity = cfg.get("activity") or ""
        self.tools = openai_tools(tools)
        # the same first message OpenAIConversation sends: in json mode the protocol and the tool listing
        self.messages: list = [{"role": "system", "content": system_prompt(system, tools, self.mode)}]
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0, "cache_read_input_tokens": 0}
        self.request_timeout: Optional[float] = None
        #: The id the next request goes out under: the scheduler's (``ScheduledConversation``), so the helper's late
        #: answer frees the slot held for it; a fresh one otherwise.
        self.next_request_id: Optional[str] = None
        #: The last step's generation time as the helper reported it (``done.gen_s``) and as the server observed it
        #: (from sending the request frame to its ``done``): the scheduler meters the smaller (P3.3.4).
        self.last_reported_s: Optional[float] = None
        self.last_observed_s: Optional[float] = None
        self.last_request_id: Optional[str] = None
        #: (request id, seconds left, the connection's serial) when the last step ended without the helper's answer
        #: (cancelled, or the hub's wait ran out): its slot waits for the helper's acknowledgement (P4.5.3).
        self.unfinished: Optional[tuple] = None
        self._inflight: Optional[str] = None
        self._closed = False
        self._params_told = False

    # ------------------------------------------------------------------ message building
    def add_user_text(self, text: str):
        """Add a user message."""
        self.messages.append({"role": "user", "content": text})

    def add_tool_results(self, results: list):
        """Add the results of tool calls, in whichever form this mode uses (``OpenAIConversation``'s)."""
        if self.mode == "json":
            parts = [f"Result of call {i + 1} ({tid}){' [ERROR]' if err else ''}:\n{content}"
                     for i, (tid, content, err) in enumerate(results)]
            self.messages.append({"role": "user", "content": "\n\n".join(parts)})
        else:
            for tid, content, err in results:
                self.messages.append({"role": "tool", "tool_call_id": tid,
                                      "content": ("ERROR: " if err else "") + content})

    def params(self) -> dict:
        """The request's sampling parameters: the seat's (its member's saved overrides, a borrowing ticket's cap on
        ``max_tokens``), brought within the protocol's bounds (``helper_protocol.request_params``), the names changed
        logged once per conversation."""
        from ...server import helper_protocol as HP
        # a max_tokens of 0 or none is no cap, as OpenAIConversation reads it
        raw = {k: self.cfg[k] for k in PARAMS
               if self.cfg.get(k) is not None and not (k == "max_tokens" and not self.cfg[k])}
        params, changed = HP.request_params(raw)
        if changed and not self._params_told:
            self._params_told = True
            log.info("seat on %s: sampling parameters %s were brought within the protocol's bounds or dropped",
                     self.server_id, ", ".join(changed))
        return params

    # ------------------------------------------------------------------ the round trip
    def step(self) -> StepResult:
        """Send the request down the helper's connection, wait for its ``done``, and parse it here."""
        from ...server.workers import WorkerError, hub

        timeout = float(self.request_timeout or self.cfg.get("timeout") or 900)
        rid, self.next_request_id = self.next_request_id or secrets.token_hex(8), None
        self.unfinished = None
        self.last_reported_s = self.last_observed_s = None
        self.last_request_id = rid
        if self._closed:
            raise RuntimeError("The request was cancelled.")
        tools = self.tools if self.mode != "json" and self.tools else None

        started = time.time()
        self._inflight = rid
        try:
            answer = hub().submit(self.server_id, rid, self.model, self.messages, tools, self.params(), timeout)
        except WorkerError as exc:
            if getattr(exc, "unfinished", False):
                # sent, then given up (cancelled, or the hub's wait ran out): the helper's acknowledgement frees its
                # slot, and it can come only over the connection the request went out on
                self.unfinished = (rid, max(0.0, started + timeout - time.time()) + DRAIN_GRACE_SECONDS,
                                   getattr(exc, "connection", None))
            # A helper that cannot take it now (disconnected, busy, in its quiet hours, stopping) is worth waiting
            # for: the agent retries for the game's reconnect window, and the scheduler queues a busy request again
            # (P3.3.3). Anything else (a model it does not serve, its endpoint's failure, a timeout) is not, and
            # retrying it would only hammer a machine that has said no.
            if exc.retryable:
                raise WorkerConnectionError(str(exc), exc.refusal) from exc
            raise RuntimeError(str(exc)) from exc
        finally:
            self._inflight = None

        self.last_observed_s = answer.get("observed_s")
        try:
            self.last_reported_s = float(answer["gen_s"]) if answer.get("gen_s") is not None else None
        except (TypeError, ValueError):
            self.last_reported_s = None
        for key, n in usage_of(answer.get("usage")).items():
            self.usage[key] = self.usage.get(key, 0) + n
        result, entry = parse_completion(answer["message"], answer.get("finish_reason"), self.mode, self.tools)
        # the assistant turn as parsed, so the next request carries the whole history
        self.messages.append(entry)
        log.debug("helper step on %s: %.1fs, %d tool calls", self.server_id, time.time() - started,
                  len(result.tool_calls))
        return result

    def context_info(self) -> Optional[dict]:
        """What the helper last reported about this model, if anything."""
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
        """Cancel the request in flight, if any (non-blocking): the hub sends the helper ``cancel`` and ends the wait
        with an error; the helper stops the generation and acknowledges it, which frees the request's slot
        (``unfinished``; design/phase4-helper.md P4.5.3)."""
        self._closed = True
        rid = self._inflight
        if rid:
            from ...server.workers import hub
            hub().cancel(self.server_id, rid)
