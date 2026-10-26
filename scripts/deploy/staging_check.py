"""The staging run's probes, stand-ins, checks and measurements (design/phase5-release.md P5.14). Standard library only.

``scripts/deploy/staging.sh`` runs this as root inside WSL (from its copy in ``/opt/citar-staging``), never on a real
server and never on Windows:

    staging_check.py hog --mb N                          allocate and touch N MiB (the OOM probe's load)
    staging_check.py burn --seconds S --cgroup DIR       a busy loop; prints DIR's CPU use over it against the wall time
    staging_check.py ballast --mb N                      allocate, touch and keep N MiB (the other sites' memory)
    staging_check.py standin --port P --work-ms W        a loopback site doing W ms of CPU a request (php-fpm's stand-in)
    staging_check.py sites FILE                          every URL in FILE answers 200
    staging_check.py health URL [--site URL] [--version V] [--seconds S] [--handshake]
    staging_check.py watch --out FILE [--slice S]        the run's timeline: memory, OOM kills, scopes, CITAR's answers
    staging_check.py measure --out FILE ...              the first administrator, the host's settings, idle, the game,
                                                         the lab at each CPU weight, the stand-in's response times
    staging_check.py drill --url URL --handle H          the restore drill's second CITAR: signs in, reads its accounts
    staging_check.py report --logs DIR                   the run's figures, from the timeline and the measurements

Each check prints one line, ``  ok  ...`` or ``FAILED: ...``; the exit status is 0 when every check passed, 1 when one
failed, 2 for a usage error. Every server it talks to is on 127.0.0.1.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import math
import os
import re
import secrets
import signal
import socket
import subprocess
import sys
import threading
import time
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

CGROUP = Path("/sys/fs/cgroup")
SLICE = "citar-staging.slice"
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
PAGE = 4096
MIB = 1 << 20

FAILURES: list[str] = []


def ok(text: str) -> None:
    print(f"  ok  {text}", flush=True)


def check(cond: bool, text: str, detail: str = "") -> bool:
    """One check's line; a failed one is remembered for the exit status."""
    if cond:
        ok(text + (f" ({detail})" if detail else ""))
    else:
        FAILURES.append(text)
        print(f"FAILED: {text}" + (f": {detail}" if detail else ""), flush=True)
    return cond


def note(text: str) -> None:
    """A figure that is measured and recorded, not held to a bound."""
    print(f"  --  {text}", flush=True)


def done() -> int:
    if FAILURES:
        print(f"{len(FAILURES)} check(s) failed: " + "; ".join(FAILURES), flush=True)
        return 1
    return 0


# ----------------------------------------------------------------------------------------------------- systemd, cgroups

def notify(state: str) -> None:
    """sd_notify(3) without libsystemd: one datagram to $NOTIFY_SOCKET, when systemd gave one."""
    where = os.environ.get("NOTIFY_SOCKET")
    if not where:
        return
    if where.startswith("@"):
        where = "\0" + where[1:]
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as s:
        s.connect(where)
        s.send(state.encode())


def read_int(path: Path) -> int | None:
    try:
        text = path.read_text().strip()
    except OSError:
        return None
    return None if text == "max" else int(text)


def events(path: Path) -> dict:
    """A cgroup's flat keyed file (memory.events, cpu.stat) as {key: int}."""
    out = {}
    try:
        for line in path.read_text().splitlines():
            key, _, value = line.partition(" ")
            if value.strip().lstrip("-").isdigit():
                out[key] = int(value)
    except OSError:
        pass
    return out


def cgroup_of(unit: str) -> Path:
    """A unit's cgroup, as systemd names it (``ControlGroup``). A dash in a slice's name nests it (systemd.slice(5)):
    citar-staging.slice is /citar.slice/citar-staging.slice, and a unit in it is below that."""
    got = subprocess.run(["systemctl", "show", "-p", "ControlGroup", "--value", unit], capture_output=True, text=True,
                         check=False).stdout.strip()
    if got:
        return CGROUP / got.lstrip("/")
    if unit.endswith(".slice"):
        parts = unit[:-len(".slice")].split("-")
        return CGROUP.joinpath(*("-".join(parts[:i + 1]) + ".slice" for i in range(len(parts))))
    return cgroup_of(SLICE) / unit


def systemd_props(unit: str, *names: str) -> dict:
    """``systemctl show`` of a unit's properties, as {name: value}."""
    got = subprocess.run(["systemctl", "show", unit, *(f"-p{n}" for n in names)], capture_output=True, text=True,
                         check=False)
    out = {}
    for line in got.stdout.splitlines():
        key, _, value = line.partition("=")
        out[key] = value
    return out


def as_mb(value) -> float | None:
    try:
        n = int(value)
    except (TypeError, ValueError):
        return None
    if n >= (1 << 62):          # systemd's "[not set]" infinity
        return None
    return round(n / MIB, 1)


class Peak:
    """A cgroup's memory peak over a span: ``memory.peak`` reset through a file descriptor of its own (Linux 6.12 and
    later: a write resets the peak this descriptor reads), else the highest ``memory.current`` sampled."""

    def __init__(self, cgroup: Path):
        self.cgroup = cgroup
        self.fd = None
        self.sampled = 0
        try:
            self.fd = os.open(cgroup / "memory.peak", os.O_RDWR)
            os.write(self.fd, b"reset\n")
            self.how = "memory.peak, reset for the span"
        except OSError:
            if self.fd is not None:
                os.close(self.fd)
            self.fd = None
            self.how = "memory.current, sampled"

    def sample(self) -> None:
        now = read_int(self.cgroup / "memory.current") or 0
        self.sampled = max(self.sampled, now)

    def mb(self) -> float:
        if self.fd is not None:
            peak = int(os.pread(self.fd, 64, 0).decode().strip() or 0)
            return round(max(peak, self.sampled) / MIB, 1)
        return round(self.sampled / MIB, 1)

    def close(self) -> None:
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def vm_hwm_mb(pid) -> float | None:
    """A process's resident high-water mark (VmHWM), in MB."""
    try:
        for line in Path(f"/proc/{int(pid)}/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return round(int(line.split()[1]) / 1024, 1)
    except (OSError, ValueError, TypeError):
        return None
    return None


# ----------------------------------------------------------------------------------------------------- probes

def cmd_hog(args) -> int:
    """Allocate and touch --mb MiB, then hold it: under a smaller MemoryMax without swap the kernel kills this."""
    block = bytearray(args.mb * MIB)
    for i in range(0, len(block), PAGE):
        block[i] = 1
    print(f"hog: {args.mb} MiB allocated and touched, and not killed", flush=True)
    time.sleep(args.hold)
    return 0


def cmd_burn(args) -> int:
    """A busy loop for --seconds of wall time; prints the cgroup's CPU use over it (cpu.stat's usage_usec) and the
    wall time, as JSON."""
    stat = Path(args.cgroup) / "cpu.stat"
    before, t0 = events(stat).get("usage_usec"), time.monotonic()
    end = t0 + args.seconds
    n = 0
    while time.monotonic() < end:
        n += 1
    after, wall = events(stat).get("usage_usec"), time.monotonic() - t0
    if before is None or after is None:
        print(json.dumps({"error": f"no usage_usec in {stat}"}))
        return 1
    print(json.dumps({"usage_usec": after - before, "wall_usec": round(wall * 1e6), "loops": n}), flush=True)
    return 0


def cmd_ballast(args) -> int:
    """The other sites' resident memory: --mb MiB allocated and touched, READY=1 to systemd, then read through again
    every --every seconds (a site's memory is in use, not merely allocated, so it is not the first thing reclaimed)."""
    block = bytearray(args.mb * MIB)
    for i in range(0, len(block), PAGE):
        block[i] = 1
    notify("READY=1\nSTATUS=holding %d MiB" % args.mb)
    print(f"ballast: holding {args.mb} MiB", flush=True)
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    while not stop.wait(args.every):
        total = 0
        for i in range(0, len(block), PAGE):
            total += block[i]
    return 0


def calibrate(work_ms: float) -> int:
    """How many rounds of the stand-in's work take ``work_ms`` of CPU time on this machine: the fastest of seven
    batches of about 100 ms each, so a batch slowed by something else (a frequency step, another process on the core)
    does not make every request lighter than asked."""
    data, best = b"citar-staging" * 8, 0.0
    rounds = 1000
    for _ in range(7):
        t0 = time.process_time()
        for _ in range(rounds):
            data = hashlib.sha256(data).digest()
        took_ms = max(1e-6, (time.process_time() - t0) * 1000)
        best = max(best, rounds / took_ms)
        rounds = max(1000, round(best * 100))
    return max(1, round(best * work_ms))


def cmd_standin(args) -> int:
    """A loopback HTTP site that does a fixed amount of CPU work per request (php-fpm's stand-in): it answers 200 once
    the work is done. The amount is set at its start, as --work-ms of CPU time, while nothing else runs in the slice."""
    rounds = calibrate(args.work_ms)

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            data = self.path.encode()
            for _ in range(rounds):
                data = hashlib.sha256(data).digest()
            body = f"stand-in site: {args.work_ms} ms of work, {data.hex()[:8]}\n".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    notify(f"READY=1\nSTATUS={rounds} rounds a request ({args.work_ms} ms of CPU)")
    print(f"stand-in: 127.0.0.1:{args.port}, {rounds} rounds a request ({args.work_ms} ms of CPU)", flush=True)
    signal.signal(signal.SIGTERM, lambda *_: threading.Thread(target=server.shutdown, daemon=True).start())
    server.serve_forever(poll_interval=0.5)
    return 0


# ----------------------------------------------------------------------------------------------------- HTTP

def connection(url: str, timeout: float):
    """An HTTP connection to ``url``'s host and port, always at 127.0.0.1 (the staging sites are loopback ones; a name
    is only the Host header), and that Host header."""
    parts = urlsplit(url)
    if parts.scheme != "http" or not parts.hostname:
        raise ValueError(f"{url}: staging speaks plain http to 127.0.0.1")
    return http.client.HTTPConnection("127.0.0.1", parts.port or 80, timeout=timeout), parts.netloc


def fetch(url: str, path: str = "", *, method: str = "GET", body=None, headers=None, timeout: float = 30.0):
    """(status, headers, body bytes) of one request."""
    conn, host = connection(url, timeout)
    parts = urlsplit(url)
    target = path or (parts.path or "/") + (f"?{parts.query}" if parts.query else "")
    data = None
    sent = {"Host": host, "User-Agent": "citar-staging", "Connection": "close", **(headers or {})}
    if body is not None:
        data = json.dumps(body).encode()
        sent["Content-Type"] = "application/json"
    try:
        conn.request(method, target, body=data, headers=sent)
        response = conn.getresponse()
        return response.status, response.getheaders(), response.read()
    finally:
        conn.close()


def handshake(url: str) -> str | None:
    """A WebSocket handshake on /ws/worker through ``url``: None when CITAR answers it (101 with this handshake's
    accept key), else what happened (a proxy that drops the Upgrade hands the app a plain GET: 404)."""
    nonce = base64.b64encode(os.urandom(16)).decode()
    conn, host = connection(url, 10)
    try:
        conn.putrequest("GET", "/ws/worker", skip_host=True, skip_accept_encoding=True)
        for name, value in (("Host", host), ("Upgrade", "websocket"), ("Connection", "Upgrade"),
                            ("Sec-WebSocket-Key", nonce), ("Sec-WebSocket-Version", "13")):
            conn.putheader(name, value)
        conn.endheaders()
        response = conn.getresponse()
        want = base64.b64encode(hashlib.sha1((nonce + WS_GUID).encode()).digest()).decode()
        if response.status != 101:
            return f"answers {response.status}, not 101"
        if response.getheader("Sec-WebSocket-Accept") != want:
            return "answers 101 without this handshake's accept key"
        return None
    finally:
        conn.close()


class Session:
    """A signed-in administrator's calls to CITAR: the session cookie and its CSRF token sent by hand (the cookie is
    Secure, for the https origin, and staging speaks http to 127.0.0.1)."""

    def __init__(self, url: str):
        self.url = url.rstrip("/")
        self.cookie = ""
        self.csrf = ""

    def call(self, method: str, path: str, body=None, timeout: float = 60.0):
        headers = {}
        if self.cookie:
            headers["Cookie"] = f"citar_session={self.cookie}"
        if self.csrf and method not in ("GET", "HEAD"):
            headers["x-citar-csrf"] = self.csrf
        status, got, raw = fetch(self.url, path, method=method, body=body, headers=headers, timeout=timeout)
        for name, value in got:
            if name.lower() == "set-cookie":
                jar = SimpleCookie()
                jar.load(value)
                if "citar_session" in jar:
                    self.cookie = jar["citar_session"].value
        try:
            data = json.loads(raw) if raw else None
        except ValueError:
            data = raw.decode("utf-8", "replace")
        return status, data

    def login(self, handle: str, password: str) -> bool:
        status, data = self.call("POST", "/api/auth/login", {"identifier": handle, "password": password})
        if status == 200 and isinstance(data, dict):
            self.csrf = data.get("csrf_token") or ""
            return bool(self.cookie)
        print(f"sign-in as {handle}: {status} {str(data)[:300]}", flush=True)
        return False


def wait_until(predicate, seconds: float, every: float = 0.5):
    """Calls ``predicate`` until it returns something true or ``seconds`` pass; returns its last value."""
    deadline = time.monotonic() + seconds
    while True:
        value = predicate()
        if value or time.monotonic() >= deadline:
            return value
        time.sleep(every)


def health_once(url: str, site: str | None, version: str | None, want_ws: bool) -> str | None:
    try:
        status, _, _ = fetch(url, "/api/auth/config", timeout=5)
        if status != 200:
            return f"{url}/api/auth/config answers {status}"
        if version:
            status, _, raw = fetch(url, "/api/meta", timeout=5)
            got = json.loads(raw).get("version") if status == 200 else None
            if pep440(got) != pep440(version):
                return f"{url}/api/meta says CITAR {got}, not {version}"
        if site:
            status, _, _ = fetch(site, "/api/auth/config", timeout=5)
            if status != 200:
                return f"{site}/api/auth/config answers {status}"
            if want_ws:
                problem = handshake(site)
                if problem:
                    return f"{site}/ws/worker: the WebSocket handshake {problem}"
    except (OSError, ValueError, http.client.HTTPException) as exc:
        return f"{type(exc).__name__}: {exc}"
    return None


def pep440(version) -> str:
    return str(version or "").replace("-alpha.", "a").replace("-beta.", "b").replace("-rc.", "rc")


def cmd_health(args) -> int:
    healthy = wait_until(lambda: health_once(args.url, args.site, args.version, args.handshake) is None, args.seconds)
    last = None if healthy else health_once(args.url, args.site, args.version, args.handshake)
    what = f"CITAR{(' ' + args.version) if args.version else ''} answers at {args.url}"
    if args.site:
        what += f" and through {args.site}" + (" (its WebSocket upgrade passes the proxy)" if args.handshake else "")
    check(last is None, what, last or "")
    return done()


def site_urls(path: str) -> list[str]:
    return [line.strip() for line in Path(path).read_text().splitlines()
            if line.strip() and not line.strip().startswith("#")]


def cmd_sites(args) -> int:
    urls = site_urls(args.file)
    bad = []
    for url in urls:
        try:
            status, _, _ = fetch(url, timeout=10)
            if status != 200:
                bad.append(f"{url} answers {status}")
        except (OSError, http.client.HTTPException) as exc:
            bad.append(f"{url}: {type(exc).__name__}: {exc}")
    check(not bad and bool(urls), f"the {len(urls)} stand-in site(s) answer 200{(' ' + args.when) if args.when else ''}",
          "; ".join(bad) if bad else ", ".join(urls))
    return done()


# ----------------------------------------------------------------------------------------------------- the timeline

def cmd_watch(args) -> int:
    """Samples, every --every seconds until a TERM: citar.service's and the slice's memory, the slice's swap and
    oom_kill count, every transient unit in the slice (the pip scope, the verification's and citar-admin's units) with
    its memory, and whether CITAR answers /api/auth/config (the downtime), one JSON line each."""
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    slice_dir = cgroup_of(args.slice)
    with open(args.out, "a", encoding="utf-8") as out:
        n = 0
        while not stop.is_set():
            t0 = time.monotonic()
            row = {"t": round(time.time(), 3)}
            row["slice"] = {"mem": read_int(slice_dir / "memory.current"),
                            "swap": read_int(slice_dir / "memory.swap.current"),
                            "oom_kill": events(slice_dir / "memory.events").get("oom_kill")}
            units = {}
            try:
                children = sorted(p for p in slice_dir.iterdir() if p.is_dir())
            except OSError:
                children = []
            for child in children:
                units[child.name] = {"mem": read_int(child / "memory.current"),
                                     "peak": read_int(child / "memory.peak")}
            row["units"] = units
            if n % max(1, round(args.http_every / args.every)) == 0:
                try:
                    status, _, _ = fetch(args.url, "/api/auth/config", timeout=1.0)
                    row["up"] = status == 200
                except (OSError, http.client.HTTPException):
                    row["up"] = False
            out.write(json.dumps(row, separators=(",", ":")) + "\n")
            out.flush()
            n += 1
            stop.wait(max(0.0, args.every - (time.monotonic() - t0)))
    return 0


# ----------------------------------------------------------------------------------------------------- load

class Load:
    """A steady load on the stand-in (and a lighter one on CITAR through its site): one request every ``period``
    seconds, each timed; a request that runs late delays the next, never doubles it up."""

    def __init__(self, url: str, period: float, name: str):
        self.url, self.period, self.name = url, period, name
        self.samples: list[tuple[float, float, int]] = []     # (start, seconds, status)
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name=f"load-{name}", daemon=True)

    def start(self) -> Load:
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=30)

    def _run(self) -> None:
        nxt = time.monotonic()
        while not self._stop.is_set():
            t0 = time.monotonic()
            try:
                status, _, _ = fetch(self.url, timeout=30)
            except (OSError, http.client.HTTPException):
                status = 0
            self.samples.append((t0, time.monotonic() - t0, status))
            nxt = max(nxt + self.period, time.monotonic())
            self._stop.wait(max(0.0, nxt - time.monotonic()))

    def between(self, spans: list[tuple[float, float]]) -> list[float]:
        """The response times (seconds) of the requests that started inside any of ``spans``."""
        return [s for t, s, _ in self.samples if any(a <= t <= b for a, b in spans)]

    def failed_between(self, spans) -> int:
        return sum(1 for t, _, status in self.samples if status != 200 and any(a <= t <= b for a, b in spans))


def percentile(values: list[float], q: float) -> float | None:
    """The q-th percentile (nearest rank) of ``values``, or None for none."""
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(q / 100 * len(ordered)))
    return ordered[rank - 1]


def times(values: list[float]) -> dict:
    if not values:
        return {"n": 0}
    return {"n": len(values), "p50_ms": round(percentile(values, 50) * 1000, 1),
            "p95_ms": round(percentile(values, 95) * 1000, 1), "max_ms": round(max(values) * 1000, 1)}


# ----------------------------------------------------------------------------------------------------- measure

def citar_admin(*args: str, stdin: str | None = None) -> subprocess.CompletedProcess:
    """``citar-admin`` as an administrator runs it (the installed wrapper: systemd-run as citar, in the slice)."""
    return subprocess.run(["/usr/local/bin/citar-admin", *args], input=stdin, capture_output=True, text=True,
                          timeout=180, check=False)


def host_defaults(python: str) -> dict:
    """``host_cpu_defaults(mode="server", cpus=1, ram_mb=961)`` as the release's own code computes it (the VPS's one
    CPU and 961 MB), run with that release's Python."""
    code = ("import json; from citar.machine_settings import host_cpu_defaults as d; "
            "print(json.dumps(d(mode='server', cpus=1, ram_mb=961)))")
    got = subprocess.run([python, "-P", "-c", code], capture_output=True, text=True, timeout=120, check=False,
                         cwd="/", env={"PATH": "/usr/bin:/bin", "HOME": "/nonexistent"})
    if got.returncode != 0:
        raise SystemExit(f"host_cpu_defaults could not be computed: {got.stderr[-500:]}")
    return json.loads(got.stdout.strip().splitlines()[-1])


class Memory:
    """citar.service's memory over a phase: its cgroup's peak for the span, systemd's MemoryCurrent and its MemoryPeak
    (since the service started), the server process's own resident high-water mark, and the job host's."""

    def __init__(self, unit: str = "citar.service"):
        self.unit = unit
        self.peak = Peak(cgroup_of(unit))
        self.host_pid = None
        self.host_hwm = None
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="memory", daemon=True)
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(0.25):
            self.peak.sample()
            if self.host_pid:
                hwm = vm_hwm_mb(self.host_pid)
                if hwm is not None:
                    self.host_hwm = max(self.host_hwm or 0.0, hwm)

    def end(self) -> dict:
        self._stop.set()
        self._thread.join(timeout=5)
        props = systemd_props(self.unit, "MemoryCurrent", "MemoryPeak", "MemorySwapPeak", "MainPID")
        out = {"span_peak_mb": self.peak.mb(), "span_peak_by": self.peak.how,
               "MemoryCurrent_mb": as_mb(props.get("MemoryCurrent")), "MemoryPeak_mb": as_mb(props.get("MemoryPeak")),
               "MemorySwapPeak_mb": as_mb(props.get("MemorySwapPeak")),
               "server_VmHWM_mb": vm_hwm_mb(props.get("MainPID"))}
        if self.host_hwm is not None:
            out["job_host_VmHWM_mb"] = self.host_hwm
        self.peak.close()
        return out


def slice_oom(slice_: str = SLICE) -> int:
    return events(cgroup_of(slice_) / "memory.events").get("oom_kill", 0)


def executor_of(view: dict) -> dict:
    """The local executor (the server's own job host) in /api/admin/jobs."""
    for e in ((view or {}).get("dispatcher") or {}).get("executors") or []:
        if e.get("kind") == "local":
            return e
    return {}


def set_weight(weight: int | None) -> None:
    """citar.service's CPUWeight= and IOWeight= for a span, at run time (systemctl set-property --runtime); None takes
    the run-time setting away again, back to the unit file's."""
    control = Path("/run/systemd/system.control/citar.service.d")
    if weight is None:
        for name in ("50-CPUWeight.conf", "50-IOWeight.conf"):
            try:
                (control / name).unlink()
            except FileNotFoundError:
                pass
        subprocess.run(["systemctl", "daemon-reload"], check=True)
        return
    subprocess.run(["systemctl", "set-property", "--runtime", "citar.service", f"CPUWeight={weight}",
                    f"IOWeight={weight}"], check=True)


class Measure:
    def __init__(self, args):
        self.args = args
        self.data: dict = {"args": {k: v for k, v in vars(args).items() if k != "func"}}
        self.admin = Session(args.url)
        self.load = Load(args.standin, 1.0 / args.rate, "stand-in")
        self.citar_load = Load(args.site.rstrip("/") + "/api/auth/config", 1.0, "citar")

    def save(self) -> None:
        Path(self.args.out).write_text(json.dumps(self.data, indent=2, default=str) + "\n", encoding="utf-8")

    # ---- the first administrator, signed in
    def first_admin(self) -> bool:
        password = secrets.token_urlsafe(18)
        Path(self.args.password_file).write_text(password + "\n", encoding="utf-8")
        os.chmod(self.args.password_file, 0o600)
        got = citar_admin("create-user", "--handle", self.args.handle, "--email", f"{self.args.handle}@example.org",
                          "--role", "admin", "--password-stdin", stdin=password + "\n")
        made = check(got.returncode == 0 and "Created admin" in got.stdout,
                     "the first administrator made with citar-admin create-user --password-stdin (systemd-run as "
                     "citar, in the slice)", (got.stdout + got.stderr).strip().splitlines()[-1:][0]
                     if (got.stdout + got.stderr).strip() else f"exit {got.returncode}")
        listed = citar_admin("list-users")
        check(listed.returncode == 0 and self.args.handle in listed.stdout, "citar-admin list-users names it")
        signed = check(made and self.admin.login(self.args.handle, password), "it signs in through the API")
        return made and signed

    # ---- the host machine's settings, as the VPS computes them
    def host_settings(self) -> str | None:
        want = host_defaults(self.args.python)
        self.data["host_defaults"] = want
        check(want.get("memory_mb") == 240 and want.get("threads") == 1 and want.get("cpu_percent") == 50,
              "host_cpu_defaults(mode='server', cpus=1, ram_mb=961), from the release's own code", json.dumps(want))
        # The Servers page's read (GET /api/servers) names the host machine; reading the registry makes its row on a
        # new server, which has none until something reads it.
        status, servers = self.admin.call("GET", "/api/servers")
        machine = (servers or {}).get("host_server_id") if status == 200 else None
        self.data["host_machine"] = machine
        if not check(bool(machine), "the host machine's row is found (GET /api/servers: host_server_id)",
                     f"{status} {str(machine)}"):
            return None
        status, got = self.admin.call("PUT", f"/api/machines/{machine}/settings", {"capabilities": {"cpu": want}})
        status2, read = self.admin.call("GET", f"/api/machines/{machine}/settings")
        cpu = ((read or {}).get("capabilities") or {}).get("cpu") if status2 == 200 else {}
        check(status == 200 and {k: (cpu or {}).get(k) for k in want} == want,
              f"PUT /api/machines/{machine}/settings sets them, and they read back", json.dumps(cpu)[:300])
        return machine

    # ---- a span with the load running
    def span(self, name: str, body, *, weight: int | None = None) -> dict:
        if weight is not None:
            set_weight(weight)
        mem = Memory()
        oom0 = slice_oom()
        t0 = time.monotonic()
        result = body(mem) or {}
        t1 = time.monotonic()
        result["memory"] = mem.end()
        result["seconds"] = round(t1 - t0, 1)
        spans = result.pop("_spans", [(t0, t1)])
        result["standin"] = times(self.load.between(spans))
        result["standin"]["failed"] = self.load.failed_between(spans)
        result["citar_via_site"] = times(self.citar_load.between(spans))
        result["oom_kill"] = slice_oom() - oom0
        check(result["oom_kill"] == 0, f"{name}: no OOM kill in the slice", f"{result['oom_kill']} kill(s)")
        self.data[name] = result
        self.save()
        return result

    def idle(self, _mem) -> dict:
        time.sleep(self.args.idle_seconds)
        return {}

    def game(self, _mem) -> dict:
        rounds = self.args.game_rounds
        status, made = self.admin.call("POST", "/api/games", {
            "name": "staging: four bots", "config": {"map_size": "small", "seed": 6611, "turn_limit": rounds},
            "seats": [{"type": "bot"}] * 4})
        if not check(status == 200 and isinstance(made, dict), "a 4-bot small game is created through the API",
                     str(made)[:300]):
            return {"error": made}
        gid = made["id"]
        self.admin.call("POST", f"/api/games/{gid}/control", {"ai_delay": 0})
        t0 = time.monotonic()
        state = {}

        def played():
            nonlocal state
            st, state = self.admin.call("GET", f"/api/games/{gid}")
            return st == 200 and (state.get("phase") != "playing" or (state.get("turn") or 0) >= rounds)

        finished = wait_until(played, self.args.game_timeout, 0.5)
        took = time.monotonic() - t0
        turn = (state or {}).get("turn")
        check(bool(finished) and (turn or 0) >= rounds,
              f"the game plays {rounds} rounds through the API", f"turn {turn}, phase {state.get('phase')}, "
              f"{took:.1f} s")
        self.admin.call("POST", f"/api/games/{gid}/control", {"paused": True})
        return {"game": gid, "rounds": turn, "seconds_per_round": round(took / max(1, turn or 1), 3)}

    def experiment(self, mem, name: str, games: int, seed: int) -> dict:
        """Queues a lab experiment of ``games`` duels (the Bots page's A/B route) and follows it to its end: the spans
        its runs were in flight (from the administrators' view of the dispatcher), the job host as that view shows it
        once ready, and the runs' own figures from the database (each run's peak, as its frames reported it)."""
        status, queued = self.admin.call("POST", "/api/bots/experiments", {
            "profiles": ["standard", "classic-production"], "name": name, "players": 2, "games": games,
            "seed": seed, "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": self.args.lab_turns})
        if not check(status == 200, f"{name}: a lab experiment of {games} duel games is queued", str(queued)[:300]):
            return {"error": queued, "experiment": name}
        spans: list[tuple[float, float]] = []
        running_since = None
        executor: dict = {}
        cgroup = None
        state = None
        t0 = time.monotonic()
        while time.monotonic() - t0 < self.args.lab_timeout:
            st, view = self.admin.call("GET", "/api/admin/jobs")
            if st == 200:
                ex = executor_of(view)
                if ex.get("state") == "ready" and ex.get("pid") and not executor:
                    executor = ex
                    mem.host_pid = ex["pid"]
                    try:
                        cgroup = Path(f"/proc/{ex['pid']}/cgroup").read_text().strip()
                    except OSError:
                        cgroup = None
                running = any(r.get("exp") == name for rs in (view.get("running") or {}).values() for r in rs)
                now = time.monotonic()
                if running and running_since is None:
                    running_since = now
                elif not running and running_since is not None:
                    spans.append((running_since, now))
                    running_since = None
            st, lab = self.admin.call("GET", "/api/lab")
            exp = next((e for e in (lab or {}).get("experiments", []) if e.get("name") == name), {}) \
                if st == 200 else {}
            state = exp.get("state")
            if state == "complete":
                break
            time.sleep(0.25)
        if running_since is not None:
            spans.append((running_since, time.monotonic()))
        took = time.monotonic() - t0
        runs = db_runs(self.args.db, name)
        lines = lab_lines(self.args.saves, name)
        done_runs = [r for r in runs if r["state"] == "done"]
        rounds = sum(r["rounds"] or 0 for r in done_runs)
        return {"_spans": spans, "experiment": name, "state": state, "wall_seconds": round(took, 1),
                "in_flight_seconds": round(sum(b - a for a, b in spans), 1),
                "executor": {k: executor.get(k) for k in ("machine", "state", "threads", "cpu_percent", "memory_mb",
                                                          "held", "memory_per_run_mb", "verifiable", "notes", "pid")},
                "job_host_cgroup": cgroup, "runs": len(runs), "runs_done": len(done_runs), "lines": len(lines),
                "machines": sorted({r["machine_id"] for r in runs}),
                "job_mem_mb": max((r["mem_mb"] for r in runs), default=None),
                "runs_peak_mb": max((r["peak_mb"] for r in runs), default=None),
                "runs_cpu_s": round(sum(r["cpu_s"] for r in done_runs), 2),
                "runs_wall_s": round(sum(r["wall_s"] for r in done_runs), 2), "rounds": rounds,
                "seconds_per_round": round(sum(r["wall_s"] for r in done_runs) / rounds, 4) if rounds else None,
                "cpu_seconds_per_round": round(sum(r["cpu_s"] for r in done_runs) / rounds, 4) if rounds else None}

    def lab(self, mem, memory_mb: int = 240) -> dict:
        """The three duels gate 2 names: played by the job host under citar.service, at the host's settings (its memory
        figure ``memory_mb``)."""
        games = self.args.lab_games
        result = self.experiment(mem, f"staging-lab-{memory_mb}mb-{secrets.token_hex(3)}", games, 7100 + memory_mb)
        ex = result.get("executor") or {}
        check(result.get("state") == "complete" and result.get("runs_done") == games and result.get("lines") == games,
              f"the lab's {games} games are played to their end by the job host",
              json.dumps({k: result.get(k) for k in ("state", "runs", "runs_done", "lines", "machines")}))
        check(ex.get("memory_mb") == memory_mb and ex.get("threads") == 1,
              f"the job host's ready reports the host's settings: {memory_mb} MB, one thread",
              json.dumps({k: ex.get(k) for k in ("memory_mb", "memory_per_run_mb", "threads", "cpu_percent", "held")}))
        notes = "; ".join(ex.get("notes") or [])
        check(f"RLIMIT_DATA {memory_mb} MB" in notes, "its memory is held by RLIMIT_DATA (a system service has no user "
                                                       "manager for a scope), as it logs", notes[:400])
        check("citar.service" in str(result.get("job_host_cgroup")), "the job host runs inside citar.service's cgroup",
              str(result.get("job_host_cgroup")))
        return result

    def host_memory(self, machine: str, memory_mb: int) -> bool:
        """The host's memory figure set to ``memory_mb`` (the rest of its settings kept): its job host starts again with
        it once its runs have finished."""
        want = {**self.data["host_defaults"], "memory_mb": memory_mb}
        status, _ = self.admin.call("PUT", f"/api/machines/{machine}/settings", {"capabilities": {"cpu": want}})
        return check(status == 200, f"the host's memory figure set to {memory_mb} MB (PUT /api/machines/{machine}/"
                                    f"settings)", str(status))

    def load_at(self, mem, weight: int) -> dict:
        """The response-time bound at one CPUWeight=: the stand-in's times while a longer lab keeps the job host busy."""
        return self.experiment(mem, f"staging-load-w{weight}-{secrets.token_hex(3)}", self.args.load_games,
                               7200 + weight)

    def run(self) -> int:
        self.load.start()
        self.citar_load.start()
        try:
            if not self.first_admin():
                return done()
            machine = self.host_settings()
            idle = self.span("idle", self.idle)
            idle_p95 = idle["standin"].get("p95_ms") or 0.0
            check(idle["standin"]["n"] >= 30, "the stand-in's idle response times are sampled",
                  json.dumps(idle["standin"]))
            self.span("game", self.game)
            self.span("lab", self.lab)
            # The bound is held at the installed unit's own CPUWeight= (the one this release settles on); the other
            # weights asked for are measured beside it, for the record.
            unit_weight = int(systemd_props("citar.service", "CPUWeight").get("CPUWeight") or 100)
            self.data["unit_weight"] = unit_weight
            weights = list(self.args.weights) + ([] if unit_weight in self.args.weights else [unit_weight])
            loads = []
            for weight in weights:
                result = self.span(f"load_w{weight}", lambda mem, w=weight: self.load_at(mem, w), weight=weight)
                loads.append((weight, result))
                p95 = result["standin"].get("p95_ms")
                bound = 2 * idle_p95 + 50
                result["bound_ms"] = round(bound, 1)
                result["within_bound"] = p95 is not None and p95 <= bound
                what = (f"CPUWeight={weight}: the stand-in's p95 with a lab game running is within twice its idle p95 "
                        f"plus 50 ms")
                figures = (f"{p95} ms against {bound:.1f} ms ({result['standin']['n']} requests in "
                           f"{result.get('in_flight_seconds')} s of lab; idle p95 {idle_p95} ms; CITAR's own p95 "
                           f"through nginx {result['citar_via_site'].get('p95_ms')} ms)")
                check(result["standin"]["n"] >= 30, f"CPUWeight={weight}: the stand-in's times are sampled while the "
                                                    f"lab runs", f"{result['standin']['n']} requests")
                if weight == unit_weight:
                    check(p95 is not None and p95 <= bound, what + " (the installed unit's weight)", figures)
                else:
                    note(f"{what}: {'yes' if result['within_bound'] else 'NO'}, {figures} (measured beside the unit's "
                         f"{unit_weight}, not held to it)")
                self.save()
            if machine and self.args.host_mb:
                self.host_memory(machine, self.args.host_mb)
                self.span(f"lab_{self.args.host_mb}mb", lambda mem: self.lab(mem, self.args.host_mb))
            self.data["weights"] = {str(w): {"p95_ms": r["standin"].get("p95_ms"), "bound_ms": r.get("bound_ms"),
                                             "within": r.get("within_bound"), "n": r["standin"].get("n"),
                                             "citar_p95_ms": r["citar_via_site"].get("p95_ms"),
                                             "seconds_per_round": r.get("seconds_per_round")} for w, r in loads}
        finally:
            set_weight(None)
            self.load.stop()
            self.citar_load.stop()
            self.data["failures"] = list(FAILURES)
            self.save()
        return done()


def db_runs(db: str, name: str) -> list:
    """The runs of experiment ``name`` in the server's database (read only): each with its job's memory estimate and
    its own peak, CPU and wall seconds and rounds, as its frames reported them."""
    import sqlite3
    try:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=10)
    except sqlite3.Error:
        return []
    try:
        rows = conn.execute(
            "SELECT j.ref, j.mem_mb, r.state, r.peak_mb, r.cpu_s, r.wall_s, r.rounds, r.machine_id "
            "FROM cpu_job_runs r JOIN cpu_jobs j ON j.id = r.job_id WHERE j.ref LIKE ?", (f"lab:{name}:%",)).fetchall()
    except sqlite3.Error:
        return []
    finally:
        conn.close()
    keys = ("ref", "mem_mb", "state", "peak_mb", "cpu_s", "wall_s", "rounds", "machine_id")
    return [dict(zip(keys, row)) for row in rows]


def lab_lines(saves: str, name: str) -> list:
    path = Path(saves) / "lab" / "results" / f"{name}.jsonl"
    if not path.exists():
        path = Path(saves) / "lab" / "done" / f"{name}.jsonl"
    try:
        return [json.loads(x) for x in path.read_text(encoding="utf-8").splitlines() if x.strip()]
    except (OSError, ValueError):
        return []


def cmd_measure(args) -> int:
    return Measure(args).run()


# ----------------------------------------------------------------------------------------------------- the drill

def cmd_drill(args) -> int:
    """The restore drill's second CITAR (on a copy of the backup's database, on another port): it answers, the
    administrator signs in to it, and its accounts are read."""
    healthy = wait_until(lambda: health_once(args.url, None, None, False) is None, args.seconds)
    if not check(bool(healthy), f"the second CITAR answers at {args.url}"):
        return done()
    who = Session(args.url)
    password = Path(args.password_file).read_text(encoding="utf-8").strip()
    if check(who.login(args.handle, password), f"{args.handle} signs in to it with the password made on the first"):
        status, users = who.call("GET", "/api/admin/users")
        handles = [u.get("handle") for u in (users or {}).get("users", [])] if status == 200 else []
        check(args.handle in handles, "its accounts are read: GET /api/admin/users", ", ".join(map(str, handles)))
    return done()


# ----------------------------------------------------------------------------------------------------- the report

def cmd_report(args) -> int:
    """The run's figures from its logs: each transient unit's life and peak (the pip install's scope among them), the
    times CITAR did not answer (the switches' downtime), the slice's OOM kills and highest memory and swap."""
    logs = Path(args.logs)
    rows = [json.loads(x) for x in (logs / "watch.jsonl").read_text().splitlines() if x.strip()]
    phases = [json.loads(x) for x in (logs / "phases.jsonl").read_text().splitlines() if x.strip()] \
        if (logs / "phases.jsonl").exists() else []
    units: dict = {}
    for row in rows:
        for name, u in (row.get("units") or {}).items():
            e = units.setdefault(name, {"first": row["t"], "last": row["t"], "peak_mb": 0.0})
            e["last"] = row["t"]
            e["peak_mb"] = max(e["peak_mb"], round(max(u.get("peak") or 0, u.get("mem") or 0) / MIB, 1))
    transient = {n: {"seconds": round(e["last"] - e["first"], 1), "peak_mb": e["peak_mb"]}
                 for n, e in units.items() if n.startswith("run-")}
    downs, start = [], None
    for row in rows:
        if "up" not in row:
            continue
        if not row["up"] and start is None:
            start = row["t"]
        elif row["up"] and start is not None:
            downs.append((start, row["t"]))
            start = None

    def phase_at(t):
        name = None
        for p in phases:
            if p["t"] <= t:
                name = p["phase"]
        return name

    top = max(rows, key=lambda r: r["slice"].get("mem") or 0) if rows else None
    report = {
        "samples": len(rows),
        "slice_peak_mb": round((top["slice"].get("mem") or 0) / MIB, 1) if top else None,
        "slice_peak_during": phase_at(top["t"]) if top else None,
        "slice_peak_units_mb": {n: round((u.get("mem") or 0) / MIB, 1) for n, u in (top or {}).get("units", {}).items()},
        # memory.events now: "high" and "max" count the times the slice was held at its limit (reclaimed), "oom_kill"
        # the processes killed (the setup's probe is the one expected).
        "slice_memory_events": events(cgroup_of(SLICE) / "memory.events"),
        "slice_swap_peak_mb": round(max((r["slice"].get("swap") or 0) for r in rows) / MIB, 1) if rows else None,
        "oom_kill_first": rows[0]["slice"].get("oom_kill") if rows else None,
        "oom_kill_last": rows[-1]["slice"].get("oom_kill") if rows else None,
        "transient_units": transient,
        "pip_scopes": {n: e for n, e in transient.items() if n.endswith(".scope")},
        "downtime": [{"phase": phase_at(a), "seconds": round(b - a, 1)} for a, b in downs],
        "citar_service_peak_mb": units.get("citar.service", {}).get("peak_mb"),
    }
    (logs / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    if report["oom_kill_first"] is not None:
        check(report["oom_kill_last"] == report["oom_kill_first"], "no OOM kill in the slice during the run",
              f"{report['oom_kill_first']} -> {report['oom_kill_last']}")
    return done()


# ----------------------------------------------------------------------------------------------------- arguments

def parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="staging_check.py", description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="command", required=True)

    p = sub.add_parser("hog")
    p.add_argument("--mb", type=int, required=True)
    p.add_argument("--hold", type=float, default=2.0)
    p.set_defaults(func=cmd_hog)

    p = sub.add_parser("burn")
    p.add_argument("--seconds", type=float, default=5.0)
    p.add_argument("--cgroup", required=True)
    p.set_defaults(func=cmd_burn)

    p = sub.add_parser("ballast")
    p.add_argument("--mb", type=int, required=True)
    p.add_argument("--every", type=float, default=30.0)
    p.set_defaults(func=cmd_ballast)

    p = sub.add_parser("standin")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--work-ms", type=float, default=25.0)
    p.set_defaults(func=cmd_standin)

    p = sub.add_parser("sites")
    p.add_argument("file")
    p.add_argument("--when", default="")
    p.set_defaults(func=cmd_sites)

    p = sub.add_parser("health")
    p.add_argument("url")
    p.add_argument("--site")
    p.add_argument("--version")
    p.add_argument("--seconds", type=float, default=60.0)
    p.add_argument("--handshake", action="store_true")
    p.set_defaults(func=cmd_health)

    p = sub.add_parser("watch")
    p.add_argument("--out", required=True)
    p.add_argument("--slice", default=SLICE)
    p.add_argument("--url", default="http://127.0.0.1:8765")
    p.add_argument("--every", type=float, default=0.25)
    p.add_argument("--http-every", type=float, default=0.5)
    p.set_defaults(func=cmd_watch)

    p = sub.add_parser("measure")
    p.add_argument("--out", required=True)
    p.add_argument("--url", default="http://127.0.0.1:8765")
    p.add_argument("--site", required=True)
    p.add_argument("--standin", required=True)
    p.add_argument("--python", default="/opt/citar/current/venv/bin/python")
    p.add_argument("--saves", default="/var/lib/citar/saves")
    p.add_argument("--handle", default="stagingadmin")
    p.add_argument("--password-file", required=True)
    p.add_argument("--rate", type=float, default=4.0, help="stand-in requests a second")
    p.add_argument("--idle-seconds", type=float, default=60.0)
    p.add_argument("--game-rounds", type=int, default=40)
    p.add_argument("--game-timeout", type=float, default=900.0)
    p.add_argument("--db", default="/var/lib/citar/citar.db")
    p.add_argument("--lab-games", type=int, default=3)
    p.add_argument("--load-games", type=int, default=40, help="the duels of each CPUWeight's response-time span")
    p.add_argument("--host-mb", type=int, default=0, help="a lab at this host memory figure too (0: none)")
    p.add_argument("--lab-turns", type=int, default=0, help="each duel's rounds (0: to the speed's last turn)")
    p.add_argument("--lab-timeout", type=float, default=1800.0)
    p.add_argument("--weights", type=lambda s: [int(x) for x in re.split(r"[ ,]+", s.strip()) if x], default=[50])
    p.set_defaults(func=cmd_measure)

    p = sub.add_parser("drill")
    p.add_argument("--url", required=True)
    p.add_argument("--handle", default="stagingadmin")
    p.add_argument("--password-file", required=True)
    p.add_argument("--seconds", type=float, default=60.0)
    p.set_defaults(func=cmd_drill)

    p = sub.add_parser("report")
    p.add_argument("--logs", required=True)
    p.set_defaults(func=cmd_report)
    return ap


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
