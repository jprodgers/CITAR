"""List ruleset uniques/conditionals whose placeholder is not a known UnCiv UniqueType (diagnostic)."""
import json
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from citar.engine import unique_types as UT
from citar.engine.uniques import Unique

known = {v: k for k, v in vars(UT).items() if isinstance(v, str) and not k.startswith("_")}
unknown, used = Counter(), Counter()


def walk(o):
    if isinstance(o, dict):
        for k, v in o.items():
            if k in ("uniques", "friendBonusUniques", "allyBonusUniques") and isinstance(v, list):
                for t in v:
                    u = Unique(t)
                    for x in [u] + u.mods:
                        (used if x.ph in known else unknown)[x.ph] += 1
            else:
                walk(v)
    elif isinstance(o, list):
        for x in o:
            walk(x)


for f in (ROOT / "citar/data/ruleset").glob("*.json"):
    walk(json.loads(f.read_text(encoding="utf-8")))
print(len(used), "known placeholders used;", len(unknown), "unknown")
for k, n in unknown.most_common():
    print(n, repr(k))
if "--used" in sys.argv:
    for k, n in sorted(used.items(), key=lambda x: known[x[0]]):
        print(f"{known[k]:45} {n:4}  {k}")
