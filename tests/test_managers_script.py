"""scripts/release/managers.sh: a release's Homebrew tap and Scoop bucket updated from its manifests
(design/phase5-release.md P5.15 step 8, package 5-09).

Every run here is against local throwaway repositories standing in for jprodgers/homebrew-citar and
jprodgers/scoop-citar (bare repositories seeded as 0.1.5 left them: the tap's Formula/citar.rb, the bucket's
bucket/citar.json), with git's global and system configuration set aside, so nothing here can reach GitHub or use this
machine's credentials. The manifests are the real templates filled in by scripts/release_checksums.py. ``--plan`` (the
default) shows the diff and pushes nothing; ``--push`` commits and pushes into the stand-ins.
"""
from __future__ import annotations

import importlib.util
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "release" / "managers.sh"
VERSION = "0.1.6"


def _load(name: str, path: Path):
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


checksums = _load("citar_release_checksums", ROOT / "scripts" / "release_checksums.py")


def find_bash() -> str | None:
    """bash: on Windows Git Bash's, found beside git, never System32's bash.exe (WSL's, which would run elsewhere)."""
    if os.name != "nt":
        return shutil.which("bash")
    git = shutil.which("git")
    if not git:
        return None
    for base in Path(git).resolve().parents:
        candidate = base / "bin" / "bash.exe"
        if candidate.is_file() and "system32" not in str(candidate).lower():
            return str(candidate)
    return None


BASH = find_bash()


def remove_tree(path: Path) -> None:
    """``path`` removed whole. git writes its object files read-only, which Windows will not delete, so each one
    refused is made writable and removed again; ``ignore_errors`` alone would leave every repository behind."""

    def writable_then_again(func, name, *_):
        os.chmod(name, stat.S_IWRITE)
        func(name)

    if sys.version_info >= (3, 12):
        shutil.rmtree(path, onexc=writable_then_again)
    else:  # 3.11, the suite's floor: onexc is 3.12's
        shutil.rmtree(path, onerror=writable_then_again)


def msys(path: Path | str) -> str:
    """A path as bash reads it: on Windows Git Bash's /c/... form, elsewhere as it is."""
    text = str(path)
    if os.name != "nt":
        return text
    text = text.replace("\\", "/")
    if re.match(r"^[A-Za-z]:/", text):
        text = "/" + text[0].lower() + text[2:]
    return text


@unittest.skipUnless(BASH and shutil.which("git"), "needs bash (Git Bash on Windows) and git")
class Managers(unittest.TestCase):
    def setUp(self):
        base = os.environ.get("CARGO_TARGET_DIR")
        if base:
            Path(base).mkdir(parents=True, exist_ok=True)
        self.dir = Path(tempfile.mkdtemp(prefix="citar-managers-test-", dir=base or None))
        self.addCleanup(lambda: self.dir.exists() and remove_tree(self.dir))
        empty = self.dir / "gitconfig"
        empty.write_text("", encoding="utf-8")
        (self.dir / "tmp").mkdir()
        # git with nothing of this machine's: no global or system config, no credential helper, no signing
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(GIT_CONFIG_GLOBAL=str(empty), GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0",
                        GIT_AUTHOR_NAME="Release Test", GIT_AUTHOR_EMAIL="release-test@example.invalid",
                        GIT_COMMITTER_NAME="Release Test", GIT_COMMITTER_EMAIL="release-test@example.invalid",
                        TMPDIR=msys(self.dir / "tmp"))
        self.tap = self.seed("homebrew-citar", {"Formula/citar.rb": "class Citar < Formula\nend\n",
                                                 "README.md": "# The tap\n"})
        self.bucket = self.seed("scoop-citar", {"bucket/citar.json": '{"version": "0.1.5"}\n',
                                                "README.md": "# The bucket\n"})
        self.manifests = self.dir / "managers"
        names = checksums.needed(ROOT, VERSION)
        checksums.write_manifests(ROOT, VERSION, "2026-11-01", dict.fromkeys(names, "ab" * 32),
                                  checksums.base_url(None, VERSION), self.manifests)

    def git(self, *args, cwd=None) -> str:
        done = subprocess.run(["git", *args], cwd=cwd, env=self.env, capture_output=True, text=True,
                              encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        return done.stdout

    def seed(self, name: str, files: dict) -> Path:
        """A bare repository holding ``files`` in one commit: a stand-in for one of ours on GitHub."""
        work = self.dir / f"{name}-seed"
        for rel, text in files.items():
            (work / rel).parent.mkdir(parents=True, exist_ok=True)
            (work / rel).write_text(text, encoding="utf-8", newline="\n")
        self.git("init", "--quiet", "--initial-branch=main", str(work))
        self.git("add", "-A", cwd=work)
        self.git("commit", "--quiet", "-m", "0.1.5", cwd=work)
        bare = self.dir / f"{name}.git"
        self.git("clone", "--quiet", "--bare", str(work), str(bare))
        return bare

    def state(self, bare: Path) -> str:
        return self.git("for-each-ref", "--format=%(refname) %(objectname)", cwd=bare)

    def run_script(self, *args, manifests: Path | None = None) -> subprocess.CompletedProcess:
        argv = [BASH, msys(SCRIPT), *args]
        if "--tap" not in args:
            argv += ["--tap", msys(self.tap), "--bucket", msys(self.bucket)]
        if manifests is not False:
            argv += ["--manifests", msys(manifests or self.manifests)]
        return subprocess.run(argv, env=self.env, capture_output=True, text=True, encoding="utf-8",
                              errors="replace", timeout=120)

    def test_plan_shows_the_diff_and_pushes_nothing(self):
        before = (self.state(self.tap), self.state(self.bucket))
        done = self.run_script(VERSION, "--plan")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        out = done.stdout
        self.assertIn("+++ b/Formula/citar-helper.rb", out)
        self.assertIn("deleted file mode", out)
        self.assertIn("--- a/Formula/citar.rb", out)
        self.assertIn("+++ b/bucket/citar-helper.json", out)
        self.assertIn("bucket/citar.json", out)
        self.assertIn(f"/releases/download/v{VERSION}/citar-helper-{VERSION}-macos-arm64.tar.gz", out)
        self.assertIn("not pushed (--plan)", out)
        self.assertIn("wingetcreate submit", out)
        self.assertEqual((self.state(self.tap), self.state(self.bucket)), before, "nothing reached the repositories")
        self.assertEqual(list((self.dir / "tmp").iterdir()), [], "the temporary clones are gone")
        # --plan is the default
        done = self.run_script(VERSION)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("not pushed (--plan)", done.stdout)
        self.assertEqual((self.state(self.tap), self.state(self.bucket)), before)

    def test_push_commits_the_release_into_each(self):
        done = self.run_script(VERSION, "--push")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        tap = self.git("ls-tree", "-r", "--name-only", "main", cwd=self.tap).split()
        self.assertEqual(sorted(tap), ["Formula/citar-helper.rb", "README.md"])
        bucket = self.git("ls-tree", "-r", "--name-only", "main", cwd=self.bucket).split()
        self.assertEqual(sorted(bucket), ["README.md", "bucket/citar-helper.json", "bucket/citar.json"])
        self.assertEqual(self.git("show", "main:Formula/citar-helper.rb", cwd=self.tap),
                         (self.manifests / "homebrew" / "citar-helper.rb").read_text(encoding="utf-8"))
        self.assertEqual(self.git("log", "-1", "--format=%s", "main", cwd=self.bucket).strip(), f"CITAR {VERSION}")
        # again: nothing left to change, and nothing pushed
        before = (self.state(self.tap), self.state(self.bucket))
        done = self.run_script(VERSION, "--push")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual(done.stdout.count("nothing to change"), 2, done.stdout)
        self.assertEqual((self.state(self.tap), self.state(self.bucket)), before)

    def test_manifests_that_are_not_the_releases_are_refused_before_any_clone(self):
        other = self.dir / "other"
        names = checksums.needed(ROOT, "0.1.7")
        checksums.write_manifests(ROOT, "0.1.7", "2026-12-01", dict.fromkeys(names, "cd" * 32),
                                  checksums.base_url(None, "0.1.7"), other)
        done = self.run_script(VERSION, manifests=other)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("citar-helper.rb: its URLs are not 0.1.6's release", done.stderr)
        self.assertIn('citar.json: its "version" is not 0.1.6', done.stderr)
        self.assertIn("nothing was cloned", done.stderr)
        (self.manifests / "scoop" / "citar-helper.json").unlink()
        text = (self.manifests / "scoop" / "citar.json").read_text(encoding="utf-8")
        (self.manifests / "scoop" / "citar.json").write_text(text.replace('"license"', '"{{sha256:x}}"'),
                                                              encoding="utf-8")
        done = self.run_script(VERSION)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("citar-helper.json: missing", done.stderr)
        self.assertIn("citar.json: a template placeholder is left in it", done.stderr)
        self.assertNotIn("cloning", done.stdout)

    def test_usage(self):
        for args, said in ((["--plan"], "which version"), (["v0.1.6"], "not a release version"),
                           (["0.1.6-rc1"], "not a release version"), ([VERSION, "--plan", "--push"], "one or the other"),
                           ([VERSION, "--force"], "unknown option"), ([VERSION, "0.1.7"], "one version")):
            with self.subTest(args=args):
                done = self.run_script(*args)
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn(said, done.stderr)
        done = self.run_script("--help", manifests=False)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("--push", done.stdout)

    def test_its_folder_is_removed_whole(self):
        # The cleanup's own call, on the stand-ins setUp made: git's read-only object files included (Windows).
        self.git("clone", "--quiet", str(self.tap), str(self.dir / "a-clone"))
        objects = [p for p in (self.dir / "homebrew-citar.git" / "objects").rglob("*") if p.is_file()]
        self.assertTrue(objects)
        remove_tree(self.dir)
        self.assertFalse(self.dir.exists(), "nothing of the test is left behind")

    def test_the_defaults_are_ours_and_packaging(self):
        # What a run without --tap and --bucket would clone (never run here: it would reach GitHub).
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("TAP_URL=https://github.com/jprodgers/homebrew-citar.git\n", text)
        self.assertIn("BUCKET_URL=https://github.com/jprodgers/scoop-citar.git\n", text)
        self.assertIn('manifests="$root/packaging"\n', text)
        self.assertNotIn("\r\n", SCRIPT.read_bytes().decode("utf-8"), "LF, as .gitattributes keeps *.sh")


if __name__ == "__main__":
    unittest.main()
