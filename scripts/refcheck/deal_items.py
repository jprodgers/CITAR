"""Record the Python engine's deal item dicts, for the Rust port's DealItem test (crates/citar-engine DESIGN.md 4.6,
package 1a-08).

    PYTHONHASHSEED=0 python scripts/refcheck/deal_items.py            # writes crates/citar-testkit/data/deal_items.json
    PYTHONHASHSEED=0 python scripts/refcheck/deal_items.py --check    # re-records and compares with the committed file

A deal item is stored in proposals, deals and negotiation histories exactly as diplomacy._normalize_items leaves it.
For each of the 13 item types this records what _normalize_items makes of the example in ITEM_TYPES
(diplomacy.py:17-31), plus a few inputs that exercise its defaults and name resolution: a gold_per_turn and an
open_borders with no turns (the speed's deal duration), a resource with no amount and a loosely written name, and a
tech written in lower case. The game speed is the ruleset's default.

The Rust DealItem must read every recorded dict and write it back identically, key order included.
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

from citar.engine import diplomacy
from citar.engine.rules import get_rules

OUT = ROOT / "crates" / "citar-testkit" / "data" / "deal_items.json"
EXTRA = [
    {"type": "gold_per_turn", "amount": 3},
    {"type": "open_borders"},
    {"type": "resource", "resource": "iron"},
    {"type": "tech", "tech": "bronze working"},
    {"type": "declare_war", "target": "2"},
]


class _Game:
    """The two things _normalize_items reads from a game: its rules and its speed."""

    def __init__(self):
        self.rules = get_rules()
        self.speed = self.rules.speed(None)


def examples() -> list[dict]:
    """The example item of each type, as ITEM_TYPES spells it out."""
    out = []
    for kind, text in diplomacy.ITEM_TYPES.items():
        m = re.search(r"\{.*\}", text)
        if m is None:
            raise SystemExit(f"no example for {kind}")
        item = json.loads(m.group(0))
        if item.get("type") != kind:
            raise SystemExit(f"the example for {kind} is of type {item.get('type')}")
        out.append(item)
    return out


def record() -> list[dict]:
    """Every input with what _normalize_items made of it."""
    g = _Game()
    rows = []
    for item in [*examples(), *EXTRA]:
        [norm] = diplomacy._normalize_items(g, [item])
        rows.append({"input": item, "item": norm})
    return rows


def render(rows: list[dict]) -> str:
    """One row per line."""
    head = {"format": 1, "source": "citar/engine/diplomacy.py", "recorder": "scripts/refcheck/deal_items.py"}
    body = ",\n".join(json.dumps(r, separators=(",", ":")) for r in rows)
    return json.dumps(head)[:-1] + ', "rows": [\n' + body + "\n]}\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    text = render(record())
    if args.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print("deal_items.json is up to date" if same else "deal_items.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
