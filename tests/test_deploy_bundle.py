"""``scripts/deploy/bundle.py`` (design/phase5-release.md P5.13.5, package 5-10b's gate 4): the deploy bundle.

- ``--from-dir`` makes a bundle whose ``SHA256SUMS`` checks: the wheel and constraints, ``deploy/`` and
  ``env.example`` from ``git archive``, ``BUNDLE``; the same inputs give the same bytes; a wheel whose hash differs
  from the folder's ``SHA256SUMS`` (or its ``.sha256``) is refused and nothing is written.
- The published path runs against a stand-in ``gh`` (``CITAR_BUNDLE_GH``): the arguments of ``release download`` and
  ``attestation verify`` are exactly the ones it must pass, a download that does not match the release's sums is
  refused, and so is an attestation ``gh`` does not verify.
- A bundle is never written into a git working tree.

Standard library only; the git archive is of this checkout's HEAD (``--ref``), so it runs wherever the suite does.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "deploy" / "bundle.py"


def _load():
    spec = importlib.util.spec_from_file_location("citar_deploy_bundle_for_tests", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bundle = _load()
WHEEL = "citar-0.1.6-cp311-abi3-manylinux_2_28_x86_64.whl"
CONSTRAINTS = "citar-0.1.6-server-constraints.txt"


def scratch(prefix: str) -> Path:
    """A temporary folder outside every git working tree: under CARGO_TARGET_DIR when it is set, else the system's;
    resolved (a runner's temporary folder can have a second name)."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base or None)).resolve()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


#: The stand-in gh: each call's arguments appended to STUB_GH_LOG as JSON; `release download` copies the patterns
#: from STUB_GH_ASSETS into --dir; `attestation verify` fails when STUB_GH_ATTEST_FAIL names the file.
STUB_GH = r'''
import json
import os
import shutil
import sys

args = sys.argv[1:]
with open(os.environ["STUB_GH_LOG"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(args) + "\n")
if args[:2] == ["release", "download"]:
    folder = args[args.index("--dir") + 1]
    for i, arg in enumerate(args):
        if arg == "--pattern":
            source = os.path.join(os.environ["STUB_GH_ASSETS"], args[i + 1])
            if os.path.exists(source):
                shutil.copy(source, folder)
    sys.exit(0)
if args[:2] == ["attestation", "verify"]:
    if os.environ.get("STUB_GH_ATTEST_FAIL") and args[2].endswith(os.environ["STUB_GH_ATTEST_FAIL"]):
        print("Error: verifying with issuer: no attestations found", file=sys.stderr)
        sys.exit(1)
    print("Loaded digest sha256:... for file://" + args[2])
    sys.exit(0)
sys.exit(2)
'''


class Bundle(unittest.TestCase):

    def setUp(self):
        self.tmp = scratch("citar-bundle-test-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.assets = self.tmp / "assets"
        self.assets.mkdir()
        self.wheel = b"PK\x03\x04 a stand-in wheel " * 64
        self.constraints = b"fastapi==0.200.0\nuvicorn==0.40.0\n"
        (self.assets / WHEEL).write_bytes(self.wheel)
        (self.assets / CONSTRAINTS).write_bytes(self.constraints)
        (self.assets / "SHA256SUMS").write_text(
            f"{sha256(self.wheel)}  {WHEEL}\n{sha256(self.constraints)}  {CONSTRAINTS}\n"
            f"{'0' * 64}  citar-0.1.6.tar.gz\n", encoding="utf-8", newline="\n")
        self.out = self.tmp / "out"
        self.gh_log = self.tmp / "gh.jsonl"
        (self.tmp / "gh.py").write_text(STUB_GH, encoding="utf-8")

    def run_bundle(self, *args: str, **env: str) -> tuple[int, str]:
        environ = {k: v for k, v in os.environ.items() if not k.startswith("CITAR_")}
        environ.update(CITAR_BUNDLE_GH=str(self.tmp / "gh.py"), STUB_GH_LOG=str(self.gh_log),
                       STUB_GH_ASSETS=str(self.assets), **env)
        done = subprocess.run([sys.executable, str(SCRIPT), *args, "--out", str(self.out)], env=environ,
                              capture_output=True, text=True, timeout=300)
        return done.returncode, done.stdout + done.stderr

    def unpacked(self, version: str = "0.1.6") -> tuple[Path, dict[str, bytes]]:
        target = self.out / f"citar-deploy-{version}.tar.gz"
        data = target.read_bytes()
        self.assertEqual((self.out / f"{target.name}.sha256").read_text(encoding="utf-8"),
                         f"{sha256(data)}  {target.name}\n")
        files = {}
        with tarfile.open(target) as tar:
            for member in tar.getmembers():
                self.assertTrue(member.name.startswith(f"citar-deploy-{version}"), member.name)
                self.assertEqual((member.uid, member.gid, member.uname, member.gname), (0, 0, "root", "root"))
                if member.isfile():
                    files[member.name.split("/", 1)[1]] = tar.extractfile(member).read()
        return target, files

    def assertSumsCheck(self, files: dict[str, bytes]):
        lines = files["SHA256SUMS"].decode().splitlines()
        listed = {}
        for line in lines:
            digest, name = line.split("  ", 1)
            listed[name] = digest
        self.assertEqual(sorted(listed), sorted(n for n in files if n != "SHA256SUMS"),
                         "SHA256SUMS lists every other file, and nothing more")
        for name, digest in listed.items():
            self.assertEqual(sha256(files[name]), digest, name)

    def test_from_dir_makes_a_bundle_whose_sums_check(self):
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 0, out)
        target, files = self.unpacked()
        self.assertSumsCheck(files)
        self.assertEqual(files[WHEEL], self.wheel)
        self.assertEqual(files[CONSTRAINTS], self.constraints)
        for name in bundle.NEEDED:
            self.assertIn(name, files)
        head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True,
                              check=True).stdout.strip()
        committed = subprocess.run(["git", "-C", str(ROOT), "show", "HEAD:deploy/citar-deploy.sh"],
                                   capture_output=True, check=True).stdout
        self.assertEqual(files["deploy/citar-deploy.sh"], committed, "deploy/ is git archive's, at the ref")
        info = dict(line.split("=", 1) for line in files["BUNDLE"].decode().splitlines())
        self.assertEqual((info["version"], info["pep440"], info["wheel"]), ("0.1.6", "0.1.6", WHEEL))
        self.assertEqual(info["deploy"], f"git archive HEAD ({head})")
        self.assertIn("each as SHA256SUMS says", info["source"])
        self.assertTrue(info["attestation"].startswith("none:"), info["attestation"])
        self.assertNotIn("citar-0.1.6.tar.gz", files, "only the wheel and constraints come from the release")
        self.assertEqual(self.gh_log.exists(), False, "gh was run for a --from-dir bundle")
        # The same inputs, the same bytes, on any OS: the members and the sums in byte order of their names.
        with tarfile.open(target) as tar:
            order = tar.getnames()
        self.assertEqual(order, sorted(order, key=str.encode))
        listed = [line.split("  ", 1)[1] for line in files["SHA256SUMS"].decode().splitlines()]
        self.assertEqual(listed, sorted(listed, key=str.encode))
        self.assertEqual(listed[:2], ["BUNDLE", WHEEL])
        first = target.read_bytes()
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 0, out)
        self.assertEqual(target.read_bytes(), first)
        self.assertEqual(sorted(p.name for p in self.out.iterdir()),
                         ["citar-deploy-0.1.6.tar.gz", "citar-deploy-0.1.6.tar.gz.sha256"], "a work folder was left")
        tar = shutil.which("tar")
        if sys.platform.startswith("linux") and tar and shutil.which("sha256sum"):
            unpack = self.tmp / "unpack"
            unpack.mkdir()
            subprocess.run([tar, "-xzf", str(target), "-C", str(unpack)], check=True)
            check = subprocess.run(["sha256sum", "-c", "SHA256SUMS"], cwd=unpack / "citar-deploy-0.1.6",
                                   capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
            self.assertTrue(os.access(unpack / "citar-deploy-0.1.6" / "deploy" / "citar-deploy.sh", os.X_OK))

    def test_a_tampered_wheel_is_refused_and_nothing_written(self):
        tampered = bytearray(self.wheel)
        tampered[10] ^= 1
        (self.assets / WHEEL).write_bytes(bytes(tampered))
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"refused: {WHEEL}: its SHA-256 is {sha256(bytes(tampered))}, and {self.assets / 'SHA256SUMS'} "
                      f"says {sha256(self.wheel)}", out)
        self.assertFalse(self.out.exists() and any(self.out.iterdir()), "something was written")
        # ... and against a .sha256 beside it, when the folder has no SHA256SUMS.
        (self.assets / "SHA256SUMS").unlink()
        (self.assets / f"{WHEEL}.sha256").write_text(f"{sha256(self.wheel)}  {WHEEL}\n", encoding="utf-8")
        (self.assets / f"{CONSTRAINTS}.sha256").write_text(f"{sha256(self.constraints)}  {CONSTRAINTS}\n",
                                                           encoding="utf-8")
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{WHEEL}: its SHA-256 is {sha256(bytes(tampered))}, and {WHEEL}.sha256 says", out)

    def test_a_local_build_has_nothing_to_check_against_and_says_so(self):
        (self.assets / "SHA256SUMS").unlink()
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 0, out)
        _, files = self.unpacked()
        self.assertIn("nothing to check them against (a local build)", files["BUNDLE"].decode())
        (self.assets / f"{WHEEL}.sha256").write_text(f"{sha256(self.wheel)}  {WHEEL}\n", encoding="utf-8")
        code, out = self.run_bundle("0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("has a .sha256 for some of", out)

    def test_the_published_path_passes_gh_exactly_these_arguments(self):
        code, out = self.run_bundle("0.1.6", "--ref", "HEAD")
        self.assertEqual(code, 0, out)
        calls = [json.loads(line) for line in self.gh_log.read_text(encoding="utf-8").splitlines()]
        download, *verify = calls
        folder = download[download.index("--dir") + 1]
        self.assertEqual(download, ["release", "download", "v0.1.6", "--repo", "jprodgers/CITAR", "--dir", folder,
                                    "--pattern", WHEEL, "--pattern", CONSTRAINTS, "--pattern", "SHA256SUMS"])
        self.assertEqual(verify, [["attestation", "verify", str(Path(folder) / WHEEL), "--repo", "jprodgers/CITAR"],
                                  ["attestation", "verify", str(Path(folder) / CONSTRAINTS), "--repo",
                                   "jprodgers/CITAR"]])
        self.assertTrue(Path(folder).is_relative_to(self.out), "the download went outside --out")
        self.assertFalse(Path(folder).exists(), "the download folder was left")
        _, files = self.unpacked()
        self.assertSumsCheck(files)
        info = dict(line.split("=", 1) for line in files["BUNDLE"].decode().splitlines())
        self.assertEqual(info["source"], "release v0.1.6 of jprodgers/CITAR (gh release download), each as SHA256SUMS "
                                         "says in the release's SHA256SUMS")
        self.assertEqual(info["attestation"], f"verified (gh attestation verify --repo jprodgers/CITAR) for {WHEEL} "
                                              f"and {CONSTRAINTS}")

    def test_the_published_path_refuses_a_bad_download_and_an_unverified_attestation(self):
        code, out = self.run_bundle("0.1.6", "--ref", "HEAD", STUB_GH_ATTEST_FAIL=CONSTRAINTS)
        self.assertEqual(code, 1, out)
        self.assertIn("refused: gh attestation verify ", out)
        self.assertIn("no attestations found", out)
        self.assertFalse((self.out / "citar-deploy-0.1.6.tar.gz").exists())
        (self.assets / CONSTRAINTS).write_bytes(b"fastapi==0.1\n")
        code, out = self.run_bundle("0.1.6", "--ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"refused: {CONSTRAINTS}: its SHA-256 is", out)
        self.assertEqual([json.loads(line)[:2] for line in self.gh_log.read_text(encoding="utf-8").splitlines()][-1],
                         ["release", "download"], "an attestation was asked for a file whose hash was wrong")
        (self.assets / WHEEL).unlink()
        code, out = self.run_bundle("0.1.6", "--ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"gh release download v0.1.6 did not write {WHEEL}", out)

    def test_never_into_a_git_working_tree_and_a_version_checked(self):
        environ = {k: v for k, v in os.environ.items() if not k.startswith("CITAR_")}
        done = subprocess.run([sys.executable, str(SCRIPT), "0.1.6", "--from-dir", str(self.assets), "--ref", "HEAD",
                               "--out", str(ROOT / "dist-bundle-test")], env=environ, capture_output=True, text=True,
                              timeout=300)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("is inside a git working tree: a bundle is never written into a checkout", done.stderr)
        self.assertFalse((ROOT / "dist-bundle-test").exists())
        code, out = self.run_bundle("0.1.6;rm", "--from-dir", str(self.assets))
        self.assertEqual(code, 2, out)
        self.assertIn("is not a release version", out)

    def test_a_prerelease_s_names(self):
        self.assertEqual(bundle.names("0.1.7-rc.1", "x86_64"),
                         ("citar-0.1.7rc1-cp311-abi3-manylinux_2_28_x86_64.whl",
                          "citar-0.1.7rc1-server-constraints.txt"))
        self.assertEqual(bundle.names("0.1.6", "aarch64")[0], "citar-0.1.6-cp311-abi3-manylinux_2_28_aarch64.whl")

    def test_the_needed_files_are_citar_deploy_s(self):
        text = (ROOT / "deploy" / "citar-deploy.sh").read_text(encoding="utf-8")
        body = text[text.index("\nBUNDLE_NEEDS=(") + len("\nBUNDLE_NEEDS=("):]
        body = body[:body.index(")\n")]
        self.assertEqual(sorted(body.split()), sorted(bundle.NEEDED))


if __name__ == "__main__":
    unittest.main()
