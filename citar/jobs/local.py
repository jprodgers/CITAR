"""The local executor (design/phase4-helper.md P4.5.6): the server's own job host, as the dispatcher's executor.

It is the host machine's row (``servers.host()``): one job host child (``python -m citar.jobs host``,
:class:`citar.jobs.host.JobHost`) under that row's CPU settings (``capabilities.cpu``: on by default, with the mode's
figures, ``machine_settings.host_cpu_defaults``; a figure left at 0 is the default's, so the host is always given
concrete limits, which it requires), started when there is work for it, restarted when it dies (each run of a dead
host failed ``memory`` or ``host`` from what showed it, the peak each run reported beside it) and when its settings
change (once its runs have finished: no new run starts meanwhile), and closed with the server, whose death its host
outlives by two seconds at most.

**Trusted by definition** (it is the server): its runs' verdict is ``reference``, its CPU seconds the reference cost
of the jobs it plays, and it verifies helpers' results (design/phase4-helper.md P4.5.8: every ``new`` machine's, and
the sampled ones of the others). Its host is told it
plays its parent's own work (``play_unverifiable``), so a server whose canary or self-check fails (a server on a
ruleset of its own, whose games the golden set does not hold) still plays its own lab, its ``verifiable`` false: it
then verifies nobody, and its jobs build (which covers the ruleset) is shared by no stock helper anyway.

Its report carries the extension's ``jobs_build`` (P4.4.5), so a server built in another flavour than a helper never
meets that helper's jobs.

**It wakes the dispatcher** (``bind``) when a run starts or ends, when its host ends and when a host's start is over
(P4.5.5's "on events": a job frame, an executor's change), so a thread whose game ended takes the next one at once
rather than at the dispatcher's next tick; each frame is stamped when it arrived (``citar.jobs.host.RX``), so a run is
timed by its frames, never by when the dispatcher read them.

Replaces: the lab runner's ``workers`` and ``night_workers`` (``saves/lab/config.json``), its subprocess per game, its
orphan adoption and its killing of games by pid.
"""
from __future__ import annotations

import logging
import threading
import time
from typing import Callable, Optional

from citar import engine_api
from citar.jobs import host as H

log = logging.getLogger("citar.jobs.local")

#: The machine id a run of the local executor names when no row is the host yet (the ledger's ``HOST_UNKNOWN``).
NO_HOST = "host"
#: How long after a host's death or a failed start the next start waits, doubling to ``RESTART_MAX``.
RESTART_FIRST, RESTART_MAX = 2.0, 300.0


def figures(settings: Optional[dict]) -> dict:
    """The host machine's CPU settings as concrete figures (``servers.machine_settings(host)['cpu']``, the host's
    defaults filled in): ``enabled``, ``threads``, ``cpu_percent``, ``memory_mb`` (never 0: a host refuses a 0) and
    ``work``. With no host row, the defaults."""
    from citar.machine_settings import host_cpu_defaults
    base = host_cpu_defaults()
    cpu = (settings or {}).get("cpu") or base
    return {"enabled": bool(cpu.get("enabled")),
            "threads": int(cpu.get("threads") or base["threads"]),
            "cpu_percent": int(cpu.get("cpu_percent") or base["cpu_percent"]),
            "memory_mb": int(cpu.get("memory_mb") or base["memory_mb"]),
            "work": tuple(cpu.get("work") or ())}


class LocalExecutor:
    """The server's job host as an executor. Driven by the dispatcher's thread alone (a host's start runs on a thread
    of its own, and the dispatcher looks at it only once it is over). ``host_factory(threads, cpu_percent, memory_mb)``
    makes the :class:`~citar.jobs.host.JobHost` (tests give the test jobs' switches through it)."""

    kind = "local"

    def __init__(self, *, host_factory: Optional[Callable] = None, clock: Callable[[], float] = time.monotonic,
                 settings: Optional[Callable[[], tuple]] = None):
        self.clock = clock
        self.host_factory = host_factory or (lambda threads, cpu_percent, memory_mb: H.JobHost(
            threads, cpu_percent, memory_mb, play_unverifiable=True))
        self._settings = settings or _host_settings
        self.machine, self.name = NO_HOST, "this server"
        self.want = figures(None)
        self.limits: Optional[dict] = None       # the figures the running host holds
        self.host: Optional[H.JobHost] = None
        self.ready: Optional[dict] = None
        self.state = "idle"                      # off | idle | starting | ready | draining | failed
        self.reason = ""
        self.in_flight: set = set()
        self.paused = False
        self.restart_at = 0.0
        self.backoff = RESTART_FIRST
        self._starter: Optional[threading.Thread] = None
        self._start_done = threading.Event()
        self._start_error: Optional[BaseException] = None
        self.deaths = 0
        self.jobs_build = engine_api.jobs_build()
        self._wake: Optional[Callable[[], None]] = None

    def bind(self, wake: Callable[[], None]) -> None:
        """The dispatcher's ``poke``: called (from the host's reader thread, or its start's) when a run starts or
        ends, when the host ends and when its start is over. It must not block."""
        self._wake = wake

    def _woken(self, _frame=None) -> None:
        wake = self._wake
        if wake is not None:
            wake()

    # -------------------------------------------------------------- what the dispatcher reads
    @property
    def id(self) -> str:
        return self.machine

    @property
    def threads(self) -> int:
        return int((self.limits or self.want)["threads"])

    @property
    def free(self) -> int:
        """Runs it would take now."""
        if self.state != "ready":
            return 0
        return max(0, self.threads - len(self.in_flight))

    @property
    def work(self) -> tuple:
        return tuple(self.want["work"])

    @property
    def memory_per_run_mb(self) -> Optional[int]:
        return self.host.memory_per_run_mb if self.host is not None and self.ready else None

    @property
    def verifiable(self) -> bool:
        return bool(self.ready and self.ready.get("verifiable"))

    @property
    def cpu_percent(self) -> int:
        return int((self.limits or self.want)["cpu_percent"])

    @property
    def cpus(self) -> int:
        return H.machine_cpus()

    @property
    def speed(self) -> float:
        return 1.0

    @property
    def online(self) -> bool:
        return True

    def refuses(self, job) -> Optional[str]:
        """Why this executor will not play ``job`` (None: it will): another jobs build, a kind its settings leave
        out, or more memory a run than its host leaves each (``memory_per_run_mb``, P4.3.6). An executor's
        ``refuses`` looks at nothing of a job but its kind, jobs build, ``mem_mb`` and owner: the dispatcher caches
        its answers by those within a tick."""
        if job.jobs_build != self.jobs_build:
            return f"the job is for jobs build {job.jobs_build}, this server's is {self.jobs_build}"
        if job.kind not in self.work:
            return f"its CPU settings leave out {job.kind.replace('_', ' ')}s"
        per_run = self.memory_per_run_mb
        if per_run is not None and job.mem_mb > per_run:
            return f"it needs {job.mem_mb} MB a run, and this server's job host leaves each run {per_run} MB"
        return None

    def describe(self) -> dict:
        """The executor as the Lab page and the administrators' view show it."""
        r = self.ready or {}
        limits = self.limits or self.want
        return {"machine": self.machine, "name": self.name, "kind": self.kind, "on": self.want["enabled"],
                "state": self.state, "reason": self.reason or None, "threads": limits["threads"],
                "cpu_percent": limits["cpu_percent"], "memory_mb": limits["memory_mb"],
                "work": list(self.work), "running": len(self.in_flight), "paused": self.paused,
                "verifiable": self.verifiable if self.ready else None,
                "verify_reason": r.get("reason") if self.ready and not r.get("verifiable") else None,
                "held": r.get("limits"), "memory_per_run_mb": r.get("memory_per_run_mb"),
                "jobs_build": self.jobs_build, "pid": r.get("pid"), "notes": r.get("notes") or []}

    # -------------------------------------------------------------- the host's life
    def refresh(self, wanted: bool) -> None:
        """Read the host row's settings again, and start, restart or close the host as they say: started when CPU
        work is on and ``wanted`` (there is work it could take), once its backoff is over; restarted with new
        figures once its runs have finished; closed when CPU work is turned off and nothing runs."""
        now = self.clock()
        machine, name, settings = self._settings()
        self.machine, self.name = machine or NO_HOST, name or "this server"
        self.want = figures(settings)
        self._finish_start()
        if self.state == "starting":
            return
        figures_now = {k: self.want[k] for k in ("threads", "cpu_percent", "memory_mb")}
        if not self.want["enabled"]:
            if self.host is not None and not self.in_flight:
                self._close("CPU work turned off in the host's machine settings")
            self.state = "off" if self.host is None else "draining"
            self.reason = "CPU work is off in the host's machine settings"
            return
        if self.host is not None and self.limits != figures_now:
            if self.in_flight:
                self.state, self.reason = "draining", "its CPU settings changed: it restarts once its games finish"
                return
            self._close("its CPU settings changed")
        if self.host is None:
            if not wanted:
                self.state, self.reason = "idle", ""
                return
            if now < self.restart_at:
                self.state = "failed"
                return
            self._start(figures_now)
            return
        self.state, self.reason = "ready", ""

    def _start(self, limits: dict) -> None:
        self.limits = dict(limits)
        self.host = self.host_factory(limits["threads"], limits["cpu_percent"], limits["memory_mb"])
        self.host.on_frame = self._woken
        self.ready, self._start_error = None, None
        self.state, self.reason = "starting", "its job host is starting"
        host = self.host
        done = self._start_done = threading.Event()

        def start():
            try:
                host.start()
            except BaseException as e:          # reported by _finish_start on the dispatcher's thread
                self._start_error = e
            done.set()                          # before the wake: the tick it brings sees the start over
            self._woken()                       # its runs may be offered now (or its failure seen)
        self._starter = threading.Thread(target=start, name="citar-local-host-start", daemon=True)
        self._starter.start()

    def _finish_start(self) -> None:
        if self.state != "starting" or self._starter is None or not self._start_done.is_set():
            return
        self._starter = None
        err = self._start_error
        if err is None and self.host is not None and self.host.ready:
            self.ready = self.host.ready
            self.state, self.reason = "ready", ""
            if not self.ready.get("verifiable"):
                log.warning("this server's job host is not verifiable (%s): it plays its own lab, and verifies "
                            "nobody", self.ready.get("reason"))
            if self.paused:
                self.host.pause()
            return
        self.host = None
        self.limits = None
        self._failed(f"its job host could not start: {err}")

    def _failed(self, why: str) -> None:
        self.state, self.reason = "failed", why
        self.restart_at = self.clock() + self.backoff
        self.backoff = min(RESTART_MAX, self.backoff * 2)
        log.warning("the local executor: %s; trying again in %.0f s", why, self.restart_at - self.clock())

    def _close(self, why: str) -> None:
        log.info("closing this server's job host: %s", why)
        host, self.host, self.ready, self.limits = self.host, None, None, None
        if host is not None:
            host.close()

    # -------------------------------------------------------------- runs
    def offer(self, run: str, job, deadline_s: float) -> None:
        self.in_flight.add(run)
        self.host.offer(run, job.kind, job.spec, job.spec_hash, deadline_s, mem_mb=int(job.mem_mb), job=job.id,
                        priority=int(job.priority))

    def abort(self, run: str, reason: str = "") -> None:
        if self.host is not None and run in self.in_flight:
            self.host.abort(run, reason)

    def pause(self) -> None:
        """Hold every running game at its next seat step (a benchmark holds the machine)."""
        self.paused = True
        if self.host is not None and self.ready:
            self.host.pause()

    def resume(self) -> None:
        self.paused = False
        if self.host is not None and self.ready:
            self.host.resume()

    def poll(self) -> list:
        """The host's frames since the last look (never waiting). A host that died gives a ``job_failed`` for each of
        its runs (``memory`` or ``host``, from what showed it), with the largest peak each reported (``peak_mb``,
        None when it reported none) and whether it was the host's only run (``alone``); the host is started again
        after its backoff (at once after a memory death, which was a run's)."""
        out: list = []
        if self.host is None or self.ready is None:
            return out
        while True:
            frame = self.host.next(0)
            if frame is None:
                return out
            t = frame.get("t")
            if t == "gone":
                out.extend(self._died(frame.get(H.RX)))
                return out
            if t in ("job_reject", "job_result", "job_failed", "job_aborted"):
                self.in_flight.discard(frame.get("run"))
                if t == "job_result":
                    self.backoff = RESTART_FIRST
            if t in ("heartbeat", "garbled"):
                continue
            out.append(frame)

    def _died(self, rx: Optional[float] = None) -> list:
        """The runs of a host that died, failed as its death says; each stamped with when the host's end arrived."""
        host = self.host
        per_run = host.memory_per_run_mb
        death = host.death()
        alone = len(host.in_flight) == 1
        failures = []
        for f in host.failures(death):
            failures.append({**f, "peak_mb": host.peak_mb(f["run"]), "alone": alone, "per_run_mb": per_run,
                             **({H.RX: rx} if rx is not None else {})})
        self.deaths += 1
        self.in_flight.clear()
        self.host, self.ready, self.limits = None, None, None
        log.warning("this server's job host died (%s); %d run(s) failed", death, len(failures))
        try:
            from citar import lab
            lab.log(f"the server's job host died ({death}); {len(failures)} game(s) go back to the queue")
        except OSError:
            pass
        if death.kind == "memory":
            self.state, self.reason = "idle", ""
            self.restart_at = self.clock()
        else:
            self._failed(f"its job host died: {death}")
        return failures

    def close(self) -> None:
        """End the host (its runs stop; the dispatcher's next start plays them again)."""
        if self._starter is not None:
            self._starter.join(timeout=H.READY_WAIT)
        self._close("the server is stopping")
        self.in_flight.clear()
        self.state = "idle"


def _host_settings() -> tuple:
    """(the host row's id, its name, ``servers.machine_settings`` of it), or (None, None, None) with no host."""
    from citar import servers
    host = servers.host()
    if host is None:
        return None, None, None
    return host["id"], host.get("name"), servers.machine_settings(host["id"])
