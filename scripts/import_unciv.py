"""Import the numeric data and rule logic of UnCiv's "Civ V - Gods & Kings" ruleset into CITAR.

    python scripts/import_unciv.py [path/to/Unciv]

UnCiv (https://github.com/yairm210/Unciv) is licensed under the Mozilla Public License 2.0; the generated files in
citar/data/ruleset/ are derived from its ruleset JSON and carry the same license (see citar/data/ruleset/NOTICE.md).

Only numbers and rule text ("uniques") are kept. Graphics, sounds, colours, quotes, civilopedia text, leader dialogue,
tutorial events and name lists for spies / great people are dropped. City name lists are kept only as default names.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import unciv_json

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "citar" / "data" / "ruleset"
DEFAULT_UNCIV = None
RULESET = "android/assets/jsons/Civ V - Gods & Kings"

# Fields that are presentation only.
DROP_FIELDS = {
    "quote", "civilopediaText", "attackSound", "citySound", "iconRGB", "RGB", "color", "outerColor", "innerColor",
    "startIntroPart1", "startIntroPart2", "declaringWar", "attacked", "defeated", "introduction", "neutralHello",
    "hateHello", "tradeRequest", "denounced", "uniqueName", "spyNames", "shortcutKey", "presentation", "notification",
    "victoryScreenHeader", "victoryString", "defeatString", "description", "row", "column",
}
# Uniques that only drive UnCiv's UI / tutorials / sounds.
DROP_UNIQUE = re.compile(
    r"^(Will not be displayed in Civilopedia|Play \[.*\] sound|Mark tutorial|Comment \[|Get the leader title of|"
    r"Once The Long Count activates|Excluded from map editor|\[.*\] gets a name from the \[.*\] group|"
    r"Triggers the following global alert)"
)
DROP_MODIFIER = re.compile(r"\s*<(Civilopedia link \[[^>]*\]|Suppress warning \[[^>]*\]|hidden from users)>")


def snake(name: str) -> str:
    s = re.sub(r"[^0-9a-zA-Z]+", "_", name).strip("_").lower()
    return s


def clean_uniques(lst):
    out = []
    for u in lst or []:
        if not isinstance(u, str) or DROP_UNIQUE.match(u):
            continue
        out.append(DROP_MODIFIER.sub("", u).strip())
    return out


def clean(obj: dict) -> dict:
    out = {}
    for k, v in obj.items():
        if k in DROP_FIELDS:
            continue
        if k in ("uniques", "triggeredUniques", "friendBonusUniques", "allyBonusUniques"):
            v = clean_uniques(v)
            if not v:
                continue
        out[k] = v
    return out


def keyed(items, kind: str) -> dict:
    out = {}
    for it in items:
        c = clean(it)
        c["id"] = snake(c["name"])
        out[c["name"]] = c
    return out


def main(unciv: Path):
    src = unciv / RULESET
    L = lambda f: unciv_json.load(src / f)  # noqa: E731
    OUT.mkdir(parents=True, exist_ok=True)
    files: dict[str, object] = {}

    # --- technologies: flatten columns; tech cost comes from its column ------------------------------------------
    [e["name"] for e in L("Eras.json")]
    techs, columns = {}, []
    for col in L("Techs.json"):
        columns.append({k: col[k] for k in ("columnNumber", "era", "techCost", "buildingCost", "wonderCost") if k in col})
        for t in col["techs"]:
            c = clean(t)
            c["id"] = snake(c["name"])
            c["era"] = col["era"]
            c["column"] = col["columnNumber"]
            c.setdefault("cost", col["techCost"])
            c.setdefault("prerequisites", [])
            techs[c["name"]] = c
    files["techs.json"] = {"columns": columns, "techs": techs}

    # --- buildings: default costs come from the required tech's column (Ruleset.updateBuildingCosts) --------------
    col_by_num = {c["columnNumber"]: c for c in columns}
    buildings = keyed(L("Buildings.json"), "building")
    for b in buildings.values():
        if "cost" in b:
            continue
        if any(u == "Unbuildable" for u in b.get("uniques", [])):
            continue
        tech = b.get("requiredTech")
        if tech and tech in techs:
            col = col_by_num[techs[tech]["column"]]
            wonder = b.get("isWonder") or b.get("isNationalWonder")
            b["cost"] = col["wonderCost"] if wonder else col["buildingCost"]
    files["buildings.json"] = buildings

    files["units.json"] = keyed(L("Units.json"), "unit")
    files["unit_types.json"] = keyed(L("UnitTypes.json"), "unit type")
    files["promotions.json"] = keyed(L("UnitPromotions.json"), "promotion")
    files["terrains.json"] = keyed(L("Terrains.json"), "terrain")
    files["resources.json"] = keyed(L("TileResources.json"), "resource")
    files["improvements.json"] = keyed(L("TileImprovements.json"), "improvement")
    files["beliefs.json"] = keyed(L("Beliefs.json"), "belief")
    files["religions.json"] = L("Religions.json")
    files["specialists.json"] = keyed(L("Specialists.json"), "specialist")
    files["city_state_types.json"] = keyed(L("CityStateTypes.json"), "city-state type")
    files["difficulties.json"] = keyed(L("Difficulties.json"), "difficulty")
    files["speeds.json"] = keyed(L("Speeds.json"), "speed")
    files["eras.json"] = {e["name"]: dict(clean(e), id=snake(e["name"]), number=i)
                          for i, e in enumerate(L("Eras.json"))}
    files["victories.json"] = keyed(L("VictoryTypes.json"), "victory")
    quests = keyed(L("Quests.json"), "quest")
    for q in L("Quests.json"):          # keep numeric description parameters (e.g. Invest's [50]% bonus)
        nums = [float(x) for x in re.findall(r"\[(-?\d+(?:\.\d+)?)\]", q.get("description", ""))]
        if nums:
            quests[q["name"]]["params"] = nums
    files["quests.json"] = quests
    files["ruins.json"] = keyed(L("Ruins.json"), "ruin")
    files["personalities.json"] = keyed(L("Personalities.json"), "personality")
    files["global_uniques.json"] = {"uniques": clean_uniques(L("GlobalUniques.json")["uniques"])}

    # --- policies: branches with their member policies ------------------------------------------------------------
    branches, policies = {}, {}
    for br in L("Policies.json"):
        b = clean(br)
        members = b.pop("policies", [])
        b["id"] = snake(b["name"])
        b["members"] = []
        for p in members:
            c = clean(p)
            c["id"] = snake(c["name"])
            c["branch"] = b["name"]
            c.setdefault("requires", [b["name"]])      # Ruleset.kt: members without requirements need the branch
            if c["name"].endswith(" Complete"):
                c["is_finisher"] = True
            else:
                b["members"].append(c["name"])
            policies[c["name"]] = c
        branches[b["name"]] = b
    files["policies.json"] = {"branches": branches, "policies": policies}

    # --- nations: majors, city-states and barbarians; no dialogue or colours -------------------------------------
    nations = {}
    for n in L("Nations.json"):
        if n["name"] == "Spectator":
            continue
        c = clean(n)
        c["id"] = snake(c["name"])
        if isinstance(c.get("adjective"), list):
            c["adjective"] = c["adjective"][0] if c["adjective"] else c["name"]
        c["kind"] = "barbarian" if c["name"] == "Barbarians" else ("city_state" if "cityStateType" in c else "major")
        nations[c["name"]] = c
    files["nations.json"] = nations

    for name, data in files.items():
        with open(OUT / name, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=1, ensure_ascii=False)
            f.write("\n")
    (OUT / "NOTICE.md").write_text(NOTICE, encoding="utf-8")
    print(f"Wrote {len(files)} files to {OUT}")
    for name, data in files.items():
        n = len(data.get("techs", data.get("policies", data))) if isinstance(data, dict) else len(data)
        print(f"  {name}: {n}")


NOTICE = """# UnCiv-derived rule data

The JSON files in this folder are generated by `scripts/import_unciv.py` from the "Civ V - Gods & Kings"
ruleset of **UnCiv** (https://github.com/yairm210/Unciv, © Yair Morgenstern and contributors).

UnCiv is licensed under the **Mozilla Public License, v. 2.0**; these derived files are covered by the same
license. A copy of the MPL 2.0 is available at https://mozilla.org/MPL/2.0/.

Only numeric values and rule text were imported. UnCiv's graphics, sounds, quotes, civilopedia text, leader
dialogue and tutorials are not included. Local additions live in `citar/data/custom/` (for example BenchmarkCiv).
"""

if __name__ == "__main__":
    if len(sys.argv) < 2 and DEFAULT_UNCIV is None:
        sys.exit("usage: python scripts/import_unciv.py <path to an Unciv checkout>\n"
                 "       git clone --depth 1 https://github.com/yairm210/Unciv")
    main(Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_UNCIV)
