"""Write the parameter schema of the bot version basic-1 from the live bot's PARAM_GROUPS (citar/bots/basic.py):
crates/citar-bot/params/basic-1.json (crates/citar-engine DESIGN.md P2.3.2, package 2-00b).

    python scripts/bots/export_params.py            # writes the file
    python scripts/bots/export_params.py --check    # exits 1 when the file is not what PARAM_GROUPS gives

The file is ``{"engine": "basic-1", "groups": [{"name", "help", "params": [spec, ...]}, ...]}``, the shape the Bots
page's ``/api/bots/schema`` serves, and each spec is Python's own (``key``, ``type``, ``default``, ``label``, ``help``;
``min``, ``max`` and ``unit`` for numbers; ``choices`` for a choice; ``options`` and ``presets`` for an order or a list).
Two parameters are left out, because the Rust bot has no cache for them to tune (P2.3.9 fixes 5 and 6):
``site_cache_turns`` and ``bv_cache_turns``.

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


def schema() -> dict:
    """basic-1's schema from PARAM_GROUPS: every group, in order, without the dropped parameters."""
    from citar.bots.basic import PARAM_GROUPS
    groups = []
    for name, help_text, specs in PARAM_GROUPS:
        groups.append({"name": name, "help": help_text,
                       "params": [copy.deepcopy(s) for s in specs if s["key"] not in DROPPED]})
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
