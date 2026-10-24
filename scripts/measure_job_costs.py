"""Measure what CPU jobs cost, for the dispatcher's cost table (design/phase4-helper.md P4.5.4).

    python scripts/measure_job_costs.py [--out citar/jobs/costs.json] [--seeds 3] [--quick]

Plays the jobs crate's golden jobs and a set of full and partial games per map size (the "benchmark" jobs: lab duels
and small to huge games, reference games), each through a job host of its own as the server plays them
(``citar.jobs.host.JobHost``: one thread, one core's worth of the machine, 1,024 MB), several seeds a cell, and writes
each cell's median CPU seconds and largest counted peak to ``--out``: the committed table every server seeds
``job_costs`` from, so its first jobs already have an estimate. A server's own runs replace these figures as they
finish (``citar.jobs.store.Costs``).

The figures are this machine's: a slower one gets longer deadlines from its own runs once it has played some. Run it
with the extension built (``cargo xtask develop``); it takes some minutes.
"""
from __future__ import annotations

import argparse
import json
import math
import platform
import statistics
import sys
import time
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

#: (kind, size, rounds) cells beyond the golden jobs: lab games of each size at a few turn limits, and reference games.
CELLS = [(kind, size, rounds) for kind in ("lab_game",)
         for size in ("duel", "small", "standard", "large", "huge", "gargantuan") for rounds in (100, 330)] + [
    ("lab_game", "duel", 500), ("lab_game", "small", 500), ("lab_game", "small", 750),
    ("reference_game", "duel", 330), ("reference_game", "small", 100), ("reference_game", "small", 330),
    ("reference_game", "standard", 330)]
QUICK = [("lab_game", "duel", 100), ("lab_game", "small", 100)]


def speed_for(rounds: int) -> str:
    """The fastest speed whose last turn reaches ``rounds``."""
    from citar import engine_api
    last = engine_api.rules_client()["max_turns"]
    return min((s for s in last if last[s] >= rounds), key=lambda s: last[s])


def lab_spec(size: str, rounds: int, seed: int) -> dict:
    from citar import engine_api, lab
    players = engine_api.map_sizes()[size]["players"]
    e = lab.normalize({"name": "costs", "games": 1, "seed": seed, "size": size, "maps": ["continents"],
                       "speed": speed_for(rounds), "turns": rounds,
                       "seats": [{"label": f"s{k}", "bot": "basic"} for k in range(players)]})
    return lab.job_spec(lab.game_spec(e, 0))


def reference_spec(size: str, rounds: int, seed: int) -> dict:
    return {"i": 0, "seed": seed, "size": size, "map_type": "continents", "barbarians": "normal",
            "speed": speed_for(rounds), "turn_limit": rounds, "params": None}


def play(host, kind: str, spec: dict, n: int) -> dict:
    from citar import engine_api
    from citar.jobs import host as H
    checked = engine_api.check_job_spec(kind, spec)
    run = f"cr_cost{n:06d}"
    host.offer(run, kind, checked["spec"], checked["spec_hash"], 86_400.0, mem_mb=0)
    frames = H.run_until(host, run, timeout=3600.0)
    end = frames[-1]
    if end["t"] != "job_result":
        raise RuntimeError(f"{kind} {spec.get('size')}: {end}")
    return end["outcome"]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=str(ROOT / "citar" / "jobs" / "costs.json"))
    ap.add_argument("--seeds", type=int, default=3)
    ap.add_argument("--quick", action="store_true", help="two cells only (a check of the script)")
    args = ap.parse_args(argv)
    from citar import engine_api
    from citar.jobs import host as H, store
    cpus = H.machine_cpus()
    host = H.JobHost(1, max(1, math.ceil(100 / cpus)), 1024, play_unverifiable=True)
    host.start()
    cells, n = [], 0
    try:
        golden = json.loads((ROOT / "crates" / "citar-jobs" / "golden" / "jobs.json").read_text(encoding="utf-8"))
        jobs = golden.get("jobs") or {}
        for name, job in (jobs.items() if not args.quick else ()):
            kind, spec = job["kind"], job["spec"]
            outs = []
            for _ in range(args.seeds):
                n += 1
                outs.append(play(host, kind, spec, n))
            cells.append(_cell(kind, spec, outs, f"golden {name}"))
            print(cells[-1], flush=True)
        for kind, size, rounds in (QUICK if args.quick else CELLS):
            outs = []
            t0 = time.perf_counter()
            for k in range(args.seeds):
                n += 1
                seed = 7100 + 13 * k
                spec = lab_spec(size, rounds, seed) if kind == "lab_game" else reference_spec(size, rounds, seed)
                outs.append(play(host, kind, spec, n))
            cells.append(_cell(kind, spec, outs, f"{size} to round {rounds}, {time.perf_counter() - t0:.0f} s"))
            print(cells[-1], flush=True)
    finally:
        host.close()
    merged: dict = {}
    for c in cells:
        key = (c["kind"], c["size"], c["seats"], c["rounds"])
        merged[key] = c if key not in merged or merged[key]["samples"] < c["samples"] else merged[key]
    info = engine_api.build_info()
    out = {"measured": date.today().isoformat(), "machine": f"{platform.processor() or platform.machine()} "
                                                            f"({cpus} logical CPUs, {platform.system()})",
           "build": info["build_id"], "jobs_build": engine_api.jobs_build(),
           "method": f"scripts/measure_job_costs.py: each cell {args.seeds} games through a job host of one thread "
                     "and one core's worth; cpu_s the median of the runs' CPU seconds, peak_mb the largest counted "
                     "peak; rounds the bucket of the games' turn limit (store.ROUND_BUCKET)",
           "cells": sorted(merged.values(), key=lambda c: (c["kind"], c["size"], c["seats"], c["rounds"]))}
    for c in out["cells"]:
        c["rounds"] = store.bucket(c["rounds"])
        c.pop("note", None)
    Path(args.out).write_text(json.dumps(out, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {len(out['cells'])} cells to {args.out}")
    return 0


def _cell(kind: str, spec: dict, outs: list, note: str) -> dict:
    from citar.jobs import store
    k, size, seats, rounds = store.shape(kind, spec)
    return {"kind": k, "size": size, "seats": seats, "rounds": rounds,
            "cpu_s": round(statistics.median(o["cpu_s"] for o in outs), 3),
            "peak_mb": round(max(o["peak_mb"] for o in outs), 1), "samples": len(outs), "note": note}


if __name__ == "__main__":
    sys.exit(main())
