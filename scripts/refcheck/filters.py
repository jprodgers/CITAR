"""Record the Python engine's truth tables of the ruleset's filters over every static domain, for the Rust port's
filter tests (crates/citar-engine DESIGN.md 5.7, package 1a-06).

    PYTHONHASHSEED=0 python scripts/refcheck/filters.py            # writes crates/citar-testkit/data/filters.json
    PYTHONHASHSEED=0 python scripts/refcheck/filters.py --check    # re-records and compares with the committed file

The texts are every filter the ruleset writes, of every kind: each bracketed parameter of a unique or a modifier whose
kind in UnCiv's signature (unique_types.py) is a filter, the filters inside countables (``[filter] Units`` and the
like), the improvements' ``terrainsCanBeBuiltOn`` and the nations' start biases. Each text is evaluated in each of the
ten static domains, whatever its own kind, over every object of the domain:

* ``BaseUnit``: ``base_unit_matches`` (uniques.py:477-525);
* ``Building``: ``building_matches`` (uniques.py:613-646);
* ``Terrain``: ``terrain_matches`` (uniques.py:336-360);
* ``Improvement``: ``improvement_matches`` (uniques.py:363-370);
* ``Resource``: ``resource_matches`` (uniques.py:373-388);
* ``Tech``: ``tech_matches`` (uniques.py:607-610);
* ``Era``: ``era_matches`` (uniques.py:594-604) under ``multi_filter``, as ``tech_matches`` reaches it;
* ``Policy``: ``policy_matches`` (uniques.py:649-655), over the branches and then the policies;
* ``Promotion``: the promotion lines of ``_unit_single`` (uniques.py:552-556): the name, or a tag it carries;
* ``Nation``: the nation lines of ``civ_matches`` (uniques.py:587-589): the name, or a tag it carries.

Each row is ``[domain, text, members]``, the members named in the domain's file order, which is the Rust id order. The
Rust test compares every row with ``unique::filter::statics::members``, and every static filter the compiled ruleset
holds with its row.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from citar.engine.rules import Rules
from citar.engine.uniques import (base_unit_matches, building_matches, era_matches, improvement_matches, multi_filter,
                                  placeholder, policy_matches, resource_matches, split_modifiers, tech_matches,
                                  terrain_matches)

OUT = ROOT / "crates" / "citar-testkit" / "data" / "filters.json"

# The parameter kinds whose text is a filter; populationFilter names a count, not a set.
FILTER_KINDS = {"baseUnitFilter", "buildingFilter", "cityFilter", "civFilter", "combatantFilter", "eraFilter",
                "improvementFilter", "improvementFilter/terrainFilter", "mapUnitFilter", "resourceFilter",
                "simpleTerrain", "techFilter", "terrainFilter", "tileFilter", "tileFilter/buildingFilter",
                "tileFilter/specialist/buildingFilter"}
COUNTABLE_FILTERS = ("[] Units", "[] Cities", "Remaining [] Civilizations", "[] Buildings")


def signatures() -> dict[str, list[str]]:
    """unique_types.py: placeholder -> the parameter kinds of its signature."""
    out = {}
    for line in (ROOT / "citar" / "engine" / "unique_types.py").read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\w+ = ('.*?'|\".*?\")  # (.*)$", line)
        if m:
            out[eval(m.group(1))] = re.findall(r"\[([^\[\]]*)\]", m.group(2))
    return out


def unique_texts(r: Rules) -> list[str]:
    """Every unique text of the ruleset, its unit types' and global uniques' included."""
    tables = [r.nations, r.buildings, r.policy_branches, r.policies, r.techs, r.eras, r.beliefs, r.resources,
              r.terrains, r.improvements, r.unit_types, r.units, r.promotions, r.ruins]
    out = []
    for table in tables:
        for obj in table.values():
            out += obj.get("uniques") or []
    for cs in r.city_state_types.values():
        for field in ("friendBonusUniques", "allyBonusUniques", "uniques"):
            out += cs.get(field) or []
    gl = json.loads((ROOT / "citar" / "data" / "ruleset" / "global_uniques.json").read_text(encoding="utf-8"))
    return out + gl["uniques"]


def filter_texts(r: Rules) -> list[str]:
    """Every filter text of the ruleset, in the order first met."""
    sig = signatures()
    seen: dict[str, None] = {}
    for text in unique_texts(r):
        main, mods = split_modifiers(text)
        for part in [main] + mods:
            ph, params = placeholder(part)
            kinds = sig.get(ph, [])
            if len(kinds) != len(params):
                continue
            for kind, p in zip(kinds, params):
                if kind in FILTER_KINDS:
                    seen.setdefault(p)
                elif kind == "countable":
                    cph, cparams = placeholder(p)
                    if cph in COUNTABLE_FILTERS and len(cparams) == 1:
                        seen.setdefault(cparams[0])
    for imp in r.improvements.values():
        for f in imp.get("terrainsCanBeBuiltOn") or []:
            seen.setdefault(f)
    for nation in r.nations.values():
        for b in nation.get("startBias") or []:
            if b == "Coast":
                continue
            seen.setdefault(b[len("Avoid ["):-1] if b.startswith("Avoid [") and b.endswith("]") else b)
    return list(seen)


def domains(r: Rules) -> dict:
    """Each static domain: its objects in file order, and the Python predicate for one object."""
    def tagged(table):
        return lambda name, f: multi_filter(f, lambda s: s == name or table[name]["_umap"].has_tag(s))
    policies = {**r.policy_branches, **r.policies}
    return {
        "BaseUnit": (list(r.units), lambda n, f: base_unit_matches(r, n, f)),
        "Building": (list(r.buildings), lambda n, f: building_matches(r, n, f)),
        "Terrain": (list(r.terrains), lambda n, f: terrain_matches(r, n, f)),
        "Improvement": (list(r.improvements), lambda n, f: improvement_matches(r, n, f)),
        "Resource": (list(r.resources), lambda n, f: resource_matches(r, n, f)),
        "Tech": (list(r.techs), lambda n, f: tech_matches(r, n, f)),
        "Era": (list(r.eras), lambda n, f: multi_filter(f, lambda s: era_matches(r, n, s))),
        "Policy": (list(policies), lambda n, f: policy_matches(r, n, f)),
        "Promotion": (list(r.promotions), tagged(r.promotions)),
        "Nation": (list(r.nations), tagged(r.nations)),
    }


def record() -> dict:
    r = Rules()
    texts = filter_texts(r)
    rows = []
    objects = {}
    for domain, (names, matches) in domains(r).items():
        objects[domain] = names
        for text in texts:
            rows.append([domain, text, [n for n in names if matches(n, text)]])
    return {"format": 1, "objects": objects, "texts": texts, "rows": rows}


def render(data: dict) -> str:
    """One row per line, so a changed ruleset or engine gives a readable diff."""
    lines = ["{", '"format": 1,', '"objects": ' + json.dumps(data["objects"]) + ",",
             '"texts": ' + json.dumps(data["texts"]) + ",", '"rows": [']
    rows = data["rows"]
    lines += [json.dumps(row) + ("," if i + 1 < len(rows) else "") for i, row in enumerate(rows)]
    lines += ["]", "}"]
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    data = record()
    text = render(data)
    if json.loads(text) != data:
        raise SystemExit("filters: the rendered file does not read back as what was recorded")
    if args.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print("filters.json is up to date" if same else "filters.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KB, {len(data['texts'])} texts, "
          f"{len(data['rows'])} rows)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
