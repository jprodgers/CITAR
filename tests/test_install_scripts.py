"""The install scripts (design/phase5-release.md P5.8, package 5-07): install.sh and install.ps1 install CITAR from
prebuilt wheels only, after a platform check that says plainly when there is none (musl, an unknown architecture,
an old glibc or macOS, a free-threaded, ARM64, 32-bit or non-CPython Python), tested through their hook
(``CITAR_INSTALL_TEST_FACTS``); ``--worker``/``-Worker`` downloads the self-updating helper with its ``.sha256``,
checked before anything is written, from ``CITAR_RELEASE_URL`` (https, or http to this machine only); the uninstall
removes what is the script's and nothing else; and ``gate_run.py install-ps1`` (the only way install.ps1 runs on the
laptop) refuses a run that could touch a real installation or the user's PATH.

Nothing here installs the game (that needs a release wheel: scripts/ci/install_check.py, run by package.yml's
``install`` matrix and the package's gates): every run is refused before pip, or installs a stand-in helper from a
loopback server into a temporary folder. install.sh runs in bash (on Windows, Git Bash's, never System32's, which is
WSL's), always with a temporary HOME, ``--prefix``, ``--bin-dir`` and ``--no-profile``; install.ps1 only through
gate_run.py, with ``-NoPath``.
"""
from __future__ import annotations

import hashlib
import http.server
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = ROOT / "install.sh"
INSTALL_PS1 = ROOT / "install.ps1"


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


gate_run = sys.modules.get("citar_ci_gate_run") or _load("citar_ci_gate_run", ROOT / "scripts" / "ci" / "gate_run.py")
release = _load("citar_release_for_install_scripts", ROOT / "scripts" / "release.py")
install_check = _load("citar_ci_install_check", ROOT / "scripts" / "ci" / "install_check.py")

LINUX = "system=Linux machine=x86_64 libc=glibc-2.39"
#: The four builds install.sh installs, by the facts that pick each.
POSIX_PLATFORMS = {"linux-x64": "system=Linux machine=x86_64 libc=glibc-2.28",
                   "linux-arm64": "system=Linux machine=aarch64 libc=glibc-2.39",
                   "macos-arm64": "system=Darwin machine=arm64 macos=11.0",
                   "macos-x64": "system=Darwin machine=x86_64 macos=15.1"}


def scratch(prefix: str) -> Path:
    """A temporary folder: under CARGO_TARGET_DIR when it is set (a package's own folder on the laptop), else the
    system's."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base or None))


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


def msys(path: Path | str) -> str:
    """A path as bash reads it: on Windows Git Bash's /c/... form, elsewhere as it is."""
    text = str(path)
    if os.name != "nt":
        return text
    text = text.replace("\\", "/")
    if re.match(r"^[A-Za-z]:/", text):
        text = "/" + text[0].lower() + text[2:]
    return text


class Downloads:
    """A loopback server holding files by path (``/latest/download/<name>`` ...); every path asked for is kept."""

    def __init__(self, files: dict[str, bytes]):
        self.files, self.requests = dict(files), []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                outer.requests.append(self.path)
                body = outer.files.get(self.path)
                self.send_response(200 if body is not None else 404)
                self.send_header("Content-Length", str(len(body or b"")))
                self.end_headers()
                self.wfile.write(body or b"")

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def stand_in(name: str) -> bytes:
    return f"a stand-in for {name}\n".encode()


def release_files(names: list[str], version: str = "0.1.6") -> dict[str, bytes]:
    """Each helper and its .sha256 under latest/download and download/v<version>, and under bad/ with a wrong one."""
    files = {}
    for name in names:
        body = stand_in(name)
        digest = (hashlib.sha256(body).hexdigest() + "\n").encode()
        for folder in ("/latest/download", f"/download/v{version}"):
            files[f"{folder}/{name}"] = body
            files[f"{folder}/{name}.sha256"] = digest
        files[f"/bad/latest/download/{name}"] = body
        files[f"/bad/latest/download/{name}.sha256"] = (hashlib.sha256(b"other").hexdigest() + "\n").encode()
        files[f"/odd/latest/download/{name}"] = body
        files[f"/odd/latest/download/{name}.sha256"] = b"not a digest\n"
    return files


# ------------------------------------------------------------------------------------------------ install.sh


@unittest.skipUnless(BASH, "no bash here (on Windows, Git Bash's)")
class InstallSh(unittest.TestCase):
    """install.sh in bash, with a temporary HOME, --prefix, --bin-dir and --no-profile."""

    @classmethod
    def setUpClass(cls):
        names = [f"citar-helper-{p}" for p in POSIX_PLATFORMS]
        cls.downloads = Downloads(release_files(names))

    @classmethod
    def tearDownClass(cls):
        cls.downloads.close()

    def setUp(self):
        self.tmp = scratch("citar-install-sh-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.home, self.prefix, self.bin = self.tmp / "home", self.tmp / "prefix", self.tmp / "bin"
        self.home.mkdir()
        (self.tmp / "tmp").mkdir()

    def sh(self, *args: str, facts: str = LINUX, url: str | None = None, profile: bool = False,
           prefix: str | None = None, timeout: float = 120, curl_log: Path | None = None) -> tuple[int, str, float]:
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
        env.update(HOME=msys(self.home), TMPDIR=msys(self.tmp / "tmp"), NO_COLOR="1", SHELL="/bin/bash",
                   XDG_CONFIG_HOME=msys(self.home / ".config"), XDG_DATA_HOME=msys(self.home / ".local" / "share"),
                   CITAR_INSTALL_TEST_FACTS=facts)
        if url is not None:
            env["CITAR_RELEASE_URL"] = url
        if curl_log is not None:
            env["PATH"] = str(self.stub_curl()) + os.pathsep + env.get("PATH", "")
            env["STUB_CURL_LOG"] = str(curl_log)
        argv = [BASH, msys(INSTALL_SH), *args, "--prefix", prefix or msys(self.prefix), "--bin-dir", msys(self.bin)]
        if not profile:
            argv.append("--no-profile")
        began = time.perf_counter()
        done = subprocess.run(argv, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace",
                              timeout=timeout)
        return done.returncode, done.stdout + done.stderr, time.perf_counter() - began

    def stub_curl(self) -> Path:
        """A folder holding a curl that reaches nothing: it adds its arguments to $STUB_CURL_LOG and fails as an
        unreachable host does (exit 7). Off Windows only: Git for Windows' bash puts its own folders first on PATH."""
        folder = self.tmp / "stub-bin"
        if not folder.is_dir():
            folder.mkdir()
            (folder / "curl").write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$STUB_CURL_LOG"\nexit 7\n',
                                         encoding="ascii", newline="\n")
            (folder / "curl").chmod(0o755)
        return folder

    def assertNothingMade(self):
        self.assertFalse(self.prefix.exists(), "the prefix was made")
        self.assertFalse(self.bin.exists(), "the bin folder was made")
        self.assertEqual(sorted(p.name for p in self.home.iterdir()), [], "HOME was written")

    # ---- the platform check

    def test_each_platform_without_a_build_is_told_so_before_anything_is_made(self):
        python = msys(sys.executable)
        cases = {
            "musl": ("system=Linux machine=x86_64 libc=musl", "Linux with musl libc"),
            "an unknown architecture": ("system=Linux machine=riscv64 libc=glibc-2.39", "Linux on riscv64"),
            "glibc 2.27": ("system=Linux machine=x86_64 libc=glibc-2.27", "glibc 2.27"),
            "no glibc found": ("system=Linux machine=x86_64 libc=unknown", "without glibc"),
            "macOS 10.15": ("system=Darwin machine=arm64 macos=10.15", "macOS 10.15"),
            "an unknown Mac": ("system=Darwin machine=ppc macos=11.0", "macOS on ppc"),
            "a 32-bit Python": (f"{LINUX} bits=32", "a 32-bit Python"),
            "another Python": (f"{LINUX} implementation=pypy", "pypy Python"),
        }
        for name, (facts, says) in cases.items():
            with self.subTest(name):
                code, out, _ = self.sh("--no-setup", "--python", python, facts=facts)
                self.assertEqual(code, 1, out)
                self.assertIn("There is no prebuilt CITAR for this platform", out)
                self.assertIn(says, out)
                self.assertIn("'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs)",
                              out)
                self.assertNothingMade()

    def test_a_free_threaded_python_is_told_to_use_the_regular_build(self):
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), facts=f"{LINUX} python=3.14 "
                               "free_threaded=1")
        self.assertEqual(code, 1, out)
        self.assertIn("built free-threaded (3.14t)", out)
        self.assertIn("Use the regular build of Python 3.14", out)
        self.assertIn("python3.14, not python3.14t", out)
        self.assertNothingMade()

    def test_a_python_too_old_is_named(self):
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), facts=f"{LINUX} python=3.10")
        self.assertEqual(code, 1, out)
        self.assertIn("it is Python 3.10, and CITAR needs 3.11 or newer", out)
        self.assertNothingMade()

    def test_the_glibc_floor_is_2_28_and_macos_11(self):
        for facts in ("system=Linux machine=x86_64 libc=glibc-2.28", "system=Linux machine=aarch64 libc=glibc-2.40",
                      "system=Darwin machine=arm64 macos=11", "system=Darwin machine=x86_64 macos=26.0.1"):
            with self.subTest(facts):
                # past the platform check: the download is tried, and the release has no such folder
                code, out, _ = self.sh("--worker", facts=facts, url=self.downloads.url + "/missing")
                self.assertEqual(code, 1, out)
                self.assertIn("Could not download", out)
                self.assertNothingMade()
        for facts in ("system=Linux machine=x86_64 libc=glibc-2.9", "system=Darwin machine=arm64 macos=10.16"):
            with self.subTest(facts):
                code, out, _ = self.sh("--worker", facts=facts, url=self.downloads.url)
                self.assertEqual(code, 1, out)
                self.assertIn("no prebuilt CITAR helper", out)

    def test_the_helper_on_a_platform_without_a_build_asks_for_nothing(self):
        asked = len(self.downloads.requests)
        code, out, _ = self.sh("--worker", facts="system=Linux machine=aarch64 libc=musl", url=self.downloads.url)
        self.assertEqual(code, 1, out)
        self.assertIn("There is no prebuilt CITAR helper for this platform: Linux with musl libc", out)
        self.assertIn("cargo build --release -p citar-helper", out)
        self.assertEqual(len(self.downloads.requests), asked)
        self.assertNothingMade()

    # ---- wheels only

    def test_a_folder_holding_only_a_source_distribution_is_refused(self):
        folder = self.tmp / "dists"
        folder.mkdir()
        (folder / "citar-0.1.6.tar.gz").write_bytes(b"not read")
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), "--from", msys(folder))
        self.assertEqual(code, 1, out)
        self.assertIn("holds no CITAR wheel (only citar-0.1.6.tar.gz, a source distribution)", out)
        self.assertIn("this installer installs CITAR's prebuilt wheels only", out)
        self.assertNothingMade()

    def test_a_source_distribution_or_a_checkout_is_refused(self):
        sdist = self.tmp / "citar-0.1.6.tar.gz"
        sdist.write_bytes(b"not read")
        for source in (sdist, ROOT):
            with self.subTest(source.name):
                code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), "--from", msys(source))
                self.assertEqual(code, 1, out)
                self.assertIn("wheels only", out)
                self.assertNothingMade()

    def test_a_folder_of_two_releases_needs_one_named(self):
        folder = self.tmp / "dists"
        folder.mkdir()
        for v in ("0.1.5", "0.1.6"):
            (folder / f"citar-{v}-cp311-abi3-manylinux_2_28_x86_64.whl").write_bytes(b"not read")
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), "--from", msys(folder))
        self.assertEqual(code, 1, out)
        self.assertIn("holds CITAR 0.1.5 0.1.6: name the one to install with --version", out)
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), "--from", msys(folder),
                               "--version", "0.1.7")
        self.assertEqual(code, 1, out)
        self.assertIn("holds CITAR 0.1.5 0.1.6, not 0.1.7", out)
        self.assertNothingMade()

    def test_a_prefix_holding_something_else_is_refused_untouched(self):
        self.prefix.mkdir()
        (self.prefix / "mine.txt").write_text("not CITAR's")
        wheel = self.tmp / "citar-0.1.6-cp311-abi3-manylinux_2_28_x86_64.whl"
        wheel.write_bytes(b"not read")
        code, out, _ = self.sh("--no-setup", "--python", msys(sys.executable), "--from", msys(wheel))
        self.assertEqual(code, 1, out)
        self.assertIn("is not empty and holds no CITAR install", out)
        self.assertEqual(sorted(p.name for p in self.prefix.iterdir()), ["mine.txt"])
        self.assertFalse(self.bin.exists())
        for prefix in ("/", msys(self.home)):
            code, out, _ = self.sh("--uninstall", prefix=prefix)
            self.assertEqual(code, 1, out)
            self.assertIn("would hold more than CITAR", out)

    # ---- the helper

    def test_the_helper_for_each_platform_comes_with_its_checksum(self):
        for platform, facts in POSIX_PLATFORMS.items():
            with self.subTest(platform):
                name = f"citar-helper-{platform}"
                before = len(self.downloads.requests)
                code, out, _ = self.sh("--worker", facts=facts, url=self.downloads.url + "/")
                self.assertEqual(code, 0, out)
                self.assertEqual(self.downloads.requests[before:],
                                 [f"/latest/download/{name}.sha256", f"/latest/download/{name}"])
                installed = self.bin / "citar-helper"
                self.assertEqual(installed.read_bytes(), stand_in(name))
                if os.name != "nt":
                    self.assertTrue(os.access(installed, os.X_OK))
                self.assertEqual(sorted(p.name for p in self.bin.iterdir()), ["citar-helper"])
                self.assertIn("SHA-256 " + hashlib.sha256(stand_in(name)).hexdigest(), out)
                self.assertIn("citar-helper setup", out)
                self.assertIn("citar-helper service install", out)
        # --no-profile: no shell profile was written
        self.assertFalse([p for p in self.home.rglob("*") if p.is_file()])

    def test_a_version_takes_the_helper_from_that_release(self):
        for given in ("0.1.6", "v0.1.6"):
            with self.subTest(given):
                before = len(self.downloads.requests)
                code, out, _ = self.sh("--worker", "--version", given, facts=POSIX_PLATFORMS["linux-x64"],
                                       url=self.downloads.url)
                self.assertEqual(code, 0, out)
                self.assertEqual(self.downloads.requests[before:], [
                    "/download/v0.1.6/citar-helper-linux-x64.sha256", "/download/v0.1.6/citar-helper-linux-x64"])

    def test_a_wrong_or_unreadable_checksum_is_refused_before_anything_is_written(self):
        for folder, says in (("/bad", "and " + self.downloads.url + "/bad"), ("/odd", "does not hold a SHA-256")):
            with self.subTest(folder):
                code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url + folder)
                self.assertEqual(code, 1, out)
                self.assertIn("Refused:", out)
                self.assertIn(says, out)
                self.assertIn("Nothing was installed", out)
                self.assertNothingMade()
                self.assertEqual(sorted(p.name for p in (self.tmp / "tmp").iterdir()), [], "a download was left")

    def test_a_release_address_that_is_not_https_or_this_machine_is_refused_before_any_request(self):
        cases = {
            "http://192.0.2.1/citar": "plain http to 192.0.2.1",
            "http://127.0.0.1.example.com/x": "plain http to 127.0.0.1.example.com",
            "http://127.0.0.1@192.0.2.1/x": "names a user",
            "http://127.0.0.256/x": "plain http to 127.0.0.256",
            "ftp://127.0.0.1/x": "is not an https:// address",
            "file:///etc": "is not an https:// address",
            "https:///x": "names no host",
        }
        for url, says in cases.items():
            with self.subTest(url):
                asked = len(self.downloads.requests)
                code, out, seconds = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=url)
                self.assertEqual(code, 1, out)
                self.assertIn(says, out)
                self.assertLess(seconds, 30)
                self.assertEqual(len(self.downloads.requests), asked)
                self.assertNothingMade()
        for url in ("http://localhost:9/x", "http://[::1]:9/x"):
            with self.subTest(url):
                code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=url)
                self.assertEqual(code, 1, out)
                self.assertIn("Could not download", out, "loopback http is allowed: the download itself failed")
        # 127.1.2.3 is this machine too; but macOS answers only 127.0.0.1 on lo0 (unless an alias is added), where a
        # connection would wait out curl's timeout and retries. So off Windows a stand-in curl, which reaches
        # nothing, shows the address let through; Windows (and Linux) refuse a connection to all of 127/8 at once.
        url = "http://127.1.2.3:9/x"
        log = None if os.name == "nt" else self.tmp / "curl.log"
        code, out, seconds = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=url, curl_log=log)
        self.assertEqual(code, 1, out)
        self.assertIn(f"Could not download {url}/latest/download/citar-helper-linux-x64.sha256", out)
        self.assertLess(seconds, 30)
        if log is not None:
            self.assertIn(f"{url}/latest/download/citar-helper-linux-x64.sha256", log.read_text().splitlines())

    def test_worker_takes_neither_server_nor_from(self):
        for extra, says in ((("--server",), "choose one"), (("--from", "x.whl"), "choose one")):
            with self.subTest(extra[0]):
                code, out, _ = self.sh("--worker", *extra, url=self.downloads.url)
                self.assertEqual(code, 1, out)
                self.assertIn(says, out)

    # ---- the uninstall

    def fake_game(self) -> None:
        """A prefix as the script leaves one, and its links (no real venv: the uninstall reads only the files)."""
        venv_bin = self.prefix / "venv" / "bin"
        venv_bin.mkdir(parents=True)
        (self.prefix / ".citar-installer").write_text("installed by install.sh\n")
        self.bin.mkdir(exist_ok=True)
        for name in ("citar", "citar-admin", "citar-mcp"):
            (venv_bin / name).write_text("#!/bin/sh\n")
            os.symlink(venv_bin / name, self.bin / name)

    #: What the helper's updates leave beside it (crates/citar-helper/src/update/files.rs), and this script's own
    #: half-copied file: the uninstall removes each.
    LEFTOVERS = ("citar-helper.old", "citar-helper.failed", "citar-helper.old.4242", "citar-helper.failed.17",
                 ".citar-helper.new", ".citar-helper.install")
    #: Names like those that are not theirs: they stay.
    LOOKALIKES = ("citar-helper.old.bak", "citar-helper.failed.x9", "citar-helper.older")

    def leave_leftovers(self) -> None:
        for name in (*self.LEFTOVERS, *self.LOOKALIKES):
            (self.bin / name).write_bytes(b"left by an update")

    @unittest.skipIf(os.name == "nt", "the game's links are symbolic links (Linux and macOS)")
    def test_the_uninstall_removes_the_prefix_its_links_and_the_helper_and_nothing_else(self):
        self.fake_game()
        code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url)
        self.assertEqual(code, 0, out)
        self.leave_leftovers()
        (self.bin / "other-tool").write_text("not CITAR's")
        elsewhere = self.tmp / "elsewhere"
        elsewhere.mkdir()
        os.symlink(elsewhere / "citar", self.bin / "citar-mcp-elsewhere")
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 0, out)
        self.assertFalse(self.prefix.exists())
        self.assertEqual(sorted(p.name for p in self.bin.iterdir()),
                         sorted(["citar-mcp-elsewhere", "other-tool", *self.LOOKALIKES]))
        self.assertIn("Your games, settings and results were NOT deleted", out)
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 0, out)

    def test_the_helpers_leftovers_go_without_the_helper_too(self):
        self.bin.mkdir()
        self.leave_leftovers()
        code, out, _ = self.sh("--uninstall", "--worker")
        self.assertEqual(code, 0, out)
        self.assertEqual(sorted(p.name for p in self.bin.iterdir()), sorted(self.LOOKALIKES))

    @unittest.skipIf(os.name == "nt", "the game's links are symbolic links (Linux and macOS)")
    def test_a_link_of_the_same_name_from_elsewhere_stays(self):
        self.bin.mkdir()
        other = self.tmp / "other-citar"
        other.write_text("#!/bin/sh\n")
        os.symlink(other, self.bin / "citar")
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 0, out)
        self.assertTrue((self.bin / "citar").is_symlink())

    @unittest.skipIf(os.name == "nt", "the game's links are symbolic links (Linux and macOS)")
    def test_uninstall_worker_removes_the_helper_alone(self):
        self.fake_game()
        code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url)
        self.assertEqual(code, 0, out)
        for name in self.LEFTOVERS:
            (self.bin / name).write_bytes(b"left by an update")
        code, out, _ = self.sh("--uninstall", "--worker")
        self.assertEqual(code, 0, out)
        self.assertTrue(self.prefix.is_dir())
        self.assertEqual(sorted(p.name for p in self.bin.iterdir()), ["citar", "citar-admin", "citar-mcp"])

    def test_the_uninstall_waits_for_the_helpers_service_to_go_first(self):
        code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url)
        self.assertEqual(code, 0, out)
        unit = self.home / ".config" / "systemd" / "user" / "citar-helper.service"
        unit.parent.mkdir(parents=True)
        unit.write_text(f"[Service]\nExecStart={msys(self.bin / 'citar-helper')} run --service\n")
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 1, out)
        self.assertIn("runs as a service", out)
        self.assertIn("service uninstall", out)
        self.assertIn("Nothing was removed", out)
        self.assertTrue((self.bin / "citar-helper").is_file())
        unit.write_text("[Service]\nExecStart=/somewhere/else/citar-helper run --service\n")
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 0, out)
        self.assertFalse((self.bin / "citar-helper").exists())

    @unittest.skipIf(os.name == "nt", "a bin folder reached through a symbolic link (Linux and macOS)")
    def test_the_service_guard_sees_a_bin_folder_through_a_link(self):
        # `service install` names the path the helper found for itself, every link resolved on Linux
        # (/proc/self/exe): a dotfile manager's ~/.local/bin, a link to its own folder, is named by the folder's path
        real_bin = self.tmp / "dotfiles-bin"
        real_bin.mkdir()
        os.symlink(real_bin, self.bin)
        code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url)
        self.assertEqual(code, 0, out)
        unit = self.home / ".config" / "systemd" / "user" / "citar-helper.service"
        unit.parent.mkdir(parents=True)
        named = os.path.realpath(real_bin / "citar-helper")
        unit.write_text(f'[Service]\nExecStart="{named}" run --service --home "/x"\n')
        code, out, _ = self.sh("--uninstall", "--worker")
        self.assertEqual(code, 1, out)
        self.assertIn("runs as a service", out)
        self.assertTrue((real_bin / "citar-helper").is_file(), "the helper under an enabled unit was removed")
        unit.unlink()
        code, out, _ = self.sh("--uninstall", "--worker")
        self.assertEqual(code, 0, out)
        self.assertEqual(list(real_bin.iterdir()), [])
        self.assertTrue(self.bin.is_symlink(), "the link to the bin folder stays")

    def test_a_prefix_that_is_not_citars_is_never_removed(self):
        self.prefix.mkdir()
        (self.prefix / "mine.txt").write_text("not CITAR's")
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 1, out)
        self.assertIn("does not hold a CITAR install", out)
        self.assertTrue((self.prefix / "mine.txt").is_file())
        (self.prefix / "mine.txt").unlink()
        code, out, _ = self.sh("--uninstall")
        self.assertEqual(code, 0, out)
        self.assertFalse(self.prefix.exists(), "an empty prefix is removed")

    @unittest.skipUnless(os.environ.get("GITHUB_ACTIONS") == "true",
                         "writes a shell profile (in a temporary HOME): on a CI runner only (P5.18.1 rule 3)")
    def test_without_no_profile_the_bin_folder_goes_into_the_profile_once(self):
        for _ in range(2):
            code, out, _ = self.sh("--worker", facts=POSIX_PLATFORMS["linux-x64"], url=self.downloads.url,
                                   profile=True)
            self.assertEqual(code, 0, out)
        text = (self.home / ".bashrc").read_text()
        self.assertEqual(text.count(f'export PATH="{msys(self.bin)}:$PATH"'), 1, text)


# ------------------------------------------------------------------------------------------------ install.ps1


@unittest.skipUnless(os.name == "nt" and shutil.which("powershell"), "install.ps1 runs on Windows")
class InstallPs1(unittest.TestCase):
    """install.ps1, only ever through gate_run.py install-ps1 (-Prefix and -BinDir in a temporary folder, -NoPath)."""

    @classmethod
    def setUpClass(cls):
        cls.downloads = Downloads(release_files(["citar-helper-windows-x64.exe"]))

    @classmethod
    def tearDownClass(cls):
        cls.downloads.close()

    def setUp(self):
        # resolved, as gate_run passes the folders (a runner's TEMP is written short, C:\Users\RUNNER~1\...)
        self.tmp = Path(os.path.realpath(scratch("citar-install-ps1-")))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.prefix, self.bin = self.tmp / "prefix", self.tmp / "bin"
        self.path_before = gate_run.user_path()

    def tearDown(self):
        self.assertEqual(gate_run.user_path(), self.path_before, "the user's PATH changed")

    def ps1(self, *args: str, facts: str = "", url: str | None = None, scriptblock: bool = False,
            shell: str = "powershell") -> tuple[int, str, float]:
        extra = []
        if facts:
            extra.append(f"CITAR_INSTALL_TEST_FACTS={facts}")
        if url is not None:
            extra.append(f"CITAR_RELEASE_URL={url}")
        began = time.perf_counter()
        done = gate_run.run_install_ps1([*args, "-Prefix", str(self.prefix), "-BinDir", str(self.bin), "-NoPath"],
                                        self.tmp, extra, timeout=300, capture=True, shell=shell,
                                        as_scriptblock=scriptblock)
        return done.returncode, (done.stdout or "") + (done.stderr or ""), time.perf_counter() - began

    def assertNothingMade(self):
        self.assertFalse(self.prefix.exists(), "the prefix was made")
        self.assertFalse(self.bin.exists(), "the bin folder was made")

    def test_each_python_without_a_build_is_told_so_before_anything_is_made(self):
        cases = {
            "an ARM64 Python": ("python_platform=win-arm64", "for ARM64, and CITAR's Windows wheel is built for x64"),
            "a 32-bit Python": ("python_platform=win32", "There is no prebuilt CITAR for this platform: a Python for "
                                "win32"),
            "another Python": ("implementation=pypy", "There is no prebuilt CITAR for this platform: pypy Python"),
            "a free-threaded Python": ("python=3.14 free_threaded=1", "Use the regular build of Python 3.14"),
            "a Python too old": ("python=3.10", "it is Python 3.10, and CITAR needs 3.11 or newer"),
            "another system": ("system=Linux", "This installer is for Windows"),
        }
        for name, (facts, says) in cases.items():
            with self.subTest(name):
                code, out, _ = self.ps1("-NoSetup", "-Python", sys.executable, facts=facts)
                self.assertEqual(code, 1, out)
                self.assertIn(says, " ".join(out.split()))
                self.assertNothingMade()

    def test_the_arm64_message_names_the_way_out(self):
        code, out, _ = self.ps1("-NoSetup", "-Python", sys.executable, facts="python_platform=win-arm64")
        text = " ".join(out.split())
        self.assertIn("Install the x64 build of Python", text)
        self.assertIn("or use the CITAR installer", text)

    def test_wheels_only(self):
        folder = self.tmp / "dists"
        folder.mkdir()
        (folder / "citar-0.1.6.tar.gz").write_bytes(b"not read")
        for source, says in ((folder, "holds no CITAR wheel (only citar-0.1.6.tar.gz, a source distribution)"),
                             (folder / "citar-0.1.6.tar.gz", "is not a wheel")):
            with self.subTest(source.name):
                code, out, _ = self.ps1("-NoSetup", "-Python", sys.executable, "-From", str(source))
                self.assertEqual(code, 1, out)
                self.assertIn(says, " ".join(out.split()))
                self.assertIn("wheels only", out)
                self.assertNothingMade()

    def test_the_helper_comes_with_its_checksum(self):
        name = "citar-helper-windows-x64.exe"
        for version, folder in (("", "/latest/download"), ("0.1.6", "/download/v0.1.6")):
            with self.subTest(version or "latest"):
                before = len(self.downloads.requests)
                args = ("-Worker", "-Version", version) if version else ("-Worker",)
                code, out, _ = self.ps1(*args, url=self.downloads.url)
                self.assertEqual(code, 0, out)
                self.assertEqual(self.downloads.requests[before:], [f"{folder}/{name}.sha256", f"{folder}/{name}"])
                self.assertEqual((self.bin / "citar-helper.exe").read_bytes(), stand_in(name))
                self.assertEqual(sorted(p.name for p in self.bin.iterdir()), ["citar-helper.exe"])
                self.assertIn("citar-helper.exe", out)
                self.assertIn("service install", out)

    def test_a_wrong_checksum_or_address_is_refused_before_anything_is_written(self):
        cases = ((self.downloads.url + "/bad", "and " + self.downloads.url + "/bad"),
                 (self.downloads.url + "/odd", "does not hold a SHA-256"),
                 ("http://192.0.2.1/citar", "plain http to 192.0.2.1"),
                 ("http://127.0.0.1@192.0.2.1/citar", "names a user"))
        for url, says in cases:
            with self.subTest(url):
                code, out, seconds = self.ps1("-Worker", url=url)
                self.assertEqual(code, 1, out)
                self.assertIn(says, " ".join(out.split()))
                self.assertLess(seconds, 60)
                self.assertNothingMade()

    def test_the_helper_on_a_machine_without_a_build(self):
        code, out, _ = self.ps1("-Worker", facts="machine=x86", url=self.downloads.url)
        self.assertEqual(code, 1, out)
        self.assertIn("There is no prebuilt CITAR helper for this platform: Windows on x86", " ".join(out.split()))
        self.assertNothingMade()

    def test_worker_takes_no_wheel(self):
        code, out, _ = self.ps1("-Worker", "-From", "x.whl", url=self.downloads.url)
        self.assertEqual(code, 1, out)
        self.assertIn("choose one", out)

    def test_the_uninstall_removes_what_is_the_scripts_and_nothing_else(self):
        scripts = self.prefix / "Scripts"
        scripts.mkdir(parents=True)
        (scripts / "citar.exe").write_bytes(b"MZ")
        (self.prefix / ".citar-installer").write_text("installed by install.ps1\n")
        code, out, _ = self.ps1("-Worker", url=self.downloads.url)
        self.assertEqual(code, 0, out)
        for name in ("citar", "citar-admin"):
            (self.bin / f"{name}.cmd").write_text(f'@echo off\r\n"{scripts / name}.exe" %*', encoding="ascii")
        (self.bin / "citar-mcp.cmd").write_text('@echo off\r\n"C:\\elsewhere\\citar-mcp.exe" %*', encoding="ascii")
        # what the helper's updates leave beside it (crates/citar-helper/src/update/files.rs), and the script's own
        # half-copied file; then names like those that are not theirs
        leftovers = ("citar-helper.exe.old", "citar-helper.exe.failed", "citar-helper.exe.old.4242",
                     "citar-helper.exe.failed.17", ".citar-helper.exe.new", ".citar-helper.install")
        lookalikes = ("citar-helper.exe.old.bak", "citar-helper.exe.failed.x9", "citar-helper.exe.older")
        for name in (*leftovers, *lookalikes):
            (self.bin / name).write_bytes(b"left by an update")
        (self.bin / "other-tool.exe").write_bytes(b"not CITAR's")
        code, out, _ = self.ps1("-Uninstall")
        self.assertEqual(code, 0, out)
        self.assertFalse(self.prefix.exists())
        self.assertEqual(sorted(p.name for p in self.bin.iterdir()),
                         sorted(["citar-mcp.cmd", "other-tool.exe", *lookalikes]))
        code, out, _ = self.ps1("-Uninstall")
        self.assertEqual(code, 0, out)
        # the leftovers alone (no helper) go too: else the bin folder, and its PATH entry, would stay for good
        for name in (*leftovers, *lookalikes):
            (self.bin / name).unlink(missing_ok=True)
        for name in leftovers:
            (self.bin / name).write_bytes(b"left by an update")
        code, out, _ = self.ps1("-Uninstall", "-Worker")
        self.assertEqual(code, 0, out)
        self.assertEqual(sorted(p.name for p in self.bin.iterdir()), ["citar-mcp.cmd", "other-tool.exe"])

    def test_as_a_script_block_it_ends_without_ending_the_session(self):
        """The documented one-liner runs the script as a script block (& ([scriptblock]::Create((iwr ...))) ARGS),
        where an exit would end the user's PowerShell, closing a console window before its message is read."""
        name = "citar-helper-windows-x64.exe"
        for shell in [s for s in ("powershell", "pwsh") if shutil.which(s)]:
            with self.subTest(shell):
                cases = (("a refusal", ("-Version", "bad", "-NoSetup"), None, 1, "is not a CITAR release"),
                         ("a refusal deep in the download", ("-Worker",), self.downloads.url + "/bad", 1,
                          "Nothing was installed"),
                         ("the helper installed", ("-Worker",), self.downloads.url, 0, "service install"),
                         ("the uninstall", ("-Uninstall", "-Worker"), None, 0, "were NOT deleted"))
                for what, args, url, want, says in cases:
                    with self.subTest(what):
                        code, out, _ = self.ps1(*args, url=url, scriptblock=True, shell=shell)
                        self.assertEqual(code, want, out)
                        self.assertIn(says, " ".join(out.split()))
                        self.assertIn(f"{gate_run.SESSION_CONTINUES} (install.ps1 as a script block, status {want})",
                                      out)
                        self.assertNotIn("an error left the script block", out)
                        self.assertNotIn("CITAR's installer stopped", out, "the stop was shown as an error")
                        if what == "the helper installed":
                            self.assertEqual((self.bin / "citar-helper.exe").read_bytes(), stand_in(name))
                        elif what.startswith("a refusal"):
                            self.assertNothingMade()
                self.assertEqual(list(self.bin.iterdir()), [], "the uninstall left the bin folder empty")
                self.bin.rmdir()

    def test_a_prefix_that_is_not_citars_is_never_removed(self):
        self.prefix.mkdir()
        (self.prefix / "mine.txt").write_text("not CITAR's")
        code, out, _ = self.ps1("-Uninstall")
        self.assertEqual(code, 1, out)
        self.assertIn("does not hold a CITAR install", out)
        self.assertTrue((self.prefix / "mine.txt").is_file())
        code, out, _ = self.ps1("-NoSetup", "-Python", sys.executable, "-From", str(self.tmp / "x.whl"))
        self.assertEqual(code, 1, out)
        (self.tmp / "x.whl").write_bytes(b"not read")
        (self.tmp / "citar-0.1.6-cp311-abi3-win_amd64.whl").write_bytes(b"not read")
        code, out, _ = self.ps1("-NoSetup", "-Python", sys.executable, "-From",
                                str(self.tmp / "citar-0.1.6-cp311-abi3-win_amd64.whl"))
        self.assertEqual(code, 1, out)
        self.assertIn("is not empty and holds no CITAR install", " ".join(out.split()))
        self.assertEqual(sorted(p.name for p in self.prefix.iterdir()), ["mine.txt"])


# ------------------------------------------------------------------------------------------------ gate_run install-ps1


class GateRunInstallPs1(unittest.TestCase):
    """What gate_run.py install-ps1 refuses (read on every OS; it runs install.ps1 on Windows only)."""

    def setUp(self):
        # resolved, as gate_run passes the folders (a runner's TEMP is written short, C:\Users\RUNNER~1\...)
        self.under = Path(os.path.realpath(scratch("citar-gate-ps1-")))
        self.addCleanup(shutil.rmtree, self.under, ignore_errors=True)
        self.prefix, self.bin = str(self.under / "p"), str(self.under / "b")

    def check(self, *args: str, env: tuple[str, ...] = ()) -> dict:
        return gate_run.check_install_ps1(list(args), gate_run.check_under(self.under), list(env))

    def refused(self, *args: str, env: tuple[str, ...] = (), says: str = "") -> None:
        with self.assertRaises(gate_run.Refused) as caught:
            self.check(*args, env=env)
        self.assertIn(says, str(caught.exception))

    def test_a_whole_run_is_accepted(self):
        got = self.check("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", "-From", "w.whl")
        self.assertEqual(got, {"-prefix": self.prefix, "-bindir": self.bin, "-nopath": True, "-nosetup": True,
                               "-from": "w.whl"})
        got = self.check(f"-prefix:{self.prefix}", f"-BINDIR:{self.bin}", "-nopath", "-Worker",
                         env=("CITAR_RELEASE_URL=http://127.0.0.1:8123", "CITAR_INSTALL_TEST_FACTS=machine=x86"))
        self.assertTrue(got["-worker"])
        self.check("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-Uninstall")
        self.check("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-Worker",
                   env=("CITAR_RELEASE_URL=http://192.0.2.1/x",))

    def test_without_the_prefix_the_bin_folder_or_no_path(self):
        self.refused("-BinDir", self.bin, "-NoPath", "-NoSetup", says="-Prefix DIR")
        self.refused("-Prefix", self.prefix, "-NoPath", "-NoSetup", says="-BinDir DIR")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoSetup", says="-NoPath")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath:$false", "-NoSetup", says="given alone")

    def test_folders_outside_the_target(self):
        outside = str(Path(tempfile.gettempdir()) / "citar-gate-ps1-elsewhere")
        self.refused("-Prefix", outside, "-BinDir", self.bin, "-NoPath", "-NoSetup", says="is not inside")
        self.refused("-Prefix", self.prefix, "-BinDir", outside, "-NoPath", "-NoSetup", says="is not inside")
        self.refused("-Prefix", str(self.under), "-BinDir", self.bin, "-NoPath", "-NoSetup", says="is not inside")

    def test_a_folder_powershell_reads_its_own_way_is_refused(self):
        # install.ps1 resolves -Prefix and -BinDir through PowerShell (GetUnresolvedProviderPathFromPSPath), which
        # expands a leading ~ to the real home folder and takes a provider's path or a drive of its own; Python reads
        # each as a folder under its working folder, which here is inside --under
        old = os.getcwd()
        os.chdir(self.under)
        self.addCleanup(os.chdir, old)
        cases = [("~", "a leading ~"), (os.path.join("~", "p"), "a leading ~"),
                 ("FileSystem::" + self.prefix, "a provider's path"), (os.path.join("Temp:", "p"), "a drive of its own"),
                 ("p", "is not an absolute path")]
        if os.name == "nt":
            cases += [(self.prefix[2:], "with its drive letter"),           # \dev\...: the current drive's root
                      (self.prefix[:2] + self.prefix[3:], "with its drive letter")]  # C:dev\...: that drive's folder
        for value, says in cases:
            with self.subTest(value):
                self.refused("-Prefix", value, "-BinDir", self.bin, "-NoPath", "-Uninstall", says=says)
                self.refused("-Prefix", self.prefix, "-BinDir", value, "-NoPath", "-Uninstall", says=says)

    def test_powershell_is_given_the_folders_as_checked(self):
        given = self.check("-nopath", "-prefix", os.path.join(self.prefix, "x", ".."), "-BINDIR", self.bin,
                           "-NoSetup", "-From", "w.whl")
        self.assertEqual(gate_run.ps1_arguments(given),
                         ["-NoPath", "-Prefix", self.prefix, "-BinDir", self.bin, "-NoSetup", "-From", "w.whl"])
        given = self.check("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", "-From",
                           "C:\\it's $here\u2019\\w.whl")
        command = gate_run.scriptblock_command(given)
        self.assertIn("& ([scriptblock]::Create([IO.File]::ReadAllText("
                      f"{gate_run.ps1_literal(str(gate_run.INSTALL_PS1))}))) -Prefix '{self.prefix}' -BinDir "
                      f"'{self.bin}' -NoPath -NoSetup -From 'C:\\it''s $here\u2019\u2019\\w.whl'", command)
        self.assertIn(gate_run.SESSION_CONTINUES, command)

    @unittest.skipUnless(os.name == "nt" and shutil.which("powershell"), "install.ps1 runs on Windows")
    def test_the_run_passes_powershell_what_was_checked(self):
        class Started(Exception):
            pass

        for scriptblock in (False, True):
            with self.subTest(scriptblock=scriptblock), mock.patch.object(gate_run, "start",
                                                                          side_effect=Started) as start:
                with self.assertRaises(Started):
                    gate_run.run_install_ps1(["-Prefix", os.path.join(self.prefix, "x", ".."), "-BinDir", self.bin,
                                              "-NoPath", "-Uninstall"], self.under, [], as_scriptblock=scriptblock)
                argv = start.call_args.args[0]
                if scriptblock:
                    self.assertEqual(argv[-2], "-Command")
                    self.assertIn(f"-Prefix '{self.prefix}' -BinDir '{self.bin}' -NoPath -Uninstall", argv[-1])
                else:
                    self.assertEqual(argv[-8:], ["-File", str(gate_run.INSTALL_PS1), "-Prefix", self.prefix,
                                                 "-BinDir", self.bin, "-NoPath", "-Uninstall"])

    def test_abbreviations_unknown_parameters_and_bare_words(self):
        self.refused("-Pre", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", says="write -prefix in full")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoP", "-NoSetup", says="is not a parameter")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", "-Verbose",
                     says="is not a parameter")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", "C:\\x", says="positional")
        self.refused("-Prefix", self.prefix, "-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup",
                     says="given twice")
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-NoSetup", "-From", says="needs a value")

    def test_an_install_needs_no_setup(self):
        self.refused("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", says="-NoSetup")

    def test_the_release_address_and_the_environment(self):
        base = ("-Prefix", self.prefix, "-BinDir", self.bin, "-NoPath", "-Worker")
        self.refused(*base, says="CITAR_RELEASE_URL")
        for url in ("https://github.com/jprodgers/CITAR/releases", "http://citar.jimmieprodgers.com",
                    "http://127.0.0.1@example.com/", "ftp://127.0.0.1/"):
            with self.subTest(url):
                self.refused(*base, env=(f"CITAR_RELEASE_URL={url}",), says="loopback server")
        self.refused(*base, env=("CITAR_RELEASE_URL=http://127.0.0.1:1", "CITAR_DATA_DIR=C:\\x"),
                     says="only CITAR_RELEASE_URL")
        self.refused(*base, env=("PATH=C:\\x",), says="only CITAR_RELEASE_URL")

    def test_the_command_line_refuses_with_exit_2(self):
        code = gate_run.main(["install-ps1", "--under", str(self.under), "--", "-Prefix", self.prefix, "-BinDir",
                              self.bin, "-NoSetup"])
        self.assertEqual(code, 2)

    def test_its_environment_points_every_per_user_folder_inside(self):
        # patch.dict puts the environment back as it was: other modules set CITAR_ variables the whole suite reads
        with mock.patch.dict(os.environ, {"CITAR_GATE_RUN_PROBE": "x"}):
            env = gate_run.install_ps1_env(gate_run.check_under(self.under), ["CITAR_RELEASE_URL=http://127.0.0.1:1"])
        self.assertNotIn("CITAR_GATE_RUN_PROBE", env)
        self.assertFalse([k for k in env if k.startswith("CITAR_") and k not in (
            "CITAR_RELEASE_URL", "CITAR_KEYS_FILE", *gate_run.STATE_VARIABLES)], "an inherited CITAR_ variable")
        self.assertEqual(env["CITAR_RELEASE_URL"], "http://127.0.0.1:1")
        for name in ("LOCALAPPDATA", "APPDATA", "TEMP", "TMP", "CITAR_DATA_DIR", "CITAR_STATE_DIR", "CITAR_KEYS_FILE"):
            with self.subTest(name):
                self.assertTrue(gate_run.within(Path(env[name]), self.under), env[name])
        self.assertEqual(env["PYTHON_KEYRING_BACKEND"], gate_run.NO_KEYRING)

    def test_documentation_addresses(self):
        for url, want in (("http://192.0.2.1/x", True), ("http://203.0.113.9", True), ("http://198.51.100.1:80/", True),
                          ("http://192.0.3.1/", False), ("http://example.com/", False), ("http://[::1]/", False)):
            with self.subTest(url):
                self.assertEqual(gate_run.is_documentation(url), want)


# ------------------------------------------------------------------------------------------------ the scripts' text


class TheScripts(unittest.TestCase):
    """What both scripts hold to, read from their text."""

    def setUp(self):
        self.sh = INSTALL_SH.read_text(encoding="utf-8")
        self.ps1 = INSTALL_PS1.read_text(encoding="utf-8")

    def test_citar_is_installed_from_a_wheel_only(self):
        sh_installs = [line for line in self.sh.splitlines() if re.search(r'"\$VENV_PIP" install .*\$SPEC', line)]
        self.assertEqual(len(sh_installs), 1)
        self.assertIn("--only-binary=citar", sh_installs[0])
        ps1_installs = [line for line in self.ps1.splitlines() if '"install"' in line and "$spec" in line]
        self.assertEqual(len(ps1_installs), 1)
        self.assertIn('"--only-binary=citar"', ps1_installs[0])
        for text in (self.sh, self.ps1):
            self.assertIn("--no-index", text, "a folder's wheel is picked from the folder alone")

    def test_the_helpers_names_are_the_releases(self):
        names = {a["name"] for a in release.assets("0.1.6") if a["group"] == "helper"} - {"citar-helper-manifest.json"}
        self.assertIn('HELPER_ASSET="citar-helper-$PLATFORM-$arch"', self.sh)
        posix = {f"citar-helper-{p}-{a}" for p in ("linux", "macos") for a in ("x64", "arm64")}
        for platform in ("linux", "macos"):
            self.assertIn(f'PLATFORM="{platform}"', self.sh)
        for arch in ("x64", "arm64"):
            self.assertIn(f'arch="{arch}"', self.sh)
        self.assertIn('$asset = "citar-helper-windows-x64.exe"', self.ps1)
        self.assertEqual(names, posix | {"citar-helper-windows-x64.exe"})
        self.assertEqual(set(POSIX_PLATFORMS), {n.removeprefix("citar-helper-") for n in posix})

    def test_one_release_address_and_its_rule(self):
        for text in (self.sh, self.ps1):
            self.assertIn("https://github.com/jprodgers/CITAR/releases", text)
            self.assertIn("CITAR_RELEASE_URL", text)
            self.assertIn("latest/download/", text)
            self.assertIn("download/v", text)

    def test_the_platform_floors_are_the_wheels(self):
        tags = release.WHEEL_TAGS
        self.assertIn("manylinux_2_28_x86_64", tags)
        self.assertIn("at_least \"${libc#glibc-}\" 2.28", self.sh)
        self.assertTrue(all("macosx_11_0" in t for t in tags if t.startswith("macosx")))
        self.assertIn('at_least "$macos" 11', self.sh)
        self.assertIn('$plat -ne "win-amd64"', self.ps1)
        self.assertIn("win_amd64", tags)

    def test_install_ps1_exits_only_when_run_as_a_file(self):
        """Run as a script block (its one-liners), an exit would end the user's PowerShell: every exit is a file's."""
        exits = [line.split("#", 1)[0].strip() for line in self.ps1.splitlines()]
        exits = [line for line in exits if re.search(r"\bexit\b", line)]
        self.assertEqual(len(exits), 5, exits)
        for line in exits:
            self.assertRegex(line, r"^if \(\$AsFile\) \{ exit [^{}]+ \}$")
        # the one-liners are the script-block form, which takes parameters and leaves the session's settings alone
        one_liner = ("& ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/"
                     "install.ps1)))")
        self.assertIn(one_liner + "\n", self.ps1)
        self.assertIn(one_liner + " -Worker\n", self.ps1)
        self.assertIn("`& ([scriptblock]::Create((iwr …)))`", (ROOT / "packaging" / "README.md").read_text(
            encoding="utf-8"))

    def test_the_test_hook_is_named_alike(self):
        for text in (self.sh, self.ps1):
            self.assertIn("CITAR_INSTALL_TEST_FACTS", text)

    def test_install_check_knows_each_scripts_messages(self):
        """install_check.py's expectations are the scripts' own words."""
        for says in ("musl libc", "free-threaded", "no prebuilt CITAR helper", "holds no CITAR wheel", "plain http",
                     "names a user", "built for x64", "wheels only", "service uninstall", "SHA-256"):
            self.assertIn(says.lower(), (self.sh + self.ps1).lower(), says)


class InstallCheck(unittest.TestCase):
    """scripts/ci/install_check.py's parts that need no wheel."""

    def test_pep440(self):
        self.assertEqual(install_check.pep440("0.1.6"), "0.1.6")
        self.assertEqual(install_check.pep440("0.1.6-rc.1"), "0.1.6rc1")
        self.assertEqual(install_check.pep440("1.0.0-beta.2"), "1.0.0b2")

    def test_the_work_folder_is_claimed_only_when_it_is_its_own(self):
        tmp = scratch("citar-install-check-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        work = tmp / "work"
        install_check.claim(work)
        self.assertTrue((work / install_check.MARKER).is_file())
        (work / "old.txt").write_text("an earlier run's")
        install_check.claim(work)
        self.assertFalse((work / "old.txt").exists())
        foreign = tmp / "foreign"
        foreign.mkdir()
        (foreign / "mine.txt").write_text("not the script's")
        with self.assertRaises(install_check.Unusable):
            install_check.claim(foreign)
        self.assertTrue((foreign / "mine.txt").is_file())

    def test_the_release_server_is_laid_out_as_a_release(self):
        tmp = scratch("citar-install-check-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        helper = tmp / "citar-helper-linux-x64"
        helper.write_bytes(b"helper")
        (tmp / "citar-helper-linux-x64.sha256").write_text(hashlib.sha256(b"helper").hexdigest() + "\n")
        served = install_check.Release(tmp / "srv", helper, "0.1.6")
        try:
            import urllib.request
            for path, want in (("/latest/download/citar-helper-linux-x64", b"helper"),
                               ("/download/v0.1.6/citar-helper-linux-x64", b"helper"),
                               ("/bad/latest/download/citar-helper-linux-x64.sha256",
                                (hashlib.sha256(b"not the helper").hexdigest() + "\n").encode())):
                with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(served.url + path) as r:
                    self.assertEqual(r.read(), want)
            self.assertEqual(served.requests[0], "/latest/download/citar-helper-linux-x64")
        finally:
            served.close()

    def test_the_command_line(self):
        tmp = scratch("citar-install-check-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        base = ["--wheel", str(tmp / "w.whl"), "--helper", str(tmp / "h"), "--version", "0.1.6", "--label", "x",
                "--work", str(tmp / "work"), "--under", str(tmp), "--out", str(tmp / "r.json")]
        mode = "ps1" if os.name != "nt" else "sh"  # the other OS's script: refused
        self.assertEqual(install_check.main([mode, *base]), 2)
        here = "sh" if os.name != "nt" else "ps1"
        self.assertEqual(install_check.main([here, *base]), 2, "the wheel is missing")
        with mock.patch.dict(os.environ):
            os.environ.pop("GITHUB_ACTIONS", None)
            self.assertEqual(install_check.main([here, *base, "--path-route"]), 2, "--path-route is CI's")
        self.assertFalse((tmp / "work").exists())


if __name__ == "__main__":
    unittest.main()
