"""Write the parameter schema of the bot version basic-1 from the live bot's PARAM_GROUPS (citar/bots/basic.py):
crates/citar-bot/params/basic-1.json (crates/citar-engine DESIGN.md P2.3.2, package 2-00b).

    python scripts/bots/export_params.py            # writes the file
    python scripts/bots/export_params.py --check    # exits 1 when the file is not what PARAM_GROUPS gives

The file is ``{"engine": "basic-1", "groups": [{"name", "help", "params": [spec, ...]}, ...]}``, the shape the Bots
page's ``/api/bots/schema`` serves, and each spec is Python's own (``key``, ``type``, ``default``, ``label``, ``help``;
``min``, ``max`` and ``unit`` for numbers; ``choices`` for a choice; ``options`` and ``presets`` for an order or a list).
Two parameters are left out, because the Rust bot has no cache for them to tune (P2.3.9 fixes 5 and 6):
``site_cache_turns`` and ``bv_cache_turns``.

Python's ``_n`` typed a number by its default's literal (basic.py:71-74), so a multiplier or weight whose default
happened to be written ``3`` became an ``int``, and basic-1's ``clean()`` refuses a fractional ``int`` (P2.3.2). The
41 parameters in ``RETYPED`` are written as ``float``, with their default as a float (``3`` becomes ``3.0``): the eight
whose unit is ``x`` (a multiplier, as the 26 other ``x`` parameters, already floats, are) and the 37 the engine's
``AdvisorParams`` holds as ``f64`` (advisor.rs), four of them being both. basic.py uses every one of them only in float
arithmetic and comparisons, never in ``//``, ``range`` or an index, so the Python bot plays the same with either type.

Once written, the file is the source of truth: the Rust bot reads it (``include_str!`` and the generated ``Params``),
and tests/test_bot_params.py holds it equal to PARAM_GROUPS while the Python bot exists. This script goes with the
Python engine (package 2-12).
"""
from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

OUT = ROOT / "crates" / "citar-bot" / "params" / "basic-1.json"
VERSION = "basic-1"
#: The parameters of basic.py that basic-1 drops: caches the Rust bot does not keep (DESIGN.md P2.3.9, fixes 5 and 6)
DROPPED = ("site_cache_turns", "bv_cache_turns")
#: The parameters PARAM_GROUPS types ``int`` only because of their default's literal, which basic-1 types ``float``:
#: the multipliers (unit ``x``) and the weights the engine's AdvisorParams holds as f64 (see the module's doc)
RETYPED = (
    # Building value (UnCiv mode)
    "u_gold_broke_mult", "u_culture_first_mult", "u_culture_first_below", "u_defense_threat_mult", "u_city_health",
    "u_city_strength", "u_city_strength_offset", "u_victory_building",
    # Build priorities (UnCiv mode)
    "mil_base", "mil_war_mult", "mil_below_avg_div", "mil_army_full_div", "military_min_gold",
    # Classic production
    "c_danger", "c_garrison", "c_military_min_gpt", "c_army_offense", "c_army_offense_aggr", "c_army_peace",
    "c_army_peace_aggr", "c_scout_needed", "c_scout", "c_seeker", "c_worker_urgent", "c_worker", "c_boat",
    "c_building_scale", "c_building_turns", "c_spaceship",
    # Classic building value
    "w_def_strength_div", "w_def_health_div",
    # Expansion
    "site_min_score", "site_new_lux",
    # Army
    "army_base", "army_war_per_city", "army_war_extra", "army_per_threatened_city",
    # Tactics, War, Peace and diplomacy
    "atk_kill_mult", "overwhelm_ratio", "weak_power_ratio", "friend_max_ratio",
)


def retype(spec: dict) -> dict:
    """A spec of RETYPED as basic-1 has it: a float with a float default, everything else Python's."""
    assert spec["type"] == "int", spec["key"]
    return dict(spec, type="float", default=float(spec["default"]))


def schema() -> dict:
    """basic-1's schema from PARAM_GROUPS: every group, in order, without the dropped parameters, and the retyped
    ones as floats."""
    from citar.bots.basic import PARAM_GROUPS
    groups = []
    for name, help_text, specs in PARAM_GROUPS:
        params = [copy.deepcopy(s) for s in specs if s["key"] not in DROPPED]
        groups.append({"name": name, "help": help_text,
                       "params": [retype(s) if s["key"] in RETYPED else s for s in params]})
    return {"engine": VERSION, "groups": groups}


def text() -> str:
    """The file's text: two-space JSON, the keys in Python's order, UTF-8 as written, a final newline."""
    return json.dumps(schema(), indent=2, ensure_ascii=False) + "\n"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="compare with the file instead of writing it")
    args = ap.parse_args(argv)
    want = text()
    if args.check:
        have = OUT.read_text(encoding="utf-8") if OUT.exists() else None
        if have != want:
            print(f"{OUT.relative_to(ROOT)} is not what PARAM_GROUPS gives; run scripts/bots/export_params.py")
            return 1
        print(f"{OUT.relative_to(ROOT)} is current")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(want, encoding="utf-8", newline="\n")
    doc = schema()
    count = sum(len(g["params"]) for g in doc["groups"])
    print(f"wrote {OUT.relative_to(ROOT)}: {count} parameters in {len(doc['groups'])} groups")
    return 0


if __name__ == "__main__":
    sys.exit(main())
