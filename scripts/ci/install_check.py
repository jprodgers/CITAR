"""The install scripts, run against a release's own wheel and helper served from this machine (design/phase5-release.md
P5.8, package 5-07): what package.yml's ``install`` matrix runs on each platform, and what the package's gates run on
the laptop (install.sh in WSL, install.ps1 on Windows).

    python scripts/ci/install_check.py sh  --wheel FILE --helper FILE --version V --label L --work DIR
                                           [--under DIR] [--python PY] [--path-route] --out FILE
    python scripts/ci/install_check.py ps1 --wheel FILE --helper FILE --version V --label L --work DIR
                                           [--under DIR] [--python PY] [--path-route] --out FILE

``--wheel`` is the platform's release wheel; ``--helper`` the platform's self-updating helper under its release name
(``citar-helper-<platform>[.exe]``) with its ``.sha256`` beside it, as package.yml's ``helpers`` job stages them. A
loopback HTTP server (this script's own thread, on 127.0.0.1) serves the helper as a release does,
``latest/download/<name>`` and ``download/v<V>/<name>``, and under ``bad/`` with a ``.sha256`` that is not its own.
Everything a run writes is under ``--work`` (inside ``--under``: by default ``CARGO_TARGET_DIR``, else the runner's
temporary folder), which must be missing, empty or an earlier run's: a temporary ``HOME`` for install.sh, every
prefix and bin folder, the state of each ``citar`` run. install.sh runs with ``--prefix``, ``--bin-dir`` and
``--no-profile``; install.ps1 only through ``gate_run.py install-ps1`` (``-Prefix`` and ``-BinDir`` inside
``--under``, ``-NoPath``, the user's PATH compared before and after); the helper only through ``gate_run.py helper``.

The checks, in order (each recorded in the report, ``--out``):

1. **The platform check**, through the scripts' test hook (``CITAR_INSTALL_TEST_FACTS``): musl, an unknown
   architecture, glibc 2.27, macOS 10.15 and a free-threaded Python (install.sh); an ARM64, a 32-bit and a
   free-threaded Python, and an x86 machine for the helper (install.ps1). Each exits 1 with its message, having
   created nothing.
2. **Wheels only:** a folder holding only a source distribution is refused with the no-wheel message.
3. **The game** from the wheel (``--from``/``-From``, no setup): it installed into the prefix, its commands are in
   the bin folder, and they run: ``citar --version`` is V, ``citar where``, ``citar doctor --quiet`` and
   ``citar doctor``, whose engine line names the label L and the version V, and ``citar setup --local
   --non-interactive``.
4. **The helper** (``--worker``/``-Worker``, ``CITAR_RELEASE_URL`` the loopback server): the file in the bin folder
   is the helper, byte for byte, and through ``gate_run.py helper`` its ``--version`` names V, its target and L; with
   ``--version V`` it comes from ``download/v<V>/``.
5. **The download's checks:** a ``.sha256`` that is not the file's is refused before anything is written (the bin
   folder is never made), and so is plain http to a host that is not this machine (a documentation address, RFC
   5737, which no machine answers), before any request. install.ps1 then runs as its one-liner does, a script
   block (``gate_run.py install-ps1 --as-scriptblock``): ``-Worker`` installs the helper and a wrong ``.sha256`` is
   refused, each ending the script without ending its PowerShell.
6. **The uninstall** with the same prefix and bin folder: the prefix is gone and the bin folder empty, and nothing
   else under ``--work`` moved; install.sh refuses while the helper is a service (its unit names the binary).
7. **With ``--path-route``** (on a CI runner only, ``GITHUB_ACTIONS=true``: it writes the user's profile or PATH):
   install.sh's helper without ``--no-profile`` adds its bin folder to the temporary ``HOME``'s profile;
   install.ps1's without ``-NoPath`` adds it to the user's PATH (``HKCU\\Environment``), which ``-Uninstall`` puts back
   byte for byte; and its uninstall refuses while the helper starts at sign-in (a Run value naming it).

Exit status 0 when every check holds, 1 when one does not (each named), 2 when the command cannot run.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[2]
INSTALL_SH = ROOT / "install.sh"


def _beside(name: str):
    """A script beside this one, as a module (the scripts are not a package)."""
    known = sys.modules.get(f"citar_ci_{name}")
    if known is not None:
        return known
    spec = importlib.util.spec_from_file_location(f"citar_ci_{name}", HERE.parent / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


gate_run = _beside("gate_run")
helper_release = _beside("helper_release")

MARKER = ".install_check"
MARKER_TEXT = "Made by scripts/ci/install_check.py, which may clear this folder on its next run.\n"
#: A documentation address (RFC 5737, TEST-NET-1): no machine answers it, so a script that tried it would hang.
UNREACHABLE = "http://192.0.2.1/citar"
#: How long one script run may take: the game's install downloads its dependencies.
RUN_TIMEOUT = 900.0
#: A refusal that must come before any request returns at once; anything that tried the address would take longer.
REFUSE_SECONDS = 20.0
#: The engine line of `citar doctor` (citar/doctor.py's _check_engine): build B (L), version V.
DOCTOR_ENGINE = re.compile(r"build (?P<build>[0-9a-f]{12}) \((?P<label>[^)]*)\), version (?P<version>\S+)")
#: The state variables every citar run gets, under the run's own folder (smoke_wheel.py's).
STATE = {"CITAR_STATE_DIR": ".", "CITAR_DATA_DIR": "data", "CITAR_SAVE_DIR": "saves", "CITAR_CONFIG_DIR": "config",
         "CITAR_BENCH_DIR": "benchmarks"}


class Unusable(Exception):
    """The command cannot run: exit 2."""


class Failed(Exception):
    """A check does not hold."""


def pep440(version: str) -> str:
    return version.replace("-alpha.", "a").replace("-beta.", "b").replace("-rc.", "rc")


def is_link(path: Path) -> bool:
    return path.is_symlink() or bool(getattr(path, "is_junction", lambda: False)())


def claim(work: Path) -> None:
    """``--work`` emptied (or made) and marked, if it is missing, empty or an earlier run's; else Unusable."""
    if is_link(work):
        raise Unusable(f"--work {work} is a link: name a folder of its own")
    if work.exists():
        if not work.is_dir():
            raise Unusable(f"--work {work} is not a folder")
        if any(work.iterdir()) and not (work / MARKER).is_file():
            raise Unusable(f"--work {work} holds files this script did not make (no {MARKER}): name an empty folder")
        shutil.rmtree(work)
    work.mkdir(parents=True)
    (work / MARKER).write_text(MARKER_TEXT, encoding="utf-8")


# ------------------------------------------------------------------------------------------------ the release server


class Release:
    """A loopback HTTP server laid out as a release's downloads: ``latest/download/<name>``, ``download/v<V>/<name>``,
    and ``bad/latest/download/<name>`` whose ``.sha256`` is not the file's. Every path asked for is kept."""

    def __init__(self, root: Path, helper: Path, version: str):
        self.root, self.requests = root, []
        sums = helper.with_name(helper.name + ".sha256")
        if not sums.is_file():
            raise Unusable(f"{sums} is missing: the helper comes with its .sha256, as package.yml stages it")
        for folder in (root / "latest" / "download", root / "download" / f"v{version}",
                       root / "bad" / "latest" / "download"):
            folder.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(helper, folder / helper.name)
            shutil.copyfile(sums, folder / sums.name)
        wrong = hashlib.sha256(b"not the helper").hexdigest()
        (root / "bad" / "latest" / "download" / sums.name).write_text(wrong + "\n", encoding="ascii", newline="\n")
        requests = self.requests

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(root), **kwargs)

            def log_message(self, format, *args):
                requests.append(self.path)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


# ------------------------------------------------------------------------------------------------ the runs


class Check:
    """The checks and the report."""

    def __init__(self, mode: str, work: Path):
        self.mode, self.work = mode, work
        self.items: list[dict] = []

    def record(self, name: str, ok: bool, detail: str = "") -> bool:
        self.items.append({"check": name, "ok": ok, "detail": detail})
        print(f"install_check: {'ok' if ok else 'FAILED'}: {name}{': ' + detail if detail else ''}", flush=True)
        return ok

    def failed(self) -> list[str]:
        return [f"{i['check']}: {i['detail']}" for i in self.items if not i["ok"]]


def tail(text: str | None, lines: int = 12) -> str:
    return "\n".join((text or "").strip().splitlines()[-lines:])


def citar_env(work: Path, name: str) -> dict[str, str]:
    """A citar run's environment: no inherited CITAR_* variable, its state and per-user folders under ``work``."""
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
    base = work / "state" / name
    for var, sub in STATE.items():
        (base / sub).mkdir(parents=True, exist_ok=True)
        env[var] = str(base / sub)
    env["PYTHON_KEYRING_BACKEND"] = gate_run.NO_KEYRING
    env["HOME"] = str(work / "home")
    if os.name == "nt":
        for var, sub in (("LOCALAPPDATA", "Local"), ("APPDATA", "Roaming")):
            (base / "user" / sub).mkdir(parents=True, exist_ok=True)
            env[var] = str(base / "user" / sub)
    else:
        env["XDG_DATA_HOME"] = str(work / "home" / ".local" / "share")
        env["XDG_CONFIG_HOME"] = str(work / "home" / ".config")
    return env


def run(argv: list[str], env: dict[str, str], timeout: float = RUN_TIMEOUT) -> tuple[int, str, float]:
    began = time.perf_counter()
    try:
        done = subprocess.run(argv, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace",
                              timeout=timeout)
    except subprocess.TimeoutExpired as e:
        out = e.stdout.decode("utf-8", "replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        return 124, out + f"\n(timed out after {timeout:.0f} s)", time.perf_counter() - began
    return done.returncode, (done.stdout or "") + (done.stderr or ""), time.perf_counter() - began


def created(*paths: Path) -> list[str]:
    """Which of ``paths`` exist (a refused run must have made none of them)."""
    return [str(p) for p in paths if p.exists() or is_link(p)]


def listing(folder: Path) -> list[str]:
    return sorted(p.name for p in folder.iterdir()) if folder.is_dir() else []


class Script:
    """One install script and how it is run here."""

    def __init__(self, mode: str, work: Path, under: Path, python: str, release: Release, version: str):
        self.mode, self.work, self.under, self.python, self.release = mode, work, under, python, release
        self.version = version
        self.home = work / "home"
        self.home.mkdir(exist_ok=True)
        (work / "tmp").mkdir(exist_ok=True)

    # install.sh: a temporary HOME, --no-profile unless asked, every folder under --work
    def sh(self, args: list[str], facts: str = "", release_url: str | None = None, profile: bool = False,
           timeout: float = RUN_TIMEOUT) -> tuple[int, str, float]:
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
        env.update(HOME=str(self.home), TMPDIR=str(self.work / "tmp"), NO_COLOR="1", SHELL="/bin/bash",
                   XDG_DATA_HOME=str(self.home / ".local" / "share"), XDG_CONFIG_HOME=str(self.home / ".config"))
        if facts:
            env["CITAR_INSTALL_TEST_FACTS"] = facts
        if release_url is not None:
            env["CITAR_RELEASE_URL"] = release_url
        if not profile and "--uninstall" not in args:
            args = [*args, "--no-profile"]
        return run(["bash", str(INSTALL_SH), *args], env, timeout)

    # install.ps1: only through gate_run.py install-ps1, -NoPath always
    def ps1(self, args: list[str], facts: str = "", release_url: str | None = None,
            timeout: float = RUN_TIMEOUT, scriptblock: bool = False) -> tuple[int, str, float]:
        extra = []
        if facts:
            extra.append(f"CITAR_INSTALL_TEST_FACTS={facts}")
        if release_url is not None:
            extra.append(f"CITAR_RELEASE_URL={release_url}")
        began = time.perf_counter()
        try:
            done = gate_run.run_install_ps1([*args, "-NoPath"], self.under, extra, timeout=timeout, capture=True,
                                            as_scriptblock=scriptblock)
        except gate_run.Refused as e:
            return 2, f"gate_run: refused: {e}", time.perf_counter() - began
        return done.returncode, (done.stdout or "") + (done.stderr or ""), time.perf_counter() - began

    def __call__(self, kind: str, **opts) -> tuple[int, str, float]:
        """``kind``: refuse | game | worker | uninstall, with the folders each takes (``prefix``, ``bin``)."""
        sh = self.mode == "sh"
        args: list[str] = []
        if kind in ("game", "refuse") and opts.get("wheel"):
            args += ["--from" if sh else "-From", str(opts["wheel"])]
        if kind in ("game", "refuse"):
            args += ["--no-setup" if sh else "-NoSetup", "--python" if sh else "-Python", self.python]
        if kind == "worker":
            args += ["--worker" if sh else "-Worker"]
        if opts.get("version"):
            args += ["--version" if sh else "-Version", opts["version"]]
        if kind == "uninstall":
            args += ["--uninstall" if sh else "-Uninstall"]
        args += ["--prefix" if sh else "-Prefix", str(opts.get("prefix") or self.work / "prefix"),
                 "--bin-dir" if sh else "-BinDir", str(opts["bin"])]
        if sh:
            return self.sh(args, facts=opts.get("facts", ""), release_url=opts.get("url"),
                           profile=opts.get("profile", False))
        return self.ps1(args, facts=opts.get("facts", ""), release_url=opts.get("url"),
                        scriptblock=opts.get("scriptblock", False))


# ------------------------------------------------------------------------------------------------ the checks


def check_refusals(c: Check, s: Script, wheel: Path) -> None:
    """1 and 2: the platform check's messages through the hook, and a folder holding only an sdist."""
    linux = "system=Linux machine=x86_64 libc=glibc-2.39"
    if s.mode == "sh":
        cases = [
            ("musl", "game", "system=Linux machine=x86_64 libc=musl", "musl libc"),
            ("an unknown architecture", "game", "system=Linux machine=riscv64 libc=glibc-2.39", "Linux on riscv64"),
            ("glibc 2.27", "game", "system=Linux machine=x86_64 libc=glibc-2.27", "glibc 2.27"),
            ("macOS 10.15", "game", "system=Darwin machine=arm64 macos=10.15", "macOS 10.15"),
            ("a free-threaded Python", "game", f"{linux} free_threaded=1", "free-threaded"),
            ("the helper on musl", "worker", "system=Linux machine=aarch64 libc=musl", "no prebuilt CITAR helper"),
        ]
    else:
        cases = [
            ("an ARM64 Python", "game", "python_platform=win-arm64", "built for x64"),
            ("a 32-bit Python", "game", "python_platform=win32", "no prebuilt CITAR for this platform"),
            ("a free-threaded Python", "game", "free_threaded=1", "free-threaded"),
            ("the helper on x86", "worker", "machine=x86", "no prebuilt CITAR helper"),
        ]
    for name, kind, facts, says in cases:
        prefix, bin_dir = s.work / "refused" / "prefix", s.work / "refused" / "bin"
        asked = len(s.release.requests)
        code, out, _ = s(kind, wheel=wheel, facts=facts, prefix=prefix, bin=bin_dir, url=s.release.url)
        made = created(prefix, bin_dir, s.work / "refused")
        c.record(f"refused: {name}",
                 code == 1 and says.lower() in out.lower() and not made and len(s.release.requests) == asked,
                 f"exit {code}, {'says' if says.lower() in out.lower() else 'does not say'} {says!r}"
                 + (f", made {made}" if made else "") + ("" if code == 1 else f"\n{tail(out)}"))
    sdist_dir = s.work / "sdist-only"
    sdist_dir.mkdir()
    sdist = sdist_dir / f"citar-{pep440(s.version)}.tar.gz"
    sdist.write_bytes(b"a stand-in: the script refuses on the name, before pip")
    prefix, bin_dir = s.work / "refused" / "prefix", s.work / "refused" / "bin"
    code, out, _ = s("refuse", wheel=sdist_dir, prefix=prefix, bin=bin_dir)
    made = created(prefix, bin_dir)
    c.record("refused: a folder holding only a source distribution",
             code == 1 and "holds no CITAR wheel" in out and sdist.name in out and "wheels only" in out and not made,
             f"exit {code}" + (f", made {made}" if made else "") + ("" if code == 1 else f"\n{tail(out)}"))


def commands_of(s: Script, bin_dir: Path, prefix: Path) -> dict[str, Path]:
    if s.mode == "sh":
        return {"citar": bin_dir / "citar", "python": prefix / "venv" / "bin" / "python"}
    return {"citar": bin_dir / "citar.cmd", "citar.exe": prefix / "Scripts" / "citar.exe",
            "python": prefix / "Scripts" / "python.exe"}


def check_game(c: Check, s: Script, wheel: Path, version: str, label: str) -> dict:
    """3: the game from the wheel, and its commands run."""
    prefix, bin_dir = s.work / "prefix", s.work / "bin"
    code, out, seconds = s("game", wheel=wheel, prefix=prefix, bin=bin_dir)
    if not c.record("the game installs from the wheel", code == 0, f"exit {code} in {seconds:.0f} s"
                    + ("" if code == 0 else f"\n{tail(out, 30)}")):
        return {}
    print(tail(out, 40), flush=True)
    found: dict = {"install_seconds": round(seconds, 1)}
    marker = prefix / ".citar-installer"
    c.record("the prefix is marked as the script's", marker.is_file(),
             marker.read_text(encoding="utf-8", errors="replace").strip() if marker.is_file() else "no marker")
    names = ("citar", "citar-admin", "citar-mcp")
    if s.mode == "sh":
        links = {n: os.readlink(bin_dir / n) if (bin_dir / n).is_symlink() else None for n in names}
        c.record("the commands are links into the prefix",
                 all(v and v.startswith(str(prefix) + "/") for v in links.values()), json.dumps(links))
    else:
        shims = {n: (bin_dir / f"{n}.cmd").is_file() for n in names}
        c.record("the commands are shims in the bin folder", all(shims.values()), json.dumps(shims))
    cmd = commands_of(s, bin_dir, prefix)
    env = citar_env(s.work, "game")
    code, out, _ = run([str(cmd["citar"]), "--version"], env, 120)
    c.record("citar --version", code == 0 and out.strip() == f"CITAR {version}", f"exit {code}: {out.strip()}")
    for args in (["where"], ["doctor", "--quiet"], ["setup", "--local", "--non-interactive"]):
        code, out, _ = run([str(cmd.get("citar.exe", cmd["citar"])), *args], env, 300)
        c.record(f"citar {' '.join(args)}", code == 0, f"exit {code}" + ("" if code == 0 else f"\n{tail(out)}"))
    code, out, _ = run([str(cmd.get("citar.exe", cmd["citar"])), "doctor"], env, 300)
    m = DOCTOR_ENGINE.search(out)
    got = m.groupdict() if m else {}
    c.record("citar doctor names the version and the label",
             bool(m) and got.get("label") == label and got.get("version") == version, json.dumps(got) or tail(out))
    found["doctor"] = got
    probe = ("import importlib.metadata as m, json, citar, citar.engine_api as e; "
             "print(json.dumps([m.version('citar'), citar.__file__, e.build_info()['version']]))")
    code, out, _ = run([str(cmd["python"]), "-I", "-c", probe], env, 120)
    try:
        dist, where, engine = json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        dist, where, engine = None, "", None
    inside = os.path.normcase(os.path.abspath(where)).startswith(os.path.normcase(str(prefix)) + os.sep)
    c.record("the installed CITAR is the wheel's, in the prefix", code == 0 and dist == pep440(version) and inside
             and engine == version, f"citar {dist} at {where}, engine {engine}")
    return found


def check_helper(c: Check, s: Script, helper: Path, version: str, label: str, target: str) -> dict:
    """4 and 5: the helper from the loopback release, then the download's refusals."""
    bin_dir = s.work / "bin"
    name = "citar-helper.exe" if s.mode == "ps1" else "citar-helper"
    found: dict = {}
    before = len(s.release.requests)
    code, out, seconds = s("worker", bin=bin_dir, url=s.release.url)
    asked = s.release.requests[before:]
    installed = bin_dir / name
    same = installed.is_file() and installed.read_bytes() == helper.read_bytes()
    c.record("the helper installs from the release's latest", code == 0 and same and asked == [
        f"/latest/download/{helper.name}.sha256", f"/latest/download/{helper.name}"],
        f"exit {code} in {seconds:.1f} s, asked {asked}" + ("" if code == 0 else f"\n{tail(out)}"))
    if s.mode == "sh":
        c.record("it is executable", installed.is_file() and os.access(installed, os.X_OK), oct(installed.stat().st_mode)
                 if installed.exists() else "missing")
    if installed.is_file():
        try:
            runner = helper_release.Runner(installed, s.under, 120.0)
            out = runner("--version", "--home", str(s.work / "helper-home"))
            line = helper_release.read_version(out, name, version, target, label)
            found["line"] = line["line"]
            c.record("the installed helper runs (gate_run.py helper)", True, line["line"])
        except helper_release.Failed as e:
            c.record("the installed helper runs (gate_run.py helper)", False, str(e))
    pinned = s.work / "bin-version"
    before = len(s.release.requests)
    code, out, _ = s("worker", bin=pinned, url=s.release.url, version=version)
    asked = s.release.requests[before:]
    same = (pinned / name).is_file() and (pinned / name).read_bytes() == helper.read_bytes()
    c.record("--version takes the helper from that release", code == 0 and same and asked == [
        f"/download/v{version}/{helper.name}.sha256", f"/download/v{version}/{helper.name}"],
        f"exit {code}, asked {asked}" + ("" if code == 0 else f"\n{tail(out)}"))
    bad = s.work / "bin-bad"
    code, out, _ = s("worker", bin=bad, url=s.release.url + "/bad")
    c.record("a .sha256 that is not the file's is refused, nothing written",
             code == 1 and "SHA-256" in out and not created(bad), f"exit {code}, bin folder "
             + ("made" if created(bad) else "never made") + ("" if code == 1 else f"\n{tail(out)}"))
    for url, says in ((UNREACHABLE, "plain http"), ("http://127.0.0.1@192.0.2.1/citar", "names a user")):
        refused = s.work / "bin-refused"
        code, out, seconds = s("worker", bin=refused, url=url)
        c.record(f"CITAR_RELEASE_URL {url} is refused before any request",
                 code == 1 and says in out and seconds < REFUSE_SECONDS and not created(refused),
                 f"exit {code} in {seconds:.1f} s" + ("" if code == 1 and says in out else f"\n{tail(out)}"))
    if s.mode == "ps1":
        check_scriptblock(c, s, helper, name)
    return found


def check_scriptblock(c: Check, s: Script, helper: Path, name: str) -> None:
    """4 and 5 again as the documented one-liner runs install.ps1, a script block, where an exit would end the
    user's PowerShell (closing a console window before its message is read): the script must end, the session
    go on."""
    folder = s.work / "bin-scriptblock"
    code, out, _ = s("worker", bin=folder, url=s.release.url, scriptblock=True)
    same = (folder / name).is_file() and (folder / name).read_bytes() == helper.read_bytes()
    went_on = gate_run.SESSION_CONTINUES in out and "an error left the script block" not in out
    c.record("-Worker as a script block (the one-liner) installs the helper, and the session goes on",
             code == 0 and same and went_on, f"exit {code}, {'the same file' if same else 'not the helper'}, "
             + ("the session went on" if went_on else f"\n{tail(out)}"))
    refused = s.work / "bin-scriptblock-bad"
    code, out, _ = s("worker", bin=refused, url=s.release.url + "/bad", scriptblock=True)
    went_on = gate_run.SESSION_CONTINUES in out and "an error left the script block" not in out
    c.record("a refusal in a script block ends the script, not the session",
             code == 1 and "SHA-256" in out and went_on and not created(refused),
             f"exit {code}, bin folder {'made' if created(refused) else 'never made'}, "
             + ("the session went on" if went_on else f"\n{tail(out)}"))


def check_uninstall(c: Check, s: Script) -> None:
    """6: the uninstall removes the prefix and its files from the bin folder, and nothing else."""
    prefix, bin_dir = s.work / "prefix", s.work / "bin"
    keep = s.work / "keep.txt"
    keep.write_text("not CITAR's\n", encoding="utf-8")
    elsewhere = s.work / "bin-version"
    others = listing(elsewhere)
    if s.mode == "sh":
        unit = s.home / ".config" / "systemd" / "user" / "citar-helper.service"
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text(f"[Service]\nExecStart={bin_dir / 'citar-helper'} run --service\n", encoding="utf-8")
        code, out, _ = s("uninstall", prefix=prefix, bin=bin_dir)
        c.record("the uninstall waits for the helper's service to go first",
                 code == 1 and "service uninstall" in out and prefix.is_dir() and (bin_dir / "citar-helper").is_file(),
                 f"exit {code}" + ("" if code == 1 else f"\n{tail(out)}"))
        unit.unlink()
    code, out, _ = s("uninstall", prefix=prefix, bin=bin_dir)
    c.record("the uninstall leaves the prefix gone and the bin folder empty",
             code == 0 and not prefix.exists() and listing(bin_dir) == [],
             f"exit {code}, prefix {'left' if prefix.exists() else 'gone'}, bin folder {listing(bin_dir)}"
             + ("" if code == 0 else f"\n{tail(out)}"))
    c.record("and nothing else", keep.is_file() and listing(elsewhere) == others, f"{listing(elsewhere)}")
    code, out, _ = s("uninstall", prefix=prefix, bin=bin_dir)
    c.record("a second uninstall has nothing to do", code == 0, f"exit {code}")


def check_path_route(c: Check, s: Script) -> None:
    """7 (a CI runner only): the profile line, and the user's PATH written and put back."""
    if s.mode == "sh":
        bin_dir = s.work / "bin-profile"
        code, out, _ = s("worker", bin=bin_dir, url=s.release.url, profile=True)
        profile = s.home / (".bash_profile" if sys.platform == "darwin" else ".bashrc")
        text = profile.read_text(encoding="utf-8") if profile.is_file() else ""
        c.record("without --no-profile the bin folder goes into the shell's profile",
                 code == 0 and f'export PATH="{bin_dir}:$PATH"' in text, f"exit {code}, {profile.name}: {text.strip()}")
        return
    import winreg
    bin_dir = s.work / "bin-path"
    before = gate_run.user_path()
    argv = [shutil.which("powershell") or "powershell", "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy",
            "Bypass", "-File", str(gate_run.INSTALL_PS1)]
    env = gate_run.install_ps1_env(s.under, [f"CITAR_RELEASE_URL={s.release.url}"])
    code, out, _ = run([*argv, "-Worker", "-Prefix", str(s.work / "prefix-path"), "-BinDir", str(bin_dir)], env)
    during = gate_run.user_path()
    entries = (during or ("", 0))[0].split(";")
    c.record("without -NoPath the bin folder goes on the user's PATH", code == 0 and str(bin_dir) in entries
             and (during or ("", 0))[1] == (before or ("", winreg.REG_EXPAND_SZ))[1], f"exit {code}")
    run_key = r"Software\Microsoft\Windows\CurrentVersion\Run"
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, run_key, 0, winreg.KEY_SET_VALUE) as key:
        winreg.SetValueEx(key, "CITAR helper", 0, winreg.REG_SZ, f'"{bin_dir / "citar-helper.exe"}" run --detach')
    try:
        code, out, _ = run([*argv, "-Uninstall", "-Worker", "-Prefix", str(s.work / "prefix-path"), "-BinDir",
                            str(bin_dir)], env)
        c.record("its uninstall waits for the Run value to go first",
                 code == 1 and "service uninstall" in out and (bin_dir / "citar-helper.exe").is_file(), f"exit {code}")
    finally:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, run_key, 0, winreg.KEY_SET_VALUE) as key:
            winreg.DeleteValue(key, "CITAR helper")
    code, out, _ = run([*argv, "-Uninstall", "-Worker", "-Prefix", str(s.work / "prefix-path"), "-BinDir",
                        str(bin_dir)], env)
    after = gate_run.user_path()
    c.record("its uninstall puts the user's PATH back as it was", code == 0 and after == before,
             f"exit {code}, {gate_run.fingerprint(before)} -> {gate_run.fingerprint(during)} -> "
             f"{gate_run.fingerprint(after)}")


# ------------------------------------------------------------------------------------------------ command line


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="install_check.py", description=__doc__.split("\n\n", 1)[0], allow_abbrev=False)
    ap.add_argument("mode", choices=("sh", "ps1"))
    ap.add_argument("--wheel", type=Path, required=True)
    ap.add_argument("--helper", type=Path, required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--under", type=Path, default=None)
    ap.add_argument("--python", default=sys.executable)
    ap.add_argument("--path-route", action="store_true")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args(argv)
    began = time.perf_counter()
    try:
        if args.mode == "ps1" and os.name != "nt":
            raise Unusable("install.ps1 is checked on Windows")
        if args.mode == "sh" and os.name == "nt":
            raise Unusable("install.sh is checked on Linux and macOS (in WSL on this laptop)")
        if args.path_route and os.environ.get("GITHUB_ACTIONS") != "true":
            raise Unusable("--path-route writes the user's profile or PATH: on a CI runner only")
        default = os.environ.get("CARGO_TARGET_DIR") or os.environ.get("RUNNER_TEMP")
        under = gate_run.check_under(args.under or (Path(default) if default else None))
        work = gate_run.inside(args.work, under, "--work")
        for path, what in ((args.wheel, "--wheel"), (args.helper, "--helper")):
            if not path.is_file():
                raise Unusable(f"{what} {path} is not a file")
        platform = next((p for p in helper_release.TARGETS if helper_release.asset_name(p) == args.helper.name), None)
        if platform is None:
            raise Unusable(f"--helper {args.helper.name} is not a release helper's name (citar-helper-<platform>)")
        claim(work)
    except (Unusable, gate_run.Refused) as e:
        print(f"install_check: cannot run: {e}", file=sys.stderr)
        return 2
    wheel, helper = args.wheel.resolve(), args.helper.resolve()
    check = Check(args.mode, work)
    release = Release(work / "srv", helper, args.version)
    found: dict = {}
    try:
        script = Script(args.mode, work, under, args.python, release, args.version)
        print(f"install_check: {args.mode} on {platform}, release server {release.url}, work {work}", flush=True)
        check_refusals(check, script, wheel)
        found["game"] = check_game(check, script, wheel, args.version, args.label)
        found["helper"] = check_helper(check, script, helper, args.version, args.label,
                                       helper_release.TARGETS[platform])
        check_uninstall(check, script)
        if args.path_route:
            check_path_route(check, script)
    finally:
        release.close()
    report = {"install_check": args.mode, "platform": platform, "version": args.version, "label": args.label,
              "wheel": wheel.name, "helper": helper.name, "checks": check.items, **found,
              "seconds": round(time.perf_counter() - began, 1)}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8")
    failed = check.failed()
    if failed:
        print(f"install_check: FAILED: {len(failed)} of {len(check.items)} checks:", file=sys.stderr)
        for line in failed:
            print(f"  {line.splitlines()[0]}", file=sys.stderr)
        return 1
    print(f"install_check: passed: {len(check.items)} checks in {report['seconds']} s; wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
