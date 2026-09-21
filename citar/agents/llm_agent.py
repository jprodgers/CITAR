"""Drives a language model through a turn (or a diplomatic interrupt) using the shared tool registry.

Includes guard rails for weaker models: identical actions are not re-executed within a turn, unchanged queries are
flagged, a short "turn progress" note follows every batch of actions, and stalled or runaway turns are ended.
Every model step and tool call is recorded in the session metrics.
"""
from __future__ import annotations

import json
import time
import traceback

from ..engine import tools as toolreg
from ..server.metrics import call_signature, ACTION_REPEAT_EXEMPT
from .prompts import system_prompt, TURN_START, FIRST_TURN_NOTE, NEGOTIATION_PROMPT
from .providers import make_conversation

NEGOTIATION_TOOLS = {"get_diplomacy", "get_empire", "get_players", "get_map", "get_tile", "get_tech_tree", "get_rules",
                     "read_notes", "write_notes", "log_thought", "send_message", "respond_negotiation",
                     "get_victory_status", "get_cities", "get_units"}
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
    out = []
    for t in toolreg.REGISTRY.values():
        if names is not None and t.name not in names:
            continue
        out.append({"name": t.name, "description": t.description, "input_schema": t.schema()})
    return out


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
    def __init__(self, cfg: dict):
        self.cfg = dict(cfg)
        self.max_calls = int(self.cfg.get("max_tool_calls_per_turn") or 150)
        self.max_steps = int(self.cfg.get("max_steps_per_turn") or 60)
        self.max_turn_seconds = float(self.cfg.get("max_turn_seconds") if self.cfg.get("max_turn_seconds") is not None else 1800)
        self.stall_steps = int(self.cfg.get("stall_steps") or 10)
        self.max_negotiation_calls = int(self.cfg.get("max_negotiation_calls") or 20)
        self.negotiation_wait = float(self.cfg.get("negotiation_wait_seconds") or 240)
        self.usage_total: dict = {}
        self.last_error: str | None = None
        self.cancelled = False
        self._active: set = set()
        self._waited = 0.0          # seconds this turn spent waiting for the server to come back (not counted against it)

    # ------------------------------------------------------------------
    def cancel(self):
        """Abort any turn or negotiation in progress, including in-flight model requests."""
        self.cancelled = True
        for conv in list(self._active):
            try:
                conv.close()
            except Exception:
                pass

    def _halted(self, session) -> bool:
        """Whether the game has stopped and this turn should be abandoned."""
        return self.cancelled or session.stopped

    def reconnect_seconds(self, session) -> float:
        """How long to keep trying an unreachable server: the seat's setting, else the game's, else the default."""
        for v in (self.cfg.get("reconnect_seconds"), session.game.s.config.get("reconnect_seconds")):
            try:
                if v not in (None, ""):
                    return max(0.0, float(v))
            except (TypeError, ValueError):
                pass
        return float(DEFAULT_RECONNECT_SECONDS)

    def disconnect_policy(self, session) -> str:
        """What happens when the server stays unreachable: "pause" the game or "skip" this seat's turn."""
        for v in (self.cfg.get("on_disconnect"), session.game.s.config.get("on_disconnect")):
            if v in DISCONNECT_POLICIES:
                return v
        return "pause"

    def _status(self, session, pid: int, status: str):
        """Tell the game screen what the seat is doing ("thinking", "reconnecting")."""
        session.agent_status[pid] = status
        session._broadcast({"type": "agent", "player": pid, "status": status})

    def _step(self, session, pid: int, conv, deadline: float | None = None):
        """One model call (timed and metered), riding out connection problems.

        A transient failure is retried with backoff for up to the reconnect window, so a server that drops for
        a few seconds - a worker reconnecting, LM Studio restarting, Wi-Fi blinking - costs nothing but the wait.
        Time spent waiting does not count against the turn. If the server is still unreachable when the window
        closes, _Disconnected is raised and the game's disconnect policy decides what happens next.

        With a deadline (end of the turn's time budget) the request may only use the time left, and running out
        of time raises _TimeUp instead of an error."""
        delays = [2, 3, 5, 10, 15, 20, 30]
        first_failure = None
        attempt = 0
        while True:
            if self._halted(session):
                raise _Halted()
            if deadline is not None:
                remaining = deadline + self._waited - time.time()
                if remaining <= 5:
                    raise _TimeUp()
                conv.request_timeout = remaining
            before = dict(getattr(conv, "usage", {}))
            t0 = time.perf_counter()
            try:
                step = conv.step()
                usage = getattr(conv, "usage", {})
                d = {k: usage.get(k, 0) - before.get(k, 0) for k in usage}
                seconds = time.perf_counter() - t0
                session.metrics.model_step(
                    pid, seconds, input_tokens=d.get("input_tokens", 0), output_tokens=d.get("output_tokens", 0),
                    reasoning_tokens=d.get("reasoning_tokens", 0), malformed=step.malformed)
                self._meter(session, conv, seconds, d)
                if first_failure is not None:
                    self._thought(session, pid, f"(reconnected after {time.time() - first_failure:.0f}s)", "system")
                    self._status(session, pid, "thinking")
                return step
            except Exception as e:
                if self._halted(session):
                    raise _Halted()
                name = type(e).__name__
                self._meter(session, conv, time.perf_counter() - t0, {})
                unreachable = _is_unreachable(e)
                if not unreachable and deadline is not None and time.time() >= deadline + self._waited - 5:
                    session.metrics.model_step(pid, time.perf_counter() - t0)
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
                if unreachable:
                    self._waited += time.time() - now

    def _meter(self, session, conv, seconds: float, d: dict):
        """Record a model call (time and tokens) in the usage ledger, which reports turn into costs."""
        from .. import usage
        usage.tracker().llm(getattr(session, "usage_act", None), self.cfg.get("server_id"),
                            getattr(conv, "served_model", None) or self.cfg.get("model"), seconds,
                            input_tokens=d.get("input_tokens", 0), output_tokens=d.get("output_tokens", 0),
                            reasoning_tokens=d.get("reasoning_tokens", 0), cache_read=d.get("cache_read_input_tokens", 0),
                            cache_write=d.get("cache_creation_input_tokens", 0))

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
            g.emit("agent_error", f"{g.player(pid).name}'s AI could not play ({self.cfg.get('model') or '?'} at {where}): "
                                  f"{self.last_error[:300]}. Its turn was skipped — check the seat settings.", None, player=pid)

    def _conversation(self, names=None):
        """Start a conversation with the provider, with the tools this turn may use."""
        return make_conversation(self.cfg, system_prompt(self.cfg.get("persona")), _tool_defs(names))

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
            g = session.game
            g.s.thoughts.append({"turn": g.turn, "player": pid, "text": text[:6000], "kind": kind})
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
            spec = toolreg.REGISTRY.get(c.name)
            if "__invalid_json__" in c.args:
                results.append((c.id, "Error: your tool arguments were not valid JSON.", True))
                session.metrics.tool_call(pid, c.name, {}, spec.kind if spec else "unknown", False, 0.0, "invalid JSON arguments")
                continue
            sig = call_signature(c.name, c.args)
            # --- repeated identical action: don't redo it ---------------------------------------
            if spec and spec.kind == "action" and c.name not in ACTION_REPEAT_EXEMPT and state.done.get(sig):
                n = state.done[sig] = state.done[sig] + 1
                msg = (f"Skipped: you already did exactly this earlier this turn (it succeeded; this is attempt {n}). "
                       f"Nothing needs redoing — check the TURN PROGRESS note and give different orders, or call end_turn.")
                results.append((c.id, msg, True))
                session.metrics.tool_call(pid, c.name, c.args, "action", False, 0.0, "blocked identical repeat",
                                          blocked_repeat=True)
                self._thought(session, pid, f"{c.name} {json.dumps(c.args, ensure_ascii=False)[:200]} → blocked repeat #{n}", "action")
                continue
            # --- repeated identical query with no state change: answer from cache, flag it ---------
            if spec and spec.kind == "query" and sig in state.queries:
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
            wait = self.negotiation_wait if c.name in ("open_negotiation", "respond_negotiation") else 0
            res = session.call_tool(pid, c.name, c.args, wait_negotiation=wait)
            if res["ok"]:
                text = _serialize(res["result"])
                results.append((c.id, text, False))
                if c.name == "end_turn":
                    ended = True
                if spec and spec.kind == "query":
                    state.queries[sig] = (session.version, 1, text)
                elif spec and spec.kind == "action":
                    state.done[sig] = state.done.get(sig, 0) + 1
                    r = res["result"]
                    no_op = c.name == "move_unit" and isinstance(r, dict) and r.get("from") == r.get("to") and "rebased_to" not in r
                    if c.name not in ("log_thought", "write_notes") and not no_op:
                        progress += 1
            else:
                results.append((c.id, "Error: " + res["error"], True))
            if c.name != "log_thought" and (spec is None or spec.kind == "action"):
                args = json.dumps(c.args, ensure_ascii=False)[:200]
                outcome = "ok" if res["ok"] else f"ERROR: {res['error'][:200]}"
                self._thought(session, pid, f"{c.name} {args} → {outcome}", "action")
        conv.add_tool_results(results)
        return ended, len(calls), progress

    def _progress_note(self, session, pid: int) -> str:
        """What is still unhandled this turn, told to the model after each batch of actions.

        The single most effective guard rail: models forget what they have not done, and a short reminder
        of the remaining idle units and cities turns a wasted turn into a played one.
        """
        from ..engine.briefing import turn_progress
        with session.lock:
            return turn_progress(session.game, pid)

    # ------------------------------------------------------------------
    def play_turn(self, session, pid: int):
        """Play one turn with the model, from briefing to end of turn."""
        from ..engine.briefing import briefing
        self._ensure_loaded(session, pid)
        with session.lock:
            g = session.game
            if g.s.current != pid or g.s.phase != "playing":
                return
            turn = g.turn
            text = TURN_START.format(briefing=briefing(g, pid), turn=turn,
                                     first_turn_note=FIRST_TURN_NOTE if not g.player(pid).founded_city and turn <= 2 else "")
        conv = self._conversation()
        self._active.add(conv)
        conv.add_user_text(text)
        self._check_context(session, pid, conv)
        state = _TurnState()
        calls_made = steps = nudges = 0
        started = time.time()
        self._waited = 0.0
        try:
            while True:
                if self._halted(session):
                    self._end_reason(session, pid, "cancelled")
                    return
                with session.lock:
                    if session.game.s.current != pid or session.game.s.phase != "playing" or session.game.turn != turn:
                        return
                if steps >= self.max_steps:
                    self._limit(session, pid, "step_limit", f"reached {self.max_steps} model steps")
                    return
                if calls_made >= self.max_calls:
                    self._limit(session, pid, "tool_limit", f"reached {self.max_calls} tool calls")
                    return
                if self.max_turn_seconds and time.time() - started - self._waited > self.max_turn_seconds:
                    self._limit(session, pid, "time_limit", f"exceeded {int(self.max_turn_seconds)}s")
                    return
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
            session.errors.append({"t": time.time(), "player": pid, "where": "llm play_turn", "trace": traceback.format_exc()})
            self._end_reason(session, pid, "error")
            self._report_failure(session, pid, e)
        finally:
            self._active.discard(conv)
            self._record_usage(conv)

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
            name = session.game.player(pid).name
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
                session.game.emit("agent_error", f"{session.game.player(pid).name}'s AI: {msg}", None, player=pid)

    def _limit(self, session, pid: int, reason: str, detail: str):
        """End the turn because a limit was reached, recording which."""
        self._end_reason(session, pid, reason)
        self._thought(session, pid, f"(turn ended by the game: {detail})", "system")

    # ------------------------------------------------------------------
    def respond_negotiation(self, session, pid: int, nid: int):
        """Answer a negotiation with the model, out of turn."""
        from ..engine.diplomacy import get_negotiation, negotiation_view
        from ..engine.views import empire_info
        with session.lock:
            g = session.game
            n = get_negotiation(g, nid)
            if n["status"] != "open" or n["awaiting"] != pid:
                return
            view = negotiation_view(g, n, pid)
            emp = empire_info(g, pid)
            summary = {k: emp[k] for k in ("gold", "per_turn", "era", "happiness", "score", "strategic_resources",
                                           "luxuries", "policies")}
            summary["cities"] = len(g.player_cities(pid))
            summary["at_war_with"] = [g.player(q).name for q in g.player(pid).met if g.at_war(pid, q)]
            summary["notebook"] = g.player(pid).notes[-2000:]
            text = NEGOTIATION_PROMPT.format(other=view["with_name"], nid=nid, negotiation=json.dumps(view, indent=1),
                                             summary=json.dumps(summary, default=str))
            history_len = len(n["history"])
        conv = self._conversation(NEGOTIATION_TOOLS)
        self._active.add(conv)
        conv.add_user_text(text)
        calls = steps = 0
        t0 = time.time()
        try:
            while calls < self.max_negotiation_calls and steps < self.max_negotiation_calls:
                if self._halted(session):
                    return
                with session.lock:
                    n = get_negotiation(session.game, nid)
                    if n["status"] != "open" or n["awaiting"] != pid or len(n["history"]) != history_len:
                        return
                step = self._step(session, pid, conv)
                steps += 1
                self._thought(session, pid, step.thinking, "thinking")
                self._thought(session, pid, step.text, "diplomacy")
                if not step.tool_calls:
                    conv.add_user_text("Respond with respond_negotiation (accept, reject, counter, or reply).")
                    continue
                # never block waiting for replies inside an interrupt
                results = []
                for c in step.tool_calls:
                    if c.name not in NEGOTIATION_TOOLS:
                        results.append((c.id, f"Tool '{c.name}' is not available during a diplomatic interrupt.", True))
                        continue
                    res = session.call_tool(pid, c.name, c.args)
                    results.append((c.id, _serialize(res["result"]) if res["ok"] else "Error: " + res["error"], not res["ok"]))
                conv.add_tool_results(results)
                calls += len(step.tool_calls)
        except _Halted:
            return
        except Exception as e:
            if self._halted(session):
                return
            self.last_error = f"{type(e).__name__}: {e}"
            session.errors.append({"t": time.time(), "player": pid, "where": "llm negotiation", "trace": traceback.format_exc()})
        finally:
            self._active.discard(conv)
            self._record_usage(conv)
            usage = getattr(conv, "usage", {})
            session.metrics.negotiation(pid, time.time() - t0, steps, usage.get("input_tokens", 0), usage.get("output_tokens", 0))
