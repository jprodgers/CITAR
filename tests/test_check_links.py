"""scripts/check_links.py (S4, test.yml's lint, RELEASING.md's step 2): which pages it reads, wherever the checkout is.

Standard library only: each test copies the script into a small tree of its own and runs it there, as the checklist
runs it in a release branch's checkout.
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "check_links.py"


def tree(base: Path, *, broken: bool) -> Path:
    """A checkout with a README, a page under docs/ and one under packaging/, and generated pages it must skip."""
    (base / "scripts").mkdir(parents=True)
    shutil.copy2(SCRIPT, base / "scripts" / "check_links.py")
    (base / "README.md").write_text("# Read me\n\n[The guide](docs/guide.md#install)\n", encoding="utf-8")
    (base / "docs").mkdir()
    link = "missing.md" if broken else "../README.md#read-me"
    (base / "docs" / "guide.md").write_text(f"# Guide\n\n## Install\n\n[Back]({link})\n", encoding="utf-8")
    (base / "packaging").mkdir()
    (base / "packaging" / "RELEASING.md").write_text("# Releasing\n\n[The guide](../docs/guide.md)\n",
                                                     encoding="utf-8")
    # Generated output inside the checkout is still skipped: a site build's page with a broken link.
    (base / "docs" / "site").mkdir()
    (base / "docs" / "site" / "copy.md").write_text("[Gone](nowhere.md)\n", encoding="utf-8")
    return base


def run(root: Path) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-I", str(root / "scripts" / "check_links.py"), "--strict"],
                          capture_output=True, text=True, cwd=root)


class WhereTheCheckoutIs(unittest.TestCase):
    def test_a_checkout_inside_a_folder_named_like_a_skipped_one_is_read_whole(self):
        """5-13's bumped clone lived in C:/dev/target/citar-p-5-13/bumpclone: every page's absolute path had a
        "target" part, so every page under docs/, packaging/ and crates/ was skipped and the check passed on the
        top-level files alone ("8 files"). Only the parts below the checkout count."""
        for above in ("target", "build", "dist", "site", "ops", "wiki"):
            with self.subTest(above=above), tempfile.TemporaryDirectory() as tmp:
                root = tree(Path(tmp) / above / "checkout", broken=True)
                done = run(root)
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn("docs/guide.md: [Back](missing.md) - no such file",
                              done.stdout.replace("\\", "/"))
                self.assertIn("3 files", done.stdout)

    def test_a_good_tree_passes_and_generated_output_inside_it_is_still_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = tree(Path(tmp) / "target" / "checkout", broken=False)
            done = run(root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("3 files, 3 internal links", done.stdout)
            self.assertIn("Every internal link resolves.", done.stdout)


if __name__ == "__main__":
    unittest.main()
