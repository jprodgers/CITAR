"""Install a release wheel in a fresh virtual environment and prove it on this machine, for package.yml's wheel and
sdist-build jobs (design/phase5-release.md P5.3.4).

    python scripts/ci/smoke_wheel.py smoke --wheel WHEEL_OR_DIR --python PY --work DIR [--venv DIR]
        --version V --label L --platform NAME --out FILE [--extras all]
    python scripts/ci/smoke_wheel.py constraints --wheel WHEEL_OR_DIR --python PY --work DIR [--venv DIR] --out FILE

``smoke`` makes a fresh venv with the interpreter ``PY`` (``--venv``, or ``WORK/venv``), installs ``"<wheel>[all]"``
there with ``pip install --only-binary=:all:``, and then runs this script's ``inside`` command with that venv's Python,
as ``python -P smoke_wheel.py inside ...``, from ``WORK/run`` (never the checkout: ``-c`` or ``-m`` from a checkout
would import its ``citar/`` and prove nothing about the wheel). Every ``CITAR_*`` variable of this environment is
dropped there, and the state ones (``CITAR_STATE_DIR``, ``CITAR_DATA_DIR``, ``CITAR_SAVE_DIR``, ``CITAR_CONFIG_DIR``,
``CITAR_BENCH_DIR``) point into ``WORK/state``, so nothing it runs reads or writes a real installation's data.
``inside`` checks, in the venv:

- ``citar`` and ``citar._engine`` are the venv's (under its ``site-packages``), and the copy is not a checkout;
- the engine has no test operations; ``build_info()`` gives the version ``V``, the label ``L`` and no ruleset folder;
  the installed distribution's version is ``V``'s PEP 440 form; the ruleset's counts are sane; the web client, the
  migrations and the hardware collectors are in the package;
- the state directory is ``WORK/state``, and with the variables unset it would still be outside ``site-packages``;
- ``citar --version`` says ``CITAR V``; ``citar where`` and ``citar doctor --quiet`` exit 0;
- **a job host** (``citar.jobs.host.JobHost``) with tiny limits (one thread, 25% of the machine, 256 MB) reaches
  ``ready`` with this build's jobs build and ``verifiable`` true: its canary passed and it replayed the shortest golden
  job to the committed hashes on this platform (design/phase4-helper.md P4.4.3);
- **the same host plays one seeded lab job** (a duel of 30 rounds, the same experiment on every platform) through the
  production path, as the server's dispatcher makes it (``lab.normalize``, ``lab.job_of``,
  ``engine_api.check_job_spec``, then the host's ``job_offer``), to a ``job_result`` whose hash is the result's own and
  which answers its spec (``engine_api.job_result_fits``); then the host is closed.

It writes one JSON line to ``--out``: the version, its PEP 440 form, the label, the build id, the jobs build, the lab
job's spec and result hashes, the platform, the Python, the wheel's name and how the host held its limits. package.yml's
``assemble`` (package 5-08) holds every wheel's line to one version, label, build id, jobs build and result hash, and
the sdist build's to the same with the label ``unknown``.

``constraints`` makes a fresh venv, installs ``"<wheel>[server]"`` the same way, and writes ``pip freeze --exclude
citar`` to ``--out``: the release's tested dependency set (``citar-<pep440>-server-constraints.txt``), which the image
and the server's deploy install with ``--constraint`` (P5.7, P5.13). Each of its lines must pin one distribution
(``name==version``).

A venv folder that exists must be a venv (it is made again from nothing) or empty: anything else is refused, so a
mistyped path cannot be cleared. Exit status 0 when every check passed, 1 when one failed (the message says which), 2
for a usage error or a run that could not start.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve()
RELEASE_PY = HERE.parents[1] / "release.py"

#: The state variables ``inside`` runs with, each a folder under ``WORK/state``.
STATE_VARIABLES = {"CITAR_STATE_DIR": ".", "CITAR_DATA_DIR": "data", "CITAR_SAVE_DIR": "saves",
                   "CITAR_CONFIG_DIR": "config", "CITAR_BENCH_DIR": "benchmarks"}
#: The job host's limits: one thread, a quarter of the machine, 256 MB (P5.3.4).
HOST_THREADS, HOST_CPU_PERCENT, HOST_MEMORY_MB = 1, 25, 256
#: The seeded lab job every platform plays: a duel of 30 rounds. Its result hash is compared across the platforms, so
#: changing it changes every platform's hash alike, and nothing else.
LAB_EXPERIMENT = {"name": "release-smoke", "games": 1, "seed": 5304, "size": "duel", "maps": ["pangaea"],
                  "turns": 30, "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}]}
#: How long a job host may take to play the lab job (it runs at a quarter of the machine).
RUN_TIMEOUT = 900.0
#: How long pip may take to install the wheel and its dependencies, and a console command to run.
INSTALL_TIMEOUT, COMMAND_TIMEOUT = 1200.0, 300.0
#: What the JSON line holds.
RECORD_KEYS = ("smoke", "platform", "python", "wheel", "version", "pep440", "label", "build_id", "jobs_build", "rules",
               "spec_hash", "result_hash", "turns", "verifiable", "host_limits", "seconds")
#: A constraints line: one distribution pinned to one version.
PINNED = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*(\[[A-Za-z0-9._,-]+\])?==[^\s;]+")


class Failed(Exception):
    """A check that did not hold; the message says which and why."""


class Refused(Exception):
    """A run that cannot start (a usage problem); the message says why."""


def say(line: str) -> None:
    print(f"smoke_wheel: {line}", flush=True)


def release_version(text: str):
    """``text`` read by scripts/release.py's own rule (``Version``): its PEP 440 form is ``.pep440``."""
    spec = importlib.util.spec_from_file_location("citar_release_script", RELEASE_PY)
    if spec is None or not RELEASE_PY.is_file():
        raise Refused(f"{RELEASE_PY} is missing: run this script from a checkout")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module      # its dataclasses look their module up while the class is made
    spec.loader.exec_module(module)
    try:
        return module.Version.parse(text)
    except module.Refused as e:
        raise Refused(str(e)) from None


def find_wheel(where: Path) -> Path:
    """The wheel ``where`` names: itself, or the one ``*.whl`` in it."""
    if where.is_file() and where.suffix == ".whl":
        return where.resolve()
    if where.is_dir():
        found = sorted(where.glob("*.whl"))
        if len(found) == 1:
            return found[0].resolve()
        raise Refused(f"{where}: expected exactly one wheel, found {', '.join(f.name for f in found) or 'none'}")
    raise Refused(f"{where}: no such wheel or directory")


def venv_python(venv: Path) -> Path:
    return venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")


def venv_script(venv: Path, name: str) -> Path:
    return venv / ("Scripts" if os.name == "nt" else "bin") / (f"{name}.exe" if os.name == "nt" else name)


def fresh_venv(python: str, venv: Path) -> Path:
    """A new venv at ``venv`` made by ``python``: an existing venv there is made again from nothing (``--clear``), an
    empty folder is used, and anything else is refused."""
    if venv.exists():
        if not venv.is_dir():
            raise Refused(f"{venv} is a file, not a venv")
        if any(venv.iterdir()) and not (venv / "pyvenv.cfg").is_file():
            raise Refused(f"{venv} holds files and is no venv: name an empty folder or a venv")
    run([python, "-m", "venv", "--clear", str(venv)], "make the venv", timeout=COMMAND_TIMEOUT)
    exe = venv_python(venv)
    if not exe.is_file():
        raise Failed(f"the venv at {venv} has no {exe.name}")
    return exe


def run(argv: list, what: str, *, timeout: float, cwd=None, env=None) -> str:
    """Runs ``argv``, which must exit 0 within ``timeout`` seconds; its output (standard error folded in)."""
    shown = " ".join(str(a) for a in argv)
    say(f"{what}: {shown}")
    try:
        done = subprocess.run([str(a) for a in argv], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    except subprocess.TimeoutExpired as e:
        tail = (e.stdout or b"").decode("utf-8", "replace")[-4000:]
        raise Failed(f"{what} did not end within {timeout:.0f} s:\n{tail}") from None
    except OSError as e:
        raise Failed(f"{what} could not start: {e}") from None
    out = done.stdout.decode("utf-8", "replace")
    if done.returncode != 0:
        raise Failed(f"{what} exited {done.returncode}:\n{out[-6000:]}")
    return out


def install(python: Path, wheel: Path, extras: str) -> None:
    env = {**os.environ, "PIP_DISABLE_PIP_VERSION_CHECK": "1"}
    run([python, "-m", "pip", "install", "--only-binary=:all:", f"{wheel}[{extras}]"],
        f"install {wheel.name}[{extras}]", timeout=INSTALL_TIMEOUT, env=env)


def scrubbed_env(state: Path) -> dict:
    """This environment without any ``CITAR_*`` variable or ``PYTHONPATH``/``PYTHONHOME``, and the state variables
    pointed into ``state``."""
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith("CITAR_") and k.upper() not in ("PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV")}
    for name, sub in STATE_VARIABLES.items():
        env[name] = str((state / sub).resolve())
    env["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"
    return env


def smoke(args) -> int:
    wheel = find_wheel(args.wheel)
    version = release_version(args.version)
    work = args.work.resolve()
    venv = (args.venv or work / "venv").resolve()
    state, folder = work / "state", work / "run"
    for path in (state, folder):
        if path.exists():
            shutil.rmtree(path)
        path.mkdir(parents=True)
    python = fresh_venv(args.python, venv)
    install(python, wheel, args.extras)
    out = args.out.resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        out.unlink()
    argv = [python, "-P", HERE, "inside", "--version", version.text, "--label", args.label,
            "--platform", args.platform, "--wheel-name", wheel.name, "--state", state, "--venv", venv, "--out", out]
    say(f"inside the venv, from {folder}: {' '.join(str(a) for a in argv)}")
    try:
        done = subprocess.run([str(a) for a in argv], cwd=folder, env=scrubbed_env(state), stdin=subprocess.DEVNULL,
                              timeout=RUN_TIMEOUT + 2 * COMMAND_TIMEOUT + 300)
    except subprocess.TimeoutExpired:
        raise Failed("the checks inside the venv did not end in time") from None
    if done.returncode != 0:
        raise Failed(f"the checks inside the venv failed (exit {done.returncode}): see above")
    record = read_record(out)
    say(f"passed: {json.dumps(record, sort_keys=True)}")
    return 0


def read_record(path: Path) -> dict:
    """The one JSON line at ``path``, with every key of ``RECORD_KEYS``."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
        record = json.loads(lines[0]) if len(lines) == 1 else None
    except (OSError, ValueError, IndexError) as e:
        raise Failed(f"{path} holds no smoke line: {e}") from None
    if not isinstance(record, dict):
        raise Failed(f"{path} holds {len(lines)} lines, not one JSON object")
    missing = [k for k in RECORD_KEYS if k not in record]
    if missing:
        raise Failed(f"{path}'s line lacks {', '.join(missing)}")
    return record


def constraints(args) -> int:
    wheel = find_wheel(args.wheel)
    work = args.work.resolve()
    venv = (args.venv or work / "venv").resolve()
    work.mkdir(parents=True, exist_ok=True)
    python = fresh_venv(args.python, venv)
    install(python, wheel, "server")
    env = {**os.environ, "PIP_DISABLE_PIP_VERSION_CHECK": "1"}
    frozen = run([python, "-m", "pip", "freeze", "--exclude", "citar"], "freeze the [server] venv",
                 timeout=COMMAND_TIMEOUT, env=env)
    lines = [line.strip() for line in frozen.splitlines() if line.strip()]
    problems = check_constraints(lines)
    if problems:
        raise Failed("the frozen set is no constraints file:\n  " + "\n  ".join(problems))
    out = args.out.resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(f"{line}\n" for line in lines), encoding="utf-8", newline="\n")
    say(f"wrote {out} ({len(lines)} distributions, the [server] extra's closure for {wheel.name})")
    return 0


def check_constraints(lines: list) -> list:
    """What makes ``pip freeze``'s lines unfit to constrain an install: none at all, a line that pins nothing
    (``name @ file://...``, ``-e ...``), CITAR itself, or a distribution twice."""
    if not lines:
        return ["it is empty: the [server] extra installed nothing"]
    problems, seen = [], set()
    for line in lines:
        if not PINNED.fullmatch(line):
            problems.append(f"{line!r} pins no version from an index")
            continue
        name = re.split(r"[\[=]", line, maxsplit=1)[0].lower().replace("_", "-").replace(".", "-")
        if name == "citar":
            problems.append(f"{line!r} is CITAR itself")
        if name in seen:
            problems.append(f"{name} is in it twice")
        seen.add(name)
    return problems


# ------------------------------------------------------------------------------------------------ inside the venv


def check(condition, message: str, ok: str) -> None:
    if not condition:
        raise Failed(message)
    say(f"ok: {ok}")


def under(path, folder) -> bool:
    try:
        Path(path).resolve().relative_to(Path(folder).resolve())
        return True
    except ValueError:
        return False


def inside(args) -> int:
    """The checks, run by the venv's Python with ``-P`` from a folder of their own (``smoke`` starts this)."""
    import importlib.metadata
    import platform
    import sysconfig

    t0 = time.perf_counter()
    version = release_version(args.version)
    state, venv = args.state.resolve(), args.venv.resolve()
    check(under(sys.prefix, venv), f"this is {sys.prefix}, not the venv {venv}", f"running in the venv {venv}")
    check(not any(k.startswith("CITAR_") and k not in STATE_VARIABLES for k in os.environ),
          f"CITAR_ variables reached the checks: {sorted(k for k in os.environ if k.startswith('CITAR_'))}",
          "no CITAR_ variable but the state ones")
    site = {Path(sysconfig.get_paths()[k]).resolve() for k in ("purelib", "platlib")}
    import citar
    from citar import _engine, engine_api, paths
    check(any(under(citar.__file__, s) for s in site), f"citar is {citar.__file__}, not the venv's site-packages "
          f"({', '.join(map(str, site))})", f"citar from {Path(citar.__file__).parent}")
    check(any(under(_engine.__file__, s) for s in site), f"the extension is {_engine.__file__}, not the wheel's",
          f"citar._engine from {Path(_engine.__file__).name}")
    check(not paths.in_source_checkout(), "the installed copy thinks it is a checkout", "not a checkout")
    check(not _engine.HAS_TEST_OPS, "the release wheel carries the test operations", "no test operations")
    info = engine_api.build_info()
    check(info["version"] == version.text, f"build_info's version is {info['version']}, not {version.text}",
          f"build_info version {info['version']}")
    check(info["label"] == args.label, f"build_info's label is {info['label']!r}, not {args.label!r}: the build's "
          f"CITAR_BUILD_ID did not reach the engine", f"label {info['label']}")
    check(info["ruleset_dir"] is None, f"build_info names a ruleset folder: {info['ruleset_dir']}",
          "the ruleset compiled in")
    installed = importlib.metadata.version("citar")
    check(installed == version.pep440, f"the installed distribution is {installed}, not {version.pep440}",
          f"distribution citar {installed}")
    check(citar.__version__ == version.text, f"citar.__version__ is {citar.__version__}, not {version.text}",
          f"__version__ {citar.__version__}")
    counts = engine_api.ruleset_counts()
    check(counts.get("techs", 0) > 50 and counts.get("nations", 0) > 40, f"the ruleset did not load: {counts}",
          f"ruleset {counts.get('techs')} techs, {counts.get('nations')} nations")
    for what, path in (("the web client", paths.web_dir() / "index.html"),
                       ("the migrations", paths.migrations_dir() / "env.py"),
                       ("the hardware collectors", paths.collectors_dir() / "collect_hardware.sh")):
        check(path.is_file(), f"{what} is missing from the wheel ({path})", what)
    check(list((paths.migrations_dir() / "versions").glob("*.py")), "no migration in the wheel", "migration versions")
    check(under(paths.save_dir(), state) and under(paths.data_dir(), state),
          f"the state is {paths.save_dir()} and {paths.data_dir()}, not under {state}", f"state under {state}")
    held = {k: os.environ.pop(k) for k in list(STATE_VARIABLES)}
    try:
        default = paths.state_dir()      # computed only: nothing is created
    finally:
        os.environ.update(held)
    check(not any(under(default, s) for s in site), f"with no variable set the state would go to {default}, inside "
          f"site-packages", f"the default state directory is outside site-packages ({default})")

    venv_env = dict(os.environ)
    out = run([venv_script(venv, "citar"), "--version"], "citar --version", timeout=COMMAND_TIMEOUT, env=venv_env)
    check(out.strip() == f"CITAR {version.text}", f"citar --version said {out.strip()!r}", out.strip())
    for argv in (["where"], ["doctor", "--quiet"]):
        out = run([venv_script(venv, "citar"), *argv], f"citar {' '.join(argv)}", timeout=COMMAND_TIMEOUT,
                  env=venv_env)
        print("".join(f"    {line}\n" for line in out.splitlines()), end="", flush=True)
    say("ok: citar --version, where and doctor --quiet exit 0")

    record = play_lab_job(version, info, args)
    record.update({"smoke": 1, "platform": args.platform, "python": platform.python_version(),
                   "wheel": args.wheel_name, "version": version.text, "pep440": version.pep440,
                   "label": info["label"], "build_id": info["build_id"], "rules": info["rules"],
                   "seconds": round(time.perf_counter() - t0, 1)})
    line = json.dumps({k: record[k] for k in RECORD_KEYS}, sort_keys=False)
    args.out.write_text(line + "\n", encoding="utf-8", newline="\n")
    say(f"ok: wrote {args.out}")
    return 0


def play_lab_job(version, info: dict, args) -> dict:
    """The job host's part: ready and verifiable, then the seeded lab job through the production path."""
    from citar import engine_api, lab
    from citar.jobs import host as H

    jobs_build = engine_api.jobs_build()
    host = H.JobHost(HOST_THREADS, HOST_CPU_PERCENT, HOST_MEMORY_MB)
    t0 = time.perf_counter()
    try:
        try:
            ready = host.start()
        except H.HostError as e:
            raise Failed(f"the job host did not start ({e.code}): {e}") from None
        check(ready.get("jobs_build") == jobs_build, f"the host's jobs build is {ready.get('jobs_build')}, not "
              f"{jobs_build}", f"job host ready in {time.perf_counter() - t0:.2f} s, jobs build {jobs_build}")
        check(ready.get("verifiable") is True, f"the job host is not verifiable: {ready}",
              "verifiable (its canary passed and it replayed the golden job to the committed hashes)")
        exp = lab.normalize(dict(LAB_EXPERIMENT))
        kind, spec = lab.job_of(exp, 0)
        checked = engine_api.check_job_spec(kind, spec)
        t1 = time.perf_counter()
        host.offer("smoke-run", kind, checked["spec"], checked["spec_hash"], RUN_TIMEOUT, job="smoke-job")
        try:
            frames = H.run_until(host, "smoke-run", timeout=RUN_TIMEOUT + 60)
        except (TimeoutError, H.HostError) as e:
            raise Failed(f"the lab job did not finish: {e}") from None
        last = frames[-1]
        check(frames[0].get("t") == "job_accept" and last.get("t") == "job_result",
              f"the lab job ended with {last}", f"the lab job played in {time.perf_counter() - t1:.2f} s")
        outcome = last["outcome"]
        result = outcome["result"]
        check("crash" not in result, f"the lab job crashed: {result.get('crash')}", "no crash")
        check(engine_api.job_result_hash(kind, result) == outcome["result_hash"],
              "the result's hash is not the one the host reported", f"result hash {outcome['result_hash']}")
        try:
            engine_api.job_result_fits(kind, checked["spec"], result, outcome["chain"], checked["spec_hash"])
        except engine_api.JobRefused as e:
            raise Failed(f"the result does not answer its spec ({e.code}): {e}") from None
        say("ok: the result answers its spec")
        check(result.get("engine") == info["build_id"], f"the result names the build {result.get('engine')}",
              "the result names this build")
        check(not host.in_flight, f"runs still in flight: {host.in_flight}", "nothing left in flight")
        return {"jobs_build": jobs_build, "spec_hash": checked["spec_hash"], "result_hash": outcome["result_hash"],
                "turns": result.get("turns"), "verifiable": ready.get("verifiable"),
                "host_limits": ready.get("limits")}
    finally:
        code = host.close()
        say(f"the job host closed (exit {code})")


# ------------------------------------------------------------------------------------------------ command line


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(prog="smoke_wheel.py", description=__doc__.splitlines()[0])
    sub = top.add_subparsers(dest="command", required=True)
    for name in ("smoke", "constraints"):
        p = sub.add_parser(name)
        p.add_argument("--wheel", type=Path, required=True, help="the wheel, or a folder holding exactly one")
        p.add_argument("--python", required=True, help="the interpreter that makes the venv")
        p.add_argument("--work", type=Path, required=True, help="a folder for the venv, the state and the run")
        p.add_argument("--venv", type=Path, help="where to make the venv (default WORK/venv)")
        p.add_argument("--out", type=Path, required=True, help="the smoke line, or the constraints file")
        if name == "smoke":
            p.add_argument("--version", required=True, help="the release's version, X.Y.Z[-rc.N]")
            p.add_argument("--label", required=True, help="the build label the wheel must report (unknown: none)")
            p.add_argument("--platform", required=True, help="the platform's name in the line, e.g. windows-x64")
            p.add_argument("--extras", default="all", help="the extras installed with the wheel (default all)")
    p = sub.add_parser("inside", help="the checks, inside the venv (smoke runs it)")
    for flag in ("--version", "--label", "--platform", "--wheel-name"):
        p.add_argument(flag, required=True)
    for flag in ("--state", "--venv", "--out"):
        p.add_argument(flag, type=Path, required=True)
    return top


def main(argv=None) -> int:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="backslashreplace")
    args = parser().parse_args(argv)
    try:
        return {"smoke": smoke, "constraints": constraints, "inside": inside}[args.command](args)
    except Refused as e:
        print(f"smoke_wheel {args.command}: refused: {e}", file=sys.stderr)
        return 2
    except Failed as e:
        print(f"smoke_wheel {args.command}: FAILED: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
