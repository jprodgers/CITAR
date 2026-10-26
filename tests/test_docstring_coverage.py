"""scripts/docstring_coverage.py: which modules it reads, wherever the checkout is.

Standard library only: each test copies the script into a small tree of its own and runs it there, the way
tests/test_check_links.py holds check_links.py, which had the same defect (5-13).
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "docstring_coverage.py"

DOCUMENTED = '"""A module that says what it is for."""\n\n\ndef ok():\n    """Documented."""\n'
UNDOCUMENTED = "def bare():\n    return 1\n"


def tree(base: Path) -> Path:
    """A checkout whose citar/ holds one documented module and one with no docstring at all, and generated output
    inside the package that the script must still skip."""
    (base / "scripts").mkdir(parents=True)
    shutil.copy2(SCRIPT, base / "scripts" / "docstring_coverage.py")
    (base / "citar").mkdir()
    (base / "citar" / "good.py").write_text(DOCUMENTED, encoding="utf-8")
    (base / "citar" / "mod.py").write_text(UNDOCUMENTED, encoding="utf-8")
    # A build folder inside the package is still skipped: it holds a copy, not the source.
    (base / "citar" / "build").mkdir()
    (base / "citar" / "build" / "copy.py").write_text(UNDOCUMENTED, encoding="utf-8")
    return base


def run(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-I", str(root / "scripts" / "docstring_coverage.py"), *args],
                          capture_output=True, text=True, cwd=root)


class WhereTheCheckoutIs(unittest.TestCase):
    def test_a_checkout_inside_a_folder_named_like_a_skipped_one_is_read_whole(self):
        """It tested every part of a module's absolute path against its skip list, so a checkout under a folder named
        build, dist, site, wiki or ops read nothing ("total 0 0 0%") and --strict passed. Only the parts below the
        folder it walks count."""
        for above in ("build", "dist", "site", "wiki", "ops"):
            with self.subTest(above=above), tempfile.TemporaryDirectory() as tmp:
                root = tree(Path(tmp) / above / "checkout")
                done = run(root, "--strict")
                out = done.stdout.replace("\\", "/")
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn("1 module(s) with no docstring:", out)
                self.assertIn("  citar/mod.py", out)
                self.assertNotIn("citar/build/copy.py", out)
                self.assertRegex(out, r"total\s+2\s+1\s+50%")

    def test_a_documented_tree_passes_and_generated_output_inside_it_is_still_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = tree(Path(tmp) / "build" / "checkout")
            (root / "citar" / "mod.py").write_text(DOCUMENTED, encoding="utf-8")
            done = run(root, "--strict")
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertNotIn("copy.py", done.stdout)
            self.assertRegex(done.stdout, r"total\s+2\s+2\s+100%")

    def test_a_folder_named_on_the_command_line_is_walked_below_itself_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = tree(Path(tmp) / "dist" / "checkout")
            done = run(root, "citar", "--list")
            out = done.stdout.replace("\\", "/")
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("citar/mod.py  (1 of 1)", out)
            self.assertNotIn("copy.py", out)


if __name__ == "__main__":
    unittest.main()
