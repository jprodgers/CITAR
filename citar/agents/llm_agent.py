"""Drives a language model through a turn (or a diplomatic interrupt) using the shared tool registry.

Includes guard rails for weaker models: identical actions are not re-executed within a turn, unchanged queries are
flagged, a short "turn progress" note follows every batch of actions, and stalled or runaway turns are ended.
Every model step and tool call is recorded in the session metrics.

Every request the seat makes is one of its lease's (``GameSession.lease_for``; design/phase3-features.md P3.3.3): the
scheduler gives it a slot on its machine, so a busy machine queues it rather than refusing it, and the agent tells the
lease when its seat is blocked on something other than the machine (a chat reply, a pause), so the slot is lent
meanwhile. Time spent queueing for the machine and waiting on chats is the agent's ``_waited``: it never counts against
the turn's ``max_turn_seconds`` (P3.3.4), and the request's timeout is set only once the slot is granted.

**Chats** (design/phase3-features.md P3.5.2, P3.5.3). A chat the seat answers out of its turn has a conversation of its
own (``ChatThread``), kept by the agent for as long as the chat is open: each exchange tells the model what changed
since its last answer, and runs on the seat's lease until the chat moves. The chat clock (``citar.server.chat``) is the
chat timer: the agent's waits for the other side (``_await_answer``, the tool's own wait; ``_end_turn``, until every
chat it is in has moved or closed) end when the clock closes a chat, with a safety cap of twice the game's longest chat
timeout. An answer the other side gives after the tool's wait is put in front of the model before its next step
(``_chat_news``), worded by where the chat stands.
"""
from __future__ import annotations

import json
import threading
import time
import traceback
from typing import Optional

from .. import engine_api
from ..server.metrics import call_signature, ACTION_REPEAT_EXEMPT
from .prompts import (system_prompt, TURN_START, FIRST_TURN_NOTE, chat_system_prompt, CHAT_PROMPT, CHAT_UPDATE,
                      entry_text, late_note, proposal_text, time_left_text)
from .providers import make_conversation

#: The tools of a chat's conversation (P3.5.3): enough to look up what an answer needs and to answer.
CHAT_TOOLS = {"get_diplomacy", "get_empire", "get_players", "get_victory_status", "read_notes", "write_notes",
              "log_thought", "respond_negotiation", "send_message"}
#: How long a message the model sends in a chat waits for the answer inside the tool call, by default, before the turn
#: goes on (``negotiation_wait_seconds``): the answer then reaches the model as a NEGOTIATION UPDATE before its next
#: step, and end_turn waits for it.
DEFAULT_REPLY_WAIT_SECONDS = 60.0
MAX_RESULT_CHARS = 14000


class _Halted(Exception):
    """Raised internally when the game is closed or the seat's controller changes mid-turn."""


class _TimeUp(Exception):
    """Raised internally when the turn's time budget runs out, including while a model request is in progress."""


class _Disconnected(Exception):
    """Raised internally when the model's server stayed unreachable for the whole reconnect window."""


# How long a seat keeps trying to reach its model's server before the game's disconnect policy applies
# (pause the game, or skip the turn). Overridden per game (config "reconnect_seconds") or per seat.
DEFAULT_RECONNECT_SECONDS = 180
DISCONNECT_POLICIES = ("pause", "skip")
# Failures worth retrying: the server may come back. The first group is "cannot reach it at all" - a dropped
# connection or a worker that went away - which is what the disconnect policy is about.
_UNREACHABLE = ("Connection", "ConnectTimeout", "ServiceUnavailable", "Unavailable")
_TRANSIENT = _UNREACHABLE + ("Timeout", "RateLimit", "InternalServer", "Overloaded")


def _is_unreachable(e: Exception) -> bool:
    """Whether an error means the model's server could not be reached (rather than that it answered badly)."""
    return any(k in type(e).__name__ for k in _UNREACHABLE)


def reachable(cfg: dict, timeout: float = 5.0) -> bool:
    """A cheap check that a seat's model server answers at all, used to resume a game paused by a disconnect.

    A worker seat is reachable when its worker is connected. Anything else is asked for its model list:
    any HTTP answer below 500 - even 401 - means the server is back.
    """
    provider = (cfg.get("provider") or "anthropic").lower()
    if provider == "worker":
        from ..server.workers import hub
        return hub().is_online(cfg.get("server_id") or "")
    if provider == "dryrun":
        return True
    base = cfg.get("base_url") or {"anthropic": "https://api.anthropic.com/v1",
                                   "openai": "https://api.openai.com/v1"}.get(provider)
    if not base:
        return False
    try:
        import httpx
        return httpx.get(base.rstrip("/") + "/models", timeout=timeout).status_code < 500
    except Exception:
        return False


def _tool_defs(names: set | None = None) -> list[dict]:
    """The tool definitions sent to a model, from the one registry."""
    return [{"name": t["name"], "description": t["description"], "input_schema": t["input_schema"]}
            for t in engine_api.tool_list() if names is None or t["name"] in names]


def _serialize(result) -> str:
    """A tool result as the text a model sees."""
    if isinstance(result, str):
        text = result
    else:
        text = json.dumps(result, ensure_ascii=False, default=str)
    if len(text) > MAX_RESULT_CHARS:
        text = text[:MAX_RESULT_CHARS] + "\n...(truncated; ask for something more specific)"
    return text


class _TurnState:
    """What has happened during one turn: calls made, errors, repeats and progress.

    The state the guard rails consult. Per turn rather than per game, because a model that loops within
    a turn and behaves across the game is a specific, fixable problem.
    """
    def __init__(self):
        self.done: dict[str, int] = {}          # signature -> successful executions
        self.queries: dict[str, tuple] = {}     # signature -> (session version, times asked, result text)
        self.steps_without_progress = 0
        self.nudged_stall = False
        self.unit_moves: dict[str, int] = {}


class LLMAgent:
    """Drives one seat with a language model: briefing, tool calls, limits and metrics.

    Everything that shapes play lives here rather than in a provider, which is what makes comparing two
    models a comparison of the models: the briefing, the guard rails, the step and time limits and the
    measurement are identical whether the model is local or hosted.

    The guard rails exist because weak models fail in characteristic ways - repeating a successful
    action, re-asking an unchanged question, emitting a tool call as prose. Each is handled rather than
    punished, so that a score reflects how well the model played and not how well it formatted.
    """
    def __init__(self, cfg: dict, binding=None):
        self.cfg = dict(cfg)
        #: The seat's lease (``citar.pool.scheduler.Lease``): every request of the seat is one of its, in its turn or
        #: answering a chat out of turn. None: the calls go to the provider unscheduled (a caller with no session).
        self.binding = binding
        #: The clock the turn's time budget is kept on (a test's fake clock replaces it).
        self._clock = time.time
        self.max_calls = int(self.cfg.get("max_tool_calls_per_turn") or 150)
        self.max_steps = int(self.cfg.get("max_steps_per_turn") or 60)
        self.max_turn_seconds = float(self.cfg.get("max_turn_seconds") if self.cfg.get("max_turn_seconds") is not None else 1800)
        self.stall_steps = int(self.cfg.get("stall_steps") or 10)
        self.max_negotiation_calls = int(self.cfg.get("max_negotiation_calls") or 20)
        #: the tool's own wait for an answer (``_await_answer``)
        self.reply_wait = float(self.cfg.get("negotiation_wait_seconds") or DEFAULT_REPLY_WAIT_SECONDS)
        #: the safety cap on the turn's waits for chats, None for the game's (``_wait_cap``)
        self.wait_cap: float | None = None
        self.usage_total: dict = {}
        self.last_error: str | None = None
        self.cancelled = False
        self._active: set = set()
        self._waited = 0.0          # seconds this turn spent waiting for the server to come back (not counted against it)
        # this seat's model calls waiting for their machine now (its turn's and its chat answers'), for its status
        self._machine_waits = 0
        self._waits_lock = threading.Lock()
        #: the chats the seat answers, one conversation each (``ChatThread``), by negotiation id
        self._threads: dict[int, ChatThread] = {}
        self._threads_lock = threading.Lock()
        #: the chats the turn's model knows of and where each stood when it last heard (``_chat_news``)
        self._marks: dict = {}

    # ------------------------------------------------------------------
    def cancel(self):
        """Abort any turn or negotiation in progress, including in-flight model requests."""
        self.cancelled = True
        for conv in list(self._active):
            try:
                conv.close()
            except Exception:
                pass
        with self._threads_lock:
            threads = list(self._threads.values())
        for t in threads:
            t.cancel()

    def cancel_chat(self, nid: int) -> None:
        """Stop the answer in progress for chat ``nid`` (the chat clock, at its deadline; non-blocking, allowed under
        the session's lock): its conversation's request is cancelled, and the exchange returns without answering."""
        with self._threads_lock:
            t = self._threads.get(nid)
        if t is not None:
            t.cancel()

    def chat_closed(self, nid: int) -> None:
        """Chat ``nid`` closed: its conversation is let go (non-blocking)."""
        with self._threads_lock:
            t = self._threads.pop(nid, None)
        if t is not None:
            t.close()

    def chat_thread(self, nid: int) -> "ChatThread":
        """The conversation of chat ``nid``, made on first use (``ChatThread``)."""
        with self._threads_lock:
            t = self._threads.get(nid)
            if t is None or t.closed:
                t = self._threads[nid] = ChatThread(self, nid)
            return t

    def _halted(self, session) -> bool:
        """Whether the game has stopped and this turn should be abandoned."""
        return self.cancelled or session.stopped

    def reconnect_seconds(self, session) -> float:
        """How long to keep trying an unreachable server: the seat's setting, else the game's, else the default."""
        for v in (self.cfg.get("reconnect_seconds"), session.game.config.get("reconnect_seconds")):
            try:
                if v not in (None, ""):
                    return max(0.0, float(v))
            except (TypeError, ValueError):
                pass
        return float(DEFAULT_RECONNECT_SECONDS)

    def disconnect_policy(self, session) -> str:
        """What happens when the server stays unreachable: "pause" the game or "skip" this seat's turn."""
        for v in (self.cfg.get("on_disconnect"), session.game.config.get("on_disconnect")):
            if v in DISCONNECT_POLICIES:
                return v
        return "pause"

    def _status(self, session, pid: int, status: str):
        """Tell the game screen what the seat is doing ("thinking", "reconnecting")."""
        session.agent_status[pid] = status
        session._broadcast({"type": "agent", "player": pid, "status": status})

    def _waiting_for_machine(self, session, pid: int, place):
        """A model call of the seat waits for its machine (``place``: its ``position``, ``eta_s`` and the ``machine``'s
        name; ``ScheduledConversation.on_wait``, on the waiting thread), or no longer does (None). A seat mid-turn shows
        "queued" (with its place) on the game screen while any of its calls waits, and "thinking" again once none
        does, as it showed a helper's busy answers before the scheduler. A chat answered out of turn leaves the seat's
        status as it is."""
        with self._waits_lock:
            self._machine_waits = max(0, self._machine_waits + (1 if place is not None else -1))
            first, last = place is not None and self._machine_waits == 1, place is None and self._machine_waits == 0
            status = session.agent_status.get(pid)
            if first and status == "thinking":
                session.agent_status[pid] = "queued"
                note = {"type": "agent", "player": pid, "status": "queued", "machine": place.get("machine"),
                        "position": place.get("position"), "eta_s": place.get("eta_s")}
            elif last and status == "queued":
                session.agent_status[pid] = "thinking"
                note = {"type": "agent", "player": pid, "status": "thinking"}
            else:
                return
        session._broadcast(note)

    def _step(self, session, pid: int, conv, deadline: float | None = None, chat: bool = False):
        """One model call (timed and metered), riding out connection problems.

        On a scheduled machine the call first waits for its lease's slot (``ScheduledConversation``): a machine busy
        with someone else's work is a queue, not a failure, and its ``busy`` refusals are the scheduler's to absorb
        (no sleep, nothing metered). The time queued is added to ``_waited``, so it never counts against the turn, and
        the request's timeout is set only once the slot is granted, from the turn's time left then (``budget``).

        A transient failure is retried with backoff for up to the reconnect window, so a server that drops for
        a few seconds - a worker reconnecting, LM Studio restarting, Wi-Fi blinking - costs nothing but the wait.
        Time spent waiting does not count against the turn. If the server is still unreachable when the window
        closes, _Disconnected is raised and the game's disconnect policy decides what happens next.

        With a deadline (end of the turn's time budget) the request may only use the time left, and running out
        of time raises _TimeUp instead of an error. The step's metrics carry its generation time and, apart, the time
        it waited for the machine; the usage ledger is metered by the scheduler's listener. A ``chat`` step (an answer
        out of the turn's loop) adds nothing to the turn's ``_waited``."""
        delays = [2, 3, 5, 10, 15, 20, 30]
        first_failure = None
        attempt = 0
        scheduled = hasattr(conv, "budget")
        if scheduled:
            conv.halted = lambda: self._halted(session)
            conv.on_wait = lambda place: self._waiting_for_machine(session, pid, place)
        while True:
            if self._halted(session):
                raise _Halted()
            if deadline is not None:
                remaining = deadline + self._waited - self._clock()
                if remaining <= 5:
                    raise _TimeUp()
                if scheduled:
                    conv.budget = lambda queued, end=deadline: self._time_left(end, queued)
                else:
                    conv.request_timeout = remaining
            before = dict(getattr(conv, "usage", {}))
            t0 = time.perf_counter()
            try:
                step = conv.step()
                usage = getattr(conv, "usage", {})
                d = {k: usage.get(k, 0) - before.get(k, 0) for k in usage}
                queue_s = float(getattr(conv, "last_queue_s", 0.0) or 0.0) if scheduled else 0.0
                gen_s = float(conv.last_gen_s) if scheduled else time.perf_counter() - t0
                if not chat:
                    self._waited += queue_s
                session.metrics.model_step(
                    pid, gen_s, queue_s=queue_s, input_tokens=d.get("input_tokens", 0),
                    output_tokens=d.get("output_tokens", 0), reasoning_tokens=d.get("reasoning_tokens", 0),
                    malformed=step.malformed)
                if first_failure is not None:
                    self._thought(session, pid, f"(reconnected after {time.time() - first_failure:.0f}s)", "system")
                    self._status(session, pid, "thinking")
                return step
            except _TimeUp:
                if not chat:
                    self._waited += float(getattr(conv, "last_queue_s", 0.0) or 0.0) if scheduled else 0.0
                raise
            except Exception as e:
                queued = float(getattr(conv, "last_queue_s", 0.0) or 0.0) if scheduled else 0.0
                if not chat:
                    self._waited += queued
                if self._halted(session):
                    raise _Halted()
                name = type(e).__name__
                unreachable = _is_unreachable(e)
                if not unreachable and deadline is not None and self._clock() >= deadline + self._waited - 5:
                    session.metrics.model_step(pid, max(0.0, time.perf_counter() - t0 - queued))
                    raise _TimeUp()
                if not any(k in name for k in _TRANSIENT):
                    raise
                now = time.time()
                if first_failure is None:
                    first_failure = now
                window = self.reconnect_seconds(session)
                if now - first_failure >= window:
                    if unreachable:
                        raise _Disconnected(f"{name}: {e}") from e
                    raise
                delay = min(delays[min(attempt, len(delays) - 1)], max(1.0, window - (now - first_failure)))
                attempt += 1
                if unreachable:
                    self._status(session, pid, "reconnecting")
                self._thought(session, pid, f"(model call failed: {name}: {e}; retrying in {delay:.0f}s — will keep "
                                            f"trying for {window - (now - first_failure):.0f}s more)", "system")
                slept = 0.0
                while slept < delay:
                    if self._halted(session):
                        raise _Halted()
                    # a worker that reconnects is tried straight away rather than at the end of the backoff
                    if self.cfg.get("provider") == "worker" and slept >= 1 and reachable(self.cfg):
                        break
                    time.sleep(0.5)
                    slept += 0.5
                if unreachable and not chat:
                    self._waited += time.time() - now
                    if self.cfg.get("provider") != "worker" or reachable(self.cfg):
                        # back to "thinking" as the retry goes out: a reply can take minutes, and the game
                        # screen should not say "reconnecting" all that time
                        self._status(session, pid, "thinking")

    def _time_left(self, deadline: float, queued: float) -> float:
        """The request's timeout once its slot is granted (``ScheduledConversation.budget``): what is left of the turn,
        the time just queued for the machine not counted against it. Raises _TimeUp when nothing worth a request is."""
        remaining = deadline + self._waited + queued - self._clock()
        if remaining <= 5:
            raise _TimeUp()
        return remaining

    def _idle(self):
        """The seat is blocked on something other than the machine (a chat reply, a pause): its lease lends its slot."""
        if self.binding is not None and hasattr(self.binding, "idle"):
            self.binding.idle()

    def _activate(self):
        """The seat goes on with its turn: its lease's slot is reserved from pooled prompts again."""
        if self.binding is not None and hasattr(self.binding, "activate"):
            self.binding.activate()

    def _ensure_loaded(self, session, pid: int):
        """Once per agent: load the seat's model with its load profile on a server CITAR manages (lobby games; the
        benchmark and probe runners load models themselves before starting)."""
        if getattr(self, "_load_checked", False) or not self.cfg.get("server_id") or not self.cfg.get("load"):
            return
        self._load_checked = True
        from .. import servers
        try:
            sv = servers.get(self.cfg["server_id"])
            secs = servers.ensure_model(sv, self.cfg["model"], self.cfg["load"])
            if secs:
                self._thought(session, pid, f"(loaded {self.cfg['model']} on {sv['name']} with profile "
                                            f"'{self.cfg['load'].get('name')}' in {secs:.0f}s)", "system")
        except Exception as e:
            self._thought(session, pid, f"(could not load {self.cfg.get('model')}: {e}; trying anyway)", "system")

    def _report_failure(self, session, pid: int, e: Exception):
        """Record a failure against the seat, where the lobby will show it."""
        where = self.cfg.get("base_url") or self.cfg.get("provider") or "the model"
        self.last_error = f"{type(e).__name__}: {e}"
        low = str(e).lower()
        if any(k in low for k in ("terminated", "context", "too long", "n_ctx", "exceeds")):
            self.last_error += (" — this usually means the prompt outgrew the model's context window; load the model "
                                "with a larger context length (32k+ recommended).")
        self._thought(session, pid, f"(AI error: {self.last_error})", "system")
        with session.lock:
            g = session.game
            g.emit("agent_error", f"{g.player_name(pid)}'s AI could not play ({self.cfg.get('model') or '?'} at {where}): "
                                  f"{self.last_error[:300]}. Its turn was skipped — check the seat settings.", None, player=pid)

    def _conversation(self, names=None):
        """Start a conversation with the provider, with the tools this turn may use, on the seat's lease (the provider's
        conversation wrapped as ``make_conversation(..., binding=)`` wraps it: every step takes the lease's slot)."""
        conv = make_conversation(self.cfg, system_prompt(self.cfg.get("persona")), _tool_defs(names))
        if self.binding is None:
            return conv
        from ..pool.scheduler import ScheduledConversation
        return ScheduledConversation(conv, self.binding, self.cfg)

    def _record_usage(self, conv):
        """Write this turn's token and timing usage to the ledger."""
        for k, v in getattr(conv, "usage", {}).items():
            self.usage_total[k] = self.usage_total.get(k, 0) + v

    def _thought(self, session, pid: int, text: str, kind: str = "reasoning"):
        """Record the model's reasoning for spectators and the replay."""
        text = (text or "").strip()
        if not text:
            return
        with session.lock:
            session.game.add_thought(pid, text[:6000], kind)
        session._broadcast({"type": "thought", "player": pid, "turn": session.game.turn, "text": text[:6000], "kind": kind})

    def _end_reason(self, session, pid: int, reason: str):
        """Record how the turn ended, which is half of what the metrics are for."""
        session.metrics.set_end_reason(pid, reason)

    # ------------------------------------------------------------------
    def _run_calls(self, session, pid: int, conv, calls, state: _TurnState) -> tuple[bool, int, int]:
        """Execute tool calls. Returns (ended_turn, calls_executed, successful_new_actions)."""
        results = []
        ended = False
        progress = 0
        for c in calls:
            if self._halted(session):
                raise _Halted()
            kind = engine_api.tool_kind(c.name)
            if "__invalid_json__" in c.args:
                results.append((c.id, "Error: your tool arguments were not valid JSON.", True))
                session.metrics.tool_call(pid, c.name, {}, kind or "unknown", False, 0.0, "invalid JSON arguments")
                continue
            sig = call_signature(c.name, c.args)
            # --- repeated identical action: don't redo it ---------------------------------------
            if kind == "action" and c.name not in ACTION_REPEAT_EXEMPT and state.done.get(sig):
                n = state.done[sig] = state.done[sig] + 1
                msg = (f"Skipped: you already did exactly this earlier this turn (it succeeded; this is attempt {n}). "
                       f"Nothing needs redoing — check the TURN PROGRESS note and give different orders, or call end_turn.")
                results.append((c.id, msg, True))
                session.metrics.tool_call(pid, c.name, c.args, "action", False, 0.0, "blocked identical repeat",
                                          blocked_repeat=True)
                self._thought(session, pid, f"{c.name} {json.dumps(c.args, ensure_ascii=False)[:200]} → blocked repeat #{n}", "action")
                continue
            # --- repeated identical query with no state change: answer from cache, flag it ---------
            if kind == "query" and sig in state.queries:
                version, times, text = state.queries[sig]
                if version == session.version:
                    state.queries[sig] = (version, times + 1, text)
                    session.metrics.tool_call(pid, c.name, c.args, "query", True, 0.0)
                    results.append((c.id, f"(Unchanged — you already asked this {times} time(s) this turn and nothing has "
                                          f"changed since.)\n{text}", False))
                    continue
            # --- many different move orders for the same unit in one turn: it's probably stuck -----------
            if c.name == "move_unit" and "unit_id" in c.args:
                uid = str(c.args.get("unit_id"))
                state.unit_moves[uid] = state.unit_moves.get(uid, 0) + 1
                if state.unit_moves[uid] > 4:
                    msg = (f"Skipped: this is move order #{state.unit_moves[uid]} for unit {uid} this turn. Stop retrying "
                           f"moves for it — call get_unit({uid}) to see its remaining moves and reachable tiles, give it a "
                           f"different order (fortify/sleep/explore), or leave it until next turn.")
                    results.append((c.id, msg, True))
                    session.metrics.tool_call(pid, c.name, c.args, "action", False, 0.0, "blocked: too many move orders",
                                              blocked_repeat=True)
                    continue
            if c.name == "end_turn":
                res = self._end_turn(session, pid)
            else:
                res = session.call_tool(pid, c.name, c.args)
                if res["ok"] and c.name in ("open_negotiation", "respond_negotiation"):
                    res = {"ok": True, "result": self._await_answer(session, pid, c.args, res["result"])}
            if res["ok"]:
                text = _serialize(res["result"])
                results.append((c.id, text, False))
                if c.name == "end_turn":
                    ended = True
                if kind == "query":
                    state.queries[sig] = (session.version, 1, text)
                elif kind == "action":
                    state.done[sig] = state.done.get(sig, 0) + 1
                    r = res["result"]
                    no_op = c.name == "move_unit" and isinstance(r, dict) and r.get("from") == r.get("to") and "rebased_to" not in r
                    if c.name not in ("log_thought", "write_notes") and not no_op:
                        progress += 1
            else:
                results.append((c.id, "Error: " + res["error"], True))
            if c.name != "log_thought" and kind in (None, "action"):
                args = json.dumps(c.args, ensure_ascii=False)[:200]
                outcome = "ok" if res["ok"] else f"ERROR: {res['error'][:200]}"
                self._thought(session, pid, f"{c.name} {args} → {outcome}", "action")
        conv.add_tool_results(results)
        return ended, len(calls), progress

    # Waiting for the other side of a chat. The chat clock (citar.server.chat) times every chat by the side it waits
    # on and closes it, or declines for a silent model, at its deadline: these waits end when the chat moves or closes.
    # Each keeps a safety cap of twice the game's longest chat timeout (ChatClock.safety_cap) of unpaused time, in case
    # the clock is not running, after which _end_turn closes the chats itself. Time the game spends paused does not
    # count, and none of it is the model's (_waited).
    def _await_answer(self, session, pid: int, args: dict, result: dict) -> dict:
        """After the model spoke in a chat: wait for the other side's answer, for the tool's own wait at most
        (``reply_wait``; the chat clock may close the chat sooner), and add it to the result. An answer that comes
        later reaches the model before its next step (``_chat_news``)."""
        nid = result.get("negotiation_id") if isinstance(result, dict) else None
        try:
            nid = int(nid or args.get("negotiation_id"))
        except (TypeError, ValueError):
            return result
        with session.lock:
            n = session.game.negotiation_head(nid)
            if n["status"] == "open" and n["awaiting"] != pid:
                # waiting on the other side: the seat's slot is lent meanwhile (the counterpart may need it to answer),
                # and the wait is not the model's time (P3.3.4)
                self._idle()
                t = self._clock()
                try:
                    session._await_reply(pid, nid, min(self.reply_wait, self._wait_cap(session)),
                                         halted=lambda: self._halted(session), hold_paused=True)
                finally:
                    self._waited += max(0.0, self._clock() - t)
                    self._activate()
            result = session._reply_result(pid, nid, result)
            # what the result told the model, of this chat only: any other chat that moved meanwhile (while the model
            # generated this step, or during the wait) is still news for its next step (``_chat_news``)
            self._heard(session, pid, nid)
        if self._halted(session):
            raise _Halted()
        return result

    def _heard(self, session, pid: int, nid: int) -> None:
        """The model has been told where chat ``nid`` stands now: its mark is brought up to date (forgotten once the
        chat is closed). Session's lock held."""
        try:
            n = session.game.negotiation_head(nid)
        except engine_api.ActionError:
            n = None
        if n is not None and n["status"] == "open":
            self._marks[nid] = (n["status"], n["awaiting"], n["entries"], n["initiator"])
        else:
            self._marks.pop(nid, None)

    def _wait_cap(self, session) -> float:
        """The safety cap on the turn's waits for chats: ``wait_cap`` when set (a probe's turn case, whose other seats
        never answer: 0), else the game's (``ChatClock.safety_cap``)."""
        return float(self.wait_cap) if self.wait_cap is not None else session.chat.safety_cap()

    def _end_turn(self, session, pid: int) -> dict:
        """End the turn, once this seat's chats let it.

        The end_turn tool refuses while a chat the seat is in is open. When they all wait on the other side, that is
        the harness's wait rather than the model's mistake, so it happens here, before the single end_turn call: it
        lasts until every chat has moved or closed (the chat clock closes one whose time ran out), and past the safety
        cap the chats still open are closed "(no reply in time)" here. A chat waiting on this seat goes back to the
        model as the refusal, since it must answer that itself: one it left unanswered through the tool (a mistake the
        metrics count), one answered during the wait with what they said (news, which they do not).
        """
        def mark(nid):
            """Where a negotiation stands now: what an answer changes."""
            n = g.negotiation_head(nid)
            return n["status"], n["awaiting"], n["entries"]

        heard = []
        g = session.game
        cap = self._wait_cap(session)
        spent = 0.0
        while True:
            with session.lock:
                if g.current != pid or g.phase != "playing":
                    break
                mine = g.open_negotiation_heads(pid)
                if not mine:
                    break
                if any(n["awaiting"] == pid for n in mine):
                    if not heard:
                        break
                    error = g.end_turn_refusal(pid) + f" While you waited, they answered: {_serialize(heard)}"
                    session.metrics.tool_call(pid, "end_turn", {}, "action", False, 0.0, error, expected=True)
                    return {"ok": False, "error": error}
                if spent >= cap:
                    # the chat clock should have closed them long since: the safety net (P3.5.2)
                    for n in mine:
                        session.close_negotiation(n["id"], "expired", "(no reply in time)")
                    continue
                before = {n["id"]: mark(n["id"]) for n in mine}
                self._idle()
                t = self._clock()
                try:
                    spent += session._await(lambda was=before: any(mark(nid) != then for nid, then in was.items()),
                                            cap - spent, lambda: self._halted(session), hold_paused=True)
                finally:
                    self._waited += max(0.0, self._clock() - t)
                    self._activate()
                for nid, was in before.items():
                    if mark(nid) != was:
                        news = session._reply_result(pid, nid, {}).get("negotiation")
                        if news:
                            heard.append(news)
                            # told in the refusal, as it stands now: that chat's mark only (a chat this wait did not
                            # watch keeps its own, and is news before the next step)
                            self._heard(session, pid, nid)
            if self._halted(session):
                raise _Halted()
        return session.call_tool(pid, "end_turn", {})

    # ------------------------------------------------------------------ late replies (P3.5.3)
    @staticmethod
    def _chat_marks(session, pid: int, known=()) -> dict:
        """The chats seat ``pid`` is in that are open, and those of ``known`` (closed since), each as (status, awaiting,
        entries, initiator): what an answer changes, and who opened it. Session's lock held."""
        g = session.game
        out = {n["id"]: (n["status"], n["awaiting"], n["entries"], n["initiator"])
               for n in g.open_negotiation_heads(pid)}
        for nid in known:
            if nid not in out:
                try:
                    n = g.negotiation_head(nid)
                except engine_api.ActionError:
                    continue
                out[nid] = (n["status"], n["awaiting"], n["entries"], n["initiator"])
        return out

    def _chat_news(self, session, pid: int) -> list[str]:
        """Before a step of the turn: a note for each chat the seat is in that changed since the model last heard
        (an answer after the tool's wait, a close by the chat clock), worded by where it stands, so the model is never
        asked to answer a chat that is closed (``prompts.late_note``). Chats that closed are then forgotten. A chat
        somebody else opened that waits on the seat is its responder's to answer (``GameSession._dispatch_negotiation_
        interrupts``), so the turn is told only when it closes."""
        with session.lock:
            now = self._chat_marks(session, pid, self._marks)
            notes = []
            for nid, mark in sorted(now.items()):
                if self._marks.get(nid) == mark:
                    continue
                if mark[0] == "open" and (mark[1] != pid or mark[3] != pid):
                    continue            # waiting on the other side, or the responder's to answer: nothing to say
                try:
                    notes.append(late_note(session.game.negotiation_view(nid, pid)))
                except engine_api.ActionError:
                    continue
            self._marks = {nid: mark for nid, mark in now.items() if mark[0] == "open"}
        return notes

    def _progress_note(self, session, pid: int) -> str:
        """What is still unhandled this turn, told to the model after each batch of actions.

        The single most effective guard rail: models forget what they have not done, and a short reminder
        of the remaining idle units and cities turns a wasted turn into a played one.
        """
        with session.lock:
            return session.game.turn_progress(pid)

    # ------------------------------------------------------------------
    def play_turn(self, session, pid: int):
        """Play one turn with the model, from briefing to end of turn."""
        self._ensure_loaded(session, pid)
        with session.lock:
            g = session.game
            if g.current != pid or g.phase != "playing":
                return
            turn = g.turn
            text = TURN_START.format(briefing=g.briefing(pid), turn=turn,
                                     first_turn_note=FIRST_TURN_NOTE if not g.player(pid)["founded_city"] and turn <= 2 else "")
            self._marks = self._chat_marks(session, pid)        # the briefing lists the chats as they stand now
        conv = self._conversation()
        self._active.add(conv)
        conv.add_user_text(text)
        self._check_context(session, pid, conv)
        state = _TurnState()
        calls_made = steps = nudges = 0
        started = self._clock()
        self._waited = 0.0
        self._activate()                        # the driver did already; a probe's turn case calls this directly
        try:
            while True:
                if self._halted(session):
                    self._end_reason(session, pid, "cancelled")
                    return
                with session.lock:
                    if session.game.current != pid or session.game.phase != "playing" or session.game.turn != turn:
                        return
                if session.paused:
                    # paused from the game screen mid-turn: hold before the next model call, and do not count the
                    # pause against the turn's time; the seat's slot is lent meanwhile
                    held = self._clock()
                    self._status(session, pid, "paused")
                    self._idle()
                    while session.paused and not self._halted(session):
                        time.sleep(0.5)
                    self._activate()
                    self._waited += self._clock() - held
                    if self._halted(session):
                        continue
                    self._status(session, pid, "thinking")
                if steps >= self.max_steps:
                    self._limit(session, pid, "step_limit", f"reached {self.max_steps} model steps")
                    return
                if calls_made >= self.max_calls:
                    self._limit(session, pid, "tool_limit", f"reached {self.max_calls} tool calls")
                    return
                if self.max_turn_seconds and self._clock() - started - self._waited > self.max_turn_seconds:
                    self._limit(session, pid, "time_limit", f"exceeded {int(self.max_turn_seconds)}s")
                    return
                news = self._chat_news(session, pid)
                if news:
                    # answers that came after the tool's wait, and chats the chat clock closed: before the next step
                    conv.add_user_text("\n\n".join(news))
                step = self._step(session, pid, conv,
                                  deadline=started + self.max_turn_seconds if self.max_turn_seconds else None)
                steps += 1
                self.last_error = None
                self._thought(session, pid, step.thinking, "thinking")
                self._thought(session, pid, step.text, "reasoning")
                if step.stop_reason == "refusal":
                    self._thought(session, pid, "(model declined to continue this turn)", "system")
                    self._end_reason(session, pid, "refusal")
                    return
                if not step.tool_calls:
                    if step.stop_reason == "max_tokens":
                        continue
                    nudges += 1
                    if nudges > 2:
                        self._end_reason(session, pid, "no_tool_calls")
                        return
                    conv.add_user_text("You did not call any tool. " + self._progress_note(session, pid))
                    continue
                ended, n, progress = self._run_calls(session, pid, conv, step.tool_calls, state)
                calls_made += n
                if ended:
                    self._end_reason(session, pid, "end_turn")
                    return
                if progress:
                    state.steps_without_progress = 0
                    conv.add_user_text(self._progress_note(session, pid))
                else:
                    state.steps_without_progress += 1
                    if state.steps_without_progress >= self.stall_steps:
                        self._limit(session, pid, "stalled", f"{self.stall_steps} model steps without a new successful order")
                        return
                    if state.steps_without_progress == max(3, self.stall_steps // 2):
                        session.metrics.bump(pid, "stall_nudges")
                        conv.add_user_text("You seem to be going in circles: your last several steps produced no new "
                                           "successful orders. Stop re-checking things. " + self._progress_note(session, pid)
                                           + " Give a new order now, or call end_turn.")
                if calls_made >= self.max_calls - 5:
                    conv.add_user_text(f"You are near this turn's tool call limit ({self.max_calls}). Finish up and call end_turn.")
        except _Halted:
            self._end_reason(session, pid, "cancelled")
            return
        except _TimeUp:
            self._limit(session, pid, "time_limit", f"exceeded {int(self.max_turn_seconds)}s (the model was still "
                                                   "generating when time ran out)")
            return
        except _Disconnected as e:
            self._disconnected(session, pid, str(e))
            return
        except Exception as e:
            if self._halted(session):
                return
            if getattr(e, "code", None) == "budget":
                # the grant's hard budget refused the request (design/phase3-features.md P3.4.6): the game pauses with
                # the budget's reason, as for quiet hours, and resumes by itself when the budget's period turns
                self._end_reason(session, pid, "budget")
                self._thought(session, pid, f"(the machine's budget refused the request: {e})", "system")
                session.pause_for_budget(pid, str(e), getattr(e, "budget", None), getattr(e, "retry_at", None))
                return
            session.errors.append({"t": time.time(), "player": pid, "where": "llm play_turn", "trace": traceback.format_exc()})
            self._end_reason(session, pid, "error")
            self._report_failure(session, pid, e)
        finally:
            self._active.discard(conv)
            self._record_usage(conv)
            self._idle()

    def _disconnected(self, session, pid: int, detail: str):
        """The server stayed unreachable for the whole reconnect window: apply the game's disconnect policy.

        "pause" stops the whole game - bots included - until the server answers again (or someone presses
        Resume); the interrupted turn is then replayed from where it stood. "skip" ends this seat's turn, and the
        next turn tries again, waiting out another reconnect window first.
        """
        self._end_reason(session, pid, "disconnected")
        self.last_error = f"server unreachable: {detail}"
        where = self.cfg.get("base_url") or self.cfg.get("server_id") or self.cfg.get("provider") or "the model server"
        window = self.reconnect_seconds(session)
        with session.lock:
            name = session.game.player_name(pid)
        if self.disconnect_policy(session) == "pause":
            self._thought(session, pid, f"(server still unreachable after {window:.0f}s: pausing the game)", "system")
            session.pause_for_disconnect(pid, f"{name}'s model server ({where}) has been unreachable for "
                                              f"{window:.0f}s", self.cfg)
            return
        self._thought(session, pid, f"(server still unreachable after {window:.0f}s: skipping this turn)", "system")
        with session.lock:
            session.game.emit("agent_error", f"{name}'s model server ({where}) was unreachable for {window:.0f}s, so "
                                             f"its turn was skipped ({detail[:200]}).", None, player=pid)

    def _check_context(self, session, pid: int, conv):
        """Record the model's context window (LM Studio) and warn once if it is too small for CITAR's prompts."""
        info_fn = getattr(conv, "context_info", None)
        info = info_fn() if info_fn else None
        if not info:
            return
        rec = session.metrics.current(pid)
        if rec is not None:
            rec["context_length"] = info.get("loaded_context")
            rec["model_loaded_at_start"] = info.get("state") == "loaded"
        ctx = info.get("loaded_context")
        if ctx and ctx < 24000 and not getattr(self, "_warned_context", False):
            self._warned_context = True
            msg = (f"{self.cfg.get('model')} is loaded in LM Studio with only {ctx:,} tokens of context. CITAR turns need "
                   f"roughly 20-40k; the model will lose track of its briefing and repeat itself. Reload it with a larger "
                   f"context length (max {info.get('max_context') or '?'}).")
            self._thought(session, pid, "(warning) " + msg, "system")
            with session.lock:
                session.game.emit("agent_error", f"{session.game.player_name(pid)}'s AI: {msg}", None, player=pid)

    def _limit(self, session, pid: int, reason: str, detail: str):
        """End the turn because a limit was reached, recording which."""
        self._end_reason(session, pid, reason)
        self._thought(session, pid, f"(turn ended by the game: {detail})", "system")

    # ------------------------------------------------------------------
    def respond_negotiation(self, session, pid: int, nid: int) -> str:
        """Answer chat ``nid`` out of turn (the seat's responder): one exchange of the chat's own conversation
        (``ChatThread.exchange``), whose outcome it returns."""
        return self.chat_thread(nid).exchange(session, pid)


class ChatThread:
    """One chat's conversation for a model seat (design/phase3-features.md P3.5.3), held by its agent while the chat is
    open (``LLMAgent.chat_thread``), dropped when it closes (``chat_closed``), and rebuilt from the chat's history after
    a restart (the agent is new then, and its first message carries the whole history).

    Before it, every answer opened a fresh conversation with the turn's full system prompt and fifteen tools, so a
    model re-read its whole situation for every message and forgot what it had said. Here the system prompt is the
    chat's own (``chat_system_prompt``, static so the provider caches it) and each exchange adds only what changed:
    the other side's entries since the model's last answer, the proposal from the seat's side and the minutes left.

    An exchange runs up to ``max_negotiation_calls`` steps on the seat's binding (its lease; a probe's ticket), with the
    tools of ``CHAT_TOOLS``, and stops when the chat moves. Its outcome (``exchange``): ``answered`` (the model answered
    it), ``moved`` (it no longer waits on the seat: answered or closed by somebody else), ``unanswered`` (the model
    used its steps without answering), ``failed`` (the model could not be asked: an error, a machine's refusal),
    ``cancelled`` (the seat halted, or the chat clock's deadline came: ``cancel``) or ``paused`` (the game paused for
    its machine, or loaded and not yet resumed, before the model was asked, ``GameSession.answers_held``: it answers
    when the game resumes). The session declines only ``unanswered`` and ``failed`` ones ("(no response)"); a slow
    answer is the chat clock's to decline."""

    def __init__(self, agent: LLMAgent, nid: int):
        self.agent = agent
        self.nid = nid
        self.conv = None
        self.binding = None          # the binding its conversation's steps run on
        self.seen = 0                # the chat's entries the model has been told of
        self.cancels = 0             # moves on each cancel: an exchange in progress stops at its next look
        self.closed = False
        self._unanswered = False     # the last message has no step after it (one was cut off): a retry asks again
        self.last_outcome: Optional[str] = None
        self.last_cause: Optional[str] = None
        self._dead = False           # its conversation was closed by a cancel: the next exchange starts a new one
        self.exchanges = 0
        self.conversations = 0

    def cancel(self) -> None:
        """Stop the exchange in progress (non-blocking): its request is cancelled, and it returns ``cancelled``."""
        self.cancels += 1
        conv = self.conv
        if conv is not None:
            self._dead = True
            try:
                conv.close()
            except Exception:
                pass

    def close(self) -> None:
        """The chat closed: nothing more is asked in it."""
        self.closed = True
        self.cancel()

    def _conversation(self, session, pid: int, fresh: bool):
        """The chat's conversation on the agent's binding now: made on the first exchange (or after a cancel closed
        it), else the one it has, moved onto a new binding when the agent has one (a probe's ticket per answer)."""
        a = self.agent
        binding = a.binding
        if not fresh and self.conv is not None:
            if binding is not self.binding and hasattr(self.conv, "rebind") and binding is not None:
                self.conv.rebind(binding)
                self.binding = binding
            if binding is self.binding:
                return self.conv
        inner = make_conversation(a.cfg, chat_system_prompt(a.cfg.get("persona")), _tool_defs(CHAT_TOOLS))
        if binding is not None:
            from ..pool.scheduler import ScheduledConversation
            conv = ScheduledConversation(inner, binding, a.cfg)
            # its place in the machine's queue and its start, to both sides of the chat (negotiation_status)
            conv.on_status = lambda payload, nid=self.nid: session.chat.answer_status(nid, pid, payload)
        else:
            conv = inner
        self.conv, self.binding, self._dead = conv, binding, False
        self.conversations += 1
        return conv

    def _message(self, session, pid: int, fresh: bool) -> Optional[tuple]:
        """What the model is told now, and the chat's entries then; None when the chat no longer waits on the seat.
        Session's lock held."""
        g = session.game
        try:
            n = g.negotiation_head(self.nid)
        except engine_api.ActionError:
            return None
        if n["status"] != "open" or n["awaiting"] != pid:
            return None
        view = g.negotiation_view(self.nid, pid)
        timing = session.chat.deadline_of(self.nid)
        left = time_left_text(timing.get("left_s"), bool(timing.get("paused")))
        theirs = [e for e in view.get("history") or [] if not e.get("you") and (fresh or e.get("seq", 0) > self.seen)]
        said = "; then ".join(entry_text(e) for e in theirs) or "nothing new"
        if fresh:
            emp = g.empire_summary(pid)
            summary = {k: emp[k] for k in ("gold", "per_turn", "era", "happiness", "score", "strategic_resources",
                                           "luxuries", "policies", "cities", "at_war_with")}
            summary["notebook"] = emp["notes"][-2000:]
            last = entry_text(theirs[-1]) if theirs else "nothing yet"
            text = CHAT_PROMPT.format(other=view["with_name"], nid=self.nid, time_left=left, said=last,
                                      proposal=proposal_text(view), negotiation=json.dumps(view, indent=1),
                                      summary=json.dumps(summary, default=str))
        else:
            text = CHAT_UPDATE.format(other=view["with_name"], nid=self.nid, said=said, proposal=proposal_text(view),
                                      current=json.dumps({"current_proposal": view.get("current_proposal")}),
                                      time_left=left)
        return text, n["entries"]

    def exchange(self, session, pid: int) -> str:
        """Tell the model what changed in the chat and run its steps until it answers or the chat moves (the class's
        docstring names the outcomes). Its row goes on the game's metrics (``Metrics.negotiation``), except for an
        exchange that was cancelled or paused."""
        a = self.agent
        start = self.cancels
        if session.answers_held():
            return "paused"
        # a conversation closed behind the thread's back (the scheduler cancelling the ticket it ran on: a benchmark
        # took the machine) refuses every step: the chat starts a new one, told the whole chat again. ``is True``: a
        # provider's own ``closed`` may be anything (an Event), and only the scheduler's wrapper says it so
        fresh = self.conv is None or self._dead or getattr(self.conv, "closed", False) is True
        with session.lock:
            told = self._message(session, pid, fresh)
        if told is None:
            return "moved"
        text, entries = told
        conv = self._conversation(session, pid, fresh)
        if fresh or not self._unanswered or entries != self.seen:
            conv.add_user_text(text)
        # else: the same message, which a step cut off (a machine that took its slot back) never answered, is asked
        # again as it stands, rather than told "nothing new"
        self.seen = entries
        self._unanswered = True
        self.exchanges += 1
        if not hasattr(conv, "heard"):
            # a conversation on no binding hears nothing from a machine: the model is asked now (a scheduled one says
            # so itself when each step has its slot)
            session.chat.answer_status(self.nid, pid, {"state": "thinking"})
        a._active.add(conv)
        steps = calls = 0
        gen_s = queue_s = 0.0
        t0 = time.perf_counter()
        before = dict(getattr(conv, "usage", {}) or {})
        answered = False
        outcome: Optional[str] = None
        cause: Optional[str] = None
        try:
            while steps < a.max_negotiation_calls and calls < a.max_negotiation_calls:
                with session.lock:
                    n = session.game.negotiation_head(self.nid)
                moved = n["status"] != "open" or n["awaiting"] != pid or n["entries"] != entries
                if moved and answered:
                    # the model's own answer (an accept or a reject closes the chat, and so its conversation)
                    outcome = "answered"
                    break
                if a._halted(session) or self.cancels != start:
                    outcome = "cancelled"
                    break
                if moved:
                    outcome = "moved"
                    break
                if session.answers_held():
                    outcome = "paused"
                    break
                step = a._step(session, pid, conv, chat=True)
                self._unanswered = False
                steps += 1
                gen_s += float(getattr(conv, "last_gen_s", 0.0) or 0.0)
                queue_s += float(getattr(conv, "last_queue_s", 0.0) or 0.0)
                a._thought(session, pid, step.thinking, "thinking")
                a._thought(session, pid, step.text, "diplomacy")
                if not step.tool_calls:
                    conv.add_user_text("Answer with respond_negotiation (accept, reject, counter or reply), with a "
                                       "message.")
                    self._unanswered = True
                    continue
                results = []
                for c in step.tool_calls:
                    if c.name not in CHAT_TOOLS:
                        results.append((c.id, f"Tool '{c.name}' is not available in a chat: answer with "
                                              "respond_negotiation.", True))
                        continue
                    res = session.call_tool(pid, c.name, c.args)
                    if c.name == "respond_negotiation" and res["ok"] and _same_nid(c.args, self.nid):
                        answered = True
                    results.append((c.id, _serialize(res["result"]) if res["ok"] else "Error: " + res["error"],
                                    not res["ok"]))
                conv.add_tool_results(results)
                calls += len(step.tool_calls)
        except _Halted:
            outcome = "cancelled"
        except Exception as e:
            if a._halted(session) or self.cancels != start:
                outcome = "cancelled"
            else:
                a.last_error = f"{type(e).__name__}: {e}"
                outcome, cause = "failed", _cause(e)
                session.errors.append({"t": time.time(), "player": pid, "where": "llm negotiation",
                                       "negotiation": self.nid, "trace": traceback.format_exc()})
        finally:
            a._active.discard(conv)
            usage = dict(getattr(conv, "usage", {}) or {})
            for k, v in usage.items():
                a.usage_total[k] = a.usage_total.get(k, 0) + v - (before.get(k, 0) or 0)
            if outcome is None:
                with session.lock:
                    n = session.game.negotiation_head(self.nid)
                still = n["status"] == "open" and n["awaiting"] == pid and n["entries"] == entries
                outcome = "unanswered" if still else ("answered" if answered else "moved")
            if outcome == "unanswered":
                cause = "failed"
            if outcome not in ("cancelled", "paused"):
                session.metrics.negotiation(
                    pid, time.perf_counter() - t0, steps, usage.get("input_tokens", 0) - before.get("input_tokens", 0),
                    usage.get("output_tokens", 0) - before.get("output_tokens", 0), nid=self.nid,
                    gen_s=gen_s, queue_s=queue_s,
                    outcome={"answered": "answered", "moved": "closed"}.get(outcome, "fallback"), cause=cause)
            session.chat.answer_status(self.nid, pid, {"state": "done"})
        self.last_outcome, self.last_cause = outcome, cause
        return outcome


def _same_nid(args: dict, nid: int) -> bool:
    """Whether a tool call's ``negotiation_id`` names chat ``nid``."""
    try:
        return int((args or {}).get("negotiation_id")) == nid
    except (TypeError, ValueError):
        return False


def _cause(e: BaseException) -> str:
    """Why a model's answer was not given (P3.5.2's causes): ``queued_behind_benchmark`` when a benchmark took or held
    its machine, ``refused`` for any other refusal of the machine (a budget, a disabled machine, quiet hours, a
    model not loaded), ``failed`` for an error of the model or its connection."""
    code = getattr(e, "code", "") or ""
    if code == "benchmark":
        return "queued_behind_benchmark"
    from ..pool.scheduler import SchedulerError
    if isinstance(e, SchedulerError) or code:
        return "refused"
    return "failed"


class NoMachineAgent:
    """A model seat with no machine to play on (P3.2.9): its block names no machine, and the game's owner is not
    an administrator, so nothing the block says is dialled.

    Its turn is skipped as a disconnected seat's would be under the "skip" policy, never by pausing the whole game
    (no machine comes back by itself); it says why on its first turn, and every turn in the seat's thoughts. It
    answers no chat: the session's responder declines what it leaves, as for a model that failed to answer.
    """

    def __init__(self, reason: str):
        self.cfg: dict = {}
        self.reason = reason
        self.last_error: str | None = reason
        self.cancelled = False
        self.usage_total: dict = {}
        self._told = False

    def cancel(self):
        """Nothing runs: there is nothing to stop."""
        self.cancelled = True

    def play_turn(self, session, pid: int):
        """Skip the turn, saying why."""
        with session.lock:
            g = session.game
            if g.current != pid or g.phase != "playing":
                return
            session.metrics.set_end_reason(pid, "disconnected")
            g.add_thought(pid, f"(this seat has no machine: {self.reason[:300]})", "system")
            if not self._told:
                self._told = True
                g.emit("agent_error", f"{g.player_name(pid)}'s AI has no machine to play on, so its turns are "
                                      f"skipped: {self.reason[:300]}", None, player=pid)

    def respond_negotiation(self, session, pid: int, nid: int) -> str:
        """No answer: the responder declines it ("(no response)")."""
        return "unanswered"
