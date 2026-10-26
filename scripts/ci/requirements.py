"""Print the project's requirements from pyproject.toml, with the named extras, one per line for ``pip install -r``.

Installing the package itself builds its Rust extension (maturin), which a job that already has the extension has
no use for: CI's test jobs take it from the build-ext job's wheel (scripts/ci/unpack_ext.py) and the Docker image
from a release wheel, and the image's dependency layer is built from pyproject.toml alone, so that editing the code
does not reinstall the world. Both install what this prints first.

    python scripts/ci/requirements.py all       # [project] dependencies and the `all` extra
    python scripts/ci/requirements.py server

The project's own extras inside an extra (``citar[anthropic,oauth]``) are expanded, and each requirement is printed
once, in the file's order. An extra the file does not define is an error (exit 2), as pip would only warn.
"""
from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def requirements(pyproject: dict, extras: list[str]) -> list[str]:
    """``[project] dependencies``, then each extra's, the project's own extras expanded, without repeats."""
    project = pyproject["project"]
    name = project["name"]
    optional = project.get("optional-dependencies", {})
    own = re.compile(rf"^{re.escape(name)}\s*\[([^\]]*)\]\s*$", re.IGNORECASE)
    out: list[str] = []
    seen: set[str] = set()

    def add(reqs: list[str]) -> None:
        for req in reqs:
            match = own.match(req)
            if match:
                expand([e.strip() for e in match.group(1).split(",") if e.strip()])
            elif req not in out:
                out.append(req)

    def expand(names: list[str]) -> None:
        for extra in names:
            if extra in seen:
                continue
            if extra not in optional:
                raise KeyError(extra)
            seen.add(extra)
            add(optional[extra])

    add(project.get("dependencies", []))
    expand(extras)
    return out


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("extras", nargs="*", help="extras to include, as in citar[all]")
    args = parser.parse_args(argv)
    data = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    try:
        reqs = requirements(data, args.extras)
    except KeyError as e:
        print(f"requirements: pyproject.toml has no extra {e.args[0]!r}", file=sys.stderr)
        return 2
    print("\n".join(reqs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
