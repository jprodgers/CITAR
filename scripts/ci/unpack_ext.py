"""Put the Rust extension from a built wheel into this checkout, for CI's test jobs.

test.yml's build-ext job builds ``citar._engine`` once per OS, as an abi3 wheel (crates/citar-engine/DESIGN.md
P2.6.8). Each test job then runs the checkout's own sources with that build: this takes the extension library
(``citar/_engine.pyd``, ``citar/_engine.abi3.so``) out of the wheel and writes it into the checkout's ``citar/``,
where ``import citar._engine`` finds it while every other module still comes from the checkout. An abi3 library
loads in every Python from 3.11, so the three Linux versions share one build.

    python scripts/ci/unpack_ext.py WHEEL_OR_DIR [--test-ops]

``WHEEL_OR_DIR`` is a wheel, or a directory holding exactly one. Only the library is taken: the wheel's Python
files are a copy of the checkout's. Any other ``_engine`` library already in ``citar/`` is removed first, so an
older build with another file name can never be the one Python imports. The library's suffix must be one this
interpreter loads (``importlib.machinery.EXTENSION_SUFFIXES``), which tells a Windows wheel from a Linux or macOS
one; the import catches the rest. Last, a fresh interpreter imports it from the checkout and prints its build;
``--test-ops`` also requires the test operations, which the suite's engine tests need: without them those tests
are skipped, and a test job that skipped them would pass having tested nothing of the engine. A library that fails
this check is removed again.

Exit status 0 when the library is in place and imports, 1 when not (with the reason), 2 for a usage error.
"""
from __future__ import annotations

import argparse
import importlib.machinery
import os
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "citar"
MODULE = "_engine"


class Refused(Exception):
    """The wheel cannot be unpacked here; the message says why."""


def find_wheel(where: Path) -> Path:
    """The wheel ``where`` names: itself, or the one wheel in it."""
    if where.is_file():
        return where
    if not where.is_dir():
        raise Refused(f"{where}: no such wheel or directory")
    wheels = sorted(where.glob("*.whl"))
    if len(wheels) != 1:
        found = ", ".join(w.name for w in wheels) or "none"
        raise Refused(f"{where}: expected exactly one wheel, found {found}")
    return wheels[0]


def library_suffix(name: str) -> str | None:
    """The extension-module suffix of ``citar/_engine<suffix>`` in a wheel, or None for any other member."""
    prefix = f"citar/{MODULE}"
    if not name.startswith(prefix) or "/" in name[len("citar/"):]:
        return None
    suffix = name[len(prefix):]
    # The stub, _engine.pyi, and anything else that is not a library.
    return suffix if suffix.endswith((".pyd", ".so")) else None


def unpack(wheel: Path, package: Path = PACKAGE) -> Path:
    """Write the wheel's extension library into ``package`` and return its path."""
    try:
        archive = zipfile.ZipFile(wheel)
    except (OSError, zipfile.BadZipFile) as e:
        raise Refused(f"{wheel}: not a wheel ({e})") from None
    with archive:
        members = [(n, s) for n in archive.namelist() if (s := library_suffix(n)) is not None]
        if len(members) != 1:
            found = ", ".join(n for n, _ in members) or "none"
            raise Refused(f"{wheel.name}: expected one citar/{MODULE} library, found {found}")
        name, suffix = members[0]
        if suffix not in importlib.machinery.EXTENSION_SUFFIXES:
            raise Refused(f"{wheel.name}: {name} is not a library this Python loads (it loads "
                          f"{', '.join(importlib.machinery.EXTENSION_SUFFIXES)}): another platform's wheel?")
        data = archive.read(name)
    for old in package.glob(f"{MODULE}.*"):
        if library_suffix(f"citar/{old.name}") is not None:
            old.unlink()
    target = package / f"{MODULE}{suffix}"
    target.write_bytes(data)
    return target


def check_import(root: Path = ROOT, test_ops: bool = False) -> str:
    """Import the extension from the checkout in a fresh interpreter; its build, or Refused."""
    code = ("import json, sys, citar._engine as E\n"
            "print(json.loads(E.build_info())['label'], E.__file__, E.HAS_TEST_OPS)\n"
            f"sys.exit(0 if E.HAS_TEST_OPS or not {test_ops!r} else 3)\n")
    # The checkout's library, not a dev loop's: CITAR_EXT_DIR would put another folder first, and a dev venv's
    # citar-dev.pth sets it at startup, so the check runs without the site hook (-S) as well as without the variable.
    # The import needs nothing outside the standard library.
    env = {k: v for k, v in os.environ.items() if k != "CITAR_EXT_DIR"}
    done = subprocess.run([sys.executable, "-S", "-c", code], cwd=root, env=env, capture_output=True, text=True)
    if done.returncode == 3:
        raise Refused(f"the extension was built without the test operations: {done.stdout.strip()}")
    if done.returncode != 0:
        raise Refused(f"the extension does not import: {(done.stdout + done.stderr).strip()}")
    return done.stdout.strip()


def install(where: Path, test_ops: bool = False, package: Path = PACKAGE) -> tuple[Path, Path, str]:
    """Unpack the wheel ``where`` names into ``package`` and import it: the wheel, the library and its build.

    A library that does not pass is removed again, so a refusal never leaves a build in place that the next import
    would pick up."""
    wheel = find_wheel(where)
    target = unpack(wheel, package)
    try:
        build = check_import(package.parent, test_ops)
    except Refused:
        target.unlink(missing_ok=True)
        raise
    return wheel, target, build


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("wheel", type=Path, help="a wheel, or a directory holding exactly one")
    parser.add_argument("--test-ops", action="store_true", help="require the engine's test operations")
    args = parser.parse_args(argv)
    try:
        wheel, target, build = install(args.wheel, args.test_ops)
    except Refused as e:
        print(f"unpack_ext: {e}", file=sys.stderr)
        return 1
    print(f"unpack_ext: {wheel.name} -> {target.relative_to(ROOT).as_posix()}; imports as {build}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
