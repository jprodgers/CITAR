"""Hold a built wheel or source distribution to the checkout it was built from, for CI's build-ext and package jobs.

maturin decides what each file carries from pyproject.toml's ``include`` and ``exclude``, the package folder and
``.gitignore``, and how it reads them has changed between its releases (crates/citar-engine/DESIGN.md, "As built in
2-06b"). test.yml pins the maturin it builds with (``MATURIN_VERSION``); this check makes a change of either one, the
pin or the lists, fail the job when it drops a file or picks up a stray one, rather than ship it.

    python scripts/ci/check_dist.py wheel WHEEL_OR_DIR [--tag TAG] [--library NAME]
    python scripts/ci/check_dist.py sdist SDIST_OR_DIR

A wheel must have:

- one ``Tag``, ``TAG`` when given (and the file name ending in it), otherwise a ``cp311-abi3`` one;
- exactly one extension library ``citar/_engine.*``, named ``NAME`` when given;
- under ``citar/``, exactly the files git tracks there, and the library;
- everything else inside its one ``citar-<version>.dist-info/``, the version being the file name's;
- every tracked file that a wheel ``include`` names, at its own path;
- no member twice.

A source distribution must have:

- one top folder, ``citar-<version>/``, the file name's;
- in it, nothing but files git tracks, and ``PKG-INFO``;
- every file git tracks under ``citar/``, every tracked file that an sdist ``include`` names and no ``exclude`` takes
  away, and the files a build from it starts from (``pyproject.toml``, the workspace's ``Cargo.toml`` and
  ``Cargo.lock``, ``crates/citar-py/Cargo.toml``);
- every file git tracks under ``crates/citar-engine/data/``: the ruleset, which the engine compiles in and its build
  id hashes, and which maturin carries with the crate rather than through an ``include``;
- no member twice.

``WHEEL_OR_DIR`` and ``SDIST_OR_DIR`` are the file, or a directory holding exactly one ``*.whl`` or ``*.tar.gz``. The
tracked files are ``git ls-files``' in the checkout this script lives in.

Exit status 0 when the file is as it should be, 1 when not (every difference listed), 2 when it cannot be checked
(no such file, no git checkout, a pattern the check does not read) or for a usage error.
"""
from __future__ import annotations

import argparse
import collections
import re
import subprocess
import sys
import tarfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = "citar/"
#: The extension library's member name: citar/_engine.pyd, citar/_engine.abi3.so, or a version-specific suffix.
LIBRARY = re.compile(r"citar/_engine\.(?:[^/]*\.)?(?:pyd|so)")
#: Files a build from the source distribution cannot start without.
SDIST_NEEDS = ("pyproject.toml", "Cargo.toml", "Cargo.lock", "crates/citar-py/Cargo.toml")
#: Folders whose every tracked file a build from the source distribution compiles in: the ruleset, which
#: ``rules::source::embedded()`` takes with include_bytes! and the engine's build script hashes into its build id.
#: No include names it (maturin packs it with the crate), so without this a change to maturin, or to the crate's
#: ``include`` or ``exclude``, could drop it and leave a source distribution that does not build.
SDIST_NEEDS_TREES = ("crates/citar-engine/data/",)
#: The one file in a source distribution that no checkout has: its metadata, which the build writes.
SDIST_OWN = "PKG-INFO"


class Refused(Exception):
    """The file cannot be checked at all (a usage problem); the message says why."""


def find(where: Path, suffix: str) -> Path:
    """The file ``where`` names: itself, or the one ``*<suffix>`` in it."""
    if where.is_file():
        return where
    if not where.is_dir():
        raise Refused(f"{where}: no such file or directory")
    found = sorted(where.glob(f"*{suffix}"))
    if len(found) != 1:
        raise Refused(f"{where}: expected exactly one *{suffix}, found {', '.join(f.name for f in found) or 'none'}")
    return found[0]


def tracked_files(root: Path = ROOT) -> set[str]:
    """Every path git tracks in the checkout at ``root``, relative to it, with forward slashes."""
    try:
        done = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True)
    except (OSError, subprocess.CalledProcessError) as e:
        raise Refused(f"git ls-files failed in {root} ({e}): the check needs the checkout the file was built from")
    return {p for p in done.stdout.decode("utf-8").split("\0") if p}


def glob_regex(pattern: str) -> re.Pattern[str]:
    """maturin's glob as a regular expression over a slash-separated path: ``**/`` any folders, ``*`` and ``?``
    within one name."""
    if any(c in pattern for c in "[]{}!"):
        raise Refused(f"the pattern {pattern!r} uses glob syntax this check does not read; teach glob_regex it")
    out, i = [], 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out))


def patterns(maturin: dict, key: str, form: str) -> list[re.Pattern[str]]:
    """The ``include`` or ``exclude`` patterns of ``[tool.maturin]`` that apply to ``form`` ("wheel" or "sdist"). A
    plain string, or a table without ``format``, applies to both."""
    out = []
    for entry in maturin.get(key, []):
        if isinstance(entry, str):
            path, formats = entry, ("sdist", "wheel")
        else:
            path = entry["path"]
            formats = entry.get("format", ("sdist", "wheel"))
            formats = (formats,) if isinstance(formats, str) else tuple(formats)
        if form in formats:
            out.append(glob_regex(path))
    return out


def named(tracked: set[str], maturin: dict, form: str) -> set[str]:
    """The tracked files that ``form``'s includes name and its excludes leave in."""
    include, exclude = patterns(maturin, "include", form), patterns(maturin, "exclude", form)
    return {p for p in tracked
            if any(r.fullmatch(p) for r in include) and not any(r.fullmatch(p) for r in exclude)}


def twice(names: list[str]) -> list[str]:
    return [f"{n} is in it {k} times" for n, k in sorted(collections.Counter(names).items()) if k > 1]


def listed(what: str, paths) -> list[str]:
    return [f"{what}: {p}" for p in sorted(paths)]


def check_wheel(path: Path, tracked: set[str], maturin: dict, tag: str | None = None,
                library: str | None = None) -> list[str]:
    """What is wrong with the wheel at ``path``: one line per difference, none when it is as it should be."""
    parts = path.name.removesuffix(".whl").split("-")
    if not path.name.endswith(".whl") or len(parts) not in (5, 6):
        return [f"{path.name}: not a wheel's file name (name-version[-build]-python-abi-platform.whl)"]
    dist_info = f"{parts[0]}-{parts[1]}.dist-info/"
    name_tag = "-".join(parts[-3:])
    try:
        archive = zipfile.ZipFile(path)
    except (OSError, zipfile.BadZipFile) as e:
        return [f"{path.name}: not a wheel ({e})"]
    with archive:
        names = archive.namelist()
        meta = archive.read(dist_info + "WHEEL").decode("utf-8") if dist_info + "WHEEL" in names else None
    problems = twice(names)
    members = set(names)

    if meta is None:
        problems.append(f"no {dist_info}WHEEL")
    else:
        tags = [line.split(":", 1)[1].strip() for line in meta.splitlines() if line.startswith("Tag:")]
        if len(tags) != 1:
            problems.append(f"expected one Tag in {dist_info}WHEEL, found {tags}")
        elif tag is not None and tags[0] != tag:
            problems.append(f"its tag is {tags[0]}, not {tag}")
        elif tag is None and not tags[0].startswith("cp311-abi3-"):
            problems.append(f"its tag is {tags[0]}, not an abi3 one for Python 3.11 and later")
        if len(tags) == 1 and name_tag != tags[0]:
            problems.append(f"its file name says {name_tag}, its WHEEL {tags[0]}")
    for needed in ("METADATA", "RECORD"):
        if dist_info + needed not in members:
            problems.append(f"no {dist_info}{needed}")

    in_package = {m for m in members if m.startswith(PACKAGE) and not m.endswith("/")}
    libraries = sorted(m for m in in_package - tracked if LIBRARY.fullmatch(m))
    if len(libraries) != 1:
        problems.append(f"expected one citar/_engine library, found {', '.join(libraries) or 'none'}")
    elif library is not None and libraries[0] != library:
        problems.append(f"its library is {libraries[0]}, not {library}")
    want = {p for p in tracked if p.startswith(PACKAGE)}
    problems += listed("missing (git tracks it under citar/)", want - in_package)
    problems += listed("stray (git does not track it)", in_package - want - set(libraries))
    # The includes under citar/ are in `want` already; this is for any that names a file elsewhere.
    problems += listed("missing (an include names it)", named(tracked, maturin, "wheel") - members - want)
    problems += listed("outside citar/ and the dist-info",
                       {m for m in members - in_package if not m.startswith(dist_info) and not m.endswith("/")})
    return problems


def check_sdist(path: Path, tracked: set[str], maturin: dict) -> list[str]:
    """What is wrong with the source distribution at ``path``: one line per difference, none when it is right."""
    if not path.name.endswith(".tar.gz"):
        return [f"{path.name}: not a source distribution's file name (name-version.tar.gz)"]
    top = path.name.removesuffix(".tar.gz")
    try:
        with tarfile.open(path, "r:gz") as archive:
            entries = archive.getmembers()
    except (OSError, tarfile.TarError) as e:
        return [f"{path.name}: not a source distribution ({e})"]
    problems, files = [], []
    for entry in entries:
        head, _, rest = entry.name.partition("/")
        if head != top:
            problems.append(f"{entry.name} is outside {top}/")
        elif entry.isfile():
            files.append(rest)
        elif not entry.isdir():
            problems.append(f"{entry.name} is neither a file nor a folder")
    problems += twice(files)
    members = set(files)
    if SDIST_OWN not in members:
        problems.append(f"no {SDIST_OWN}")
    problems += listed("stray (git does not track it)", members - tracked - {SDIST_OWN})
    want = {p for p in tracked if p.startswith(PACKAGE)}
    problems += listed("missing (git tracks it under citar/)", want - members)
    problems += listed("missing (an include names it)", named(tracked, maturin, "sdist") - members - want)
    problems += listed("missing (a build from it starts there)", set(SDIST_NEEDS) - members)
    problems += listed("missing (a build from it compiles it in)",
                       {p for p in tracked if p.startswith(SDIST_NEEDS_TREES)} - members)
    return problems


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    kinds = parser.add_subparsers(dest="kind", required=True)
    wheel = kinds.add_parser("wheel", help="check a wheel")
    wheel.add_argument("path", type=Path, help="a wheel, or a directory holding exactly one")
    wheel.add_argument("--tag", help="the one tag it must have (default: any cp311-abi3 tag)")
    wheel.add_argument("--library", help="the extension library's member name, e.g. citar/_engine.abi3.so")
    sdist = kinds.add_parser("sdist", help="check a source distribution")
    sdist.add_argument("path", type=Path, help="a .tar.gz, or a directory holding exactly one")
    args = parser.parse_args(argv)
    try:
        maturin = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))["tool"]["maturin"]
        tracked = tracked_files()
        if args.kind == "wheel":
            path = find(args.path, ".whl")
            problems = check_wheel(path, tracked, maturin, args.tag, args.library)
        else:
            path = find(args.path, ".tar.gz")
            problems = check_sdist(path, tracked, maturin)
    except Refused as e:
        print(f"check_dist: {e}", file=sys.stderr)
        return 2
    if problems:
        print(f"check_dist: {path.name} is not as the checkout says it should be:", file=sys.stderr)
        for line in problems:
            print(f"  {line}", file=sys.stderr)
        return 1
    package = sum(1 for p in tracked if p.startswith(PACKAGE))
    print(f"check_dist: {path.name} is as the checkout says: the {package} files git tracks under citar/, and the rest")
    return 0


if __name__ == "__main__":
    sys.exit(main())
