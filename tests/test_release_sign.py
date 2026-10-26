"""scripts/ci/release_sign.py: the helper manifest's real signature on a tag (design/phase5-release.md P5.4.2 and
P5.11, package 5-09).

P5.4.2's four states are tried twice. ``States`` runs the script's own logic with xtask stood in by a function, so
every suite environment holds the rules. ``WithTheRealXtask`` runs the command line with the xtask binary a release
build makes (``CITAR_TEST_XTASK`` names it: package 5-09's gate 2 builds it with ``cargo build --release -p xtask``),
with throwaway keys made by its own ``keygen`` in a temporary folder outside any git working tree, and with cargo
trapped (``trapped``): the signing job builds nothing, so nothing here may run cargo. Its signature is then checked
against the committed line by xtask and, independently, by ``verify_manifest.py``.
"""
import ast
import base64
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "ci" / "release_sign.py"
FIXTURE = ROOT / "tests" / "data" / "helper_manifest"
ENV = "CITAR_UPDATE_SIGNING_KEY"


def load(name: str):
    spec = importlib.util.spec_from_file_location(f"citar_ci_{name}", ROOT / "scripts" / "ci" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


rs = load("release_sign")

#: The real xtask binary, for the command-line tests (gate 2): never built here.
XTASK = os.environ.get("CITAR_TEST_XTASK")


def scratch(test) -> Path:
    """A folder outside any git working tree (xtask keygen refuses one inside): the package's target folder when
    CARGO_TARGET_DIR names one, else the system's temporary folder."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    path = Path(tempfile.mkdtemp(prefix="citar-release-sign-", dir=base or None))
    test.addCleanup(shutil.rmtree, path, ignore_errors=True)
    return path


#: The toolchain the trap names: none is installed by that name, so rustup's cargo proxy refuses to run.
TRAP_TOOLCHAIN = "citar-trap-no-such-toolchain"


def trapped(env: dict, trap: Path) -> dict:
    """``env`` with every way to cargo closed, each failing loudly:
    - a ``cargo`` (sh) and a ``cargo.cmd`` first on ``PATH``, which say "cargo was run" and exit 97: what a shell
      finds (on Windows only a shell finds the .cmd);
    - ``CARGO`` naming that stand-in: xtask runs ``$CARGO`` before a bare ``cargo``, as cargo's own children do;
    - ``RUSTUP_TOOLCHAIN`` naming no installed toolchain, with rustup's auto-install off: a program started without a
      shell on Windows looks for ``cargo.exe`` alone (Rust's ``Command::new("cargo")`` as well), and finds rustup's
      proxy, which then refuses to run rather than run cargo.
    """
    out = dict(env)
    out["PATH"] = str(trap) + os.pathsep + env.get("PATH", "")
    out["CARGO"] = str(trap / ("cargo.cmd" if os.name == "nt" else "cargo"))
    out["RUSTUP_TOOLCHAIN"] = TRAP_TOOLCHAIN
    out["RUSTUP_AUTO_INSTALL"] = "0"
    return out


def a_key_line(seed: int) -> str:
    """A public key's form (32 bytes, base64): enough for the script's own reading, which xtask then checks."""
    return base64.b64encode(bytes([seed]) * 32).decode("ascii")


class Fake:
    """xtask stood in: sign-manifest writes a signature naming the secret's key; verify-manifest passes when that key
    is in the key file. Each call is recorded."""

    def __init__(self, test):
        self.calls = []
        self.test = test

    def __call__(self, binary, *args):
        self.calls.append(args)
        if args[0] == "sign-manifest":
            key = os.environ[args[args.index("--key-env") + 1]]
            Path(args[args.index("--out") + 1]).write_text(f"signed-by:{key}\n", encoding="utf-8")
            return subprocess.CompletedProcess(args, 0, "wrote it", "")
        pub, _manifest, sig = Path(args[2]), args[3], Path(args[4])
        signer = sig.read_text(encoding="utf-8").split(":", 1)[1].strip()
        for number, line in enumerate(pub.read_text(encoding="utf-8").splitlines(), 1):
            if line.strip() == signer:
                return subprocess.CompletedProcess(args, 0, f"verified: signed by the key on line {number} of {pub}",
                                                   "")
        return subprocess.CompletedProcess(args, 1, "", "does not verify")


class States(unittest.TestCase):
    """P5.4.2's table, with xtask stood in."""

    def setUp(self):
        self.dir = scratch(self)
        self.manifest = self.dir / "manifest" / rs.MANIFEST
        self.manifest.parent.mkdir()
        shutil.copyfile(FIXTURE / rs.MANIFEST, self.manifest)
        self.binary = self.dir / "xtask-bin" / "xtask"
        self.binary.parent.mkdir()
        self.binary.write_bytes(b"a stand-in binary")
        (self.dir / "xtask-bin" / "xtask.sha256").write_text(
            hashlib.sha256(b"a stand-in binary").hexdigest() + "\n", encoding="ascii")
        self.keys = self.dir / "update-keys.pub"
        self.out = self.dir / "signature"
        self.fake = Fake(self)
        patcher = mock.patch.object(rs, "xtask", self.fake)
        patcher.start()
        self.addCleanup(patcher.stop)

    def sign(self, keys: str, secret: str | None):
        self.keys.write_text(keys, encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if k != ENV}
        if secret is not None:
            env[ENV] = secret
        with mock.patch.dict(os.environ, env, clear=True):
            return rs.sign(self.binary, self.manifest, self.keys, ENV, self.out)

    def report(self) -> dict:
        return json.loads((self.out / rs.REPORT).read_text(encoding="utf-8"))

    def test_no_key_and_no_secret_is_the_notice(self):
        for keys in ("", "\n# the owner commits the public key here\n\n"):
            with self.subTest(keys=keys):
                self.assertFalse(self.sign(keys, None))
                self.assertFalse((self.out / rs.SIGNATURE).exists())
                self.assertEqual(self.report(), {"signed": False, "keys": 0, "secret": False,
                                                 "verified_by_line": None})
                self.assertEqual(self.fake.calls, [], "nothing to sign: xtask is not run")
        # a secret of blanks is no secret
        self.assertFalse(self.sign("", "  \n"))

    def test_a_secret_without_a_committed_key_is_refused(self):
        with self.assertRaisesRegex(rs.Refused, "commit the public key first"):
            self.sign("", "the key file's text")
        self.assertFalse((self.out / rs.SIGNATURE).exists())
        self.assertEqual(self.fake.calls, [])

    def test_a_committed_key_without_the_secret_is_refused(self):
        with self.assertRaisesRegex(rs.Refused, "would trust a key no release signs with"):
            self.sign(a_key_line(1) + "\n", None)
        self.assertEqual(self.fake.calls, [])

    def test_the_committed_keys_secret_signs_and_verifies(self):
        self.assertTrue(self.sign(f"# current\n{a_key_line(1)}\n{a_key_line(2)}\n", a_key_line(2)))
        self.assertEqual((self.out / rs.SIGNATURE).read_text(encoding="utf-8"), f"signed-by:{a_key_line(2)}\n")
        self.assertEqual(self.report(), {"signed": True, "keys": 2, "secret": True, "verified_by_line": 3})
        sign, verify = self.fake.calls
        self.assertEqual(sign[:5], ("sign-manifest", "--key-env", ENV, "--out", str(self.out / rs.SIGNATURE)))
        self.assertNotIn(a_key_line(2), " ".join(sign), "the secret reaches xtask through the environment only")
        self.assertEqual(verify, ("verify-manifest", "--pub", str(self.keys), str(self.manifest),
                                  str(self.out / rs.SIGNATURE)))

    def test_another_secret_is_refused_and_leaves_no_signature(self):
        with self.assertRaisesRegex(rs.Refused, "the secret CITAR_UPDATE_SIGNING_KEY is not the committed key"):
            self.sign(a_key_line(1) + "\n", a_key_line(3))
        self.assertFalse((self.out / rs.SIGNATURE).exists())
        self.assertFalse((self.out / rs.REPORT).exists())

    def test_the_key_file_is_read_as_citar_jobs_reads_it(self):
        for text, why in ((f"{a_key_line(1)}\nnot a key\n", "line 2 is not a public key"),
                          ("c2hvcnQ=\n", "line 1 is not a public key"),
                          ("\n".join(a_key_line(n) for n in (1, 2, 3)) + "\n", "3 keys, over the 2")):
            with self.subTest(text=text):
                with self.assertRaisesRegex(rs.Refused, why) as caught:
                    self.sign(text, a_key_line(1))
                self.assertNotIn("not a key", str(caught.exception), "a key file's line is never echoed")
        # one key given twice is one key
        self.assertTrue(self.sign(f"{a_key_line(1)}\n{a_key_line(1)}\n", a_key_line(1)))
        self.assertEqual(self.report()["keys"], 1)

    def test_the_binary_must_be_the_one_package_yml_built(self):
        (self.dir / "xtask-bin" / "xtask.sha256").write_text("0" * 64 + "\n", encoding="ascii")
        with self.assertRaisesRegex(rs.Refused, "not the binary package.yml built"):
            self.sign(a_key_line(1) + "\n", a_key_line(1))
        (self.dir / "xtask-bin" / "xtask.sha256").unlink()
        with self.assertRaisesRegex(rs.Refused, "no SHA-256 beside the xtask binary"):
            self.sign(a_key_line(1) + "\n", a_key_line(1))
        self.binary.unlink()
        with self.assertRaisesRegex(rs.Usage, "no xtask binary"):
            self.sign(a_key_line(1) + "\n", a_key_line(1))
        self.assertEqual(self.fake.calls, [])


class Expect(unittest.TestCase):
    """check-signature's half: the signature is there exactly when a key is committed."""

    def setUp(self):
        self.dir = scratch(self)
        self.keys = self.dir / "update-keys.pub"
        self.signature = self.dir / "signature"
        self.signature.mkdir()
        self.release = self.dir / "release"
        self.release.mkdir()
        (self.release / rs.MANIFEST).write_bytes(b"{}")

    def test_the_four_combinations(self):
        self.keys.write_text("", encoding="utf-8")
        self.assertFalse(rs.expect(self.keys, self.signature, self.release))
        self.assertFalse(rs.expect(self.keys, self.dir / "nothing-came", self.release))
        (self.signature / rs.SIGNATURE).write_text("sig\n", encoding="utf-8")
        with self.assertRaisesRegex(rs.Refused, "holds no public key: no helper would trust it"):
            rs.expect(self.keys, self.signature, self.release)
        self.keys.write_text(a_key_line(1) + "\n", encoding="utf-8")
        self.assertTrue(rs.expect(self.keys, self.signature, self.release))
        self.assertEqual((self.release / rs.SIGNATURE).read_text(encoding="utf-8"), "sig\n")
        (self.signature / rs.SIGNATURE).unlink()
        with self.assertRaisesRegex(rs.Refused, "left no citar-helper-manifest.json.sig"):
            rs.expect(self.keys, self.signature, self.release)

    def test_a_signature_already_in_the_release_must_be_the_signers(self):
        self.keys.write_text(a_key_line(1) + "\n", encoding="utf-8")
        (self.signature / rs.SIGNATURE).write_text("sig\n", encoding="utf-8")
        (self.release / rs.SIGNATURE).write_text("another\n", encoding="utf-8")
        with self.assertRaisesRegex(rs.Refused, "is not the signature sign-helper-manifest made"):
            rs.expect(self.keys, self.signature, self.release)
        self.keys.write_text("", encoding="utf-8")
        (self.signature / rs.SIGNATURE).unlink()
        with self.assertRaisesRegex(rs.Refused, "holds no public key"):
            rs.expect(self.keys, self.signature, self.release)

    def test_the_command_line_writes_the_output(self):
        self.keys.write_text("", encoding="utf-8")
        output = self.dir / "github-output"
        done = subprocess.run([sys.executable, str(SCRIPT), "expect", "--keys", str(self.keys), "--signature",
                               str(self.signature), "--release", str(self.release), "--github-output", str(output)],
                              capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(output.read_text(encoding="utf-8"), "signed=false\n")


class TheScript(unittest.TestCase):
    def test_it_is_the_standard_library_alone_and_never_names_cargo(self):
        # The signing job runs it on the runner's own Python, with nothing installed, beside the secret.
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"))
        imported = {alias.name.split(".")[0] for node in ast.walk(tree) if isinstance(node, ast.Import)
                    for alias in node.names}
        imported |= {node.module.split(".")[0] for node in ast.walk(tree)
                     if isinstance(node, ast.ImportFrom) and node.module}
        self.assertLessEqual(imported, set(sys.stdlib_module_names) | {"__future__"}, imported)
        literals = [node.value for node in ast.walk(tree) if isinstance(node, ast.Constant)
                    and isinstance(node.value, str)]
        self.assertFalse([s for s in literals if s.strip().split(" ")[0] in ("cargo", "pip", "pip3", "maturin")],
                         "no package tool is named as a command")

    def test_usage_errors_exit_2(self):
        dir = scratch(self)
        keys = dir / "keys.pub"
        keys.write_text("", encoding="utf-8")
        for args in (["sign", "--xtask", str(dir / "x"), "--manifest", str(dir / "none.json"), "--keys", str(keys),
                      "--key-env", ENV, "--out", str(dir / "o")],
                     ["sign", "--xtask", str(dir / "x"), "--manifest", str(FIXTURE / rs.MANIFEST), "--keys",
                      str(keys), "--key-env", "NOT A NAME", "--out", str(dir / "o")],
                     ["sign", "--xtask", str(dir / "x"), "--manifest", str(FIXTURE / rs.MANIFEST), "--keys",
                      str(dir / "missing.pub"), "--key-env", ENV, "--out", str(dir / "o")],
                     ["sign", "--xtask", str(dir / "x")], ["sig"]):
            with self.subTest(args=args):
                done = subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True,
                                      encoding="utf-8")
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)


@unittest.skipUnless(XTASK, "CITAR_TEST_XTASK names no xtask binary (package 5-09's gate 2 builds one)")
class WithTheRealXtask(unittest.TestCase):
    """The command line with the real xtask, throwaway keys and cargo trapped (gate 2)."""

    @classmethod
    def setUpClass(cls):
        cls.xtask = Path(XTASK)

    def setUp(self):
        self.dir = scratch(self)
        # The xtask artifact as package.yml uploads it: the binary and its SHA-256 beside it.
        bin_dir = self.dir / "xtask-bin"
        bin_dir.mkdir()
        self.binary = bin_dir / self.xtask.name
        shutil.copyfile(self.xtask, self.binary)
        (bin_dir / (self.xtask.name + ".sha256")).write_text(
            hashlib.sha256(self.binary.read_bytes()).hexdigest() + "\n", encoding="ascii")
        self.manifest = self.dir / "manifest" / rs.MANIFEST
        self.manifest.parent.mkdir()
        shutil.copyfile(FIXTURE / rs.MANIFEST, self.manifest)
        self.keys = self.dir / "update-keys.pub"
        self.out = self.dir / "signature"
        # A cargo that fails, first on PATH and named by CARGO (trapped): the signing job builds nothing.
        self.trap = self.dir / "trap"
        self.trap.mkdir()
        (self.trap / "cargo").write_text("#!/bin/sh\necho 'cargo was run' >&2\nexit 97\n", encoding="utf-8")
        (self.trap / "cargo").chmod(0o755)
        (self.trap / "cargo.cmd").write_text("@echo cargo was run 1>&2\r\n@exit /b 97\r\n", encoding="utf-8")
        self.a = self.keygen("a")
        self.b = self.keygen("b")

    def keygen(self, name: str) -> dict:
        folder = self.dir / "keys" / name
        folder.parent.mkdir(exist_ok=True)
        done = subprocess.run([str(self.xtask), "keygen", "--out", str(folder)], capture_output=True, text=True,
                              encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        return {"key": (folder / "citar-update-signing.key").read_text(encoding="utf-8"),
                "pub": (folder / "citar-update-signing.pub").read_text(encoding="utf-8").strip(),
                "secret": json.loads((folder / "citar-update-signing.key").read_text(encoding="utf-8"))["secret"]}

    def sign(self, keys: str, secret: str | None, actions: bool = False) -> subprocess.CompletedProcess:
        self.keys.write_text(keys, encoding="utf-8")
        env = trapped({k: v for k, v in os.environ.items() if k not in (ENV, "GITHUB_ACTIONS")}, self.trap)
        if secret is not None:
            env[ENV] = secret
        if actions:
            env["GITHUB_ACTIONS"] = "true"
        done = subprocess.run([sys.executable, str(SCRIPT), "sign", "--xtask", str(self.binary), "--manifest",
                               str(self.manifest), "--keys", str(self.keys), "--key-env", ENV, "--out",
                               str(self.out), "--github-output", str(self.dir / "output")],
                              capture_output=True, text=True, encoding="utf-8", env=env)
        said = done.stdout + done.stderr
        self.assertNotIn("cargo was run", said)
        self.assertNotIn(TRAP_TOOLCHAIN, said, "rustup's cargo proxy was started")
        for key in (self.a, self.b):
            self.assertNotIn(key["secret"], said, "the secret is never printed")
        return done

    def test_the_trap_is_set(self):
        # Each way a program reaches cargo fails, and says so: without a shell (as xtask's Command would start it),
        # through a shell, and by CARGO.
        env = trapped(dict(os.environ), self.trap)
        try:
            bare = subprocess.run(["cargo", "--version"], capture_output=True, text=True, env=env)
        except FileNotFoundError:
            bare = None  # no cargo on this machine at all: nothing to run
        if bare is not None:
            self.assertNotEqual(bare.returncode, 0, bare.stdout + bare.stderr)
            said = bare.stdout + bare.stderr
            self.assertTrue("cargo was run" in said or TRAP_TOOLCHAIN in said, said)
        shell = subprocess.run("cargo --version", capture_output=True, text=True, env=env, shell=True)
        self.assertEqual(shell.returncode, 97, "a shell finds the stand-in first on PATH")
        named = subprocess.run([env["CARGO"], "--version"], capture_output=True, text=True, env=env)
        self.assertEqual(named.returncode, 97, "CARGO names the stand-in")

    def test_no_key_and_no_secret_is_a_notice(self):
        done = self.sign("", None, actions=True)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("::notice title=helper signing::", done.stdout)
        self.assertIn("updates unavailable", done.stdout)
        self.assertFalse((self.out / rs.SIGNATURE).exists())
        self.assertEqual((self.dir / "output").read_text(encoding="utf-8"), "signed=false\n")

    def test_a_secret_and_no_key_is_refused(self):
        done = self.sign("", self.a["key"], actions=True)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("commit the public key first", done.stderr)
        self.assertIn("::error title=helper signing::", done.stdout)
        self.assertFalse((self.out / rs.SIGNATURE).exists())

    def test_a_key_and_no_secret_is_refused(self):
        done = self.sign(self.a["pub"] + "\n", None)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("would trust a key no release signs with", done.stderr)

    def test_the_committed_keys_secret_signs_a_signature_both_verifiers_accept(self):
        done = self.sign(f"# the update key\n{self.a['pub']}\n", self.a["key"])
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        sig = self.out / rs.SIGNATURE
        self.assertTrue(sig.is_file())
        self.assertIn("verifies against line 2 of", done.stdout)
        self.assertEqual(json.loads((self.out / rs.REPORT).read_text(encoding="utf-8"))["verified_by_line"], 2)
        self.assertEqual((self.dir / "output").read_text(encoding="utf-8"), "signed=true\n")
        again = subprocess.run([str(self.xtask), "verify-manifest", "--pub", str(self.keys), str(self.manifest),
                                str(sig)], capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        try:
            import cryptography  # noqa: F401  (the [keyring] extra brings it)
        except ImportError:
            return
        independent = subprocess.run([sys.executable, str(ROOT / "scripts" / "ci" / "verify_manifest.py"), "--pub",
                                      str(self.keys), str(self.manifest), str(sig)], capture_output=True, text=True,
                                     encoding="utf-8")
        self.assertEqual(independent.returncode, 0, independent.stdout + independent.stderr)
        self.assertIn("signed by the key on line 2", independent.stdout)

    def test_another_secret_is_refused_as_not_the_committed_key(self):
        done = self.sign(self.a["pub"] + "\n", self.b["key"])
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("is not the committed key", done.stderr)
        self.assertFalse((self.out / rs.SIGNATURE).exists(), "a signature by another key is not left to upload")

    def test_a_secret_that_is_not_a_key_file_is_refused(self):
        done = self.sign(self.a["pub"] + "\n", self.a["pub"])
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("is not an update-signing key file", done.stderr)

    def test_a_changed_binary_is_refused_before_it_runs(self):
        with self.binary.open("ab") as f:
            f.write(b"\0")
        done = self.sign(self.a["pub"] + "\n", self.a["key"])
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("not the binary package.yml built", done.stderr)
        self.assertFalse((self.out / rs.SIGNATURE).exists())


if __name__ == "__main__":
    unittest.main()
