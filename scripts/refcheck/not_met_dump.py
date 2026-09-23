"""Record the Python engine's refusal texts for requirements whose conditionals fail, for the Rust port's
``Cond::describe`` and ``uq::requirement_problems`` (crates/citar-engine DESIGN.md 5.8, package 1a-07).

    PYTHONHASHSEED=0 python scripts/refcheck/not_met_dump.py            # writes crates/citar-testkit/data/not_met.json
    PYTHONHASHSEED=0 python scripts/refcheck/not_met_dump.py --check    # re-records and compares with the committed file

``cities._not_met`` (cities.py:1169-1193) turns each failing conditional of an ``Only available`` or ``Can only be
built`` unique into a reason: the two building-count conditionals name the civilization's own version of the building,
``Can only be built`` repeats its whole text, and anything else is ``Not available (<the conditional>)``. The file holds:

* ``forced``: every such unique of every building and unit (a unit's with its unit type's, as ``rejection_reasons``
  read them), with every conditional made to fail, so that each gives its text: ``[nation, kind, object, text,
  [[type, message], ...]]``. Each unique is recorded for the first major nation, and again for every other major
  nation whose texts differ (those whose own building replaces the one a count names).
* ``fixtures``: every such unique of every building and unit, for every city of the committed fixture states
  (refcheck/fixtures-mini and fixtures-late), evaluated as ``rejection_reasons`` evaluates it, in the city's own
  context: ``[case, city, nation, kind, object, text, [failing conditionals, by position among the unique's
  conditionals], [[type, message], ...]]``. A row is kept for the first city where its unique, failing conditionals
  and texts occur; the states repeat them many times over.

The Rust test gives each failing conditional's ``describe`` (or the whole text, for ``Can only be built``) for the
nation, and compares the list with the row. That the same conditionals fail in the Rust engine's evaluation of the
fixtures is for the packages that load them to check.
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

from citar.engine import cities, uniques
from citar.engine import unique_types as U
from citar.engine.rules import Rules

OUT = ROOT / "crates" / "citar-testkit" / "data" / "not_met.json"
FIXTURES = [ROOT / "refcheck" / "fixtures-mini", ROOT / "refcheck" / "fixtures-late"]
KINDS = {U.OnlyAvailable: False, U.CanOnlyBeBuiltWhen: True}


def requirements(r: Rules):
    """Every (kind, object, unique, built_variant) the rejection reasons read, in file order."""
    for name, b in r.buildings.items():
        for u in b["_umap"].all:
            if u.ph in KINDS:
                yield "Building", name, u, KINDS[u.ph]
    for name, d in r.units.items():
        for ph, variant in KINDS.items():
            for u in d["_umap"].get(ph):
                yield "Unit", name, u, variant


def conditionals(u):
    """The unique's modifiers that are conditionals, in its order: what Rust compiles to its conditionals."""
    return [m for m in u.mods if m.ph in uniques._COND]


class Stub:
    """Just enough of a game for ``_not_met``: the rules, and each seat's nation."""

    def __init__(self, r: Rules, nation: str):
        self.rules = r
        self._p = type("P", (), {"nation": nation})()

    def player(self, pid):
        return self._p


def forced(r: Rules) -> list:
    rows = []
    majors = [n for n, d in r.nations.items() if d.get("kind", "major") == "major"]
    saved = uniques._COND
    uniques._COND = {k: (lambda u, m, c: False) for k in saved}
    try:
        for kind, name, u, variant in requirements(r):
            if not u.mods:
                continue
            first = None
            for nation in majors:
                out = [list(x) for x in cities._not_met(Stub(r, nation), u, None, 0, variant)]
                if first is None or out != first:
                    rows.append([nation, kind, name, u.text, out])
                first = out if first is None else first
    finally:
        uniques._COND = saved
    return rows


def fixture_states():
    for folder in FIXTURES:
        for case in sorted(p for p in folder.iterdir() if p.is_dir()):
            for f in sorted(case.glob("t*.json.gz"), key=lambda p: int(p.name[1:].split(".")[0])):
                yield f"{case.name}/{f.name.split('.')[0]}", f


def fixtures(r: Rules) -> list:
    from scripts.refcheck.common import load_game
    rows = []
    seen = set()
    for label, path in fixture_states():
        with gzip.open(path, "rt", encoding="utf-8") as fh:
            g = load_game(json.dumps(json.load(fh)["state"]))
        for cid in sorted(g.s.cities):
            city = g.s.cities[cid]
            ctx = cities.city_ctx(g, city)
            pid = city.owner
            nation = g.player(pid).nation
            for kind, name, u, variant in requirements(g.rules):
                conds = conditionals(u)
                failing = [i for i, m in enumerate(conds) if not uniques._COND[m.ph](u, m, ctx)]
                if not failing:
                    continue
                out = [list(x) for x in cities._not_met(g, u, ctx, pid, variant)]
                key = (kind, name, u.text, tuple(failing), json.dumps(out))
                if key in seen:
                    continue
                seen.add(key)
                rows.append([label, cid, nation, kind, name, u.text, failing, out])
    return rows


def record() -> dict:
    r = Rules()
    return {"format": 1, "forced": forced(r), "fixtures": fixtures(r)}


def render(data: dict) -> str:
    """One row per line, so a changed ruleset or engine gives a readable diff."""
    lines = ["{", '"format": 1,']
    for key in ("forced", "fixtures"):
        rows = data[key]
        lines.append(f'"{key}": [')
        lines += [json.dumps(row, ensure_ascii=False) + ("," if i + 1 < len(rows) else "") for i, row in enumerate(rows)]
        lines.append("]," if key == "forced" else "]")
    lines.append("}")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    data = record()
    text = render(data)
    if json.loads(text) != data:
        raise SystemExit("not_met: the rendered file does not read back as what was recorded")
    if args.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print("not_met.json is up to date" if same else "not_met.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KB, {len(data['forced'])} forced rows, "
          f"{len(data['fixtures'])} fixture rows)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
