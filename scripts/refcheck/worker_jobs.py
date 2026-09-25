"""Record the Python engine's worker job choices on the fixture states, for the Rust port's job maps
(crates/citar-engine DESIGN.md 6.11, package 1c-04 gate 4).

    PYTHONHASHSEED=0 python scripts/refcheck/worker_jobs.py            # writes crates/citar-testkit/data/worker_jobs.json
    PYTHONHASHSEED=0 python scripts/refcheck/worker_jobs.py --check    # re-records and compares with the committed file
    PYTHONHASHSEED=0 python scripts/refcheck/worker_jobs.py --corpus refcheck/corpus --out refcheck/corpus/worker_jobs.json

For each state (refcheck/fixtures-mini and fixtures-late, or a corpus folder), loaded as refcheck loads it and with
its visibility refreshed, the file holds under the state's name ``<case>/t<turn>``:

* ``workers``: every unit of a living major civilization that can build improvements, in id order, with the job
  ``automation.worker_jobs`` gives it with no tile claimed: ``[unit, [tile, improvement]]``, or ``[unit, null]``;
* ``tiles``: for the first such unit of each civilization and unit type, the best job ``automation._best_job_uncached``
  finds on every land tile of its civilization's cities but their centres: ``[owner, type, [[tile, improvement,
  value], ...]]`` with the value to six decimals, or ``[tile, null, null]``.

The Rust test (crates/citar-testkit/tests/engine/workers.rs) loads each state with ``Game::from_python`` and logs every
choice its job maps make differently, with the intended differences that explain them.
"""
from __future__ import annotations

import argparse
import gzip
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

FIXTURES = [ROOT / "refcheck" / "fixtures-mini", ROOT / "refcheck" / "fixtures-late"]
OUT = ROOT / "crates" / "citar-testkit" / "data" / "worker_jobs.json"


def states(folders):
    """Every fixture file under the folders, by case then turn."""
    out = []
    for folder in folders:
        for case in sorted(p for p in Path(folder).iterdir() if p.is_dir()):
            for f in sorted(case.glob("t*.json.gz"), key=lambda p: int(p.name[1:].split(".")[0])):
                out.append((f"{case.name}/{f.name.split('.')[0]}", f))
    return out


def record(path: Path) -> dict:
    """One state's worker choices and tile jobs."""
    from citar.engine import automation, cities, tiles as T, visibility
    from citar.engine import unique_types as U
    from citar.engine.game import Game
    from citar.engine.state import GameState
    from citar.engine.units import unit_uniques
    with gzip.open(path, "rt", encoding="utf-8") as fh:
        doc = json.load(fh)
    g = Game(GameState.from_dict(doc["state"]))
    visibility.refresh(g, force=True)
    workers, tiles, seen = [], [], set()
    for u in sorted(g.s.units.values(), key=lambda u: u.id):
        p = g.player(u.owner)
        if p.kind != "major" or not p.alive or not unit_uniques(g, u, U.BuildImprovements):
            continue
        job = automation.worker_jobs(g, u, set())
        workers.append([u.id, None if job is None else [job[0], job[1]]])
        if (u.owner, u.type) in seen:
            continue
        seen.add((u.owner, u.type))
        found = []
        for c in g.player_cities(u.owner):
            for idx in cities.city_tiles(g, c):
                if idx == c.idx or not T.is_land(g, idx):
                    continue
                best = automation._best_job_uncached(g, u, idx)
                found.append([idx, None, None] if best is None else [idx, best[0], round(best[1], 6)])
        found.sort(key=lambda row: row[0])
        tiles.append([u.owner, u.type, found])
    return {"workers": workers, "tiles": tiles}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="re-record and compare with the committed file")
    ap.add_argument("--corpus", help="a corpus folder to record instead of the committed fixtures")
    ap.add_argument("--out", help="where to write (the committed file by default)")
    a = ap.parse_args()
    folders = [Path(a.corpus)] if a.corpus else FIXTURES
    out = {name: record(path) for name, path in states(folders)}
    text = json.dumps(out, separators=(",", ":"), sort_keys=True) + "\n"
    target = Path(a.out) if a.out else OUT
    if a.check:
        old = target.read_text(encoding="utf-8") if target.exists() else ""
        if old != text:
            tmp = Path(tempfile.gettempdir()) / "worker_jobs.json"
            tmp.write_text(text, encoding="utf-8", newline="\n")
            print(f"{target} differs from a fresh recording ({tmp})")
            return 1
        print(f"{target}: unchanged ({len(out)} states)")
        return 0
    target.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {target}: {len(out)} states, {sum(len(v['workers']) for v in out.values())} workers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
