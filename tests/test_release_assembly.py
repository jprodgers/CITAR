"""A release assembled and its package managers' manifests (design/phase5-release.md P5.9 and P5.10, package 5-08):
scripts/ci/assemble_release.py on a fake release (every asset a few bytes, every report as its build writes it),
scripts/release_checksums.py's templates, its ``--from-dir`` manifests and its ``--verify`` against a loopback copy
of the release with a PyPI JSON stand-in, and scripts/ci/check_managers.py's structure and schema checks. The managers
themselves (Scoop, winget, Homebrew) run on CI's runners only (package.yml's ``managers`` job): here their commands
are refused outside a runner.
"""
from __future__ import annotations

import hashlib
import http.server
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent


def _load(name: str, path: Path):
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


release = _load("citar_release_for_checksums", ROOT / "scripts" / "release.py")
checksums = _load("citar_release_checksums", ROOT / "scripts" / "release_checksums.py")
assemble = _load("citar_ci_assemble_release", ROOT / "scripts" / "ci" / "assemble_release.py")
managers = _load("citar_ci_check_managers", ROOT / "scripts" / "ci" / "check_managers.py")
helper_release = sys.modules["citar_ci_helper_release"]
smoke_wheel = sys.modules["citar_ci_smoke_wheel"]

#: The release the fakes are of: the tree's own version, so release.py notes finds its CHANGELOG section.
VERSION = release.check(ROOT)[1]
LABEL = "ci+abc1234"
BUILD, JOBS, PROTOCOL = "0b412a18fe95", "b2a9ff8212e2", 2
RESULT, SPEC = "4914910c" + "0" * 56, "a070d12c" + "1" * 56
#: The assets whose .sha256 their build writes beside them (pkg-windows, pkg-helper-*, pkg-helper-packaged-*).
SIDES_FROM_BUILDS = ("windows", "helper-packaged")


def scratch(prefix: str) -> Path:
    """A temporary folder: under CARGO_TARGET_DIR when it is set (a package's own folder on the laptop)."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base, "tmp").mkdir(parents=True, exist_ok=True)
        return Path(tempfile.mkdtemp(prefix=prefix, dir=Path(base, "tmp")))
    return Path(tempfile.mkdtemp(prefix=prefix))


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def quiet(func, *args, **kwargs):
    """``func``'s exit status, its output kept: (status, stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        code = func(*args, **kwargs)
    return code, out.getvalue(), err.getvalue()


class FakeRelease:
    """The folders package.yml's ``assemble`` job downloads: ``release/`` (every asset, the builds' own ``.sha256``
    files beside theirs) and ``reports/`` (every report, of one build)."""

    def __init__(self, base: Path, version: str = VERSION, label: str = LABEL):
        self.base, self.version, self.label = base, version, label
        self.release, self.reports, self.preview = base / "release", base / "reports", base / "preview"
        self.release.mkdir(parents=True)
        self.reports.mkdir(parents=True)
        for asset in release.assets(version):
            data = f"{asset['name']} of {version}\n".encode()
            (self.release / asset["name"]).write_bytes(data)
            helper = asset["group"] == "helper" and asset["name"] != "citar-helper-manifest.json"
            if asset["group"] in SIDES_FROM_BUILDS or helper:
                (self.release / asset["sha256"]).write_text(sha(data) + "\n", encoding="ascii", newline="\n")
        wheels = assemble.wheel_names(version)
        for platform in release.HELPER_PLATFORMS:
            for py, full in (("3.11", "3.11.9"), ("3.14", "3.14.6")):
                self.write(f"smoke-{platform}-{py}.json", self.smoke(platform, full, wheels[platform], label), True)
            self.write(f"helper-{platform}.json", self.helper(platform))
            self.write(f"install-{platform}.json", self.install(platform))
        pep440 = release.Version.parse(version).pep440
        self.write("smoke-sdist.json", self.smoke("sdist-linux-x64", "3.12.3",
                                                  f"citar-{pep440}-cp311-abi3-linux_x86_64.whl", "unknown"), True)
        self.write("windows-bundle.json", {"version": version, "label": label, "build_id": BUILD, "jobs_build": JOBS,
                                           "jobs": f"verifiable, jobs build {JOBS}"})
        self.write("windows-installed.json", {"version": version})
        self.write("windows-uninstalled.json", {"left": []})
        for arch, platform in assemble.IMAGES.items():
            self.write(f"image-{arch}.json", {
                "image": f"ghcr.io/jprodgers/citar:{version}-{arch}", "platform": f"image-{arch}",
                "version": version, "label": label, "build_id": BUILD, "jobs_build": JOBS, "rules": "gnk",
                "smoke": self.smoke(platform, "3.13.9", wheels[platform], label)})

    def write(self, name: str, data: dict, line: bool = False) -> None:
        text = json.dumps(data) + "\n" if line else json.dumps(data, indent=2) + "\n"
        (self.reports / name).write_text(text, encoding="utf-8", newline="\n")

    def read(self, name: str) -> dict:
        return json.loads((self.reports / name).read_text(encoding="utf-8"))

    def smoke(self, platform: str, python: str, wheel: str, label: str) -> dict:
        record = {"smoke": 1, "platform": platform, "python": python, "wheel": wheel, "version": self.version,
                  "pep440": release.Version.parse(self.version).pep440, "label": label, "build_id": BUILD,
                  "jobs_build": JOBS, "rules": "gnk", "spec_hash": SPEC, "result_hash": RESULT, "turns": 30,
                  "verifiable": True, "host_limits": "hard", "seconds": 1.0}
        self.assert_keys(record)
        return record

    @staticmethod
    def assert_keys(record: dict) -> None:
        assert tuple(record) == smoke_wheel.RECORD_KEYS, "the fake smoke line is smoke_wheel.py's"

    def helper_line(self, platform: str, build: str = BUILD, label: str | None = None) -> str:
        return (f"citar-helper {self.version} (build {build}, jobs build {JOBS}, protocol {PROTOCOL}, target "
                f"{helper_release.TARGETS[platform]}, {label or self.label})")

    def helper(self, platform: str) -> dict:
        report = {"platform": platform, "target": helper_release.TARGETS[platform], "version": self.version,
                  "label": self.label, "keys_trusted": 0, "build": BUILD, "jobs_build": JOBS, "protocol": PROTOCOL,
                  "seconds": 2.0}
        for build, name in (("self", helper_release.asset_name(platform)),
                            ("packaged", helper_release.archive_name(self.version, platform))):
            path = self.release / name
            report[build] = {"line": self.helper_line(platform), "build": BUILD, "jobs_build": JOBS,
                             "protocol": PROTOCOL, "file": name, "sha256": sha(path.read_bytes()),
                             "size": path.stat().st_size, "runs": []}
        return report

    def install(self, platform: str) -> dict:
        return {"install_check": "ps1" if platform.startswith("windows") else "sh", "platform": platform,
                "version": self.version, "label": self.label, "wheel": assemble.wheel_names(self.version)[platform],
                "helper": {"line": self.helper_line(platform)},
                "checks": [{"check": "citar --version", "ok": True, "detail": ""}],
                "game": {"doctor": {"build": BUILD, "label": self.label, "version": self.version}}}

    def run(self, label: str | None = None) -> tuple[int, str, str]:
        return quiet(assemble.main, ["assemble", "--version", self.version, "--label", label or self.label,
                                     "--release", str(self.release), "--reports", str(self.reports),
                                     "--preview", str(self.preview)])


class Assemble(unittest.TestCase):
    """Gate 1: the complete set passes and writes SHA256SUMS; each fault fails naming its file."""

    def setUp(self):
        self.dir = scratch("p508-assemble-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fake = FakeRelease(self.dir)

    def fails(self, *names: str) -> str:
        code, out, err = self.fake.run()
        self.assertEqual(code, 1, out + err)
        for name in names:
            self.assertIn(name, err)
        self.assertFalse((self.fake.release / assemble.SUMS).exists(), "nothing is summed when a check fails")
        return err

    def test_the_complete_set_passes_and_its_sums_check(self):
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        files = sorted(p.name for p in self.fake.release.iterdir())
        self.assertEqual(files, sorted(release.asset_files(VERSION)))
        sums = checksums.read_sums((self.fake.release / "SHA256SUMS").read_text(encoding="ascii"), "SHA256SUMS")
        self.assertEqual(list(sums), [a["name"] for a in release.assets(VERSION)])
        for name, digest in sums.items():
            self.assertEqual(digest, sha((self.fake.release / name).read_bytes()), name)
            self.assertEqual((self.fake.release / f"{name}.sha256").read_text(encoding="ascii"), digest + "\n")
        self.assertIn(f"one build: {VERSION}, {LABEL}, build {BUILD}, jobs build {JOBS}", out)
        tool = shutil.which("sha256sum")
        if tool:      # Git Bash's on Windows, coreutils' elsewhere: the format sha256sum -c accepts
            done = subprocess.run([tool, "-c", "SHA256SUMS"], cwd=self.fake.release, capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertEqual(done.stdout.count(": OK"), len(release.assets(VERSION)))

    def test_the_preview(self):
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        preview = self.fake.preview
        notes = (preview / "notes.md").read_text(encoding="utf-8")
        self.assertTrue(notes.startswith(release.notes(ROOT, VERSION).rstrip("\n")), "the CHANGELOG section first")
        self.assertIn((self.fake.release / "SHA256SUMS").read_text(encoding="ascii").strip(), notes)
        # the strong form (docs/INSTALL.md, scripts/deploy/bundle.py): --repo alone accepts any workflow of the repository
        self.assertIn("gh attestation verify <file> --repo jprodgers/CITAR --signer-workflow "
                      f"jprodgers/CITAR/.github/workflows/release.yml --source-ref refs/tags/v{VERSION} "
                      "--deny-self-hosted-runners", notes)
        for route in ("pipx install", "install.sh", "install.ps1", "scoop install citar", "winget install",
                      f"ghcr.io/jprodgers/citar:{VERSION}", "brew install jprodgers/citar/citar-helper"):
            self.assertIn(route, notes)
        self.assertEqual((preview / "subjects.txt").read_text(encoding="utf-8").split(),
                         release.asset_files(VERSION))
        assets = json.loads((preview / "assets.json").read_text(encoding="utf-8"))
        self.assertEqual([a["name"] for a in assets["assets"]], [a["name"] for a in release.assets(VERSION)])
        for a in assets["assets"]:
            data = (self.fake.release / a["name"]).read_bytes()
            self.assertEqual((a["size"], a["sha256"]), (len(data), sha(data)), a["name"])
        self.assertEqual(assets["bytes"], sum((self.fake.release / n).stat().st_size
                                              for n in release.asset_files(VERSION)))
        build = json.loads((preview / "build.json").read_text(encoding="utf-8"))
        self.assertEqual((build["build_id"], build["jobs_build"], build["result_hash"], build["protocol"]),
                         (BUILD, JOBS, RESULT, PROTOCOL))
        self.assertEqual(len(build["sources"]), len(assemble.expected_reports()))
        problems = managers.structure(preview / "managers", VERSION, checksums.base_url(None, VERSION),
                                      self.fake.release)
        self.assertEqual(problems, [])

    def test_a_missing_asset_and_an_extra_file(self):
        (self.fake.release / f"CITAR-{VERSION}-setup.exe").unlink()
        (self.fake.release / "stray.txt").write_text("x", encoding="ascii")
        (self.fake.release / "pkg-sdist").mkdir()
        self.fails(f"CITAR-{VERSION}-setup.exe: missing", "stray.txt: extra", "pkg-sdist: extra")

    def test_a_sha256_beside_its_file_that_is_not_its_hash(self):
        (self.fake.release / "citar-helper-linux-x64.sha256").write_text("0" * 64 + "\n", encoding="ascii")
        self.fails("citar-helper-linux-x64.sha256: says 000")

    def test_a_helper_reporting_another_build(self):
        report = self.fake.read("helper-macos-x64.json")
        report["self"]["line"] = self.fake.helper_line("macos-x64", build="ffffffffffff")
        self.fake.write("helper-macos-x64.json", report)
        self.fails("helper-macos-x64.json (self): build id ffffffffffff")

    def test_a_wheel_reporting_another_jobs_build(self):
        line = self.fake.read("smoke-linux-arm64-3.14.json")
        line["jobs_build"] = "eeeeeeeeeeee"
        self.fake.write("smoke-linux-arm64-3.14.json", line, True)
        self.fails("smoke-linux-arm64-3.14.json: jobs build eeeeeeeeeeee")

    def test_a_wheel_with_another_result_hash(self):
        line = self.fake.read("smoke-windows-x64-3.11.json")
        line["result_hash"] = "d" * 64
        self.fake.write("smoke-windows-x64-3.11.json", line, True)
        self.fails("smoke-windows-x64-3.11.json: the seeded lab job's result hash " + "d" * 64)

    def test_the_sdist_builds_label_is_unknown_and_never_the_releases(self):
        line = self.fake.read("smoke-sdist.json")
        line["label"] = LABEL
        self.fake.write("smoke-sdist.json", line, True)
        self.fails(f"smoke-sdist.json: label {LABEL}, not unknown")

    def test_the_sdist_builds_unknown_is_not_compared_with_the_label(self):
        # Every other report says the release's label and the sdist build's says unknown: that is one label, not two.
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        self.assertNotIn("label", err)

    def test_another_label_version_or_platform(self):
        report = self.fake.read("windows-bundle.json")
        report["label"] = "ci+0000000"
        self.fake.write("windows-bundle.json", report)
        image = self.fake.read("image-arm64.json")
        image["version"] = "9.9.9"
        self.fake.write("image-arm64.json", image)
        line = self.fake.read("smoke-macos-arm64-3.11.json")
        line["platform"] = "macos-x64"
        self.fake.write("smoke-macos-arm64-3.11.json", line, True)
        self.fails("windows-bundle.json: label ci+0000000", "image-arm64.json: version 9.9.9",
                   "smoke-macos-arm64-3.11.json: platform macos-x64")

    def test_a_helper_whose_tested_file_is_not_the_releases(self):
        report = self.fake.read("helper-linux-x64.json")
        report["packaged"]["sha256"] = "c" * 64
        self.fake.write("helper-linux-x64.json", report)
        self.fails(f"helper-linux-x64.json: the packaged build tested ({'c' * 64}) is not the release's "
                   f"citar-helper-{VERSION}-linux-x64.tar.gz")

    def test_an_images_smoke_and_an_install_check(self):
        image = self.fake.read("image-amd64.json")
        image["smoke"]["result_hash"] = "e" * 64
        self.fake.write("image-amd64.json", image)
        install = self.fake.read("install-macos-arm64.json")
        install["checks"].append({"check": "the uninstall leaves nothing", "ok": False, "detail": "x"})
        self.fake.write("install-macos-arm64.json", install)
        self.fails("image-amd64.json (smoke): the seeded lab job's result hash " + "e" * 64,
                   "install-macos-arm64.json: checks that did not pass: the uninstall leaves nothing")

    def test_a_missing_and_an_extra_report(self):
        (self.fake.reports / "helper-linux-arm64.json").unlink()
        (self.fake.reports / "smoke-linux-x64-3.12.json").write_text("{}", encoding="utf-8")
        self.fails("helper-linux-arm64.json: missing", "smoke-linux-x64-3.12.json: extra")

    def test_a_report_that_does_not_read_and_a_smoke_line_short_of_keys(self):
        (self.fake.reports / "image-amd64.json").write_text("not json", encoding="utf-8")
        line = self.fake.read("smoke-macos-x64-3.14.json")
        del line["verifiable"]
        self.fake.write("smoke-macos-x64-3.14.json", line, True)
        self.fails("image-amd64.json: does not read", "smoke-macos-x64-3.14.json: the smoke line lacks verifiable")

    def test_the_command_line_refuses_a_preview_inside_the_release(self):
        code, out, err = quiet(assemble.main, ["assemble", "--version", VERSION, "--label", LABEL, "--release",
                                               str(self.fake.release), "--reports", str(self.fake.reports),
                                               "--preview", str(self.fake.release / "preview")])
        self.assertEqual(code, 2, err)
        code, out, err = quiet(assemble.main, ["assemble", "--version", "v1", "--label", LABEL, "--release", "r",
                                               "--reports", "r", "--preview", "p"])
        self.assertEqual(code, 2, err)

    def test_sums_takes_the_signature_and_writes_its_side(self):
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        (self.fake.release / "citar-helper-manifest.json.sig").write_bytes(b"\x01" * 64)
        code, out, err = quiet(assemble.main, ["sums", "--version", VERSION, "--release", str(self.fake.release)])
        self.assertEqual(code, 1, "unsigned, the .sig is extra")
        self.assertIn("citar-helper-manifest.json.sig: extra", err)
        code, out, err = quiet(assemble.main, ["sums", "--version", VERSION, "--release", str(self.fake.release),
                                               "--signed"])
        self.assertEqual(code, 0, out + err)
        self.assertEqual(sorted(p.name for p in self.fake.release.iterdir()),
                         sorted(release.asset_files(VERSION, signed=True)))
        sums = (self.fake.release / "SHA256SUMS").read_text(encoding="ascii")
        signature = sha(bytes([1]) * 64)
        self.assertIn(f"{signature}  citar-helper-manifest.json.sig\n", sums)


class TheTagOnlyJobsFiles(unittest.TestCase):
    """release.yml's file steps (package 5-09's gate 3): check-signature's ``sums --out`` (rel-release-sums), then
    attest-files' and github-release's ``final`` over a fresh download of pkg-release-assets: what is attested and
    uploaded is release.py assets V's files, and the signature only on a signed tag."""

    def setUp(self):
        self.dir = scratch("p509-final-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fake = FakeRelease(self.dir / "built")
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        self.signature = bytes([7]) * 64

    def download(self, name: str) -> Path:
        """pkg-release-assets, as a later job downloads it: assemble's folder, byte for byte."""
        folder = self.dir / name
        shutil.copytree(self.fake.release, folder)
        return folder

    def sums(self, signed: bool) -> tuple[Path, Path]:
        """check-signature: the download, the signature copied in when signed, then sums --out."""
        release_dir = self.download("check-signature")
        args = ["sums", "--version", VERSION, "--release", str(release_dir), "--out", str(self.dir / "sums")]
        if signed:
            (release_dir / assemble.SIGNATURE).write_bytes(self.signature)
            args.append("--signed")
        code, out, err = quiet(assemble.main, args)
        self.assertEqual(code, 0, out + err)
        return release_dir, self.dir / "sums"

    def final(self, sums: Path, name: str, listing: Path | None = None) -> tuple[int, str, str, Path]:
        folder = self.download(name)
        args = ["final", "--version", VERSION, "--release", str(folder), "--sums", str(sums)]
        if listing is not None:
            args += ["--list", str(listing)]
        return (*quiet(assemble.main, args), folder)

    def test_unsigned_the_sums_and_notes_are_assembles(self):
        _, sums = self.sums(signed=False)
        self.assertEqual(sorted(p.name for p in sums.iterdir()), ["SHA256SUMS", "notes.md"])
        for name in ("SHA256SUMS", "notes.md"):
            self.assertEqual((sums / name).read_bytes(), (self.fake.preview / name).read_bytes(), name)
        for job in ("attest-files", "github-release"):
            listing = self.dir / f"{job}.txt"
            code, out, err, folder = self.final(sums, job, listing)
            self.assertEqual(code, 0, out + err)
            self.assertIn("unsigned", out)
            self.assertEqual(sorted(p.name for p in folder.iterdir()), sorted(release.asset_files(VERSION)))
            self.assertEqual(listing.read_text(encoding="utf-8").split(), release.asset_files(VERSION))

    def test_signed_the_signature_joins_the_sums_the_notes_and_the_list(self):
        _, sums = self.sums(signed=True)
        sig = assemble.SIGNATURE
        self.assertEqual(sorted(p.name for p in sums.iterdir()),
                         sorted(["SHA256SUMS", "notes.md", sig, f"{sig}.sha256"]))
        line = f"{sha(self.signature)}  {sig}"
        self.assertIn(line + "\n", (sums / "SHA256SUMS").read_text(encoding="ascii"))
        self.assertIn(line, (sums / "notes.md").read_text(encoding="utf-8"), "the notes list the signature's hash")
        self.assertNotIn(line, (self.fake.preview / "notes.md").read_text(encoding="utf-8"))
        listing = self.dir / "list.txt"
        code, out, err, folder = self.final(sums, "attest-files", listing)
        self.assertEqual(code, 0, out + err)
        self.assertIn("signed", out)
        self.assertEqual(listing.read_text(encoding="utf-8").split(), release.asset_files(VERSION, signed=True))
        self.assertEqual((folder / sig).read_bytes(), self.signature)
        self.assertEqual((folder / "SHA256SUMS").read_bytes(), (sums / "SHA256SUMS").read_bytes())
        self.assertNotEqual((folder / "SHA256SUMS").read_bytes(),
                            (self.fake.release / "SHA256SUMS").read_bytes(), "the download's unsigned sums replaced")

    def test_final_refuses_what_does_not_hold(self):
        _, sums = self.sums(signed=True)
        cases = {
            "a changed byte": (lambda f: (f / f"citar-{release.Version.parse(VERSION).pep440}.tar.gz").write_bytes(
                b"changed"), "tar.gz.sha256: says"),
            "an extra file": (lambda f: (f / "notes.md").write_text("x", encoding="utf-8"), "notes.md: extra"),
            "an asset gone": (lambda f: (f / "THIRD-PARTY-LICENSES.txt").unlink(), "THIRD-PARTY-LICENSES.txt: missing"),
            "another signature": (lambda f: (f / assemble.SIGNATURE).write_bytes(b"other"), "another one"),
        }
        for what, (plant, named) in cases.items():
            with self.subTest(what):
                folder = self.download(f"plant-{what.replace(' ', '-')}")
                plant(folder)
                code, out, err = quiet(assemble.main, ["final", "--version", VERSION, "--release", str(folder),
                                                       "--sums", str(sums)])
                self.assertEqual(code, 1, out + err)
                self.assertIn(named, err)

    def test_final_refuses_sums_that_are_not_the_files_and_a_signature_unsums_name(self):
        _, sums = self.sums(signed=False)
        text = (sums / "SHA256SUMS").read_text(encoding="ascii")
        first = text.splitlines()[0]
        (sums / "SHA256SUMS").write_text(text.replace(first, "0" * 64 + first[64:]), encoding="ascii")
        code, out, err, _ = self.final(sums, "wrong-sums")
        self.assertEqual(code, 1, out + err)
        self.assertIn("SHA256SUMS: not the sums of these files", err)
        (sums / "SHA256SUMS").write_text(text, encoding="ascii")
        folder = self.download("stray-signature")
        (folder / assemble.SIGNATURE).write_bytes(self.signature)
        code, out, err = quiet(assemble.main, ["final", "--version", VERSION, "--release", str(folder), "--sums",
                                               str(sums)])
        self.assertEqual(code, 1, out + err)
        self.assertIn("an unsigned release", err)
        (sums / f"{assemble.SIGNATURE}").write_bytes(self.signature)
        code, out, err, _ = self.final(sums, "no-side")
        self.assertEqual(code, 1, out + err)
        self.assertIn(f"{assemble.SIGNATURE}.sha256: missing", err)

    def test_sums_out_must_be_empty_and_outside_the_release(self):
        release_dir = self.download("out-checks")
        code, out, err = quiet(assemble.main, ["sums", "--version", VERSION, "--release", str(release_dir), "--out",
                                               str(release_dir / "sums")])
        self.assertEqual(code, 2, err)
        full = self.dir / "full"
        full.mkdir()
        (full / "left.txt").write_text("x", encoding="utf-8")
        code, out, err = quiet(assemble.main, ["sums", "--version", VERSION, "--release", str(release_dir), "--out",
                                               str(full)])
        self.assertEqual(code, 2, err)
        self.assertIn("must be empty", err)

    def images(self) -> Path:
        folder = self.dir / "images"
        folder.mkdir()
        for arch in assemble.IMAGES:
            (folder / f"{arch}.json").write_text(json.dumps({
                "arch": arch, "os": "linux", "id": "ab" * 32, "file": f"citar-image-{VERSION}-{arch}.tar.zst",
                "sha256": "cd" * 32, "image": f"ghcr.io/jprodgers/citar:{VERSION}-{arch}"}), encoding="utf-8")
        return folder

    def summary(self, *extra: str) -> tuple[int, str, str, str]:
        keys = self.dir / "update-keys.pub"
        if not keys.exists():
            keys.write_text("", encoding="utf-8")
        out_file = self.dir / "summary.md"
        out_file.unlink(missing_ok=True)
        code, out, err = quiet(assemble.main, ["summary", "--version", VERSION, "--label", LABEL, "--preview",
                                               str(self.fake.preview), "--keys", str(keys), "--out", str(out_file),
                                               *extra])
        return code, out, err, out_file.read_text(encoding="utf-8") if out_file.exists() else ""

    def test_the_dry_runs_summary(self):
        _, sums = self.sums(signed=False)
        listing = self.dir / "list.txt"
        code, out, err, _ = self.final(sums, "rehearsal", listing)
        self.assertEqual(code, 0, out + err)
        code, out, err, text = self.summary("--images", str(self.images()), "--upload-list", str(listing))
        self.assertEqual(code, 0, out + err)
        self.assertIn(f"# Release dry run: {VERSION}", text)
        self.assertIn(LABEL, text)
        for job, env, _what in assemble.TAG_ONLY_JOBS:
            self.assertIn(f"| `{job}` | {f'`{env}`' if env else 'none'} |", text)
        self.assertIn("gives the notice that nothing is signed", text)
        for asset in release.assets(VERSION):
            self.assertIn(f"`{asset['name']}`", text)
        self.assertIn(f"## The attestation's subjects ({len(release.asset_files(VERSION))})", text)
        self.assertIn("| arm64 | linux |", text)
        self.assertIn(f"release.py assets {VERSION}'s {len(release.asset_files(VERSION))} files", text)
        # With a key committed, the signing job's line says it would sign.
        (self.dir / "update-keys.pub").write_text("# the key\n" + "A" * 43 + "=\n", encoding="utf-8")
        code, out, err, text = self.summary()
        self.assertEqual(code, 0, out + err)
        self.assertIn("verifies it against update-keys.pub's 1 key(s)", text)

    def test_the_summary_refuses_a_wrong_upload_list_or_image_set(self):
        listing = self.dir / "list.txt"
        listing.write_text("\n".join(release.asset_files(VERSION, signed=True)) + "\n", encoding="utf-8")
        code, out, err, _ = self.summary("--upload-list", str(listing))
        self.assertEqual(code, 1, out + err)
        self.assertIn("the rehearsed upload list is not release.py assets", err)
        images = self.images()
        (images / "arm64.json").unlink()
        code, out, err, _ = self.summary("--images", str(images))
        self.assertEqual(code, 1, out + err)
        self.assertIn("the images loaded are ['amd64']", err)

    def test_the_summarys_jobs_are_release_ymls_tag_only_jobs(self):
        import yaml
        data = yaml.safe_load((ROOT / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8"))
        guard = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
        tag_only = [job_id for job_id, job in data["jobs"].items() if guard in str(job.get("if", ""))]
        self.assertEqual([job for job, _env, _what in assemble.TAG_ONLY_JOBS], tag_only)
        for job, env, _what in assemble.TAG_ONLY_JOBS:
            named = str(data["jobs"][job].get("environment", ""))
            self.assertEqual(f"'{env}'" in named or named == env, True, f"{job}: {named!r}")
            if not env:
                self.assertNotIn("environment", data["jobs"][job], job)


class Templates(unittest.TestCase):
    """release_checksums.py's templates and vocabulary."""

    def test_every_template_is_a_manifest_check_managers_knows(self):
        found = sorted(str(p.relative_to(ROOT / "packaging")).replace("\\", "/")[: -len(".in")]
                       for p in checksums.templates(ROOT))
        self.assertEqual(found, sorted(managers.MANIFESTS))
        self.assertFalse((ROOT / "packaging" / "homebrew" / "citar.rb.in").exists())

    def test_no_template_names_pypi_and_each_names_only_assets(self):
        for path in checksums.templates(ROOT):
            text = path.read_text(encoding="utf-8")
            with self.subTest(template=path.name):
                self.assertNotIn("pythonhosted", text)
                self.assertNotIn("pypi.org", text)
                names = checksums.referenced(text, VERSION, path.name)
                installs = "installer" in path.name or path.parent.name in ("scoop", "homebrew")
                self.assertEqual(bool(names), installs, "a manifest that installs names its asset; no other does")

    def test_the_vocabulary(self):
        hashes = {f"CITAR-{VERSION}-setup.exe": "ab" * 32}
        text = ("v={{version}} d={{date}} u={{url:CITAR-{{version}}-setup.exe}} "
                "s={{sha256:CITAR-{{version}}-setup.exe}} S={{SHA256:CITAR-{{version}}-setup.exe}}\n")
        out = checksums.render(text, VERSION, "2026-11-01", hashes, "https://example.test/d/", "t.in")
        self.assertEqual(out, f"v={VERSION} d=2026-11-01 u=https://example.test/d/CITAR-{VERSION}-setup.exe "
                              f"s={'ab' * 32} S={'AB' * 32}\n")
        with self.assertRaisesRegex(checksums.Problem, r"t\.in:2: \{\{url:citar-0\.0\.0\.tar\.gz\}\} names"):
            checksums.render("x\n{{url:citar-0.0.0.tar.gz}}\n", VERSION, "2026-11-01", hashes, "https://e/", "t.in")
        with self.assertRaisesRegex(checksums.Problem, r"t\.in:1: a placeholder this script does not know"):
            checksums.render("{{name}}\n", VERSION, "2026-11-01", hashes, "https://e/", "t.in")

    def test_the_base_url_is_https_or_loopback_http(self):
        self.assertEqual(checksums.base_url(None, "0.1.6"),
                         "https://github.com/jprodgers/CITAR/releases/download/v0.1.6/")
        self.assertEqual(checksums.base_url("http://127.0.0.1:8123", "0.1.6"), "http://127.0.0.1:8123/")
        self.assertEqual(checksums.base_url("http://[::1]:8/x/", "0.1.6"), "http://[::1]:8/x/")
        for bad in ("http://example.com/", "ftp://127.0.0.1/", "https://user@example.com/", "https:///x",
                    "https://example.com/?a=1"):
            with self.subTest(url=bad), self.assertRaises(checksums.Usage):
                checksums.base_url(bad, "0.1.6")

    def test_the_date_is_the_changelogs_unless_given(self):
        found, _ = release.sections(release.Text(ROOT, release.CHANGELOG))
        dated = next(s for s in found if s.name == VERSION)
        self.assertEqual(checksums.release_date(ROOT, VERSION, None), dated.date)
        self.assertEqual(checksums.release_date(ROOT, VERSION, "2026-11-01"), "2026-11-01")
        with self.assertRaises(checksums.Usage):
            checksums.release_date(ROOT, VERSION, "2026-13-01")
        with self.assertRaisesRegex(checksums.Problem, r"no section '## \[9\.9\.9\]'"):
            checksums.release_date(ROOT, "9.9.9", None)


class Manifests(unittest.TestCase):
    """Gate 2's checks, on manifests written from a fake release (``--from-dir``, ``--out``)."""

    def setUp(self):
        self.dir = scratch("p508-manifests-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fake = FakeRelease(self.dir)
        self.out = self.dir / "managers"

    def write(self, *extra: str) -> tuple[int, str, str]:
        return quiet(checksums.main, [VERSION, "--from-dir", str(self.fake.release), "--out", str(self.out),
                                      *extra])

    def test_every_manifest_with_each_url_and_hash(self):
        code, out, err = self.write()
        self.assertEqual(code, 0, out + err)
        base = checksums.base_url(None, VERSION)
        self.assertEqual(managers.structure(self.out, VERSION, base, self.fake.release), [])
        scoop = json.loads((self.out / "scoop" / "citar-helper.json").read_text(encoding="utf-8"))
        name = f"citar-helper-{VERSION}-windows-x64.zip"
        self.assertEqual(scoop["architecture"]["64bit"],
                         {"url": base + name, "hash": sha((self.fake.release / name).read_bytes())})
        installer = managers.yaml_load((self.out / "winget" / "game" / "JimmieRodgers.CITAR.installer.yaml")
                                       .read_text(encoding="utf-8"))
        setup = f"CITAR-{VERSION}-setup.exe"
        self.assertEqual(installer["Installers"][0]["InstallerSha256"],
                         sha((self.fake.release / setup).read_bytes()).upper())
        self.assertEqual(installer["ReleaseDate"], checksums.release_date(ROOT, VERSION, None))
        formula = (self.out / "homebrew" / "citar-helper.rb").read_text(encoding="utf-8")
        for platform in ("macos-arm64", "macos-x64", "linux-x64", "linux-arm64"):
            archive = helper_release.archive_name(VERSION, platform)
            self.assertIn(f'url "{base}{archive}"', formula)
            self.assertIn(f'sha256 "{sha((self.fake.release / archive).read_bytes())}"', formula)
        for path in self.out.rglob("*.yaml"):
            self.assertTrue(path.read_text(encoding="utf-8").startswith("# yaml-language-server: $schema="), path)

    def test_a_loopback_base_url(self):
        code, out, err = self.write("--base-url", "http://127.0.0.1:9/")
        self.assertEqual(code, 0, out + err)
        self.assertEqual(managers.structure(self.out, VERSION, "http://127.0.0.1:9/", self.fake.release), [])
        self.assertIn("http://127.0.0.1:9/citar-helper-", (self.out / "homebrew" / "citar-helper.rb").read_text(
            encoding="utf-8"))
        code, out, err = self.write("--base-url", "http://example.com/")
        self.assertEqual(code, 2, err)

    def test_a_file_missing_from_the_folder(self):
        (self.fake.release / f"citar-helper-{VERSION}-linux-arm64.tar.gz").unlink()
        code, out, err = self.write()
        self.assertEqual(code, 1)
        self.assertIn(f"lacks citar-helper-{VERSION}-linux-arm64.tar.gz", err)

    def test_the_structure_check_catches_each_plant(self):
        code, out, err = self.write()
        self.assertEqual(code, 0, out + err)
        base = checksums.base_url(None, VERSION)
        plants = (
            ("winget/helper/JimmieRodgers.CITAR.Helper.locale.en-US.yaml", "Documentations:", "Documentation:",
             "spelt 'Documentations'"),
            ("winget/game/JimmieRodgers.CITAR.yaml", "# yaml-language-server", "#  yaml-language-server",
             "does not begin with its schema header"),
            ("winget/game/JimmieRodgers.CITAR.installer.yaml", "Scope: user\n", "Scope: [user\n",
             "does not parse as YAML"),
            ("winget/helper/JimmieRodgers.CITAR.Helper.installer.yaml", "ManifestType: installer",
             "ManifestType: version", "ManifestType 'version', not installer"),
            ("winget/game/JimmieRodgers.CITAR.locale.en-US.yaml", "ShortDescription:", "ShortDescriptio:",
             "no ShortDescription"),
            ("homebrew/citar-helper.rb", "    on_intel do\n      url", "    on_arm do\n      url",
             "on_macos has two on_arm blocks"),
            ("scoop/citar.json", '"extract_dir": "CITAR"', '"extract_dir": "citar"', "extract_dir 'citar'"),
            # The merge's CI: Homebrew reads the version from .../releases/download/vV/, and audit --strict refuses a
            # stated one as redundant.
            ("homebrew/citar-helper.rb", '  license "', f'  version "{VERSION}"\n  license "', "states a version"),
            # winget validate warns "Scope is not supported for InstallerType portable".
            ("winget/helper/JimmieRodgers.CITAR.Helper.installer.yaml", "ReleaseDate:", "Scope: user\nReleaseDate:",
             "a Scope, which winget does not support for a portable installer"),
        )
        for rel, old, new, finding in plants:
            with self.subTest(manifest=rel, finding=finding):
                path = self.out / rel
                text = path.read_text(encoding="utf-8")
                self.assertIn(old, text)
                path.write_text(text.replace(old, new, 1), encoding="utf-8", newline="\n")
                try:
                    problems = managers.structure(self.out, VERSION, base, self.fake.release)
                    self.assertTrue([p for p in problems if p.startswith(rel) and finding in p], problems)
                finally:
                    path.write_text(text, encoding="utf-8", newline="\n")
        name = f"citar-helper-{VERSION}-macos-x64.tar.gz"
        (self.fake.release / name).write_bytes(b"another build")
        problems = managers.structure(self.out, VERSION, base, self.fake.release)
        self.assertTrue([p for p in problems if "on_macos / on_intel's sha256" in p], problems)
        (self.out / "scoop" / "extra.json").write_text("{}", encoding="utf-8")
        (self.out / "winget" / "game" / "JimmieRodgers.CITAR.yaml").unlink()
        problems = managers.structure(self.out, VERSION, base, None)
        self.assertIn("scoop/extra.json: no manifest this check knows", problems)
        self.assertIn("winget/game/JimmieRodgers.CITAR.yaml: missing", problems)

    @unittest.skipUnless(importlib.util.find_spec("jsonschema"), "jsonschema is not installed")
    def test_winget_schemas_with_a_schema_of_its_own(self):
        # The real schemas are fetched from each header's URL by gate 2 and the managers job; here a stand-in with
        # the same file names holds the command's own path: a file that breaks its schema is named.
        code, out, err = self.write()
        self.assertEqual(code, 0, out + err)
        schemas = self.dir / "schemas"
        schemas.mkdir()
        for kind in ("installer", "defaultLocale", "version"):
            (schemas / f"winget-manifest.{kind}.1.12.0.schema.json").write_text(json.dumps({
                "$schema": "http://json-schema.org/draft-07/schema#", "type": "object",
                "properties": {"PackageVersion": {"type": "string", "pattern": r"^\d+\.\d+\.\d+"},
                               "ReleaseDate": {"type": "string", "format": "date"}},
                "required": list(managers.WINGET_REQUIRED[kind])}), encoding="utf-8")
        code, out, err = quiet(managers.main, ["winget-schemas", "--dir", str(self.out), "--schemas", str(schemas)])
        self.assertEqual(code, 0, out + err)
        self.assertEqual(out.count("validated: "), 6)
        path = self.out / "winget" / "helper" / "JimmieRodgers.CITAR.Helper.yaml"
        path.write_text(path.read_text(encoding="utf-8").replace("DefaultLocale: en-US\n", ""), encoding="utf-8")
        code, out, err = quiet(managers.main, ["winget-schemas", "--dir", str(self.out), "--schemas", str(schemas)])
        self.assertEqual(code, 1)
        self.assertIn("winget/helper/JimmieRodgers.CITAR.Helper.yaml: (the document): 'DefaultLocale' is a required "
                      "property", err)

    def test_winget_validate_passes_on_an_older_clients_header_warnings_only(self):
        # The merge's CI: windows-latest's winget 1.11.510 holds each header to its own newest schema, so it warned on
        # every 1.12.0 header and exited non-zero. That warning alone passes on an older client, and nothing else does.
        code, out, err = self.write()
        self.assertEqual(code, 0, out + err)
        self.assertEqual(managers.manifest_version(self.out / "winget" / "helper"), (1, 12))
        self.assertEqual(managers.version_tuple("v1.11.510\n"), (1, 11))
        header = ("Manifest Warning: The schema header URL does not match the expected pattern. Value: "
                  "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{}.1.12.0.schema.json Line: 1, "
                  "Column: 25 File: JimmieRodgers.CITAR.{}.yaml")
        headers = "\n".join(header.format(kind, kind) for kind in ("installer", "defaultLocale", "version"))
        warned = "Manifest validation succeeded with warnings.\n" + headers + "\n"
        older, same, newer = (1, 11), (1, 12), (1, 13)
        self.assertEqual(managers.validate_outcome(0, "Manifest validation succeeded.\n", same, same), (True, ""))
        passed, why = managers.validate_outcome(-1978335138, warned, older, same)
        self.assertTrue(passed)
        self.assertIn("only the schema header warning (3 files), from a winget 1.11 older than ManifestVersion 1.12",
                      why)
        for client in (same, newer, None):
            with self.subTest(client=client):
                self.assertEqual(managers.validate_outcome(1, warned, client, same), (False, ""))
        scope = warned + "Manifest Warning: Scope is not supported for InstallerType portable.\n"
        self.assertEqual(managers.validate_outcome(1, scope, older, same), (False, ""))
        error = "Manifest validation failed.\nManifest Error: Field type is not supported. Field: Scope\n" + headers
        self.assertEqual(managers.validate_outcome(1, error, older, same), (False, ""))
        self.assertEqual(managers.validate_outcome(1, warned, older, None), (False, ""))

    def test_the_loopback_formula_states_the_version_its_url_cannot_carry(self):
        # The merge's CI: from http://127.0.0.1:<port>/citar-helper-V-macos-arm64.tar.gz Homebrew read the version
        # "64", so brew test failed; the real URLs' /releases/download/vV/ gives V, and stating it there is refused.
        code, out, err = self.write("--base-url", "http://127.0.0.1:9/")
        self.assertEqual(code, 0, out + err)
        text = (self.out / "homebrew" / "citar-helper.rb").read_text(encoding="utf-8")
        stated = managers.loopback_formula(text, VERSION)
        self.assertEqual(stated.count(f'  version "{VERSION}"\n'), 1)
        self.assertIn(f'  homepage "https://github.com/jprodgers/CITAR"\n  version "{VERSION}"\n  license "', stated)
        self.assertEqual(stated.replace(f'  version "{VERSION}"\n', ""), text)
        with self.assertRaisesRegex(managers.Failed, "states a version already"):
            managers.loopback_formula(stated, VERSION)
        with self.assertRaisesRegex(managers.Failed, "no homepage line"):
            managers.loopback_formula(text.replace("  homepage ", "  # homepage "), VERSION)
        info = json.dumps({"formulae": [{"name": "citar-helper", "versions": {"stable": VERSION, "head": None}}],
                           "casks": []})
        self.assertEqual(managers.formula_version("Fetching nothing\n" + info), VERSION)
        self.assertEqual(managers.formula_version(info + "\nWarning: stderr after the JSON\n"), VERSION)
        self.assertIsNone(managers.formula_version("no JSON here"))
        self.assertIsNone(managers.formula_version('{"formulae": []}'))
        audit = ("Fetching gem metadata\ncitar/dryrun/citar-helper\n  * Stable: `version 0.1.5` is redundant with "
                 "version scanned from URL\nError: 1 problem in 1 formula detected.\n")
        self.assertEqual(managers.brew_problems(audit),
                         "* Stable: `version 0.1.5` is redundant with version scanned from URL")
        self.assertEqual(managers.brew_problems("all fine\n"), "all fine")

    def test_a_release_date_stays_a_string(self):
        self.assertEqual(managers.yaml_load("ReleaseDate: 2026-11-01\n"), {"ReleaseDate": "2026-11-01"})

    def test_the_managers_run_on_a_runner_only(self):
        for manager in ("scoop", "winget", "brew"):
            argv = [manager, "--release", str(self.fake.release), "--version", VERSION, "--work",
                    str(self.dir / "w"), "--under", str(self.dir), "--out", str(self.dir / "r.json")]
            if manager == "winget":
                argv += ["--manifests", str(self.out)]
            with self.subTest(manager=manager), mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}):
                code, out, err = quiet(managers.main, argv)
                self.assertEqual(code, 2, out + err)
                self.assertIn("CI's runners only", err)
                self.assertFalse((self.dir / "r.json").exists())

    def test_the_pypi_json_stand_in(self):
        target = self.dir / "pypi.json"
        code, out, err = quiet(checksums.main, [VERSION, "--from-dir", str(self.fake.release), "--write-pypi-json",
                                                str(target)])
        self.assertEqual(code, 0, out + err)
        data = json.loads(target.read_text(encoding="utf-8"))
        self.assertEqual(data["info"], {"name": "citar", "version": release.Version.parse(VERSION).pep440})
        self.assertEqual(sorted(u["filename"] for u in data["urls"]), sorted(checksums.python_dists(VERSION)))
        for u in data["urls"]:
            self.assertEqual(u["digests"]["sha256"], sha((self.fake.release / u["filename"]).read_bytes()))


class Served:
    """A folder on a loopback server, as a release's download folder."""

    def __init__(self, root: Path):
        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(root), **kwargs)

            def log_message(self, format, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Verify(unittest.TestCase):
    """Gate 3: ``--verify V --base-url http://127.0.0.1:<port> --pypi-json FILE`` over the fake release."""

    def setUp(self):
        self.dir = scratch("p508-verify-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fake = FakeRelease(self.dir)
        code, out, err = self.fake.run()
        self.assertEqual(code, 0, out + err)
        self.pypi = self.dir / "pypi.json"
        code, out, err = quiet(checksums.main, [VERSION, "--from-dir", str(self.fake.release), "--write-pypi-json",
                                                str(self.pypi)])
        self.assertEqual(code, 0, out + err)
        self.served = Served(self.fake.release)
        self.addCleanup(self.served.close)

    def verify(self) -> tuple[int, str, str]:
        return quiet(checksums.main, ["--verify", VERSION, "--base-url", self.served.url, "--pypi-json",
                                      str(self.pypi)])

    def test_the_release_passes(self):
        code, out, err = self.verify()
        self.assertEqual(code, 0, out + err)
        self.assertEqual(out.count("release_checksums: ok: "),
                         len(release.assets(VERSION)) + len(checksums.python_dists(VERSION)))

    def test_one_changed_byte_in_an_asset(self):
        path = self.fake.release / f"CITAR-{VERSION}-windows-x64.zip"
        data = bytearray(path.read_bytes())
        data[0] ^= 1
        path.write_bytes(bytes(data))
        code, out, err = self.verify()
        self.assertEqual(code, 1)
        self.assertIn(f"FAILED: CITAR-{VERSION}-windows-x64.zip: its SHA-256 is", err)
        self.assertIn(f"FAILED: CITAR-{VERSION}-windows-x64.zip.sha256: says", err)
        self.assertEqual(err.count("FAILED: "), 2, err)

    def test_a_pypi_hash_that_differs(self):
        data = json.loads(self.pypi.read_text(encoding="utf-8"))
        data["urls"][0]["digests"]["sha256"] = "f" * 64
        self.pypi.write_text(json.dumps(data), encoding="utf-8")
        code, out, err = self.verify()
        self.assertEqual(code, 1)
        self.assertIn(f"FAILED: {data['urls'][0]['filename']}: PyPI's SHA-256 is {'f' * 64}", err)

    def test_pypi_missing_a_wheel_or_holding_another_file(self):
        data = json.loads(self.pypi.read_text(encoding="utf-8"))
        gone = data["urls"].pop(1)["filename"]
        data["urls"].append({"filename": "citar-9.9.9.tar.gz", "digests": {"sha256": "a" * 64}})
        self.pypi.write_text(json.dumps(data), encoding="utf-8")
        code, out, err = self.verify()
        self.assertEqual(code, 1)
        self.assertIn(f"FAILED: {gone}: PyPI", err)
        self.assertIn("FAILED: citar-9.9.9.tar.gz: PyPI has it", err)

    def test_sums_that_miss_an_asset_or_list_another(self):
        sums = self.fake.release / "SHA256SUMS"
        lines = sums.read_text(encoding="ascii").splitlines()
        dropped = lines.pop(3).split()[1]
        lines.append("b" * 64 + "  citar-extra.bin")
        sums.write_text("\n".join(lines) + "\n", encoding="ascii")
        code, out, err = self.verify()
        self.assertEqual(code, 1)
        self.assertIn(f"FAILED: {dropped}: SHA256SUMS does not list it", err)
        self.assertIn("FAILED: citar-extra.bin: SHA256SUMS lists it", err)

    def test_an_asset_gone_from_the_release(self):
        (self.fake.release / "citar-helper-macos-arm64").unlink()
        code, out, err = self.verify()
        self.assertEqual(code, 1)
        self.assertIn("FAILED: citar-helper-macos-arm64: cannot be downloaded", err)

    def test_a_signed_release_is_verified_with_its_signature(self):
        (self.fake.release / "citar-helper-manifest.json.sig").write_bytes(b"\x02" * 64)
        code, out, err = quiet(assemble.main, ["sums", "--version", VERSION, "--release", str(self.fake.release),
                                               "--signed"])
        self.assertEqual(code, 0, out + err)
        code, out, err = self.verify()
        self.assertEqual(code, 0, out + err)
        self.assertIn("ok: citar-helper-manifest.json.sig", out)

    def test_the_manifests_from_the_published_sums(self):
        # Without --from-dir the hashes come from SHA256SUMS where the manifests point (here the loopback copy)
        out = self.dir / "from-sums"
        code, stdout, err = quiet(checksums.main, [VERSION, "--base-url", self.served.url, "--out", str(out)])
        self.assertEqual(code, 0, stdout + err)
        self.assertEqual(managers.structure(out, VERSION, self.served.url, self.fake.release), [])
        lines = (self.fake.release / "SHA256SUMS").read_text(encoding="ascii").splitlines()
        (self.fake.release / "SHA256SUMS").write_text(
            "\n".join(line for line in lines if "macos-x64.tar.gz" not in line) + "\n", encoding="ascii")
        code, stdout, err = quiet(checksums.main, [VERSION, "--base-url", self.served.url, "--out", str(out)])
        self.assertEqual(code, 1)
        self.assertIn(f"SHA256SUMS lists no citar-helper-{VERSION}-macos-x64.tar.gz", err)

    def test_the_checklists_spelling_with_the_version_first(self):
        # P5.15 step 7 (RELEASING.md) writes `release_checksums.py V --verify`; it must check the same release
        args = ["--base-url", self.served.url, "--pypi-json", str(self.pypi)]
        for argv in ([VERSION, "--verify", *args], ["--verify", *args, VERSION], [VERSION, "--verify", VERSION, *args]):
            with self.subTest(argv=argv[:3]):
                code, out, err = quiet(checksums.main, argv)
                self.assertEqual(code, 0, out + err)
                self.assertIn(f"release_checksums: verifying {VERSION} at {self.served.url}", out)
                self.assertEqual(out.count("release_checksums: ok: "),
                                 len(release.assets(VERSION)) + len(checksums.python_dists(VERSION)))
        path = self.fake.release / f"citar-helper-{VERSION}-linux-x64.tar.gz"
        path.write_bytes(path.read_bytes() + b"\0")
        code, out, err = quiet(checksums.main, [VERSION, "--verify", *args])
        self.assertEqual(code, 1)
        self.assertIn(f"FAILED: citar-helper-{VERSION}-linux-x64.tar.gz: its SHA-256 is", err)

    def test_verify_takes_only_its_own_options(self):
        code, out, err = quiet(checksums.main, ["--verify", VERSION, "--out", str(self.dir / "x")])
        self.assertEqual(code, 2, err)
        code, out, err = quiet(checksums.main, [VERSION, "--verify", "--from-dir", str(self.fake.release)])
        self.assertEqual(code, 2, err)
        code, out, err = quiet(checksums.main, ["--verify", VERSION, "--base-url", "http://example.com/"])
        self.assertEqual(code, 2, err)
        code, out, err = quiet(checksums.main, ["--verify", "--base-url", self.served.url])
        self.assertEqual(code, 2, err)
        self.assertIn("--verify needs the release's version", err)
        code, out, err = quiet(checksums.main, ["9.9.9", "--verify", VERSION, "--base-url", self.served.url])
        self.assertEqual(code, 2, err)
        self.assertIn(f"--verify {VERSION} and the version 9.9.9 differ", err)
        self.assertNotIn("verifying", out)


if __name__ == "__main__":
    unittest.main()
