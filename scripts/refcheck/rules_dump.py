"""Record the Python ruleset's client JSON and name lookups, for the Rust port's ruleset tests (crates/citar-engine
DESIGN.md 5.3, package 1a-03).

    PYTHONHASHSEED=0 python scripts/refcheck/rules_dump.py            # writes crates/citar-testkit/data/rules_dump.json
    PYTHONHASHSEED=0 python scripts/refcheck/rules_dump.py --check    # re-records and compares with the committed file

It records two things:

* ``client``: ``Rules.to_client()`` (rules.py:303-331), which the Rust ``Ruleset::client_json`` must equal when both
  are compared as JSON values, object key order included, since the browser iterates the tables in document order;
* ``resolve``: ``Rules.resolve(kind, text)`` (rules.py:263-270) as rows ``[text, answer]`` per kind, for every
  object's name and id, each also written in other ways a tool call might write it (upper case, underscores for
  spaces, the normalised key, trailing punctuation), plus texts that resolve to nothing and Unicode letters that
  lower-case to ASCII. ``answer`` is the name as the ruleset spells it, or null.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from citar.engine.rules import Rules, norm

OUT = ROOT / "crates" / "citar-testkit" / "data" / "rules_dump.json"

KELVIN_SIGN = chr(0x212A)            # lower-cases to an ASCII "k"
DOTTED_CAPITAL_I = chr(0x130)        # lower-cases to "i" and a combining dot, which norm drops

# Texts that name nothing in any table, and texts that stress the normalisation.
EXTRA_PROBES = ("", " ", "_", "no such thing", "Nothing At All", KELVIN_SIGN + "orea", DOTTED_CAPITAL_I + "nca",
                "---", "0", "9999")


def name_variants(name: str) -> list[str]:
    """The ways a caller might write a name: exactly, loosely, and wrongly."""
    return [name, name.upper(), name.replace(" ", "_"), norm(name), name + "!", "x" + name]


def id_variants(key: str) -> list[str]:
    """The ways a caller might write an id."""
    return [key, key.upper(), "x" + key]


def record() -> dict:
    rules = Rules()
    resolve = {}
    for kind, table in rules.tables().items():
        probes: list[str] = []
        for name, obj in table.items():
            probes += name_variants(name)
            if obj.get("id"):
                probes += id_variants(obj["id"])
        probes += EXTRA_PROBES
        seen, rows = set(), []
        for text in probes:
            if text not in seen:
                seen.add(text)
                rows.append([text, rules.resolve(kind, text)])
        resolve[kind] = rows
    return {"format": 1, "client": rules.to_client(), "resolve": resolve}


def render(data: dict) -> str:
    """The client JSON indented and one resolve row per line, so a changed ruleset gives a readable diff."""
    lines = ["{", '"format": 1,', '"client": ' + json.dumps(data["client"], indent=1) + ",", '"resolve": {']
    kinds = list(data["resolve"].items())
    for k, (kind, rows) in enumerate(kinds):
        lines.append(json.dumps(kind) + ": [")
        lines += [json.dumps(row) + ("," if i + 1 < len(rows) else "") for i, row in enumerate(rows)]
        lines.append("]" + ("," if k + 1 < len(kinds) else ""))
    lines += ["}", "}"]
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    data = record()
    text = render(data)
    if json.loads(text) != data:
        raise SystemExit("rules_dump: the rendered file does not read back as what was recorded")
    if args.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print("rules_dump.json is up to date" if same else "rules_dump.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    rows = sum(len(r) for r in data["resolve"].values())
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KB, {rows} resolve rows)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
