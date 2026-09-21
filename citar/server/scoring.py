"""Model evaluation across games: per-model metric aggregates and the overall model score.

Overall score (0-100) = weighted mix of
  * Benchmark performance - how the model does in benchmark games against scripted bots: 100 for a win, 0 if
    eliminated, otherwise its share of the score against the strongest bot (50 = level). Each game is weighted by the
    turns it has been played, so long games count far more than short tests.
  * Reliability - clean turn endings, few rejected orders, little looping (all games).
  * Speed - average turn time on a log scale: 20s or less = 100, 30 minutes or more = 0 (all games).
Models without benchmark games get no overall score (the benchmark is the big part).
"""
from __future__ import annotations

import math
from pathlib import Path
from typing import Optional

from .metrics import Metrics
from .session import SAVE_DIR, SessionManager, load_save_file

_digest_cache: dict[str, tuple[float, dict]] = {}


def _digest_save(path: Path) -> Optional[dict]:
    """Extract what scoring needs from a save file (cached by modification time)."""
    key = str(path)
    try:
        mtime = path.stat().st_mtime
    except OSError:
        return None
    hit = _digest_cache.get(key)
    if hit and hit[0] == mtime:
        return hit[1]
    try:
        data = load_save_file(path)
    except Exception:
        return None
    sess = data.get("session", {})
    seats = sess.get("seats", [])
    state = data["state"]
    players = {p["id"]: p for p in state["players"]}
    summary = {}
    if data.get("metrics"):
        m = Metrics(data["metrics"])
        summary = m.summary({st["player"]: {"name": players[st["player"]]["name"], "controller": st["type"],
                                            "model": (st.get("llm") or {}).get("model") if st["type"] == "llm" else None}
                             for st in seats})
    dry = {st["player"] for st in seats if (st.get("llm") or {}).get("provider") == "dryrun"}
    summary = {pid: row for pid, row in summary.items() if pid not in dry}
    bench = _benchmark_from_state(sess, seats, state)
    if bench and bench.get("llm_player") in dry:
        bench = None
    digest = {"game_id": sess.get("id") or path.parent.name, "name": sess.get("name", path.parent.name),
              "turn": state["turn"], "summary": summary, "benchmark": bench}
    _digest_cache[key] = (mtime, digest)
    return digest


def _benchmark_from_state(sess: dict, seats: list, state: dict) -> Optional[dict]:
    """Extract a benchmark result from a finished game's state."""
    from .benchmarks import performance
    bench = sess.get("benchmark")
    name = sess.get("name", "")
    if not bench and not name.startswith("Bench:"):
        return None
    llm = (bench or {}).get("llm_player")
    if llm is None:
        llm = next((st["player"] for st in seats if st["type"] == "llm"), None)
    if llm is None:
        return None
    stats = state.get("stats") or []
    if not stats:
        return None
    last = stats[-1]["players"]
    majors = [p for p in state["players"] if p.get("kind") == "major"]
    scores = {p["id"]: (last.get(str(p["id"])) or {}).get("score", 0) for p in majors}
    alive = {p["id"]: p.get("alive", True) for p in majors}
    model = (bench or {}).get("model") or (seats[llm].get("llm") or {}).get("model")
    limit = (state.get("config") or {}).get("turn_limit") or 0
    return {"llm_player": llm, "model": model, "server": (bench or {}).get("server") or (seats[llm].get("llm") or {}).get("base_url"),
            "scenario": (bench or {}).get("scenario") or f"{state['config'].get('map_size')} / {state['config'].get('map_type')} (legacy bench)",
            "run_id": (bench or {}).get("run_id"), "job_id": (bench or {}).get("job_id"),
            "turns": max(0, state["turn"] - 1), "turn_limit": limit, "phase": state.get("phase"),
            "performance": performance(scores, llm, state.get("phase", "playing"), state.get("winner"), alive),
            "score": scores.get(llm), "best_bot_score": max([v for k, v in scores.items() if k != llm] or [0])}


def _reports(manager: SessionManager) -> dict:
    """{game_id: {"name", "turn", "summary", "benchmark"}} for running games and the latest save of every other game."""
    from .benchmarks import game_progress
    out = {}
    for s in list(manager.sessions.values()):
        with s.lock:
            # dry-run seats test a setup; they are not models and don't count toward scores
            dry = {st.player for st in s.seats if st.type == "llm" and st.llm.get("provider") == "dryrun"}
            bench = None
            if s.benchmark and s.benchmark.get("llm_player", 0) not in dry:
                p = game_progress(s)
                bench = {"model": s.benchmark.get("model"), "server": s.benchmark.get("server"), "scenario": s.benchmark.get("scenario"),
                         "run_id": s.benchmark.get("run_id"), "job_id": s.benchmark.get("job_id"),
                         "turns": max(0, s.game.turn - 1), "turn_limit": p["turn_limit"], "phase": p["phase"],
                         "performance": p["performance"], "score": p["score"], "best_bot_score": p["best_bot_score"]}
            summary = {pid: row for pid, row in s.metrics_report()["summary"].items() if pid not in dry}
            out[s.id] = {"name": s.name, "turn": s.game.turn, "summary": summary, "benchmark": bench}
    if SAVE_DIR.exists():
        seen = set(out)
        for p in sorted(SAVE_DIR.glob("*/*.citar"), key=lambda p: -p.stat().st_mtime):
            gid = p.parent.name
            if gid in seen:
                continue
            seen.add(gid)
            d = _digest_save(p)
            if d:
                out[gid] = d
    return out


def compare_models(manager: SessionManager, reports: Optional[dict] = None) -> list[dict]:
    """Aggregate AI metrics per model across all running games and saved games (latest save per game)."""
    reports = reports if reports is not None else _reports(manager)
    by_model: dict = {}
    for gid, rep in reports.items():
        for pid, row in rep["summary"].items():
            if not row.get("turns"):
                continue
            key = row.get("model") or row.get("controller")
            agg = by_model.setdefault(key, {"model": key, "controller": row.get("controller"), "games": 0, "turns": 0,
                                            "_wall": 0.0, "_steps": 0.0, "_calls": 0.0, "_errors": 0.0, "_repeats": 0.0,
                                            "_out": 0.0, "_model_s_tok": [], "malformed_calls": 0, "blocked_repeats": 0,
                                            "stall_nudges": 0, "end_reasons": {}, "max_turn_s": 0.0, "game_list": []})
            n = row["turns"]
            agg["games"] += 1
            agg["turns"] += n
            agg["_wall"] += row["avg_turn_s"] * n
            agg["_steps"] += row["avg_model_steps"] * n
            agg["_calls"] += row["avg_tool_calls"] * n
            agg["_errors"] += row["avg_errors"] * n
            agg["_repeats"] += row["avg_repeats"] * n
            agg["_out"] += row["avg_output_tokens"] * n
            if row.get("output_tokens_per_s"):
                agg["_model_s_tok"].append(row["output_tokens_per_s"])
            agg["malformed_calls"] += row["malformed_calls"]
            agg["blocked_repeats"] += row["blocked_repeats"]
            agg["stall_nudges"] += row["stall_nudges"]
            agg["max_turn_s"] = max(agg["max_turn_s"], row["max_turn_s"])
            for k, v in row["end_reasons"].items():
                agg["end_reasons"][k] = agg["end_reasons"].get(k, 0) + v
            agg["game_list"].append(f"{rep['name']} (T{rep['turn']})")
    out = []
    for agg in by_model.values():
        t = max(1, agg["turns"])
        clean_ends = agg["end_reasons"].get("end_turn", 0)
        out.append({
            "model": agg["model"], "controller": agg["controller"], "games": agg["games"], "turns": agg["turns"],
            "avg_turn_s": round(agg["_wall"] / t, 1), "max_turn_s": round(agg["max_turn_s"], 1),
            "avg_model_steps": round(agg["_steps"] / t, 2), "avg_tool_calls": round(agg["_calls"] / t, 2),
            "avg_errors": round(agg["_errors"] / t, 2), "avg_repeats": round(agg["_repeats"] / t, 2),
            "avg_output_tokens": round(agg["_out"] / t),
            "output_tokens_per_s": round(sum(agg["_model_s_tok"]) / len(agg["_model_s_tok"]), 1) if agg["_model_s_tok"] else None,
            "malformed_calls": agg["malformed_calls"], "blocked_repeats": agg["blocked_repeats"],
            "stall_nudges": agg["stall_nudges"], "clean_turn_end_rate": round(clean_ends / t, 3),
            "end_reasons": agg["end_reasons"], "games_list": agg["game_list"],
        })
    out.sort(key=lambda r: (r["controller"] != "llm", r["avg_turn_s"]))
    return out


def reliability_score(m: dict) -> float:
    """How reliably a model played: clean turn endings, few rejected orders, little looping."""
    clean = m["clean_turn_end_rate"]
    error_rate = m["avg_errors"] / m["avg_tool_calls"] if m["avg_tool_calls"] else 0.0
    return round(100 * clean * (1 - min(0.5, error_rate)) * (1 - min(0.5, m["avg_repeats"] / 10)), 1)


def speed_score(avg_turn_s: float) -> float:
    """How fast a model played, on a log scale from 20 seconds to 30 minutes a turn."""
    lo, hi = math.log(20), math.log(1800)
    t = math.log(max(1.0, avg_turn_s))
    return round(100 * max(0.0, min(1.0, (hi - t) / (hi - lo))), 1)


def model_scores(manager: SessionManager, weights: dict) -> dict:
    """Every model's overall score and its components."""
    reports = _reports(manager)
    metrics = {m["model"]: m for m in compare_models(manager, reports) if m["controller"] == "llm"}
    bench: dict[str, list] = {}
    for gid, rep in reports.items():
        b = rep.get("benchmark")
        if not b or not b.get("model") or b.get("performance") is None or b["turns"] < 1:
            continue
        bench.setdefault(b["model"], []).append({**b, "game_id": gid, "game": rep["name"]})
    wb, wr, ws = (float(weights.get(k, 0)) for k in ("benchmark", "reliability", "speed"))
    rows = []
    for model in sorted((set(metrics) | set(bench)) - {"llm", None}):   # "llm" = seats with no model id (tests)
        games = bench.get(model, [])
        m = metrics.get(model)
        turns = sum(g["turns"] for g in games)
        perf = round(sum(g["performance"] * g["turns"] for g in games) / turns, 1) if turns else None
        rel = reliability_score(m) if m else None
        spd = speed_score(m["avg_turn_s"]) if m else None
        overall = None
        if perf is not None and rel is not None and (wb + wr + ws) > 0:
            overall = round((wb * perf + wr * rel + ws * spd) / (wb + wr + ws), 1)
        rows.append({
            "model": model, "overall": overall, "benchmark": perf, "reliability": rel, "speed": spd,
            "benchmark_games": len(games), "benchmark_turns": turns,
            "benchmark_finished": sum(1 for g in games if g["phase"] != "playing"),
            "wins": sum(1 for g in games if g["performance"] == 100.0 and g["phase"] != "playing"),
            "games": sorted(games, key=lambda g: -g["turns"]),
            "metrics": m,
        })
    rows.sort(key=lambda r: (r["overall"] is None, -(r["overall"] or 0), -(r["benchmark"] or 0)))
    return {"weights": {"benchmark": wb, "reliability": wr, "speed": ws}, "models": rows}
