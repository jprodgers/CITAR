"""Record how the Python engine reads every unique text of the ruleset, for the refcheck ``uniques`` group
(crates/citar-engine/DESIGN.md 9.2, package 1a-05).

    PYTHONHASHSEED=0 python scripts/refcheck/uniques_dump.py            # writes refcheck/uniques.json.gz
    PYTHONHASHSEED=0 python scripts/refcheck/uniques_dump.py --check    # re-records and compares with the committed file

One record per unique text, for every object that has uniques, in the order the Rust compiler numbers them (Python's
``civ_umaps`` order, then the objects outside it: DESIGN.md 5.5), and in each object in the order its file lists them:

* ``id``: ``<source>/<object>/<index>``, the record's key;
* ``source``, ``name``, ``index``: the kind of object (Rust's ``Source``: ``Building``, ``CityStateAlly``, ...), its
  name and the text's position in its list; a unit's own list, without its unit type's (rules.py:116-118 copied those);
* ``text``, and ``type`` and ``placeholder`` as ``Unique`` parsed it (uniques.py:120-131), ``type`` being the
  unique_types.py name of the placeholder, or null for a text UnCiv does not know (``Aircraft``);
* ``params``: each parameter as the Python engine read it, by the kind UnCiv's signature gives it: ``num()`` for the
  amounts and fractions (uniques.py:93-102), ``parse_stats`` for stats as all seven keys (uniques.py:79-90), and the
  text for everything else, names and filters included;
* ``local``: ``Unique.is_local`` (uniques.py:130); ``timed``: the turns of ``<for [n] turns>``, or null;
* ``modifiers``: each ``<...>`` the same way, as ``{type, placeholder, params}``, in the text's order (the Rust answer
  folds some of them, so the group compares them as a multiset).

The Rust side compiles the same texts at load and writes the same fields from its compiled uniques; any difference is a
parameter Rust reads otherwise than Python did.
"""
from __future__ import annotations

import argparse
import gzip
import io
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from citar.engine.rules import Rules
from citar.engine.uniques import STAT_KEY, Unique, num, parse_stats

OUT = ROOT / "refcheck" / "uniques.json.gz"
STATS = [STAT_KEY[s] for s in ("Food", "Production", "Gold", "Science", "Culture", "Happiness", "Faith")]
NUMERIC = {"amount", "relativeAmount", "positiveAmount", "nonNegativeAmount", "fraction"}


def signatures() -> dict[str, tuple[str, list[str]]]:
    """unique_types.py: placeholder -> (type name, parameter kinds from the signature in its comment)."""
    out = {}
    for line in (ROOT / "citar" / "engine" / "unique_types.py").read_text(encoding="utf-8").splitlines():
        m = re.match(r"^(\w+) = ('.*?'|\".*?\")  # (.*)$", line)
        if not m:
            continue
        kinds, depth, cur = [], 0, ""
        for c in m.group(3):
            if c == "[":
                if depth:
                    cur += c
                else:
                    cur = ""
                depth += 1
            elif c == "]":
                depth -= 1
                if depth == 0:
                    kinds.append(cur)
                else:
                    cur += c
            elif depth:
                cur += c
        out[eval(m.group(2))] = (m.group(1), kinds)
    return out


SIG = signatures()


def read(kind: str, p: str):
    """A parameter as the Python engine read it."""
    if kind in NUMERIC:
        return num(p)
    if kind == "stats":
        s = parse_stats(p)
        return None if s is None else {k: s.get(k, 0.0) for k in STATS}
    if kind == "positiveAmount/'all'":
        return p if p in ("All", "all") else num(p)
    return p


def reading(ph: str, params: list[str]) -> tuple:
    """(type, params read by kind); an unknown placeholder keeps its parameters as text."""
    name, kinds = SIG.get(ph, (None, []))
    if name is None or len(kinds) != len(params):
        return name, list(params)
    return name, [read(k, p) for k, p in zip(kinds, params)]


def sources(r: Rules) -> list[tuple[str, str, list]]:
    """(source kind, object name, unique texts) in the Rust compiler's order."""
    out = []
    add = lambda kind, table, field="uniques": out.extend(  # noqa: E731
        (kind, name, obj.get(field) or []) for name, obj in table.items())
    add("Nation", r.nations)
    add("Building", r.buildings)
    add("Policy", r.policy_branches)
    add("Policy", r.policies)
    add("Tech", r.techs)
    add("Era", r.eras)
    add("CityStateFriend", r.city_state_types, "friendBonusUniques")
    add("CityStateAlly", r.city_state_types, "allyBonusUniques")
    add("CityStateType", r.city_state_types)
    add("Belief", r.beliefs)
    add("Resource", r.resources)
    gl = json.loads((ROOT / "citar" / "data" / "ruleset" / "global_uniques.json").read_text(encoding="utf-8"))
    out.append(("Global", "", gl["uniques"]))
    add("Terrain", r.terrains)
    add("Improvement", r.improvements)
    add("UnitType", r.unit_types)
    add("Unit", r.units)
    add("Promotion", r.promotions)
    add("Ruins", r.ruins)
    return out


def record() -> dict:
    rules = Rules()
    rows = []
    for kind, name, texts in sources(rules):
        for i, text in enumerate(texts):
            u = Unique(text, kind, name)
            ty, params = reading(u.ph, u.params)
            mods = []
            for m in u.mods:
                mty, mparams = reading(m.ph, m.params)
                mods.append({"type": mty, "placeholder": m.ph, "params": mparams})
            timer = u.mod("for [] turns")
            rows.append({
                "id": f"{kind}/{name}/{i}",
                "source": kind,
                "name": name,
                "index": i,
                "text": text,
                "type": ty,
                "placeholder": u.ph,
                "params": params,
                "local": u.is_local,
                "timed": int(timer.n(0)) if timer is not None else None,
                "modifiers": mods,
            })
    return {"format": 1, "count": len(rows), "uniques": rows}


def render(data: dict) -> bytes:
    """One record per line, gzipped without a timestamp, so a fresh recording of the same ruleset is the same bytes."""
    lines = ['{"format": 1,', f'"count": {data["count"]},', '"uniques": [']
    rows = data["uniques"]
    lines += [json.dumps(row, ensure_ascii=False) + ("," if i + 1 < len(rows) else "") for i, row in enumerate(rows)]
    lines += ["]}"]
    text = ("\n".join(lines) + "\n").encode("utf-8")
    buf = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=buf, mtime=0, compresslevel=9) as f:
        f.write(text)
    return buf.getvalue()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    data = record()
    blob = render(data)
    if json.loads(gzip.decompress(blob).decode("utf-8")) != data:
        raise SystemExit("uniques_dump: the rendered file does not read back as what was recorded")
    if args.check:
        same = OUT.exists() and OUT.read_bytes() == blob
        print("uniques.json.gz is up to date" if same else "uniques.json.gz differs from a fresh recording")
        return 0 if same else 1
    OUT.write_bytes(blob)
    print(f"wrote {OUT.relative_to(ROOT)} ({len(blob) // 1024} KB, {data['count']} uniques)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
