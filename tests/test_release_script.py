"""scripts/release.py: one version for a release (design/phase5-release.md P5.2).

``check`` against the tree and against each place planted out of step; ``bump`` in a temporary copy of the checked
files (every place moves and nothing else, the installer script keeps its CRLF); ``prepare``'s three modes; ``notes``
against release.yml's awk; ``label``, ``meta`` and the version rule; ``assets`` against P5.10's table.

Nothing here names the tree's version: a release dry run bumps the runner's checkout before the suite runs (P5.2's
``prepare --mode dry-run``), so the tests read the version from the tree (``SOURCE``) and bump past it. A dry run or a
tag may name a prerelease (``-f version=0.1.6-rc.1``), so the copy tests also run from a copy first bumped to one (the
``...FromAPrerelease`` classes): a prerelease's next release shares its ``X.Y.Z``, and so the installer's numeric
version.
"""
import ast
import contextlib
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "release.py"

spec = importlib.util.spec_from_file_location("release_script", SCRIPT)
release = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = release  # dataclasses look their module up while the class is made
spec.loader.exec_module(release)

#: The files the checked set reads, besides each workspace member's manifest.
CHECKED = ("Cargo.toml", "Cargo.lock", "crates/citar-engine/fuzz/Cargo.toml", "crates/citar-engine/fuzz/Cargo.lock",
           "citar/__init__.py", "installer/citar.iss", "KNOWN_ISSUES.md", "CHANGELOG.md")
DATE = "2026-11-01"
#: The date of the prerelease the ``...FromAPrerelease`` copies start from.
RC_DATE = "2026-10-25"
#: A version older than any the tree will hold, so planting it puts any place out of step.
OLD = "0.0.1"


def _next(version: str) -> str:
    """The next release after ``version``: its release when it is a prerelease, else the next patch."""
    v = release.Version.parse(version)
    return f"{v.major}.{v.minor}.{v.patch + (0 if v.prerelease else 1)}"


def _after(version: str) -> str:
    """The next patch after ``version``'s numbers: newer than ``version`` whether or not it is a prerelease."""
    v = release.Version.parse(version)
    return f"{v.major}.{v.minor}.{v.patch + 1}"


def _below(root: Path, version: str) -> str:
    """The release below ``version`` in ``root``'s changelog."""
    found = [s.name for s in release.sections(release.Text(root, "CHANGELOG.md"))[0] if s.name != "Unreleased"]
    return found[found.index(version) + 1]


#: The tree's version. The copy tests read theirs from the copy (``CopyTest.source``), which may be a prerelease.
SOURCE = release.Tree(ROOT).source.value

#: release.yml's notes step at 51ce329 (`:380-384`), the program between its single quotes as the shell hands it to
#: awk. 5-02a reduces release.yml to its gate, so the program is kept here, where `notes` is held to it.
NOTES_AWK = r'''
            $0 ~ "^## \\[?" v "\\]?" { found = 1; next }
            found && /^## / { exit }
            found { print }
          '''

#: P5.10's asset table for 0.1.6, in its order.
ASSETS_016 = [
    "citar-0.1.6.tar.gz",
    "citar-0.1.6-cp311-abi3-win_amd64.whl",
    "citar-0.1.6-cp311-abi3-macosx_11_0_arm64.whl",
    "citar-0.1.6-cp311-abi3-macosx_11_0_x86_64.whl",
    "citar-0.1.6-cp311-abi3-manylinux_2_28_x86_64.whl",
    "citar-0.1.6-cp311-abi3-manylinux_2_28_aarch64.whl",
    "citar-0.1.6-server-constraints.txt",
    "CITAR-0.1.6-setup.exe",
    "CITAR-0.1.6-windows-x64.zip",
    "citar-helper-windows-x64.exe",
    "citar-helper-macos-arm64",
    "citar-helper-macos-x64",
    "citar-helper-linux-x64",
    "citar-helper-linux-arm64",
    "citar-helper-manifest.json",
    "citar-helper-0.1.6-windows-x64.zip",
    "citar-helper-0.1.6-macos-arm64.tar.gz",
    "citar-helper-0.1.6-macos-x64.tar.gz",
    "citar-helper-0.1.6-linux-x64.tar.gz",
    "citar-helper-0.1.6-linux-arm64.tar.gz",
    "THIRD-PARTY-LICENSES.txt",
]


def quiet(fn, *args, **kwargs):
    """Call ``fn`` with its output captured; returns (result, output)."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
        result = fn(*args, **kwargs)
    return result, out.getvalue()


def run_main(*argv):
    """``release.main`` with its output captured: (exit status, output)."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
        try:
            code = release.main(list(argv))
        except SystemExit as e:
            code = e.code
    return code, out.getvalue()


def lines_of(text: str) -> list[str]:
    return text.split("\n")


class Copy:
    """A temporary copy of the checked files, byte for byte (line endings included). When the changelog's Unreleased
    section is empty (as just after a bump), it gets one line, so the copy has something to release."""

    def __init__(self, tmp: Path):
        self.root = tmp
        members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
        for rel in (*CHECKED, *(f"{m}/Cargo.toml" for m in members)):
            (tmp / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / rel, tmp / rel)
        self._something_to_release()
        self.before = self.snapshot()

    def _something_to_release(self) -> None:
        log = release.Text(self.root, "CHANGELOG.md")
        start, end = release.section_lines(log, "Unreleased")
        if not "".join(log.lines[start:end]).strip():
            self.write("CHANGELOG.md", self.read("CHANGELOG.md").replace(
                "## [Unreleased]\n", "## [Unreleased]\n\n- A change for the tests.\n", 1))

    def bump_to(self, version: str, date: str = RC_DATE) -> None:
        """Bump the copy to ``version`` and take that as where it starts (``before``), with a line to release."""
        quiet(release.bump, self.root, version, date, out=print)
        self._something_to_release()
        self.before = self.snapshot()

    def snapshot(self) -> dict[str, bytes]:
        return {p.relative_to(self.root).as_posix(): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}

    def read(self, rel: str) -> str:
        return (self.root / rel).read_bytes().decode("utf-8")

    def write(self, rel: str, text: str) -> None:
        (self.root / rel).write_bytes(text.encode("utf-8"))

    def original(self, rel: str) -> str:
        return self.before[rel].decode("utf-8")

    def changed(self) -> dict[str, tuple[list[str], list[str]]]:
        """Each changed file's removed and added lines (by a line-set difference that keeps order)."""
        out = {}
        for rel, data in self.snapshot().items():
            if data != self.before.get(rel):
                old = lines_of(self.before.get(rel, b"").decode("utf-8"))
                new = lines_of(data.decode("utf-8"))
                out[rel] = ([line for line in old if line not in new], [line for line in new if line not in old])
        return out


class CopyTest(unittest.TestCase):
    """A test on a copy of the checked files. Its versions are the copy's: ``source`` (the tree's, or with
    ``prerelease`` set the prerelease the copy was first bumped to), ``next`` (the release after it: a prerelease's own
    release, which shares its ``X.Y.Z``, else the next patch), ``after`` (the next patch, newer than the source
    whatever it is) and ``previous`` (the release below it in the changelog)."""

    #: Start from ``X.Y.Z-rc.1`` of the patch after the tree's, as a dry run of a prerelease bumps a runner's checkout.
    prerelease = False

    def make_copy(self, path: Path) -> Copy:
        copy = Copy(path)
        if self.prerelease:
            copy.bump_to(f"{_after(SOURCE)}-rc.1")
        return copy

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.copy = self.make_copy(Path(self._tmp.name))
        self.root = self.copy.root
        tree = release.Tree(self.root)
        self.source_where = tree.source.where
        self.known = next(p for p in tree.places if p.rule == "at-least")
        self.source = tree.source.value
        self.next = _next(self.source)
        self.after = _after(self.source)
        self.previous = _below(self.root, self.source)

    def problems(self, version=None) -> list[str]:
        try:
            release.check(self.root, version)
        except release.Disagrees as e:
            return e.problems
        return []

    def bump(self, version, date=DATE):
        return quiet(release.bump, self.root, version, date, out=print)

    def line_of(self, rel: str, text: str) -> int:
        """The 1-based line of the copy's ``rel`` that is exactly ``text``."""
        return lines_of(self.copy.read(rel)).index(text) + 1


class TheTree(unittest.TestCase):
    def test_the_tree_agrees(self):
        tree, version = release.check(ROOT)
        self.assertEqual(version, SOURCE)
        tomlv = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
        self.assertEqual(version, tomlv)
        files = [p.file.rel for p in tree.places]
        # The source, the eleven members' lock entries, the fuzz lock's three path packages, __version__, the
        # installer's two defaults and KNOWN_ISSUES' header.
        self.assertEqual([files.count(f) for f in ("Cargo.toml", "Cargo.lock", "crates/citar-engine/fuzz/Cargo.lock",
                                                   "citar/__init__.py", "installer/citar.iss", "KNOWN_ISSUES.md")],
                         [1, 11, 3, 1, 2, 1])
        self.assertEqual(run_main("check", "--root", str(ROOT))[0], 0)

    def test_the_lock_places_are_the_members(self):
        tree = release.Tree(ROOT)
        lock = sorted(p.what.split(" in ")[0] for p in tree.places if p.file.rel == "Cargo.lock")
        members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
        names = sorted(tomllib.loads((ROOT / m / "Cargo.toml").read_text(encoding="utf-8"))["package"]["name"]
                       for m in members)
        self.assertEqual(lock, names)
        fuzz = sorted(p.what.split(" in ")[0] for p in tree.places if "fuzz" in p.file.rel)
        self.assertEqual(fuzz, ["citar-bot", "citar-engine", "citar-testkit"])

    def test_the_installer_script_and_install_page_name_no_version(self):
        """citar.iss's comment and INSTALL.md's example are version-free, so they never need a bump."""
        iss = (ROOT / "installer" / "citar.iss").read_bytes().decode("utf-8")
        comments = "".join(line for line in iss.splitlines(keepends=True) if line.lstrip().startswith(";"))
        self.assertNotRegex(comments, r"/DAppVersion=[0-9]")
        self.assertIn("VersionInfoVersion={#AppVersionNumeric}", iss)
        install = (ROOT / "docs" / "INSTALL.md").read_text(encoding="utf-8")
        self.assertIn(r"Get-FileHash .\CITAR-<version>-setup.exe", install)
        self.assertNotRegex(install, r"CITAR-[0-9]+\.[0-9]+\.[0-9]+-setup\.exe")

    def test_the_lint_job_runs_the_check(self):
        text = (ROOT / ".github" / "workflows" / "test.yml").read_text(encoding="utf-8")
        lint = text[text.index("\n  lint:"):]
        lint = lint[:re.search(r"\n  [a-z-]+:\n", lint[1:]).start() + 1]
        self.assertIn("python scripts/release.py check", lint)


class Check(CopyTest):
    def plant(self, place_index: int, value: str):
        tree = release.Tree(self.root)
        place = tree.places[place_index]
        place.rewrite(value)
        place.file.save()
        return place

    def test_the_copy_starts_at_the_version_meant(self):
        self.assertEqual(self.source, f"{_after(SOURCE)}-rc.1" if self.prerelease else SOURCE)
        self.assertEqual(self.problems(), [])
        self.assertEqual(self.problems(self.source), [])
        self.assertTrue(release.Version.parse(self.next).newer_than(release.Version.parse(self.source)))
        self.assertTrue(release.Version.parse(self.after).newer_than(release.Version.parse(self.next))
                        or self.after == self.next)
        if self.prerelease:
            # Bumped from the tree's version, which is the release below it.
            self.assertEqual(self.previous, SOURCE)
            self.assertEqual(release.Version.parse(self.source).numeric, release.Version.parse(self.next).numeric)

    def test_each_place_planted_out_of_step_is_named_with_its_file_and_line(self):
        count = len(release.Tree(self.root).places)
        self.assertEqual(count, 19)
        for k in range(count):
            with self.subTest(place=k):
                place = release.Tree(self.root).places[k]
                wrong = f"{OLD}.0" if place.rule == "numeric" else OLD
                planted = self.plant(k, wrong)
                try:
                    problems = self.problems()
                    code, output = run_main("check", "--root", str(self.root))
                finally:
                    # Only the planted file changed: put it back for the next place.
                    (self.root / planted.file.rel).write_bytes(self.copy.before[planted.file.rel])
                self.assertEqual(code, 1)
                if planted.what.endswith("(the source)"):
                    # The source moved: every other place now disagrees with it, and the changelog too.
                    self.assertGreater(len(problems), 1)
                    continue
                self.assertEqual(len(problems), 1, problems)
                self.assertTrue(problems[0].startswith(f"{planted.file.rel}:{planted.index + 1}: "), problems)
                self.assertIn(wrong, problems[0])
                self.assertIn(f"{planted.file.rel}:{planted.index + 1}: ", output)
        self.assertEqual(self.problems(), [])

    def test_the_source_planted_alone_names_every_other_place(self):
        self.plant(0, OLD)
        problems = self.problems()
        named = {p.split(": ")[0] for p in problems}
        for place in release.Tree(self.root).places[1:]:
            if place.rule != "at-least":  # KNOWN_ISSUES may name a newer release than the source
                self.assertIn(place.where, named)
        self.assertNotIn(self.source_where, named)
        self.assertTrue(any(p.startswith("CHANGELOG.md:") for p in problems))

    def test_a_source_that_is_no_version_is_named(self):
        self.plant(0, f"{self.source}x")
        self.assertEqual(self.problems(), [f"{self.source_where}: the source's version '{self.source}x' is not a "
                                           "release version: X.Y.Z, or X.Y.Z-alpha.N, -beta.N or -rc.N"])
        with self.assertRaisesRegex(release.Refused, "out of step before the bump"):
            self.bump(self.next)

    def test_known_issues_may_name_a_newer_release(self):
        text = self.copy.read("KNOWN_ISSUES.md")
        header = f"in {self.known.value}."
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, "in 99.0.0.", 1))
        self.assertEqual(self.problems(), [])
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, f"in {OLD}.", 1))
        [problem] = self.problems()
        self.assertTrue(problem.startswith(f"{self.known.where}: the header's release is {OLD}, older than the source"),
                        problem)

    def test_a_missing_or_doubled_place_is_named(self):
        iss = self.copy.read("installer/citar.iss")
        self.copy.write("installer/citar.iss", re.sub(r'#define AppVersionNumeric "[^"]*"', "; gone", iss))
        self.assertEqual(self.problems(), ["installer/citar.iss: expected one #define AppVersionNumeric line, "
                                           "found 0 (lines: none)"])
        self.copy.write("installer/citar.iss", iss)
        init = self.copy.read("citar/__init__.py")
        self.copy.write("citar/__init__.py", init + f'__version__ = "{self.source}"\n')
        [problem] = self.problems()
        self.assertTrue(problem.startswith("citar/__init__.py: expected one __version__ line, found 2"), problem)
        self.copy.write("citar/__init__.py", init)
        lock = self.copy.read("Cargo.lock")
        entry = f'name = "citar-sim"\nversion = "{self.source}"\n'
        self.assertIn(entry, lock)
        self.copy.write("Cargo.lock", lock.replace(entry, f'name = "citar-sim2"\nversion = "{self.source}"\n'))
        self.assertEqual(self.problems(), ["Cargo.lock: no entry for the workspace member citar-sim (run cargo to "
                                           "update it)"])
        (self.root / "CHANGELOG.md").unlink()
        self.assertIn("CHANGELOG.md: missing", self.problems())

    def test_the_changelog_planted_out_of_step(self):
        text = self.copy.read("CHANGELOG.md")
        lines = lines_of(text)
        found, _ = release.sections(release.Text(self.root, "CHANGELOG.md"))
        mine = next(s for s in found if s.name == self.source)
        heading = f"## [{self.source}] - {mine.date}"
        newest = lines.index(heading) + 1
        unreleased = lines.index("## [Unreleased]") + 1
        url = "https://github.com/jprodgers/CITAR"
        link = lines.index(f"[Unreleased]: {url}/compare/v{self.source}...HEAD") + 1
        own_link = f"[{self.source}]: {url}/compare/v{self.previous}...v{self.source}"
        own = lines.index(own_link) + 1
        below = next(s for s in found if s.name == self.previous)
        cases = {
            "the Unreleased link from the release before": (
                text.replace(f"compare/v{self.source}...HEAD", f"compare/v{self.previous}...HEAD"),
                [f"CHANGELOG.md:{link}: [Unreleased] links to {url}/compare/v{self.previous}...HEAD, "
                 f"expected {url}/compare/v{self.source}...HEAD"]),
            "the release's link from another": (
                text.replace(f"compare/v{self.previous}...v{self.source}", f"compare/v{OLD}...v{self.source}"),
                [f"CHANGELOG.md:{own}: [{self.source}] links to {url}/compare/v{OLD}...v{self.source}, "
                 f"expected {url}/compare/v{self.previous}...v{self.source}"]),
            "the release's section undated": (
                text.replace(heading, f"## [{self.source}]"),
                [f"CHANGELOG.md:{newest}: '## [{self.source}]' needs its date, ' - YYYY-MM-DD'"]),
            "a date that is no date": (
                text.replace(heading, f"## [{self.source}] - 2026-09-31"),
                [f"CHANGELOG.md:{newest}: '## [{self.source}]' needs its date, ' - YYYY-MM-DD'"]),
            "a section added by hand above the release": (
                text.replace("## [Unreleased]\n", f"## [Unreleased]\n\n## [{self.next}] - {DATE}\n", 1),
                [f"CHANGELOG.md:{unreleased + 2}: the newest release section is [{self.next}], but the release is "
                 f"{self.source}"]),
            "no Unreleased section": (
                text.replace("## [Unreleased]\n", "", 1),
                ["CHANGELOG.md: expected one '## [Unreleased]', found 0 (lines: none)"]),
            "the Unreleased section below a release": (
                text.replace("## [Unreleased]\n", "", 1).replace(f"## [{self.previous}]", f"## [Unreleased]\n\n## "
                                                                f"[{self.previous}]", 1),
                # The heading's line is the one the release below had, less the removed line.
                [f"CHANGELOG.md:{below.index}: '## [Unreleased]' must come before every release's section "
                 f"(## [{self.source}] is at line {newest - 1})"]),
            "no Unreleased link": (
                text.replace(f"[Unreleased]: {url}/compare/v{self.source}...HEAD\n", ""),
                [f"CHANGELOG.md: no '[Unreleased]: ...' link (expected {url}/compare/v{self.source}...HEAD)"]),
            "the release's link twice": (
                text + own_link + "\n",
                [f"CHANGELOG.md: 2 '[{self.source}]:' links (lines {own}, {len(lines)})"]),
        }
        for name, (planted, expected) in cases.items():
            with self.subTest(name):
                self.copy.write("CHANGELOG.md", planted)
                self.assertEqual(self.problems(), expected)
        self.copy.write("CHANGELOG.md", text)
        self.assertEqual(self.problems(), [])

    def test_a_version_given_holds_every_place_to_it(self):
        self.assertEqual(self.problems(self.source), [])
        problems = self.problems(self.next)
        named = {p.split(": ")[0] for p in problems}
        numeric = release.Version.parse(self.next).numeric
        for place in release.Tree(self.root).places:
            with self.subTest(place.where):
                if place.rule == "at-least":  # KNOWN_ISSUES may already name the next release
                    continue
                if place.rule == "numeric" and place.value == numeric:
                    # A prerelease and its release share X.Y.Z.0: the installer's numeric version already holds.
                    self.assertTrue(release.Version.parse(self.source).prerelease)
                    self.assertNotIn(place.where, named)
                    continue
                self.assertIn(place.where, named)
        self.assertIn(f"CHANGELOG.md:{self.line_of('CHANGELOG.md', '## [Unreleased]')}", named)
        with self.assertRaises(release.Refused):
            release.check(self.root, f"{self.next};rm")


class Bump(CopyTest):
    def test_bump_moves_every_place_and_nothing_else(self):
        (tree, version), output = self.bump(self.next)
        self.assertEqual(version, self.next)
        changed = self.copy.changed()
        moves_known_issues = release.Version.parse(self.next).newer_than(release.Version.parse(self.known.value))
        self.assertEqual(sorted(changed), sorted(["Cargo.toml", "Cargo.lock", "crates/citar-engine/fuzz/Cargo.lock",
                                                  "citar/__init__.py", "installer/citar.iss", "CHANGELOG.md",
                                                  *(["KNOWN_ISSUES.md"] if moves_known_issues else [])]))
        self.assertEqual(changed["Cargo.toml"], ([f'version = "{self.source}"'], [f'version = "{self.next}"']))
        for rel, count in (("Cargo.lock", 11), ("crates/citar-engine/fuzz/Cargo.lock", 3)):
            old = lines_of(self.copy.original(rel))
            new = lines_of(self.copy.read(rel))
            diff = [(a, b) for a, b in zip(old, new, strict=True) if a != b]
            self.assertEqual(diff, [(f'version = "{self.source}"', f'version = "{self.next}"')] * count, rel)
        self.assertEqual(changed["citar/__init__.py"],
                         ([f'__version__ = "{self.source}"'], [f'__version__ = "{self.next}"']))
        source, after = release.Version.parse(self.source), release.Version.parse(self.next)
        # The numeric version moves only when the numbers do: a prerelease's release keeps its X.Y.Z.0.
        moved = [("AppVersion", self.source, self.next)]
        if source.numeric != after.numeric:
            moved.append(("AppVersionNumeric", source.numeric, after.numeric))
        self.assertEqual(changed["installer/citar.iss"], ([f'  #define {name} "{old}"\r' for name, old, _ in moved],
                                                          [f'  #define {name} "{new}"\r' for name, _, new in moved]))
        # The installer script keeps its CRLF on every line.
        iss = (self.root / "installer" / "citar.iss").read_bytes()
        self.assertEqual(iss.count(b"\n"), iss.count(b"\r\n"))
        self.assertIn(f'  #define AppVersionNumeric "{after.numeric}"\r\n'.encode("utf-8"), iss)
        if moves_known_issues:
            self.assertEqual(self.copy.read("KNOWN_ISSUES.md"), self.copy.original("KNOWN_ISSUES.md").replace(
                f"in {self.known.value}.", f"in {self.next}.", 1))
        url = "https://github.com/jprodgers/CITAR"
        log = self.copy.read("CHANGELOG.md")
        old_log = self.copy.original("CHANGELOG.md")
        first = lines_of(old_log)[lines_of(old_log).index("## [Unreleased]") + 2]
        self.assertIn(f"## [Unreleased]\n\n## [{self.next}] - {DATE}\n\n{first}\n", log)
        self.assertIn(f"[Unreleased]: {url}/compare/v{self.next}...HEAD\n"
                      f"[{self.next}]: {url}/compare/v{self.source}...v{self.next}\n"
                      f"[{self.source}]: {url}/compare/v{self.previous}...v{self.source}\n", log)
        self.assertEqual(log.replace(f"\n\n## [{self.next}] - {DATE}", "", 1)
                         .replace(f"compare/v{self.next}...HEAD\n"
                                  f"[{self.next}]: {url}/compare/v{self.source}...v{self.next}\n",
                                  f"compare/v{self.source}...HEAD\n"), old_log)
        self.assertIn(f"{self.source_where}: [workspace.package] version (the source) {self.source} -> {self.next}",
                      output)
        self.assertEqual(self.problems(self.next), [])
        # The release's notes are what the Unreleased section held.
        before = release.Text(self.root, "CHANGELOG.md")
        before.lines = [line + "\n" for line in lines_of(old_log)[:-1]]
        start, end = release.section_lines(before, "Unreleased")
        self.assertEqual(release.notes(self.root, self.next), "".join(before.lines[start:end]))

    def test_a_second_bump_and_an_older_one_are_refused(self):
        self.bump(self.next)
        after = self.copy.snapshot()
        for version in (self.next, self.source, f"{self.next}-rc.1"):
            with self.subTest(version):
                with self.assertRaisesRegex(release.Refused, f"{re.escape(version)} is not newer than the source's "
                                                             f"{re.escape(self.next)}"):
                    self.bump(version)
                self.assertEqual(self.copy.snapshot(), after)
        code, output = run_main("bump", self.source, "--date", DATE, "--root", str(self.root))
        self.assertEqual(code, 1)
        self.assertIn(f"refused: {self.source} is not newer than the source's {self.next}", output)

    def test_bump_refuses_a_bad_version_or_date_an_empty_section_and_a_tree_out_of_step(self):
        for version in (f"{self.next};rm", f"v{self.next}", f"{self.next}-dev.1"):
            with self.assertRaisesRegex(release.Refused, "is not a release version"):
                self.bump(version)
        for date in ("2026-13-01", "01-11-2026", "tomorrow", "2026-11-01\n"):
            with self.assertRaisesRegex(release.Refused, "is not a date"):
                self.bump(self.next, date)
        self.assertEqual(self.copy.changed(), {})
        log = self.copy.read("CHANGELOG.md")
        start = log.index("## [Unreleased]\n") + len("## [Unreleased]\n")
        self.copy.write("CHANGELOG.md", log[:start] + "\n" + log[log.index(f"## [{self.source}]"):])
        line = self.line_of("CHANGELOG.md", "## [Unreleased]")
        with self.assertRaisesRegex(release.Refused, rf"CHANGELOG\.md:{line}: the Unreleased section is empty"):
            self.bump(self.next)
        self.copy.write("CHANGELOG.md", log)
        init = self.copy.read("citar/__init__.py")
        self.copy.write("citar/__init__.py", init.replace(f'"{self.source}"', f'"{OLD}"'))
        line = self.line_of("citar/__init__.py", f'__version__ = "{OLD}"')
        with self.assertRaisesRegex(release.Refused, rf"out of step before the bump(.|\n)*citar/__init__\.py:{line}:"):
            self.bump(self.next)
        self.assertEqual(self.copy.read("Cargo.toml"), self.copy.original("Cargo.toml"))

    def test_known_issues_moves_only_when_it_is_older(self):
        text = self.copy.read("KNOWN_ISSUES.md")
        header = f"in {self.known.value}."
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, f"in {self.source}.", 1))
        _, output = self.bump(self.next)
        self.assertIn(f"{self.known.where}: the header's release {self.source} -> {self.next}", output)
        self.assertEqual(self.copy.read("KNOWN_ISSUES.md"), text.replace(header, f"in {self.next}.", 1))
        # Newer than the release: left alone.
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, "in 99.0.0.", 1))
        self.copy.write("CHANGELOG.md", self.copy.read("CHANGELOG.md").replace(
            "## [Unreleased]\n", "## [Unreleased]\n\n- More.\n", 1))
        self.bump(_next(self.next))
        self.assertIn("misleading in 99.0.0.", self.copy.read("KNOWN_ISSUES.md"))

    def test_a_prerelease_and_then_its_release(self):
        # Of the next patch, which is newer than the source even when the source is itself a prerelease.
        rc = f"{self.after}-rc.1"
        self.bump(rc)
        iss = self.copy.read("installer/citar.iss")
        self.assertIn(f'#define AppVersion "{rc}"', iss)
        self.assertIn(f'#define AppVersionNumeric "{release.Version.parse(self.after).numeric}"', iss)
        self.assertIn(f"## [{rc}] - {DATE}", self.copy.read("CHANGELOG.md"))
        self.assertEqual(self.problems(rc), [])
        with self.assertRaises(release.Refused):
            self.bump(f"{self.after}-beta.2")
        with self.assertRaisesRegex(release.Refused, "Unreleased section is empty"):
            self.bump(self.after)
        log = self.copy.read("CHANGELOG.md")
        self.copy.write("CHANGELOG.md", log.replace("## [Unreleased]\n", f"## [Unreleased]\n\nThe release of {rc}, "
                                                    "unchanged.\n", 1))
        self.bump(self.after, "2026-11-08")
        url = "https://github.com/jprodgers/CITAR"
        self.assertIn(f"[{self.after}]: {url}/compare/v{rc}...v{self.after}\n"
                      f"[{rc}]: {url}/compare/v{self.source}...v{rc}\n", self.copy.read("CHANGELOG.md"))
        self.assertEqual(self.problems(self.after), [])


class Prepare(CopyTest):
    def prepare(self, mode, version=None, date=DATE, env=None):
        environ = {k: v for k, v in os.environ.items() if k != "GITHUB_ACTIONS"}
        environ.update(env or {})
        with mock.patch.dict(os.environ, environ, clear=True):
            return quiet(release.prepare, self.root, mode, version, date, out=print)

    def test_ci_only_checks(self):
        result, output = self.prepare("ci")
        self.assertEqual(result, self.source)
        self.assertEqual(self.copy.changed(), {})
        self.assertIn(f"every place says {self.source}", output)
        init = self.copy.read("citar/__init__.py")
        self.copy.write("citar/__init__.py", init.replace(f'"{self.source}"', f'"{OLD}"'))
        with self.assertRaises(release.Disagrees):
            self.prepare("ci")

    def test_tag_checks_against_its_version(self):
        self.assertEqual(self.prepare("tag", self.source)[0], self.source)
        with self.assertRaises(release.Disagrees) as caught:
            self.prepare("tag", self.next)
        self.assertIn(f"{self.source_where}: [workspace.package] version (the source) is {self.source}, but "
                      f"the release is {self.next}", caught.exception.problems)
        with self.assertRaisesRegex(release.Refused, "needs --version"):
            self.prepare("tag")
        self.assertEqual(self.copy.changed(), {})

    def test_dry_run_bumps_the_runners_checkout_then_checks(self):
        result, output = self.prepare("dry-run", self.next)
        self.assertEqual(result, self.next)
        self.assertIn(f"bumped this runner's checkout from {self.source} to {self.next}", output)
        self.assertNotIn("::notice", output)
        bumped = self.copy.snapshot()
        # The same files as `bump` writes.
        other = self.make_copy(Path(self._tmp.name + "-bump"))
        self.addCleanup(shutil.rmtree, other.root, True)
        quiet(release.bump, other.root, self.next, DATE, out=print)
        self.assertEqual(other.snapshot(), bumped)
        # Again: the source now says the version asked for, so it only checks.
        _, output = self.prepare("dry-run", self.next)
        self.assertNotIn("bumped", output)
        self.assertEqual(self.copy.snapshot(), bumped)

    def test_dry_run_on_actions_leaves_a_notice(self):
        _, output = self.prepare("dry-run", self.next, env={"GITHUB_ACTIONS": "true"})
        self.assertIn(f"::notice title=release.py prepare::bumped this runner's checkout from {self.source} "
                      f"to {self.next}", output)

    def test_dry_run_refuses_an_older_version_and_needs_one(self):
        with self.assertRaisesRegex(release.Refused, f"{OLD} is not newer"):
            self.prepare("dry-run", OLD)
        with self.assertRaisesRegex(release.Refused, "needs --version"):
            self.prepare("dry-run")
        self.assertEqual(self.copy.changed(), {})

    def test_the_command_line(self):
        code, output = run_main("prepare", "--mode", "tag", "--version", self.next, "--root", str(self.root))
        self.assertEqual(code, 1)
        self.assertIn(f"{self.source_where}:", output)
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("GITHUB_ACTIONS", None)
            code, output = run_main("prepare", "--mode", "dry-run", "--version", self.next, "--date", DATE,
                                    "--root", str(self.root))
        self.assertEqual(code, 0, output)
        self.assertEqual(run_main("prepare", "--mode", "nightly", "--root", str(self.root))[0], 2)


class CheckFromAPrerelease(Check):
    """Check's tests on a copy at a prerelease, as a runner's checkout is after a dry run of ``X.Y.Z-rc.N``."""
    prerelease = True


class BumpFromAPrerelease(Bump):
    """Bump's tests from a prerelease: its next release keeps the installer's numeric version."""
    prerelease = True


class PrepareFromAPrerelease(Prepare):
    """Prepare's tests from a prerelease."""
    prerelease = True


class Notes(unittest.TestCase):
    """Against the released sections, which no bump moves."""

    def test_notes_are_the_section_byte_for_byte(self):
        data = (ROOT / "CHANGELOG.md").read_bytes()
        heading = b"## [0.1.5] - 2026-09-22\n"
        start = data.index(heading) + len(heading)
        end = data.index(b"\n## [0.1.4]", start) + 1
        self.assertEqual(release.notes(ROOT, "0.1.5").encode("utf-8"), data[start:end])

    @unittest.skipUnless(shutil.which("awk"), "no awk on PATH")
    def test_notes_equal_release_ymls_awk(self):
        with tempfile.TemporaryDirectory() as tmp:
            program = Path(tmp) / "notes.awk"
            program.write_bytes(NOTES_AWK.encode("utf-8"))
            for version in ("0.1.5", "0.1.4", "0.1.1"):
                with self.subTest(version):
                    awk = subprocess.run(["awk", "-v", f"v={version}", "-f", str(program), str(ROOT / "CHANGELOG.md")],
                                         capture_output=True, check=True).stdout
                    self.assertEqual(release.notes(ROOT, version).encode("utf-8"), awk)

    def test_the_last_section_stops_at_the_links(self):
        body = release.notes(ROOT, "0.1.0")
        self.assertNotIn("]: https://github.com/jprodgers/CITAR/", body)
        self.assertTrue(body.endswith("\n\n"))

    def test_the_command_line_writes_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "notes.md"
            self.assertEqual(run_main("notes", "0.1.5", "--out", str(out))[0], 0)
            self.assertEqual(out.read_bytes(), release.notes(ROOT, "0.1.5").encode("utf-8"))
            done = subprocess.run([sys.executable, "-I", str(SCRIPT), "notes", "0.1.5"], capture_output=True,
                                  check=True)
            self.assertEqual(done.stdout, out.read_bytes())

    def test_a_missing_or_empty_section_is_refused(self):
        with self.assertRaisesRegex(release.Refused, r"no section '## \[99\.0\.0\]'"):
            release.notes(ROOT, "99.0.0")
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "CHANGELOG.md").write_bytes(b"# Changelog\r\n\r\n## [0.2.0] - 2026-11-01\r\n\r\n"
                                                     b"## [0.1.9] - 2026-10-01\r\n\r\n- A fix.\r\n\r\n"
                                                     b"[0.1.9]: https://example.invalid\r\n")
            with self.assertRaisesRegex(release.Refused, "the section of 0.2.0 is empty"):
                release.notes(Path(tmp), "0.2.0")
            # A CRLF file's section keeps its CRLF.
            self.assertEqual(release.notes(Path(tmp), "0.1.9"), "\r\n- A fix.\r\n\r\n")
        self.assertEqual(run_main("notes", "99.0.0")[0], 1)


class LabelAndMeta(unittest.TestCase):
    def test_labels(self):
        self.assertEqual(release.label("tag", "0.1.6", None), "v0.1.6")
        self.assertEqual(release.label("tag", "0.1.6-rc.1", "abc1234"), "v0.1.6-rc.1")
        self.assertEqual(release.label("dry-run", "0.1.6", "5C4BDD7F0123abc"), "v0.1.6-dryrun+5c4bdd7")
        self.assertEqual(release.label("ci", None, "5c4bdd7f3b2a1c0d9e8f7a6b5c4d3e2f1a0b9c8d"), "ci+5c4bdd7")
        for mode, version, sha in (("tag", None, None), ("dry-run", "0.1.6", None), ("ci", None, "xyz1234"),
                                   ("ci", None, "abc12"), ("dry-run", "0.1.6;rm", "abc1234"), ("nightly", "0.1.6", "a"),
                                   ("ci", None, "abc1234; rm -rf /")):
            with self.subTest(mode=mode, version=version, sha=sha):
                with self.assertRaises(release.Refused):
                    release.label(mode, version, sha)

    def test_the_version_rule(self):
        for good in ("0.1.6", "0.1.6-rc.1", "0.1.6-alpha.0", "0.1.6-beta.12", "10.20.30"):
            self.assertEqual(release.Version.parse(good).text, good)
        for bad in ("0.1.6;rm", "v0.1.6", "0.1", "0.1.6.0", "0.1.6-rc1", "0.1.6-RC.1", "0.1.6-dev.1", "01.1.6",
                    "0.1.06", "0.1.6-rc.01", "0.1.6\n", " 0.1.6", "0.1.６", "0.1.٦", "", "0.1.6+build"):
            with self.subTest(repr(bad)):
                with self.assertRaisesRegex(release.Refused, "is not a release version"):
                    release.Version.parse(bad)

    def test_versions_order_as_semver_does(self):
        order = ["0.1.5", "0.1.6-alpha.1", "0.1.6-alpha.2", "0.1.6-beta.1", "0.1.6-rc.1", "0.1.6-rc.10", "0.1.6",
                 "0.1.7", "0.2.0", "1.0.0"]
        parsed = [release.Version.parse(v) for v in order]
        for a, b in zip(parsed, parsed[1:]):
            self.assertTrue(b.newer_than(a), (a.text, b.text))
            self.assertFalse(a.newer_than(b))
        self.assertFalse(parsed[0].newer_than(parsed[0]))

    def test_pep440_and_numeric(self):
        cases = {"0.1.6": ("0.1.6", "0.1.6.0", False), "0.1.6-rc.1": ("0.1.6rc1", "0.1.6.0", True),
                 "0.2.0-alpha.3": ("0.2.0a3", "0.2.0.0", True), "1.2.3-beta.4": ("1.2.3b4", "1.2.3.0", True)}
        for text, expected in cases.items():
            v = release.Version.parse(text)
            self.assertEqual((v.pep440, v.numeric, v.prerelease), expected)

    def test_meta_for_each_mode(self):
        self.assertEqual(release.meta(ROOT, "tag", "0.1.6-rc.1", None, "2026-11-01"), {
            "version": "0.1.6-rc.1", "pep440": "0.1.6rc1", "numeric": "0.1.6.0", "label": "v0.1.6-rc.1",
            "date": "2026-11-01", "mode": "tag", "prerelease": "true"})
        self.assertEqual(release.meta(ROOT, "dry-run", "0.1.6", "5c4bdd7f", "2026-11-01"), {
            "version": "0.1.6", "pep440": "0.1.6", "numeric": "0.1.6.0", "label": "v0.1.6-dryrun+5c4bdd7",
            "date": "2026-11-01", "mode": "dry-run", "prerelease": "false"})
        ci = release.meta(ROOT, "ci", None, "5c4bdd7f", None)
        self.assertEqual((ci["version"], ci["label"], ci["mode"]), (SOURCE, "ci+5c4bdd7", "ci"))
        self.assertRegex(ci["date"], r"^\d{4}-\d{2}-\d{2}$")
        for mode, version, sha, date in (("tag", "0.1.6;rm", None, None), ("tag", None, None, None),
                                         ("dry-run", "0.1.6", "5c4bdd7f", "2026-02-30"), ("ci", "v0.1.6", "5c4bdd7f",
                                                                                           None)):
            with self.subTest(mode=mode, version=version, date=date):
                with self.assertRaises(release.Refused):
                    release.meta(ROOT, mode, version, sha, date)

    def test_meta_appends_to_the_github_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "github_output"
            out.write_bytes(b"earlier=1\n")
            code, printed = run_main("meta", "--mode", "dry-run", "--version", "0.1.6", "--sha", "5c4bdd7f",
                                     "--date", "2026-11-01", "--github-output", str(out))
            self.assertEqual(code, 0)
            self.assertEqual(out.read_bytes(), b"earlier=1\n" + printed.encode("utf-8"))
            self.assertEqual(printed, "version=0.1.6\npep440=0.1.6\nnumeric=0.1.6.0\nlabel=v0.1.6-dryrun+5c4bdd7\n"
                                      "date=2026-11-01\nmode=dry-run\nprerelease=false\n")
            code, printed = run_main("meta", "--mode", "tag", "--version", "0.1.6;rm", "--github-output", str(out))
            self.assertEqual(code, 1)
            self.assertIn("refused: '0.1.6;rm' is not a release version", printed)
            self.assertNotIn("0.1.6;rm", out.read_text(encoding="utf-8"))


class Assets(unittest.TestCase):
    def test_the_list_is_p5_10s(self):
        names = [a["name"] for a in release.assets("0.1.6")]
        self.assertEqual(names, ASSETS_016)
        files = release.asset_files("0.1.6")
        self.assertEqual(files, [f for name in ASSETS_016 for f in (name, f"{name}.sha256")] + ["SHA256SUMS"])
        self.assertEqual(len(files), 43)

    def test_a_prerelease_uses_pep_440_for_the_python_files(self):
        names = [a["name"] for a in release.assets("0.1.6-rc.1")]
        self.assertEqual(names, [n.replace("citar-0.1.6", "citar-0.1.6rc1").replace("CITAR-0.1.6", "CITAR-0.1.6-rc.1")
                                 .replace("citar-helper-0.1.6", "citar-helper-0.1.6-rc.1") for n in ASSETS_016])
        self.assertIn("citar-0.1.6rc1-cp311-abi3-manylinux_2_28_aarch64.whl", names)
        self.assertIn("CITAR-0.1.6-rc.1-setup.exe", names)

    def test_a_signed_tag_adds_the_signature(self):
        files = release.asset_files("0.1.6", signed=True)
        self.assertEqual(sorted(set(files) - set(release.asset_files("0.1.6"))),
                         ["citar-helper-manifest.json.sig", "citar-helper-manifest.json.sig.sha256"])

    def test_the_helper_names_are_the_download_cards(self):
        rules = (ROOT / "citar" / "web" / "js" / "helperrules.js").read_text(encoding="utf-8")
        cards = sorted(re.findall(r'file: "(citar-helper-[a-z0-9.-]+)"', rules))
        self.assertEqual(len(cards), 5)
        ours = sorted(a["name"] for a in release.assets("0.1.6") if a["group"] == "helper"
                      and not a["name"].endswith(".json"))
        self.assertEqual(ours, cards)

    def test_the_command_line(self):
        code, printed = run_main("assets", "0.1.6-rc.1", "--json")
        self.assertEqual(code, 0)
        data = json.loads(printed)
        self.assertEqual((data["version"], data["pep440"], data["signed"]), ("0.1.6-rc.1", "0.1.6rc1", False))
        self.assertEqual(data["files"], release.asset_files("0.1.6-rc.1"))
        code, printed = run_main("assets", "0.1.6")
        self.assertEqual(printed.split(), release.asset_files("0.1.6"))
        self.assertEqual(run_main("assets", "0.1.6;rm")[0], 1)

    def test_what_it_prints_ends_its_lines_in_lf_on_every_platform(self):
        """RELEASING.md step 3: `release.py assets V > assets.txt && diff assets.txt preview/subjects.txt`, in Git
        Bash on Windows too, where Python's standard output writes "\\r\\n" unless told otherwise (found by 5-13's
        walk of the checklist over the dry run's preview). meta's values are read by shells the same way."""
        done = subprocess.run([sys.executable, "-I", str(SCRIPT), "assets", "0.1.6"], capture_output=True, check=True)
        self.assertEqual(done.stdout, "".join(f"{n}\n" for n in release.asset_files("0.1.6")).encode("ascii"))
        done = subprocess.run([sys.executable, "-I", str(SCRIPT), "meta", "--mode", "dry-run", "--version", "0.1.6",
                               "--sha", "5c4bdd7f", "--date", "2026-11-01"], capture_output=True, check=True)
        self.assertNotIn(b"\r", done.stdout)
        self.assertTrue(done.stdout.startswith(b"version=0.1.6\npep440=0.1.6\n"), done.stdout)
        done = subprocess.run([sys.executable, "-I", str(SCRIPT), "label", "--mode", "tag", "--version", "0.1.6"],
                              capture_output=True, check=True)
        self.assertEqual(done.stdout, b"v0.1.6\n")


class Standalone(unittest.TestCase):
    def test_it_runs_with_the_standard_library_alone(self):
        """Every job runs it before installing anything: an isolated interpreter (no site-packages) checks the tree."""
        done = subprocess.run([sys.executable, "-I", "-S", str(SCRIPT), "check"], capture_output=True, text=True,
                              cwd=tempfile.gettempdir())
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(f"agree on {SOURCE}", done.stdout)
        done = subprocess.run([sys.executable, "-I", str(SCRIPT), "check", "--version", OLD], capture_output=True,
                              text=True)
        self.assertEqual(done.returncode, 1)
        self.assertIn("Cargo.toml:", done.stderr)
        self.assertEqual(subprocess.run([sys.executable, "-I", str(SCRIPT)], capture_output=True).returncode, 2)

    def run_as(self, version_info: tuple, *argv: str, tomllib_missing: bool):
        """The script as ``python -I -S`` runs it, under an interpreter that reports ``version_info`` (and, as before
        3.11, has no ``tomllib`` when ``tomllib_missing``)."""
        program = ("import runpy, sys\n"
                   + ("sys.modules['tomllib'] = None\n" if tomllib_missing else "")
                   + f"sys.version_info = {version_info!r}\n"
                   f"runpy.run_path({str(SCRIPT)!r}, run_name='__main__')\n")
        return subprocess.run([sys.executable, "-I", "-S", "-c", program, *argv], capture_output=True, text=True,
                              cwd=tempfile.gettempdir())

    def test_an_older_python_is_refused_with_its_reason(self):
        """Ubuntu 22.04's python3 is 3.10, which has no tomllib: a job that runs the script there is told why, with a
        usage error's status, instead of an ImportError's traceback."""
        done = self.run_as((3, 10, 12, "final", 0), "check", tomllib_missing=True)
        self.assertEqual((done.returncode, done.stdout), (2, ""), done.stderr)
        self.assertEqual(done.stderr, "release.py needs Python 3.11 or newer (it reads TOML with tomllib); this is "
                                      "Python 3.10.12\n")
        # 3.11 itself is enough.
        done = self.run_as((3, 11, 0, "final", 0), "check", tomllib_missing=False)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(f"agree on {SOURCE}", done.stdout)

    def test_older_pythons_compile_it_up_to_the_refusal(self):
        """An older interpreter compiles the whole file before the version check runs, so the file keeps to Python
        3.8's grammar (ast checks the forms that came after it; the f-strings 3.12 allows are held by the suite's
        Python 3.11 job, which compiles the script when this module loads it)."""
        ast.parse(SCRIPT.read_text(encoding="utf-8"), filename=str(SCRIPT), feature_version=(3, 8))
        with self.assertRaises(SyntaxError):  # the check can fail: a form 3.8 lacks
            ast.parse("match x:\n    case 1:\n        pass\n", feature_version=(3, 8))


if __name__ == "__main__":
    unittest.main()
