"""The server's job host, from the parent's side (design/phase4-helper.md P4.4.3, P4.5.6).

The job host is the jobs crate's (``citar_jobs::host``), the same Rust as the helper's ``citar-helper job-host``; the
server runs it as ``python -m citar.jobs host``, a child process, so that a game's crash, an out-of-memory death or a
hung game stays away from the server, and every OS has one process to put its limits on. This module is what
``citar_jobs::host::parent`` is to the helper, the same rules in Python:

- **Starting a host** (:meth:`JobHost.start`): the child, its pipes, and on Linux, when the user's service manager
  answers (:func:`scope_available`), under ``systemd-run --user --scope`` with the limits as the scope's properties
  (:func:`scope_args`), so the kernel holds them; then the ``limits`` frame with the extension's jobs build, and the
  host's ``ready``, whose jobs build must be the same: a host of another build is ended, never used.
- **Its frames** (:meth:`JobHost.next`), as dicts with their ``t``, and which runs are still in flight on it.
- **Its death** (:meth:`JobHost.death`, :func:`classify`): a memory death ends the whole host, and every run of it
  fails ``memory`` (:meth:`JobHost.failures`); it shows as Rust's ``memory allocation of N bytes failed`` on the
  host's standard error (a Job Object's limit or ``RLIMIT_DATA`` refused a commit), as the ``oom_kill`` count of the
  scope's parent cgroup rising (the kernel killed it at the scope's ``MemoryMax``), or as the macOS watchdog's line
  or exit code. Any other death fails its runs ``host``; both are retried elsewhere.

The protocol (JSON lines; ``limits``, ``ready``, ``fatal``, the job frames, ``pause``, ``resume``, ``heartbeat``) is
described in ``crates/citar-jobs/src/host/mod.rs``. The dispatcher and the local executor that use this are 4-09's.
"""
from __future__ import annotations

import collections
import json
import logging
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from citar import engine_api

log = logging.getLogger("citar.jobs.host")

#: How long a parent waits for a host's ``ready`` (its limits, the canary and the self-check).
READY_WAIT = 120.0
#: How many lines of a host's log are kept for the classification and the message.
TAIL = 64
#: How long the user's service manager has to answer.
PROBE_WAIT = 5.0
#: The line the macOS watchdog prints before it ends a host past its memory limit (``citar_jobs::limits``).
WATCHDOG_LINE = "citar job host: resident memory passed the limit"
#: The exit code of a host the macOS watchdog ended.
EXIT_MEMORY = 3
#: Where cgroup v2 is mounted.
CGROUP_ROOT = "/sys/fs/cgroup"
#: The frames that end a run.
_ENDS = {"job_reject", "job_result", "job_failed", "job_aborted"}


def command() -> list:
    """The server's job host: this Python running ``citar.jobs``."""
    return [sys.executable, "-m", "citar.jobs", "host"]


def machine_cpus() -> int:
    """The logical CPUs of the whole machine, as the host counts them: on Linux the online CPUs, not what a cgroup's
    quota leaves."""
    if sys.platform.startswith("linux"):
        try:
            n = parse_cpu_list(Path("/sys/devices/system/cpu/online").read_text())
            if n:
                return n
        except OSError:
            pass
    return os.cpu_count() or 1


def parse_cpu_list(text: str) -> int:
    """Counts a kernel CPU list (``0-11``, ``0,2-5``); 0 when it does not read."""
    n = 0
    for part in text.strip().split(","):
        if not part:
            continue
        a, _, b = part.partition("-")
        try:
            lo, hi = int(a), int(b or a)
        except ValueError:
            return 0
        if hi < lo:
            return 0
        n += hi - lo + 1
    return n


def _answers(args: list) -> Optional[str]:
    """``args``' standard output when it ran and succeeded within :data:`PROBE_WAIT`."""
    try:
        r = subprocess.run(args, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=PROBE_WAIT)
    except (OSError, subprocess.SubprocessError):
        return None
    return r.stdout if r.returncode == 0 else None


#: ``systemd-run``'s arguments for the trial scope :func:`scope_available` makes (``citar_jobs``' ``SCOPE_TRIAL``).
SCOPE_TRIAL = ("--user", "--scope", "--quiet", "--collect", "--", "true")


def scope_available() -> bool:
    """Whether a host can be started under a ``systemd --user`` scope: Linux, the user's service manager answering,
    and a trial scope made and ended within :data:`PROBE_WAIT` (what a host's start does, so a manager that answers
    but will not make this user a scope leaves hosts started directly rather than never started). Not on a VPS's
    system service with no user manager: there the host holds its memory by ``RLIMIT_DATA`` and its CPU softly
    (``nice`` and its threads)."""
    if not sys.platform.startswith("linux"):
        return False
    version = _answers(["systemctl", "--user", "show", "-p", "Version", "--value"])
    return bool(version and version.strip()) and _answers(["systemd-run", *SCOPE_TRIAL]) is not None


def scope_args(threads: int, cpu_percent: int, memory_mb: int, cpus: int) -> list:
    """``systemd-run``'s arguments for a host holding these limits on a machine of ``cpus`` logical CPUs, as
    ``citar_jobs::host::parent::scope_args`` writes them."""
    return ["--user", "--scope", "--quiet", "--collect",
            "-p", f"CPUQuota={cpu_percent * cpus}%",
            "-p", f"MemoryMax={memory_mb}M",
            "-p", "MemorySwapMax=0",
            "-p", f"TasksMax={threads + 16}",
            "--"]


def oom_kills(cgroup: str) -> Optional[int]:
    """The ``oom_kill`` count in a cgroup's ``memory.events`` (its descendants' kills included), or None."""
    try:
        text = Path(f"{CGROUP_ROOT}{cgroup}/memory.events").read_text()
    except OSError:
        return None
    for line in text.splitlines():
        key, _, value = line.partition(" ")
        if key == "oom_kill":
            try:
                return int(value)
            except ValueError:
                return None
    return None


def is_allocation_failure(line: str) -> bool:
    """Whether ``line`` is Rust's report of an allocation that failed."""
    s = line.strip()
    return s.startswith("memory allocation of ") and s.endswith(" bytes failed")


@dataclass(frozen=True)
class Death:
    """Why a host ended: ``kind`` is ``memory`` (the memory limit ended it), ``refused`` (it refused to start: then
    ``code`` is its ``fatal`` code) or ``other``; ``why`` says what showed it."""
    kind: str
    why: str
    code: Optional[str] = None

    def __str__(self) -> str:
        return {"memory": "the memory limit ended the job host: ",
                "refused": "the job host refused to start: "}.get(self.kind, "the job host ended: ") + self.why


def classify(returncode: Optional[int], tail: list, oom: Optional[tuple] = None,
             fatal: Optional[dict] = None) -> Death:
    """Why a host ended, from its exit code, the last lines of its log, its scope parent's ``oom_kill`` count before
    and after (Linux), and the ``fatal`` frame it sent, if any: ``citar_jobs::host::parent::classify``'s rules."""
    for line in tail:
        if is_allocation_failure(line):
            return Death("memory", f'an allocation failed: "{line.strip()}"')
    if oom and oom[1] > oom[0]:
        return Death("memory", f"the cgroup's oom_kill count rose from {oom[0]} to {oom[1]}")
    for line in tail:
        if WATCHDOG_LINE in line:
            return Death("memory", f'its watchdog: "{line.strip()}"')
    if returncode == EXIT_MEMORY and fatal is None:
        return Death("memory", f"its exit code {EXIT_MEMORY} (the memory watchdog's)")
    if fatal is not None:
        return Death("refused", str(fatal.get("message", "")), str(fatal.get("code", "")))
    exit_ = "it did not exit, and was ended" if returncode is None else f"exit code {returncode}"
    last = f"; its log's last line: {tail[-1].strip()[:300]}" if tail else ""
    return Death("other", exit_ + last)


class HostError(RuntimeError):
    """A host that could not be started: ``code`` is ``start``, ``refused`` (its ``fatal`` code in ``fatal``),
    ``build`` (its ``ready`` named another jobs build), ``died`` (``death`` says why), ``timeout`` or ``protocol``."""

    def __init__(self, code: str, message: str, fatal: Optional[str] = None, death: Optional[Death] = None):
        super().__init__(message)
        self.code, self.fatal, self.death = code, fatal, death

    @property
    def limits(self) -> bool:
        """Whether the host could not hold its limits (or live under them): a ``job_reject`` ``limits``."""
        return self.fatal == "limits" or (self.death is not None and self.death.kind == "memory")


class JobHost:
    """One job host child, holding ``threads``, ``cpu_percent`` (of the whole machine, 1 to 100) and ``memory_mb``
    (every run together, and the host's own).

    ``scope`` chooses the ``systemd --user`` wrapper (None: where :func:`scope_available` says; a test turns it off).
    ``jobs_build`` is the build the host must be (the extension's by default). ``env`` adds variables to the child's
    environment (the test jobs' switches, in tests). Not thread-safe: one owner (4-09's local executor) drives it.
    """

    def __init__(self, threads: int, cpu_percent: int, memory_mb: int, *, scope: Optional[bool] = None,
                 jobs_build: Optional[str] = None, env: Optional[dict] = None, ready_wait: float = READY_WAIT,
                 argv: Optional[list] = None):
        self.threads, self.cpu_percent, self.memory_mb = threads, cpu_percent, memory_mb
        self.scope = scope_available() if scope is None else scope
        self.jobs_build = jobs_build or engine_api.jobs_build()
        self.env = dict(env or {})
        self.ready_wait = ready_wait
        self.argv = list(argv or command())
        self.ready: Optional[dict] = None
        self.in_flight: set = set()
        self.fatal: Optional[dict] = None
        self._proc: Optional[subprocess.Popen] = None
        self._frames: queue.Queue = queue.Queue()
        self._tail: collections.deque = collections.deque(maxlen=TAIL)
        self._log: collections.deque = collections.deque(maxlen=1000)
        self._log_reader: Optional[threading.Thread] = None
        self._oom: Optional[tuple] = None

    @property
    def pid(self) -> Optional[int]:
        """The child's process id. In a venv on Windows the child is the venv's launcher and the host its own child
        (:attr:`host_pid`)."""
        return self._proc.pid if self._proc else None

    @property
    def host_pid(self) -> Optional[int]:
        """The host's own process id, as its ``ready`` gave it."""
        return self.ready.get("pid") if self.ready else None

    def _argv(self) -> list:
        if not self.scope:
            return list(self.argv)
        return ["systemd-run", *scope_args(self.threads, self.cpu_percent, self.memory_mb, machine_cpus()),
                *self.argv]

    def start(self) -> dict:
        """Starts the host and waits for its ``ready``, which it returns. Raises :class:`HostError`; the host is
        ended first."""
        env = {**os.environ, **self.env}
        flags = getattr(subprocess, "CREATE_NO_WINDOW", 0) if os.name == "nt" else 0
        try:
            self._proc = subprocess.Popen(self._argv(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          stderr=subprocess.PIPE, env=env, creationflags=flags)
        except OSError as e:
            raise HostError("start", f"the job host could not be started: {e}") from e
        threading.Thread(target=self._read_frames, name="citar-host-frames", daemon=True).start()
        self._log_reader = threading.Thread(target=self._read_log, name="citar-host-log", daemon=True)
        self._log_reader.start()
        self._send({"t": "limits", "jobs_build": self.jobs_build, "threads": self.threads,
                    "cpu_percent": self.cpu_percent, "memory_mb": self.memory_mb})
        first = self.next(self.ready_wait)
        t = first.get("t") if first else None
        if t == "ready":
            if first.get("jobs_build") != self.jobs_build:
                self._end(0)
                raise HostError("build", f"the job host's jobs build is {first.get('jobs_build')}, not this build's "
                                         f"{self.jobs_build}")
            self.ready = first
            log.info("job host %s ready: cpu %s, memory %s, verifiable %s; %s", first.get("pid"),
                     first.get("limits", {}).get("cpu"), first.get("limits", {}).get("memory"),
                     first.get("verifiable"), "; ".join(first.get("notes") or []))
            cgroup = first.get("cgroup")
            if cgroup and "/" in cgroup.strip("/"):
                parent = cgroup.rsplit("/", 1)[0]
                before = oom_kills(parent)
                if before is not None:
                    self._oom = (parent, before)
            return first
        if t == "fatal":
            self._end(2)
            raise HostError("refused", f"the job host refused to start ({first.get('code')}): {first.get('message')}",
                            fatal=str(first.get("code")))
        if t == "gone":
            death = self.death(5)
            raise HostError("died", f"the job host ended before it was ready: {death}", death=death)
        self._end(0)
        if first is None:
            raise HostError("timeout", f"the job host was not ready within {self.ready_wait} s")
        raise HostError("protocol", f"the job host sent {t or first} before ready")

    def _read_frames(self) -> None:
        # Each reader closes its pipe once the host has closed its end: nothing else reads it.
        with self._proc.stdout as out:
            for raw in out:
                line = raw.decode("utf-8", "replace").strip()
                if not line:
                    continue
                try:
                    frame = json.loads(line)
                    if not isinstance(frame, dict) or not isinstance(frame.get("t"), str):
                        raise ValueError("not a frame")
                except ValueError:
                    frame = {"t": "garbled", "line": line[:300]}
                self._frames.put(frame)
        self._frames.put({"t": "gone"})

    def _read_log(self) -> None:
        with self._proc.stderr as err:
            for raw in err:
                line = raw.decode("utf-8", "replace").rstrip("\r\n")[:2000]
                self._tail.append(line)
                self._log.append(line)
                log.info("job host %s: %s", self._proc.pid, line)

    def _send(self, frame: dict) -> None:
        try:
            self._proc.stdin.write((json.dumps(frame, separators=(",", ":")) + "\n").encode("utf-8"))
            self._proc.stdin.flush()
        except (OSError, ValueError, AttributeError):
            pass    # a host gone shows as its death, through next()

    def offer(self, run: str, kind: str, spec: dict, spec_hash: str, deadline_s: float, mem_mb: int = 0,
              job: Optional[str] = None, priority: int = 0) -> None:
        """Offers a run (answered ``job_accept`` or ``job_reject``)."""
        self.in_flight.add(run)
        frame = {"t": "job_offer", "run": run, "kind": kind, "spec": spec, "spec_hash": spec_hash,
                 "jobs_build": self.jobs_build, "deadline_s": deadline_s, "mem_mb": mem_mb, "priority": priority}
        if job is not None:
            frame["job"] = job
        self._send(frame)

    def abort(self, run: str, reason: str = "") -> None:
        self._send({"t": "job_abort", "run": run, "reason": reason})

    def pause(self) -> None:
        self._send({"t": "pause"})

    def resume(self) -> None:
        self._send({"t": "resume"})

    def next(self, timeout: Optional[float] = None) -> Optional[dict]:
        """The next frame, waiting up to ``timeout`` seconds (None when none came); ``{"t": "gone"}`` once the host's
        output closed (then :meth:`death`), ``{"t": "garbled", "line"}`` for a line that is not a frame."""
        try:
            frame = self._frames.get(timeout=timeout)
        except queue.Empty:
            return None
        t = frame.get("t")
        if t in _ENDS:
            self.in_flight.discard(frame.get("run"))
        elif t == "fatal":
            self.fatal = frame
        elif t == "gone":
            self._frames.put(frame)     # every later look sees it too
        return frame

    def take_log(self) -> list:
        """The host's log lines since the last call (at most the last 1,000)."""
        out = list(self._log)
        self._log.clear()
        return out

    def _end(self, wait: float) -> Optional[int]:
        """Closes the host's input (it ends its runs and exits within two seconds), waits up to ``wait`` seconds and
        ends it if it has not exited."""
        if self._proc is None:
            return None
        try:
            self._proc.stdin.close()
        except OSError:
            pass
        try:
            return self._proc.wait(timeout=wait)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            own = self.host_pid
            if own and own != self._proc.pid:
                # A venv's launcher on Windows: the host is its child, which killing the launcher leaves running.
                try:
                    os.kill(own, signal.SIGTERM)
                except OSError:
                    pass
            return self._proc.wait()

    def death(self, wait: float = 5.0) -> Death:
        """Why the host ended, once its output closed: it is waited for up to ``wait`` seconds (and ended past it)
        and its death classified (:func:`classify`)."""
        code = self._end(wait)
        if self._log_reader is not None:
            self._log_reader.join(timeout=1.0)
        oom = None
        if self._oom is not None:
            after = oom_kills(self._oom[0])
            if after is not None:
                oom = (self._oom[1], after)
        return classify(code, list(self._tail), oom, self.fatal)

    def failures(self, death: Death) -> list:
        """A ``job_failed`` for each run still in flight on a host that died of ``death``: ``memory`` for a memory
        death, ``host`` otherwise; each retryable."""
        code = "memory" if death.kind == "memory" else "host"
        return [{"t": "job_failed", "run": run, "code": code, "message": str(death), "retryable": True}
                for run in sorted(self.in_flight)]

    def close(self, wait: float = 3.0) -> Optional[int]:
        """Closes the host: its runs stop and it exits within two seconds (ended if not); its exit code."""
        return self._end(wait)


def run_until(host: JobHost, run: str, timeout: float = 600.0) -> list:
    """The frames about ``run`` until it ends (a result, a failure, an abort or a refusal), skipping the others;
    raises TimeoutError, or HostError("died") when the host ends first."""
    deadline = time.monotonic() + timeout
    seen = []
    while True:
        left = deadline - time.monotonic()
        frame = host.next(max(0.0, left))
        if frame is None:
            raise TimeoutError(f"run {run}: nothing for {timeout} s")
        if frame["t"] == "gone":
            death = host.death()
            raise HostError("died", f"run {run}: {death}", death=death)
        if frame.get("run") == run:
            seen.append(frame)
            if frame["t"] in _ENDS:
                return seen
