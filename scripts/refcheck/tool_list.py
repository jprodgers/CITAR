"""Record the Python engine's tool list, which the Rust registry's schemas must equal apart from its listed fixes
(crates/citar-engine/DESIGN.md 8.3, package 1d-01, gate 2).

    PYTHONHASHSEED=0 python scripts/refcheck/tool_list.py            # writes tests/rules/tool_list.json
    PYTHONHASHSEED=0 python scripts/refcheck/tool_list.py --check    # compares the committed file with the engine

The file holds ``tools.tool_list()`` exactly: every tool in the order ``tools.py`` registers them, each as
``{"name", "description", "input_schema", "kind", "any_time", "category"}``, the form sent to models, MCP clients and
the browser. The Rust test ``the_schemas_equal_python_s_tool_list`` (crates/citar-testkit/tests/engine/tools.rs)
compares ``api::tools::schemas_json()`` with it; ``tests/test_rule_scripts.py`` runs the check, so a change to a
Python tool that is not re-recorded fails there.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from citar.engine import tools

OUT = ROOT / "tests" / "rules" / "tool_list.json"


def render() -> str:
    """The tool list as the committed file holds it: one tool per line, keys in the engine's order."""
    lines = ",\n".join("  " + json.dumps(t, ensure_ascii=False) for t in tools.tool_list())
    return "[\n" + lines + "\n]\n"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args(argv)
    text = render()
    if args.check:
        committed = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if committed != text:
            print(f"{OUT.relative_to(ROOT).as_posix()} is out of date: re-record it with scripts/refcheck/tool_list.py")
            return 1
        print(f"{OUT.relative_to(ROOT).as_posix()} is current ({len(tools.REGISTRY)} tools)")
        return 0
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT).as_posix()} ({len(tools.REGISTRY)} tools)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
