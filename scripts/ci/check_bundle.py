"""The Windows bundle's checks (design/phase5-release.md P5.6, package 5-05): what package.yml's ``windows`` job runs on
the bundle it built, on the installer's silent install, and on the portable zip, and what the package's local gate
runs on the same bundle.

    python scripts/ci/check_bundle.py bundle --dir DIST/CITAR --version V --label L --work DIR [--python VENV-PY]
                                             [--out REPORT.json]
    python scripts/ci/check_bundle.py installed --dir INSTALLED --version V --work DIR [--iss installer/citar.iss]
    python scripts/ci/check_bundle.py uninstalled --dir INSTALLED [--iss installer/citar.iss] [--wait S]
    python scripts/ci/check_bundle.py zip --dir DIST/CITAR --version V --out OUTDIR
    python scripts/ci/check_bundle.py sums FILE...

**bundle:** the folder PyInstaller made from the wheel (``installer/citar.spec``). ``citar.exe --version`` says
``CITAR V``; ``where``, ``doctor --quiet`` and ``doctor`` exit 0, every state folder they name under ``--work``; the
doctor's engine line names the build's label and V, and its jobs line says the job host it started is verifiable:
the lab's job host started **from the bundle**, as ``citar.exe jobs-host`` (``citar/jobs/host.py``), the windowed
``citar-play.exe``'s too; the hidden ``jobs-host`` takes no argument. Exactly one ``citar/_engine.pyd``. At the
root, beside the executables: ``LICENSE``, ``NOTICE.md`` and ``THIRD-PARTY-LICENSES.txt``, byte for byte the
checkout's, ``README.md``, and ``PYTHON-LICENSES.txt``, whose index names CPython and PyInstaller's bootloader and no
build tool (PyInstaller, pillow, pip, setuptools); with ``--python`` (the venv the bundle was built in) it must be
byte for byte what ``installer/python_licenses.py`` writes there, the installed closure of ``citar[all]``.

**installed** (after ``CITAR-V-setup.exe /VERYSILENT ... /DIR=INSTALLED``): the installed ``citar.exe`` says ``CITAR V``
and ``doctor --quiet`` exits 0; the per-user uninstall key ``{AppId}_is1`` (``installer/citar.iss``'s AppId) is under
``HKCU`` (read only), its ``DisplayVersion`` V and its ``InstallLocation`` INSTALLED. **uninstalled** (after
``unins000.exe /VERYSILENT``, which hands itself to a copy in the temporary folder and returns at once): within
``--wait`` seconds the key is gone and INSTALLED holds no file.

**zip:** ``CITAR-V-windows-x64.zip`` in OUTDIR, the bundle under ``CITAR/`` (Scoop's portable package), its entries
in name order with one date, and its ``.sha256``. **sums:** a ``.sha256`` beside each FILE (its hex digest and a
newline, as ``scripts/ci/helper_release.py`` writes them).

Every run of the bundle's programs has every ``CITAR_*`` variable of this environment dropped, and its state, the API
key file and the per-user bases under ``--work`` (as ``scripts/ci/gate_run.py server`` gives a server), so nothing
reaches a real installation (P5.18.1 rule 2). Exit 0 when every check holds, 1 on the first that does not, 2 on a usage
error.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[2]
#: The notices at the bundle's root that are the checkout's, byte for byte (the wheel carries the same files).
SAME_AS_CHECKOUT = {"LICENSE": "LICENSE", "NOTICE.md": "NOTICE.md",
                    "THIRD-PARTY-LICENSES.txt": "packaging/THIRD-PARTY-LICENSES.txt"}
NOTICES = (*SAME_AS_CHECKOUT, "README.md", "PYTHON-LICENSES.txt")
#: Distributions the bundle's venv holds to build it, which it does not ship: never in PYTHON-LICENSES.txt's index.
#: (PyInstaller's pywin32-ctypes is not among them: keyring needs it on Windows, so citar[all] ships it.)
BUILD_TOOLS = ("pyinstaller", "pyinstaller-hooks-contrib", "pillow", "pip", "setuptools", "altgraph", "pefile")
#: The uninstall key's place under HKCU (Inno Setup's: ``<AppId>_is1``).
UNINSTALL = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
COMMAND_TIMEOUT = 300.0
#: The date every entry of the zip carries (the earliest a zip can hold), so its bytes depend on the files alone.
ZIP_DATE = (1980, 1, 1, 0, 0, 0)


class Failed(Exception):
    """A check that does not hold."""


def say(text: str) -> None:
    print(text, flush=True)


def check(ok: bool, problem: str, done: str = "") -> None:
    if not ok:
        raise Failed(problem)
    if done:
        say(f"ok: {done}")


def _gate_run():
    spec = importlib.util.spec_from_file_location("ci_gate_run_for_bundle", HERE.with_name("gate_run.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def isolated_env(work: Path) -> dict:
    """This environment without a ``CITAR_*`` variable, with every state folder, the API key file and the per-user
    bases under ``work/state`` (``gate_run.py``'s server environment, without the host and port)."""
    gr = _gate_run()
    state = work / "state"
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
    for name, sub in gr.STATE_VARIABLES.items():
        (state / sub).mkdir(parents=True, exist_ok=True)
        env[name] = str((state / sub).resolve())
    env["CITAR_KEYS_FILE"] = str((state / gr.KEYS_FILE).resolve())
    env["PYTHON_KEYRING_BACKEND"] = gr.NO_KEYRING
    for name, sub in gr.USER_BASES.items():
        (state / "user" / sub).mkdir(parents=True, exist_ok=True)
        env[name] = str((state / "user" / sub).resolve())
    return env


def run(argv: list, env: dict, what: str, want: int = 0) -> str:
    """``argv``'s output (both streams), printed indented; Failed unless it exits ``want``."""
    try:
        done = subprocess.run([str(a) for a in argv], capture_output=True, text=True, encoding="utf-8",
                              errors="replace", env=env, timeout=COMMAND_TIMEOUT, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        raise Failed(f"{what} did not end within {COMMAND_TIMEOUT:.0f} s") from None
    except OSError as e:
        raise Failed(f"{what} did not start: {e}") from None
    out = (done.stdout or "") + (done.stderr or "")
    print("".join(f"    {line}\n" for line in out.splitlines()), end="", flush=True)
    check(done.returncode == want, f"{what} exited {done.returncode}, not {want}")
    return out


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def write_sha256(path: Path) -> Path:
    out = path.with_name(path.name + ".sha256")
    out.write_text(sha256(path) + "\n", encoding="ascii", newline="\n")
    return out


def under(path: str, folder: Path) -> bool:
    a = os.path.normcase(os.path.realpath(path))
    b = os.path.normcase(os.path.realpath(str(folder)))
    return a == b or a.startswith(b.rstrip("\\/") + os.sep)


def program(folder: Path) -> Path:
    exe = folder / ("citar.exe" if os.name == "nt" else "citar")
    check(exe.is_file(), f"no {exe.name} in {folder}")
    return exe


def common_runs(exe: Path, version: str, work: Path, label: str | None) -> dict:
    """``--version``, ``where``, ``doctor --quiet`` and ``doctor``, with the state under ``work``."""
    env = isolated_env(work)
    out = run([exe, "--version"], env, "citar --version").strip()
    check(out == f"CITAR {version}", f"citar --version said {out!r}, not 'CITAR {version}'", out)
    where = run([exe, "where"], env, "citar where")
    state = (work / "state").resolve()
    for line in where.splitlines():
        key, _, value = line.partition("  ")
        if key.strip() in ("state", "saves", "config", "benchmarks", "private data"):
            check(under(value.strip(), state), f"citar where puts {key.strip()} at {value.strip()}, not under "
                                               f"{state}")
    say(f"ok: citar where: every state folder under {state}")
    run([exe, "doctor", "--quiet"], env, "citar doctor --quiet")
    say("ok: citar doctor --quiet exits 0")
    doctor = run([exe, "doctor"], env, "citar doctor")
    engine = re.search(r"\[  ok  \] engine: build ([0-9a-f]{12}) \((.*)\), version (\S+)", doctor)
    check(engine is not None, "citar doctor printed no engine line")
    check(engine.group(3) == version, f"the engine's version is {engine.group(3)}, not {version}")
    if label is not None:
        check(engine.group(2) == label, f"the engine's label is {engine.group(2)!r}, not {label!r}: the wheel's "
                                        f"CITAR_BUILD_ID did not reach the bundle")
    jobs = re.search(r"^\[(.{6})\] jobs: (.*)$", doctor, re.M)
    check(jobs is not None, "citar doctor printed no jobs line")
    check(jobs.group(1) == "  ok  " and jobs.group(2).startswith("verifiable"),
          f"the job host started from the bundle is not verifiable: [{jobs.group(1)}] {jobs.group(2)}",
          f"the job host started from the bundle: {jobs.group(2)}")
    return {"version": version, "build_id": engine.group(1), "label": engine.group(2), "jobs": jobs.group(2)}


def cmd_bundle(args) -> dict:
    folder = args.dir.resolve()
    exe = program(folder)
    report = common_runs(exe, args.version, args.work, args.label)
    run([exe, "jobs-host", "now"], isolated_env(args.work), "citar jobs-host with an argument", want=2)
    say("ok: the hidden jobs-host command is there, and takes no argument")
    engines = [p for p in folder.rglob("_engine.pyd") if p.parent.name == "citar"]
    check(len(engines) == 1, f"the bundle holds {len(engines)} citar/_engine.pyd: {engines}",
          f"{engines[0].relative_to(folder) if engines else ''}, {engines[0].stat().st_size if engines else 0:,} "
          f"bytes")
    for name in NOTICES:
        check((folder / name).is_file(), f"no {name} at the bundle's root")
    for name, rel in SAME_AS_CHECKOUT.items():
        check((folder / name).read_bytes() == (args.checkout / rel).read_bytes(),
              f"the bundle's {name} is not the checkout's {rel}")
    say(f"ok: {', '.join(NOTICES)} at the root; {', '.join(SAME_AS_CHECKOUT)} the checkout's bytes")
    text = (folder / "PYTHON-LICENSES.txt").read_text(encoding="utf-8")
    index = text.split("\nIndex\n-----\n", 1)[1].split("\n\n", 1)[0].splitlines() if "\nIndex\n" in text else []
    names = [line.split(" ", 1)[0].lower() for line in index]
    check(any(line.startswith("CPython ") for line in index), "PYTHON-LICENSES.txt names no CPython")
    check(any(line.startswith("PyInstaller ") and "bootloader" in line for line in index),
          "PYTHON-LICENSES.txt names no PyInstaller bootloader")
    tools = sorted(set(names) & set(BUILD_TOOLS))
    check(not tools, f"PYTHON-LICENSES.txt lists build tools the bundle does not ship: {tools}")
    for needed in ("fastapi", "uvicorn", "sqlalchemy", "anthropic", "openai", "mcp", "authlib", "keyring"):
        check(needed in names, f"PYTHON-LICENSES.txt does not list {needed}, which citar[all] needs")
    say(f"ok: PYTHON-LICENSES.txt: {len(index) - 2} distributions, CPython and the bootloader; no build tool")
    if args.python:
        fresh = args.work / "python-licenses-again.txt"
        run([args.python, "-P", ROOT / "installer" / "python_licenses.py", "--out", fresh], dict(os.environ),
            "python_licenses.py in the bundle's venv")
        check(fresh.read_bytes() == (folder / "PYTHON-LICENSES.txt").read_bytes(),
              f"PYTHON-LICENSES.txt is not what python_licenses.py writes in {args.python}'s venv",
              "PYTHON-LICENSES.txt is the bundle venv's closure, byte for byte")
    report.update({"engine": str(engines[0].relative_to(folder)), "engine_bytes": engines[0].stat().st_size,
                   "notices": {n: sha256(folder / n) for n in NOTICES}, "python_licenses": names})
    return report


def app_id(iss: Path) -> str:
    """``AppId`` of ``installer/citar.iss`` (``{{GUID}`` there: Inno's escaped brace), as the registry names it."""
    m = re.search(r"^AppId=\{(\{[0-9A-Fa-f-]{36}\})\s*$", iss.read_text(encoding="utf-8"), re.M)
    check(m is not None, f"{iss} has no AppId={{{{GUID}}")
    return m.group(1)


def uninstall_key(iss: Path):
    """The installer's per-user uninstall key's values, or None when it is not there (read only)."""
    import winreg

    name = f"{UNINSTALL}\\{app_id(iss)}_is1"
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, name) as key:
            values, i = {}, 0
            while True:
                try:
                    n, v, _ = winreg.EnumValue(key, i)
                except OSError:
                    return values
                values[n] = v
                i += 1
    except FileNotFoundError:
        return None


def cmd_installed(args) -> dict:
    folder = args.dir.resolve()
    exe = program(folder)
    env = isolated_env(args.work)
    out = run([exe, "--version"], env, "the installed citar --version").strip()
    check(out == f"CITAR {args.version}", f"the installed citar --version said {out!r}", out)
    run([exe, "doctor", "--quiet"], env, "the installed citar doctor --quiet")
    say("ok: the installed citar doctor --quiet exits 0")
    key = uninstall_key(args.iss)
    check(key is not None, f"no uninstall key {app_id(args.iss)}_is1 under HKCU\\{UNINSTALL}")
    check(key.get("DisplayVersion") == args.version, f"the uninstall key's DisplayVersion is "
                                                     f"{key.get('DisplayVersion')!r}, not {args.version}")
    location = str(key.get("InstallLocation") or "").rstrip("\\/")
    check(os.path.normcase(location) == os.path.normcase(str(folder)),
          f"the uninstall key's InstallLocation is {location!r}, not {folder}",
          f"the per-user uninstall key {app_id(args.iss)}_is1: version {args.version}, at {location}")
    return {"installed": str(folder), "uninstall_key": f"HKCU\\{UNINSTALL}\\{app_id(args.iss)}_is1"}


def cmd_uninstalled(args) -> dict:
    folder = args.dir.resolve()
    deadline = time.monotonic() + args.wait
    while True:
        key = uninstall_key(args.iss)
        files = [p for p in folder.rglob("*") if p.is_file()] if folder.exists() else []
        if key is None and not files:
            break
        if time.monotonic() > deadline:
            raise Failed(f"after {args.wait:.0f} s the uninstall key is {'there' if key is not None else 'gone'} and "
                         f"{len(files)} file(s) are left in {folder}: {[str(p) for p in files[:5]]}")
        time.sleep(1.0)
    say(f"ok: uninstalled: no uninstall key, no file left in {folder}")
    return {"uninstalled": str(folder)}


def cmd_zip(args) -> dict:
    folder = args.dir.resolve()
    program(folder)
    args.out.mkdir(parents=True, exist_ok=True)
    out = args.out / f"CITAR-{args.version}-windows-x64.zip"
    files = sorted(p for p in folder.rglob("*") if p.is_file())
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for path in files:
            info = zipfile.ZipInfo(f"CITAR/{path.relative_to(folder).as_posix()}", date_time=ZIP_DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, path.read_bytes())
    with zipfile.ZipFile(out) as z:
        names = z.namelist()
        check(z.testzip() is None, f"{out.name} does not read back")
    check(names and all(n.startswith("CITAR/") for n in names), f"{out.name} holds entries outside CITAR/")
    check("CITAR/citar.exe" in names and "CITAR/citar-play.exe" in names, f"{out.name} lacks an executable")
    side = write_sha256(out)
    say(f"ok: {out.name}: {len(names)} files under CITAR/, {out.stat().st_size:,} bytes; {side.name}")
    return {"zip": out.name, "files": len(names), "bytes": out.stat().st_size, "sha256": sha256(out)}


def cmd_sums(args) -> dict:
    out = {}
    for path in args.files:
        check(path.is_file(), f"{path} is not a file")
        side = write_sha256(path)
        out[path.name] = sha256(path)
        say(f"ok: {side.name}")
    return out


def main(argv: list | None = None) -> int:
    ap = argparse.ArgumentParser(prog="check_bundle.py", description=__doc__.split("\n\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("bundle")
    p.add_argument("--dir", type=Path, required=True, help="the bundle's folder (DIST/CITAR)")
    p.add_argument("--version", required=True)
    p.add_argument("--label", default=None, help="the build label the wheel was built with")
    p.add_argument("--work", type=Path, required=True, help="a folder for the runs' state")
    p.add_argument("--python", type=Path, default=None, help="the venv's Python the bundle was built with")
    p.add_argument("--checkout", type=Path, default=ROOT)
    p.add_argument("--out", type=Path, default=None, help="a JSON report")
    p.set_defaults(func=cmd_bundle)
    for name, func in (("installed", cmd_installed), ("uninstalled", cmd_uninstalled)):
        p = sub.add_parser(name)
        p.add_argument("--dir", type=Path, required=True, help="the folder the installer installed into")
        p.add_argument("--iss", type=Path, default=ROOT / "installer" / "citar.iss")
        if name == "installed":
            p.add_argument("--version", required=True)
            p.add_argument("--work", type=Path, required=True)
        else:
            p.add_argument("--wait", type=float, default=120.0)
        p.add_argument("--out", type=Path, default=None)
        p.set_defaults(func=func)
    p = sub.add_parser("zip")
    p.add_argument("--dir", type=Path, required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--out", type=Path, required=True, help="the folder to write the zip and its .sha256 into")
    p.set_defaults(func=cmd_zip)
    p = sub.add_parser("sums")
    p.add_argument("files", type=Path, nargs="+")
    p.set_defaults(func=cmd_sums)
    args = ap.parse_args(argv)
    if getattr(args, "work", None) is not None:
        args.work = args.work.resolve()
        args.work.mkdir(parents=True, exist_ok=True)
    try:
        report = args.func(args)
    except Failed as e:
        print(f"check_bundle: FAILED: {e}", file=sys.stderr, flush=True)
        return 1
    out = getattr(args, "out", None)
    if out is not None and args.cmd in ("bundle", "installed", "uninstalled"):
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    say(f"check_bundle {args.cmd}: every check holds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
