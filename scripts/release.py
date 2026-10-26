"""One version for a release: check it, bump it, and the values every release job reads (design/phase5-release.md P5.2).

Cargo's ``[workspace.package] version`` is the source. The wheel takes it (``dynamic = ["version"]``), every crate and
so every helper takes it, and ``cargo xtask check`` holds ``citar/__init__.py`` to it. Everything else that names the
release is a checked set, which this script reads and rewrites:

- ``Cargo.toml``'s ``[workspace.package] version``: the source;
- ``Cargo.lock``'s entries of the workspace members, and ``crates/citar-engine/fuzz/Cargo.lock``'s (the fuzz crate is
  outside the workspace, with its own lock, which rust.yml checks ``--locked``): equal;
- ``citar/__init__.py``'s ``__version__``: equal;
- ``installer/citar.iss``'s ``#define AppVersion`` (equal) and ``#define AppVersionNumeric`` (the four numbers Inno
  Setup's ``VersionInfoVersion`` needs, ``X.Y.Z.0``): the defaults of a hand build, which CI overrides with ``/D``;
- ``KNOWN_ISSUES.md``'s header ("in X"): the source version or newer, since it names the release being prepared;
- ``CHANGELOG.md``: one ``## [Unreleased]`` above every release; the newest release section is the source's,
  ``## [X] - YYYY-MM-DD``; the links ``[Unreleased]`` (compare ``vX...HEAD``) and ``[X]`` (compare from the release
  below it).

With ``--version V`` every place is held to V instead of the source. The commands::

    python scripts/release.py check [--version V]
    python scripts/release.py bump V --date YYYY-MM-DD
    python scripts/release.py prepare --mode ci|dry-run|tag [--version V] [--date YYYY-MM-DD]
    python scripts/release.py label --mode ci|dry-run|tag [--version V] [--sha SHA]
    python scripts/release.py meta --mode ci|dry-run|tag [--version V] [--sha SHA] [--date D] [--github-output FILE]
    python scripts/release.py notes V [--out FILE]
    python scripts/release.py assets V [--signed] [--json]

- ``check`` names every place out of step with its file and line.
- ``bump`` refuses a version that is not newer than the source, a tree that ``check`` fails and an empty Unreleased
  section; it rewrites the set (only the version in each place, each file's line endings kept), turns
  ``## [Unreleased]`` into ``## [V] - date`` under a new empty ``## [Unreleased]`` with the two compare links, then runs
  ``check --version V``. It replaces the hand bump of 0.1.5's procedure.
- ``prepare`` is the first step of every job that builds or tests: ``ci`` checks; ``tag`` checks against V; ``dry-run``
  bumps the runner's checkout to V when the source differs (a notice says so: the bump stays in that runner), then
  checks. So a dry run of a branch at 0.1.5 builds what a tag of the bumped branch would.
- ``label`` is ``CITAR_BUILD_ID`` for a release build: ``vV`` on a tag, ``vV-dryrun+<sha7>`` on a dry run, ``ci+<sha7>``
  otherwise.
- ``meta`` refuses a version that is not ``X.Y.Z`` or ``X.Y.Z-(alpha|beta|rc).N`` before any job uses it, and prints
  (and with ``--github-output`` appends to that file) ``version``, ``pep440`` (maturin's mapping: ``-rc.1`` is
  ``rc1``), ``numeric``, ``label``, ``date`` (one per run, so jobs either side of midnight agree), ``mode`` and
  ``prerelease``. In ``ci`` mode the version is the source's.
- ``notes`` prints V's CHANGELOG section byte for byte (the text after its heading, up to the next ``## `` heading or
  the link definitions at the end), which is what release.yml's awk printed.
- ``assets`` prints the release's file names (P5.10), the one list every job and check compares with: each file then
  its ``.sha256``, ``SHA256SUMS`` last; ``--signed`` adds the manifest's signature (a signed tag's); ``--json`` gives
  them with their groups.

Every command takes ``--root DIR``, the checkout (by default the one this script is in). Standard library only, so a
job runs it before installing anything, on Python 3.11 or newer (the package's own floor; ``tomllib`` reads the
manifests): a job runs it after ``actions/setup-python``, as ``python`` on Windows, and inside a manylinux container
as ``/opt/python/cp311-cp311/bin/python``, never the image's system ``python3``. An older interpreter (Ubuntu 22.04's
``python3`` is 3.10) is refused with that reason, before anything else is read. Exit status: 0 done; 1 a place out of
step, or a version, bump or section refused (each named); 2 a usage error, an older Python among them.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

#: The oldest Python this script runs on: ``tomllib`` came in 3.11. The whole file keeps to Python 3.8's grammar, so an
#: older interpreter compiles it and reaches this refusal instead of dying on an ImportError's traceback (tests hold
#: both).
OLDEST_PYTHON = (3, 11)
if sys.version_info < OLDEST_PYTHON:
    print(f"release.py needs Python {'.'.join(map(str, OLDEST_PYTHON))} or newer (it reads TOML with tomllib); this "
          f"is Python {'.'.join(map(str, sys.version_info[:3]))}", file=sys.stderr)
    sys.exit(2)

import tomllib  # after the version check, which names what an older Python lacks

ROOT = Path(__file__).resolve().parent.parent
REPO_URL = "https://github.com/jprodgers/CITAR"

SOURCE = "Cargo.toml"
LOCK = "Cargo.lock"
FUZZ_LOCK = "crates/citar-engine/fuzz/Cargo.lock"
PYTHON = "citar/__init__.py"
INSTALLER = "installer/citar.iss"
KNOWN_ISSUES = "KNOWN_ISSUES.md"
CHANGELOG = "CHANGELOG.md"

#: A release's version: semver's three numbers without leading zeros, and optionally ``-alpha.N``, ``-beta.N`` or
#: ``-rc.N``. Anything else is refused before a job uses it (it reaches file names, tags and shells).
VERSION = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(alpha|beta|rc)\.(0|[1-9][0-9]*))?", re.ASCII)
#: How semver orders the prerelease kinds, and how PEP 440 (and so maturin, naming the wheel) spells them.
PRERELEASES = {"alpha": (0, "a"), "beta": (1, "b"), "rc": (2, "rc")}
SHA = re.compile(r"[0-9a-f]{7,40}", re.ASCII)
DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", re.ASCII)
MODES = ("ci", "dry-run", "tag")

#: The wheels' platform tags (P5.3.2) and the helper's platforms (P4.10's unversioned names), in P5.10's order.
WHEEL_TAGS = ("win_amd64", "macosx_11_0_arm64", "macosx_11_0_x86_64", "manylinux_2_28_x86_64", "manylinux_2_28_aarch64")
HELPER_PLATFORMS = ("windows-x64", "macos-arm64", "macos-x64", "linux-x64", "linux-arm64")


class Refused(Exception):
    """A request this script will not carry out; the message says why."""


class Disagrees(Exception):
    """The checked set is out of step; each problem names its file and line."""

    def __init__(self, problems: list[str]):
        super().__init__(f"{len(problems)} problem(s)")
        self.problems = problems


@dataclass(frozen=True)
class Version:
    text: str
    major: int
    minor: int
    patch: int
    pre: str | None
    number: int | None

    @classmethod
    def parse(cls, text: str) -> Version:
        m = VERSION.fullmatch(text) if isinstance(text, str) else None
        if not m:
            raise Refused(f"{text!r} is not a release version: X.Y.Z, or X.Y.Z-alpha.N, -beta.N or -rc.N")
        number = int(m[5]) if m[5] is not None else None
        return cls(text, int(m[1]), int(m[2]), int(m[3]), m[4], number)

    def key(self) -> tuple:
        """Semver's order: a prerelease comes before its release, alpha before beta before rc."""
        if self.pre is None:
            return (self.major, self.minor, self.patch, 1, 0, 0)
        return (self.major, self.minor, self.patch, 0, PRERELEASES[self.pre][0], self.number)

    def newer_than(self, other: Version) -> bool:
        return self.key() > other.key()

    @property
    def pep440(self) -> str:
        if self.pre is None:
            return self.text
        return f"{self.major}.{self.minor}.{self.patch}{PRERELEASES[self.pre][1]}{self.number}"

    @property
    def numeric(self) -> str:
        """Inno Setup's four numbers (``VersionInfoVersion`` takes no prerelease): the prerelease part is dropped."""
        return f"{self.major}.{self.minor}.{self.patch}.0"

    @property
    def prerelease(self) -> bool:
        return self.pre is not None


# ------------------------------------------------------------------------------------------------ files and places


class Text:
    """A text file as lines that keep their own endings, so a rewrite touches only the value it means to."""

    def __init__(self, root: Path, rel: str):
        self.rel = rel
        self.path = root / rel
        with open(self.path, encoding="utf-8", newline="") as f:
            data = f.read()
        parts = data.split("\n")
        self.lines = [p + "\n" for p in parts[:-1]] + ([parts[-1]] if parts[-1] else [])
        self.original = data

    def body(self, i: int) -> str:
        line = self.lines[i]
        for end in ("\r\n", "\n"):
            if line.endswith(end):
                return line[: -len(end)]
        return line

    def ending(self, i: int) -> str:
        line = self.lines[i]
        return line[len(self.body(i)):] or "\n"

    def bodies(self):
        return (self.body(i) for i in range(len(self.lines)))

    def set_body(self, i: int, body: str) -> None:
        self.lines[i] = body + self.lines[i][len(self.body(i)):]

    def insert(self, i: int, bodies: list[str]) -> None:
        """Insert lines before line ``i``, ending as line ``i - 1`` does (which gets an ending if it had none)."""
        end = self.ending(max(i - 1, 0))
        if i > 0 and self.body(i - 1) == self.lines[i - 1]:
            self.lines[i - 1] += end
        self.lines[i:i] = [b + end for b in bodies]

    def text(self) -> str:
        return "".join(self.lines)

    def changed(self) -> bool:
        return self.text() != self.original

    def save(self) -> None:
        with open(self.path, "w", encoding="utf-8", newline="") as f:
            f.write(self.text())


@dataclass
class Place:
    """One version in the checked set: where it is and what rule holds it."""
    file: Text
    index: int
    start: int
    end: int
    what: str
    rule: str  # "equal", "numeric" (X.Y.Z.0 of the version) or "at-least" (the version or newer)

    @property
    def value(self) -> str:
        return self.file.body(self.index)[self.start:self.end]

    @property
    def where(self) -> str:
        return f"{self.file.rel}:{self.index + 1}"

    def rewrite(self, new: str) -> None:
        body = self.file.body(self.index)
        self.file.set_body(self.index, body[:self.start] + new + body[self.end:])
        self.end = self.start + len(new)


class Tree:
    """The checked set as one checkout holds it: every place, and the problems found reading it."""

    def __init__(self, root: Path):
        self.root = root
        self.files: dict[str, Text] = {}
        self.problems: list[str] = []
        self.places: list[Place] = []
        self.source: Place | None = None
        self.changelog: Text | None = None
        self._read()

    def _open(self, rel: str) -> Text | None:
        try:
            self.files[rel] = Text(self.root, rel)
        except FileNotFoundError:
            self.problems.append(f"{rel}: missing")
            return None
        except (OSError, UnicodeDecodeError) as e:
            self.problems.append(f"{rel}: cannot be read ({e})")
            return None
        return self.files[rel]

    def _read(self) -> None:
        cargo = self._open(SOURCE)
        members: list[str] = []
        if cargo is not None:
            self.source = self._source(cargo)
            members = self._members(cargo)
        for rel, label in ((LOCK, "the lock"), (FUZZ_LOCK, "the fuzz crate's lock")):
            text = self._open(rel)
            if text is not None:
                self._lock(text, members, label, every_member=rel == LOCK)
        text = self._open(PYTHON)
        if text is not None:
            self._one(text, r'__version__\s*=\s*"([^"]*)"\s*', "__version__", "equal")
        text = self._open(INSTALLER)
        if text is not None:
            self._one(text, r'\s*#define\s+AppVersion\s+"([^"]*)"\s*', "#define AppVersion", "equal")
            self._one(text, r'\s*#define\s+AppVersionNumeric\s+"([^"]*)"\s*', "#define AppVersionNumeric", "numeric")
        text = self._open(KNOWN_ISSUES)
        if text is not None:
            self._known_issues(text)
        self.changelog = self._open(CHANGELOG)

    def _source(self, cargo: Text) -> Place | None:
        table = None
        found = None
        for i, body in enumerate(cargo.bodies()):
            stripped = body.strip()
            if stripped.startswith("["):
                table = stripped.split("#", 1)[0].strip()
                continue
            m = re.fullmatch(r'\s*version\s*=\s*"([^"]*)"\s*(?:#.*)?', body)
            if table == "[workspace.package]" and m:
                found = Place(cargo, i, m.start(1), m.end(1), "[workspace.package] version (the source)", "equal")
                break
        if found is None:
            self.problems.append(f'{SOURCE}: no `version = "..."` line in [workspace.package]')
            return None
        try:
            parsed = tomllib.loads(cargo.original)["workspace"]["package"]["version"]
        except (tomllib.TOMLDecodeError, KeyError, TypeError) as e:
            self.problems.append(f"{SOURCE}: [workspace.package] version cannot be read as TOML ({e})")
            return None
        if parsed != found.value:
            self.problems.append(f"{found.where}: this line says {found.value}, but the TOML's version is {parsed}")
            return None
        self.places.append(found)
        return found

    def _members(self, cargo: Text) -> list[str]:
        """The workspace members' package names, read from their manifests."""
        try:
            paths = tomllib.loads(cargo.original)["workspace"]["members"]
        except (tomllib.TOMLDecodeError, KeyError, TypeError) as e:
            self.problems.append(f"{SOURCE}: [workspace] members cannot be read ({e})")
            return []
        names = []
        for member in paths:
            manifest = f"{member}/Cargo.toml"
            try:
                names.append(tomllib.loads((self.root / manifest).read_text(encoding="utf-8"))["package"]["name"])
            except (OSError, tomllib.TOMLDecodeError, KeyError, TypeError) as e:
                self.problems.append(f"{manifest}: the member's package name cannot be read ({e})")
        return names

    def _lock(self, text: Text, members: list[str], label: str, every_member: bool) -> None:
        """The version lines of the members' entries: a member is a path package, so its entry has no ``source``."""
        entries: list[dict] = []
        entry = None
        for i, body in enumerate(text.bodies()):
            stripped = body.strip()
            if stripped == "[[package]]":
                entry = {"name": None, "version": None, "source": False}
                entries.append(entry)
            elif stripped.startswith("["):
                entry = None
            elif entry is not None:
                m = re.fullmatch(r'(name|version|source)\s*=\s*"([^"]*)"', stripped)
                if m and m[1] == "version":
                    start = body.index('"') + 1
                    entry["version"] = (i, start, start + len(m[2]))
                elif m and m[1] == "name":
                    entry["name"] = m[2]
                elif m:
                    entry["source"] = True
        seen: dict[str, int] = {}
        for e in entries:
            if e["source"] or e["name"] not in members:
                continue
            seen[e["name"]] = seen.get(e["name"], 0) + 1
            if e["version"] is None:
                self.problems.append(f"{text.rel}: the entry of {e['name']} has no version line")
                continue
            i, start, end = e["version"]
            self.places.append(Place(text, i, start, end, f"{e['name']} in {label}", "equal"))
        for name, count in seen.items():
            if count > 1:
                self.problems.append(f"{text.rel}: {count} entries for the workspace member {name}")
        if every_member:
            for name in members:
                if name not in seen:
                    self.problems.append(f"{text.rel}: no entry for the workspace member {name} "
                                         "(run cargo to update it)")

    def _one(self, text: Text, pattern: str, what: str, rule: str) -> None:
        found = [(i, m) for i, body in enumerate(text.bodies()) if (m := re.fullmatch(pattern, body))]
        if len(found) != 1:
            lines = ", ".join(str(i + 1) for i, _ in found) or "none"
            self.problems.append(f"{text.rel}: expected one {what} line, found {len(found)} (lines: {lines})")
            return
        i, m = found[0]
        self.places.append(Place(text, i, m.start(1), m.end(1), what, rule))

    def _known_issues(self, text: Text) -> None:
        """"in X" in the header, which ends at the first ``---``: the release this file describes."""
        pattern = re.compile(r"\bin ([0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[0-9]+)?)\b")
        found = []
        for i, body in enumerate(text.bodies()):
            if body.strip() == "---":
                break
            found += [(i, m) for m in pattern.finditer(body)]
        if len(found) != 1:
            self.problems.append(f"{text.rel}: expected one 'in X.Y.Z' in the header above the first ---, "
                                 f"found {len(found)}")
            return
        i, m = found[0]
        self.places.append(Place(text, i, m.start(1), m.end(1), "the header's release", "at-least"))


# ------------------------------------------------------------------------------------------------ the changelog

HEADING = re.compile(r"## \[([^\]]+)\](?: - (.+?))?\s*")
LINK = re.compile(r"\[([^\]]+)\]: (\S+)\s*")


@dataclass(frozen=True)
class Section:
    name: str
    date: str | None
    index: int


def sections(text: Text) -> tuple[list[Section], list[str]]:
    found, problems = [], []
    for i, body in enumerate(text.bodies()):
        if not body.startswith("## ["):
            continue
        m = HEADING.fullmatch(body)
        if not m:
            problems.append(f"{text.rel}:{i + 1}: a release heading this script cannot read: {body!r}")
            continue
        found.append(Section(m[1], m[2], i))
    return found, problems


def links(text: Text) -> dict[str, list[tuple[int, str]]]:
    out: dict[str, list[tuple[int, str]]] = {}
    for i, body in enumerate(text.bodies()):
        m = LINK.fullmatch(body)
        if m:
            out.setdefault(m[1], []).append((i, m[2]))
    return out


def compare_url(older: str | None, newer: str) -> str:
    return f"{REPO_URL}/compare/v{older}...v{newer}" if older else f"{REPO_URL}/releases/tag/v{newer}"


def check_changelog(text: Text, release: str) -> list[str]:
    """The changelog agrees with ``release``: its section is the newest, dated, with both compare links."""
    rel = text.rel
    found, problems = sections(text)
    unreleased = [s for s in found if s.name == "Unreleased"]
    versions = [s for s in found if s.name != "Unreleased"]
    if len(unreleased) != 1:
        where = ", ".join(str(s.index + 1) for s in unreleased) or "none"
        problems.append(f"{rel}: expected one '## [Unreleased]', found {len(unreleased)} (lines: {where})")
    elif versions and versions[0].index < unreleased[0].index:
        problems.append(f"{rel}:{unreleased[0].index + 1}: '## [Unreleased]' must come before every release's section "
                        f"(## [{versions[0].name}] is at line {versions[0].index + 1})")
    names = [s.name for s in versions]
    for name in sorted({n for n in names if names.count(n) > 1}):
        problems.append(f"{rel}: {names.count(name)} sections named [{name}]")
    mine = [s for s in versions if s.name == release]
    if not versions or versions[0].name != release:
        if versions:
            problems.append(f"{rel}:{versions[0].index + 1}: the newest release section is [{versions[0].name}], "
                            f"but the release is {release}")
        if not mine:
            line = f":{unreleased[0].index + 1}" if unreleased else ""
            problems.append(f"{rel}{line}: no '## [{release}] - YYYY-MM-DD' section for the release")
    if mine:
        section = mine[0]
        if section.date is None or not valid_date(section.date):
            problems.append(f"{rel}:{section.index + 1}: '## [{release}]' needs its date, ' - YYYY-MM-DD'")
    defined = links(text)
    for label, where in defined.items():
        if len(where) > 1 and (label == "Unreleased" or VERSION.fullmatch(label)):
            problems.append(f"{rel}: {len(where)} '[{label}]:' links (lines "
                            f"{', '.join(str(i + 1) for i, _ in where)})")
    expected = {"Unreleased": f"{REPO_URL}/compare/v{release}...HEAD"}
    if mine:
        position = versions.index(mine[0])
        older = versions[position + 1].name if position + 1 < len(versions) else None
        expected[release] = compare_url(older, release)
    else:
        expected[release] = None
    for label, url in expected.items():
        if label not in defined:
            problems.append(f"{rel}: no '[{label}]: ...' link" + (f" (expected {url})" if url else ""))
        elif url is not None and defined[label][0][1] != url:
            i, actual = defined[label][0]
            problems.append(f"{rel}:{i + 1}: [{label}] links to {actual}, expected {url}")
    return problems


def valid_date(text: str) -> bool:
    if not DATE.fullmatch(text):
        return False
    try:
        datetime.date.fromisoformat(text)
    except ValueError:
        return False
    return True


def section_lines(text: Text, version: str) -> tuple[int, int] | None:
    """The line range of ``version``'s section: after its heading, up to the next ``## `` heading, or to the link
    definitions at the end of the file (the last section's)."""
    found, _ = sections(text)
    mine = [s for s in found if s.name == version]
    if not mine:
        return None
    start = mine[0].index + 1
    end = len(text.lines)
    for i in range(start, len(text.lines)):
        if text.lines[i].startswith("## "):
            end = i
            break
    tail = len(text.lines)
    while tail > start and (LINK.fullmatch(text.body(tail - 1)) or not text.body(tail - 1).strip()):
        tail -= 1
    # The trailing block of link definitions and blank lines starts after ``tail``; it ends the last section at its
    # first link, so the blank line before it stays the section's, as a middle section keeps the one before the next
    # heading.
    if end == len(text.lines):
        for i in range(tail, len(text.lines)):
            if LINK.fullmatch(text.body(i)):
                end = i
                break
    return start, end


# ------------------------------------------------------------------------------------------------ the commands


def check(root: Path, version: str | None = None) -> tuple[Tree, str]:
    """Every place of the checked set agrees with the source, or with ``version`` when given; raises Disagrees."""
    tree = Tree(root)
    problems = list(tree.problems)
    if tree.source is None:
        raise Disagrees(problems)
    if version is not None:
        target = Version.parse(version)
    else:
        try:
            target = Version.parse(tree.source.value)
        except Refused as e:
            raise Disagrees([*problems, f"{tree.source.where}: the source's version {e}"]) from e
    origin = "the release" if version is not None else f"the source ({tree.source.where})"
    for place in tree.places:
        value = place.value
        if place.rule == "equal" and value != target.text:
            problems.append(f"{place.where}: {place.what} is {value}, but {origin} is {target.text}")
        elif place.rule == "numeric" and value != target.numeric:
            problems.append(f"{place.where}: {place.what} is {value}, but {origin} {target.text} needs "
                            f"{target.numeric}")
        elif place.rule == "at-least":
            try:
                older = target.newer_than(Version.parse(value))
            except Refused:
                problems.append(f"{place.where}: {place.what} is {value}, which is not a release version")
                continue
            if older:
                problems.append(f"{place.where}: {place.what} is {value}, older than {origin}, {target.text}")
    if tree.changelog is not None:
        problems += check_changelog(tree.changelog, target.text)
    if problems:
        raise Disagrees(problems)
    return tree, target.text


def bump(root: Path, version: str, date: str, out=print) -> tuple[Tree, str]:
    """Move every place of the checked set to ``version``, turn the Unreleased section into its section, and check
    the result against ``version``."""
    new = Version.parse(version)
    if not valid_date(date):
        raise Refused(f"{date!r} is not a date (YYYY-MM-DD)")
    try:
        tree, current = check(root)
    except Disagrees as e:
        raise Refused("the tree is out of step before the bump; fix it first:\n  " + "\n  ".join(e.problems)) from e
    if not new.newer_than(Version.parse(current)):
        raise Refused(f"{version} is not newer than the source's {current}")
    log = tree.changelog
    found, _ = sections(log)
    unreleased = next(s for s in found if s.name == "Unreleased")
    start, end = section_lines(log, "Unreleased")
    if not "".join(log.lines[start:end]).strip():
        raise Refused(f"{CHANGELOG}:{unreleased.index + 1}: the Unreleased section is empty: nothing to release")
    for place in tree.places:
        old = place.value
        if place.rule == "equal":
            place.rewrite(version)
        elif place.rule == "numeric":
            place.rewrite(new.numeric)
        elif place.rule == "at-least" and new.newer_than(Version.parse(old)):
            place.rewrite(version)
        if place.value != old:
            out(f"{place.where}: {place.what} {old} -> {place.value}")
    link_index = links(log)["Unreleased"][0][0]
    log.set_body(link_index, f"[Unreleased]: {REPO_URL}/compare/v{version}...HEAD")
    log.insert(link_index + 1, [f"[{version}]: {compare_url(current, version)}"])
    log.insert(unreleased.index + 1, ["", f"## [{version}] - {date}"])
    out(f"{CHANGELOG}: [Unreleased] is now ## [{version}] - {date}, under a new empty ## [Unreleased], "
        f"with the links [Unreleased] and [{version}]")
    for text in tree.files.values():
        if text.changed():
            text.save()
    return check(root, version)


def prepare(root: Path, mode: str, version: str | None, date: str | None, out=print) -> str:
    """The first step of a job that builds or tests; returns the version the checkout now holds."""
    if mode in ("tag", "dry-run") and version is None:
        raise Refused(f"prepare --mode {mode} needs --version")
    if version is not None:
        Version.parse(version)
    if mode == "dry-run":
        tree = Tree(root)
        if tree.source is not None and tree.source.value != version:
            source = tree.source.value
            bump(root, version, date or today(), out=out)
            notice = (f"bumped this runner's checkout from {source} to {version} for the dry run: the bump is in this "
                      f"runner only, and nothing is committed")
            out(f"release.py prepare: {notice}")
            if os.environ.get("GITHUB_ACTIONS") == "true":
                out(f"::notice title=release.py prepare::{notice}")
    _, target = check(root, version)
    out(f"release.py prepare --mode {mode}: every place says {target}")
    return target


def label(mode: str, version: str | None, sha: str | None) -> str:
    if mode not in MODES:
        raise Refused(f"unknown mode {mode!r} (ci, dry-run or tag)")
    if mode in ("tag", "dry-run"):
        if version is None:
            raise Refused(f"a {mode} label needs --version")
        Version.parse(version)
    if mode in ("dry-run", "ci"):
        if sha is None or not SHA.fullmatch(sha.lower()):
            given = f", not {sha!r}" if sha is not None else ""
            raise Refused(f"a {mode} label needs --sha, the commit's hex SHA (7 to 40 digits){given}")
    if mode == "tag":
        return f"v{version}"
    if mode == "dry-run":
        return f"v{version}-dryrun+{sha.lower()[:7]}"
    return f"ci+{sha.lower()[:7]}"


def meta(root: Path, mode: str, version: str | None, sha: str | None, date: str | None) -> dict[str, str]:
    if mode == "ci" and version is None:
        tree = Tree(root)
        if tree.source is None:
            raise Refused("the source version cannot be read:\n  " + "\n  ".join(tree.problems))
        version = tree.source.value
    v = Version.parse(version) if version is not None else None
    if date is not None and not valid_date(date):
        raise Refused(f"{date!r} is not a date (YYYY-MM-DD)")
    the_label = label(mode, version, sha)
    return {
        "version": v.text,
        "pep440": v.pep440,
        "numeric": v.numeric,
        "label": the_label,
        "date": date or today(),
        "mode": mode,
        "prerelease": "true" if v.prerelease else "false",
    }


def notes(root: Path, version: str) -> str:
    Version.parse(version)
    text = Text(root, CHANGELOG)
    span = section_lines(text, version)
    if span is None:
        raise Refused(f"{CHANGELOG} has no section '## [{version}]'")
    body = "".join(text.lines[span[0]:span[1]])
    if not body.strip():
        raise Refused(f"{CHANGELOG}:{span[0]}: the section of {version} is empty")
    return body


def assets(version: str, signed: bool = False) -> list[dict[str, str]]:
    """P5.10's release assets, in its order, each with its group; every one has a ``.sha256`` beside it."""
    v = Version.parse(version)
    p = v.pep440
    groups = [
        ("python", [f"citar-{p}.tar.gz", *(f"citar-{p}-cp311-abi3-{tag}.whl" for tag in WHEEL_TAGS),
                    f"citar-{p}-server-constraints.txt"]),
        ("windows", [f"CITAR-{version}-setup.exe", f"CITAR-{version}-windows-x64.zip"]),
        ("helper", [*(f"citar-helper-{platform}" + (".exe" if platform.startswith("windows") else "")
                      for platform in HELPER_PLATFORMS),
                    "citar-helper-manifest.json", *(["citar-helper-manifest.json.sig"] if signed else [])]),
        ("helper-packaged", [f"citar-helper-{version}-{platform}" + (".zip" if platform.startswith("windows")
                                                                     else ".tar.gz")
                             for platform in HELPER_PLATFORMS]),
        ("notices", ["THIRD-PARTY-LICENSES.txt"]),
    ]
    return [{"name": name, "group": group, "sha256": f"{name}.sha256"} for group, names in groups for name in names]


def asset_files(version: str, signed: bool = False) -> list[str]:
    """Every file a release holds: each asset then its ``.sha256``, and ``SHA256SUMS`` last."""
    files = []
    for asset in assets(version, signed):
        files += [asset["name"], asset["sha256"]]
    return [*files, "SHA256SUMS"]


def today() -> str:
    return datetime.datetime.now(datetime.timezone.utc).date().isoformat()


# ------------------------------------------------------------------------------------------------ command line


def parser() -> argparse.ArgumentParser:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", type=Path, default=ROOT, help="the checkout (default: this script's)")
    top = argparse.ArgumentParser(prog="release.py", description=__doc__.split("\n\n")[0])
    sub = top.add_subparsers(dest="command", required=True)
    p = sub.add_parser("check", parents=[common], help="every place the version lives agrees")
    p.add_argument("--version", help="hold every place to this version instead of the source")
    p = sub.add_parser("bump", parents=[common], help="move every place to a new version")
    p.add_argument("version")
    p.add_argument("--date", required=True, help="the release's date, YYYY-MM-DD")
    p = sub.add_parser("prepare", parents=[common], help="the first step of a release job")
    p.add_argument("--mode", required=True, choices=MODES)
    p.add_argument("--version")
    p.add_argument("--date", help="a dry run's bump date (default: today, UTC)")
    p = sub.add_parser("label", parents=[common], help="CITAR_BUILD_ID for a release build")
    p.add_argument("--mode", required=True, choices=MODES)
    p.add_argument("--version")
    p.add_argument("--sha")
    p = sub.add_parser("meta", parents=[common], help="the values every release job reads")
    p.add_argument("--mode", required=True, choices=MODES)
    p.add_argument("--version")
    p.add_argument("--sha")
    p.add_argument("--date")
    p.add_argument("--github-output", type=Path, help="also append the values to this file ($GITHUB_OUTPUT)")
    p = sub.add_parser("notes", parents=[common], help="a release's CHANGELOG section, byte for byte")
    p.add_argument("version")
    p.add_argument("--out", type=Path, help="write the section to this file instead of standard output")
    p = sub.add_parser("assets", parents=[common], help="the release's file names")
    p.add_argument("version")
    p.add_argument("--signed", action="store_true", help="a signed tag's list, with the manifest's signature")
    p.add_argument("--json", action="store_true")
    return top


def main(argv: list[str] | None = None) -> int:
    for stream in (sys.stdout, sys.stderr):
        # A file's text in a message must not fail on a console whose code page lacks a character.
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="backslashreplace")
    if hasattr(sys.stdout, "reconfigure"):
        # What it prints is data that a shell compares with files written with "\n" (RELEASING.md step 3 diffs
        # `assets` against the preview's subjects.txt), so every line ends in "\n" on Windows too, as `notes`, which
        # writes bytes, already did; Python's default there is "\r\n", and the diff failed in Git Bash (5-13).
        sys.stdout.reconfigure(newline="\n")
    args = parser().parse_args(argv)
    root = args.root.resolve()
    try:
        if args.command == "check":
            tree, target = check(root, args.version)
            print(f"release.py check: {len(tree.places)} places and the changelog agree on {target}")
        elif args.command == "bump":
            tree, target = bump(root, args.version, args.date)
            print(f"release.py bump: {len(tree.places)} places and the changelog now say {target}")
        elif args.command == "prepare":
            prepare(root, args.mode, args.version, args.date)
        elif args.command == "label":
            print(label(args.mode, args.version, args.sha))
        elif args.command == "meta":
            values = meta(root, args.mode, args.version, args.sha, args.date)
            lines = "".join(f"{k}={v}\n" for k, v in values.items())
            sys.stdout.write(lines)
            if args.github_output is not None:
                with open(args.github_output, "a", encoding="utf-8", newline="\n") as f:
                    f.write(lines)
        elif args.command == "notes":
            body = notes(root, args.version).encode("utf-8")
            if args.out is not None:
                args.out.write_bytes(body)
            else:
                sys.stdout.flush()
                sys.stdout.buffer.write(body)
                sys.stdout.buffer.flush()
        elif args.command == "assets":
            if args.json:
                v = Version.parse(args.version)
                print(json.dumps({"version": v.text, "pep440": v.pep440, "signed": args.signed,
                                  "assets": assets(args.version, args.signed),
                                  "files": asset_files(args.version, args.signed)}, indent=2))
            else:
                print("\n".join(asset_files(args.version, args.signed)))
    except Disagrees as e:
        for problem in e.problems:
            print(problem, file=sys.stderr)
        print(f"release.py {args.command}: {len(e.problems)} problem(s): the places above are out of step",
              file=sys.stderr)
        return 1
    except Refused as e:
        print(f"release.py {args.command}: refused: {e}", file=sys.stderr)
        return 1
    except OSError as e:
        print(f"release.py {args.command}: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
