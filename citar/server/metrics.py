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
    """
    def __init__(self, data: Optional[dict] = None):
        self.data = data or {"turns": [], "negotiations": []}
        self._open: dict[int, dict] = {}
        for rec in self.data["turns"]:
            if rec.get("ended") is None and not rec.get("interrupted"):
                # A turn that was in progress when the game was saved: the server stopped mid-turn, so its timing
                # is meaningless (it would include the downtime). Keep it for the record but exclude it from stats;
                # the turn is replayed and measured again from scratch.
                rec["interrupted"] = True
                rec["end_reason"] = "interrupted"

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
        rec["wall_s"] = round(rec["ended"] - rec["started"], 3)
        if reason and not rec["end_reason"]:
            rec["end_reason"] = reason
        rec["end_reason"] = rec["end_reason"] or "end_turn"
        self._open.pop(pid, None)

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
                  blocked_repeat: bool = False):
        """Record one tool call: its result, timing, and whether it repeated an earlier one."""
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
        if not ok and not blocked_repeat:
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
            negs = [x for x in self.data["negotiations"] if x["player"] == pid]
            if negs:
                out[pid]["negotiation_replies"] = len(negs)
                out[pid]["avg_negotiation_s"] = round(statistics.mean(x["seconds"] for x in negs), 1)
        return out

    def turn_rows(self) -> list[dict]:
        """Every turn as a row, which is what the CSV export and the charts are built from."""
        rows = []
        for r in self.data["turns"]:
            row = {k: v for k, v in r.items() if k not in ("signatures", "by_tool", "error_samples")}
            row["top_tools"] = ", ".join(f"{k}x{v['count']}" for k, v in sorted(r["by_tool"].items(), key=lambda kv: -kv[1]["count"])[:6])
            rows.append(row)
        return rows
