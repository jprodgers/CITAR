"""Distribution tables for baseline files, and a comparison of two.

    python scripts/refcheck/summarize.py refcheck/baseline/python-XXXX.jsonl
    python scripts/refcheck/summarize.py python.jsonl rust.jsonl            # side by side, with differences
    python scripts/refcheck/summarize.py a.jsonl --size small --map pangaea # only those games
    python scripts/refcheck/summarize.py a.jsonl b.jsonl --json             # the same numbers as JSON

A comparison is a sanity check, not a gate: two engines that play differently will not give identical numbers.
Each row shows both means, the difference, and the difference in pooled standard deviations (d); rows where
|d| >= 0.2 are marked * and |d| >= 0.5 **, which is where to look for a porting mistake or an explanation.
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from collections import Counter
from pathlib import Path

METRICS = ("cities", "population", "techs", "score", "military", "wars_declared", "cities_captured")


def load(path: Path, size: str = None, map_type: str = None) -> tuple[list[dict], int]:
    """The finished games in a baseline file (optionally one size or map type), and how many crashed."""
    games, crashed = [], 0
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        r = json.loads(line)
        if (size and r.get("size") != size) or (map_type and r.get("map_type") != map_type):
            continue
        if r.get("crash"):
            crashed += 1
        else:
            games.append(r)
    return games, crashed


def percentile(xs: list, q: float) -> float:
    """The q-th quantile (0..1) of sorted values, interpolating between neighbours."""
    if not xs:
        return float("nan")
    pos = (len(xs) - 1) * q
    lo, hi = math.floor(pos), math.ceil(pos)
    return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo)


def describe(values: list) -> dict:
    """n, mean, standard deviation and percentiles of some numbers."""
    xs = sorted(float(v) for v in values if v is not None)
    n = len(xs)
    if not n:
        return {"n": 0}
    mean = sum(xs) / n
    sd = math.sqrt(sum((x - mean) ** 2 for x in xs) / (n - 1)) if n > 1 else 0.0
    return {"n": n, "mean": mean, "sd": sd, "min": xs[0], "p10": percentile(xs, .1), "p25": percentile(xs, .25),
            "p50": percentile(xs, .5), "p75": percentile(xs, .75), "p90": percentile(xs, .9), "max": xs[-1]}


def points(games: list[dict]) -> list[str]:
    """The checkpoints the games recorded (turns 100, 200, 300 unless the run chose others), then "end"."""
    turns = {k for g in games for c in g["civs"] for k in c["at"] if k != "end"}
    return sorted(turns, key=int) + ["end"]


def summarize(games: list[dict], crashed: int) -> dict:
    """Every distribution the comparison uses, from one baseline's games."""
    out = {"games": len(games), "crashed": crashed,
           "bot_errors": sum(g.get("bot_errors", 0) for g in games),
           "sizes": dict(Counter(g["size"] for g in games)), "maps": dict(Counter(g["map_type"] for g in games)),
           "seconds": describe([g.get("seconds") for g in games]),
           "turns": describe([g["turns"] for g in games]),
           "victory": dict(Counter(g.get("victory") or "none" for g in games).most_common()),
           "per_game": {"wars_declared": describe([sum(c["at"].get("end", {}).get("wars_declared", 0)
                                                       for c in g["civs"]) for g in games]),
                        "cities_captured": describe([sum(c["at"].get("end", {}).get("cities_captured", 0)
                                                         for c in g["civs"]) for g in games]),
                        "eliminated": describe([sum(1 for c in g["civs"] if not c["alive"]) for g in games])},
           "points": {}}
    for pt in points(games):
        rows = [c["at"][pt] for g in games for c in g["civs"] if pt in c["at"]]
        alive = [r for r in rows if r.get("alive")]
        out["points"][pt] = {"civs": len(rows), "alive": len(alive),
                             **{m: describe([r.get(m) for r in alive]) for m in METRICS}}
    return out


def _f(x, width: int = 8, nd: int = 1) -> str:
    """A number for a table cell."""
    if x is None or (isinstance(x, float) and math.isnan(x)):
        return "-".rjust(width)
    return f"{x:{width}.{nd}f}"


def print_one(s: dict, title: str):
    """The tables for one baseline."""
    print(f"== {title}")
    print(f"games {s['games']}, crashed {s['crashed']}, bot errors {s['bot_errors']}; sizes {s['sizes']}; "
          f"maps {s['maps']}; seconds per game {_f(s['seconds'].get('mean'), 0)}")
    print("victory: " + ", ".join(f"{k} {v} ({100 * v / max(1, s['games']):.0f}%)"
                                  for k, v in s["victory"].items()))
    head = f"{'':28}{'n':>6}{'mean':>9}{'sd':>9}{'p10':>9}{'p25':>9}{'p50':>9}{'p75':>9}{'p90':>9}"
    print(head)

    def row(label, d):
        """One distribution as a table row."""
        if not d.get("n"):
            print(f"{label:28}{0:>6}")
            return
        print(f"{label:28}{d['n']:>6}" + "".join(_f(d[k], 9) for k in ("mean", "sd", "p10", "p25", "p50", "p75",
                                                                          "p90")))
    row("game length (turns)", s["turns"])
    for k, d in s["per_game"].items():
        row(f"per game: {k}", d)
    for pt, d in s["points"].items():
        print(f"-- turn {pt}: {d['alive']} of {d['civs']} civilizations alive")
        for m in METRICS:
            row(f"  {m}", d[m])


def _effect(a: dict, b: dict):
    """Difference of means in pooled standard deviations, or None when it is undefined."""
    if not a.get("n") or not b.get("n"):
        return None
    pooled = math.sqrt(((a["n"] - 1) * a["sd"] ** 2 + (b["n"] - 1) * b["sd"] ** 2) / max(1, a["n"] + b["n"] - 2))
    return (b["mean"] - a["mean"]) / pooled if pooled else (0.0 if a["mean"] == b["mean"] else None)


def compare(sa: dict, sb: dict) -> list[dict]:
    """Every compared distribution: both means, the difference and the effect size."""
    rows = []

    def add(label, a, b):
        """One compared distribution."""
        d = _effect(a, b)
        rows.append({"what": label, "n_a": a.get("n", 0), "n_b": b.get("n", 0), "mean_a": a.get("mean"),
                     "mean_b": b.get("mean"), "diff": (b["mean"] - a["mean"]) if a.get("n") and b.get("n") else None,
                     "d": d, "flag": "" if d is None or abs(d) < 0.2 else ("*" if abs(d) < 0.5 else "**")})
    add("game length (turns)", sa["turns"], sb["turns"])
    for k in sa["per_game"]:
        add(f"per game: {k}", sa["per_game"][k], sb["per_game"][k])
    shared = [pt for pt in sa["points"] if pt in sb["points"]]
    for pt in shared:
        for m in METRICS:
            add(f"turn {pt}: {m}", sa["points"][pt][m], sb["points"][pt][m])
    return rows


def print_compare(sa: dict, sb: dict, rows: list[dict], a: str, b: str):
    """The comparison table."""
    print(f"== {a} (A) vs {b} (B)")
    print(f"games A {sa['games']} (crashed {sa['crashed']}), B {sb['games']} (crashed {sb['crashed']})")
    kinds = sorted(set(sa["victory"]) | set(sb["victory"]))
    print("victory share:  " + ", ".join(f"{k} {100 * sa['victory'].get(k, 0) / max(1, sa['games']):.0f}% -> "
                                         f"{100 * sb['victory'].get(k, 0) / max(1, sb['games']):.0f}%" for k in kinds))
    print(f"{'':30}{'n A':>6}{'n B':>6}{'mean A':>10}{'mean B':>10}{'B - A':>10}{'d':>8}")
    for r in rows:
        print(f"{r['what']:30}{r['n_a']:>6}{r['n_b']:>6}{_f(r['mean_a'], 10, 2)}{_f(r['mean_b'], 10, 2)}"
              f"{_f(r['diff'], 10, 2)}{_f(r['d'], 8, 2)} {r['flag']}")


def main():
    """The command line."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("files", nargs="+", type=Path, help="one baseline file, or two to compare")
    ap.add_argument("--size", default=None, help="only games on this map size")
    ap.add_argument("--map", default=None, help="only games on this map type")
    ap.add_argument("--json", action="store_true", help="print the numbers as JSON instead of tables")
    a = ap.parse_args()
    if len(a.files) > 2:
        ap.error("give one file, or two to compare")
    sums = [summarize(*load(p, a.size, a.map)) for p in a.files]
    if len(sums) == 1:
        if a.json:
            print(json.dumps(sums[0], indent=1))
        else:
            print_one(sums[0], str(a.files[0]))
        return 0
    rows = compare(*sums)
    if a.json:
        print(json.dumps({"a": sums[0], "b": sums[1], "compare": rows}, indent=1))
    else:
        for s, p in zip(sums, a.files):
            print_one(s, str(p))
            print()
        print_compare(*sums, rows, str(a.files[0]), str(a.files[1]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
