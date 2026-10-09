"""Per-seat performance metrics for evaluating AI players (and humans): turn times, model latency and tokens,
tool usage counts and timings, errors, repeated calls, malformed calls, stalls and forced turn ends.

Stored with the save file so games can be compared afterwards.
"""
from __future__ import annotations

import json
import statistics
import time
from typing import Optional

ACTION_REPEAT_EXEMPT = {"buy", "attack", "end_turn", "move_unit"}


def call_signature(name: str, args: dict) -> str:
    """A stable signature for a tool call, so identical repeats can be counted.

    The basis of loop detection: two calls with the same name and the same arguments are the same
    attempt, and a model making many of them has misunderstood the state rather than the rules.
    """
    return name + " " + json.dumps(args or {}, sort_keys=True, ensure_ascii=False, default=str)


def _new_turn(pid: int, turn: int) -> dict:
    """A blank record for one seat's turn."""
    return {
        "player": pid, "turn": turn, "started": time.time(), "ended": None, "wall_s": None,
        "model_steps": 0, "model_s": 0.0, "input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0,
        "tool_calls": 0, "actions_ok": 0, "queries": 0, "errors": 0, "repeats": 0, "malformed": 0,
        "blocked_repeats": 0, "stall_nudges": 0, "by_tool": {}, "signatures": {}, "error_samples": [],
        "end_reason": None, "controller": None, "model": None,
    }


class Metrics:
    """Per-seat, per-turn measurement of how an AI played.

    What it records is chosen to answer "why is this model scoring badly", which is rarely answered by
    the score: time per turn and per model call, steps, tool calls, errors, identical repeats, repairs
    of malformed calls, stall nudges, tokens, and how each turn ended.

    Counted per turn rather than per game on purpose. A model that loops for two hundred calls in one
    turn and behaves for the rest is a different problem from one that is slightly repetitive
    throughout, and a per-game average hides the difference.

    Only an open turn's record changes: every change goes through ``current`` (or ``_open``), and a record that is
    closed (ended, interrupted or superseded) is never changed again, nor is a negotiation's once it is recorded.
    ``json_parts`` relies on it to encode each settled record once.
    """
    def __init__(self, data: Optional[dict] = None):
        self.data = data or {"turns": [], "negotiations": []}
        self._open: dict[int, dict] = {}
        self._encoded: dict[str, _Encoded] = {}      # per list in ``data``: its settled records, encoded (json_parts)
        for rec in self.data["turns"]:
            if rec.get("ended") is None and not rec.get("interrupted"):
                # A turn that was in progress when the game was saved: the server stopped mid-turn, so its timing
                # is meaningless (it would include the downtime). Keep it for the record but exclude it from stats;
                # the turn is replayed and measured again from scratch.
                rec["interrupted"] = True
                rec["end_reason"] = "interrupted"

    def json_parts(self) -> list[bytes]:
        """The metrics as JSON in pieces, whose concatenation (``b"".join``) is ``json.dumps(self.data)``'s bytes: what a
        save takes under the session's lock (``GameSession._snapshot``), and joins on its writer thread.

        The records grow by one a seat a turn, so dumping them whole at every autosave cost time under the lock that
        grew with the game (25 to 29 ms at the end of a 24-seat gargantuan game, against P2.5.3's 10 ms budget for
        everything a save does under the lock). A record that is no longer open never changes again (see the class), so
        each is encoded once, by the first save that finds it settled, and kept: a save encodes the records settled
        since the last one and the open turn's, whatever the game's length. Called with the session's lock held.
        """
        if not all(isinstance(key, str) for key in self.data):
            return [json.dumps(self.data).encode()]      # json's own spelling of other keys; never the server's
        parts = [b"{"]
        for i, (key, value) in enumerate(self.data.items()):
            parts.append((", " if i else "").encode() + json.dumps(key).encode() + b": ")
            if key in ("turns", "negotiations") and isinstance(value, list):
                parts.extend(self._list_parts(key, value))
            else:
                parts.append(json.dumps(value).encode())
        parts.append(b"}")
        return parts

    def _list_parts(self, key: str, records: list) -> list[bytes]:
        """One of ``data``'s lists as JSON pieces: its settled prefix from the cache (extended by the records settled
        since), then the rest encoded afresh (the open turn's record, and any after it)."""
        enc = self._encoded.get(key)
        if enc is None or enc.of is not records or enc.done > len(records):
            enc = self._encoded[key] = _Encoded(records)      # a list replaced or cut: encoded again from the start
        open_ids = {id(r) for r in self._open.values()} if key == "turns" else ()
        k = enc.done
        while k < len(records) and id(records[k]) not in open_ids:
            k += 1
        if k > enc.done:
            block = ", ".join(json.dumps(r) for r in records[enc.done:k])
            enc.blocks.append(((", " if enc.done else "") + block).encode())
            enc.done = k
        rest = [((", " if i else "") + json.dumps(r)).encode() for i, r in enumerate(records[k:], start=k)]
        return [b"[", *enc.blocks, *rest, b"]"]

    @staticmethod
    def _valid(rec: dict) -> bool:
        """Whether a player id is one worth recording."""
        return rec.get("ended") is not None and not rec.get("interrupted")

    # ------------------------------------------------------------------
    def begin_turn(self, pid: int, turn: int, controller: str, model: Optional[str] = None):
        """Start recording a seat's turn."""
        cur = self._open.get(pid)
        if cur and cur["turn"] == turn and cur["ended"] is None:
            return cur
        if cur and cur["ended"] is None:
            self.end_turn(pid, "superseded")
        rec = _new_turn(pid, turn)
        rec["controller"] = controller
        rec["model"] = model
        self.data["turns"].append(rec)
        self._open[pid] = rec
        return rec

    def current(self, pid: int) -> Optional[dict]:
        """The turn record in progress for a seat."""
        rec = self._open.get(pid)
        return rec if rec and rec["ended"] is None else None

    def end_turn(self, pid: int, reason: Optional[str] = None):
        """Finish a seat's turn, recording why it ended."""
        rec = self.current(pid)
        if rec is None:
            return
        rec["ended"] = time.time()
        self._unpause_rec(rec, rec["ended"])
        # a paused game is not a slow player: time spent paused is left out of the turn's duration
        rec["wall_s"] = round(rec["ended"] - rec["started"] - rec.get("paused_s", 0.0), 3)
        if reason and not rec["end_reason"]:
            rec["end_reason"] = reason
        rec["end_reason"] = rec["end_reason"] or "end_turn"
        self._open.pop(pid, None)

    def pause(self):
        """The game was paused: stop the clock on every turn in progress."""
        now = time.time()
        for rec in self._open.values():
            if rec["ended"] is None and rec.get("paused_at") is None:
                rec["paused_at"] = now

    def unpause(self):
        """The game was resumed: restart the clock, remembering how long it was stopped."""
        now = time.time()
        for rec in self._open.values():
            self._unpause_rec(rec, now)

    @staticmethod
    def _unpause_rec(rec: dict, now: float):
        """Fold a pause in progress into a turn record's paused time."""
        if rec.get("paused_at") is not None:
            rec["paused_s"] = round(rec.get("paused_s", 0.0) + max(0.0, now - rec["paused_at"]), 3)
            rec["paused_at"] = None

    def interrupt_open(self):
        """Mark every turn in progress as interrupted (game suspended mid-turn): its timing would include the pause, so
        it is excluded from stats and the turn is measured again from scratch when play resumes."""
        for pid, rec in list(self._open.items()):
            if rec["ended"] is None:
                rec["interrupted"] = True
                rec["end_reason"] = "interrupted"
            self._open.pop(pid, None)

    def set_end_reason(self, pid: int, reason: str):
        """Record why a turn ended, if it has not been recorded already."""
        rec = self.current(pid)
        if rec is not None:
            rec["end_reason"] = reason

    # ------------------------------------------------------------------
    def tool_call(self, pid: int, name: str, args: dict, kind: str, ok: bool, seconds: float, error: Optional[str] = None,
                  blocked_repeat: bool = False, expected: bool = False):
        """Record one tool call: its result, timing, and whether it repeated an earlier one.

        ``expected`` marks a refusal that is news rather than a mistake - an end_turn the agent held back because an
        answer arrived while it waited - so it is not counted as an error against the model.
        """
        rec = self.current(pid)
        if rec is None:
            return
        rec["tool_calls"] += 1
        t = rec["by_tool"].setdefault(name, {"count": 0, "errors": 0, "seconds": 0.0})
        t["count"] += 1
        t["seconds"] = round(t["seconds"] + seconds, 4)
        if kind == "query":
            rec["queries"] += 1
        elif ok:
            rec["actions_ok"] += 1
        if not ok and not blocked_repeat and not expected:
            rec["errors"] += 1
            t["errors"] += 1
            if error and len(rec["error_samples"]) < 12:
                rec["error_samples"].append(f"{name}: {error[:200]}")
        if blocked_repeat:
            rec["blocked_repeats"] += 1
        sig = call_signature(name, args)[:300]
        n = rec["signatures"].get(sig, 0) + 1
        rec["signatures"][sig] = n
        if n > 1 and name not in ACTION_REPEAT_EXEMPT:
            rec["repeats"] += 1

    def model_step(self, pid: int, seconds: float, input_tokens: int = 0, output_tokens: int = 0,
                   reasoning_tokens: int = 0, malformed: int = 0):
        """Record one model step: its duration and its tokens."""
        rec = self.current(pid)
        if rec is None:
            return
        rec["model_steps"] += 1
        rec["model_s"] = round(rec["model_s"] + seconds, 3)
        rec["peak_prompt_tokens"] = max(rec.get("peak_prompt_tokens", 0), input_tokens or 0)
        rec["slowest_step_s"] = round(max(rec.get("slowest_step_s", 0.0), seconds), 2)
        rec["input_tokens"] += input_tokens or 0
        rec["output_tokens"] += output_tokens or 0
        rec["reasoning_tokens"] += reasoning_tokens or 0
        rec["malformed"] += malformed or 0

    def bot_actions(self, pid: int, counts: Optional[dict]):
        """Add a bot's actions to its turn's record: ``counts`` is ``{tool: [taken, refused]}``, what one drive or one
        answer of the bot did (``EngineGame.drive``'s ``actions`` for the seat, or ``answer``'s).

        A bot's actions never pass through the session's tool calls, so its turn has no per-tool rows (``by_tool``,
        ``tool_calls``); these counts stand in for them. Kept per tool, summed over the turn's drives."""
        rec = self.current(pid)
        if rec is None or not counts:
            return
        acts = rec.setdefault("bot_actions", {})
        for tool, pair in counts.items():
            taken, refused = (list(pair) + [0, 0])[:2]
            have = acts.setdefault(tool, [0, 0])
            have[0] += int(taken or 0)
            have[1] += int(refused or 0)

    def bump(self, pid: int, field: str, n: int = 1):
        """Increment a counter on the current turn."""
        rec = self.current(pid)
        if rec is not None:
            rec[field] = rec.get(field, 0) + n

    def negotiation(self, pid: int, seconds: float, steps: int, input_tokens: int, output_tokens: int):
        """Record a negotiation handled during this turn."""
        self.data["negotiations"].append({"player": pid, "t": time.time(), "seconds": round(seconds, 3), "steps": steps,
                                          "input_tokens": input_tokens, "output_tokens": output_tokens})

    # ------------------------------------------------------------------
    def summary(self, players: dict) -> dict:
        """players: {pid: {"name", "controller", "model"}} — returns per-seat aggregates."""
        out = {}
        for pid, info in players.items():
            turns = [r for r in self.data["turns"] if r["player"] == pid and self._valid(r)]
            if not turns:
                out[pid] = {**info, "turns": 0}
                continue
            walls = [r["wall_s"] for r in turns]
            model_s = sum(r["model_s"] for r in turns)
            out_tok = sum(r["output_tokens"] for r in turns)
            by_tool: dict = {}
            sig_counts: dict = {}
            reasons: dict = {}
            errors: dict = {}
            for r in turns:
                for name, t in r["by_tool"].items():
                    agg = by_tool.setdefault(name, {"count": 0, "errors": 0, "seconds": 0.0, "max_in_turn": 0})
                    agg["count"] += t["count"]
                    agg["errors"] += t["errors"]
                    agg["seconds"] += t["seconds"]
                    agg["max_in_turn"] = max(agg["max_in_turn"], t["count"])
                for sig, n in r["signatures"].items():
                    if n > 1:
                        sig_counts[sig] = max(sig_counts.get(sig, 0), n)
                reasons[r["end_reason"]] = reasons.get(r["end_reason"], 0) + 1
                for e in r["error_samples"]:
                    key = e.split(":", 1)[0] + ":" + e.split(":", 1)[1][:80] if ":" in e else e
                    errors[key] = errors.get(key, 0) + 1
            n = len(turns)
            tool_calls = sum(r["tool_calls"] for r in turns)
            for agg in by_tool.values():
                agg["per_turn"] = round(agg["count"] / n, 2)
                agg["avg_ms"] = round(agg["seconds"] / max(1, agg["count"]) * 1000, 1)
                agg["seconds"] = round(agg["seconds"], 2)
            bot = _bot_totals(turns)
            out[pid] = {
                **info,
                "turns": n,
                "avg_turn_s": round(statistics.mean(walls), 1),
                "median_turn_s": round(statistics.median(walls), 1),
                "max_turn_s": round(max(walls), 1),
                "total_s": round(sum(walls), 1),
                "avg_model_steps": round(sum(r["model_steps"] for r in turns) / n, 2),
                "avg_step_s": round(model_s / max(1, sum(r["model_steps"] for r in turns)), 2),
                "avg_tool_calls": round(tool_calls / n, 2),
                "avg_actions_ok": round(sum(r["actions_ok"] for r in turns) / n, 2),
                "avg_queries": round(sum(r["queries"] for r in turns) / n, 2),
                "error_rate": round(sum(r["errors"] for r in turns) / max(1, tool_calls), 3),
                "avg_errors": round(sum(r["errors"] for r in turns) / n, 2),
                "avg_repeats": round(sum(r["repeats"] for r in turns) / n, 2),
                "blocked_repeats": sum(r["blocked_repeats"] for r in turns),
                "malformed_calls": sum(r["malformed"] for r in turns),
                "stall_nudges": sum(r.get("stall_nudges", 0) for r in turns),
                "avg_input_tokens": round(sum(r["input_tokens"] for r in turns) / n),
                "avg_output_tokens": round(out_tok / n),
                "avg_reasoning_tokens": round(sum(r["reasoning_tokens"] for r in turns) / n),
                "output_tokens_per_s": round(out_tok / model_s, 1) if model_s > 0 else None,
                "peak_prompt_tokens": max((r.get("peak_prompt_tokens", 0) for r in turns), default=0),
                "slowest_step_s": max((r.get("slowest_step_s", 0) for r in turns), default=0),
                "context_length": next((r["context_length"] for r in reversed(turns) if r.get("context_length")), None),
                "turns_model_not_loaded": sum(1 for r in turns if r.get("model_loaded_at_start") is False),
                "end_reasons": reasons,
                "tools": dict(sorted(by_tool.items(), key=lambda kv: -kv[1]["count"])),
                "top_repeats": sorted(({"call": k, "max_times_in_a_turn": v} for k, v in sig_counts.items()),
                                      key=lambda d: -d["max_times_in_a_turn"])[:10],
                "top_errors": sorted(({"error": k, "count": v} for k, v in errors.items()), key=lambda d: -d["count"])[:10],
            }
            if bot:
                taken = sum(a["taken"] for a in bot.values())
                refused = sum(a["refused"] for a in bot.values())
                for agg in bot.values():
                    agg["per_turn"] = round(agg["taken"] / n, 2)
                out[pid]["bot_actions"] = bot
                out[pid]["avg_bot_actions"] = round(taken / n, 2)
                out[pid]["bot_refusal_rate"] = round(refused / max(1, taken + refused), 3)
            negs = [x for x in self.data["negotiations"] if x["player"] == pid]
            if negs:
                out[pid]["negotiation_replies"] = len(negs)
                out[pid]["avg_negotiation_s"] = round(statistics.mean(x["seconds"] for x in negs), 1)
        return out

    def turn_rows(self) -> list[dict]:
        """Every turn as a row, which is what the CSV export and the charts are built from. A bot's turn has no
        per-tool rows: its ``top_tools`` are its actions taken (``bot_actions``), and ``bot_actions`` is kept as the
        record has it, ``{tool: [taken, refused]}``."""
        rows = []
        for r in self.data["turns"]:
            row = {k: v for k, v in r.items() if k not in ("signatures", "by_tool", "error_samples")}
            if r.get("by_tool"):
                row["top_tools"] = ", ".join(f"{k}x{v['count']}" for k, v in sorted(r["by_tool"].items(), key=lambda kv: -kv[1]["count"])[:6])
            else:
                acts = r.get("bot_actions") or {}
                row["top_tools"] = ", ".join(f"{k}x{v[0]}" for k, v in sorted(acts.items(), key=lambda kv: -kv[1][0])[:6] if v[0])
            rows.append(row)
        return rows


class _Encoded:
    """The settled prefix of one of a Metrics' lists, encoded (``Metrics.json_parts``): ``blocks`` joined are the JSON
    of ``of[:done]`` without its brackets, each block holding the records one save found newly settled."""

    __slots__ = ("of", "done", "blocks")

    def __init__(self, of: list):
        self.of, self.done, self.blocks = of, 0, []


def _bot_totals(turns: list[dict]) -> dict:
    """A bot seat's actions over its turns, by tool: {"taken", "refused", "max_in_turn"}; empty for a seat that is no
    bot (or a bot recorded before its actions were counted)."""
    out: dict = {}
    for r in turns:
        for tool, (taken, refused) in (r.get("bot_actions") or {}).items():
            agg = out.setdefault(tool, {"taken": 0, "refused": 0, "max_in_turn": 0})
            agg["taken"] += taken
            agg["refused"] += refused
            agg["max_in_turn"] = max(agg["max_in_turn"], taken)
    return dict(sorted(out.items(), key=lambda kv: -kv[1]["taken"]))


def bot_actions_text(acts: Optional[dict]) -> str:
    """A turn's ``bot_actions`` as one line for the CSV: "move_unit 12 (2 refused), found_city 1"."""
    parts = []
    for tool, (taken, refused) in sorted((acts or {}).items(), key=lambda kv: -kv[1][0]):
        parts.append(f"{tool} {taken}" + (f" ({refused} refused)" if refused else ""))
    return ", ".join(parts)
