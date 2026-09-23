"""Record reference fixtures: game states from seeded all-bot games, with the Python engine's answers to
deterministic questions about each.

    python scripts/refcheck/record.py --quick              # a few small states -> refcheck/fixtures-mini/ (committed)
    python scripts/refcheck/record.py --full               # the corpus -> refcheck/corpus/ (git-ignored)
    python scripts/refcheck/record.py --full --only large  # cases whose name contains "large"
    python scripts/refcheck/record.py --quick --check      # re-record into a temporary folder and compare
    python scripts/refcheck/record.py --full --list        # the cases and their checkpoints, without playing

Each checkpoint is one file, <out>/<case>/t<turn>.json.gz:

    {"meta":    {"case", "engine", "bot", "seed", "config", "turn", "current", "checkpoints", "query_order",
                 "side_effects", "query_crashes", "bot_errors", "setup", "python", "hash_seed"},
     "state":   GameState.to_dict() of the game at the start of that round (the first living major's turn has
                begun), after a load and a visibility refresh,
     "queries": {group: {"fn": {...}, ...answers}}}

See refcheck/README.md for how the Rust port uses them, and scripts/refcheck/queries.py for the questions. The
script re-runs itself with PYTHONHASHSEED=0 so its output is the same bytes on every run; cases run in parallel
processes (--workers, default: all cores but four).
"""
from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
import traceback
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import common

CHECKPOINTS = (1, 25, 60, 120, 200, 280)
EARLY = (1, 25)
FULL_SIZES = ("duel", "small", "standard", "large")


def case(name: str, seed: int, size: str, map_type: str, barbarians: str, checkpoints, setup: str = None,
         players: int = 0) -> dict:
    """One recorded game: how to start it, where to take checkpoints, and an optional scenario setup that runs at
    the first checkpoint."""
    return {"name": name, "seed": seed, "checkpoints": list(checkpoints), "setup": setup,
            "config": common.game_config(seed, size, map_type, barbarians, players=players)}


def quick_cases() -> list[dict]:
    """A few small states that record in about fifteen seconds: the committed fixtures and the CI check."""
    return [case("duel-continents-normal", 11, "duel", "continents", "normal", (1, 20, 50)),
            case("small-pangaea-raging", 12, "small", "pangaea", "raging", (1, 12, 40)),
            case("scenario-duel-fractal", 13, "duel", "fractal", "off", (10, 11, 12), setup="world_war")]


def full_cases(seeds: int = 2) -> list[dict]:
    """The corpus: every size up to large on every map type, with barbarians off, normal and raging rotating so
    each size meets all three; early checkpoints on huge and gargantuan; and scenario states.

    Games are Quick speed (330 turns), so every checkpoint up to 280 is reached unless someone wins first.
    """
    out, i = [], 0
    for _ in range(seeds):
        for size in FULL_SIZES:
            for map_type in common.MAP_TYPES:
                barb = common.BARBARIANS[i % 3]
                seed = 1000 + i
                out.append(case(f"{size}-{map_type}-{barb}-s{seed}", seed, size, map_type, barb, CHECKPOINTS))
                i += 1
    out.append(case("huge-continents-normal-s2001", 2001, "huge", "continents", "normal", EARLY))
    out.append(case("gargantuan-pangaea-normal-s2002", 2002, "gargantuan", "pangaea", "normal", EARLY))
    out.append(case("scenario-small-continents-s3001", 3001, "small", "continents", "normal", (60, 61, 63),
                    setup="world_war"))
    out.append(case("scenario-standard-pangaea-s3002", 3002, "standard", "pangaea", "raging", (120, 121, 123),
                    setup="world_war"))
    return out


def _plain(obj):
    """Meta values as plain JSON, whatever the setup steps returned."""
    return json.loads(json.dumps(obj, default=str))


def record_case(c: dict, out_dir: str) -> dict:
    """Play one case and write its checkpoints. Runs in a worker process."""
    import queries
    import scenarios
    timer = common.Timer()
    out = Path(out_dir) / c["name"]
    written, errors, setup_log, timings = [], [], [], {}
    try:
        g = common.new_game(c["config"])
        bots = common.make_bots(g, c["seed"])
        todo = sorted(c["checkpoints"])
        eng, bot = common.engine_hash(), common.bot_hash()

        def on_round(g):
            """Run the setup at the first checkpoint, and record every checkpoint reached."""
            if not todo:
                return False
            if g.turn < todo[0]:
                return True
            if c["setup"] and not written:
                setup_log.extend(scenarios.SETUPS[c["setup"]](g))
            turn = g.turn
            while todo and todo[0] <= turn:
                todo.pop(0)
            text = common.settle(g)
            answers, side, secs = queries.answer_all(text, c["seed"], turn)
            crashed = [k for k, v in answers.items() if "fn" not in v]
            # no timings in the file: the same game must give the same bytes, so a re-record shows no diff
            meta = {"case": c["name"], "engine": eng, "bot": bot, "seed": c["seed"], "config": c["config"],
                    "turn": turn, "current": g.s.current, "checkpoints": c["checkpoints"],
                    "query_order": [name for name, _ in queries.GROUPS], "side_effects": side,
                    "query_crashes": crashed, "bot_errors": list(errors),
                    "setup": _plain(setup_log) if c["setup"] else None,
                    "python": sys.version.split()[0], "hash_seed": "0"}
            doc = '{"meta":' + common.dumps(meta) + ',"state":' + text + ',"queries":' + common.dumps(answers) + "}"
            common.write_gz(out / f"t{turn}.json.gz", doc)
            written.append(turn)
            timings[turn] = {"at": timer(), "queries": round(sum(secs.values()), 1),
                             "slowest": max(secs, key=secs.get), "crashed": crashed}
            return bool(todo)

        common.play(g, bots, on_round=on_round, errors=errors)
        return {"case": c["name"], "written": written, "missed": todo, "seconds": timer(), "bot_errors": len(errors),
                "timings": timings}
    except Exception as e:
        return {"case": c["name"], "written": written, "crash": f"{type(e).__name__}: {e}",
                "trace": traceback.format_exc(limit=8), "seconds": timer()}


def run(cases: list[dict], out_dir: Path, workers: int) -> list[dict]:
    """Record every case, in parallel, printing each as it finishes."""
    out_dir.mkdir(parents=True, exist_ok=True)
    results = []
    print(f"Recording {len(cases)} case(s) into {out_dir} with {workers} worker(s).", flush=True)
    with ProcessPoolExecutor(max_workers=workers) as ex:
        futs = {ex.submit(record_case, c, str(out_dir)): c for c in cases}
        for f in as_completed(futs):
            r = f.result()
            results.append(r)
            if r.get("crash"):
                print(f"  {r['case']}: CRASHED after {r['seconds']}s: {r['crash']}\n{r['trace']}", flush=True)
            else:
                missed = f", game ended before {r['missed']}" if r["missed"] else ""
                errs = f", {r['bot_errors']} bot error(s)" if r["bot_errors"] else ""
                q = ", ".join(f"t{t} {v['queries']}s ({v['slowest']})" for t, v in r["timings"].items())
                print(f"  {r['case']}: turns {r['written']} in {r['seconds']}s{missed}{errs}; queries {q}",
                      flush=True)
                for t, v in r["timings"].items():
                    if v["crashed"]:
                        print(f"    t{t}: the Python engine raised answering {', '.join(v['crashed'])} (the "
                              f"traceback is in the fixture)", flush=True)
    return sorted(results, key=lambda r: r["case"])


def _content(path: Path) -> dict:
    """A fixture without the fields that change with the code but not with its behaviour: the hashes of the
    engine and bot sources, and the Python version."""
    d = common.read_gz(path)
    for k in ("engine", "bot", "python"):
        d["meta"].pop(k, None)
    return d


def check(committed: Path, cases: list[dict], workers: int) -> int:
    """Re-record into a temporary folder and compare with the committed fixtures. Returns an exit code."""
    tmp = Path(tempfile.mkdtemp(prefix="refcheck-"))
    try:
        run(cases, tmp, workers)
        names = {c["name"] for c in cases}
        old = {p.relative_to(committed) for p in committed.rglob("*.json.gz") if p.parent.name in names}
        new = {p.relative_to(tmp) for p in tmp.rglob("*.json.gz")}
        bad = sorted(str(p) for p in old ^ new)
        for rel in sorted(old & new):
            a, b = _content(committed / rel), _content(tmp / rel)
            if a != b:
                parts = [k for k in ("state", "queries") if a[k] != b[k]]
                if a["queries"] != b["queries"]:
                    parts += [f"queries.{g}" for g in a["queries"] if a["queries"][g] != b["queries"].get(g)]
                bad.append(f"{rel}: {', '.join(parts) or 'meta'} differ")
        if bad:
            print("The Python engine no longer reproduces the committed fixtures:")
            for line in bad:
                print("  " + line)
            print("If the change is deliberate, re-record them: python scripts/refcheck/record.py --quick")
            return 1
        print(f"All {len(old)} fixtures reproduced.")
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    """The command line."""
    common.ensure_hash_seed()
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--quick", action="store_true", help="a few small states into refcheck/fixtures-mini/")
    mode.add_argument("--full", action="store_true", help="the corpus into refcheck/corpus/")
    ap.add_argument("--seeds", type=int, default=2, help="--full: games per size and map type (default 2)")
    ap.add_argument("--only", default="", help="only cases whose name contains this")
    ap.add_argument("--out", default=None, help="output folder (default by mode)")
    ap.add_argument("--workers", type=int, default=common.default_workers())
    ap.add_argument("--check", action="store_true", help="re-record and compare with what is there instead")
    ap.add_argument("--list", action="store_true", help="print the cases and exit")
    a = ap.parse_args()
    cases = quick_cases() if a.quick else full_cases(a.seeds)
    cases = [c for c in cases if a.only in c["name"]]
    out = Path(a.out) if a.out else common.REFCHECK / ("fixtures-mini" if a.quick else "corpus")
    if a.list:
        for c in cases:
            cfg = c["config"]
            print(f"{c['name']:40} {cfg['map_size']:10} {cfg['map_type']:12} barbarians {cfg['barbarians']:7} "
                  f"players {len(cfg['players']):2}  turns {c['checkpoints']}" + (f"  setup {c['setup']}"
                                                                                   if c["setup"] else ""))
        print(f"{len(cases)} cases, {sum(len(c['checkpoints']) for c in cases)} checkpoints.")
        return 0
    if not cases:
        print("No case matches.")
        return 1
    if a.check:
        return check(out, cases, a.workers)
    if a.quick and not a.only and out.exists():
        # the committed set is exactly what --quick makes, nothing left over. Files only: a synced folder (OneDrive)
        # can refuse to remove a directory it is watching, and an empty one left behind does no harm
        for p in out.rglob("*.json.gz"):
            p.unlink()
    timer = common.Timer()
    results = run(cases, out, a.workers)
    crashed = [r["case"] for r in results if r.get("crash")]
    files = list(out.rglob("*.json.gz"))
    size = sum(p.stat().st_size for p in files)
    print(f"{len(files)} files, {size / 1e6:.2f} MB, in {timer()}s." + (f" Crashed: {', '.join(crashed)}"
                                                                          if crashed else ""))
    return 1 if crashed else 0


if __name__ == "__main__":
    sys.exit(main())
