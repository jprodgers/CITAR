"""Model evaluation across games: per-model metric aggregates and the overall model score.

Overall score (0-100) = weighted mix of
  * Benchmark performance - how the model does in benchmark games against scripted bots: 100 for a win, 0 if
    eliminated, otherwise its share of the score against the strongest bot (50 = level). Each game is weighted by the
    turns it has been played, so long games count far more than short tests.
  * Reliability - clean turn endings, few rejected orders, little looping (all games).
  * Speed - average turn time on a log scale: 20s or less = 100, 30 minutes or more = 0 (all games). A benchmark job
    whose start had to go ahead beside pooled prompts still running on its machine (``contaminated``,
    design/phase3-features.md P3.3.3) leaves its model seat's timings out: they measure the other work as well.
Models without benchmark games get no overall score (the benchmark is the big part).

Pooling (design/phase3-features.md P3.2.4): a member's figures are made from their own games and the games
other members pool (``aggregate.scope_for``: the game's row says `pool` and its owner has not opted out), and a
model's pooled part only when enough other accounts are behind it (``aggregate.publishable``); other members'
games are counted, never named. An administrator's are made from every game, as before. Python lines this
replaces: ``_reports`` reading every save whatever its owner's choice (``scoring.py:87-115`` at ``d93345a``).
"""
from __future__ import annotations

import math
from pathlib import Path
from typing import Optional

from .. import aggregate, engine_api
from .metrics import Metrics
from .session import SAVE_DIR, SessionManager


def viewer_scope(session, viewer) -> Optional[aggregate.Scope]:
    """The games ``viewer``'s figures may be made from (``aggregate.scope_for``), or None for an administrator in
    good standing, whose figures cover every game."""
    if viewer is not None and viewer.role == "admin" and viewer.status == "active":
        return None
    return aggregate.scope_for(session, viewer)

_digest_cache: dict[str, tuple[float, dict]] = {}


def _digest_save(path: Path) -> Optional[dict]:
    """Extract what scoring needs from a save file (cached by modification time): the state's summary from its header,
    the session's record and metrics from its body (never the history)."""
    key = str(path)
    try:
        mtime = path.stat().st_mtime
    except OSError:
        return None
    hit = _digest_cache.get(key)
    if hit and hit[0] == mtime:
        return hit[1]
    try:
        doc = engine_api.read_save(path)
        sess, metrics = doc.session, doc.metrics
    except Exception:
        return None
    seats = sess.get("seats", [])
    # the save's engine part is read through the facade, which is all that knows its layout
    state = doc.summary
    summary = {}
    if metrics:
        m = Metrics(metrics)
        summary = m.summary({st["player"]: {"name": state["names"][st["player"]], "controller": st["type"],
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
    """Extract a benchmark result from a saved game: ``state`` is the summary of its state (its header's, as
    engine_api.state_summary gives it)."""
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
    scores = state["scores"]
    if not scores:
        return None             # no turn has been scored yet
    alive = {p["id"]: p["alive"] for p in state["majors"]}
    model = (bench or {}).get("model") or (seats[llm].get("llm") or {}).get("model")
    limit = state["turn_limit"] or 0
    return {"llm_player": llm, "contaminated": bool((bench or {}).get("contaminated")),
            "model": model, "server": (bench or {}).get("server") or (seats[llm].get("llm") or {}).get("base_url"),
            "scenario": (bench or {}).get("scenario") or f"{state['map_size']} / {state['map_type']} (legacy bench)",
            "run_id": (bench or {}).get("run_id"), "job_id": (bench or {}).get("job_id"),
            "turns": max(0, state["turn"] - 1), "turn_limit": limit, "phase": state["phase"],
            "performance": performance(scores, llm, state["phase"] or "playing", state["winner_id"], alive),
            "score": scores.get(llm), "best_bot_score": max([v for k, v in scores.items() if k != llm] or [0])}


def _reports(manager: SessionManager, allowed: Optional[set] = None) -> dict:
    """{game_id: {"name", "turn", "summary", "benchmark"}} for running games and the latest save of every other game;
    only the games in ``allowed`` when it is given (a member's scope), whose saves alone are read."""
    from .benchmarks import game_progress
    out = {}
    for s in list(manager.sessions.values()):
        if allowed is not None and s.id not in allowed:
            continue
        with s.lock:
            # dry-run seats test a setup; they are not models and don't count toward scores
            dry = {st.player for st in s.seats if st.type == "llm" and st.llm.get("provider") == "dryrun"}
            bench = None
            if s.benchmark and s.benchmark.get("llm_player", 0) not in dry:
                p = game_progress(s)
                bench = {"llm_player": s.benchmark.get("llm_player", 0),
                         "contaminated": bool(s.benchmark.get("contaminated")),
                         "model": s.benchmark.get("model"), "server": s.benchmark.get("server"), "scenario": s.benchmark.get("scenario"),
                         "run_id": s.benchmark.get("run_id"), "job_id": s.benchmark.get("job_id"),
                         "turns": max(0, s.game.turn - 1), "turn_limit": p["turn_limit"], "phase": p["phase"],
                         "performance": p["performance"], "score": p["score"], "best_bot_score": p["best_bot_score"]}
            summary = {pid: row for pid, row in s.metrics_report()["summary"].items() if pid not in dry}
            out[s.id] = {"name": s.name, "turn": s.game.turn, "summary": summary, "benchmark": bench}
    if SAVE_DIR.exists():
        seen = set(out)
        for p in sorted(SAVE_DIR.glob("*/*.citar"), key=lambda p: -p.stat().st_mtime):
            gid = p.parent.name
            if gid in seen or (allowed is not None and gid not in allowed):
                continue
            seen.add(gid)
            d = _digest_save(p)
            if d:
                out[gid] = d
    return out


def _label(row: dict):
    """The model a seat's metrics are filed under (its controller for a seat with no model: a bot, a person)."""
    return row.get("model") or row.get("controller")


def _published(reports: dict, scope: Optional[aggregate.Scope]) -> tuple:
    """For a member's ``scope``: per model, the games its figures may use, and the models whose pooled part was
    withheld with how many accounts it had (``aggregate.publishable``); (None, {}) for an administrator."""
    if scope is None:
        return None, {}
    by_model: dict = {}
    for gid, rep in reports.items():
        for row in rep["summary"].values():
            if row.get("turns"):
                by_model.setdefault(_label(row), set()).add(gid)
        b = rep.get("benchmark")
        if b and b.get("model"):
            by_model.setdefault(b["model"], set()).add(gid)
    return aggregate.publishable(scope, by_model)


def _withheld_note(contributors: int) -> str:
    """Why a model's pooled figures are not shown."""
    return (f"Pooled results from {contributors} other account{'s' if contributors != 1 else ''}: at least "
            f"{aggregate.min_contributors()} are needed before they are shown as an average.")


def compare_models(manager: SessionManager, reports: Optional[dict] = None,
                   scope: Optional[aggregate.Scope] = None) -> list[dict]:
    """Aggregate AI metrics per model across running games and saved games (latest save per game): every game for an
    administrator (``scope`` None); for a member, their own games and the pooled ones (``viewer_scope``), a model's
    pooled part only when enough accounts are behind it, and only their own games named (``games_list``; the
    others are counted in ``pooled_games``). A model whose figures were all withheld is a row with ``withheld``
    and the reason."""
    reports = reports if reports is not None else _reports(manager, None if scope is None else scope.game_ids)
    allowed, withheld = _published(reports, scope)
    by_model: dict = {}
    for gid, rep in reports.items():
        bench = rep.get("benchmark") or {}
        for pid, row in rep["summary"].items():
            if not row.get("turns"):
                continue
            key = _label(row)
            if allowed is not None and gid not in allowed.get(key, ()):
                continue
            # a contaminated benchmark job's model seat: its timings measured the pooled prompts beside it too
            timed = not (bench.get("contaminated") and pid == bench.get("llm_player"))
            agg = by_model.setdefault(key, {"model": key, "controller": row.get("controller"), "games": 0, "turns": 0,
                                            "_wall": 0.0, "_steps": 0.0, "_calls": 0.0, "_errors": 0.0, "_repeats": 0.0,
                                            "_out": 0.0, "_model_s_tok": [], "malformed_calls": 0, "blocked_repeats": 0,
                                            "stall_nudges": 0, "end_reasons": {}, "max_turn_s": 0.0, "game_list": [],
                                            "pooled_games": 0, "_gen": 0.0, "_queue": 0.0, "_gen_turns": 0,
                                            "_timed": 0, "contaminated_games": 0})
            n = row["turns"]
            agg["games"] += 1
            agg["turns"] += n
            if timed:
                agg["_timed"] += n
                agg["_wall"] += row["avg_turn_s"] * n
                agg["max_turn_s"] = max(agg["max_turn_s"], row["max_turn_s"])
                if row.get("output_tokens_per_s"):
                    agg["_model_s_tok"].append(row["output_tokens_per_s"])
                if row.get("avg_model_s") is not None:
                    # generation time and the wait for the machine, apart (P3.3.4)
                    agg["_gen"] += row["avg_model_s"] * n
                    agg["_queue"] += (row.get("avg_queue_s") or 0.0) * n
                    agg["_gen_turns"] += n
            else:
                agg["contaminated_games"] += 1
            agg["_steps"] += row["avg_model_steps"] * n
            agg["_calls"] += row["avg_tool_calls"] * n
            agg["_errors"] += row["avg_errors"] * n
            agg["_repeats"] += row["avg_repeats"] * n
            agg["_out"] += row["avg_output_tokens"] * n
            agg["malformed_calls"] += row["malformed_calls"]
            agg["blocked_repeats"] += row["blocked_repeats"]
            agg["stall_nudges"] += row["stall_nudges"]
            for k, v in row["end_reasons"].items():
                agg["end_reasons"][k] = agg["end_reasons"].get(k, 0) + v
            if scope is None or gid in scope.own_ids:
                agg["game_list"].append(f"{rep['name']} (T{rep['turn']})")
            else:
                agg["pooled_games"] += 1
    out = []
    for agg in by_model.values():
        t = max(1, agg["turns"])
        clean_ends = agg["end_reasons"].get("end_turn", 0)
        g = agg["_gen_turns"]
        timed = agg["_timed"]
        out.append({
            "model": agg["model"], "controller": agg["controller"], "games": agg["games"], "turns": agg["turns"],
            # timings come from the turns that were timed cleanly (``timed_turns``): None when there were none
            "avg_turn_s": round(agg["_wall"] / timed, 1) if timed else None,
            "max_turn_s": round(agg["max_turn_s"], 1) if timed else None,
            "timed_turns": timed, "contaminated_games": agg["contaminated_games"],
            "avg_model_s": round(agg["_gen"] / g, 2) if g else None,
            "avg_queue_s": round(agg["_queue"] / g, 2) if g else None,
            "avg_model_steps": round(agg["_steps"] / t, 2), "avg_tool_calls": round(agg["_calls"] / t, 2),
            "avg_errors": round(agg["_errors"] / t, 2), "avg_repeats": round(agg["_repeats"] / t, 2),
            "avg_output_tokens": round(agg["_out"] / t),
            "output_tokens_per_s": round(sum(agg["_model_s_tok"]) / len(agg["_model_s_tok"]), 1) if agg["_model_s_tok"] else None,
            "malformed_calls": agg["malformed_calls"], "blocked_repeats": agg["blocked_repeats"],
            "stall_nudges": agg["stall_nudges"], "clean_turn_end_rate": round(clean_ends / t, 3),
            "end_reasons": agg["end_reasons"], "games_list": agg["game_list"], "pooled_games": agg["pooled_games"],
            "pooled_withheld": withheld.get(agg["model"]),
        })
    out.sort(key=lambda r: (r["controller"] != "llm", r["avg_turn_s"] if r["avg_turn_s"] is not None else math.inf))
    shown = {r["model"] for r in out}
    controllers = _controllers(reports) if withheld else {}
    for key, contributors in sorted(withheld.items(), key=lambda kv: str(kv[0])):
        if key not in shown and key in controllers:
            out.append({"model": key, "controller": controllers[key], "withheld": True,
                        "contributors": contributors, "reason": _withheld_note(contributors)})
    return out


def _controllers(reports: dict) -> dict:
    """Each model label's controller (``llm``, ``bot``...), from the seats' metrics."""
    out: dict = {}
    for rep in reports.values():
        for row in rep["summary"].values():
            if row.get("turns"):
                out.setdefault(_label(row), row.get("controller"))
    return out


def reliability_score(m: dict) -> float:
    """How reliably a model played: clean turn endings, few rejected orders, little looping."""
    clean = m["clean_turn_end_rate"]
    error_rate = m["avg_errors"] / m["avg_tool_calls"] if m["avg_tool_calls"] else 0.0
    return round(100 * clean * (1 - min(0.5, error_rate)) * (1 - min(0.5, m["avg_repeats"] / 10)), 1)


def speed_score(seconds_per_turn: float) -> float:
    """How fast a model played, on a log scale from 20 seconds to 30 minutes a turn."""
    lo, hi = math.log(20), math.log(1800)
    t = math.log(max(1.0, seconds_per_turn))
    return round(100 * max(0.0, min(1.0, (hi - t) / (hi - lo))), 1)


def model_speed(m: dict) -> Optional[float]:
    """A model's speed score from its generation time per turn only (P3.3.4): time it waited for a busy machine, and
    the game's own time, are never the model's. A row with no generation figure (metrics older than the scheduler)
    falls back to its turn time; one with no cleanly timed turn at all (every game of it a contaminated benchmark job)
    has no speed score."""
    gen = m.get("avg_model_s")
    if gen is not None:
        return speed_score(gen)
    return speed_score(m["avg_turn_s"]) if m.get("avg_turn_s") is not None else None


def model_scores(manager: SessionManager, weights: dict, scope: Optional[aggregate.Scope] = None) -> dict:
    """Every model's overall score and its components: from every game for an administrator (``scope`` None), from a
    member's own and the pooled games for a member, as ``compare_models`` says. A member is shown the games behind a
    score only when they are their own (``games``; ``pooled_games`` counts the rest), and is told which models' pooled
    figures were withheld (``withheld``) and what their scope holds (``scope``)."""
    reports = _reports(manager, None if scope is None else scope.game_ids)
    allowed, withheld = _published(reports, scope)
    metrics = {m["model"]: m for m in compare_models(manager, reports, scope)
               if m["controller"] == "llm" and not m.get("withheld")}
    bench: dict[str, list] = {}
    pooled: dict[str, int] = {}
    for gid, rep in reports.items():
        b = rep.get("benchmark")
        if not b or not b.get("model") or b.get("performance") is None or b["turns"] < 1:
            continue
        if allowed is not None and gid not in allowed.get(b["model"], ()):
            continue
        own = scope is None or gid in scope.own_ids
        bench.setdefault(b["model"], []).append({**b, "game_id": gid, "game": rep["name"], "_own": own})
        if not own:
            pooled[b["model"]] = pooled.get(b["model"], 0) + 1
    wb, wr, ws = (float(weights.get(k, 0)) for k in ("benchmark", "reliability", "speed"))
    rows = []
    for model in sorted((set(metrics) | set(bench)) - {"llm", None}):   # "llm" = seats with no model id (tests)
        games = bench.get(model, [])
        m = metrics.get(model)
        turns = sum(g["turns"] for g in games)
        perf = round(sum(g["performance"] * g["turns"] for g in games) / turns, 1) if turns else None
        rel = reliability_score(m) if m else None
        spd = model_speed(m) if m else None
        overall = None
        # with no speed score (no cleanly timed turn), the overall score is made of the other two
        parts = [(wb, perf), (wr, rel)] + ([(ws, spd)] if spd is not None else [])
        weight = sum(w for w, _ in parts)
        if perf is not None and rel is not None and weight > 0:
            overall = round(sum(w * v for w, v in parts) / weight, 1)
        rows.append({
            "model": model, "overall": overall, "benchmark": perf, "reliability": rel, "speed": spd,
            "benchmark_games": len(games), "benchmark_turns": turns,
            "benchmark_finished": sum(1 for g in games if g["phase"] != "playing"),
            "wins": sum(1 for g in games if g["performance"] == 100.0 and g["phase"] != "playing"),
            # another member's game is a number in these figures, never a name or an id
            "games": sorted(({k: v for k, v in g.items() if k != "_own"} for g in games if g["_own"]),
                            key=lambda g: -g["turns"]),
            "pooled_games": pooled.get(model, 0),
            "pooled_withheld": withheld.get(model),
            "metrics": m,
        })
    rows.sort(key=lambda r: (r["overall"] is None, -(r["overall"] or 0), -(r["benchmark"] or 0)))
    out = {"weights": {"benchmark": wb, "reliability": wr, "speed": ws}, "models": rows}
    if scope is not None:
        shown = {r["model"] for r in rows}
        out["scope"] = scope.client()
        out["withheld"] = [{"model": k, "contributors": n, "reason": _withheld_note(n)}
                           for k, n in sorted(withheld.items(), key=lambda kv: str(kv[0]))
                           if k not in shown and k not in (None, "llm", "bot", "human", "mcp")]
    return out
