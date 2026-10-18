"""The CITAR helper installed as a service for real, on a CI runner (design/phase4-helper.md P4.3.9, P4.7.10; package
4-14's helper.yml): install, start, connect to a loopback fake server, stay up while refused, stop, uninstall.

    python scripts/ci/service_check.py BINARY --scope user|system

BINARY is a release build of the helper (helper.yml's artifact). The fake server is a WebSocket on 127.0.0.1 that
refuses every hello's token (`refused` ``token``, close 4401): a helper in service mode must then idle (one attempt an
hour), so the check also sees that no service manager restarts it. Per OS:

- **Linux** (``--scope user``): a systemd user unit, after ``loginctl enable-linger`` (the job does it); the unit's
  ``NRestarts`` stays 0 and its process the same while refused; afterwards no unit file and no unit listed.
- **Windows** (``--scope system``): the Windows service as ``NT SERVICE\\citar-helper`` (its account, its failure
  actions with the non-crash flag, the home's access list, the binary copied into the home's ``bin``); afterwards no
  service. (``--scope user``): the Run value and the Run supervisor; afterwards no value and no helper process.
- **macOS** (``--scope user``): the LaunchAgent; the plist passes ``plutil -lint``; loaded where the runner allows a GUI
  domain (where it does not, the check says so and passes on the lint alone).

It refuses to run anywhere but a GitHub Actions runner with ``CITAR_SERVICE_CHECK=1`` (exit 2): it installs a service
for real, which no developer's machine should get from a test (P4.7.1). Whatever happens, it uninstalls at the end.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

TOKEN = "cwt_ci_service_check_0123456789"
WINDOWS = os.name == "nt"
MACOS = sys.platform == "darwin"


class FakeHub:
    """A WebSocket server on 127.0.0.1 that refuses every hello, noting each one and when."""

    def __init__(self):
        self.hellos: list[tuple[float, dict]] = []
        self.port = 0
        ready = threading.Event()
        self.loop = asyncio.new_event_loop()

        async def handler(ws):
            try:
                hello = json.loads(await ws.recv())
            except Exception:
                return
            self.hellos.append((time.monotonic(), hello))
            await ws.send(json.dumps({"t": "refused", "ts": time.time(), "code": "token",
                                      "message": "CI service check: this token is refused on purpose"}))
            await ws.close(4401, "token")

        async def main():
            from websockets.asyncio.server import serve
            async with serve(handler, "127.0.0.1", 0) as server:
                self.port = server.sockets[0].getsockname()[1]
                ready.set()
                await asyncio.Future()

        threading.Thread(target=lambda: self.loop.run_until_complete(main()), daemon=True).start()
        if not ready.wait(30):
            raise SystemExit("the fake server did not start")


def run(*args: str, check: bool = False, timeout: float = 120) -> subprocess.CompletedProcess:
    got = subprocess.run(list(args), capture_output=True, text=True, timeout=timeout)
    print(f"$ {' '.join(args)}\n  exit {got.returncode}\n  {(got.stdout + got.stderr).strip()[-1500:]}", flush=True)
    if check and got.returncode != 0:
        raise SystemExit(f"FAILED: {' '.join(args)} exited {got.returncode}")
    return got


def wait_for(what: str, test, within: float, every: float = 0.5):
    until = time.monotonic() + within
    while time.monotonic() < until:
        got = test()
        if got:
            return got
        time.sleep(every)
    raise SystemExit(f"FAILED: {what} within {within:.0f} s")


def ok(what: str, condition: bool, detail: str = "") -> None:
    print(f"{'ok' if condition else 'FAILED'}: {what}{(' (' + detail + ')') if detail else ''}", flush=True)
    if not condition:
        raise SystemExit(1)


def service_pid(scope: str, home: Path) -> str:
    """The helper's process as the service manager knows it, and for systemd its restart count."""
    if WINDOWS and scope == "system":
        out = run("sc.exe", "queryex", "citar-helper").stdout
        m = re.search(r"PID\s*:\s*(\d+)", out)
        return m.group(1) if m else ""
    if WINDOWS:
        try:
            rec = json.loads((home / "helper" / "supervisor.json").read_text(encoding="utf-8"))
            return f"{rec.get('pid')}/{rec.get('child')}"
        except Exception:
            return ""
    if MACOS:
        out = run("launchctl", "print", f"gui/{os.getuid()}/org.citar.helper").stdout
        m = re.search(r"^\s*pid = (\d+)", out, re.M)
        return m.group(1) if m else ""
    out = run("systemctl", "--user", "show", "citar-helper.service", "-p", "MainPID", "-p", "NRestarts").stdout
    return " ".join(sorted(out.split()))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("binary")
    ap.add_argument("--scope", choices=["user", "system"], required=True)
    args = ap.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("CITAR_SERVICE_CHECK") != "1":
        print("refused: this installs a service for real; it runs on a CI runner with CITAR_SERVICE_CHECK=1 only",
              flush=True)
        return 2
    if not WINDOWS and not MACOS:
        # the user's service manager, which `loginctl enable-linger` started (the job does it), on its bus
        os.environ.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
        wait_for("the user's systemd manager", lambda: (Path(os.environ["XDG_RUNTIME_DIR"]) / "bus").exists(), 60)
    binary = str(Path(args.binary).resolve())
    if not WINDOWS:
        os.chmod(binary, 0o755)
    hub = FakeHub()
    if WINDOWS and args.scope == "system":
        home = Path(os.environ.get("ProgramData", r"C:\ProgramData")) / "CITAR" / "helper"
        home_args: list[str] = []
    else:
        home = Path(tempfile.mkdtemp(prefix="citar-service-check-"))
        home_args = ["--home", str(home)]
    home.mkdir(parents=True, exist_ok=True)
    (home / "helper.json").write_text(json.dumps({
        "server_url": f"http://127.0.0.1:{hub.port}", "token": TOKEN, "no_llm": True, "no_power": True,
        "updates": "off"}), encoding="utf-8")
    if not WINDOWS:
        os.chmod(home / "helper.json", 0o600)
    scope = f"--{args.scope}"
    run(binary, "--version", check=True)
    run(binary, "service", "install", scope, "--dry-run", *home_args, check=True)
    installed = False
    try:
        got = run(binary, "service", "install", scope, *home_args, timeout=300)
        if got.returncode != 0 and MACOS and "bootstrap" in (got.stdout + got.stderr):
            plist = Path.home() / "Library" / "LaunchAgents" / "org.citar.helper.plist"
            lint = run("plutil", "-lint", str(plist))
            ok("the LaunchAgent's plist passes plutil -lint", lint.returncode == 0)
            print("::notice::launchctl bootstrap was refused on this runner (no GUI domain): the plist was linted, "
                  "not loaded", flush=True)
            return 0
        ok("service install exits 0", got.returncode == 0, got.stderr.strip()[-300:])
        installed = True
        first = wait_for("the helper's hello at the fake server", lambda: hub.hellos[:1], 180)
        hello = first[0][1]
        ok("the helper says hello with protocol 2 and its token",
           hello.get("t") == "hello" and hello.get("protocol") == 2 and hello.get("token") == TOKEN,
           json.dumps({k: hello.get(k) for k in ("t", "protocol", "helper")})[:300])
        status = run(binary, "service", "status", scope, *home_args)
        ok("service status says it runs (exit 0)", status.returncode == 0 and "running" in status.stdout)
        before = wait_for("the service's process", lambda: service_pid(args.scope, home), 30)
        time.sleep(20)
        after = service_pid(args.scope, home)
        ok("refused, it idles: one hello, the same process, no restart", len(hub.hellos) == 1 and before == after,
           f"hellos {len(hub.hellos)}, before {before}, after {after}")
        if not WINDOWS and not MACOS:
            ok("systemd restarted it 0 times", "NRestarts=0" in after, after)
            unit = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "systemd" / "user" / \
                "citar-helper.service"
            text = unit.read_text(encoding="utf-8")
            ok("the unit carries its restart rules", all(r in text for r in (
                "Restart=on-failure", "RestartSec=15", "RestartPreventExitStatus=78", "run --service --home")))
        if MACOS:
            plist = Path.home() / "Library" / "LaunchAgents" / "org.citar.helper.plist"
            ok("the LaunchAgent's plist passes plutil -lint", run("plutil", "-lint", str(plist)).returncode == 0)
        if WINDOWS and args.scope == "system":
            qc = run("sc.exe", "qc", "citar-helper").stdout
            ok("the service runs as NT SERVICE\\citar-helper", "NT SERVICE\\citar-helper" in qc, qc[-300:])
            ok("its binary is the copy in the home's bin", str(home / "bin" / "citar-helper.exe") in qc)
            failure = run("sc.exe", "qfailure", "citar-helper").stdout
            ok("its failure actions restart it after 15 s", "RESTART -- Delay = 15000" in failure, failure[-300:])
            flag = run("sc.exe", "qfailureflag", "citar-helper").stdout
            ok("and count a non-zero exit (the non-crash flag)", "TRUE" in flag, flag[-200:])
            acl = run("icacls", str(home)).stdout
            ok("the home is SYSTEM's, Administrators' and the service's alone, nothing inherited",
               "NT SERVICE\\citar-helper" in acl and "BUILTIN\\Users" not in acl and "(I)" not in acl, acl[-500:])
            key = home / "helper" / "machine-key.json"
            ok("the service made its machine key in its home", key.is_file())
        if WINDOWS and args.scope == "user":
            value = run("reg.exe", "query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v",
                        "CITAR helper").stdout
            ok("the Run value starts run --detach with the home", "run --detach --home" in value
               and str(home) in value, value[-300:])
        got = run(binary, "service", "uninstall", scope, *home_args, timeout=120)
        ok("service uninstall exits 0", got.returncode == 0, got.stderr.strip()[-300:])
        installed = False
        if WINDOWS and args.scope == "system":
            ok("no citar-helper service afterwards", run("sc.exe", "query", "citar-helper").returncode == 1060)
        elif WINDOWS:
            gone = run("reg.exe", "query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "CITAR helper")
            ok("no Run value afterwards", gone.returncode != 0)
            procs = run("tasklist", "/FI", "IMAGENAME eq citar-helper*").stdout
            ok("no helper process afterwards", "citar-helper" not in procs.lower(), procs[-300:])
        elif MACOS:
            ok("no LaunchAgent afterwards", run("launchctl", "print", f"gui/{os.getuid()}/org.citar.helper").returncode
               != 0 and not (Path.home() / "Library" / "LaunchAgents" / "org.citar.helper.plist").exists())
        else:
            listed = run("systemctl", "--user", "list-unit-files", "citar*", "--no-legend").stdout.strip()
            ok("no citar unit afterwards", listed == "", listed)
        status = run(binary, "service", "status", scope, *home_args)
        ok("service status says it is not running (exit 3)", status.returncode == 3)
        print("OK: the service installed, ran, idled while refused, and was removed", flush=True)
        return 0
    finally:
        if installed:
            run(binary, "service", "uninstall", scope, *home_args, timeout=120)


if __name__ == "__main__":
    sys.exit(main())
