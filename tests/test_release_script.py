"""scripts/release.py: one version for a release (design/phase5-release.md P5.2).

``check`` against the tree and against each place planted out of step; ``bump`` in a temporary copy of the checked
files (every place moves and nothing else, the installer script keeps its CRLF); ``prepare``'s three modes; ``notes``
against release.yml's awk; ``label``, ``meta`` and the version rule; ``assets`` against P5.10's table.

Nothing here names the tree's version: a release dry run bumps the runner's checkout before the suite runs (P5.2's
``prepare --mode dry-run``), so the tests read the version from the tree (``SOURCE``) and bump past it (``NEXT``).
"""
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
#: A version older than any the tree will hold, so planting it puts any place out of step.
OLD = "0.0.1"


def _next(version: str) -> str:
    """The next release after ``version``: its release when it is a prerelease, else the next patch."""
    v = release.Version.parse(version)
    return f"{v.major}.{v.minor}.{v.patch + (0 if v.prerelease else 1)}"


#: The tree's version, the release after it, and the release below it in the changelog.
SOURCE = release.Tree(ROOT).source.value
NEXT = _next(SOURCE)
_versions = [s.name for s in release.sections(release.Text(ROOT, "CHANGELOG.md"))[0] if s.name != "Unreleased"]
PREVIOUS = _versions[_versions.index(SOURCE) + 1]

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
        log = release.Text(tmp, "CHANGELOG.md")
        start, end = release.section_lines(log, "Unreleased")
        if not "".join(log.lines[start:end]).strip():
            self.write("CHANGELOG.md", self.read("CHANGELOG.md").replace(
                "## [Unreleased]\n", "## [Unreleased]\n\n- A change for the tests.\n", 1))
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
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.copy = Copy(Path(self._tmp.name))
        self.root = self.copy.root
        tree = release.Tree(self.root)
        self.source_where = tree.source.where
        self.known = next(p for p in tree.places if p.rule == "at-least")

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
        self.plant(0, f"{SOURCE}x")
        self.assertEqual(self.problems(), [f"{self.source_where}: the source's version '{SOURCE}x' is not a release "
                                           "version: X.Y.Z, or X.Y.Z-alpha.N, -beta.N or -rc.N"])
        with self.assertRaisesRegex(release.Refused, "out of step before the bump"):
            self.bump(NEXT)

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
        self.copy.write("citar/__init__.py", init + f'__version__ = "{SOURCE}"\n')
        [problem] = self.problems()
        self.assertTrue(problem.startswith("citar/__init__.py: expected one __version__ line, found 2"), problem)
        self.copy.write("citar/__init__.py", init)
        lock = self.copy.read("Cargo.lock")
        entry = f'name = "citar-sim"\nversion = "{SOURCE}"\n'
        self.assertIn(entry, lock)
        self.copy.write("Cargo.lock", lock.replace(entry, f'name = "citar-sim2"\nversion = "{SOURCE}"\n'))
        self.assertEqual(self.problems(), ["Cargo.lock: no entry for the workspace member citar-sim (run cargo to "
                                           "update it)"])
        (self.root / "CHANGELOG.md").unlink()
        self.assertIn("CHANGELOG.md: missing", self.problems())

    def test_the_changelog_planted_out_of_step(self):
        text = self.copy.read("CHANGELOG.md")
        lines = lines_of(text)
        found, _ = release.sections(release.Text(self.root, "CHANGELOG.md"))
        mine = next(s for s in found if s.name == SOURCE)
        heading = f"## [{SOURCE}] - {mine.date}"
        newest = lines.index(heading) + 1
        unreleased = lines.index("## [Unreleased]") + 1
        url = "https://github.com/jprodgers/CITAR"
        link = lines.index(f"[Unreleased]: {url}/compare/v{SOURCE}...HEAD") + 1
        own_link = f"[{SOURCE}]: {url}/compare/v{PREVIOUS}...v{SOURCE}"
        own = lines.index(own_link) + 1
        below = next(s for s in found if s.name == PREVIOUS)
        cases = {
            "the Unreleased link from the release before": (
                text.replace(f"compare/v{SOURCE}...HEAD", f"compare/v{PREVIOUS}...HEAD"),
                [f"CHANGELOG.md:{link}: [Unreleased] links to {url}/compare/v{PREVIOUS}...HEAD, "
                 f"expected {url}/compare/v{SOURCE}...HEAD"]),
            "the release's link from another": (
                text.replace(f"compare/v{PREVIOUS}...v{SOURCE}", f"compare/v{OLD}...v{SOURCE}"),
                [f"CHANGELOG.md:{own}: [{SOURCE}] links to {url}/compare/v{OLD}...v{SOURCE}, "
                 f"expected {url}/compare/v{PREVIOUS}...v{SOURCE}"]),
            "the release's section undated": (
                text.replace(heading, f"## [{SOURCE}]"),
                [f"CHANGELOG.md:{newest}: '## [{SOURCE}]' needs its date, ' - YYYY-MM-DD'"]),
            "a date that is no date": (
                text.replace(heading, f"## [{SOURCE}] - 2026-09-31"),
                [f"CHANGELOG.md:{newest}: '## [{SOURCE}]' needs its date, ' - YYYY-MM-DD'"]),
            "a section added by hand above the release": (
                text.replace("## [Unreleased]\n", f"## [Unreleased]\n\n## [{NEXT}] - {DATE}\n", 1),
                [f"CHANGELOG.md:{unreleased + 2}: the newest release section is [{NEXT}], but the release is "
                 f"{SOURCE}"]),
            "no Unreleased section": (
                text.replace("## [Unreleased]\n", "", 1),
                ["CHANGELOG.md: expected one '## [Unreleased]', found 0 (lines: none)"]),
            "the Unreleased section below a release": (
                text.replace("## [Unreleased]\n", "", 1).replace(f"## [{PREVIOUS}]", f"## [Unreleased]\n\n## "
                                                                f"[{PREVIOUS}]", 1),
                # The heading's line is the one the release below had, less the removed line.
                [f"CHANGELOG.md:{below.index}: '## [Unreleased]' must come before every release's section "
                 f"(## [{SOURCE}] is at line {newest - 1})"]),
            "no Unreleased link": (
                text.replace(f"[Unreleased]: {url}/compare/v{SOURCE}...HEAD\n", ""),
                [f"CHANGELOG.md: no '[Unreleased]: ...' link (expected {url}/compare/v{SOURCE}...HEAD)"]),
            "the release's link twice": (
                text + own_link + "\n",
                [f"CHANGELOG.md: 2 '[{SOURCE}]:' links (lines {own}, {len(lines)})"]),
        }
        for name, (planted, expected) in cases.items():
            with self.subTest(name):
                self.copy.write("CHANGELOG.md", planted)
                self.assertEqual(self.problems(), expected)
        self.copy.write("CHANGELOG.md", text)
        self.assertEqual(self.problems(), [])

    def test_a_version_given_holds_every_place_to_it(self):
        self.assertEqual(self.problems(SOURCE), [])
        problems = self.problems(NEXT)
        named = {p.split(": ")[0] for p in problems}
        for place in release.Tree(self.root).places:
            if place.rule != "at-least":  # KNOWN_ISSUES may already name the next release
                self.assertIn(place.where, named)
        self.assertIn(f"CHANGELOG.md:{self.line_of('CHANGELOG.md', '## [Unreleased]')}", named)
        with self.assertRaises(release.Refused):
            release.check(self.root, f"{NEXT};rm")


class Bump(CopyTest):
    def test_bump_moves_every_place_and_nothing_else(self):
        (tree, version), output = self.bump(NEXT)
        self.assertEqual(version, NEXT)
        changed = self.copy.changed()
        moves_known_issues = release.Version.parse(NEXT).newer_than(release.Version.parse(self.known.value))
        self.assertEqual(sorted(changed), sorted(["Cargo.toml", "Cargo.lock", "crates/citar-engine/fuzz/Cargo.lock",
                                                  "citar/__init__.py", "installer/citar.iss", "CHANGELOG.md",
                                                  *(["KNOWN_ISSUES.md"] if moves_known_issues else [])]))
        self.assertEqual(changed["Cargo.toml"], ([f'version = "{SOURCE}"'], [f'version = "{NEXT}"']))
        for rel, count in (("Cargo.lock", 11), ("crates/citar-engine/fuzz/Cargo.lock", 3)):
            old = lines_of(self.copy.original(rel))
            new = lines_of(self.copy.read(rel))
            diff = [(a, b) for a, b in zip(old, new, strict=True) if a != b]
            self.assertEqual(diff, [(f'version = "{SOURCE}"', f'version = "{NEXT}"')] * count, rel)
        self.assertEqual(changed["citar/__init__.py"], ([f'__version__ = "{SOURCE}"'], [f'__version__ = "{NEXT}"']))
        source, after = release.Version.parse(SOURCE), release.Version.parse(NEXT)
        self.assertEqual(changed["installer/citar.iss"],
                         ([f'  #define AppVersion "{SOURCE}"\r', f'  #define AppVersionNumeric "{source.numeric}"\r'],
                          [f'  #define AppVersion "{NEXT}"\r', f'  #define AppVersionNumeric "{after.numeric}"\r']))
        # The installer script keeps its CRLF on every line.
        iss = (self.root / "installer" / "citar.iss").read_bytes()
        self.assertEqual(iss.count(b"\n"), iss.count(b"\r\n"))
        if moves_known_issues:
            self.assertEqual(self.copy.read("KNOWN_ISSUES.md"),
                             self.copy.original("KNOWN_ISSUES.md").replace(f"in {self.known.value}.", f"in {NEXT}.", 1))
        url = "https://github.com/jprodgers/CITAR"
        log = self.copy.read("CHANGELOG.md")
        old_log = self.copy.original("CHANGELOG.md")
        first = lines_of(old_log)[lines_of(old_log).index("## [Unreleased]") + 2]
        self.assertIn(f"## [Unreleased]\n\n## [{NEXT}] - {DATE}\n\n{first}\n", log)
        self.assertIn(f"[Unreleased]: {url}/compare/v{NEXT}...HEAD\n"
                      f"[{NEXT}]: {url}/compare/v{SOURCE}...v{NEXT}\n"
                      f"[{SOURCE}]: {url}/compare/v{PREVIOUS}...v{SOURCE}\n", log)
        self.assertEqual(log.replace(f"\n\n## [{NEXT}] - {DATE}", "", 1)
                         .replace(f"compare/v{NEXT}...HEAD\n[{NEXT}]: {url}/compare/v{SOURCE}...v{NEXT}\n",
                                  f"compare/v{SOURCE}...HEAD\n"), old_log)
        self.assertIn(f"{self.source_where}: [workspace.package] version (the source) {SOURCE} -> {NEXT}", output)
        self.assertEqual(self.problems(NEXT), [])
        # The release's notes are what the Unreleased section held.
        before = release.Text(self.root, "CHANGELOG.md")
        before.lines = [line + "\n" for line in lines_of(old_log)[:-1]]
        start, end = release.section_lines(before, "Unreleased")
        self.assertEqual(release.notes(self.root, NEXT), "".join(before.lines[start:end]))

    def test_a_second_bump_and_an_older_one_are_refused(self):
        self.bump(NEXT)
        after = self.copy.snapshot()
        for version in (NEXT, SOURCE, f"{NEXT}-rc.1"):
            with self.subTest(version):
                with self.assertRaisesRegex(release.Refused, f"{re.escape(version)} is not newer than the source's "
                                                             f"{re.escape(NEXT)}"):
                    self.bump(version)
                self.assertEqual(self.copy.snapshot(), after)
        code, output = run_main("bump", SOURCE, "--date", DATE, "--root", str(self.root))
        self.assertEqual(code, 1)
        self.assertIn(f"refused: {SOURCE} is not newer than the source's {NEXT}", output)

    def test_bump_refuses_a_bad_version_or_date_an_empty_section_and_a_tree_out_of_step(self):
        for version in (f"{NEXT};rm", f"v{NEXT}", f"{NEXT}-dev.1"):
            with self.assertRaisesRegex(release.Refused, "is not a release version"):
                self.bump(version)
        for date in ("2026-13-01", "01-11-2026", "tomorrow", "2026-11-01\n"):
            with self.assertRaisesRegex(release.Refused, "is not a date"):
                self.bump(NEXT, date)
        self.assertEqual(self.copy.changed(), {})
        log = self.copy.read("CHANGELOG.md")
        start = log.index("## [Unreleased]\n") + len("## [Unreleased]\n")
        self.copy.write("CHANGELOG.md", log[:start] + "\n" + log[log.index(f"## [{SOURCE}]"):])
        line = self.line_of("CHANGELOG.md", "## [Unreleased]")
        with self.assertRaisesRegex(release.Refused, rf"CHANGELOG\.md:{line}: the Unreleased section is empty"):
            self.bump(NEXT)
        self.copy.write("CHANGELOG.md", log)
        init = self.copy.read("citar/__init__.py")
        self.copy.write("citar/__init__.py", init.replace(f'"{SOURCE}"', f'"{OLD}"'))
        line = self.line_of("citar/__init__.py", f'__version__ = "{OLD}"')
        with self.assertRaisesRegex(release.Refused, rf"out of step before the bump(.|\n)*citar/__init__\.py:{line}:"):
            self.bump(NEXT)
        self.assertEqual(self.copy.read("Cargo.toml"), self.copy.original("Cargo.toml"))

    def test_known_issues_moves_only_when_it_is_older(self):
        text = self.copy.read("KNOWN_ISSUES.md")
        header = f"in {self.known.value}."
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, f"in {SOURCE}.", 1))
        _, output = self.bump(NEXT)
        self.assertIn(f"{self.known.where}: the header's release {SOURCE} -> {NEXT}", output)
        self.assertEqual(self.copy.read("KNOWN_ISSUES.md"), text.replace(header, f"in {NEXT}.", 1))
        # Newer than the release: left alone.
        self.copy.write("KNOWN_ISSUES.md", text.replace(header, "in 99.0.0.", 1))
        self.copy.write("CHANGELOG.md", self.copy.read("CHANGELOG.md").replace(
            "## [Unreleased]\n", "## [Unreleased]\n\n- More.\n", 1))
        self.bump(_next(NEXT))
        self.assertIn("misleading in 99.0.0.", self.copy.read("KNOWN_ISSUES.md"))

    def test_a_prerelease_and_then_its_release(self):
        rc = f"{NEXT}-rc.1"
        self.bump(rc)
        iss = self.copy.read("installer/citar.iss")
        self.assertIn(f'#define AppVersion "{rc}"', iss)
        self.assertIn(f'#define AppVersionNumeric "{release.Version.parse(NEXT).numeric}"', iss)
        self.assertIn(f"## [{rc}] - {DATE}", self.copy.read("CHANGELOG.md"))
        self.assertEqual(self.problems(rc), [])
        with self.assertRaises(release.Refused):
            self.bump(f"{NEXT}-beta.2")
        with self.assertRaisesRegex(release.Refused, "Unreleased section is empty"):
            self.bump(NEXT)
        log = self.copy.read("CHANGELOG.md")
        self.copy.write("CHANGELOG.md", log.replace("## [Unreleased]\n", f"## [Unreleased]\n\nThe release of {rc}, "
                                                    "unchanged.\n", 1))
        self.bump(NEXT, "2026-11-08")
        url = "https://github.com/jprodgers/CITAR"
        self.assertIn(f"[{NEXT}]: {url}/compare/v{rc}...v{NEXT}\n[{rc}]: {url}/compare/v{SOURCE}...v{rc}\n",
                      self.copy.read("CHANGELOG.md"))
        self.assertEqual(self.problems(NEXT), [])


class Prepare(CopyTest):
    def prepare(self, mode, version=None, date=DATE, env=None):
        environ = {k: v for k, v in os.environ.items() if k != "GITHUB_ACTIONS"}
        environ.update(env or {})
        with mock.patch.dict(os.environ, environ, clear=True):
            return quiet(release.prepare, self.root, mode, version, date, out=print)

    def test_ci_only_checks(self):
        result, output = self.prepare("ci")
        self.assertEqual(result, SOURCE)
        self.assertEqual(self.copy.changed(), {})
        self.assertIn(f"every place says {SOURCE}", output)
        self.copy.write("citar/__init__.py", self.copy.read("citar/__init__.py").replace(f'"{SOURCE}"', f'"{OLD}"'))
        with self.assertRaises(release.Disagrees):
            self.prepare("ci")

    def test_tag_checks_against_its_version(self):
        self.assertEqual(self.prepare("tag", SOURCE)[0], SOURCE)
        with self.assertRaises(release.Disagrees) as caught:
            self.prepare("tag", NEXT)
        self.assertIn(f"{self.source_where}: [workspace.package] version (the source) is {SOURCE}, but the release is "
                      f"{NEXT}", caught.exception.problems)
        with self.assertRaisesRegex(release.Refused, "needs --version"):
            self.prepare("tag")
        self.assertEqual(self.copy.changed(), {})

    def test_dry_run_bumps_the_runners_checkout_then_checks(self):
        result, output = self.prepare("dry-run", NEXT)
        self.assertEqual(result, NEXT)
        self.assertIn(f"bumped this runner's checkout from {SOURCE} to {NEXT}", output)
        self.assertNotIn("::notice", output)
        bumped = self.copy.snapshot()
        # The same files as `bump` writes.
        other = Copy(Path(self._tmp.name + "-bump"))
        self.addCleanup(shutil.rmtree, other.root, True)
        quiet(release.bump, other.root, NEXT, DATE, out=print)
        self.assertEqual(other.snapshot(), bumped)
        # Again: the source now says the version asked for, so it only checks.
        _, output = self.prepare("dry-run", NEXT)
        self.assertNotIn("bumped", output)
        self.assertEqual(self.copy.snapshot(), bumped)

    def test_dry_run_on_actions_leaves_a_notice(self):
        _, output = self.prepare("dry-run", NEXT, env={"GITHUB_ACTIONS": "true"})
        self.assertIn(f"::notice title=release.py prepare::bumped this runner's checkout from {SOURCE} to {NEXT}",
                      output)

    def test_dry_run_refuses_an_older_version_and_needs_one(self):
        with self.assertRaisesRegex(release.Refused, f"{OLD} is not newer"):
            self.prepare("dry-run", OLD)
        with self.assertRaisesRegex(release.Refused, "needs --version"):
            self.prepare("dry-run")
        self.assertEqual(self.copy.changed(), {})

    def test_the_command_line(self):
        code, output = run_main("prepare", "--mode", "tag", "--version", NEXT, "--root", str(self.root))
        self.assertEqual(code, 1)
        self.assertIn(f"{self.source_where}:", output)
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("GITHUB_ACTIONS", None)
            code, output = run_main("prepare", "--mode", "dry-run", "--version", NEXT, "--date", DATE,
                                    "--root", str(self.root))
        self.assertEqual(code, 0, output)
        self.assertEqual(run_main("prepare", "--mode", "nightly", "--root", str(self.root))[0], 2)


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


if __name__ == "__main__":
    unittest.main()
