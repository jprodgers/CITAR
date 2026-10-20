"""The helper's release side in scripts/ci/ (design/phase5-release.md P5.4, package 5-03): verify_manifest.py, the
independent check of the manifest's signature, and helper_release.py, which checks a Linux build's glibc floor, stages
both builds (the self-updating binary under its release name, the packaged one in its archive), checks them on the
runner that built them, and reads the manifest back.

The committed fixture (tests/data/helper_manifest/) was signed by the cryptography package from the seed [9; 32], and
xtask's test holds ed25519-dalek to the same signature byte for byte, so the two verifiers are known to agree on the
domain, the bytes and the encoding. The builds themselves are checked for real by 5-03's gates and package.yml; here a
stand-in helper (a script, on Unix) answers as a release build does.
"""
import base64
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "tests" / "data" / "helper_manifest"


def load(name: str):
    spec = importlib.util.spec_from_file_location(f"citar_ci_{name}", ROOT / "scripts" / "ci" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


verify_manifest = load("verify_manifest")
helper_release = load("helper_release")
gate_run = helper_release.gate_run

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
except ImportError:  # the [keyring] extra brings it to every suite environment
    Ed25519PrivateKey = None

needs_cryptography = unittest.skipIf(Ed25519PrivateKey is None, "the cryptography package is not installed")


def scratch(test) -> Path:
    path = Path(tempfile.mkdtemp(prefix="citar-helper-release-"))
    test.addCleanup(shutil.rmtree, path, ignore_errors=True)
    return path


def run(script: str, *args, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(ROOT / "scripts" / "ci" / script), *map(str, args)],
                          capture_output=True, text=True, encoding="utf-8", **kwargs)


def public_line(key) -> str:
    return base64.b64encode(key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)).decode("ascii")


@needs_cryptography
class VerifyManifest(unittest.TestCase):
    def setUp(self):
        self.dir = scratch(self)
        self.manifest = self.dir / "citar-helper-manifest.json"
        shutil.copyfile(FIXTURE / "citar-helper-manifest.json", self.manifest)
        shutil.copyfile(FIXTURE / "citar-helper-manifest.json.sig", self.dir / "citar-helper-manifest.json.sig")
        self.pub = FIXTURE / "signing.pub"

    def test_the_fixture_xtasks_signer_agrees_with_verifies_by_its_key_line(self):
        out = run("verify_manifest.py", "--pub", self.pub, self.manifest)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("signed by the key on line 2 of", out.stdout)
        # the signature is the seed's, recomputed here: ed25519 is deterministic
        key = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
        data = self.manifest.read_bytes()
        sig = base64.b64encode(key.sign(verify_manifest.MANIFEST_DOMAIN + data)).decode("ascii")
        self.assertEqual(sig, (self.dir / "citar-helper-manifest.json.sig").read_text(encoding="ascii").strip())

    def test_one_byte_changed_another_key_and_no_domain_each_fail(self):
        data = self.manifest.read_bytes()
        keys = verify_manifest.public_keys(self.pub.read_text(encoding="utf-8"))
        sig = (self.dir / "citar-helper-manifest.json.sig").read_text(encoding="ascii")
        self.assertEqual(verify_manifest.verified_by(data, sig, keys), 2)
        for at in (0, len(data) // 2, len(data) - 1):
            changed = bytearray(data)
            changed[at] ^= 0x01
            with self.subTest(byte=at), self.assertRaises(verify_manifest.Failed):
                verify_manifest.verified_by(bytes(changed), sig, keys)
        flipped = self.dir / "changed.json"
        changed = bytearray(data)
        changed[len(data) // 2] ^= 0x01
        flipped.write_bytes(changed)
        out = run("verify_manifest.py", "--pub", self.pub, flipped, self.dir / "citar-helper-manifest.json.sig")
        self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
        self.assertIn("does not verify", out.stderr)
        other = self.dir / "other.pub"
        other.write_text(public_line(Ed25519PrivateKey.generate()) + "\n", encoding="ascii")
        out = run("verify_manifest.py", "--pub", other, self.manifest)
        self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
        # a signature over the bytes alone, without the domain, is not one
        key = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
        bare = base64.b64encode(key.sign(data)).decode("ascii")
        with self.assertRaises(verify_manifest.Failed):
            verify_manifest.verified_by(data, bare, keys)
        for bad, why in (("AAAA", "not 64"), ("not base64!", "base64")):
            with self.subTest(signature=bad), self.assertRaisesRegex(verify_manifest.Failed, why):
                verify_manifest.verified_by(data, bad, keys)

    def test_either_of_two_keys_and_the_line_that_made_it(self):
        both = self.dir / "both.pub"
        both.write_text(f"# the next key\n{public_line(Ed25519PrivateKey.generate())}\n\n"
                        f"{self.pub.read_text(encoding='ascii').splitlines()[-1]}\n", encoding="ascii")
        out = run("verify_manifest.py", "--pub", both, self.manifest)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("on line 4 of", out.stdout)

    def test_a_key_file_that_is_not_one_is_refused_without_echoing_it(self):
        key = Ed25519PrivateKey.generate()
        secret = base64.b64encode(key.private_bytes_raw()).decode("ascii") + "x"
        private = self.dir / "private.key"
        private.write_text(json.dumps({"kind": "citar-helper-update-signing-key", "secret": secret}) + "\n",
                           encoding="ascii")
        out = run("verify_manifest.py", "--pub", private, self.manifest)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("line 1 is not a public key", out.stderr)
        self.assertNotIn(secret, out.stdout + out.stderr)
        empty = self.dir / "empty.pub"
        empty.write_text("# none yet\n", encoding="ascii")
        out = run("verify_manifest.py", "--pub", empty, self.manifest)
        self.assertEqual(out.returncode, 2)
        self.assertIn("holds no public key", out.stderr)
        self.assertEqual(run("verify_manifest.py", self.manifest).returncode, 2, "no --pub")

    def test_a_signed_file_no_helper_reads_fails(self):
        key = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
        for name, data in (("array.json", b"[1, 2]\n"), ("v2.json", b'{"v": 2, "assets": {"a": {}}}\n'),
                           ("vtrue.json", b'{"v": true, "assets": {"a": {}}}\n'),
                           ("bare.json", b'{"v": 1, "assets": {}}\n'),
                           ("large.json", b'{"v": 1, "pad": "' + b"x" * 70_000 + b'", "assets": {"a": {}}}')):
            path = self.dir / name
            path.write_bytes(data)
            sig = base64.b64encode(key.sign(verify_manifest.MANIFEST_DOMAIN + data)).decode("ascii")
            (self.dir / f"{name}.sig").write_text(sig, encoding="ascii")
            with self.subTest(name=name):
                out = run("verify_manifest.py", "--pub", self.pub, path)
                self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
                self.assertIn("no helper reads it", out.stderr)

    def test_check_keys_reads_the_committed_file_and_refuses_three_or_junk(self):
        out = run("verify_manifest.py", "--check-keys", ROOT / "crates" / "citar-jobs" / "update-keys.pub")
        self.assertEqual(out.returncode, 0, out.stderr)
        keys = [line for line in (ROOT / "crates" / "citar-jobs" / "update-keys.pub").read_text().splitlines()
                if line.strip() and not line.strip().startswith("#")]
        self.assertIn("no public key" if not keys else f"{len(keys)} public key(s)", out.stdout)
        three = self.dir / "three.pub"
        three.write_text("".join(public_line(Ed25519PrivateKey.generate()) + "\n" for _ in range(3)),
                         encoding="ascii")
        out = run("verify_manifest.py", "--check-keys", three)
        self.assertEqual(out.returncode, 1)
        self.assertIn("over the 2", out.stderr)
        junk = self.dir / "junk.pub"
        junk.write_text("# fine\nnot a key\n", encoding="ascii")
        out = run("verify_manifest.py", "--check-keys", junk)
        self.assertEqual(out.returncode, 1)
        self.assertIn("line 2 is not a public key", out.stderr)


OBJDUMP = """
citar-helper:     file format elf64-x86-64

DYNAMIC SYMBOL TABLE:
0000000000000000      DF *UND*\t0000000000000000 (GLIBC_2.2.5) free
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.17  clock_gettime
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.28  statx
0000000000000000  w   D  *UND*\t0000000000000000  Base        __gmon_start__
"""


class Glibc(unittest.TestCase):
    def test_the_newest_version_and_its_floor(self):
        needs = helper_release.glibc_needs(OBJDUMP)
        self.assertEqual(sorted(needs), ["2.17", "2.2.5", "2.28"])
        self.assertEqual(helper_release.glibc_problem(needs, "2.28"), ("2.28", ""))
        newest, why = helper_release.glibc_problem(needs, "2.27")
        self.assertEqual(newest, "2.28")
        self.assertIn("above the floor 2.27", why)
        self.assertIn("statx", why)

    def test_a_build_on_ubuntu_24_04_is_refused_naming_its_symbols(self):
        text = OBJDUMP + ("0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.39  pidfd_spawnp\n"
                          "0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.34  __libc_start_main\n")
        newest, why = helper_release.glibc_problem(helper_release.glibc_needs(text), "2.28")
        self.assertEqual(newest, "2.39")
        self.assertIn("GLIBC_2.34, GLIBC_2.39", why)
        self.assertIn("pidfd_spawnp", why)
        self.assertIn("__libc_start_main", why)

    def test_versions_compare_as_numbers_and_private_or_none_is_refused(self):
        self.assertGreater(helper_release.version_key("2.10"), helper_release.version_key("2.9"))
        text = "0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.10  f\n"
        self.assertIn("above the floor 2.9", helper_release.glibc_problem(helper_release.glibc_needs(text), "2.9")[1])
        private = OBJDUMP + "0000000000000000      DF *UND*\t0000000000000000  GLIBC_PRIVATE _dl_x\n"
        self.assertIn("GLIBC_PRIVATE", helper_release.glibc_problem(helper_release.glibc_needs(private), "2.28")[1])
        self.assertIn("names no glibc version", helper_release.glibc_problem({}, "2.28")[1])


class Stage(unittest.TestCase):
    def setUp(self):
        self.dir = scratch(self)
        self.bin = self.dir / "build"
        self.bin.mkdir()
        (self.bin / "self").write_bytes(b"self-updating build")
        (self.bin / "packaged").write_bytes(b"packaged build")

    def stage(self, platform, out):
        return helper_release.stage(self.bin / "self", self.bin / "packaged", platform, "0.1.7-rc.1", out)

    def test_each_platform_stages_its_names_its_sums_and_exactly_its_files(self):
        for platform in helper_release.TARGETS:
            with self.subTest(platform=platform):
                out = self.dir / platform
                self.stage(platform, out)
                asset = out / helper_release.asset_name(platform)
                archive = out / helper_release.archive_name("0.1.7-rc.1", platform)
                self.assertEqual(sorted(p.name for p in out.iterdir()),
                                 sorted([asset.name, asset.name + ".sha256", archive.name, archive.name + ".sha256"]))
                self.assertEqual(asset.read_bytes(), b"self-updating build")
                for path in (asset, archive):
                    side = (out / (path.name + ".sha256")).read_bytes()
                    self.assertEqual(side, hashlib.sha256(path.read_bytes()).hexdigest().encode() + b"\n")
                listing = helper_release.listing(archive, platform, "0.1.7-rc.1")
                binary = "citar-helper.exe" if platform == "windows-x64" else "citar-helper"
                self.assertEqual([line.split()[0] for line in listing], [binary, "LICENSE", "NOTICE.md"])
                self.assertEqual(listing[0], f"{binary} 755")
        self.assertEqual(helper_release.archive_name("0.1.6", "windows-x64"), "citar-helper-0.1.6-windows-x64.zip")
        self.assertEqual(helper_release.asset_name("macos-arm64"), "citar-helper-macos-arm64")

    def test_the_archives_are_reproducible_and_carry_the_checkouts_notices(self):
        for platform in ("linux-x64", "windows-x64"):
            with self.subTest(platform=platform):
                a, b = self.dir / f"{platform}-a", self.dir / f"{platform}-b"
                self.stage(platform, a)
                self.stage(platform, b)
                name = helper_release.archive_name("0.1.7-rc.1", platform)
                self.assertEqual((a / name).read_bytes(), (b / name).read_bytes())
                packaged = helper_release.unpack_binary(a / name, platform, self.dir / f"{platform}-unpacked")
                self.assertEqual(packaged.read_bytes(), b"packaged build")
                if platform == "linux-x64":
                    with tarfile.open(a / name) as t:
                        notice = t.extractfile("NOTICE.md").read()
                        self.assertTrue(all(m.uid == 0 and m.mtime == helper_release.ARCHIVE_TIME for m in t))
                else:
                    with zipfile.ZipFile(a / name) as z:
                        notice = z.read("NOTICE.md")
                self.assertEqual(notice, (ROOT / "NOTICE.md").read_bytes())

    def test_listing_refuses_an_extra_file_a_missing_one_a_folder_and_no_exec_bit(self):
        archive = self.dir / helper_release.archive_name("0.1.7", "linux-x64")

        def tar(members):
            with tarfile.open(archive, "w:gz") as t:
                for name, mode, kind in members:
                    info = tarfile.TarInfo(name)
                    info.mode, info.type = mode, kind
                    data = b"" if kind == tarfile.DIRTYPE else b"x"
                    info.size = len(data)
                    t.addfile(info, io.BytesIO(data))

        good = [("citar-helper", 0o755, tarfile.REGTYPE), ("LICENSE", 0o644, tarfile.REGTYPE),
                ("NOTICE.md", 0o644, tarfile.REGTYPE)]
        tar(good)
        self.assertEqual(len(helper_release.listing(archive, "linux-x64", "0.1.7")), 3)
        for members, why in ((good + [("extra.txt", 0o644, tarfile.REGTYPE)], "not exactly"),
                             (good[:2], "not exactly"),
                             ([("citar-helper", 0o755, tarfile.DIRTYPE)] + good[1:], "not a regular file"),
                             ([("citar-helper", 0o644, tarfile.REGTYPE)] + good[1:], "not executable"),
                             ([("bin/citar-helper", 0o755, tarfile.REGTYPE)] + good[1:], "not exactly")):
            with self.subTest(why=why, members=[m[0] for m in members]):
                tar(members)
                with self.assertRaisesRegex(helper_release.Failed, why):
                    helper_release.listing(archive, "linux-x64", "0.1.7")
        with self.assertRaisesRegex(helper_release.Failed, "is not citar-helper-0.1.8"):
            helper_release.listing(archive, "linux-x64", "0.1.8")


class Manifest(unittest.TestCase):
    def setUp(self):
        self.dir = scratch(self)
        self.assets = self.dir / "assets"
        self.assets.mkdir()
        assets = {}
        for platform, target in helper_release.TARGETS.items():
            name = helper_release.asset_name(platform)
            (self.assets / name).write_bytes(f"binary for {target}".encode())
            helper_release.write_sha256(self.assets / name)
            assets[target] = {"name": name, "sha256": helper_release.sha256(self.assets / name),
                              "size": (self.assets / name).stat().st_size,
                              "url": f"https://github.com/jprodgers/CITAR/releases/download/v0.1.7/{name}"}
        self.doc = {"v": 1, "version": "0.1.7", "protocol": 2, "build": "0b412a18fe95",
                    "released_at": "2026-11-01T00:00:00Z", "assets": assets}
        self.manifest = self.dir / "citar-helper-manifest.json"

    def check(self, doc):
        self.manifest.write_text(json.dumps(doc, indent=2), encoding="utf-8")
        return helper_release.check_manifest(self.manifest, self.assets, "0.1.7")

    def test_it_holds_the_five_files_their_urls_and_sums(self):
        lines = self.check(self.doc)
        self.assertEqual(len(lines), 6)

    def test_a_wrong_hash_size_url_name_target_or_version_fails(self):
        target = "x86_64-unknown-linux-gnu"
        for field, value, why in (("sha256", "0" * 64, "sha256"), ("size", 1, "size"),
                                  ("url", "https://example.com/citar-helper-linux-x64", "url"),
                                  ("name", "citar-helper", "name")):
            with self.subTest(field=field):
                doc = json.loads(json.dumps(self.doc))
                doc["assets"][target][field] = value
                with self.assertRaisesRegex(helper_release.Failed, why):
                    self.check(doc)
        doc = json.loads(json.dumps(self.doc))
        del doc["assets"]["aarch64-apple-darwin"]
        with self.assertRaisesRegex(helper_release.Failed, "not the five targets"):
            self.check(doc)
        with self.assertRaisesRegex(helper_release.Failed, "for 0.1.7"):
            self.check({**self.doc, "version": "0.1.8"})
        # the file a runner hashed is not the one listed
        (self.assets / "citar-helper-linux-x64.sha256").write_text("0" * 64 + "\n", encoding="ascii")
        with self.assertRaisesRegex(helper_release.Failed, "citar-helper-linux-x64.sha256 says"):
            self.check(self.doc)


#: A stand-in release helper (a Python script, so Unix only), as `check` drives one: its build line, a self-test, and
#: update check and apply as a self-updating or a packaged build (the second when it runs from a `packaged` folder).
FAKE = """#!{python} -S
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
home = Path(args[args.index("--home") + 1])
assert os.environ.get("CITAR_DATA_DIR") == str(home), "gate_run gives the home as CITAR_DATA_DIR"
assert not [k for k in os.environ if k.startswith("CITAR_") and k != "CITAR_DATA_DIR"], "every other CITAR_ dropped"
packaged = Path(__file__).parent.name == "packaged"
line = "citar-helper {version} (build 0b412a18fe95, jobs build b2a9ff8212e2, protocol 2, target {target}, {label})"
if "--version" in args:
    print(line)
elif "--self-test" in args:
    print("self-test: the canary and the self-check pass")
elif args[:2] == ["update", "check"]:
    settings = home / "helper.json"
    policy = json.loads(settings.read_text()).get("updates") if settings.exists() else None
    if packaged:
        policy = ("auto, which a packaged build refuses: update it through your package manager" if policy == "auto"
                  else "notify (a packaged build: its package manager updates it)")
    else:
        policy = policy or "auto"
    print(line)
    print("update keys trusted: 0 (this build does not update itself)")
    print("updates: unavailable, this build trusts no update key; policy: " + {policy_hook})
elif args[:2] == ["update", "apply"] and packaged:
    print("citar-helper: this helper was installed by a package manager: update it through your package manager",
          file=sys.stderr)
    sys.exit(2)
else:
    sys.exit(9)
"""


@unittest.skipIf(os.name == "nt", "the stand-in helper is a script: a Unix runner runs it, Windows' gate 1 the real one")
class Check(unittest.TestCase):
    def setUp(self):
        self.dir = scratch(self)
        self.keys = self.dir / "update-keys.pub"
        self.keys.write_text("", encoding="ascii")

    def staged(self, label="ci+abc1234", policy_hook="policy"):
        build = self.dir / "build"
        build.mkdir(exist_ok=True)
        script = FAKE.format(python=sys.executable, version="0.1.7", target="x86_64-unknown-linux-gnu", label=label,
                             policy_hook=policy_hook)
        for name in ("self", "packaged"):
            path = build / name
            path.write_text(script, encoding="utf-8")
            path.chmod(0o755)
        out = self.dir / "stage"
        helper_release.stage(build / "self", build / "packaged", "linux-x64", "0.1.7", out)
        return out

    def check(self, stage, work="work"):
        return helper_release.check(stage, "linux-x64", "x86_64-unknown-linux-gnu", "0.1.7", "ci+abc1234",
                                    self.dir / work, self.dir, self.keys, timeout=60)

    def test_both_builds_hold_and_the_report_says_so(self):
        os.environ["CITAR_STRAY_FOR_THIS_TEST"] = "1"
        self.addCleanup(os.environ.pop, "CITAR_STRAY_FOR_THIS_TEST", None)
        report = self.check(self.staged())
        self.assertEqual((report["build"], report["jobs_build"], report["protocol"]), ("0b412a18fe95", "b2a9ff8212e2", 2))
        self.assertEqual(report["self"]["file"], "citar-helper-linux-x64")
        self.assertEqual(report["packaged"]["file"], "citar-helper-0.1.7-linux-x64.tar.gz")
        self.assertEqual(report["keys_trusted"], 0)
        runs = [r["args"][:2] for r in report["packaged"]["runs"]]
        self.assertIn(["update", "apply"], runs, "the packaged build's update apply is refused")
        self.assertTrue(all("--home" in r["args"] for r in report["self"]["runs"] + report["packaged"]["runs"]))

    def test_another_label_or_a_packaged_build_that_takes_auto_fails(self):
        with self.assertRaisesRegex(helper_release.Failed, "ci\\+other"):
            self.check(self.staged(label="ci+other"))
        stage = self.staged(policy_hook="'auto' if policy.startswith('auto') else policy")
        with self.assertRaisesRegex(helper_release.Failed, "updates auto"):
            self.check(stage, work="work2")

    def test_a_home_outside_the_folder_or_a_used_work_folder_is_refused(self):
        stage = self.staged()
        with self.assertRaises(gate_run.Refused):
            helper_release.check(stage, "linux-x64", "x86_64-unknown-linux-gnu", "0.1.7", "ci+abc1234",
                                 Path(tempfile.gettempdir()) / "elsewhere", self.dir, self.keys, timeout=60)
        (self.dir / "work" / "home-self").mkdir(parents=True)
        (self.dir / "work" / "home-self" / "helper.json").write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(helper_release.Unusable, "not empty"):
            self.check(stage)


class TheCommandLine(unittest.TestCase):
    def test_usage_and_a_platform_that_is_not_one(self):
        out = run("helper_release.py", "listing", "x.tar.gz", "--platform", "solaris-sparc", "--version", "0.1.7")
        self.assertEqual(out.returncode, 2)
        self.assertIn("solaris-sparc", out.stderr)
        out = run("helper_release.py", "glibc", "--objdump", str(ROOT / "no-such-objdump"), __file__)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)

    def test_its_targets_are_the_releases(self):
        spec = importlib.util.spec_from_file_location("citar_release_for_helpers", ROOT / "scripts" / "release.py")
        release = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = release
        spec.loader.exec_module(release)
        self.assertEqual(sorted(helper_release.TARGETS), sorted(release.HELPER_PLATFORMS))
        names = [a["name"] for a in release.assets("0.1.7") if a["group"] in ("helper", "helper-packaged")]
        for platform in helper_release.TARGETS:
            self.assertIn(helper_release.asset_name(platform), names)
            self.assertIn(helper_release.archive_name("0.1.7", platform), names)
        xtask = (ROOT / "xtask" / "src" / "manifest.rs").read_text(encoding="utf-8")
        for platform, target in helper_release.TARGETS.items():
            self.assertIn(f'("{target}", "{helper_release.asset_name(platform)}")', xtask)
        self.assertEqual(helper_release.RELEASE_URL, "https://github.com/jprodgers/CITAR/releases/download/v{version}/")
        self.assertIn('"https://github.com/jprodgers/CITAR/releases/download/v{version}"', xtask)

