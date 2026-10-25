"""``deploy/citar-deploy.sh`` (design/phase5-release.md P5.13.3, package 5-10b): the deploy of a release bundle on a
server, its plans, its refusals, and the revision logic that decides whether a failed release's database is put back.

Everything runs under ``CITAR_DEPLOY_ROOT``, a temporary folder standing in for the server's ``/``, with stand-ins
first on PATH for ``systemctl`` (it starts a unit's ``ExecStart`` from the unit file under that root, runs the real
``citar-backup`` there, and stops what it started), ``systemd-run`` (a scope runs its command; a service runs it with
only the environment its properties give, logged) and ``id`` (the ``citar`` account), so the logic runs as an ordinary
user and touches nothing outside the folder. The releases are stand-in wheels (pure Python, no dependency) whose
``citar`` answers ``--version``, ``doctor`` and ``serve`` (a real SQLite database migrated to its own revision, refusing
one it does not know, as the server does, P5.0.9), installed by the script into real venvs, offline.

- **The plans** (gate 2): ``--plan`` of install, cutover, rollback (with and without ``--restore-backup``),
  uncutover and prune print exactly ``tests/data/deploy/*.txt`` (the port of the stand-in CITAR and the machine's
  architecture put in), with and without a slice, and change nothing.
- **The preflight's refusals** (gate 2): a bundle with a changed byte, a wheel of another platform, too little disk and
  another site failing are each refused before any change, naming why.
- **The revision logic** (gate 3): a failed health check after the revision moved restores the snapshot before the
  previous release starts, and with the revision unchanged does not; rollback across a revision the target does not
  know is refused without ``--restore-backup`` and names the deploy's backup, and with it restores; the staged
  verification's environment holds no live state path.
- One definition: the script's helpers and sandbox are citar-backup's, citar-admin's and the unit's.

Linux only, with bash, the sqlite3 command, GNU tar, flock and a Python that makes venvs, as the server has them (CI's
Linux legs and the package's WSL gate); the text checks run everywhere. Standard library only.
"""
from __future__ import annotations

import base64
import hashlib
import http.server
import json
import os
import platform
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEPLOY = ROOT / "deploy"
SCRIPT = DEPLOY / "citar-deploy.sh"
PLANS = ROOT / "tests" / "data" / "deploy"
ARCH = platform.machine()
#: Set to write tests/data/deploy/*.txt from this run's plans (then read every one before committing it).
BLESS = os.environ.get("CITAR_DEPLOY_BLESS") == "1"


def scratch(prefix: str) -> Path:
    """A temporary folder: under CARGO_TARGET_DIR when it is set (a package's own folder on the laptop), else the
    system's; resolved, as the script resolves what it is given."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base or None)).resolve()


def _gnu_tar() -> bool:
    try:
        return "GNU tar" in subprocess.run(["tar", "--version"], capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return False


def _can_make_venvs() -> bool:
    try:
        return subprocess.run([sys.executable, "-c", "import venv, ensurepip"], capture_output=True,
                              timeout=60).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


LINUX_TOOLS = (sys.platform.startswith("linux") and all(shutil.which(t) for t in ("bash", "sqlite3", "flock", "sha256sum"))
               and _gnu_tar())
RUNS = LINUX_TOOLS and _can_make_venvs()


def bash_env(**extra: str) -> dict[str, str]:
    """No CITAR_* variable and no shell option from outside (an exported SHELLOPTS reaches every bash below it)."""
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith(("CITAR_", "PIP_")) and k.upper() not in ("SHELLOPTS", "BASHOPTS")}
    env.update(extra)
    return env


# ------------------------------------------------------------------------------------------------ one definition


def _function(text: str, name: str) -> str:
    start = text.index(f"\n{name}() {{\n") + 1
    return text[start:text.index("\n}\n", start) + 2]


def _array(text: str, name: str) -> list[str]:
    body = text[text.index(f"\n{name}=(\n") + len(name) + 4:]
    body = body[:body.index("\n)\n")]
    return re.findall(r'-p "([^"]+)"', body)


class OneDefinition(unittest.TestCase):
    """The script shares citar-backup's reading of the env file, citar-admin's and the unit's sandbox, and the wizard's
    targets; text checks, on every platform."""

    def setUp(self):
        self.text = SCRIPT.read_text(encoding="utf-8")

    def test_env_get_and_at_are_citar_backup_s_own_lines(self):
        backup = (DEPLOY / "citar-backup.sh").read_text(encoding="utf-8")
        for name in ("env_get", "at"):
            self.assertEqual(_function(self.text, name), _function(backup, name), name)

    def test_the_verification_s_sandbox_is_the_unit_s_and_citar_admin_s(self):
        sandbox = _array(self.text, "SANDBOX")
        admin = _array((DEPLOY / "citar-admin.sh").read_text(encoding="utf-8"), "properties")
        own = ("User=", "Group=", "WorkingDirectory=", "Environment=", "EnvironmentFile=", "ReadWritePaths=")
        self.assertEqual(sorted(sandbox), sorted(p for p in admin if not p.startswith(own)))
        unit, section = {}, None
        for line in (DEPLOY / "citar.service").read_text(encoding="utf-8").splitlines():
            if line.startswith("["):
                section = line
            elif section == "[Service]" and "=" in line and not line.startswith("#"):
                unit[line.split("=", 1)[0]] = line.split("=", 1)[1]
        for prop in sandbox:
            key, value = prop.split("=", 1)
            self.assertEqual(unit.get(key), value, prop)
        for key in ("ProtectSystem", "ProtectHome", "PrivateTmp", "NoNewPrivileges", "SystemCallFilter"):
            self.assertIn(f"{key}={unit[key]}", sandbox)

    def test_the_files_it_installs_are_the_wizard_s_targets_and_its_own_two(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location("citar_wizard_units_for_deploy",
                                                      ROOT / "citar" / "wizard" / "units.py")
        units = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = units
        spec.loader.exec_module(units)
        body = self.text[self.text.index("\nFILES=(\n"):]
        body = body[:body.index("\n)\n")]
        table = {}
        for source, target, mode in re.findall(r'"deploy/([^|]+)\|([^|]+)\|(\d+)"', body):
            table[source] = (target.replace("$UNITS", "/etc/systemd/system"), mode)
        wizard = {name: (str(path), "0755" if name.endswith(".sh") else "0644") for name, path in units.TARGETS.items()}
        self.assertEqual({k: v for k, v in table.items() if k in wizard}, wizard)
        self.assertEqual(sorted(set(table) - set(wizard)), ["citar-deploy.sh", "citar-secrets.sh"])
        self.assertEqual(table["citar-deploy.sh"], ("/usr/local/sbin/citar-deploy", "0755"))
        self.assertEqual(table["citar-secrets.sh"], ("/usr/local/bin/citar-secrets", "0755"))

    def test_the_engine_check_is_the_wheels_smoke_s(self):
        smoke = (ROOT / "scripts" / "ci" / "smoke_wheel.py").read_text(encoding="utf-8")
        facts = re.search(r'^EXTENSION_FACTS = "([^"]+)"', smoke, re.M).group(1)
        self.assertIn(f'ENGINE_FACTS="{facts}"', self.text)

    def test_the_plans_are_pinned_to_lf(self):
        attributes = (ROOT / ".gitattributes").read_text(encoding="utf-8")
        self.assertIn("tests/data/deploy/*.txt text eol=lf", attributes)


# ------------------------------------------------------------------------------------------------ stand-ins

#: The stand-in CITAR's web side: /api/meta (its version, or none, as 0.1.5's), /api/auth/config, and a WebSocket
#: handshake on /ws/worker answered with its accept key (else 404, as the app answers a plain GET there).
FAKE_SERVER = r'''
import base64
import hashlib
import http.server
import json


def handler(version):
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            if self.path == "/ws/worker" and self.headers.get("Upgrade", "").lower() == "websocket":
                key = self.headers.get("Sec-WebSocket-Key", "") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
                self.send_response(101)
                self.send_header("Upgrade", "websocket")
                self.send_header("Connection", "Upgrade")
                self.send_header("Sec-WebSocket-Accept",
                                 base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
                self.end_headers()
                self.close_connection = True
                return
            if self.path == "/api/meta":
                body = {"mode": "server"} if version is None else {"version": version, "mode": "server"}
            elif self.path == "/api/auth/config":
                body = {"registration": "invite"}
            else:
                body = None
            data = json.dumps(body or {"detail": "Not Found"}).encode()
            self.send_response(200 if body else 404)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *args):
            pass

    return Handler
'''

#: The stand-in release's console script: this head (its version, revisions and faults put in), the web side, the
#: rest.
FAKE_CLI_HEAD = '''import json
import os
import sqlite3
import sys

VERSION = {version!r}
REVISIONS = {revisions!r}
BROKEN = {broken!r}
DOCTOR = {doctor!r}
'''
FAKE_CLI_TAIL = r'''

def log(line):
    if os.environ.get("FAKE_LOG"):
        with open(os.environ["FAKE_LOG"], "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def database():
    url = os.environ.get("CITAR_DB_URL", "")
    if url.startswith("sqlite:///"):
        return url[len("sqlite:///"):]
    return os.path.join(os.environ.get("CITAR_DATA_DIR") or os.environ.get("CITAR_STATE_DIR") or ".", "citar.db")


def main():
    args = sys.argv[1:]
    if args == ["--version"]:
        print("CITAR " + VERSION)
        return 0
    if args[:1] == ["doctor"]:
        log("doctor " + VERSION + " " + json.dumps({k: v for k, v in sorted(os.environ.items())
                                                     if k.startswith("CITAR_")}))
        print("CITAR " + VERSION + " - checking this installation")
        if DOCTOR == "fail":
            print("[ FAIL ] connection: planted")
            return 1
        if DOCTOR == "unverifiable":
            print("[ warn ] jobs: not verifiable: planted")
            return 0
        print("[  ok  ] jobs: verifiable: the job host passed its canary and self-check (planted)")
        return 0
    if args[:1] != ["serve"]:
        log("citar " + VERSION + ": " + " ".join(args))
        return 0
    path = database()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    db = sqlite3.connect(path)
    db.execute("CREATE TABLE IF NOT EXISTS alembic_version (version_num VARCHAR(32) NOT NULL PRIMARY KEY)")
    row = db.execute("SELECT version_num FROM alembic_version").fetchone()
    if row and row[0] not in REVISIONS:
        log("citar " + VERSION + ": refuses " + row[0])
        return 1
    if BROKEN == "before":
        log("citar " + VERSION + ": exits before migrating")
        return 1
    if not row or row[0] != REVISIONS[-1]:
        db.execute("DELETE FROM alembic_version")
        db.execute("INSERT INTO alembic_version VALUES (?)", (REVISIONS[-1],))
        db.commit()
    db.close()
    log("citar " + VERSION + ": at " + REVISIONS[-1] + ", " + ("exits after migrating" if BROKEN else "serving"))
    if BROKEN == "after":
        return 1
    http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["CITAR_PORT"])), handler(VERSION)).serve_forever()


if __name__ == "__main__":
    sys.exit(main())
'''


def fake_wheel(folder: Path, version: str, revisions: list[str], broken: str | None = None, doctor: str = "ok",
               test_ops: bool = False, arch: str = ARCH) -> Path:
    """A stand-in CITAR wheel for this machine (its name a release wheel's), whose migrations know ``revisions``."""
    info = f"citar-{version}.dist-info"
    files = {
        "citar/__init__.py": f"__version__ = {version!r}\n",
        "citar/cli.py": (FAKE_CLI_HEAD.format(version=version, revisions=revisions, broken=broken, doctor=doctor)
                         + FAKE_SERVER + FAKE_CLI_TAIL),
        "citar/_engine.py": f"HAS_TEST_OPS = {test_ops!r}\n",
        "citar/migrations/__init__.py": "",
        f"{info}/METADATA": f"Metadata-Version: 2.1\nName: citar\nVersion: {version}\nProvides-Extra: server\n",
        f"{info}/WHEEL": (f"Wheel-Version: 1.0\nGenerator: tests\nRoot-Is-Purelib: true\n"
                          f"Tag: cp311-abi3-manylinux_2_28_{arch}\n"),
        f"{info}/entry_points.txt": ("[console_scripts]\ncitar = citar.cli:main\ncitar-admin = citar.cli:main\n"
                                     "citar-mcp = citar.cli:main\n"),
    }
    for revision in revisions:
        files[f"citar/migrations/versions/{revision}.py"] = f'revision: str = "{revision}"\n'
    record = []
    for path, text in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(text.encode()).digest()).rstrip(b"=").decode()
        record.append(f"{path},sha256={digest},{len(text.encode())}")
    files[f"{info}/RECORD"] = "\n".join(record + [f"{info}/RECORD,,"]) + "\n"
    target = folder / f"citar-{version}-cp311-abi3-manylinux_2_28_{arch}.whl"
    with zipfile.ZipFile(target, "w") as wheel:
        for path, text in files.items():
            wheel.writestr(path, text)
    return target


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def make_bundle(folder: Path, wheel: Path, version: str) -> Path:
    """A deploy bundle as scripts/deploy/bundle.py writes one: the wheel, its constraints, deploy/ and env.example
    from this tree, BUNDLE, and SHA256SUMS over every other file."""
    folder.mkdir(parents=True)
    shutil.copy2(wheel, folder / wheel.name)
    (folder / f"citar-{version}-server-constraints.txt").write_text("# the release's tested set (stand-in)\n",
                                                                     encoding="utf-8", newline="\n")
    shutil.copytree(DEPLOY, folder / "deploy")
    shutil.copy2(ROOT / "env.example", folder / "env.example")
    (folder / "BUNDLE").write_text(f"version={version}\npep440={version}\nsource=tests\n", encoding="utf-8",
                                   newline="\n")
    rehash(folder)
    return folder


def rehash(folder: Path) -> None:
    lines = [f"{sha256(p)}  {p.relative_to(folder).as_posix()}" for p in sorted(folder.rglob("*"))
             if p.is_file() and p.name != "SHA256SUMS"]
    (folder / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


#: systemctl's stand-in: each call logged with where current points; `start` runs the unit's ExecStart (under the
#: root, with its environment file and the root put in front of its state paths), a oneshot in the foreground and a
#: service in the background; `stop` ends what it started; `show -p ExecMainStatus` and `is-active` answer from that.
STUB_SYSTEMCTL = r'''
import json
import os
import signal
import subprocess
import sys
import time

ROOT = os.environ["STUB_ROOT"]
STATE = os.environ["STUB_STATE"]
UNITS = ROOT + "/etc/systemd/system"
PATHS = ("CITAR_STATE_DIR", "CITAR_DATA_DIR", "CITAR_SAVE_DIR", "CITAR_CONFIG_DIR", "CITAR_BENCH_DIR")


def current():
    try:
        return os.readlink(ROOT + "/opt/citar/current")
    except OSError:
        return "none"


def log(line):
    with open(os.environ["STUB_LOG"], "a", encoding="utf-8") as handle:
        handle.write(line + "\n")


def unit(name):
    name = name if "." in name else name + ".service"
    values = {}
    try:
        with open(f"{UNITS}/{name}", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if "=" in line and not line.startswith(("#", ";", "[")):
                    key, _, value = line.partition("=")
                    values.setdefault(key, []).append(value)
    except OSError:
        return None
    return values


def rooted(value):
    return ROOT + value if value.startswith("/") and not value.startswith(ROOT) else value


def environment(values):
    env = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8"}
    env.update({k: v for k, v in os.environ.items() if k.startswith("FAKE_")})
    for assignment in values.get("Environment", []):
        key, _, value = assignment.partition("=")
        env[key] = value
    for path in values.get("EnvironmentFile", []):
        with open(rooted(path.lstrip("-")), encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line and not line.startswith(("#", ";")) and "=" in line:
                    key, _, value = line.partition("=")
                    env[key.strip()] = value.strip().strip('"')
    for key in PATHS:
        if key in env:
            env[key] = rooted(env[key])
    if env.get("CITAR_DB_URL", "").startswith("sqlite:////"):
        env["CITAR_DB_URL"] = "sqlite:///" + rooted(env["CITAR_DB_URL"][len("sqlite:///"):])
    return env


def alive(pid):
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
            return handle.read().split(") ", 1)[1][0] != "Z"
    except OSError:
        return False


def pid_of(name):
    try:
        with open(f"{STATE}/{name}.pid", encoding="utf-8") as handle:
            return int(handle.read())
    except (OSError, ValueError):
        return None


def main(args):
    log(f"systemctl {' '.join(args)} [current -> {current()}]")
    verb = args[0]
    name = args[-1].removesuffix(".service")
    if verb == "show":
        try:
            with open(f"{STATE}/{name}.status", encoding="utf-8") as handle:
                print(handle.read().strip())
        except OSError:
            print("0")
        return 0
    if verb == "is-active":
        pid = pid_of(name)
        print("active" if pid and alive(pid) else "inactive")
        return 0 if pid and alive(pid) else 3
    if verb == "cat":
        values = unit(name)
        return 0 if values is not None else 1
    if verb == "stop":
        pid = pid_of(name)
        if pid and alive(pid):
            os.killpg(pid, signal.SIGTERM)
            deadline = time.monotonic() + 10
            while alive(pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            if alive(pid):
                os.killpg(pid, signal.SIGKILL)
        if pid:
            os.remove(f"{STATE}/{name}.pid")
        return 0
    if verb != "start":
        return 0
    for name in [a.removesuffix(".service") for a in args[1:]]:
        if name == "citar-backup" and os.environ.get("STUB_BACKUP_FAIL"):
            status = 2
        else:
            values = unit(name)
            if values is None:
                log(f"  no unit {name}")
                return 5
            argv = values["ExecStart"][-1].split()
            argv[0] = rooted(argv[0])
            env = environment(values)
            cwd = rooted(values.get("WorkingDirectory", ["/"])[-1])
            if values.get("Type", ["simple"])[-1] == "oneshot":
                env["CITAR_DEPLOY_ROOT"] = ROOT
                with open(f"{STATE}/{name}.out", "a", encoding="utf-8") as out:
                    status = subprocess.run(["bash", *argv] if argv[0].endswith("citar-backup") else argv, env=env,
                                            cwd=cwd if os.path.isdir(cwd) else None, stdout=out,
                                            stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL).returncode
            else:
                if pid_of(name) and alive(pid_of(name)):
                    continue
                with open(f"{STATE}/{name}.out", "a", encoding="utf-8") as out:
                    child = subprocess.Popen(argv, env=env, cwd=cwd, stdout=out, stderr=subprocess.STDOUT,
                                             stdin=subprocess.DEVNULL, start_new_session=True)
                with open(f"{STATE}/{name}.pid", "w", encoding="utf-8") as handle:
                    handle.write(str(child.pid))
                continue
        with open(f"{STATE}/{name}.status", "w", encoding="utf-8") as handle:
            handle.write(str(status))
        if status != 0:
            return 1
    return 0


sys.exit(main(sys.argv[1:]))
'''

#: systemd-run's stand-in: every call logged as a JSON line; a scope runs its command as it is; a service runs it with
#: only what its properties give (its environment file and Environment=, the working directory), plus the tests'
#: FAKE_* variables, as systemd would start it.
STUB_SYSTEMD_RUN = r'''
import json
import os
import subprocess
import sys

args = sys.argv[1:]
flags, props, i = [], [], 0
while i < len(args) and args[i] != "--":
    if args[i] == "-p":
        props.append(args[i + 1])
        i += 2
    else:
        flags.append(args[i])
        i += 1
command = args[i + 1:]
entry = {"flags": flags, "properties": props, "command": command}
if "--scope" in flags:
    with open(os.environ["STUB_RUNS"], "a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry) + "\n")
    sys.exit(subprocess.run(command).returncode)
given = {}
for prop in props:
    key, _, value = prop.partition("=")
    if key == "EnvironmentFile":
        with open(value.lstrip("-"), encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line and not line.startswith(("#", ";")) and "=" in line:
                    k, _, v = line.partition("=")
                    given[k.strip()] = v.strip()
for prop in props:
    key, _, value = prop.partition("=")
    if key == "Environment":
        k, _, v = value.partition("=")
        given[k] = v
entry["environment"] = given
entry["directory"] = dict(p.split("=", 1) for p in props).get("WorkingDirectory")
with open(os.environ["STUB_RUNS"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(entry) + "\n")
env = {"PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C.UTF-8"}
env.update({k: v for k, v in os.environ.items() if k.startswith("FAKE_")})
env.update(given)
sys.exit(subprocess.run(command, env=env, cwd=entry["directory"]).returncode)
'''


class Sites:
    """Stand-ins for this box's other sites (answering 200, or ``status``) and, when ``version`` is given, for a
    running CITAR (its port in the env file, its site name http://citar.test:<port>)."""

    def __init__(self):
        self.servers = []

    def start(self, handler_class) -> int:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler_class)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.servers.append(server)
        return server.server_address[1]

    def site(self, status: int = 200) -> str:
        class Site(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(status)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"ok")

            def log_message(self, *args):
                pass

        return f"http://127.0.0.1:{self.start(Site)}/"

    def citar(self, version: str | None) -> int:
        namespace: dict = {}
        exec(FAKE_SERVER, namespace)
        return self.start(namespace["handler"](version))

    def close(self):
        for server in self.servers:
            server.shutdown()
            server.server_close()


#: The 0.1.5 layout's units and stand-ins (its server, lab and backup), as the VPS has them.
OLD_SERVICE = """[Unit]
Description=CITAR 0.1.5
[Service]
Type=exec
User=citar
WorkingDirectory=/opt/citar
EnvironmentFile=/etc/citar/citar.env
ExecStart=/opt/citar/.venv/bin/python -m citar.server
"""
OLD_LAB = """[Unit]
Description=CITAR lab runner
[Service]
Type=exec
WorkingDirectory=/opt/citar
EnvironmentFile=/etc/citar/citar.env
ExecStart=/opt/citar/.venv/bin/python -m citar.lab
"""
OLD_BACKUP_SERVICE = """[Unit]
Description=Back up the CITAR database, configuration and saves
[Service]
Type=oneshot
User=root
ExecStart=/usr/local/bin/citar-backup
"""
OLD_BACKUP = """#!/usr/bin/env bash
# 0.1.5's backup, standing in: a folder with the database and saves.
out="$CITAR_DEPLOY_ROOT/var/backups/citar/$(date -u +%Y%m%d-%H%M%S)"
mkdir -p "$out" && cp -a "$CITAR_DEPLOY_ROOT/var/lib/citar/citar.db" "$out/" 2>/dev/null
echo "backup complete: $out"
"""
OLD_SERVER = "import os\n" + FAKE_SERVER + """
import http.server
http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["CITAR_PORT"])), handler(None)).serve_forever()
"""


class Box:
    """A server under a temporary root: the env file, the other sites' list, the stand-ins on PATH, and helpers to
    lay out releases, 0.1.5's tree and backups, and to run citar-deploy."""

    def __init__(self, case: unittest.TestCase):
        self.case = case
        self.tmp = scratch("citar-deploy-")
        case.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.root = self.tmp / "root"
        self.stub = self.tmp / "stub-bin"
        self.state = self.tmp / "stub-state"
        self.log = self.tmp / "calls.log"
        self.runs = self.tmp / "runs.jsonl"
        self.fake_log = self.tmp / "fake.log"
        for folder in (self.root / "etc" / "citar", self.root / "var" / "lib" / "citar", self.stub, self.state,
                       self.root / "etc" / "systemd" / "system", self.root / "usr" / "local" / "bin",
                       self.root / "usr" / "local" / "sbin",
                       self.root / "home" / "deploy"):
            folder.mkdir(parents=True, exist_ok=True)
        for path in (self.log, self.runs, self.fake_log):
            path.write_text("", encoding="utf-8")
        self.sites = Sites()
        case.addCleanup(self.sites.close)
        case.addCleanup(self.stop_all)
        self.write_stub("systemctl", f"#!{sys.executable}\n" + STUB_SYSTEMCTL)
        self.write_stub("systemd-run", f"#!{sys.executable}\n" + STUB_SYSTEMD_RUN)
        self.write_stub("id", '#!/bin/sh\nif [ "$1" = -u ] && [ "$2" = citar ]; then echo 999; exit 0; fi\n'
                              'if [ "$1" = citar ]; then echo "uid=999(citar) gid=999(citar)"; exit 0; fi\n'
                              'exec /usr/bin/id "$@"\n')
        self.write_stub("python3.12", f'#!/bin/sh\nexec "{sys.executable}" "$@"\n')
        self.port = 0
        #: The runs wait 8 s for a release that does not come up; a plan shows the real 60 s.
        self.extra_env: dict[str, str] = {"CITAR_DEPLOY_HEALTH_SECONDS": "8"}

    def write_stub(self, name: str, text: str):
        (self.stub / name).write_text(text, encoding="utf-8", newline="\n")
        (self.stub / name).chmod(0o755)

    def stop_all(self):
        for pid_file in self.state.glob("*.pid"):
            try:
                os.killpg(int(pid_file.read_text()), 9)
            except (OSError, ValueError):
                pass

    # -------------------------------------------------------------------------------------------- the configuration

    def configure(self, port: int, sites: list[str], extra: str = ""):
        self.port = port
        (self.root / "etc" / "citar" / "citar.env").write_text(
            f"CITAR_MODE=server\nCITAR_PUBLIC_ORIGIN=http://citar.test:{port}\nCITAR_SECRET_KEY=not-a-real-one\n"
            f"CITAR_HOST=127.0.0.1\nCITAR_PORT={port}\nCITAR_STATE_DIR=/var/lib/citar\n{extra}", encoding="utf-8")
        (self.root / "etc" / "citar" / "deploy-checks.conf").write_text(
            "# the other sites\n" + "".join(f"{url}\n" for url in sites), encoding="utf-8")

    def install_units(self, older_backup: bool = False):
        """deploy/'s units and scripts where citar-deploy installs them (with ``older_backup`` the backup script one
        line different, so a plan shows it replaced)."""
        units = self.root / "etc" / "systemd" / "system"
        for name in ("citar.service", "citar-backup.service", "citar-backup.timer"):
            shutil.copy2(DEPLOY / name, units / name)
        bin_ = self.root / "usr" / "local" / "bin"
        for name, target in (("citar-admin.sh", "citar-admin"), ("citar-backup.sh", "citar-backup"),
                             ("citar-secrets.sh", "citar-secrets")):
            shutil.copy2(DEPLOY / name, bin_ / target)
        if older_backup:
            path = bin_ / "citar-backup"
            path.write_text(path.read_text(encoding="utf-8").replace("KEEP_DAYS:-14", "KEEP_DAYS:-7"),
                            encoding="utf-8", newline="\n")

    def release(self, version: str, revisions: list[str], venv: bool = False, current: bool = False, **kw):
        """releases/<version>: a real venv with the stand-in wheel installed (``venv``), or only its migrations (what a
        plan reads); marked complete."""
        folder = self.root / "opt" / "citar" / "releases" / version
        folder.mkdir(parents=True, exist_ok=True)
        if venv:
            wheel = fake_wheel(self.tmp, version, revisions, **kw)
            subprocess.run([sys.executable, "-m", "venv", str(folder / "venv")], check=True, timeout=300)
            subprocess.run([str(folder / "venv" / "bin" / "pip"), "install", "--quiet", "--no-index",
                            "--disable-pip-version-check", str(wheel)], check=True, timeout=300)
        else:
            versions = folder / "venv" / "lib" / "python3.12" / "site-packages" / "citar" / "migrations" / "versions"
            versions.mkdir(parents=True, exist_ok=True)
            for revision in revisions:
                (versions / f"{revision}.py").write_text(f'revision: str = "{revision}"\n', encoding="utf-8")
        (folder / ".complete").write_text(f"verified by citar-deploy: CITAR {version}\n", encoding="utf-8")
        if current:
            link = self.root / "opt" / "citar" / "current"
            if link.is_symlink():
                link.unlink()
            os.symlink(f"releases/{version}", link)

    def database(self, revision: str | None):
        db = sqlite3.connect(self.root / "var" / "lib" / "citar" / "citar.db")
        try:
            db.execute("CREATE TABLE IF NOT EXISTS alembic_version (version_num VARCHAR(32) NOT NULL PRIMARY KEY)")
            db.execute("CREATE TABLE IF NOT EXISTS users (handle TEXT)")
            db.execute("DELETE FROM alembic_version")
            if revision:
                db.execute("INSERT INTO alembic_version VALUES (?)", (revision,))
            db.commit()
        finally:
            db.close()

    def revision(self) -> str | None:
        db = sqlite3.connect(self.root / "var" / "lib" / "citar" / "citar.db")
        try:
            row = db.execute("SELECT version_num FROM alembic_version").fetchone()
            return row[0] if row else None
        finally:
            db.close()

    def old_layout(self, lab: bool = True):
        """0.1.5 as the VPS has it: its tree and venv in /opt/citar, its units, its backup and its state."""
        opt = self.root / "opt" / "citar"
        (opt / "citar").mkdir(parents=True)
        (opt / "citar" / "__init__.py").write_text('__version__ = "0.1.5"\n', encoding="utf-8")
        (opt / "citar" / "server.py").write_text(OLD_SERVER, encoding="utf-8")
        (opt / "citar" / "lab.py").write_text("import time\ntime.sleep(3600)\n", encoding="utf-8")
        (opt / ".venv" / "bin").mkdir(parents=True)
        os.symlink(sys.executable, opt / ".venv" / "bin" / "python")
        units = self.root / "etc" / "systemd" / "system"
        (units / "citar.service").write_text(OLD_SERVICE, encoding="utf-8")
        (units / "citar-backup.service").write_text(OLD_BACKUP_SERVICE, encoding="utf-8")
        shutil.copy2(DEPLOY / "citar-backup.timer", units / "citar-backup.timer")
        if lab:
            (units / "citar-lab.service").write_text(OLD_LAB, encoding="utf-8")
        bin_ = self.root / "usr" / "local" / "bin"
        (bin_ / "citar-backup").write_text(OLD_BACKUP, encoding="utf-8", newline="\n")
        (bin_ / "citar-backup").chmod(0o755)
        (bin_ / "citar-admin").write_text("#!/bin/sh\n# 0.1.5's hand-made wrapper\n", encoding="utf-8")
        (bin_ / "citar-admin").chmod(0o755)
        (self.root / "var" / "lib" / "citar" / "saves").mkdir(parents=True, exist_ok=True)
        (self.root / "var" / "lib" / "citar" / "saves" / "old-game.citar").write_text("0.1.5's\n", encoding="utf-8")

    # -------------------------------------------------------------------------------------------- running it

    def env(self, slice_: str | None = None, **extra: str) -> dict[str, str]:
        env = bash_env(PATH=f"{self.stub}{os.pathsep}{os.environ.get('PATH', '')}", CITAR_DEPLOY_ROOT=str(self.root),
                       STUB_ROOT=str(self.root), STUB_STATE=str(self.state),
                       STUB_LOG=str(self.log), STUB_RUNS=str(self.runs), FAKE_LOG=str(self.fake_log),
                       PIP_NO_INDEX="1", PIP_DISABLE_PIP_VERSION_CHECK="1", TMPDIR=str(self.tmp))
        if slice_:
            env["CITAR_DEPLOY_SLICE"] = slice_
        env.update(self.extra_env)
        env.update(extra)
        return env

    def deploy(self, *args: str, slice_: str | None = None, **extra: str) -> tuple[int, str, str]:
        done = subprocess.run(["bash", str(SCRIPT), *args], env=self.env(slice_, **extra), capture_output=True,
                              text=True, timeout=900, stdin=subprocess.DEVNULL)
        return done.returncode, done.stdout, done.stderr

    def systemctl(self, *args: str):
        subprocess.run([str(self.stub / "systemctl"), *args], env=self.env(), timeout=60, check=True)

    def wait_serving(self, says: str, seconds: float = 20):
        deadline = time.monotonic() + seconds
        while says not in self.fake_log.read_text(encoding="utf-8"):
            self.case.assertLess(time.monotonic(), deadline, f"no '{says}' in the stand-in's log")
            time.sleep(0.1)

    def calls(self) -> list[str]:
        return [line for line in self.log.read_text(encoding="utf-8").splitlines()
                if not line.startswith(("systemctl show", "systemctl is-active"))]

    def tree(self) -> list[str]:
        """Every path under the root, with each file's SHA-256 and each link's target: what "nothing changed" means."""
        out = []
        for path in sorted(self.root.rglob("*")):
            rel = path.relative_to(self.root).as_posix()
            if "site-packages" in rel or "__pycache__" in rel:
                continue
            if path.is_symlink():
                out.append(f"{rel} -> {os.readlink(path)}")
            elif path.is_file():
                out.append(f"{rel} {sha256(path)[:12]}")
            else:
                out.append(rel + "/")
        return out

    def bundle(self, version: str, revisions: list[str], **kw) -> Path:
        wheel = fake_wheel(self.tmp, version, revisions, **kw)
        return make_bundle(self.root / "home" / "deploy" / f"citar-deploy-{version}", wheel, version)


def plan_text(name: str, out: str, port: int) -> None:
    """The plan against tests/data/deploy/<name>.txt (its @PORT@ and @ARCH@ put in), or written there with BLESS."""
    path = PLANS / f"{name}.txt"
    if BLESS:
        PLANS.mkdir(parents=True, exist_ok=True)
        path.write_text(out.replace(str(port), "@PORT@").replace(ARCH, "@ARCH@"), encoding="utf-8", newline="\n")
        return
    want = path.read_text(encoding="utf-8").replace("@PORT@", str(port)).replace("@ARCH@", ARCH)
    if out != want:
        import difflib

        raise AssertionError(f"the plan differs from {path.relative_to(ROOT)}:\n" + "".join(
            difflib.unified_diff(want.splitlines(True), out.splitlines(True), "expected", "printed")))


# ------------------------------------------------------------------------------------------------ the plans


@unittest.skipUnless(LINUX_TOOLS, "Linux with bash, the sqlite3 command, flock, sha256sum and GNU tar")
class Plans(unittest.TestCase):
    """Gate 2: each command's --plan against its committed text, with and without a slice; nothing changes."""

    def setUp(self):
        self.box = Box(self)
        self.box.extra_env.clear()

    def running(self, version: str = "0.1.6", meta: str | None = "0.1.6") -> int:
        """A running CITAR (a stand-in answering as ``meta``), the other sites, and the env file naming them."""
        port = self.box.sites.citar(meta)
        self.box.configure(port, [self.box.sites.site(), self.box.sites.site()])
        return port

    def plan(self, name: str, *args: str, slices: bool = True, port: int):
        before = self.box.tree()
        code, out, err = self.box.deploy(*args, "--plan")
        self.assertEqual(code, 0, out + err)
        plan_text(name, out, port)
        if slices:
            code, sliced, err = self.box.deploy(*args, "--plan", slice_="citar-staging.slice")
            self.assertEqual(code, 0, sliced + err)
            plan_text(f"{name}-slice", sliced, port)
            self.assertEqual(sliced.replace(" --slice=citar-staging.slice", ""), out,
                             "the slice changes the systemd-run lines alone")
        else:
            code, sliced, err = self.box.deploy(*args, "--plan", slice_="citar-staging.slice")
            self.assertEqual((code, sliced), (0, out), "this command starts no systemd-run")
        self.assertEqual(self.box.tree(), before, "the plan changed something")
        self.assertEqual([c for c in self.box.calls() if not c.startswith("systemctl cat")], [],
                         "the plan ran systemctl")
        self.assertEqual(self.box.runs.read_text(encoding="utf-8"), "", "the plan ran systemd-run")

    def test_install(self):
        port = self.running()
        self.box.install_units(older_backup=True)
        self.box.release("0.1.5", ["r1"])
        self.box.release("0.1.6", ["r1", "r2"], current=True)
        self.box.database("r2")
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        self.plan("install", "install", "--bundle", str(bundle), port=port)

    def test_cutover(self):
        port = self.running(meta=None)
        self.box.old_layout()
        self.box.database(None)
        with open(self.box.root / "etc" / "citar" / "citar.env", "a", encoding="utf-8") as env:
            env.write("CITAR_LAB_WORKERS=1\n")
        bundle = self.box.bundle("0.1.6", ["r1", "r2"])
        self.plan("cutover", "cutover", "--bundle", str(bundle), port=port)

    def test_rollback(self):
        port = self.running("0.1.7", "0.1.7")
        self.box.release("0.1.6", ["r1", "r2"])
        self.box.release("0.1.7", ["r1", "r2", "r3"], current=True)
        self.box.database("r2")
        self.plan("rollback", "rollback", slices=False, port=port)

    def test_rollback_with_a_backup(self):
        port = self.running("0.1.7", "0.1.7")
        self.box.release("0.1.6", ["r1", "r2"])
        self.box.release("0.1.7", ["r1", "r2", "r3"], current=True)
        self.box.database("r3")
        backup = self.box.root / "var" / "backups" / "citar" / "20261101-120000"
        backup.mkdir(parents=True)
        shutil.copy2(self.box.root / "var" / "lib" / "citar" / "citar.db", backup / "citar.db")
        db = sqlite3.connect(backup / "citar.db")
        db.execute("UPDATE alembic_version SET version_num = 'r2'")
        db.commit()
        db.close()
        subprocess.run(["tar", "-czf", str(backup / "state.tar.gz"), "-C", str(self.box.root / "var" / "lib" / "citar"),
                        "."], check=True)
        (backup / "citar.env").write_text("", encoding="utf-8")
        (backup / "RELEASE").write_text(
            "made=20261101-120000\nrelease=releases/0.1.6\nversion=0.1.6\nstate=/var/lib/citar\ndata=/var/lib/citar\n"
            "saves=/var/lib/citar/saves\nconfig=/var/lib/citar/config\nbenchmarks=/var/lib/citar/benchmarks\n"
            "archives=state.tar.gz\ndatabase=/var/lib/citar/citar.db\nrevision=r2\n", encoding="utf-8")
        self.plan("rollback-restore", "rollback", "--to", "0.1.6", "--restore-backup",
                  "/var/backups/citar/20261101-120000", slices=False, port=port)

    def test_uncutover(self):
        port = self.running("0.1.6", "0.1.6")
        self.box.release("0.1.6", ["r1", "r2"], current=True)
        (self.box.root / "opt" / "citar-0.1.5" / "citar").mkdir(parents=True)
        (self.box.root / "var" / "lib" / "citar-archive-20261101-120000").mkdir(parents=True)
        record = self.box.root / "var" / "lib" / "citar-deploy" / "cutover"
        (record / "files" / "etc" / "systemd" / "system").mkdir(parents=True)
        for name in ("citar.service", "citar-lab.service", "citar-backup.service"):
            (record / "files" / "etc" / "systemd" / "system" / name).write_text("[Service]\n", encoding="utf-8")
        (record / "CUTOVER").write_text(
            "made=20261101-120000\nold_version=0.1.5\nold_tree=/opt/citar-0.1.5\n"
            "state_archive=/var/lib/citar-archive-20261101-120000\nbackup=/var/backups/citar/20261101-115959\nlab=1\n"
            "release=0.1.6\nsaved=/etc/systemd/system/citar.service /etc/systemd/system/citar-lab.service "
            "/etc/systemd/system/citar-backup.service\nabsent=/usr/local/bin/citar-secrets "
            "/usr/local/sbin/citar-deploy\n", encoding="utf-8")
        self.plan("uncutover", "uncutover", slices=False, port=port)

    def test_prune(self):
        port = self.running("0.1.7", "0.1.7")
        for version in ("0.1.3", "0.1.4", "0.1.5", "0.1.6"):
            self.box.release(version, ["r1"])
        self.box.release("0.1.7", ["r1"], current=True)
        self.plan("prune", "prune", slices=False, port=port)


# ------------------------------------------------------------------------------------------------ refusals


@unittest.skipUnless(LINUX_TOOLS, "Linux with bash, the sqlite3 command, flock, sha256sum and GNU tar")
class Refusals(unittest.TestCase):
    """Gate 2's second half: each is refused before any change, naming why, in the plan and in the run alike."""

    def setUp(self):
        self.box = Box(self)
        self.port = self.box.sites.citar("0.1.6")
        self.box.configure(self.port, [self.box.sites.site()])
        self.box.install_units()
        self.box.release("0.1.6", ["r1", "r2"], current=True)
        self.box.database("r2")

    def refused(self, bundle: Path, says: str, **env: str):
        before = self.box.tree()
        for plan in (("--plan",), ()):
            code, out, err = self.box.deploy("install", "--bundle", str(bundle), *plan, **env)
            self.assertEqual(code, 1, out + err)
            self.assertIn(says, err)
            self.assertIn("Nothing was changed.", err)
            self.assertNotIn("1. Stage", out)
        self.assertEqual(self.box.tree(), before)
        self.assertEqual([c for c in self.box.calls() if not c.startswith("systemctl cat")], [])
        self.assertEqual(self.box.runs.read_text(encoding="utf-8"), "")

    def test_a_bundle_with_a_changed_byte(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        wheel = next(bundle.glob("*.whl"))
        data = bytearray(wheel.read_bytes())
        data[100] ^= 1
        wheel.write_bytes(bytes(data))
        self.refused(bundle, f"The bundle does not match its SHA256SUMS: {wheel.name}: its SHA-256 is {sha256(wheel)}")

    def test_a_file_the_sums_do_not_list(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        (bundle / "deploy" / "planted.sh").write_text("echo planted\n", encoding="utf-8")
        self.refused(bundle, "deploy/planted.sh is in the bundle and not in SHA256SUMS")

    def test_a_wheel_of_another_platform(self):
        other = "aarch64" if ARCH != "aarch64" else "x86_64"
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"], arch=other)
        self.refused(bundle, f"citar-0.1.7-cp311-abi3-manylinux_2_28_{other}.whl is not for this machine ({ARCH}, "
                             f"glibc")

    def test_too_little_disk(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        self.box.write_stub("df", "#!/bin/sh\nprintf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n"
                                  "/dev/stand-in 1000000 897600 102400 90%% /\\n'\n")
        self.refused(bundle, "Too little disk under /opt/citar: 100 MB free, and a deploy needs 1536 MB")

    def test_another_site_failing(self):
        failing = self.box.sites.site(503)
        self.box.configure(self.port, [self.box.sites.site(), failing])
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        self.refused(bundle, f"Another site on this box does not answer 200, and a box already in trouble is not "
                             f"deployed to:\n{failing} answers 503")

    def test_an_older_release_the_running_one_and_one_staged_already(self):
        self.refused(self.box.bundle("0.1.5", ["r1"]),
                     "CITAR 0.1.5 is older than the release that runs (0.1.6): going back is citar-deploy rollback")
        self.refused(self.box.bundle("0.1.6", ["r1", "r2"]), "CITAR 0.1.6 is the release that runs")
        (self.box.root / "opt" / "citar" / "releases" / "0.1.7").mkdir()
        self.refused(self.box.bundle("0.1.7", ["r1", "r2", "r3"]),
                     "/opt/citar/releases/0.1.7 is here already (an earlier install of it, or one that failed): "
                     "--reinstall removes it")

    def test_citar_not_answering(self):
        self.box.configure(self.box.sites.citar("0.1.5"), [self.box.sites.site()])
        self.refused(self.box.bundle("0.1.7", ["r1", "r2", "r3"]), "CITAR does not answer as it should before the "
                                                                   "deploy")

    @unittest.skipUnless(shutil.which("git") and (ROOT / ".git").exists(), "a git checkout (bundle.py's git archive)")
    def test_a_bundle_bundle_py_made_passes_the_checks(self):
        """scripts/deploy/bundle.py --from-dir's bundle, unpacked on the box, is one citar-deploy accepts."""
        assets = self.box.tmp / "assets"
        assets.mkdir()
        fake_wheel(assets, "0.1.7", ["r1", "r2", "r3"])
        (assets / "citar-0.1.7-server-constraints.txt").write_text("# stand-in\n", encoding="utf-8")
        made = subprocess.run([sys.executable, str(ROOT / "scripts" / "deploy" / "bundle.py"), "0.1.7", "--arch", ARCH,
                               "--from-dir", str(assets), "--ref", "HEAD", "--out", str(self.box.tmp / "out")],
                              capture_output=True, text=True, timeout=300)
        self.assertEqual(made.returncode, 0, made.stdout + made.stderr)
        home = self.box.root / "home" / "deploy"
        subprocess.run(["tar", "-xzf", str(self.box.tmp / "out" / "citar-deploy-0.1.7.tar.gz"), "-C", str(home)],
                       check=True)
        code, out, err = self.box.deploy("install", "--bundle", str(home / "citar-deploy-0.1.7"), "--plan")
        self.assertEqual(code, 0, out + err)
        self.assertIn("ok  bundle: /home/deploy/citar-deploy-0.1.7, CITAR 0.1.7: every file as its SHA256SUMS says",
                      out)
        self.assertIn(f"ok  wheel: citar-0.1.7-cp311-abi3-manylinux_2_28_{ARCH}.whl, for this machine", out)

    def test_real_system_tools_under_the_test_root(self):
        code, out, err = self.box.deploy("install", "--bundle", "/nowhere", "--plan", PATH=os.environ["PATH"])
        self.assertEqual(code, 1, out + err)
        self.assertIn("CITAR_DEPLOY_ROOT is for tests: put stand-ins for systemctl and systemd-run first on PATH", err)


# ------------------------------------------------------------------------------------------------ the runs


@unittest.skipUnless(RUNS, "Linux with bash, the sqlite3 command, flock, sha256sum, GNU tar and a Python that makes "
                           "venvs")
class Install(unittest.TestCase):
    """Gate 3, under CITAR_DEPLOY_ROOT with the stand-ins and a real SQLite database: the install's switch, and what a
    failed release's undo does with the database."""

    def setUp(self):
        self.box = Box(self)
        self.site = self.box.sites.site()
        port = _free_port()
        self.box.configure(port, [self.site])
        self.box.install_units(older_backup=True)
        self.box.release("0.1.6", ["r1", "r2"], venv=True, current=True)
        self.box.database("r2")
        self.box.systemctl("start", "citar")
        self.box.wait_serving("citar 0.1.6: at r2, serving")
        self.box.log.write_text("", encoding="utf-8")

    def backups(self) -> list[Path]:
        return sorted(p for p in (self.box.root / "var" / "backups" / "citar").iterdir() if p.is_dir())

    def test_an_install_that_migrates_switches_with_the_service_stopped_after_a_backup(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        code, out, err = self.box.deploy("install", "--bundle", str(bundle), slice_="citar-staging.slice")
        self.assertEqual(code, 0, out + err)
        self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.7")
        self.assertEqual(self.box.revision(), "r3")
        self.assertEqual(self.box.calls(), [
            "systemctl stop citar [current -> releases/0.1.6]",
            "systemctl start citar-backup [current -> releases/0.1.6]",
            "systemctl daemon-reload [current -> releases/0.1.7]",
            "systemctl start citar [current -> releases/0.1.7]",
        ])
        [backup] = self.backups()
        release = dict(line.split("=", 1) for line in (backup / "RELEASE").read_text().splitlines())
        self.assertEqual((release["version"], release["revision"]), ("0.1.6", "r2"),
                         "the backup is of 0.1.6's database, taken with it stopped, before the switch")
        rel = self.box.root / "opt" / "citar" / "releases" / "0.1.7"
        self.assertEqual(sorted(p.name for p in rel.iterdir()),
                         [".complete", "SHA256SUMS", "citar-0.1.7-cp311-abi3-manylinux_2_28_" + ARCH + ".whl",
                          "citar-0.1.7-server-constraints.txt", "freeze.txt", "venv"])
        self.assertEqual((rel / ".complete").read_text(), "verified by citar-deploy: CITAR 0.1.7\n")
        check = subprocess.run(["sha256sum", "-c", "SHA256SUMS"], cwd=rel, capture_output=True, text=True)
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
        # The pip install ran in its scope, in the slice; the verification in the slice as citar, with the sandbox.
        runs = [json.loads(line) for line in self.box.runs.read_text(encoding="utf-8").splitlines()]
        scope, *verify = runs
        self.assertEqual(scope["flags"], ["--scope", "--quiet", "--slice=citar-staging.slice"])
        self.assertEqual(scope["properties"], ["MemoryMax=500M", "CPUWeight=20"])
        self.assertEqual(scope["command"][1:4], ["install", "--disable-pip-version-check", "--no-cache-dir"])
        self.assertIn("--only-binary=:all:", scope["command"])
        self.assertEqual([r["command"][1:] for r in verify],
                         [["--version"], ["-P", "-c", "import json, citar._engine as E; "
                                                      "print(json.dumps([E.__file__, E.HAS_TEST_OPS]))"], ["doctor"]])
        for run in verify:
            self.assertIn("--slice=citar-staging.slice", run["flags"])
            self.assertIn("User=citar", run["properties"])
            self.assertIn("ProtectSystem=strict", run["properties"])
        # The units and scripts from the bundle: the changed backup script replaced, citar-deploy itself added.
        self.assertEqual((self.box.root / "usr" / "local" / "bin" / "citar-backup").read_bytes(),
                         (DEPLOY / "citar-backup.sh").read_bytes())
        self.assertEqual((self.box.root / "usr" / "local" / "sbin" / "citar-deploy").read_bytes(), SCRIPT.read_bytes())
        self.assertIn("/usr/local/bin/citar-backup <- the bundle's deploy/citar-backup.sh (0755): it differs", out)
        self.assertIn("-KEEP_DAYS=\"${CITAR_BACKUP_KEEP_DAYS:-7}\"", out)
        log = (self.box.root / "var" / "log" / "citar-deploy.log").read_text()
        self.assertRegex(log, rf"install version=0.1.7 previous=0.1.6 result=ok revision_before=r2 revision_after=r3 "
                              rf"backup=/var/backups/citar/{backup.name} downtime=\d+s\n")
        self.assertIn("CITAR 0.1.7 runs. 0.1.6 stays in /opt/citar/releases/0.1.6", out)
        self.assertFalse(list((self.box.root / "var" / "lib" / "citar-deploy").glob("verify.*")),
                         "the verification's folder was left")

    def test_a_failed_release_that_migrated_is_undone_with_the_database_put_back_first(self):
        """The revision moved: the snapshot is restored before 0.1.6 starts, so 0.1.6 starts (it refuses r3)."""
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"], broken="after")
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 1, out + err)
        self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.6")
        self.assertEqual(self.box.revision(), "r2", "the database 0.1.7 migrated was not put back")
        fake = self.box.fake_log.read_text(encoding="utf-8")
        self.assertIn("citar 0.1.7: at r3, exits after migrating", fake)
        self.assertNotIn("citar 0.1.6: refuses r3", fake, "0.1.6 started before the database was put back")
        self.assertTrue(fake.rstrip().endswith("citar 0.1.6: at r2, serving"), fake)
        [backup] = self.backups()
        self.assertIn(f"The database was restored: yes, from /var/backups/citar/{backup.name}/citar.db (CITAR 0.1.7 "
                      f"had moved its revision from r2 to r3)", err)
        self.assertIn("citar runs 0.1.6 again", err)
        self.assertEqual(self.box.calls(), [
            "systemctl stop citar [current -> releases/0.1.6]",
            "systemctl start citar-backup [current -> releases/0.1.6]",
            "systemctl daemon-reload [current -> releases/0.1.7]",
            "systemctl start citar [current -> releases/0.1.7]",
            "systemctl stop citar [current -> releases/0.1.7]",
            "systemctl daemon-reload [current -> releases/0.1.6]",
            "systemctl start citar [current -> releases/0.1.6]",
        ])
        # The files the switch replaced are put back, and the one it added removed.
        self.assertIn("KEEP_DAYS:-7", (self.box.root / "usr" / "local" / "bin" / "citar-backup").read_text())
        self.assertFalse((self.box.root / "usr" / "local" / "sbin" / "citar-deploy").exists())
        log = (self.box.root / "var" / "log" / "citar-deploy.log").read_text()
        self.assertIn("install version=0.1.7 previous=0.1.6 result=undone revision_before=r2 revision_after=r3", log)
        self.assertIn("restored=yes", log)

    def test_a_failed_release_that_did_not_migrate_is_undone_without_a_restore(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2"], broken="before")
        db = self.box.root / "var" / "lib" / "citar" / "citar.db"
        db_conn = sqlite3.connect(db)
        db_conn.execute("INSERT INTO users VALUES ('written by 0.1.6 after the backup')")
        db_conn.commit()
        db_conn.close()
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 1, out + err)
        self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.6")
        self.assertIn("The database was restored: no (its revision is still r2)", err)
        self.assertIn("citar 0.1.7: exits before migrating", self.box.fake_log.read_text(encoding="utf-8"))
        db_conn = sqlite3.connect(db)
        self.assertEqual(db_conn.execute("SELECT count(*) FROM users").fetchone(), (1,),
                         "the database was replaced although its revision never moved")
        db_conn.close()

    def test_the_verification_s_environment_holds_no_live_state_path(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        with open(self.box.root / "etc" / "citar" / "citar.env", "a", encoding="utf-8") as env:
            env.write("CITAR_DATA_DIR=/var/lib/citar\nCITAR_DB_URL=sqlite:////var/lib/citar/citar.db\n"
                      "CITAR_SAVE_DIR=/var/lib/citar/saves\nCITAR_KEYS_FILE=/var/lib/citar/keys.enc\n")
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 0, out + err)
        live = [str(self.box.root / "var" / "lib" / "citar"), "/var/lib/citar"]
        verify = [json.loads(line) for line in self.box.runs.read_text(encoding="utf-8").splitlines()][1:]
        self.assertEqual(len(verify), 3)
        for run in verify:
            folder = run["directory"]
            self.assertRegex(folder, re.escape(str(self.box.root)) + r"/var/lib/citar-deploy/verify\.\w+$")
            for key, value in run["environment"].items():
                for path in live:
                    self.assertFalse(value == path or value.startswith(path + "/") or path + "/" in value,
                                     f"{key}={value} names the live state")
            env = run["environment"]
            self.assertEqual(env["CITAR_STATE_DIR"], f"{folder}/state")
            self.assertEqual(env["CITAR_DATA_DIR"], f"{folder}/state")
            self.assertEqual(env["CITAR_DB_URL"], f"sqlite:///{folder}/state/citar.db")
            for key in ("CITAR_SAVE_DIR", "CITAR_CONFIG_DIR", "CITAR_BENCH_DIR", "CITAR_KEYS_FILE"):
                self.assertTrue(env[key].startswith(f"{folder}/state/"), key)
            self.assertEqual((env["CITAR_MODE"], env["CITAR_SECRET_KEY"]), ("server", "not-a-real-one"),
                             "the env file's other settings are kept")
            self.assertIn(f"ReadWritePaths={folder}", run["properties"])
        doctor = [line for line in self.box.fake_log.read_text(encoding="utf-8").splitlines()
                  if line.startswith("doctor 0.1.7 ")]
        self.assertEqual(len(doctor), 1)
        seen = json.loads(doctor[0].split(" ", 2)[2])
        self.assertTrue(all("/var/lib/citar/" not in v and not v.endswith("/var/lib/citar") for v in seen.values()),
                        seen)

    def test_a_verification_that_fails_changes_nothing_that_runs(self):
        for doctor, says in (("fail", "[ FAIL ] connection: planted"), ("unverifiable", "jobs: not verifiable")):
            with self.subTest(doctor):
                bundle = self.box.bundle(f"0.1.{7 if doctor == 'fail' else 8}", ["r1", "r2"], doctor=doctor)
                code, out, err = self.box.deploy("install", "--bundle", str(bundle))
                self.assertEqual(code, 1, out + err)
                self.assertIn(says, out)
                self.assertIn("failed its verification (above), so nothing that runs was changed", err)
                self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.6")
                self.assertEqual(self.box.calls(), [])
        bundle = self.box.bundle("0.1.9", ["r1", "r2"], test_ops=True)
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 1, out + err)
        self.assertIn("or with the test operations", out)
        self.assertFalse((self.box.root / "opt" / "citar" / "releases" / "0.1.9" / ".complete").exists())

    def test_rollback_across_a_revision_the_target_does_not_know(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2", "r3"])
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 0, out + err)
        [backup] = self.backups()
        saves = self.box.root / "var" / "lib" / "citar" / "saves"
        saves.mkdir(exist_ok=True)
        (saves / "made-by-0.1.7.citar").write_text("0.1.7's\n", encoding="utf-8")
        self.box.log.write_text("", encoding="utf-8")
        before = self.box.tree()
        code, out, err = self.box.deploy("rollback")
        self.assertEqual(code, 1, out + err)
        self.assertIn("The database is at revision r3, which CITAR 0.1.6 does not know: CITAR 0.1.7 migrated it", err)
        self.assertIn(f"sudo citar-deploy rollback --to 0.1.6 --restore-backup /var/backups/citar/{backup.name}", err)
        self.assertEqual(self.box.tree(), before)
        self.assertEqual(self.box.calls(), [])
        # With the deploy's backup: stopped, the state and the database restored, switched, started.
        code, out, err = self.box.deploy("rollback", "--restore-backup", f"/var/backups/citar/{backup.name}")
        self.assertEqual(code, 0, out + err)
        self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.6")
        self.assertEqual(self.box.revision(), "r2")
        self.assertFalse((saves / "made-by-0.1.7.citar").exists(), "the state was not restored")
        aside = list((self.box.root / "var" / "lib").glob("citar.before-restore-*"))
        self.assertEqual(len(aside), 1)
        self.assertTrue((aside[0] / "saves" / "made-by-0.1.7.citar").exists(), "the state moved aside was not kept")
        self.assertEqual(self.box.calls(), ["systemctl stop citar [current -> releases/0.1.7]",
                                            "systemctl start citar [current -> releases/0.1.6]"])
        self.assertTrue(self.box.fake_log.read_text(encoding="utf-8").rstrip().endswith("citar 0.1.6: at r2, serving"))
        log = (self.box.root / "var" / "log" / "citar-deploy.log").read_text()
        self.assertIn(f"rollback version=0.1.6 previous=0.1.7 result=ok revision_before=r3 "
                      f"restored=/var/backups/citar/{backup.name}", log)

    def test_rollback_without_a_migration_and_prune(self):
        bundle = self.box.bundle("0.1.7", ["r1", "r2"])
        code, out, err = self.box.deploy("install", "--bundle", str(bundle))
        self.assertEqual(code, 0, out + err)
        self.assertEqual(self.box.revision(), "r2")
        code, out, err = self.box.deploy("rollback")
        self.assertEqual(code, 0, out + err)
        self.assertEqual(os.readlink(self.box.root / "opt" / "citar" / "current"), "releases/0.1.6")
        self.assertIn("revision: r2, which CITAR 0.1.6 knows", out)
        code, out, err = self.box.deploy("status")
        self.assertEqual(code, 0, out + err)
        self.assertIn("release     0.1.6 (complete, runs)", out)
        self.assertIn("release     0.1.7 (complete)", out)
        self.assertIn("rollback version=0.1.6 previous=0.1.7", out)
        for version in ("0.1.3", "0.1.4"):
            self.box.release(version, ["r1"])
        code, out, err = self.box.deploy("prune", "--keep", "2")
        self.assertEqual(code, 0, out + err)
        self.assertEqual(sorted(p.name for p in (self.box.root / "opt" / "citar" / "releases").iterdir()),
                         ["0.1.4", "0.1.6"], "the release that runs and the one before it are kept, and no more")


@unittest.skipUnless(RUNS, "Linux with bash, the sqlite3 command, flock, sha256sum, GNU tar and a Python that makes "
                           "venvs")
class Cutover(unittest.TestCase):
    """The first deploy, from 0.1.5's layout, and its undoing: by hand, and by itself when the new release fails."""

    def setUp(self):
        self.box = Box(self)
        self.site = self.box.sites.site()
        self.box.configure(_free_port(), [self.site], extra="CITAR_LAB_WORKERS=1\n")
        self.box.old_layout()
        self.box.database(None)
        self.box.systemctl("start", "citar", "citar-lab")
        self.box.log.write_text("", encoding="utf-8")
        deadline = time.monotonic() + 20
        while subprocess.run(["bash", "-c", f"exec 3<>/dev/tcp/127.0.0.1/{self.box.port}"],
                             capture_output=True).returncode != 0:
            self.assertLess(time.monotonic(), deadline, "0.1.5's stand-in did not start")
            time.sleep(0.1)
        self.before = self.box.tree()

    #: What a cutover and its undoing leave beside 0.1.5, on purpose: the new layout and state moved aside, the
    #: deploy's record and log, the backups, and the bundle it came from.
    BESIDE = ("opt/citar-0.1.6-undone-", "var/lib/citar-0.1.6-undone-", "var/lib/citar-deploy", "var/log/",
              "var/backups/", "home/")

    def assertBackAsBefore(self):
        def kept(tree: list[str]) -> list[str]:
            return [line for line in tree if not line.startswith(self.BESIDE)]

        self.assertEqual(kept(self.box.tree()), kept(self.before), "0.1.5 is not back exactly as it was")

    def test_a_cutover_and_its_undoing(self):
        bundle = self.box.bundle("0.1.6", ["r1", "r2"])
        code, out, err = self.box.deploy("cutover", "--bundle", str(bundle))
        self.assertEqual(code, 0, out + err)
        root = self.box.root
        self.assertEqual(os.readlink(root / "opt" / "citar" / "current"), "releases/0.1.6")
        self.assertTrue((root / "opt" / "citar-0.1.5" / "citar" / "__init__.py").exists())
        [archive] = (root / "var" / "lib").glob("citar-archive-*")
        self.assertTrue((archive / "saves" / "old-game.citar").exists())
        self.assertEqual(self.box.revision(), "r2", "0.1.6 made its database on the fresh state directory")
        self.assertFalse((root / "var" / "lib" / "citar" / "saves" / "old-game.citar").exists())
        self.assertFalse((root / "etc" / "systemd" / "system" / "citar-lab.service").exists())
        self.assertEqual((root / "etc" / "systemd" / "system" / "citar.service").read_bytes(),
                         (DEPLOY / "citar.service").read_bytes())
        self.assertIn("settings: /etc/citar/citar.env sets CITAR_LAB_WORKERS, which neither env.example\n      nor CITAR "
                      "0.1.6's code names", out)
        self.assertEqual(self.box.calls(), [
            "systemctl stop citar [current -> none]",
            "systemctl stop citar-lab [current -> none]",
            "systemctl start citar-backup [current -> none]",
            "systemctl disable citar-lab [current -> none]",
            "systemctl daemon-reload [current -> releases/0.1.6]",
            "systemctl enable citar citar-backup.timer [current -> releases/0.1.6]",
            "systemctl start citar [current -> releases/0.1.6]",
        ])
        record = dict(line.split("=", 1) for line in
                      (root / "var" / "lib" / "citar-deploy" / "cutover" / "CUTOVER").read_text().splitlines())
        self.assertEqual(record["old_tree"], "/opt/citar-0.1.5")
        self.assertEqual(record["state_archive"], f"/var/lib/{archive.name}")
        self.assertEqual(record["absent"], "/usr/local/bin/citar-secrets /usr/local/sbin/citar-deploy")
        self.assertIn("cutover version=0.1.6 previous=0.1.5 result=ok",
                      (root / "var" / "log" / "citar-deploy.log").read_text())

        self.box.log.write_text("", encoding="utf-8")
        code, out, err = self.box.deploy("uncutover")
        self.assertEqual(code, 0, out + err)
        self.assertIn("CITAR 0.1.5 runs again from /opt/citar.", out)
        self.assertBackAsBefore()
        self.assertTrue(list((root / "opt").glob("citar-0.1.6-undone-*")), "the new layout was not kept aside")
        self.assertEqual(self.box.calls()[-3:], ["systemctl enable citar-lab [current -> none]",
                                                 "systemctl start citar [current -> none]",
                                                 "systemctl start citar-lab [current -> none]"])

    def test_a_cutover_whose_release_fails_undoes_itself(self):
        bundle = self.box.bundle("0.1.6", ["r1", "r2"], broken="after")
        code, out, err = self.box.deploy("cutover", "--bundle", str(bundle))
        self.assertEqual(code, 1, out + err)
        self.assertIn("The cutover to CITAR 0.1.6 failed (CITAR 0.1.6 did not answer as it should", err)
        self.assertIn("and was undone: CITAR 0.1.5 runs again from /opt/citar", err)
        self.assertBackAsBefore()
        self.assertEqual(self.box.calls()[-2:], ["systemctl start citar [current -> none]",
                                                 "systemctl start citar-lab [current -> none]"])

    def test_a_cutover_whose_verification_fails_undoes_itself(self):
        bundle = self.box.bundle("0.1.6", ["r1", "r2"], doctor="fail")
        code, out, err = self.box.deploy("cutover", "--bundle", str(bundle))
        self.assertEqual(code, 1, out + err)
        self.assertIn("failed (CITAR 0.1.6 failed its verification (above)), and was undone", err)
        self.assertNotIn("systemctl start citar [current -> releases/0.1.6]", self.box.calls())
        self.assertBackAsBefore()
        self.assertTrue((self.box.root / "opt" / "citar" / "citar" / "__init__.py").exists())


def _free_port() -> int:
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


if __name__ == "__main__":
    unittest.main()
