"""Run a built CITAR server image and prove it, and save it for a release: package.yml's image job and package 5-06's
gate (design/phase5-release.md P5.7). Standard library only; the engine is Docker (CI's runners) or Podman (WSL).

    python scripts/ci/check_image.py run --engine docker|podman --image REF --arch amd64|arm64 --version V --label L
        --wheel FILE [--constraints FILE] --out FILE
    python scripts/ci/check_image.py save --engine docker|podman --image REF --out FILE.tar.zst [--report FILE]

``run`` checks the image as built (its architecture, ``USER 10001:10001``, the state volume, the port, the health
check, the version label), then runs it as it is deployed, its port published on 127.0.0.1 only and its state in a
volume of its own:

- the health route answers within 60 s, and ``/api/meta`` names the version and the label;
- in the running container, as uid and gid 10001: ``citar --version``; the image's own health check exits 0; the
  engine is the venv's and has no test operations; ``build_info()`` gives the version and the label; ``citar doctor
  --quiet`` exits 0 and ``citar doctor``'s jobs line says the job host it started there is verifiable; the state
  directory is the server's (10001:10001, 0750); every installed distribution is the one ``--constraints`` pins
  (the release's tested set, ``pip freeze`` against it);
- in a container of its own (no network, the checkout's ``scripts/`` mounted read-only), the wheels' smoke
  (``scripts/ci/smoke_wheel.py inside``, the state in the container's ``/tmp``): its job host plays the seeded lab
  job, whose result hash every wheel's smoke line shares;
- **a restart keeps the volume's state**: an account made in the first container (``citar admin create-user``) and the
  generated secret key are there in a second container started on the same volume after the first was stopped (by
  SIGTERM, within 30 s) and removed.

Every container and the volume are removed afterwards, whatever happened (their names start ``citar-check-``). The
report (``--out``) holds the image's id and architecture, the version, label, build id and jobs build (package 5-08's
``assemble`` holds them to the wheels', the helpers' and the bundle's), the smoke line, and what each check measured.

``save`` writes the image to ``FILE.tar.zst`` (the engine's ``save``, then zstd), removes the image, loads the file
again and requires the same image id: what release.yml loads and pushes is what ``run`` tested. With ``--report`` it
adds the file's name, size and SHA-256 to that report.

Exit status 0 when every check held, 1 when one failed (the message says which), 2 for a usage error or a run that
could not start (no such engine, image or file).
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import secrets
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve()
SCRIPTS = HERE.parents[1]
ENGINES = ("docker", "podman")
ARCHES = ("amd64", "arm64")
#: What the image is built to be (the Dockerfile): who it runs as, where its state lives, the port it serves.
USER, STATE_DIR, PORT = "10001:10001", "/var/lib/citar", 8765
STATE_OWNER = "10001:10001:750"
VENV = "/opt/venv"
#: The image's version label (the build's --label), compared with --version when present.
VERSION_LABEL = "org.opencontainers.image.version"
#: How long the server may take to answer its health route (P5.7), and to stop on SIGTERM before the engine kills it.
HEALTH_SECONDS, STOP_SECONDS = 60.0, 30
#: Every container and volume a run makes is named with this, then a random part.
PREFIX = "citar-check-"
#: Where the smoke's container sees the checkout's scripts (read-only), and its state and run folders.
CHECK_MOUNT = "/opt/citar-check/scripts"
SMOKE_DIR = "/tmp/citar-smoke"
#: The smoke's line, printed by its container after this marker.
SMOKE_MARK = "SMOKE-LINE "
#: The account the restart check makes (example.com: the account rules refuse a special-use name such as .invalid), and
#: the build facts asked of the running server.
MARKER_HANDLE, MARKER_EMAIL = "volume-check", "volume-check@example.com"
BUILD_FACTS = ("import json; from citar import engine_api; "
               "print(json.dumps({'build': engine_api.build_info(), 'jobs_build': engine_api.jobs_build()}))")
#: The smoke, as its container's shell runs it: the image's server variables dropped, the state variables pointed into
#: the container's /tmp, and smoke_wheel.py's inside checks run with -P from a folder of their own. The values come in
#: as the shell's positional arguments, never written into the script.
SMOKE_SCRIPT = """set -eu
unset CITAR_HOST CITAR_PORT
export PYTHONIOENCODING=utf-8
{exports}
mkdir -p "$state" "$run"
cd "$run"
{venv}/bin/python -P {mount}/ci/smoke_wheel.py inside --version "$1" --label "$2" --platform "$3" \\
  --wheel-name "$4" --state "$state" --venv {venv} --out "$out"
printf '{mark}%s\\n' "$(cat "$out")"
"""
#: Timeouts, in seconds, for an engine's commands.
QUICK, EXEC, SMOKE, SAVE = 120, 600, 2400, 1200


class Failed(Exception):
    """A check that did not hold; the message says which and why."""


class Refused(Exception):
    """A run that cannot start (a usage problem); the message says why."""


def say(line: str) -> None:
    print(f"check_image: {line}", flush=True)


def check(condition, message: str, ok: str) -> None:
    if not condition:
        raise Failed(message)
    say(f"ok: {ok}")


def load_script(name: str):
    """A sibling script of this one, loaded by path (scripts/ci is no package)."""
    spec = importlib.util.spec_from_file_location(f"citar_ci_{name}", HERE.parent / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module      # its dataclasses look their module up while the class is made
    spec.loader.exec_module(module)
    return module


smoke_wheel = load_script("smoke_wheel")


# ------------------------------------------------------------------------------------------------ what is read


def parse_port(text: str) -> int:
    """The host port an engine's ``port NAME 8765/tcp`` names, which must be on 127.0.0.1 and only there."""
    found = []
    for line in text.split("\n"):
        line = line.strip()
        if not line:
            continue
        host, _, port = line.rpartition(":")
        if host != "127.0.0.1" or not port.isdigit():
            raise Failed(f"the port is published on {line!r}, not on 127.0.0.1 alone")
        found.append(int(port))
    if len(set(found)) != 1:
        raise Failed(f"the engine names no single host port for {PORT}: {text!r}")
    return found[0]


def image_id(raw: str) -> str:
    """An image id as Docker (``sha256:<hex>``) or Podman (``<hex>``) prints it, as 64 hex digits."""
    value = raw.strip().removeprefix("sha256:")
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise Failed(f"{raw!r} is no image id")
    return value


def healthcheck_test(data: dict) -> list:
    """The image's health check command (Docker keeps it in ``Config.Healthcheck``, Podman in ``HealthCheck``)."""
    for where in ((data.get("Config") or {}).get("Healthcheck"), data.get("HealthCheck"), data.get("Healthcheck")):
        if isinstance(where, dict) and where.get("Test"):
            return list(where["Test"])
    return []


def image_facts(data: dict, arch: str, version: str) -> dict:
    """The image as built, checked against the Dockerfile and this build: its id, architecture, user, state volume,
    port, health check and version label."""
    config = data.get("Config") or {}
    facts = {"id": image_id(str(data.get("Id", ""))), "arch": data.get("Architecture"), "os": data.get("Os"),
             "user": config.get("User"), "created": data.get("Created"), "size": data.get("Size")}
    check(facts["os"] == "linux" and facts["arch"] == arch,
          f"the image is {facts['os']}/{facts['arch']}, not linux/{arch}", f"linux/{arch}, id {facts['id'][:12]}")
    check(facts["user"] == USER, f"the image runs as {facts['user']!r}, not {USER!r}", f"USER {USER}")
    env = dict(item.split("=", 1) for item in config.get("Env") or [] if "=" in item)
    check(env.get("CITAR_STATE_DIR") == STATE_DIR and STATE_DIR in (config.get("Volumes") or {}),
          f"the state is {env.get('CITAR_STATE_DIR')!r} and the volumes {sorted(config.get('Volumes') or {})}",
          f"its state in the volume {STATE_DIR}")
    check(f"{PORT}/tcp" in (config.get("ExposedPorts") or {}) and env.get("CITAR_PORT") == str(PORT),
          f"the image exposes {sorted(config.get('ExposedPorts') or {})}, CITAR_PORT {env.get('CITAR_PORT')!r}",
          f"port {PORT}")
    test = healthcheck_test(data)
    check(test[:1] in (["CMD"], ["CMD-SHELL"]) and len(test) > 1, f"the image's health check is {test!r}",
          "a health check")
    facts["healthcheck"] = test
    labels = config.get("Labels") or {}
    check(labels.get(VERSION_LABEL) == version, f"its {VERSION_LABEL} label is {labels.get(VERSION_LABEL)!r}, not "
          f"{version!r}", f"labelled version {version}")
    return facts


def jobs_line(doctor: str) -> str:
    """``citar doctor``'s jobs line, which must say the job host it started is verifiable."""
    found = re.search(r"^\[(.{6})\] jobs: (.*)$", doctor, re.M)
    if found is None or found.group(1) != "  ok  " or not found.group(2).startswith("verifiable"):
        raise Failed(f"citar doctor's jobs line is not verifiable: {found.group(0) if found else 'none printed'}")
    return found.group(2)


def distribution(line: str) -> tuple:
    """A ``name==version`` line as (its PEP 503 name, its version)."""
    name, _, version = line.strip().partition("==")
    name = re.sub(r"\[.*\]$", "", name)
    return re.sub(r"[-_.]+", "-", name).lower(), version


def freeze_problems(constraints: list, frozen: list) -> tuple:
    """What in an image's ``pip freeze`` the release's constraints do not pin at that version (problems), and what
    they pin that is not installed (notes, which a platform's markers may explain)."""
    pinned = dict(distribution(line) for line in constraints if line.strip() and not line.startswith("#"))
    installed = dict(distribution(line) for line in frozen if line.strip())
    problems = [f"{name} {version} is installed, and the constraints pin "
                f"{pinned[name] if name in pinned else 'nothing of that name'}"
                for name, version in sorted(installed.items()) if pinned.get(name) != version]
    notes = [f"{name} {pinned[name]} is pinned and not installed" for name in sorted(set(pinned) - set(installed))]
    return problems, notes


def smoke_record(output: str) -> dict:
    """The smoke's JSON line from its container's output, with every key of ``smoke_wheel.RECORD_KEYS``."""
    lines = [line[len(SMOKE_MARK):] for line in output.splitlines() if line.startswith(SMOKE_MARK)]
    if len(lines) != 1:
        raise Failed(f"the smoke printed {len(lines)} lines after {SMOKE_MARK!r}, not one")
    try:
        record = json.loads(lines[0])
    except ValueError as e:
        raise Failed(f"the smoke's line does not read: {e}") from None
    missing = [k for k in smoke_wheel.RECORD_KEYS if k not in record] if isinstance(record, dict) else ["all"]
    if missing:
        raise Failed(f"the smoke's line lacks {', '.join(missing)}")
    return record


def smoke_script() -> str:
    """The shell script the smoke's container runs (``SMOKE_SCRIPT`` with its folders filled in)."""
    state, run, out = f"{SMOKE_DIR}/state", f"{SMOKE_DIR}/run", f"{SMOKE_DIR}/line.json"
    exports = "\n".join([f"state={state} run={run} out={out}"]
                        + [f"export {name}={state}" + ("" if sub == "." else f"/{sub}")
                           for name, sub in smoke_wheel.STATE_VARIABLES.items()])
    return SMOKE_SCRIPT.format(exports=exports, venv=VENV, mount=CHECK_MOUNT, mark=SMOKE_MARK)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# ------------------------------------------------------------------------------------------------ the engine


class Engine:
    """Docker's or Podman's command line, the two read alike here."""

    def __init__(self, name: str):
        if name not in ENGINES:
            raise Refused(f"--engine {name}: one of {', '.join(ENGINES)}")
        exe = shutil.which(name)
        if exe is None:
            raise Refused(f"no {name} on PATH")
        self.name, self.exe = name, exe

    def __call__(self, *args, what: str, timeout: float = QUICK, stdin: str | None = None,
                 ok=(0,)) -> tuple[int, str]:
        """Runs the engine with ``args``; its exit status (one of ``ok``, else ``Failed``) and its standard output.
        Standard error is shown on a failure only, so a warning never spoils a JSON answer."""
        argv = [self.exe, *map(str, args)]
        try:
            done = subprocess.run(argv, input=stdin, capture_output=True, text=True, encoding="utf-8",
                                  errors="replace", timeout=timeout, stdin=None if stdin is not None
                                  else subprocess.DEVNULL)
        except subprocess.TimeoutExpired as e:
            raise Failed(f"{what} ({self.name} {args[0]}) did not end within {timeout:.0f} s: "
                         f"{(e.stdout or '')[-2000:]}") from None
        except OSError as e:
            raise Failed(f"{what} could not start: {e}") from None
        if done.returncode not in ok:
            raise Failed(f"{what} ({self.name} {' '.join(map(str, args[:2]))}) exited {done.returncode}:\n"
                         f"{(done.stdout or '')[-4000:]}{(done.stderr or '')[-4000:]}")
        return done.returncode, done.stdout or ""

    def inspect(self, kind: str, ref: str) -> dict:
        _, out = self(kind, "inspect", ref, what=f"inspect the {kind} {ref}")
        try:
            data = json.loads(out)
        except ValueError:
            raise Failed(f"{self.name} {kind} inspect {ref} printed no JSON: {out[-2000:]!r}") from None
        if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
            raise Failed(f"{self.name} {kind} inspect {ref} did not describe one {kind}")
        return data[0]

    def has_image(self, ref: str) -> bool:
        code, _ = self("image", "inspect", ref, what=f"look for {ref}", ok=(0, 1, 125))
        return code == 0

    def exec(self, name: str, *argv, what: str, timeout: float = EXEC, stdin: str | None = None) -> str:
        flags = ["-i"] if stdin is not None else []
        return self("exec", *flags, name, *argv, what=what, timeout=timeout, stdin=stdin)[1]

    def logs(self, name: str) -> str:
        try:
            _, out = self("logs", "--tail", "120", name, what=f"the logs of {name}", ok=(0, 1, 125))
            return out
        except Failed as e:
            return str(e)

    def leftovers(self, prefix: str) -> list:
        _, containers = self("ps", "-a", "--filter", f"name={prefix}", "--format", "{{.Names}}",
                             what="list this run's containers")
        _, volumes = self("volume", "ls", "--filter", f"name={prefix}", "--format", "{{.Name}}",
                          what="list this run's volumes")
        return [n for n in (containers + volumes).split() if n.startswith(prefix)]


# ------------------------------------------------------------------------------------------------ run


def http_get(port: int, path: str, timeout: float = 3.0) -> tuple:
    """The status and body of ``GET http://127.0.0.1:<port><path>``; (0, reason) when nothing answered."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except (urllib.error.URLError, OSError) as e:
        return 0, str(e)


class Server:
    """One server container on the run's volume, its port published on 127.0.0.1 only."""

    def __init__(self, engine: Engine, name: str, volume: str, image: str):
        self.engine, self.name, self.volume, self.image = engine, name, volume, image
        self.port = 0

    def start(self) -> float:
        """Starts the container and waits for its health route: the seconds it took, at most ``HEALTH_SECONDS``."""
        t0 = time.perf_counter()
        self.engine("run", "-d", "--name", self.name, "-p", f"127.0.0.1::{PORT}",
                    "-v", f"{self.volume}:{STATE_DIR}", self.image, what=f"start {self.name}")
        _, out = self.engine("port", self.name, f"{PORT}/tcp", what=f"the port of {self.name}")
        self.port = parse_port(out)
        last = (0, "")
        while time.perf_counter() - t0 < HEALTH_SECONDS:
            last = http_get(self.port, "/api/auth/config", timeout=2.0)
            if last[0] == 200:
                seconds = time.perf_counter() - t0
                say(f"ok: {self.name} answered its health route on 127.0.0.1:{self.port} in {seconds:.1f} s")
                return seconds
            if not self.engine.inspect("container", self.name).get("State", {}).get("Running"):
                raise Failed(f"{self.name} exited before it answered:\n{self.engine.logs(self.name)}")
            time.sleep(0.5)
        raise Failed(f"{self.name} did not answer its health route within {HEALTH_SECONDS:.0f} s (last: {last[0]} "
                     f"{last[1][:300]!r}):\n{self.engine.logs(self.name)}")

    def stop(self) -> dict:
        """Stops the container with SIGTERM, which it must obey within ``STOP_SECONDS`` and exit 0, then removes it."""
        t0 = time.perf_counter()
        self.engine("stop", "-t", STOP_SECONDS, self.name, what=f"stop {self.name}", timeout=STOP_SECONDS + 60)
        seconds = time.perf_counter() - t0
        state = self.engine.inspect("container", self.name).get("State", {})
        code = state.get("ExitCode")
        check(not state.get("Running") and code == 0 and not state.get("OOMKilled") and seconds < STOP_SECONDS,
              f"{self.name} stopped with exit code {code} in {seconds:.1f} s (OOM killed: {state.get('OOMKilled')}):\n"
              f"{self.engine.logs(self.name)}", f"{self.name} stopped on SIGTERM in {seconds:.1f} s, exit 0")
        self.engine("rm", self.name, what=f"remove {self.name}")
        return {"stop_seconds": round(seconds, 1), "exit_code": code}

    def exec(self, *argv, what: str, stdin: str | None = None) -> str:
        return self.engine.exec(self.name, *argv, what=what, stdin=stdin)


def run(args) -> int:
    engine = Engine(args.engine)
    version = smoke_wheel.release_version(args.version)
    wheel = Path(args.wheel)
    if not wheel.is_file() or wheel.suffix != ".whl":
        raise Refused(f"--wheel {wheel}: no such wheel")
    constraints = None
    if args.constraints is not None:
        try:
            constraints = args.constraints.read_text(encoding="utf-8").splitlines()
        except OSError as e:
            raise Refused(f"--constraints {args.constraints}: {e}") from None
    if not engine.has_image(args.image):
        raise Refused(f"{engine.name} has no image {args.image}: build it first")
    say(f"{args.image} with {engine.name} ({engine('--version', what='the engine version')[1].strip()})")
    t0 = time.perf_counter()
    report = {"image": args.image, "engine": engine.name, "platform": f"image-{args.arch}",
              **image_facts(engine.inspect("image", args.image), args.arch, version.text)}
    prefix = f"{PREFIX}{secrets.token_hex(4)}"
    volume = f"{prefix}-state"
    servers = []
    try:
        engine("volume", "create", volume, what="make the state volume")
        first = Server(engine, f"{prefix}-a", volume, args.image)
        servers.append(first)
        report["health_seconds"] = round(first.start(), 1)
        report.update(check_running(first, version, args.label, constraints))
        report["smoke"] = smoke(engine, prefix, args, version)
        for key in ("build_id", "jobs_build", "version", "label"):
            check(report["smoke"][key] == report[key], f"the smoke's {key} is {report['smoke'][key]}, the server's "
                  f"{report[key]}", f"the smoke's {key} is the server's")
        report["restart"] = restart(engine, first, Server(engine, f"{prefix}-b", volume, args.image), servers,
                                    version, args.label)
    finally:
        cleanup(engine, prefix, servers, volume)
    left = engine.leftovers(prefix)
    check(not left, f"left behind: {', '.join(left)}", "no container or volume of this run left")
    report["seconds"] = round(time.perf_counter() - t0, 1)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    say(f"passed: wrote {args.out}")
    return 0


def check_running(server: Server, version, label: str, constraints) -> dict:
    """The checks inside the first server, as deployed: who it runs as, its version, health check, engine, doctor,
    state directory and dependencies."""
    status, body = http_get(server.port, "/api/meta")
    try:
        meta = json.loads(body) if status == 200 else {}
    except ValueError:
        meta = {}
    check(meta.get("version") == version.text and meta.get("label") == label,
          f"/api/meta answered {status}: {body[:500]!r}", f"/api/meta: version {meta.get('version')}, label "
          f"{meta.get('label')}, mode {meta.get('mode')}")
    ids = server.exec("sh", "-c", "id -u; id -g", what="who the server runs as").split()
    check(ids == ["10001", "10001"], f"the container runs as uid/gid {ids}", "uid 10001, gid 10001")
    out = server.exec("citar", "--version", what="citar --version").strip()
    check(out == f"CITAR {version.text}", f"citar --version said {out!r}", out)
    test = server.engine.inspect("image", server.image)
    command = healthcheck_test(test)
    argv = command[1:] if command[0] == "CMD" else ["/bin/sh", "-c", command[1]]
    server.exec(*argv, what="the image's health check")
    say("ok: the image's health check exits 0")
    out = server.exec("python", "-P", "-c", smoke_wheel.EXTENSION_FACTS, what="ask the extension")
    library, test_ops = json.loads(out.strip().splitlines()[-1])
    check(library.startswith(f"{VENV}/lib/") and "/site-packages/citar/" in library,
          f"the extension is {library}, not the venv's", f"citar._engine from {library}")
    check(test_ops is False, "the image's engine has the test operations", "no test operations")
    facts = json.loads(server.exec("python", "-P", "-c", BUILD_FACTS, what="build_info()").strip().splitlines()[-1])
    build = facts["build"]
    check(build["version"] == version.text and build["label"] == label,
          f"build_info() gives {build['version']} and {build['label']!r}, not {version.text} and {label!r}",
          f"build_info(): {build['version']}, {build['label']}, build {build['build_id']}, jobs build "
          f"{facts['jobs_build']}")
    server.exec("citar", "doctor", "--quiet", what="citar doctor --quiet")
    say("ok: citar doctor --quiet exits 0")
    jobs = jobs_line(server.exec("citar", "doctor", what="citar doctor"))
    say(f"ok: citar doctor, as uid 10001: the job host is {jobs}")
    owner = server.exec("stat", "-c", "%u:%g:%a", STATE_DIR, what="the state directory's owner").strip()
    check(owner == STATE_OWNER, f"{STATE_DIR} is {owner}, not {STATE_OWNER}", f"{STATE_DIR} {owner}")
    frozen = server.exec("python", "-P", "-m", "pip", "freeze", "--exclude", "citar", "--disable-pip-version-check",
                         what="pip freeze").splitlines()
    pinned = None
    if constraints is not None:
        problems, notes = freeze_problems(constraints, frozen)
        for note in notes:
            say(f"note: {note}")
        check(not problems, "the image's dependencies are not the release's tested set:\n  " + "\n  ".join(problems),
              f"its {len([f for f in frozen if f.strip()])} distributions are the constraints' versions")
        pinned = len([c for c in constraints if c.strip() and not c.startswith("#")])
    return {"version": build["version"], "label": build["label"], "build_id": build["build_id"],
            "rules": build["rules"], "jobs_build": facts["jobs_build"], "meta": meta, "uid": 10001, "gid": 10001,
            "engine_library": library, "test_ops": test_ops, "doctor_jobs": jobs, "state_owner": owner,
            "dependencies": len([f for f in frozen if f.strip()]), "constraints": pinned}


def smoke(engine: Engine, prefix: str, args, version) -> dict:
    """smoke_wheel.py's inside checks in a container of their own: no network, the scripts read-only, the state in
    its /tmp, as the image's user."""
    name = f"{prefix}-smoke"
    _, out = engine("run", "--rm", "--name", name, "--network", "none", "-v", f"{SCRIPTS}:{CHECK_MOUNT}:ro",
                    "--entrypoint", "/bin/sh", args.image, "-c", smoke_script(), "smoke", version.text, args.label,
                    f"image-{args.arch}", Path(args.wheel).name, what="the wheels' smoke in the image",
                    timeout=SMOKE)
    print("".join(f"    {line}\n" for line in out.splitlines()), end="", flush=True)
    record = smoke_record(out)
    say(f"ok: the smoke in the image: result hash {record['result_hash']}")
    return record


def restart(engine: Engine, first: Server, second: Server, servers: list, version, label: str) -> dict:
    """An account and the secret key made by the first server are in the volume for a second container, started
    after the first was stopped and removed."""
    password = secrets.token_urlsafe(24)
    out = first.exec("citar", "admin", "create-user", "--handle", MARKER_HANDLE, "--email", MARKER_EMAIL,
                     "--password-stdin", what="make an account", stdin=password + "\n")
    check("Created" in out, f"citar admin create-user said {out.strip()!r}", f"made the account {MARKER_HANDLE}")
    key = first.exec("sha256sum", f"{STATE_DIR}/secret_key", what="the secret key's hash").split()[0]
    before = sorted(first.exec("ls", "-A", STATE_DIR, what="the state directory").split())
    stopped = first.stop()
    servers.remove(first)
    servers.append(second)
    seconds = second.start()
    users = second.exec("citar", "admin", "list-users", what="the accounts after the restart")
    check(re.search(rf"^{re.escape(MARKER_HANDLE)}\s", users, re.M) is not None,
          f"the account {MARKER_HANDLE} is gone after the restart:\n{users}", "the account survived the restart")
    again = second.exec("sha256sum", f"{STATE_DIR}/secret_key", what="the secret key's hash").split()[0]
    check(again == key, "the secret key changed with the restart", "the secret key survived the restart")
    after = sorted(second.exec("ls", "-A", STATE_DIR, what="the state directory").split())
    check(set(before) <= set(after), f"the state directory lost {sorted(set(before) - set(after))}",
          f"the state directory kept {', '.join(before)}")
    status, body = http_get(second.port, "/api/meta")
    check(status == 200 and json.loads(body).get("version") == version.text and json.loads(body).get("label") == label,
          f"/api/meta after the restart answered {status}: {body[:300]!r}", "the second server's /api/meta")
    stopped_again = second.stop()
    servers.remove(second)
    return {"first": stopped, "second": {**stopped_again, "health_seconds": round(seconds, 1)}, "state": before}


def cleanup(engine: Engine, prefix: str, servers: list, volume: str) -> None:
    """Removes every container and the volume this run made, whatever happened; a failure here is shown, and the
    leftovers check after it fails the run."""
    for name in [s.name for s in servers] + [f"{prefix}-smoke"]:
        try:
            engine("rm", "-f", name, what=f"remove {name}", ok=(0, 1, 125))
        except Failed as e:
            say(f"cleanup: {e}")
    try:
        engine("volume", "rm", "-f", volume, what=f"remove {volume}", ok=(0, 1, 125))
    except Failed as e:
        say(f"cleanup: {e}")


# ------------------------------------------------------------------------------------------------ save


def save(args) -> int:
    engine = Engine(args.engine)
    if shutil.which("zstd") is None:
        raise Refused("no zstd on PATH")
    if not args.out.name.endswith(".tar.zst"):
        raise Refused(f"--out {args.out}: name a .tar.zst file")
    if not engine.has_image(args.image):
        raise Refused(f"{engine.name} has no image {args.image}")
    report = None
    if args.report is not None:
        try:
            report = json.loads(args.report.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            raise Refused(f"--report {args.report}: {e}") from None
    before = image_id(str(engine.inspect("image", args.image).get("Id", "")))
    if report is not None:
        check(report.get("id") == before, f"the image is {before[:12]}, not the one the report tested "
              f"({str(report.get('id'))[:12]})", "the image is the one run tested")
    out = args.out.resolve()
    tar = out.with_name(out.name.removesuffix(".zst"))
    out.parent.mkdir(parents=True, exist_ok=True)
    for path in (out, tar):
        if path.exists():
            path.unlink()
    t0 = time.perf_counter()
    form = ["--format", "docker-archive"] if engine.name == "podman" else []
    engine("save", *form, "-o", tar, args.image, what=f"save {args.image}", timeout=SAVE)
    zstd(["-q", "-T0", "--rm", "-o", out, tar], "compress the image")
    size, digest = out.stat().st_size, sha256(out)
    say(f"ok: saved {out.name}, {size:,} bytes, sha256 {digest} ({time.perf_counter() - t0:.0f} s)")
    # By its id, every tag of it, so the load below cannot find the old copy under another name.
    engine("image", "rm", "-f", before, what=f"remove {args.image}")
    check(not engine.has_image(before) and not engine.has_image(args.image),
          f"{args.image} ({before[:12]}) is still there after its removal", f"{args.image} removed")
    zstd(["-q", "-d", "-o", tar, out], "decompress the image")
    try:
        engine("load", "-i", tar, what=f"load {tar.name}", timeout=SAVE)
    finally:
        tar.unlink(missing_ok=True)
    after = image_id(str(engine.inspect("image", args.image).get("Id", "")))
    check(after == before, f"the loaded image is {after}, the saved one {before}",
          f"loaded again as {args.image}, the same id {after[:12]}")
    if report is not None:
        report["saved"] = {"file": out.name, "bytes": size, "sha256": digest, "id": after}
        args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
        say(f"ok: {args.report} names the file")
    return 0


def zstd(argv: list, what: str) -> None:
    try:
        done = subprocess.run(["zstd", *map(str, argv)], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              timeout=SAVE)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Failed(f"{what}: {e}") from None
    if done.returncode != 0:
        raise Failed(f"{what}: zstd exited {done.returncode}: {done.stderr[-2000:]}")


# ------------------------------------------------------------------------------------------------ command line


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(prog="check_image.py", description=__doc__.splitlines()[0])
    sub = top.add_subparsers(dest="command", required=True)
    p = sub.add_parser("run", help="run the image and prove it")
    p.add_argument("--engine", required=True, help="docker or podman")
    p.add_argument("--image", required=True, help="the image, by its tag")
    p.add_argument("--arch", required=True, choices=ARCHES)
    p.add_argument("--version", required=True, help="the release's version, X.Y.Z[-rc.N]")
    p.add_argument("--label", required=True, help="the build label the image must report")
    p.add_argument("--wheel", required=True, help="the wheel the image was built from (its name is in the smoke line)")
    p.add_argument("--constraints", type=Path, help="the release's constraints the image must have installed")
    p.add_argument("--out", type=Path, required=True, help="the report")
    p = sub.add_parser("save", help="save the image, then load it again and require the same id")
    p.add_argument("--engine", required=True, help="docker or podman")
    p.add_argument("--image", required=True, help="the image, by its tag")
    p.add_argument("--out", type=Path, required=True, help="the file, FILE.tar.zst")
    p.add_argument("--report", type=Path, help="run's report, which gains the file's name, size and hash")
    return top


def main(argv=None) -> int:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="backslashreplace")
    args = parser().parse_args(argv)
    try:
        return {"run": run, "save": save}[args.command](args)
    except (Refused, smoke_wheel.Refused) as e:
        print(f"check_image {args.command}: refused: {e}", file=sys.stderr)
        return 2
    except (Failed, smoke_wheel.Failed) as e:
        print(f"check_image {args.command}: FAILED: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
