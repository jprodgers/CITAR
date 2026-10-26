"""scripts/ci/stale_terms.py (design/phase5-release.md P5.16): the retired routes' phrases are refused in the docs, the
units and the manifests, outside the CHANGELOG's history, its Compatibility notes and the sections that describe
0.1.5's layout because a tool acts on it. Standard library only."""
import importlib.util
import io
import shutil
import sys
import tempfile
import tomllib
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "ci" / "stale_terms.py"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module      # dataclasses look their module up while the class is made
    spec.loader.exec_module(module)
    return module


st = _load("stale_terms_under_test", SCRIPT)
release = _load("release_for_stale_terms", ROOT / "scripts" / "release.py")


def run(argv):
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        code = st.main(argv)
    return code, out.getvalue(), err.getvalue()


class Tree:
    """A copy of the files the scan reads, to plant in."""

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-stale-terms-"))
        for path, _kind in st.files(ROOT):
            target = self.dir / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, target)

    def close(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def append(self, path, text):
        target = self.dir / path
        with open(target, "a", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        return len(target.read_text(encoding="utf-8").splitlines())

    def write(self, path, text):
        target = self.dir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8", newline="\n")

    def insert_after(self, path, anchor, text):
        """Insert `text` (one line, or several joined by newlines) after the first line equal to `anchor`; return
        the line number of its first line."""
        target = self.dir / path
        lines = target.read_text(encoding="utf-8").splitlines()
        at = lines.index(anchor) + 1
        lines[at:at] = text.split("\n")
        target.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
        return at + 1


class TheTree(unittest.TestCase):
    def test_the_tree_names_no_retired_route(self):
        code, out, err = run(["--root", str(ROOT)])
        self.assertEqual(code, 0, out + err)
        self.assertIn("no retired route named", out)

    def test_every_exempt_section_exists(self):
        self.assertEqual(st.missing_sections(ROOT), [])

    def test_it_reads_the_docs_the_units_and_the_manifests(self):
        kinds = dict(st.files(ROOT))
        for path, kind in [("README.md", st.DOCS), ("docs/server/RUNBOOK.md", st.DOCS),
                           ("packaging/RELEASING.md", st.DOCS), ("deploy/citar.service", st.UNITS),
                           ("citar/wizard/deploy/citar.service", st.UNITS),
                           ("packaging/homebrew/citar-helper.rb.in", st.MANIFESTS)]:
            self.assertEqual(kinds.get(path), kind, path)
        for path in kinds:
            self.assertFalse(path.startswith(("design/", "wiki/", "crates/")), path)

    def test_a_root_that_is_no_checkout_is_a_usage_error(self):
        empty = Path(tempfile.mkdtemp(prefix="citar-stale-terms-empty-"))
        try:
            code, _out, err = run(["--root", str(empty)])
        finally:
            shutil.rmtree(empty, ignore_errors=True)
        self.assertEqual(code, 2)
        self.assertIn("not a CITAR checkout", err)


class Planted(unittest.TestCase):
    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.close)

    def assertNamed(self, path, line, name):
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 1, out + err)
        self.assertIn(f"{path}:{line}: {name}:", out)

    def assertClean(self):
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 0, out + err)

    def test_each_phrase_planted_in_a_doc_is_named_with_its_file_and_line(self):
        for phrase in st.PHRASES:
            if st.DOCS not in phrase.kinds:
                continue
            with self.subTest(phrase.name):
                tree = Tree()
                try:
                    line = tree.append("docs/INSTALL.md", f"\n{phrase.sample}\n")
                    code, out, err = run(["--root", str(tree.dir)])
                finally:
                    tree.close()
                self.assertEqual(code, 1, out + err)
                self.assertIn(f"docs/INSTALL.md:{line}: {phrase.name}:", out)

    def test_each_sample_is_found_by_its_own_phrase(self):
        for phrase in st.PHRASES:
            with self.subTest(phrase.name):
                self.assertRegex(phrase.sample, phrase.pattern)

    def test_a_phrase_in_a_shell_block_is_named_too(self):
        line = self.tree.append("docs/server/RUNBOOK.md",
                                "\n## Elsewhere\n\n```bash\n# from the laptop\ngit archive HEAD | ssh box tar x\n```\n")
        self.assertNamed("docs/server/RUNBOOK.md", line - 1, "git-archive-deploy")

    def test_a_unit_running_the_server_as_a_module_is_named(self):
        self.tree.write("deploy/citar.service",
                        "[Service]\nWorkingDirectory=/opt/citar\nExecStart=/opt/citar/.venv/bin/python -m citar.server\n")
        code, out, _err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 1)
        self.assertIn("deploy/citar.service:2: unit-in-source-tree:", out)
        self.assertIn("deploy/citar.service:3: server-module:", out)
        self.assertIn("deploy/citar.service:3: source-tree-venv:", out)

    def test_a_manifest_downloading_the_pypi_sdist_is_named(self):
        line = self.tree.append("packaging/homebrew/citar-helper.rb.in",
                                '  url "https://files.pythonhosted.org/packages/source/c/citar/citar-0.1.5.tar.gz"\n')
        self.assertNamed("packaging/homebrew/citar-helper.rb.in", line, "pypi-sdist")

    def test_a_phrase_only_for_docs_is_not_read_in_a_unit(self):
        # deploy/citar.service's comments may say 0.1.6 has no citar-lab unit.
        self.tree.append("deploy/citar.service", "# 0.1.6 has no citar-lab unit\n")
        self.assertClean()

    def test_what_the_new_routes_say_is_not_named(self):
        for text in ["brew install jprodgers/citar/citar-helper", "brew uninstall citar",
                     "`git archive` of `deploy/` and `env.example` at the tag",
                     "pip install --require-hashes -r scripts/ci/verify-manifest-requirements.txt",
                     "ExecStart=/opt/citar/current/venv/bin/citar serve",
                     "python -m citar.server.admin_cli", "https://pypi.org/project/citar/"]:
            with self.subTest(text):
                tree = Tree()
                try:
                    tree.append("docs/FAQ.md", f"\n{text}\n")
                    code, out, err = run(["--root", str(tree.dir)])
                finally:
                    tree.close()
                self.assertEqual(code, 0, out + err)


class Exemptions(unittest.TestCase):
    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.close)

    def test_the_changelogs_history_is_exempt(self):
        self.tree.insert_after("CHANGELOG.md", "## [0.1.5] - 2026-09-22", "sudo systemctl restart citar citar-lab")
        self.tree.insert_after("CHANGELOG.md", "## [0.1.0] - 2026-09-20", "brew install jprodgers/citar/citar")
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 0, out + err)

    def test_the_unreleased_compatibility_notes_are_exempt_and_the_rest_of_unreleased_is_not(self):
        assert_unreleased_rules(self, self.tree)

    def test_an_exempt_section_is_exempt_and_the_next_one_is_not(self):
        lines = (self.tree.dir / "docs/server/DEPLOY.md").read_text(encoding="utf-8").splitlines()
        heading = next(line for line in lines if line.lstrip("#").strip() == "Moving from 0.1.5")
        self.tree.insert_after("docs/server/DEPLOY.md", heading, "git archive HEAD > /tmp/citar.tar")
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 0, out + err)
        line = self.tree.append("docs/server/DEPLOY.md", "\n## Afterwards\n\ngit archive HEAD > /tmp/citar.tar\n")
        self.assertIn(f"docs/server/DEPLOY.md:{line}: git-archive-deploy:", run(["--root", str(self.tree.dir)])[1])

    def test_a_hash_line_in_a_code_block_ends_no_section(self):
        # The staging rehearsal's own shell comments must not close its section.
        text = ("\n## The staging rehearsal\n\n```bash\n# the 0.1.5 layout\n```\n\n"
                "Staged as the box has it: citar-lab beside citar.\n")
        lines = st.sections(text.splitlines())
        self.assertEqual(lines[-1], ("The staging rehearsal",))
        self.assertEqual(lines[4], ("The staging rehearsal",))

    def test_an_exemption_naming_a_heading_that_is_gone_fails(self):
        path = self.tree.dir / "docs/server/DEPLOY.md"
        path.write_text(path.read_text(encoding="utf-8").replace("Moving from 0.1.5", "Coming from the old one"),
                        encoding="utf-8", newline="\n")
        code, out, _err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 1)
        self.assertIn("no heading 'Moving from 0.1.5'", out)


def assert_unreleased_rules(test, tree):
    """Unreleased's Compatibility notes are exempt and its other subsections are not. The test plants subsections of
    its own right under ``## [Unreleased]``, so it holds whatever the live CHANGELOG's Unreleased says, empty included
    (as it is after ``release.py bump``)."""
    first = tree.insert_after("CHANGELOG.md", "## [Unreleased]", "\n".join([
        "", "### Compatibility", "", "- `brew install jprodgers/citar/citar` is gone.",
        "", "### Added", "", "- brew install jprodgers/citar/citar"]))
    compatibility, added = first + 3, first + 7
    code, out, err = run(["--root", str(tree.dir)])
    test.assertEqual(code, 1, out + err)
    test.assertIn(f"CHANGELOG.md:{added}: homebrew-game:", out)
    test.assertNotIn(f"CHANGELOG.md:{compatibility}:", out)
    test.assertEqual(out.count("CHANGELOG.md:"), 1, out)


class AfterABump(unittest.TestCase):
    """``release.py bump`` turns Unreleased into the release's own section under a new, empty Unreleased: the first step
    of every release run (a dry run's too, on each test leg's checkout) and of 5-13's bumped clone. The scan, and
    these tests, must hold on that tree as well as on the branch's. The copy is bumped by release.py itself, so a
    change to the heading it writes is caught here."""

    DATE = "2026-01-01"

    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.close)
        # what release.py reads besides the scanned files: the files of its places and each member's manifest
        members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
        for rel in (*release.Tree(ROOT).files, *(f"{member}/Cargo.toml" for member in members)):
            target = self.tree.dir / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / rel, target)
        log = release.Text(self.tree.dir, "CHANGELOG.md")
        start, end = release.section_lines(log, "Unreleased")
        if not "".join(log.lines[start:end]).strip():   # a checkout bumped already: give the next bump a line
            self.tree.insert_after("CHANGELOG.md", "## [Unreleased]", "\n- A change for the tests.")
        source = release.Version.parse(release.Tree(self.tree.dir).source.value)
        self.version = f"{source.major}.{source.minor}.{source.patch + 1}"
        with redirect_stdout(io.StringIO()):
            release.bump(self.tree.dir, self.version, self.DATE, out=print)
        self.heading = f"## [{self.version}] - {self.DATE}"
        lines = (self.tree.dir / "CHANGELOG.md").read_text(encoding="utf-8").splitlines()
        unreleased = lines.index("## [Unreleased]")
        self.assertEqual([line for line in lines[unreleased + 1:] if line.strip()][0], self.heading,
                         "release.py bump leaves Unreleased empty, above the release's section")

    def test_the_bumped_tree_names_no_retired_route(self):
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 0, out + err)

    def test_the_release_s_section_is_history(self):
        self.tree.insert_after("CHANGELOG.md", self.heading, "\n### Added\n\n- brew install jprodgers/citar/citar")
        code, out, err = run(["--root", str(self.tree.dir)])
        self.assertEqual(code, 0, out + err)

    def test_the_new_unreleased_section_is_scanned(self):
        assert_unreleased_rules(self, self.tree)


if __name__ == "__main__":
    unittest.main()
