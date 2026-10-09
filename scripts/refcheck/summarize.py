"""Distribution tables for baseline files, the comparison of two, and the statistical gates G1-G4.

    python scripts/refcheck/summarize.py refcheck/baseline/python/small.jsonl        # one run's tables
    python scripts/refcheck/summarize.py python.jsonl rust.jsonl                     # compared, stratum by stratum
    python scripts/refcheck/summarize.py python.jsonl rust.jsonl --gate refcheck/baseline/explained.toml \
        --min-games 120 --checked rust-checks.jsonl                                  # the gates G1-G4
    python scripts/refcheck/summarize.py rust.jsonl --split-half                     # the run's noise against itself
    python scripts/refcheck/summarize.py a.jsonl b.jsonl --first 60                  # only games 0-59 of each
    python scripts/refcheck/summarize.py a.jsonl --size small --map pangaea          # only those games
    python scripts/refcheck/summarize.py a.jsonl b.jsonl --json                      # the numbers, per game too

The method (crates/citar-engine/DESIGN.md P2.4.4). The unit is the game: the civilizations of one game are
correlated, so each metric is one number per game, the mean over the majors alive at the checkpoint for a state
metric (cities, population, techs, score, military, policies, land, era) and the game's total for an event metric
(wars declared, cities captured). Map sizes are strata, compared separately. For each metric, checkpoint and
stratum a comparison gives the ratio of the means (B / A), the difference B - A with a bootstrap 90% interval
(2,000 resamples of each run's games, seeded by the cell, so every run of this script prints the same numbers),
and d, the difference in pooled per-game standard deviations, for reference. Rates per stratum: the share of
games with a war declared, with a city captured, ending before the turn limit, by victory type, and with a major
eliminated.

The gates (--gate FILE, A the Python run and B the Rust one; exit 1 when one fails):
  G1  B has no crashed, poisoned or timed-out game (a crash line), and every map size of A has at least
      --min-games finished games in B (by default as many as A has there), so a run cut short or missing a
      stratum cannot pass by comparing less. Each --checked run (games played with `citar-sim baseline
      --checks`, where a broken invariant is a crash line) has finished games, no crash line, and every line
      says "checks": true.
  G2  cities, population, techs and score at turns 100, 200 and 300: B's mean within [0.75, 1.33] of A's on small
      maps ([0.67, 1.5] on any other), and B's means rising from 100 to 200 to 300.
  G3  every metric at every checkpoint: a gap of 10% or more (15% off small maps) whose interval excludes 0 needs
      an entry in FILE; an event metric only where A's mean is at least 1 a game.
  G4  every rate within 25 percentage points of A's (35 off small maps), or an entry in FILE.
FILE holds [[gap]] entries: metric, checkpoint ("100", "200", "300" or "end"; "game" for a rate), stratum and
reason; optionally the band the explained gap was measured in (ratio = [lo, hi] for a metric, B's mean over A's;
points = [lo, hi] for a rate, B's share minus A's), outside which an entry no longer answers its gap ("the
explained gap moved"); and optionally pending (the follow-up package that owes the fix). An entry whose gap no
longer meets its rule is a warning, not a failure, so that a later run cannot fail on noise. --no-pending fails
on every entry marked pending, whether its gap is still material, under its rule or on a map size not compared
(package 2-12's precondition: no fix may still be owed).

Pure Python, standard library only: it stays after the Python engine is removed.
"""
from __future__ import annotations

import argparse
import json
import math
import random
import sys
import tomllib
from collections import Counter
from pathlib import Path

STATE_METRICS = ("cities", "population", "techs", "score", "military", "policies", "land", "era")
EVENT_METRICS = ("wars_declared", "cities_captured")
METRICS = STATE_METRICS + EVENT_METRICS
# G2's metrics and checkpoints: what a gross porting mistake moves first (cities that never grow).
GROSS = ("cities", "population", "techs", "score")
GROSS_POINTS = ("100", "200", "300")
# what identifies one game in a file (baseline.IDENTITY)
IDENTITY = ("i", "seed", "size", "map_type", "barbarians", "speed", "turn_limit")
RESAMPLES = 2000
LEVEL = 0.90
# The gates' bounds by stratum (DESIGN.md P2.4.4): small maps, and every other size.
RULES = {"small": {"ratio": (0.75, 1.33), "gap": 0.10, "rate_points": 25.0}}
OTHER_RULES = {"ratio": (0.67, 1.5), "gap": 0.15, "rate_points": 35.0}
# A rate's checkpoint in explained.toml.
RATE_POINT = "game"
GAP_KEYS = {"metric", "checkpoint", "stratum", "reason", "pending", "ratio", "points"}


def rules_for(stratum: str) -> dict:
    """The gates' bounds for a stratum."""
    return RULES.get(stratum, OTHER_RULES)


# ---- Reading ------------------------------------------------------------------------------------------------------

def read_lines(path: Path) -> list[dict]:
    """Every game line of a baseline file, in order. A line that is not JSON (a run stopped mid-write) is skipped
    with a warning."""
    out = []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            r = None
        if not isinstance(r, dict):
            print(f"{path}:{n}: skipped a line that is not a game (a run stopped mid-write?)", file=sys.stderr)
            continue
        out.append(r)
    return out


def keep(r: dict, size: str = None, map_type: str = None, first: int = None) -> bool:
    """Whether a line is among the games asked for."""
    if (size and r.get("size") != size) or (map_type and r.get("map_type") != map_type):
        return False
    return first is None or (isinstance(r.get("i"), int) and r["i"] < first)


def load(path: Path, size: str = None, map_type: str = None, first: int = None) -> tuple[list[dict], list[dict]]:
    """The finished games in a baseline file (optionally one size, one map type, or games 0..first-1), and every
    crash line among them.

    A resumed run can hold one game on several lines (a crash, then the replay that finished it), so each game
    counts once among the finished: its last finished line. Every crash line is returned, replayed or not: G1
    counts a crash even when a later run finished the game.
    """
    return split(read_lines(path), size, map_type, first)


def split(lines: list[dict], size: str = None, map_type: str = None,
          first: int = None) -> tuple[list[dict], list[dict]]:
    """`load`'s finished games and crash lines, from lines already read."""
    finished, crashes = {}, []
    for r in lines:
        if not keep(r, size, map_type, first):
            continue
        if r.get("crash"):
            crashes.append(r)
        else:
            finished[tuple(r.get(k) for k in IDENTITY)] = r
    return sorted(finished.values(), key=lambda g: (g.get("i", 0), g.get("seed", 0))), crashes


def load_checked(path: Path) -> dict:
    """A run said to be played with the engine's invariants on (`--checked`): its finished games, its crash lines,
    and every line that does not say `"checks": true`. citar-sim writes that key on each line of a `--checks`
    run; without it a crash-free run played with the invariants off would pass for a checked one, since its
    lines are otherwise the same."""
    lines = read_lines(path)
    games, crashes = split(lines)
    return {"path": str(path), "games": games, "crashes": crashes,
            "unflagged": [r for r in lines if r.get("checks") is not True]}


# ---- Per-game values ----------------------------------------------------------------------------------------------

def points(games: list[dict]) -> list[str]:
    """The checkpoints the games recorded (turns 100, 200, 300 unless the run chose others), then "end"."""
    turns = {k for g in games for c in g["civs"] for k in c["at"] if k != "end"}
    return sorted(turns, key=int) + ["end"]


def game_value(g: dict, metric: str, point: str):
    """One game's number for a metric at a checkpoint: the mean over the majors alive there for a state metric,
    the total over every major for an event metric; None when the game never reached the checkpoint."""
    rows = [c["at"][point] for c in g["civs"] if point in c["at"]]
    if not rows:
        return None
    if metric in EVENT_METRICS:
        return float(sum(r.get(metric, 0) for r in rows))
    alive = [r[metric] for r in rows if r.get("alive") and r.get(metric) is not None]
    return sum(alive) / len(alive) if alive else None


def per_game(games: list[dict], metric: str, point: str) -> list[float]:
    """Every game's number for a metric at a checkpoint, leaving out the games without one."""
    return [v for v in (game_value(g, metric, point) for g in games) if v is not None]


def rates(games: list[dict]) -> dict[str, list[bool]]:
    """Per rate, whether each game had it: a war declared, a city captured, an end before the turn limit (any
    victory but Time), each victory type, and a major eliminated."""
    def total(g, key):
        return sum(c["at"].get("end", {}).get(key, 0) for c in g["civs"])
    out = {"war": [total(g, "wars_declared") > 0 for g in games],
           "capture": [total(g, "cities_captured") > 0 for g in games],
           "early_end": [(g.get("victory") or "Time") != "Time" for g in games],
           "elimination": [any(not c["alive"] for c in g["civs"]) for g in games]}
    for v in sorted({g.get("victory") or "none" for g in games}):
        out[f"victory_{v}"] = [(g.get("victory") or "none") == v for g in games]
    return out


# ---- Statistics ---------------------------------------------------------------------------------------------------

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


def mean(xs: list) -> float:
    """The mean, or nan for nothing."""
    return sum(xs) / len(xs) if xs else float("nan")


def effect(a: list, b: list):
    """The difference of means in pooled standard deviations (d), or None when it is undefined."""
    da, db = describe(a), describe(b)
    if da["n"] < 1 or db["n"] < 1 or da["n"] + db["n"] < 3:
        return None
    pooled = math.sqrt(((da["n"] - 1) * da["sd"] ** 2 + (db["n"] - 1) * db["sd"] ** 2) / (da["n"] + db["n"] - 2))
    if pooled:
        return (db["mean"] - da["mean"]) / pooled
    return 0.0 if da["mean"] == db["mean"] else None


def bootstrap(a: list, b: list, key: str) -> tuple[float, float]:
    """The bootstrap 90% interval of mean(b) - mean(a): RESAMPLES resamples of each sample's games with
    replacement, the 5th and 95th percentiles of the differences. The stream is seeded by `key` (the cell), so the
    interval does not move when cells are added or reordered."""
    if not a or not b:
        return float("nan"), float("nan")
    rng = random.Random(f"summarize:{key}")
    na, nb = len(a), len(b)
    diffs = sorted(sum(rng.choices(b, k=nb)) / nb - sum(rng.choices(a, k=na)) / na for _ in range(RESAMPLES))
    tail = (1 - LEVEL) / 2
    return percentile(diffs, tail), percentile(diffs, 1 - tail)


def excludes_zero(lo: float, hi: float) -> bool:
    """Whether an interval lies wholly on one side of 0."""
    return lo > 0 or hi < 0


# ---- One run ------------------------------------------------------------------------------------------------------

def summarize(games: list[dict], crashes: list[dict]) -> dict:
    """Every distribution of one run's games (all strata together)."""
    out = {"games": len(games), "crashed": len(crashes),
           "bot_errors": sum(g.get("bot_errors", 0) for g in games),
           "sizes": dict(Counter(g["size"] for g in games)), "maps": dict(Counter(g["map_type"] for g in games)),
           "seconds": describe([g.get("seconds") for g in games]),
           "turns": describe([g["turns"] for g in games]),
           "victory": dict(Counter(g.get("victory") or "none" for g in games).most_common()),
           "rates": {k: sum(v) / len(v) if v else None for k, v in rates(games).items()},
           "points": {}}
    for pt in points(games):
        rows = [c["at"][pt] for g in games for c in g["civs"] if pt in c["at"]]
        out["points"][pt] = {"civs": len(rows), "alive": sum(1 for r in rows if r.get("alive")),
                             "per_game": {m: describe(per_game(games, m, pt)) for m in METRICS}}
    return out


def _f(x, width: int = 8, nd: int = 1) -> str:
    """A number for a table cell."""
    if x is None or (isinstance(x, float) and math.isnan(x)):
        return "-".rjust(width)
    return f"{x:{width}.{nd}f}"


def print_one(s: dict, title: str):
    """The tables for one run: per game, so the means are of games, not of civilizations."""
    print(f"== {title}")
    print(f"games {s['games']}, crashed {s['crashed']}, bot errors {s['bot_errors']}; sizes {s['sizes']}; "
          f"maps {s['maps']}; seconds per game {_f(s['seconds'].get('mean'), 0)}")
    print("victory: " + ", ".join(f"{k} {v} ({100 * v / max(1, s['games']):.0f}%)"
                                  for k, v in s["victory"].items()))
    print("rates: " + ", ".join(f"{k} {100 * v:.0f}%" for k, v in s["rates"].items() if v is not None))
    print(f"{'':28}{'n':>6}{'mean':>9}{'sd':>9}{'p10':>9}{'p25':>9}{'p50':>9}{'p75':>9}{'p90':>9}")

    def row(label, d):
        """One distribution as a table row."""
        if not d.get("n"):
            print(f"{label:28}{0:>6}")
            return
        print(f"{label:28}{d['n']:>6}" + "".join(_f(d[k], 9) for k in ("mean", "sd", "p10", "p25", "p50", "p75",
                                                                          "p90")))
    row("game length (turns)", s["turns"])
    for pt, d in s["points"].items():
        print(f"-- turn {pt}: {d['alive']} of {d['civs']} civilizations alive; per game:")
        for m in METRICS:
            row(f"  {m}", d["per_game"][m])


# ---- Two runs -----------------------------------------------------------------------------------------------------

def strata(games: list[dict]) -> dict[str, list[dict]]:
    """The games by map size, in the order the sizes first appear."""
    out = {}
    for g in games:
        out.setdefault(g["size"], []).append(g)
    return out


def compare_stratum(stratum: str, a: list[dict], b: list[dict], tag: str = "") -> dict:
    """Every cell of one stratum: per metric and checkpoint the per-game means, their ratio, the difference with
    its interval and d; and per rate both shares and the difference in percentage points."""
    cells = []
    pts = [p for p in points(a) if p in points(b)]
    for pt in pts:
        for m in METRICS:
            va, vb = per_game(a, m, pt), per_game(b, m, pt)
            ma, mb = mean(va), mean(vb)
            lo, hi = bootstrap(va, vb, f"{tag}{stratum}:{m}:{pt}")
            ratio = mb / ma if va and vb and ma else None
            cells.append({"metric": m, "checkpoint": pt, "kind": "event" if m in EVENT_METRICS else "state",
                          "n_a": len(va), "n_b": len(vb), "mean_a": ma, "mean_b": mb, "ratio": ratio,
                          "diff": mb - ma if va and vb else None, "lo": lo, "hi": hi, "d": effect(va, vb),
                          "values_a": va, "values_b": vb})
    ra, rb = rates(a), rates(b)
    rate_rows = []
    for k in sorted(set(ra) | set(rb), key=lambda k: (k.startswith("victory_"), k)):
        sa = sum(ra.get(k, [])) / len(a) if a else None
        sb = sum(rb.get(k, [])) / len(b) if b else None
        rate_rows.append({"rate": k, "share_a": sa, "share_b": sb,
                          "points": 100 * (sb - sa) if sa is not None and sb is not None else None})
    return {"stratum": stratum, "games_a": len(a), "games_b": len(b), "cells": cells, "rates": rate_rows}


def material(cell: dict, stratum: str) -> bool:
    """G3's rule: a gap of the stratum's size or more, whose interval excludes 0; an event metric only where A's
    mean is at least 1 a game."""
    if cell["ratio"] is None:
        return False
    if cell["kind"] == "event" and not cell["mean_a"] >= 1:
        return False
    return abs(cell["ratio"] - 1) >= rules_for(stratum)["gap"] and excludes_zero(cell["lo"], cell["hi"])


def rate_off(row: dict, stratum: str) -> bool:
    """G4's rule: a rate more than the stratum's bound in percentage points from A's."""
    return row["points"] is not None and abs(row["points"]) > rules_for(stratum)["rate_points"]


def split_half(games: list[dict]) -> tuple[list[dict], list[dict]]:
    """A run's games in two halves, by the parity of their rank in game order: each half keeps the rotation of map
    types, which advances with the game index."""
    ordered = sorted(games, key=lambda g: (g.get("i", 0), g.get("seed", 0)))
    return ordered[0::2], ordered[1::2]


def print_compare(c: dict, a: str, b: str, explained: dict = None):
    """The comparison table of one stratum, with each cell's flag: G3 for a material gap, E when explained."""
    explained = explained or {}
    s = c["stratum"]
    r = rules_for(s)
    print(f"== {s}: {a} (A, {c['games_a']} games) against {b} (B, {c['games_b']} games)")
    print(f"   a gap is material at {100 * r['gap']:.0f}% with its 90% interval clear of 0 (G3); "
          f"G2 bounds {r['ratio'][0]}-{r['ratio'][1]}x")
    print(f"{'':16}{'cp':>5}{'n A':>5}{'n B':>5}{'mean A':>10}{'mean B':>10}{'ratio':>7}{'B - A':>9}"
          f"{'90% interval':>21}{'d':>7}")
    for x in c["cells"]:
        flag = ""
        if material(x, s):
            flag = "E" if (x["metric"], x["checkpoint"], s) in explained else "G3"
        interval = f"[{_f(x['lo'], 0, 2)}, {_f(x['hi'], 0, 2)}]"
        print(f"{x['metric']:16}{x['checkpoint']:>5}{x['n_a']:>5}{x['n_b']:>5}{_f(x['mean_a'], 10, 2)}"
              f"{_f(x['mean_b'], 10, 2)}{_f(x['ratio'], 7, 2)}{_f(x['diff'], 9, 2)}{interval:>21}"
              f"{_f(x['d'], 7, 2)} {flag}")
    print(f"-- rates (G4 within {r['rate_points']:.0f} points)")
    for x in c["rates"]:
        flag = ""
        if rate_off(x, s):
            flag = "E" if (f"rate:{x['rate']}", RATE_POINT, s) in explained else "G4"
        print(f"   {x['rate']:24}{_f(100 * x['share_a'] if x['share_a'] is not None else None, 6, 0)}% ->"
              f"{_f(100 * x['share_b'] if x['share_b'] is not None else None, 5, 0)}%"
              f"{_f(x['points'], 8, 0)} points {flag}")


def noise_note(c: dict) -> str:
    """How many cells of a split-half comparison would need an entry: the false alarms G3 raises on noise."""
    n = sum(1 for x in c["cells"] if material(x, c["stratum"]))
    worst = max((abs(x["ratio"] - 1) for x in c["cells"] if x["ratio"] is not None), default=float("nan"))
    rate = max((abs(x["points"]) for x in c["rates"] if x["points"] is not None), default=float("nan"))
    return (f"{c['stratum']}: {n} of {len(c['cells'])} cells material by G3's rule; the largest gap "
            f"{100 * worst:.1f}%, the largest rate gap {rate:.0f} points")


# ---- The gate -----------------------------------------------------------------------------------------------------

class GapFileError(Exception):
    """An explained.toml that cannot be used."""


def read_explained(path: Path) -> list[dict]:
    """The [[gap]] entries of an explained.toml, each checked for its keys and values."""
    try:
        doc = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise GapFileError(f"{path}: {e}") from e
    if set(doc) - {"gap"}:
        raise GapFileError(f"{path}: unknown tables {sorted(set(doc) - {'gap'})}; only [[gap]] entries")
    out, seen = [], set()
    for n, e in enumerate(doc.get("gap", []), 1):
        where = f"{path}: gap {n}"
        if set(e) - GAP_KEYS:
            raise GapFileError(f"{where}: unknown keys {sorted(set(e) - GAP_KEYS)}")
        for k in ("metric", "checkpoint", "stratum", "reason"):
            if not isinstance(e.get(k), str) or not e[k].strip():
                raise GapFileError(f"{where}: `{k}` must be a non-empty string")
        if "pending" in e and (not isinstance(e["pending"], str) or not e["pending"].strip()):
            raise GapFileError(f"{where}: `pending` names the package that owes the fix")
        m, pt = e["metric"], e["checkpoint"]
        if m.startswith("rate:"):
            if pt != RATE_POINT:
                raise GapFileError(f"{where}: a rate's checkpoint is \"{RATE_POINT}\"")
            if "ratio" in e:
                raise GapFileError(f"{where}: a rate's band is `points` (B's share minus A's), not `ratio`")
            if "points" in e:
                band(e["points"], where, "points", lowest=-100.0, highest=100.0)
        elif m not in METRICS:
            raise GapFileError(f"{where}: no metric {m!r} (one of {', '.join(METRICS)}, or rate:<name>)")
        elif pt != "end" and not pt.isdigit():
            raise GapFileError(f"{where}: checkpoint {pt!r} is a turn or \"end\"")
        elif "points" in e:
            raise GapFileError(f"{where}: a metric's band is `ratio` (B's mean over A's), not `points`")
        elif "ratio" in e:
            band(e["ratio"], where, "ratio", lowest=0.0, positive=True)
        key = (m, pt, e["stratum"])
        if key in seen:
            raise GapFileError(f"{where}: {m} at {pt} on {e['stratum']} is explained twice")
        seen.add(key)
        out.append(e)
    return out


def band(v, where: str, key: str, lowest: float, highest: float = math.inf,
         positive: bool = False) -> tuple[float, float]:
    """An entry's band, `[lo, hi]`: two finite numbers with lo < hi, within [lowest, highest] (lo over 0 when
    `positive`: a ratio)."""
    ok = (isinstance(v, list) and len(v) == 2
          and all(isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x) for x in v))
    if not ok or not (lowest <= v[0] < v[1] <= highest) or (positive and v[0] <= 0):
        bounds = (f"over {lowest:g}" if positive else f"from {lowest:g}") + (
            f" to {highest:g}" if math.isfinite(highest) else "")
        raise GapFileError(f"{where}: `{key}` is [lo, hi], two numbers {bounds} with lo < hi, not {v!r}")
    return float(v[0]), float(v[1])


def gate(comparisons: list[dict], crashes: list[dict], checked: list[dict], entries: list[dict],
         no_pending: bool, games_a: dict[str, int], games_b: dict[str, int],
         min_games: int = None) -> tuple[list[str], list[str], list[str]]:
    """G1-G4 over the strata compared: the failures, the warnings and the notes (each a line).

    `crashes` are B's crash lines; `checked` the runs `load_checked` read; `games_a` and `games_b` each file's
    finished games by map size; `min_games` the least B must finish on each of A's sizes (None: A's own count
    there)."""
    fail, warn, note = [], [], []
    for r in crashes:
        fail.append(f"G1: game {r.get('i')} (seed {r.get('seed')}, {r.get('size')}/{r.get('map_type')}) crashed: "
                    f"{r.get('crash')}")
    # A stratum B lacks, or holds few games of, would otherwise pass by comparing less: a missing stratum is
    # left out of the comparison, and a run cut short (a killed process writes no crash line) widens every
    # interval until no gap is material.
    for s, na in games_a.items():
        nb = games_b.get(s, 0)
        least = na if min_games is None else min_games
        if not nb:
            fail.append(f"G1: {s}: B has no finished game on {s} maps (A has {na}), so {s} was not compared")
        elif nb < least:
            why = "--min-games" if min_games is not None else "as many as A has, without --min-games"
            fail.append(f"G1: {s}: B has {nb} finished games on {s} maps, fewer than {least} ({why}): "
                        f"a run cut short?")
    for s, nb in games_b.items():
        if s not in games_a:
            warn.append(f"B's {nb} games on {s} maps have none in A to be compared with")
    for c in checked:
        path, games, bad, unflagged = c["path"], c["games"], c["crashes"], c["unflagged"]
        sizes = ", ".join(f"{s} {n}" for s, n in Counter(g.get("size") for g in games).items()) or "none"
        if unflagged:
            note.append(f"G1: {path}: {len(games)} games finished ({sizes}), {len(bad)} crash lines, "
                        f"{len(unflagged)} lines not marked as played with the invariants on")
        else:
            note.append(f"G1: {path}: {len(games)} games finished with the invariants on ({sizes}), "
                        f"{len(bad)} crash lines")
        if not games:
            fail.append(f"G1: {path}: no finished game, so it shows nothing about the invariants")
        if unflagged:
            order = sorted(unflagged, key=lambda r: (0, r["i"], "") if isinstance(r.get("i"), int)
                           else (1, 0, str(r.get("i"))))
            ids = ", ".join(str(r.get("i")) for r in order[:10]) + (", ..." if len(order) > 10 else "")
            fail.append(f"G1: {path}: {len(unflagged)} lines do not say \"checks\": true (games {ids}), so they "
                        f"were not played with `citar-sim baseline --checks`")
        for r in bad:
            fail.append(f"G1: {path}: game {r.get('i')} (seed {r.get('seed')}) crashed with the invariants on: "
                        f"{r.get('crash')}")
    by_key = {(e["metric"], e["checkpoint"], e["stratum"]): e for e in entries}
    used = set()
    compared = {c["stratum"] for c in comparisons}
    for c in comparisons:
        s, r = c["stratum"], rules_for(c["stratum"])
        lo_b, hi_b = r["ratio"]
        cells = {(x["metric"], x["checkpoint"]): x for x in c["cells"]}
        for m in GROSS:
            means = []
            for pt in GROSS_POINTS:
                x = cells.get((m, pt))
                if x is None or x["ratio"] is None:
                    fail.append(f"G2: {s}: {m} at {pt} has no value on both sides")
                    continue
                means.append(x["mean_b"])
                if not lo_b <= x["ratio"] <= hi_b:
                    fail.append(f"G2: {s}: {m} at {pt}: B's mean {x['mean_b']:.2f} is {x['ratio']:.2f}x A's "
                                f"{x['mean_a']:.2f}, outside {lo_b}-{hi_b}x")
            if len(means) == len(GROSS_POINTS) and not all(p < q for p, q in zip(means, means[1:])):
                fail.append(f"G2: {s}: {m}: B's means do not rise from turn 100 to 200 to 300 "
                            f"({', '.join(f'{v:.2f}' for v in means)})")
        for x in c["cells"]:
            key = (x["metric"], x["checkpoint"], s)
            if material(x, s):
                what = (f"{s}: {x['metric']} at {x['checkpoint']}: {x['ratio']:.2f}x "
                        f"({x['mean_a']:.2f} -> {x['mean_b']:.2f}, interval [{x['lo']:.2f}, {x['hi']:.2f}])")
                if key in by_key:
                    used.add(key)
                    explain(by_key[key], "G3", what, x["ratio"], fail, note, no_pending)
                else:
                    fail.append(f"G3: {what}: unexplained")
        for x in c["rates"]:
            key = (f"rate:{x['rate']}", RATE_POINT, s)
            if rate_off(x, s):
                what = (f"{s}: rate {x['rate']}: {100 * x['share_a']:.0f}% -> {100 * x['share_b']:.0f}% "
                        f"({x['points']:+.0f} points)")
                if key in by_key:
                    used.add(key)
                    explain(by_key[key], "G4", what, x["points"], fail, note, no_pending)
                else:
                    fail.append(f"G4: {what}: outside {r['rate_points']:.0f} points, unexplained")
    for key, e in by_key.items():
        if key in used:
            continue
        cell = f"{e['metric']} at {e['checkpoint']} on {e['stratum']}"
        if e["stratum"] in compared:
            warn.append(f"stale: {cell} no longer meets its rule (\"{e['reason'][:60]}...\")")
        # 2-12's precondition is that no fix is still owed, so an entry marked pending is refused whatever its
        # gap does in this run: under its rule from noise, a short run or a partial fix, or on a map size this
        # command does not compare.
        if no_pending and e.get("pending"):
            why = "its gap is under its rule in this run" if e["stratum"] in compared else \
                f"{e['stratum']} is not compared by this command"
            fail.append(f"pending: {cell} is still owed by {e['pending']} ({why}), which --no-pending refuses; "
                        f"drop the entry, or its `pending` with the reason updated, once {e['pending']} is done")
    return fail, warn, note


def explain(e: dict, label: str, what: str, observed: float, fail: list, note: list, no_pending: bool):
    """An entry used: a note; a failure when the gap left the band the entry was measured in (then the entry
    answers another gap than its reason does), or with --no-pending when the fix is still owed."""
    key = "ratio" if "ratio" in e else "points" if "points" in e else None
    if key and not e[key][0] <= observed <= e[key][1]:
        unit = "x" if key == "ratio" else " points"
        fail.append(f"{label}: {what}: the explained gap moved: {observed:.2f}{unit} is outside the entry's "
                    f"{key} [{e[key][0]:g}, {e[key][1]:g}]")
    elif e.get("pending"):
        if no_pending:
            fail.append(f"pending: {what}: explained as owed by {e['pending']}, which --no-pending refuses")
        else:
            note.append(f"explained, PENDING {e['pending']}: {what}")
    else:
        note.append(f"explained: {what}")


# ---- The command line ---------------------------------------------------------------------------------------------

def strip_values(c: dict) -> dict:
    """A comparison without its per-game values, for --json without --per-game."""
    return {**c, "cells": [{k: v for k, v in x.items() if not k.startswith("values_")} for x in c["cells"]]}


def main(argv: list[str] = None) -> int:
    """The command line."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("files", nargs="+", type=Path, help="one baseline file, or two to compare (A, then B)")
    ap.add_argument("--size", default=None, help="only games on this map size")
    ap.add_argument("--map", default=None, help="only games on this map type")
    ap.add_argument("--first", type=int, default=None, help="only games 0..N-1 of each file")
    ap.add_argument("--split-half", action="store_true",
                    help="compare the last file's games with each other, half against half: the noise floor")
    ap.add_argument("--gate", type=Path, default=None, help="run G1-G4 against this explained.toml (two files)")
    ap.add_argument("--checked", type=Path, action="append", default=[],
                    help="a run played with the invariants on, whose crash lines fail G1 (with --gate)")
    ap.add_argument("--min-games", type=int, default=None, metavar="N",
                    help="with --gate, the least finished games B must have on each of A's map sizes (default: "
                         "as many as A has there)")
    ap.add_argument("--no-pending", action="store_true",
                    help="with --gate, fail on every entry marked pending (package 2-12's precondition)")
    ap.add_argument("--json", action="store_true", help="print the numbers as JSON instead of tables")
    ap.add_argument("--per-game", action="store_true", help="with --json, each cell's per-game values too")
    a = ap.parse_args(argv)
    if len(a.files) > 2:
        ap.error("give one file, or two to compare")
    if a.gate and len(a.files) != 2:
        ap.error("--gate compares two files: the Python run, then the Rust one")
    if (a.checked or a.no_pending or a.min_games is not None) and not a.gate:
        ap.error("--checked, --min-games and --no-pending go with --gate")
    if a.min_games is not None and a.min_games < 1:
        ap.error("--min-games is at least 1")
    entries = []
    if a.gate:
        try:
            entries = read_explained(a.gate)
        except GapFileError as e:
            print(f"summarize: {e}", file=sys.stderr)
            return 2
    loaded = [load(p, a.size, a.map, a.first) for p in a.files]
    out = {"files": [str(p) for p in a.files], "runs": [summarize(*x) for x in loaded]}

    halves = []
    if a.split_half:
        for s, games in strata(loaded[-1][0]).items():
            h1, h2 = split_half(games)
            halves.append(compare_stratum(s, h1, h2, tag="half:"))
        out["split_half"] = halves
    comps = []
    if len(loaded) == 2:
        sa, sb = strata(loaded[0][0]), strata(loaded[1][0])
        comps = [compare_stratum(s, sa[s], sb[s]) for s in sa if s in sb]
        out["compare"] = comps
        out["only_in_one"] = sorted(set(sa) ^ set(sb))
    result = 0
    if a.gate:
        checked = [load_checked(p) for p in a.checked]
        counts = [{s: len(g) for s, g in strata(x[0]).items()} for x in loaded]
        fail, warn, note = gate(comps, loaded[1][1], checked, entries, a.no_pending, counts[0], counts[1],
                                a.min_games)
        if not comps:
            fail.append("no stratum is in both files: nothing was compared")
        out["gate"] = {"failures": fail, "warnings": warn, "notes": note, "passed": not fail}
        result = 1 if fail else 0

    if a.json:
        if not a.per_game:
            out["compare"] = [strip_values(c) for c in out.get("compare", [])]
            if "split_half" in out:
                out["split_half"] = [strip_values(c) for c in out["split_half"]]
        print(json.dumps(out, indent=1))
        return result
    for s, p in zip(out["runs"], a.files):
        print_one(s, str(p))
        print()
    explained = {(e["metric"], e["checkpoint"], e["stratum"]) for e in entries}
    for c in comps:
        print_compare(c, str(a.files[0]), str(a.files[1]), explained)
        print()
    if comps and out["only_in_one"]:
        print(f"not compared (in one file only): {', '.join(out['only_in_one'])}\n")
    for c in halves:
        print_compare(c, f"{a.files[-1]} even half", "odd half")
        print(f"   noise: {noise_note(c)}\n")
    if a.gate:
        g = out["gate"]
        for line in g["notes"]:
            print(f"  {line}")
        for line in g["warnings"]:
            print(f"  warning: {line}")
        for line in g["failures"]:
            print(f"  FAIL {line}")
        strata_done = ", ".join(c["stratum"] for c in comps) or "none"
        print(f"gate: {'passed' if g['passed'] else 'FAILED'} on {strata_done} ({len(g['failures'])} failures, "
              f"{len(g['warnings'])} warnings, {sum(1 for n in g['notes'] if n.startswith('explained'))} explained)")
    return result


if __name__ == "__main__":
    sys.exit(main())
