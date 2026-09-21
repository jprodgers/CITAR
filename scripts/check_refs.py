"""Static check: calls to functions of sibling engine modules that do not exist (catches refactor leftovers).

    python scripts/check_refs.py [module-to-skip ...]
"""
import pathlib
import re
import sys

eng = pathlib.Path(__file__).resolve().parent.parent / "citar" / "engine"
skip = set(sys.argv[1:])
mods = {p.stem for p in eng.glob("*.py")}
defs = {}
for m in mods:
    src = (eng / f"{m}.py").read_text(encoding="utf-8")
    defs[m] = (set(re.findall(r"^def (\w+)", src, re.M)) | set(re.findall(r"^(\w+)\s*[:=]", src, re.M))
               | set(re.findall(r"^class (\w+)", src, re.M)))

PAREN_MOD = re.compile(r"from \. import \(([^)]*)\)")
LINE_MOD = re.compile(r"from \. import ([\w ,]+)$", re.M)
PAREN_NAMES = re.compile(r"from \.(\w+) import \(([^)]*)\)")
LINE_NAMES = re.compile(r"from \.(\w+) import ([\w ,]+)$", re.M)

bad = set()
for m in sorted(mods - skip):
    src = (eng / f"{m}.py").read_text(encoding="utf-8")
    alias = {}
    for grp in PAREN_MOD.findall(src) + LINE_MOD.findall(src):
        for part in grp.replace("\n", " ").split(","):
            part = part.strip()
            if not part:
                continue
            if " as " in part:
                a, b = [x.strip() for x in part.split(" as ")]
                alias[b] = a
            else:
                alias[part] = part
    for mod, fn in re.findall(r"\b(\w+)\.(\w+)\b", src):
        real = alias.get(mod)
        if real in defs and fn not in defs[real]:
            bad.add((m, f"{mod}.{fn}"))
    for mod, names in PAREN_NAMES.findall(src) + LINE_NAMES.findall(src):
        if mod not in defs:
            continue
        for n in names.replace("\n", " ").split(","):
            n = n.strip().split(" as ")[0].strip()
            if n and n not in defs[mod]:
                bad.add((m, f"from .{mod} import {n}"))
for b in sorted(bad):
    print(*b)
