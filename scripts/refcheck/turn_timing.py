"""Time the Python engine's pass rounds on the corpus states, for the Rust port's speed ratios
(crates/citar-engine DESIGN.md 9.7 and 10, package 1e-03).

    PYTHONHASHSEED=0 python scripts/refcheck/turn_timing.py --corpus refcheck/corpus
        # writes refcheck/perf/python-turns.json
    PYTHONHASHSEED=0 python scripts/refcheck/turn_timing.py --corpus refcheck/corpus --only small-continents-normal-s1025

A pass round is a round in which every seat ends its turn with nothing played: the engine's own
work, with no bot or model moves (``testops._end_round``). Each state is loaded afresh and its
visibility refreshed, as the recorder settles a state (``common.settle``); one round is passed
untimed, so the caches hold what a game under way holds; then the next round is timed, ``repeat``
times from a fresh load, keeping the fastest. ``--workers`` times several states side by side,
one process each (a Python round is single-threaded; the default is one). The Rust bench (crates/citar-bench/benches/turns.rs)
times the same round the same way and ``cargo xtask perf`` prints the ratio of the two.

The file maps ``<case>/t<turn>`` to ``{"ms": the timed round, "warm_ms": the untimed one,
"turn": the turn the timed round began at}``, or to ``{"skip": why}`` for a state whose game is
over before the timed round ends. Timing is wall clock on one core, so run it on an idle machine.
"""
from __future__ import annotations

import argparse
import json
import multiprocessing
import platform
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import common

OUT = ROOT / "refcheck" / "perf" / "python-turns.json"


def states(folder: Path, only: list[str]):
    """Every ``<case>/t<turn>.json.gz`` of the folder, by case and turn."""
    out = []
    for case in sorted(p for p in folder.iterdir() if p.is_dir()):
        if only and case.name not in only:
            continue
        for f in case.glob("t*.json.gz"):
            out.append((case.name, int(f.name[1:].split(".")[0]), f))
    return sorted(out, key=lambda x: (x[0], x[1]))


def pass_round(g) -> float:
    """Seconds taken to end every turn left in the round, as testops._end_round does."""
    start, t0 = g.turn, time.perf_counter()
    while g.s.phase == "playing" and g.turn == start:
        g.end_turn(g.s.current)
    return time.perf_counter() - t0


def time_state(text: str, repeat: int) -> dict:
    """The fastest of ``repeat`` timed rounds, each after an untimed one from a fresh load."""
    from citar.engine import visibility
    best, warm, turn = None, None, None
    for _ in range(repeat):
        g = common.load_game(text)
        visibility.refresh(g, force=True)
        w = pass_round(g)
        if g.s.phase != "playing":
            return {"skip": "the game ends in the untimed round"}
        at = g.turn
        took = pass_round(g)
        if g.s.phase != "playing":
            return {"skip": "the game ends in the timed round"}
        if best is None or took < best:
            best, warm, turn = took, w, at
    return {"ms": round(best * 1000, 3), "warm_ms": round(warm * 1000, 3), "turn": turn}


def time_path(job) -> dict:
    """``time_state`` for one fixture file (a worker's job)."""
    path, repeat = job
    return time_state(json.dumps(common.read_gz(path)["state"]), repeat)


def dumps(doc: dict) -> str:
    """The file's text: the header fields a line each, then one line per state."""
    head = [f" {json.dumps(k)}: {json.dumps(v)}," for k, v in doc.items() if k != "rounds"]
    rows = [f"  {json.dumps(k)}: {json.dumps(v)}" for k, v in doc["rounds"].items()]
    return "{\n" + "\n".join(head) + '\n "rounds": {\n' + ",\n".join(rows) + "\n }\n}\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--corpus", type=Path, default=ROOT / "refcheck" / "corpus")
    ap.add_argument("--out", type=Path, default=OUT)
    ap.add_argument("--repeat", type=int, default=1, help="timed rounds per state, each from a fresh load")
    ap.add_argument("--workers", type=int, default=1, help="states timed side by side")
    ap.add_argument("--only", nargs="*", default=[], help="cases to time (all by default)")
    args = ap.parse_args()
    common.ensure_hash_seed()
    found = states(args.corpus, args.only)
    if not found:
        print(f"no states under {args.corpus}", file=sys.stderr)
        return 2
    rounds, clock = {}, common.Timer()
    jobs = [(path, max(1, args.repeat)) for _, _, path in found]
    with multiprocessing.Pool(max(1, args.workers)) as pool:
        for i, ((case, turn, _), row) in enumerate(zip(found, pool.imap(time_path, jobs)), 1):
            rounds[f"{case}/t{turn}"] = row
            print(f"[{i}/{len(found)} {clock()}s] {case}/t{turn}: {row}", flush=True)
    doc = {
        "format": 1,
        "engine": common.engine_hash(),
        "python": platform.python_version(),
        "machine": platform.processor() or platform.machine(),
        "method": "fresh load and visibility refresh, one untimed pass round, the next timed; "
                  f"fastest of {max(1, args.repeat)}; {max(1, args.workers)} worker(s)",
        "rounds": dict(sorted(rounds.items())),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(dumps(doc), encoding="utf-8", newline="\n")
    print(f"wrote {args.out} ({len(rounds)} states)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
