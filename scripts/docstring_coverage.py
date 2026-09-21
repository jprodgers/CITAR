"""Report which functions and classes have no docstring.

    python scripts/docstring_coverage.py               # per-module summary, worst first
    python scripts/docstring_coverage.py citar/engine  # only under that path
    python scripts/docstring_coverage.py --list MODULE # the undocumented names in one module
    python scripts/docstring_coverage.py --strict       # non-zero exit if any module lacks one

Module docstrings are the ones that matter most and are counted separately: a module that says what
it is for and why it is shaped that way is worth more than a docstring on every getter inside it.

Not enforced in CI as a percentage. A threshold turns into "Does the thing." on every function,
which is worse than an honest gap — see CONTRIBUTING.md.
"""
from __future__ import annotations

import ast
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP = {"__pycache__", ".git", "node_modules", "site", "build", "dist", "wiki", "ops"}


def documentable(tree: ast.Module):
    """Every class and function in *tree*, with its qualified name and whether it is documented.

    Nested functions are included — a closure with surprising behaviour is exactly the kind of
    thing that needs a sentence — but dunder methods are not, because ``__repr__`` explains itself
    and a docstring on it is noise.
    """
    out = []

    def walk(node, prefix=""):
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                name = f"{prefix}{child.name}"
                if not (child.name.startswith("__") and child.name.endswith("__")):
                    out.append((name, child.lineno, ast.get_docstring(child) is not None))
                walk(child, prefix=f"{name}.")
            else:
                walk(child, prefix)

    walk(tree)
    return out


def files(base: Path):
    for path in sorted(base.rglob("*.py")):
        if any(part in SKIP for part in path.parts):
            continue
        if path.name.startswith("frozen_"):
            continue                      # snapshots of the bot, frozen on purpose
        yield path


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    strict = "--strict" in sys.argv
    show_list = "--list" in sys.argv

    targets = [ROOT / a for a in args] or [ROOT / "citar"]
    rows = []
    missing_module_docstring = []

    for base in targets:
        paths = [base] if base.is_file() else list(files(base))
        for path in paths:
            try:
                tree = ast.parse(path.read_text(encoding="utf-8"))
            except SyntaxError as exc:
                print(f"  {path.relative_to(ROOT)}: {exc}")
                continue
            items = documentable(tree)
            documented = sum(1 for _, _, has in items if has)
            has_module_doc = ast.get_docstring(tree) is not None
            if not has_module_doc:
                missing_module_docstring.append(path.relative_to(ROOT))
            rows.append((path, len(items), documented, has_module_doc, items))

    if show_list:
        for path, total, documented, _, items in rows:
            undocumented = [(name, line) for name, line, has in items if not has]
            if not undocumented:
                continue
            print(f"\n{path.relative_to(ROOT)}  ({len(undocumented)} of {total})")
            for name, line in undocumented:
                print(f"  {line:5}  {name}")
        return 0

    rows.sort(key=lambda r: (r[1] - r[2]), reverse=True)
    total_items = sum(r[1] for r in rows)
    total_documented = sum(r[2] for r in rows)

    print(f"{'module':<46} {'items':>6} {'documented':>11} {'':>4}")
    print("-" * 72)
    for path, total, documented, has_module_doc, _ in rows:
        if total == 0 and has_module_doc:
            continue
        percent = f"{100 * documented // total}%" if total else "-"
        flag = "" if has_module_doc else "  no module docstring"
        print(f"{str(path.relative_to(ROOT)):<46} {total:>6} {documented:>7} {percent:>4}{flag}")

    print("-" * 72)
    share = 100 * total_documented // max(total_items, 1)
    print(f"{'total':<46} {total_items:>6} {total_documented:>7} {share:>3}%")
    if missing_module_docstring:
        print(f"\n{len(missing_module_docstring)} module(s) with no docstring:")
        for path in missing_module_docstring:
            print(f"  {path}")

    return 1 if (strict and missing_module_docstring) else 0


if __name__ == "__main__":
    raise SystemExit(main())
