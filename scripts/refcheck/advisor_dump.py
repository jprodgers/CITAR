"""Record the Python engine's production advisor on the reference states, for the report of how often the Rust port's
advisor agrees with it (crates/citar-engine DESIGN.md 6.12, package 1c-07, gate 4, informational).

    PYTHONHASHSEED=0 python scripts/refcheck/advisor_dump.py
        # the committed states: writes crates/citar-testkit/data/advisor.json
    PYTHONHASHSEED=0 python scripts/refcheck/advisor_dump.py --fixtures refcheck/corpus --out refcheck/corpus/advisor.json
        # the local corpus (git-ignored, as the corpus is)

Each state is loaded afresh, and for every city of a living major civilization it records, by name (null for nothing):

* ``auto``: what ``cities.auto_pick_production`` (cities.py:1696-1717) would start, without starting it: for a puppet
  the building of most value in the classic valuation, or Gold; else ``BasicBot(aggression=0.25, seed=city.id)
  .advise_production``;
* ``puppet_pick``: the puppet's pick, whatever the city is;
* ``unciv`` and ``classic``: ``BasicBot(seed=city.id).advise_production`` at the live bot's defaults (aggression 0.4),
  and with ``prod_mode`` classic.

A refusal inside the advisor, which ``auto_pick_production`` swallowed, is recorded as null. Each question is asked
from cleared caches (see ``guarded``). The Rust test
(crates/citar-testkit/tests/engine/advisor.rs) asks its advisor the same and reports the share that agree. It is not a
gate: the ranged-or-melee draw, the order units are read in and ties differ on purpose (tests/rules/intended.toml),
and until package 1c-04 scores city sites the Rust advisor chooses no settler.
"""
from __future__ import annotations

import argparse
import gzip
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import common
from citar.bots.basic import BasicBot
from citar.engine import cities as cm

OUT = ROOT / "crates" / "citar-testkit" / "data" / "advisor.json"
FIXTURES = [ROOT / "refcheck" / "fixtures-mini", ROOT / "refcheck" / "fixtures-late"]


def states(folders):
    """Every ``<case>/t<turn>.json.gz`` of the folders, by case and turn."""
    out = []
    for folder in folders:
        for case in sorted(p for p in folder.iterdir() if p.is_dir()):
            for f in case.glob("t*.json.gz"):
                out.append((case.name, int(f.name[1:].split(".")[0]), f))
    return sorted(out, key=lambda x: (x[0], x[1]))


def puppet_pick(g, city):
    """The puppet's branch of ``auto_pick_production`` (cities.py:1701-1705)."""
    bot = BasicBot(aggression=0.25, seed=city.id)
    items = cm.buildable_items(g, city)
    ctx = bot.context(g, city.owner)
    scored = [(bot._building_value(g, city.owner, city, b, ctx), b) for b in items["buildings"]]
    scored = [x for x in scored if x[0] > 0]
    return max(scored)[1] if scored else ("Gold" if "Gold" in items.get("other", []) else None)


def advise(g, city, aggression=0.4, **params):
    """``BasicBot.advise_production`` for the city, from a bot seeded with its id."""
    return BasicBot(aggression=aggression, seed=city.id, params=params).advise_production(g, city.owner, city)


def guarded(f, g, *args, **kwargs):
    """``f(g, ...)`` from cleared caches, or None where it raised, as ``auto_pick_production`` swallowed it.

    ``BasicBot._simulate`` swaps ``g._cache`` and ``g._ycache`` while a building is added, but values computed with
    the building stay behind in the caches it does not swap, and change the next answer asked of the same game (a
    city of the corpus advised a Monument after its puppet's pick, a Stadium on a fresh load). So every question
    starts from ``g.clear_static()``, which gives the fresh load's answer.
    """
    g.clear_static()
    try:
        return f(g, *args, **kwargs)
    except Exception:
        return None


def record(path: Path) -> list:
    doc = json.loads(gzip.open(path, "rt", encoding="utf-8").read())
    g = common.load_game(json.dumps(doc["state"]))
    rows = []
    for city in list(g.s.cities.values()):
        owner = g.player(city.owner)
        if owner.kind != "major" or not owner.alive:
            continue
        pup = guarded(puppet_pick, g, city)
        auto = pup if city.puppet else guarded(advise, g, city, aggression=0.25)
        unciv = guarded(advise, g, city)
        classic = guarded(advise, g, city, prod_mode="classic")
        rows.append({"city": city.id, "owner": city.owner, "puppet": bool(city.puppet), "auto": auto,
                     "puppet_pick": pup, "unciv": unciv, "classic": classic})
    return rows


def main():
    common.ensure_hash_seed()
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fixtures", action="append", help="a fixture folder (default: the committed ones)")
    ap.add_argument("--out", help=f"where to write (default: {OUT.relative_to(ROOT)})")
    args = ap.parse_args()
    folders = [Path(f) if Path(f).is_absolute() else ROOT / f for f in args.fixtures] if args.fixtures else FIXTURES
    out = Path(args.out) if args.out else OUT
    rows = []
    for case, turn, path in states(folders):
        rows.append({"case": case, "turn": turn, "cities": record(path)})
        print(f"{case}/t{turn}: {len(rows[-1]['cities'])} cities", flush=True)
    doc = {
        "format": 1,
        "fn": {
            "auto": "cities.auto_pick_production's choice, not set",
            "puppet_pick": "cities.auto_pick_production's puppet branch, for any city",
            "unciv": "bots.basic.BasicBot(seed=city.id).advise_production",
            "classic": "bots.basic.BasicBot(seed=city.id, params={'prod_mode': 'classic'}).advise_production",
        },
        "states": rows,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {out}: {sum(len(s['cities']) for s in rows)} cities in {len(rows)} states")


if __name__ == "__main__":
    main()
