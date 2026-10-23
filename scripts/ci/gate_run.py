"""The gate harness: how a package's gates run a release helper, a server or install.ps1 on this laptop, so none can
touch a real installation, the user's PATH or open a port beyond loopback (design/phase5-release.md P5.18.1 rules 2,
3 and 11; packages 5-03 and 5-07).

    python scripts/ci/gate_run.py helper [--under DIR] [--timeout S] -- BINARY ARGS...
    python scripts/ci/gate_run.py server --port N --state DIR [--under DIR] [--env NAME=VALUE]... [--ready PATH]
                                         [--stop-file FILE] [--timeout S] -- COMMAND ARGS...
    python scripts/ci/gate_run.py install-ps1 [--under DIR] [--env NAME=VALUE]... [--shell powershell|pwsh]
                                              [--as-scriptblock] [--timeout S] -- ARGS...

``--under`` is the folder every run's files must be in: the package's target folder (by default
``$CARGO_TARGET_DIR``), or the runner's temporary folder on CI. It must exist, and must be neither inside this
checkout nor the per-user CITAR folder (``%LOCALAPPDATA%\\CITAR``, ``~/Library/Application Support/CITAR``,
``~/.local/share/citar``), nor hold either.

**helper.** A release or packaged helper has no development guard: without ``--home`` its home is the per-user
folder, where a real installation keeps ``helper.json`` and the machine key. So the run is refused unless ARGS give
``--home DIR`` exactly once, in full, with DIR inside ``--under``; any abbreviation of ``--home``, ``--server``,
``--base-url``, ``--config``, ``--hardware``, ``--ca-file``, ``--detach`` or ``--supervisor`` is refused (the helper
accepts unambiguous prefixes, and this reads only full names). Every file the run names is inside ``--under`` too:
``--config``, ``--hardware`` and ``--ca-file``, the FILE ``scan`` writes its report to, and the BINARY itself for
``update apply`` and ``update rollback``, which put a binary in its place and keep the old one beside it. It is also
refused when it would reach beyond loopback or leave something behind: a ``--server`` or ``--base-url`` (or the
home's ``helper.json``'s ``server_url`` or ``base_url``) that is not a loopback address, ``--detach`` or
``--supervisor`` (a process that outlives the run), and ``service install``, ``uninstall`` or ``run`` without
``--dry-run`` (nothing is installed on this laptop, P5.18.1 rule 1). The helper runs with every ``CITAR_*`` variable
of this environment dropped, ``CITAR_DATA_DIR`` set to its home, and the per-user base (``LOCALAPPDATA`` on Windows,
``XDG_DATA_HOME`` on Linux) pointed inside ``--under``; its output passes through, and its exit status is this
command's.

**server.** Server mode's default host is ``0.0.0.0`` (``citar/settings.py``), and an unsigned program listening
on every interface raises Windows Defender Firewall's prompt, whose dismissal adds block rules. So the server runs
with ``CITAR_HOST=127.0.0.1`` and ``CITAR_PORT`` the ``--port`` given, which must be free on loopback and not 8799
(the owner's); a ``--env`` that names another host or port, or a COMMAND argument ``--host`` or ``--port`` that
differs, is refused. Every ``CITAR_*`` variable of this environment is dropped, and the server's state is all under
``--state``, inside ``--under``: the five state variables (``CITAR_STATE_DIR``, ``CITAR_DATA_DIR``,
``CITAR_SAVE_DIR``, ``CITAR_CONFIG_DIR``, ``CITAR_BENCH_DIR``); the API key store (``citar/keystore.py``): its
encrypted file, ``CITAR_KEYS_FILE``, in ``<state>/config``, and no operating-system credential store
(``PYTHON_KEYRING_BACKEND`` is keyring's failing backend, so the Servers page offers the file and the environment
only, never the owner's Windows Credential Manager or Keychain); and the per-user configuration and data bases
(``APPDATA`` and ``LOCALAPPDATA`` on Windows, ``XDG_CONFIG_HOME`` and ``XDG_DATA_HOME`` on Linux) in
``<state>/user``, so a default the variables above miss lands there too. A ``--env`` may not undo that: it may not
name ``PYTHON_KEYRING_BACKEND`` or a per-user base, its ``CITAR_KEYS_FILE`` must be inside ``--under``, and its
``CITAR_DB_URL`` must be a SQLite database inside ``--under`` (or in memory). With ``--ready PATH`` it prints
``gate_run: ready <url>`` once
``http://127.0.0.1:N<PATH>`` answers. It runs until the stop file appears (``--stop-file``, by default
``<state>/gate_run.stop``; the gate makes it when it is done), until ``--timeout``, or until the server exits by
itself; then it stops the server's whole process tree (a polite signal, then a kill), and checks that none of those
processes is left and the port is closed. Exit status 0 when it was stopped on request and nothing is left.

**install-ps1.** The checkout's ``install.ps1``, in Windows PowerShell (or ``pwsh``) with ``-NoProfile`` and this
process's execution policy only, given ARGS. Without ``-Prefix`` and ``-BinDir`` it would install into the per-user
CITAR folder, and without ``-NoPath`` write the user's PATH (the registry): so the run is refused unless ARGS give
``-Prefix DIR`` and ``-BinDir DIR``, each once and strictly inside ``--under``, and ``-NoPath``. Each DIR is an
absolute path (with its drive letter on Windows) with no leading ``~`` and no other ``:``, since the script resolves
the text through PowerShell, which reads those as the home folder, a provider's path or a drive of its own; and the
script is given the folders as resolved here, never the text. Every parameter is written in full (PowerShell binds
any unambiguous prefix, ``-Pre`` for ``-Prefix``, and this reads only full names), a switch takes no ``:value``
(``-NoPath:$false`` would undo it), and nothing else is passed: no bare word, no common parameter. An install runs
only with ``-NoSetup`` (the wizard would wait for answers). With ``--as-scriptblock`` the script runs as its
documented one-liner does (``& ([scriptblock]::Create((iwr -useb URL))) ARGS``, the checkout's file for the
download), and ``gate_run: the session continues`` is printed once it has ended without ending its PowerShell (an
``exit`` in it would have closed the user's window); the exit status is its ``$LASTEXITCODE``. The environment it
gets is ``--env``'s (only ``CITAR_RELEASE_URL``, which must be a loopback address, or for a test of the script's
refusal a documentation address, RFC 5737, which no machine has; and ``CITAR_INSTALL_TEST_FACTS``, the script's test
hook) with every ``CITAR_*`` variable of this one dropped, the five state variables under ``--under``, no OS
credential store, and the per-user bases (``LOCALAPPDATA``, ``APPDATA``) and the temporary folder (``TEMP``,
``TMP``) pointed inside ``--under``; ``-Worker`` needs a ``CITAR_RELEASE_URL``, since without it the helper would
come from GitHub. The user's PATH as stored (``HKCU\\Environment``, read only) is compared before and after: a run
that changed it exits 1. Output passes through; the exit status is the script's.

Exit status 2 and ``gate_run: refused: <why>`` when a run is refused (nothing is started); 124 on a timeout.
"""
from __future__ import annotations

import argparse
import ipaddress
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

#: The state variables a server runs with, each a folder under ``--state`` (smoke_wheel.py's).
STATE_VARIABLES = {"CITAR_STATE_DIR": ".", "CITAR_DATA_DIR": "data", "CITAR_SAVE_DIR": "saves",
                   "CITAR_CONFIG_DIR": "config", "CITAR_BENCH_DIR": "benchmarks"}
#: The owner's own server's port on this laptop: never used by a gate.
OWNER_PORT = 8799
LOOPBACK_HOST = "127.0.0.1"
#: keyring's backend that stores nothing (``citar/keystore.py`` reads it as no OS credential store).
NO_KEYRING = "keyring.backends.fail.Keyring"
#: Where a server's encrypted API key file is kept, under ``--state`` (``citar/keystore.py``'s ``CITAR_KEYS_FILE``).
KEYS_FILE = Path("config") / "keys.enc"
#: The per-user bases a server's defaults fall back on, each pointed under ``<state>/user``.
USER_BASES = {"APPDATA": "Roaming", "LOCALAPPDATA": "Local"} if os.name == "nt" else (
    {} if sys.platform == "darwin" else {"XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "share"})
#: What a server's ``--env`` may never set: the isolation above would be undone.
FIXED = {"PYTHON_KEYRING_BACKEND", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME"}
#: The helper's options this reads, which must be written in full (the helper takes unambiguous prefixes).
WATCHED = ("--home", "--server", "--base-url", "--config", "--hardware", "--ca-file", "--detach", "--supervisor")
#: The helper's options that name a file it reads, each inside ``--under``.
FILE_OPTIONS = ("--config", "--hardware", "--ca-file")
#: The helper's options that take a value (``--name VALUE``), so their value is not read as a command.
TAKES_VALUE = {"--home", "--server", "--token", "--config", "--base-url", "--provider", "--api-key-env",
               "--max-concurrent", "--name", "--model", "--quiet", "--ca-file", "--hardware", "--updates",
               "--update-key", "--test-fault"}
#: install.ps1's parameters as it declares them, each a switch (False) or one taking a value (True).
PS1_DECLARED = {"-Worker": False, "-Version": True, "-Prefix": True, "-BinDir": True, "-NoPath": False,
                "-From": True, "-Python": True, "-NoSetup": False, "-Uninstall": False}
#: The same by lower-cased name (PowerShell reads a parameter's name in any case), and each name as declared.
PS1_PARAMETERS = {name.lower(): value for name, value in PS1_DECLARED.items()}
PS1_SPELLING = {name.lower(): name for name in PS1_DECLARED}
#: What the script-block run prints once the script block has ended and its PowerShell goes on.
SESSION_CONTINUES = "gate_run: the session continues"
#: The variables an install.ps1 run may be given: where -Worker downloads from, and the script's test hook.
PS1_ENV = ("CITAR_RELEASE_URL", "CITAR_INSTALL_TEST_FACTS")
INSTALL_PS1 = ROOT / "install.ps1"
#: How long a stopped server's tree may take to end by itself before it is killed.
GRACE = 10.0
EXIT_REFUSED, EXIT_TIMEOUT = 2, 124


class Refused(Exception):
    """A run this harness will not start."""


# ------------------------------------------------------------------------------------------------ places


def real(p: Path | str) -> Path:
    return Path(os.path.realpath(os.path.abspath(str(p))))


def within(inner: Path, outer: Path) -> bool:
    """Whether ``inner`` is ``outer`` or inside it, both resolved (case-folded on Windows)."""
    a, b = os.path.normcase(str(real(inner))), os.path.normcase(str(real(outer)))
    return a == b or a.startswith(b.rstrip("\\/") + os.sep)


def platform_base(env: dict[str, str] | None = None) -> Path | None:
    """The per-user CITAR folder a release helper and an installed CITAR use (``citar-helper``'s ``home.rs``,
    ``citar/paths.py``)."""
    env = os.environ if env is None else env
    if os.name == "nt":
        local = env.get("LOCALAPPDATA") or (env.get("USERPROFILE") and str(Path(env["USERPROFILE"]) / "AppData"
                                                                          / "Local"))
        return Path(local) / "CITAR" if local else None
    home = env.get("HOME")
    if sys.platform == "darwin":
        return Path(home) / "Library" / "Application Support" / "CITAR" if home else None
    data = env.get("XDG_DATA_HOME") or (home and str(Path(home) / ".local" / "share"))
    return Path(data) / "citar" if data else None


def check_under(under: Path | None) -> Path:
    """``--under``, checked: an existing folder apart from this checkout and the per-user CITAR folder."""
    if under is None:
        raise Refused("give --under DIR, the package's target folder (or set CARGO_TARGET_DIR)")
    if not under.is_dir():
        raise Refused(f"--under {under} is not a folder")
    under = real(under)
    if under.parent == under or under == real(Path.home()):
        raise Refused(f"--under {under} is a drive's root or the home folder: name the package's target folder")
    if within(under, ROOT) or within(ROOT, under):
        raise Refused(f"--under {under} overlaps this checkout ({ROOT}): nothing a gate runs may write there")
    base = platform_base()
    if base is not None and (within(under, base) or within(base, under)):
        raise Refused(f"--under {under} overlaps the per-user CITAR folder {base}, a real installation's")
    return under


def inside(path: Path, under: Path, what: str) -> Path:
    """``path`` (relative to the working directory) resolved, strictly inside ``under``."""
    resolved = real(path)
    if not within(resolved, under) or os.path.normcase(str(resolved)) == os.path.normcase(str(under)):
        raise Refused(f"{what} {resolved} is not inside {under}")
    return resolved


def is_loopback(url: str) -> bool:
    """Whether a server or endpoint address names this machine only (``localhost``, ``127.0.0.0/8``, ``::1``)."""
    text = url.strip()
    if "://" not in text:
        text = "http://" + text
    try:
        host = urllib.parse.urlsplit(text).hostname
    except ValueError:
        return False
    if not host:
        return False
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


#: The documentation networks (RFC 5737): never routed, so an address in them reaches no machine.
DOCUMENTATION = tuple(ipaddress.ip_network(n) for n in ("192.0.2.0/24", "198.51.100.0/24", "203.0.113.0/24"))


def is_documentation(url: str) -> bool:
    """Whether a URL's host is a documentation address (a test of a refusal names one: nothing answers it)."""
    try:
        host = urllib.parse.urlsplit(url.strip()).hostname
        return bool(host) and any(ipaddress.ip_address(host) in net for net in DOCUMENTATION)
    except ValueError:
        return False


# ------------------------------------------------------------------------------------------------ the helper


def option_values(args: list[str], name: str) -> list[str]:
    """Every value given to ``name`` (``--name VALUE`` or ``--name=VALUE``)."""
    out = []
    for i, word in enumerate(args):
        if word == name:
            if i + 1 >= len(args):
                raise Refused(f"{name} needs a value")
            out.append(args[i + 1])
        elif word.startswith(name + "="):
            out.append(word[len(name) + 1:])
    return out


def bare_words(args: list[str]) -> list[str]:
    """The words that are not options or their values: the command and its arguments."""
    out, skip = [], False
    for word in args:
        if skip:
            skip = False
            continue
        if word.startswith("-"):
            skip = word in TAKES_VALUE
            continue
        out.append(word)
    return out


def check_helper(args: list[str], under: Path, binary: Path | str | None = None) -> Path:
    """The helper's home from its arguments (and ``binary``, the program run), every rule above checked; raises
    Refused."""
    for word in args:
        if not word.startswith("--") or word == "--":
            continue
        name = word.split("=", 1)[0]
        for full in WATCHED:
            if name != full and full.startswith(name) and len(name) > 2:
                raise Refused(f"{word} abbreviates {full}: write it in full, so this harness reads it")
    homes = option_values(args, "--home")
    if len(homes) != 1:
        raise Refused("a release helper runs only with --home DIR, given once: without it, its home is the per-user "
                      "CITAR folder, a real installation's")
    home = inside(Path(homes[0]), under, "--home")
    for name in ("--detach", "--supervisor"):
        if name in args:
            raise Refused(f"{name} starts a process that outlives the run")
    words = bare_words(args)
    if "service" in words and "--dry-run" not in args and {"install", "uninstall", "run"} & set(words):
        raise Refused("service install, uninstall and run change this machine: only --dry-run runs here")
    # read wherever the command stands among the bare words, as service above (a launcher's script may come first)
    for at, word in enumerate(words):
        if word == "scan" and at + 1 < len(words):  # scan FILE writes its report there
            inside(Path(words[at + 1]), under, "scan's FILE")
        if word == "update" and words[at + 1:at + 2] in (["apply"], ["rollback"]):
            # each puts a binary in BINARY's place and keeps the one it replaced beside it
            command = f"update {words[at + 1]}"
            if binary is None:
                raise Refused(f"{command} replaces the binary it runs: give it, to check it is inside {under}")
            inside(Path(binary), under, f"the binary {command} replaces,")
    for name in ("--server", "--base-url"):
        for value in option_values(args, name):
            if not is_loopback(value):
                raise Refused(f"{name} {value} is not a loopback address: a test helper never reaches another "
                              f"machine")
    for name in FILE_OPTIONS:
        for value in option_values(args, name):
            inside(Path(value), under, name)
    settings = home / "helper.json"
    for config in [settings, *(Path(v) for v in option_values(args, "--config"))]:
        if not config.is_file():
            continue
        try:
            data = json.loads(config.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            raise Refused(f"{config} does not read ({e})") from e
        for key in ("server_url", "base_url"):
            value = data.get(key) if isinstance(data, dict) else None
            if isinstance(value, str) and value and not is_loopback(value):
                raise Refused(f"{config}'s {key} {value} is not a loopback address")
    return home


def helper_env(home: Path, under: Path) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
    env["CITAR_DATA_DIR"] = str(home)
    base = under / "gate-run-base"
    if os.name == "nt":
        env["LOCALAPPDATA"] = str(base / "Local")
    elif sys.platform != "darwin":
        env["XDG_DATA_HOME"] = str(base / "share")
    return env


def run_helper(binary: Path | str, args: list[str], under: Path, timeout: float = 300.0,
               capture: bool = True) -> subprocess.CompletedProcess:
    """Runs ``binary ARGS`` under the rules above, its tree killed at ``timeout`` (``returncode`` 124 then)."""
    under = check_under(under)
    home = check_helper(list(args), under, binary)
    argv = [str(binary), *args]
    proc = start(argv, helper_env(home, under), capture=capture)
    try:
        out, err = proc.communicate(timeout=timeout)
        return subprocess.CompletedProcess(argv, proc.returncode, out, err)
    except subprocess.TimeoutExpired:
        stop_tree(proc, snapshot_tree(proc.pid), polite=False)
        try:
            out, err = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            out, err = None, None
        message = f"gate_run: {binary} did not end within {timeout:.0f} s; its processes were killed\n"
        return subprocess.CompletedProcess(argv, EXIT_TIMEOUT, out, (err or "") + message if capture else message)
    except BaseException:  # an interrupt, or anything else: the helper is never left running
        stop_tree(proc, snapshot_tree(proc.pid), polite=False)
        raise


# ------------------------------------------------------------------------------------------------ install.ps1


#: A path with its drive letter, as Windows writes one in full.
DRIVE_PATH = re.compile(r"[A-Za-z]:[\\/]")


def ps1_folder(value: str, under: Path, what: str) -> Path:
    """-Prefix's or -BinDir's DIR resolved, strictly inside ``under``; refused unless PowerShell reads the same folder
    from it. install.ps1 resolves the text itself (``GetUnresolvedProviderPathFromPSPath``), which expands a leading
    ``~`` to the real home folder and takes a provider's path (``FileSystem::C:\\x``) or a drive of PowerShell's own
    (``Temp:\\x``), none of which Python reads alike: so DIR is absolute (with its drive letter on Windows) and has no
    other ``:``. The run then passes the folder resolved here, never the text."""
    if value.startswith("~"):
        raise Refused(f"{what} {value}: PowerShell reads a leading ~ as the home folder; give the folder in full")
    if ":" in (value[2:] if re.match(r"[A-Za-z]:", value) else value):
        raise Refused(f"{what} {value}: PowerShell reads a provider's path (::) or a drive of its own (Temp:, Env:) "
                      f"in its own way; give the folder in full")
    if not (DRIVE_PATH.match(value) if os.name == "nt" else value.startswith("/")):
        raise Refused(f"{what} {value} is not an absolute path{' with its drive letter' if os.name == 'nt' else ''}")
    return inside(Path(value), under, what)


def check_install_ps1(args: list[str], under: Path, extra: list[str]) -> dict[str, str | bool]:
    """install.ps1's parameters from ARGS (lower-cased names; a switch is True; -Prefix and -BinDir the folders
    resolved), every rule above checked; raises Refused."""
    given: dict[str, str | bool] = {}
    i = 0
    while i < len(args):
        word = args[i]
        name, sep, inline = word.partition(":")
        key = name.lower()
        if not word.startswith("-") or word == "-":
            raise Refused(f"{word!r} is not a parameter of install.ps1 (it takes no positional argument)")
        if key not in PS1_PARAMETERS:
            full = [p for p in PS1_PARAMETERS if p.startswith(key)]
            hint = f": write {full[0]} in full, so this harness reads it" if len(full) == 1 else ""
            raise Refused(f"{name} is not a parameter of install.ps1{hint}")
        if key in given:
            raise Refused(f"{name} is given twice")
        if PS1_PARAMETERS[key]:
            if sep:
                value = inline
            elif i + 1 < len(args):
                i += 1
                value = args[i]
            else:
                raise Refused(f"{name} needs a value")
            if not value:
                raise Refused(f"{name} needs a value")
            given[key] = value
        else:
            if sep:
                raise Refused(f"{word}: a switch is given alone here ({name}:$false would turn it off)")
            given[key] = True
        i += 1
    for key, what in (("-prefix", "-Prefix"), ("-bindir", "-BinDir")):
        if key not in given:
            raise Refused(f"install.ps1 runs here only with {what} DIR inside {under}: without it, it would use the "
                          f"per-user CITAR folder, a real installation's")
        given[key] = str(ps1_folder(str(given[key]), under, what))
    if "-nopath" not in given:
        raise Refused("install.ps1 runs here only with -NoPath: without it, it writes the user's PATH (the registry)")
    installs = "-uninstall" not in given and "-worker" not in given
    if installs and "-nosetup" not in given:
        raise Refused("an install runs here only with -NoSetup: the setup wizard would wait for answers")
    names = []
    for item in extra:
        name, sep, value = item.partition("=")
        if not sep or name.upper() not in PS1_ENV:
            raise Refused(f"--env {item}: install.ps1 is given only {' and '.join(PS1_ENV)}")
        if name.upper() == "CITAR_RELEASE_URL":
            scheme = urllib.parse.urlsplit(value).scheme.lower()
            if scheme not in ("http", "https") or not (is_loopback(value) or is_documentation(value)):
                raise Refused(f"--env {item}: a test's downloads come from a loopback server (http://127.0.0.1:PORT); "
                              f"a refusal's test may name a documentation address (192.0.2.1), which no machine has")
        names.append(name.upper())
    if "-worker" in given and "-uninstall" not in given and "CITAR_RELEASE_URL" not in names:
        raise Refused("-Worker downloads the helper: give --env CITAR_RELEASE_URL=http://127.0.0.1:PORT (a loopback "
                      "server), or it would come from GitHub")
    return given


def install_ps1_env(under: Path, extra: list[str]) -> dict[str, str]:
    """The environment install.ps1 runs with (the module's doc)."""
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
    base = under / "gate-run-base"
    for name, sub in (("LOCALAPPDATA", "Local"), ("APPDATA", "Roaming"), ("TEMP", "Temp"), ("TMP", "Temp")):
        folder = base / sub
        folder.mkdir(parents=True, exist_ok=True)
        env[name] = str(folder)
    for name, sub in STATE_VARIABLES.items():
        folder = base / "state" / sub
        folder.mkdir(parents=True, exist_ok=True)
        env[name] = str(folder)
    env["CITAR_KEYS_FILE"] = str(base / "state" / KEYS_FILE)
    env["PYTHON_KEYRING_BACKEND"] = NO_KEYRING
    for item in extra:
        name, _, value = item.partition("=")
        env[name.upper()] = value
    return env


def user_path() -> tuple[str, int] | None:
    """The user's PATH as stored (``HKCU\\Environment``'s ``Path``, read only, unexpanded) and its kind, or None."""
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_READ) as key:
            value, kind = winreg.QueryValueEx(key, "Path")
    except FileNotFoundError:
        return None
    return str(value), int(kind)


def fingerprint(path: tuple[str, int] | None) -> str:
    """A short digest of the PATH (its value is the user's own, never printed)."""
    import hashlib
    text = "<none>" if path is None else f"{path[1]}:{path[0]}"
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def ps1_arguments(given: dict[str, str | bool]) -> list[str]:
    """install.ps1's arguments rebuilt from what ``check_install_ps1`` read: each name as the script declares it, and
    -Prefix and -BinDir the folders it resolved and checked, so PowerShell resolves nothing of its own."""
    out: list[str] = []
    for key, value in given.items():
        out.append(PS1_SPELLING[key])
        if value is not True:
            out.append(str(value))
    return out


#: The quotes PowerShell reads as a single quote: the ASCII one and four typographic ones.
PS_QUOTES = "'‘’‚‛"


def ps1_literal(text: str) -> str:
    """``text`` as a PowerShell single-quoted string (nothing in it expanded), each quote doubled."""
    return "'" + "".join(c * 2 if c in PS_QUOTES else c for c in text) + "'"


def scriptblock_command(given: dict[str, str | bool]) -> str:
    """The PowerShell a user runs as the documented one-liner, ``& ([scriptblock]::Create((iwr -useb URL))) ARGS``,
    with this checkout's install.ps1 for the download; then, once the script block has ended, ``SESSION_CONTINUES``
    and its ``$LASTEXITCODE`` as the exit status. The script's ``exit`` would end this PowerShell before that line,
    as it would close the user's window. An error the script block lets out (shown to the user as PowerShell shows
    one) is named, and the status is then 1 at least."""
    words: list[str] = []
    for key, value in given.items():
        words.append(PS1_SPELLING[key])
        if value is not True:
            words.append(ps1_literal(str(value)))
    call = f"& ([scriptblock]::Create([IO.File]::ReadAllText({ps1_literal(str(INSTALL_PS1))}))) {' '.join(words)}"
    return "\n".join([
        "$global:LASTEXITCODE = 0",
        "$escaped = $null",
        f"try {{ {call} }} catch {{ $escaped = $_ }}",
        "$code = $global:LASTEXITCODE",
        "if ($escaped) {",
        '    Write-Host "gate_run: an error left the script block: $escaped"',
        "    if (-not $code) { $code = 1 }",
        "}",
        f'Write-Host "{SESSION_CONTINUES} (install.ps1 as a script block, status $code)"',
        "exit $code",
    ])


def run_install_ps1(args: list[str], under: Path, extra: list[str], timeout: float = 1800.0,
                    shell: str = "powershell", capture: bool = False,
                    as_scriptblock: bool = False) -> subprocess.CompletedProcess:
    """Runs install.ps1 ARGS under the rules above, its tree killed at ``timeout`` (``returncode`` 124 then); a run
    that changed the user's PATH returns 1. PowerShell is given the parameters as they were checked (``ps1_arguments``):
    with ``-File``, or with ``as_scriptblock`` as the documented one-liner runs it (``scriptblock_command``)."""
    under = check_under(under)
    given = check_install_ps1(list(args), under, list(extra))
    if os.name != "nt":
        raise Refused("install.ps1 runs on Windows")
    exe = shutil.which(shell)
    if shell not in ("powershell", "pwsh") or exe is None:
        raise Refused(f"no {shell} here (give --shell powershell or pwsh)")
    argv = [exe, "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]
    if as_scriptblock:
        # -Command, not -EncodedCommand: Windows PowerShell answers the latter with its output as CLIXML on stderr
        argv += ["-Command", scriptblock_command(given)]
    else:
        argv += ["-File", str(INSTALL_PS1), *ps1_arguments(given)]
    before = user_path()
    proc = start(argv, install_ps1_env(under, list(extra)), capture=capture)
    try:
        try:
            out, err = proc.communicate(timeout=timeout)
            code = proc.returncode
        except subprocess.TimeoutExpired:
            stop_tree(proc, snapshot_tree(proc.pid), polite=False)
            out, err, code = None, None, EXIT_TIMEOUT
            print(f"gate_run: install.ps1 did not end within {timeout:.0f} s; its processes were killed", flush=True)
        except BaseException:  # an interrupt, or anything else: the script is never left running
            stop_tree(proc, snapshot_tree(proc.pid), polite=False)
            raise
    finally:
        after = user_path()
        if after != before:
            print(f"gate_run: the user's PATH changed during the run ({fingerprint(before)} -> "
                  f"{fingerprint(after)})", file=sys.stderr, flush=True)
    if after != before:
        code = code or 1
    return subprocess.CompletedProcess(argv, code, out, err)


# ------------------------------------------------------------------------------------------------ processes


def start(argv: list[str], env: dict[str, str], capture: bool = False) -> subprocess.Popen:
    """A process of its own group, so its tree can be signalled and found."""
    kwargs: dict = {"env": env}
    if os.name == "nt":
        kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        kwargs["start_new_session"] = True
    if capture:
        kwargs.update(stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", errors="replace")
    try:
        return subprocess.Popen(argv, **kwargs)
    except OSError as e:
        raise Refused(f"cannot start {argv[0]}: {e}") from e


def windows_process_table() -> dict[int, tuple[int, int]]:
    """Every process on Windows, from a Toolhelp snapshot (kernel32, read only): its id, its parent's, and its
    creation time where this user may read it (else 0)."""
    import ctypes
    from ctypes import wintypes

    class Entry(ctypes.Structure):  # PROCESSENTRY32W
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD), ("th32ProcessID", wintypes.DWORD),
                    ("th32DefaultHeapID", ctypes.c_size_t), ("th32ModuleID", wintypes.DWORD),
                    ("cntThreads", wintypes.DWORD), ("th32ParentProcessID", wintypes.DWORD),
                    ("pcPriClassBase", ctypes.c_long), ("dwFlags", wintypes.DWORD),
                    ("szExeFile", ctypes.c_wchar * 260)]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot_all, query_limited = 0x2, 0x1000  # TH32CS_SNAPPROCESS, PROCESS_QUERY_LIMITED_INFORMATION

    def created(pid: int) -> int:
        handle = kernel32.OpenProcess(query_limited, False, pid)
        if not handle:
            return 0
        try:
            times = [wintypes.FILETIME() for _ in range(4)]
            if not kernel32.GetProcessTimes(handle, *[ctypes.byref(t) for t in times]):
                return 0
            return (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        finally:
            kernel32.CloseHandle(handle)

    snap = kernel32.CreateToolhelp32Snapshot(snapshot_all, 0)
    if not snap or snap == wintypes.HANDLE(-1).value:
        raise OSError(ctypes.get_last_error(), "CreateToolhelp32Snapshot failed")
    table: dict[int, tuple[int, int]] = {}
    try:
        entry = Entry()
        entry.dwSize = ctypes.sizeof(Entry)
        more = kernel32.Process32FirstW(snap, ctypes.byref(entry))
        while more:
            table[int(entry.th32ProcessID)] = (int(entry.th32ParentProcessID), created(entry.th32ProcessID))
            more = kernel32.Process32NextW(snap, ctypes.byref(entry))
    finally:
        kernel32.CloseHandle(snap)
    return table


def linux_process_table() -> dict[int, tuple[int, int]]:
    """Every process on Linux, from ``/proc``: its id, its parent's, and its start in clock ticks since boot
    (``/proc/<pid>/stat``'s fields 4 and 22, counted after the command's closing parenthesis, since the command may
    hold spaces and parentheses itself)."""
    table: dict[int, tuple[int, int]] = {}
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            with open(os.path.join(entry.path, "stat"), encoding="utf-8", errors="replace") as f:
                text = f.read()
        except OSError:  # it ended while the table was read
            continue
        fields = text[text.rfind(")") + 1:].split()  # fields[0] is field 3, the state
        if len(fields) >= 20 and fields[1].isdigit() and fields[19].isdigit():
            table[int(entry.name)] = (int(fields[1]), int(fields[19]))
    return table


def ps_process_table() -> dict[int, tuple[int, int]]:
    """Every process on macOS (or another Unix without ``/proc``), from ``ps``: its id, its parent's, and its start
    (``lstart``, read in the C locale, as seconds since the epoch)."""
    table: dict[int, tuple[int, int]] = {}
    out = subprocess.run(["ps", "-A", "-o", "pid=", "-o", "ppid=", "-o", "lstart="], capture_output=True, text=True,
                         timeout=60, env={**os.environ, "LC_ALL": "C"}).stdout
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[0].isdigit() and parts[1].isdigit():
            try:
                began = int(time.mktime(time.strptime(parts[2].strip(), "%a %b %d %H:%M:%S %Y")))
            except ValueError:
                began = 0
            table[int(parts[0])] = (int(parts[1]), began)
    return table


def process_table() -> dict[int, tuple[int, int]]:
    """Every process: its id, then its parent's and its start (to tell a reused id from a child): the start is a
    process's creation time on Windows, its clock ticks since boot on Linux, and its second on macOS."""
    if os.name == "nt":
        return windows_process_table()
    if os.path.isfile("/proc/self/stat"):
        return linux_process_table()
    return ps_process_table()


def snapshot_tree(root: int) -> dict[int, int]:
    """``root`` and every process descended from it, each with its start: taken before a stop, since on Unix an
    orphan is given another parent."""
    table = process_table()
    found = {root: table.get(root, (0, 0))[1]}
    frontier = [root]
    while frontier:
        parent = frontier.pop()
        for pid, (ppid, began) in table.items():
            if ppid == parent and pid not in found and began >= found[parent]:
                found[pid] = began
                frontier.append(pid)
    return found


def left_of(tree: dict[int, int]) -> list[int]:
    """The processes of ``tree`` still running (the same id and start)."""
    table = process_table()
    return sorted(pid for pid, began in tree.items() if pid in table and table[pid][1] == began)


def stop_tree(proc: subprocess.Popen, tree: dict[int, int], polite: bool = True) -> list[int]:
    """Stops ``proc`` and the processes of ``tree``: a polite signal to its group first, then a kill of whatever
    is left; returns what is still running afterwards."""
    if polite and proc.poll() is None:
        try:
            if os.name == "nt":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                os.killpg(proc.pid, signal.SIGTERM)
        except OSError:
            pass
        try:
            proc.wait(timeout=GRACE)
        except subprocess.TimeoutExpired:
            pass
    for pid in left_of(tree):
        try:
            if os.name == "nt":
                os.kill(pid, signal.SIGTERM)  # TerminateProcess on Windows
            else:
                os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    try:
        proc.wait(timeout=GRACE)
    except subprocess.TimeoutExpired:
        pass
    deadline = time.monotonic() + GRACE
    left = left_of(tree)
    while left and time.monotonic() < deadline:
        time.sleep(0.2)
        left = left_of(tree)
    return left


# ------------------------------------------------------------------------------------------------ the server


def port_free(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        try:
            s.bind((LOOPBACK_HOST, port))
        except OSError:
            return False
    return True


def port_open(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1.0)
        return s.connect_ex((LOOPBACK_HOST, port)) == 0


def check_db_url(item: str, value: str, under: Path) -> None:
    """A ``CITAR_DB_URL`` a gate's server may be given: a SQLite database inside ``under``, or one in memory."""
    scheme, sep, rest = value.partition("://")
    if not sep or scheme.split("+", 1)[0].lower() != "sqlite":
        raise Refused(f"--env {item}: a gate's server keeps its own SQLite database, under {under}")
    path, _, query = rest.partition("?")
    if "uri" in query.lower():
        raise Refused(f"--env {item}: give the database as sqlite:///PATH, not a URI this cannot read")
    if path in ("", "/", "/:memory:"):  # in memory: nothing is written
        return
    if not path.startswith("/"):
        raise Refused(f"--env {item}: give the database as sqlite:///PATH")
    inside(Path(path[1:]), under, f"--env {item}'s database")


def check_server(port: int, state: Path, under: Path, extra: list[str], command: list[str]) -> dict[str, str]:
    """The server's environment, every rule above checked; raises Refused."""
    if port == OWNER_PORT:
        raise Refused(f"port {OWNER_PORT} is the owner's server's: give another")
    if not 1024 < port < 65536:
        raise Refused(f"--port {port} is not a port above 1024")
    state = inside(state, under, "--state")
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
    env["CITAR_KEYS_FILE"] = str(state / KEYS_FILE)
    for item in extra:
        name, sep, value = item.partition("=")
        if not sep or not name:
            raise Refused(f"--env {item} is not NAME=VALUE")
        if name.upper() == "CITAR_HOST" and value != LOOPBACK_HOST:
            raise Refused(f"--env {item}: a server here binds {LOOPBACK_HOST} only")
        if name.upper() == "CITAR_PORT" and value != str(port):
            raise Refused(f"--env {item}: the port is --port's, {port}")
        if name.upper() in STATE_VARIABLES:
            raise Refused(f"--env {item}: the state variables are --state's")
        if name.upper() in FIXED:
            raise Refused(f"--env {item}: the key store and the per-user bases are --state's (no OS credential "
                          f"store, no per-user folder of this machine)")
        if name.upper() == "CITAR_KEYS_FILE":
            inside(Path(value), under, f"--env {item}'s key file")
        if name.upper() == "CITAR_DB_URL":
            check_db_url(item, value, under)
        env[name] = value
    for value in option_values(command, "--host"):
        if value != LOOPBACK_HOST:
            raise Refused(f"the command's --host {value}: a server here binds {LOOPBACK_HOST} only")
    for value in option_values(command, "--port"):
        if value != str(port):
            raise Refused(f"the command's --port {value} is not --port's {port}")
    if not port_free(port):
        raise Refused(f"port {port} is in use on {LOOPBACK_HOST}")
    env["CITAR_HOST"] = LOOPBACK_HOST
    env["CITAR_PORT"] = str(port)
    env["PYTHON_KEYRING_BACKEND"] = NO_KEYRING
    for name, sub in STATE_VARIABLES.items():
        folder = state / sub
        folder.mkdir(parents=True, exist_ok=True)
        env[name] = str(folder)
    for name, sub in USER_BASES.items():
        folder = state / "user" / sub
        folder.mkdir(parents=True, exist_ok=True)
        env[name] = str(folder)
    return env


def answers(url: str) -> bool:
    """Whether ``url`` gives any HTTP answer."""
    try:
        with urllib.request.urlopen(url, timeout=2):
            return True
    except urllib.error.HTTPError:
        return True
    except (urllib.error.URLError, OSError, ValueError):
        return False


def check_ready(path: str | None) -> None:
    """``--ready`` is a URL's path: ``/`` first, no space or control character. Git Bash turns ``/api/x`` into
    ``C:/Program Files/Git/api/x`` unless ``MSYS_NO_PATHCONV=1`` is set, which this names."""
    if path is None:
        return
    if not path.startswith("/") or any(c.isspace() or ord(c) < 32 for c in path):
        raise Refused(f"--ready {path!r} is not a URL's path such as /api/auth/config (in Git Bash, set "
                      f"MSYS_NO_PATHCONV=1, or it rewrites a leading /)")


def serve(command: list[str], port: int, state: Path, under: Path, extra: list[str], ready: str | None,
          stop_file: Path | None, timeout: float) -> int:
    under = check_under(under)
    check_ready(ready)
    env = check_server(port, state, under, extra, command)
    state = real(state)
    stop_file = inside(stop_file, under, "--stop-file") if stop_file else state / "gate_run.stop"
    if stop_file.exists():
        raise Refused(f"the stop file {stop_file} is already there: remove it first")
    url = f"http://{LOOPBACK_HOST}:{port}{ready or ''}"
    print(f"gate_run: server on {LOOPBACK_HOST}:{port}, state in {state}, stop with {stop_file}: "
          f"{' '.join(command)}", flush=True)
    proc = start(command, env)
    began = time.monotonic()
    said_ready, reason, code = ready is None, "", 0
    try:
        while True:
            if proc.poll() is not None:
                reason, code = f"the server exited by itself (status {proc.returncode})", proc.returncode or 1
                break
            if stop_file.exists():
                reason = "stopped on request"
                break
            if time.monotonic() - began > timeout:
                reason, code = f"stopped at the timeout ({timeout:.0f} s)", EXIT_TIMEOUT
                break
            if not said_ready and answers(url):
                said_ready = True
                print(f"gate_run: ready {url} after {time.monotonic() - began:.1f} s", flush=True)
            time.sleep(0.2)
    except KeyboardInterrupt:
        reason, code = "interrupted", 130
    except Exception as e:  # whatever went wrong here, the server is stopped below, never left behind
        reason, code = f"gate_run failed while it waited ({type(e).__name__}: {e})", 1
    tree = snapshot_tree(proc.pid)
    left = stop_tree(proc, tree)
    if left:
        print(f"gate_run: processes left after the stop: {left}", flush=True)
        code = code or 1
    if port_open(port):
        print(f"gate_run: port {port} still answers after the stop", flush=True)
        code = code or 1
    try:
        stop_file.unlink()
    except OSError:
        pass
    print(f"gate_run: {reason}; {len(tree)} process(es) stopped, none left" if not left else f"gate_run: {reason}",
          flush=True)
    return code


# ------------------------------------------------------------------------------------------------ command line


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    rest: list[str] = []
    if "--" in argv:
        at = argv.index("--")
        argv, rest = argv[:at], argv[at + 1:]
    ap = argparse.ArgumentParser(prog="gate_run.py", description=__doc__.split("\n\n", 1)[0])
    sub = ap.add_subparsers(dest="mode", required=True)
    for mode in ("helper", "server", "install-ps1"):
        p = sub.add_parser(mode, allow_abbrev=False)
        p.add_argument("--under", type=Path, default=None)
        p.add_argument("--timeout", type=float, default=300.0 if mode == "helper" else 1800.0)
        if mode == "server":
            p.add_argument("--port", type=int, required=True)
            p.add_argument("--state", type=Path, required=True)
            p.add_argument("--ready", default=None, metavar="PATH")
            p.add_argument("--stop-file", type=Path, default=None)
        if mode in ("server", "install-ps1"):
            p.add_argument("--env", action="append", default=[], metavar="NAME=VALUE")
        if mode == "install-ps1":
            p.add_argument("--shell", choices=("powershell", "pwsh"), default="powershell")
            p.add_argument("--as-scriptblock", action="store_true")
    args = ap.parse_args(argv)
    under = args.under or (Path(os.environ["CARGO_TARGET_DIR"]) if os.environ.get("CARGO_TARGET_DIR") else None)
    try:
        if not rest and args.mode != "install-ps1":
            raise Refused(f"give the command after --: gate_run.py {args.mode} ... -- COMMAND ARGS...")
        if args.mode == "helper":
            print(f"gate_run: helper {' '.join(rest)}", flush=True)
            done = run_helper(rest[0], rest[1:], under, timeout=args.timeout, capture=False)
            if done.returncode == EXIT_TIMEOUT and done.stderr:
                print(done.stderr.strip(), file=sys.stderr)
            return done.returncode
        if args.mode == "install-ps1":
            how = " (as a script block)" if args.as_scriptblock else ""
            print(f"gate_run: install.ps1{how} {' '.join(rest)}", flush=True)
            return run_install_ps1(rest, under, args.env, timeout=args.timeout, shell=args.shell,
                                   as_scriptblock=args.as_scriptblock).returncode
        return serve(rest, args.port, args.state, under, args.env, args.ready, args.stop_file, args.timeout)
    except Refused as e:
        print(f"gate_run: refused: {e}", file=sys.stderr)
        return EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
