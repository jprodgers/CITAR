"""Collects everything a report needs for its scope: priced activities from the usage ledger, game outcomes and AI
metrics from save files, benchmark runs, probe runs and lab experiments, joined on the server registry."""
from __future__ import annotations

import json
import statistics
import time
from collections import defaultdict
from datetime import datetime, timedelta
from typing import Optional

from .. import costing, servers as S, usage as U

_save_cache: dict = {}


def time_range(spec: dict) -> tuple[Optional[float], float]:
    """The period a report covers, as timestamps."""
    r = spec.get("range") or {}
    now = time.time()
    preset = r.get("preset") or "all"
    if preset == "custom":
        def ts(s, end=False):
            """Parse one end of the period."""
            try:
                d = datetime.strptime(str(s)[:10], "%Y-%m-%d")
                return (d + timedelta(days=1)).timestamp() if end else d.timestamp()
            except ValueError:
                return None
        return ts(r.get("from")), ts(r.get("to"), True) or now
    days = {"today": 0, "24h": 1, "7d": 7, "30d": 30, "90d": 90, "365d": 365}.get(preset)
    if preset == "today":
        return datetime.now().replace(hour=0, minute=0, second=0, microsecond=0).timestamp(), now
    if days:
        return now - days * 86400, now
    return None, now


def _config_label(sv: Optional[dict], model: Optional[str]) -> str:
    """How to label a server-and-model combination in a report."""
    return f"{model or '?'} @ {sv['name'] if sv else 'unknown server'}"


def _game_details(game_id: str) -> Optional[dict]:
    """Outcome and AI metrics of a saved game (benchmark.citar, else autosave.citar), cached by file time."""
    from ..server.session import SAVE_DIR, load_save_file, GameSession
    folder = SAVE_DIR / game_id
    path = next((folder / n for n in ("benchmark.citar", "autosave.citar") if (folder / n).exists()), None)
    if path is None:
        return None
    key = (str(path), path.stat().st_mtime)
    if key in _save_cache:
        return _save_cache[key]
    try:
        data = load_save_file(path)
        s = GameSession.from_save(data)
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}"}
    from ..server.benchmarks import game_progress
    llm_seats = [seat for seat in s.seats if seat.type == "llm"]
    out = {"game_id": game_id, "name": s.name, "turn": s.game.turn, "phase": s.game.s.phase, "victory": s.game.s.victory,
           "winner": s.game.s.winner, "seats": []}
    for seat in llm_seats:
        s.benchmark = dict(s.benchmark or {}, llm_player=seat.player, model=seat.llm.get("model"))
        try:
            with s.lock:
                prog = game_progress(s)
        except Exception:
            prog = {}
        players = {seat.player: {"name": s.game.player(seat.player).name, "controller": "llm", "model": seat.llm.get("model")}}
        summ = s.metrics.summary(players).get(seat.player, {})
        prog.pop("series", None)
        out["seats"].append({"player": seat.player, "server_id": seat.llm.get("server_id"), "model": seat.llm.get("model"),
                             "progress": prog, "metrics": summ})
    s.stop()
    _save_cache[key] = out
    return out


def _benchmark_runs(run_ids: set) -> list[dict]:
    """The benchmark runs in scope."""
    from ..server.benchmarks import BENCH_DIR, run_summary
    out = []
    for rid in run_ids:
        p = BENCH_DIR / "runs" / f"{rid}.json"
        if not p.exists():
            continue
        try:
            run = json.loads(p.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        run["summary"] = run_summary(run)
        out.append(run)
    return out


def _probe_runs(run_ids: set) -> list[dict]:
    """The probe runs in scope."""
    from ..probes import RUNS
    out = []
    for rid in run_ids:
        d = RUNS / rid
        try:
            run = json.loads((d / "run.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        results = []
        if (d / "results.jsonl").exists():
            for line in (d / "results.jsonl").read_text(encoding="utf-8").splitlines():
                try:
                    results.append(json.loads(line))
                except ValueError:
                    pass
        run["results"] = results
        out.append(run)
    return out


def _lab(exps: set) -> dict:
    """The lab experiments in scope."""
    from .. import lab
    out = {}
    for e in exps:
        try:
            res = lab.load_results(e)
            out[e] = {"summary": lab.summarize(res), "games": len(res)}
        except Exception as ex:
            out[e] = {"error": str(ex)}
    return out


def _in_scope(a: dict, info: dict, scope: dict, servers_by_id: dict) -> bool:
    """Whether one activity belongs in this report."""
    kinds = scope.get("kinds") or []
    if kinds and info.get("kind") not in kinds:
        return False
    items = scope.get("items") or []
    if items:
        parent = info.get("parent") or {}
        ref = info.get("ref") or {}
        ok = False
        for it in items:
            k, i = it.get("kind"), it.get("id")
            if (k == "activity" and info.get("id") == i) or (k == "game" and ref.get("game_id") == i) \
                    or (k in ("benchmark_run", "probe", "lab_experiment", "suite") and parent.get("id") == i and parent.get("kind") == k) \
                    or (k == "probe_run" and ref.get("probe_run") == i) or (k == "suite" and ref.get("suite_id") == i):
                ok = True
                break
        if not ok:
            return False
    want_srv = set(scope.get("servers") or [])
    if want_srv and not (want_srv & set(a["servers"])):
        return False
    want_models = set(scope.get("models") or [])
    if want_models:
        models = set(a["models"]) | {s.get("model") for s in info.get("seats") or [] if s.get("model")} | {info.get("model")}
        if not (want_models & models):
            return False
    return True


def gather(spec: dict, progress=lambda msg: None) -> dict:
    """Collect everything a report needs from the ledger and the run records.

    Separate from pricing and from rendering, so that what is measured, what it cost and how it is
    presented can each be changed without disturbing the other two.
    """
    since, until = time_range(spec)
    reg = S.snapshot()
    servers_by_id = {s["id"]: s for s in reg["servers"]}
    basis = (spec.get("options") or {}).get("electricity_basis", "full")
    progress("Reading the usage ledger")
    ledger = U.read(since, until)
    if since is None:       # "all time": from the first recorded use
        since = min((float(sp["t0"]) for sp in ledger["spans"]), default=until - 86400)
    progress("Pricing usage")
    cost = costing.compute(since, until, reg, ledger)
    scope = spec.get("scope") or {}
    acts = []
    bench_ids, probe_ids, lab_exps = set(), set(), set()
    game_ids = []
    for act_id, a in cost["acts"].items():
        info = a["info"]
        if not _in_scope(a, info, scope, servers_by_id):
            continue
        parent = info.get("parent") or {}
        kind = info.get("kind")
        tot = a["total"]
        # the configuration this activity measures: its AI seat's model on its server (lab games: the bots)
        seats = [s for s in info.get("seats") or [] if s.get("model")]
        if kind == "lab":
            config = f"bots · {parent.get('name')}"
        elif seats:
            config = " + ".join(sorted({_config_label(servers_by_id.get(s.get("server_id")), s.get("model")) for s in seats}))
        elif info.get("model"):
            config = _config_label(servers_by_id.get(info.get("server_id")), info.get("model"))
            if kind == "report":            # writing report narratives is overhead, not a model being evaluated
                config = f"report writing · {config}"
        else:
            config = "(no model)"
        row = {"id": act_id, "kind": kind, "name": info.get("name") or act_id, "parent": parent, "ref": info.get("ref") or {},
               "config": config, "start": info.get("first_t"), "end": info.get("ended") or info.get("t"),
               "servers": a["servers"], "models": a["models"], "cost": tot,
               "total": costing.total(tot, basis), "total_full": costing.total(tot, "full"),
               "total_marginal": costing.total(tot, "marginal"), "info": info}
        acts.append(row)
        if kind in ("game", "benchmark") and row["ref"].get("game_id"):
            game_ids.append(row)
        if parent.get("kind") == "benchmark_run" and parent.get("id"):
            bench_ids.add(parent["id"])
        if row["ref"].get("probe_run"):
            probe_ids.add(row["ref"]["probe_run"])
        if kind == "lab" and parent.get("id"):
            lab_exps.add(parent["id"])
    sections = set(spec.get("sections") or [])
    need_games = sections & {"models", "behavior", "benchmarks", "summary", "cost_per_unit", "activities"}
    if need_games:
        for i, row in enumerate(game_ids):
            progress(f"Reading game {i + 1} of {len(game_ids)}")
            row["game"] = _game_details(row["ref"]["game_id"])
    progress("Reading runs")
    data = {"spec": spec, "since": since, "until": until, "generated": time.time(), "registry": reg,
            "symbol": reg.get("currency_symbol", "$"), "currency": reg.get("currency", "USD"), "basis": basis,
            "cost": cost, "acts": sorted(acts, key=lambda r: r["start"] or 0), "servers_by_id": servers_by_id,
            "benchmark_runs": _benchmark_runs(bench_ids), "probe_runs": _probe_runs(probe_ids), "lab": _lab(lab_exps),
            "ledger_rows": len(ledger["spans"])}
    lo = (spec.get("options") or {}).get("lifespan_range") or [2, 6]
    if "depreciation" in sections:
        progress("Depreciation sensitivity")
        sens = {}
        ids = {r["id"] for r in acts}
        for years in range(int(lo[0]), int(lo[1]) + 1):
            alt = costing.compute(since, until, reg, ledger, lifespan_override=years)
            sens[years] = {"allocated": sum(costing.total(a["total"], basis) for k, a in alt["acts"].items() if k in ids),
                           "depreciation": sum(a["total"]["depreciation"] for k, a in alt["acts"].items() if k in ids)}
        data["sensitivity"] = sens
    return data


def config_rollup(data: dict) -> dict:
    """Per configuration (model @ server): activities, costs and the units they produced."""
    out: dict = defaultdict(lambda: {"acts": 0, "games": 0, "finished": 0, "wins": 0, "turns": 0, "model_turns": 0,
                                     "perf": [], "cost": 0.0, "cost_marginal": 0.0, "cost_games": 0.0, "cost_probes": 0.0, "in": 0, "out": 0, "cr": 0, "cw": 0,
                                     "busy_h": 0.0, "held_h": 0.0, "probe_cases": 0, "probe_passed": 0, "probe_checked": 0,
                                     "avg_turn_s": [], "error_rate": [], "clean_end": [], "kinds": set(),
                                     "servers": set(), "kwh": 0.0, "measured_h": 0.0, "estimated_h": 0.0, "act_costs": []})
    probe_results = {r["id"]: r for r in data["probe_runs"]}
    for a in data["acts"]:
        c = out[a["config"]]
        c["acts"] += 1
        c["kinds"].add(a["kind"])
        c["cost"] += a["total"]
        if a["kind"] in ("game", "benchmark", "lab"):
            c["cost_games"] += a["total"]
        elif a["kind"] == "probe":
            c["cost_probes"] += a["total"]
        c["act_costs"].append(a["total"])
        c["cost_marginal"] += a["total_marginal"]
        for srv, sc in a["servers"].items():
            c["servers"].add(srv)
            sv = data["servers_by_id"].get(srv)
            # a server whose energy (or tokens) could not be priced makes this configuration's cost a lower bound
            if sv is None or sc.get("unpriced_kwh") or sc.get("unpriced_tokens") or (
                    sv["kind"] in ("owned", "leased") and sc.get("held_h") and sv["power"].get("idle_w") is None):
                c.setdefault("_incomplete", set()).add(sv["name"] if sv else srv)
            for f in ("in", "out", "cr", "cw"):
                c[f] += sc.get(f, 0)
            c["busy_h"] += sc.get("busy_h", 0)
            c["held_h"] += sc.get("held_h", 0)
            c["kwh"] += sc.get("kwh_idle", 0) + sc.get("kwh_dynamic", 0)
            c["measured_h"] += sc.get("energy_measured_h", 0)
            c["estimated_h"] += sc.get("energy_estimated_h", 0)
        g = a.get("game")
        if g and g.get("seats"):
            c["games"] += 1
            if g.get("phase") != "playing":
                c["finished"] += 1
            for seat in g["seats"]:
                p, m = seat.get("progress") or {}, seat.get("metrics") or {}
                if p.get("performance") is not None:
                    c["perf"].append(p["performance"])
                if p.get("outcome") == "won":
                    c["wins"] += 1
                c["model_turns"] += m.get("turns") or 0
                if m.get("avg_turn_s") is not None:
                    c["avg_turn_s"].append(m["avg_turn_s"])
                if m.get("error_rate") is not None:
                    c["error_rate"].append(m["error_rate"])
                if m.get("turns"):
                    c["clean_end"].append((m.get("end_reasons") or {}).get("end_turn", 0) / m["turns"])
            c["turns"] += max(0, (g.get("turn") or 1) - 1)
        if a["kind"] == "lab":
            c["games"] += 1
            c["finished"] += 1
            c["turns"] += a["info"].get("turns") or 0
        pr = probe_results.get(a["ref"].get("probe_run"))
        if pr:
            for r in pr["results"]:
                c["probe_cases"] += 1
                if r.get("passed") is not None:
                    c["probe_checked"] += 1
                    c["probe_passed"] += 1 if r["passed"] else 0
    for c in out.values():
        c["kinds"] = sorted(c["kinds"])
        c["servers"] = sorted(c["servers"])
        c["perf_mean"] = statistics.mean(c["perf"]) if c["perf"] else None
        c["avg_turn"] = statistics.mean(c["avg_turn_s"]) if c["avg_turn_s"] else None
        c["err"] = statistics.mean(c["error_rate"]) if c["error_rate"] else None
        c["clean"] = statistics.mean(c["clean_end"]) if c["clean_end"] else None
        # per-game units use only the cost of games; probe cases use only the cost of probe runs
        c["per_game"] = c["cost_games"] / c["games"] if c["games"] else None
        c["per_turn"] = c["cost_games"] / c["model_turns"] if c["model_turns"] else (c["cost_games"] / c["turns"] if c["turns"] else None)
        tok = c["in"] + c["out"] + c["cr"] + c["cw"]
        c["per_mtok"] = c["cost"] / tok * 1e6 if tok else None
        c["per_point"] = c["cost_games"] / sum(c["perf"]) if c["perf"] and sum(c["perf"]) > 0 else None
        c["per_win"] = c["cost_games"] / c["wins"] if c["wins"] else None
        c["per_case"] = c["cost_probes"] / c["probe_cases"] if c["probe_cases"] else None
        c["pass_rate"] = c["probe_passed"] / c["probe_checked"] if c["probe_checked"] else None
        c["measured_share"] = c["measured_h"] / (c["measured_h"] + c["estimated_h"]) if (c["measured_h"] + c["estimated_h"]) else None
        c["incomplete"] = sorted(c.pop("_incomplete", set()))
    return dict(out)
