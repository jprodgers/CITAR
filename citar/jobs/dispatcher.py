"""The job dispatcher (design/phase4-helper.md P4.5.5, P4.5.7): the lab's games as CPU jobs, offered to executors,
their results into the lab, the ratings and the ledger.

**One thread per server process** (``citar-jobs-dispatcher``), like the prompt scheduler and the ledger's writer: it
wakes every ``TICK_SECONDS`` and whenever something happens (``poke``: a run starting or ending on an executor, which
every executor is given through its ``bind``, an executor's change, a benchmark lease, the pause and an
administrator's retry), survives an exception in its loop, and is started again if it dies (``ensure_thread``). It is
the only writer of ``cpu_jobs`` and ``cpu_job_runs`` and of the lab's results files (P4.7.4); its state is the
database, and its view in memory is rebuilt when the server starts (``recover``), which also queues again the games
that failed for want of attempts (0.1.5's "the next runner tries them again"). Its view in memory changes once the
database has taken the change (a new job, an offer, an owner found), or is rebuilt from the database when the database
refused a change it had already made in memory (``_reset``: the runs' frames, the expiries).

**Timing:** a run is timed by when its frames arrived (each stamped by its executor's reader, ``host.RX``), never by
when the dispatcher read them, which can be a tick later: its start, its end and so what the server observed of it.

**A tick** (:meth:`Dispatcher.tick`):

1. **The queue** (``citar.lab``'s files, read again only when the folder changed): each experiment by priority then
   age. An experiment deleted from the queue has its waiting jobs cancelled and its runs aborted; a new priority
   reaches its waiting jobs.
2. **Jobs from the queue** (``_fill``): each game with no settled job and no live one becomes a ``cpu_jobs`` row
   (``lab.job_of``, checked and hashed by the jobs crate), with ``mem_mb`` from the cost table
   (:class:`~citar.jobs.store.Costs`); at most ``WINDOW`` x the executors' threads wait at once, taken in the order
   they would run (priority, then round-robin across owners), so a 10,000-game experiment does not become 10,000 rows
   and a new owner's work is never stuck behind another's window. Only jobs some executor would take count in the
   window: a game no executor would take as it is set now (more memory a run than any has, a kind none plays) gets
   no row, its experiment saying why, so it never holds the places of the games behind it. A game a job refuses makes
   its experiment ``refused``, with the reason. A member's experiment with no owned item yet waits up to
   ``OWNER_SECONDS`` for it (``citar lab submit`` and the A/B route write the queue's file before their item is
   committed), so its games are never charged to nobody.
3. **Executors** (``citar.jobs.local``'s, and the helpers' from 4-10) refresh their settings and hand over their
   frames: accepts, progress, results, failures, aborts. A host that died fails its runs ``memory`` or ``host``.
4. **Expiry** (``_expire``): an offer unanswered after ``OFFER_TTL_SECONDS``, a run past its deadline plus
   ``EXPIRY_GRACE_SECONDS`` (paused time extending it, up to twice), and a run silent for ``SILENT_SECONDS`` while its
   executor is there and not paused, are ``expired`` and their jobs queued again; an expiry its pause caused counts
   neither as an attempt nor as an abandon, and nor does an executor's own ``deadline`` failure its pause caused (the
   job host ends a run held past twice its deadline itself, before the server's backstop). A run's paused time is
   what it reported, with the time since a report that said it was held, or the time a benchmark held its executor,
   whichever is more (``_paused``).
5. **Benchmarks:** while a benchmark lease is live on an executor's machine it is offered nothing and its games are
   paused (``pause``, whatever ``yield_to_llm`` says), and resumed at the lease's end (P3.3.6, P4.5.5).
6. **Matching** (``_match``), unless the lab is paused (``citar lab stop``, the Lab page's Stop: running games
   finish): each executor with a free thread, available (on, ready, outside its machine's restricted hours, no
   benchmark, not backing off), takes the best queued job it does not refuse (``refuses``: another jobs build, a kind
   its settings leave out, more memory a run than it has, P4.3.6), highest priority first and round-robin across owners
   within a priority. Its deadline is ``deadline_s`` of the job's reference cost and the executor's share.
7. **Completion:** an experiment whose games have all settled moves to done/, as the runner did.

**Results** (P4.5.7, ``_accept``): checked through the binding (the result hash recomputed, the chain folded, the
result held to its spec: ``engine_api.job_result_fits``), then **committed first** (the job ``accepted``, or
``crashed`` for a crash outcome, which is final), then **appended** to the results file once per (experiment, game,
run), then the run's stored result pruned. When the server starts, every active experiment's results file is
reconciled with the database (``_reconcile``): an accepted job with no line gets one, a line whose run's job is not
accepted with that run is removed, a line cut short by a crash is dropped. A crash between the commit and the append
therefore leaves exactly one line after the restart. Each step after the commit stands alone: a line the results file
refused (a sync client's lock past ``fsutil.retry``) waits and is tried again each tick, while the run's event, its
cost and its usage rows are written all the same; the game counts as settled once its line is written. Each finished
run writes one ``cpu`` event through the ledger's writer: the executor's machine and its owner (none for the host) as
the provider, the job's owner as the consumer (none for the server's own collection, ranking and reference runs), its
CPU the executor's report bounded by what the server observed (``observed x 1.05 + 1`` seconds: a game is one
thread). The usage ledger gets the game's activity and the time its machine held it, as the runner wrote them.

**Failures** (``_failed``): a retryable one queues the job again, at most ``MAX_ATTEMPTS`` times (then ``failed``,
shown on the Lab page, and queued again at the next start or by an administrator's retry); a ``memory`` one (the
run's host died of memory) raises the job's ``mem_mb`` to the peak its run reported, past the executor's memory a run
when it reported none or was its host's only run (then it alone needed more than the host had), and does not count as
an attempt: it changes where the job may run. Memory figures are the dispatcher's own where an executor's could lie: a
remote run's peak raises a job at most past that executor's memory a run, only the reference machines' peaks enter
the cost table, and every figure is held under ``MEM_MAX``.

Replaces: 0.1.5's lab runner (``lab.run``), a separate Windows-only process that spawned one Python process per game,
kept a job file per game and a status file, adopted the games an earlier runner left through PowerShell and killed
them by pid; and ``ledger.ingest_lab``, which charged all lab CPU to the host as ``verified`` whoever played it.
"""
from __future__ import annotations

import json
import logging
import math
import os
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional

from citar import engine_api, fsutil, lab
from citar.jobs import host as H, store

log = logging.getLogger("citar.jobs.dispatcher")

#: How often the dispatcher wakes when nothing pokes it.
TICK_SECONDS = 2.0
#: How many jobs wait ahead per thread of the executors (P4.5.5); the window never holds more than twice that.
WINDOW = 4
#: Retryable failures before a job is ``failed``.
MAX_ATTEMPTS = 3
#: An offer with neither an accept nor a reject within this long is expired (P4.2.5).
OFFER_TTL_SECONDS = 30.0
#: A running run with no progress for this long, its executor there and not paused, is expired (P4.2.5).
SILENT_SECONDS = 300.0
#: The server's backstop beyond a run's deadline (the executor enforces the deadline itself).
EXPIRY_GRACE_SECONDS = 60.0
#: A deadline is ten times the expected time, within these bounds (P4.5.5).
DEADLINE_FACTOR, DEADLINE_MIN, DEADLINE_MAX = 10.0, 300.0, 6 * 3600.0
#: A machine with this many expired or abandoned runs within the window gets no offer for the window (P4.5.5).
ABANDONS, ABANDON_WINDOW = 3, 3600.0
#: The observed bound on a run's CPU seconds: a game is one thread (P4.5.7).
OBSERVED_FACTOR, OBSERVED_SLACK = 1.05, 1.0
#: A CPU job is worth at most this many times its reference cost (P4.5.7).
REFERENCE_CAP = 1.5
#: How long a member's experiment with no owned item waits for one before its games start (its item is committed just
#: after its file is written), looked up every tick meanwhile; then how often it is looked up again.
OWNER_SECONDS = 10.0
#: The most memory, in MB, any figure the dispatcher keeps may say (a terabyte): an executor's report beyond it is
#: not a figure (and would not fit the database's integer).
MEM_MAX = 1 << 20
#: An executor that refused a job for a reason that holds for every job is offered nothing for a while: 2 s,
#: doubling to 10 minutes (P4.2.5's re-offer backoff).
REJECT_FIRST, REJECT_MAX = 2.0, 600.0


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


# ---------------------------------------------------------------------------- deadlines

def share(cpu_percent: float, cpus: int, threads: int) -> float:
    """An executor's effective share of a core per game: its CPU percentage of the whole machine spread over its
    threads, at most one core (a game is one thread)."""
    if threads <= 0 or cpus <= 0 or cpu_percent <= 0:
        return 1.0
    return max(1e-3, min(1.0, cpu_percent / 100.0 * cpus / threads))


def deadline_s(cost_cpu_s: float, cpu_percent: float, cpus: int, threads: int, speed: float = 1.0) -> float:
    """A run's deadline (P4.5.5): ten times what it is expected to take, ``clamp(10 x expected, 300 s, 6 h)``, where
    expected is the job's reference cost divided by the executor's share of a core per game and by its measured
    speed (``trust.speed``, 1 until known)."""
    expected = max(0.0, float(cost_cpu_s)) / share(cpu_percent, cpus, threads) / max(1e-3, float(speed or 1.0))
    return float(min(DEADLINE_MAX, max(DEADLINE_MIN, DEADLINE_FACTOR * expected)))


def expires_at(started: float, deadline: float, paused: float) -> float:
    """When the server gives up on a running run: its deadline extended by the time it was paused, up to twice the
    deadline in all, plus the grace (the executor ends it itself at the deadline; this is the backstop)."""
    return started + min(deadline + max(0.0, paused), 2.0 * deadline) + EXPIRY_GRACE_SECONDS


def pause_caused(started: float, deadline: float, paused: float, now: float) -> bool:
    """Whether a run that expired past its deadline did so only because it was paused: its active time (the time
    since it started, less the time paused) is still within its deadline and the grace. Such an expiry is neither an
    attempt nor an abandon (P4.5.5)."""
    return paused > 0 and now - started - paused <= deadline + EXPIRY_GRACE_SECONDS


def bounded_cpu(reported: float, observed: float, reference: Optional[float]) -> tuple:
    """A run's CPU seconds as the ledger counts them (P4.5.7): the executor's report, at most what the server observed
    (``observed x 1.05 + 1``: a game is one thread) and at most ``REFERENCE_CAP`` times the job's reference cost (when
    it has one: a run of the local executor is the reference). Returns (seconds, flags)."""
    flags = {}
    cpu = max(0.0, float(reported or 0.0))
    bound = max(0.0, float(observed or 0.0)) * OBSERVED_FACTOR + OBSERVED_SLACK
    if cpu > bound:
        cpu, flags["overreported_cpu"] = bound, True
    if reference is not None and reference > 0 and cpu > REFERENCE_CAP * reference:
        cpu, flags["capped_cpu"] = REFERENCE_CAP * reference, True
    return cpu, flags


# ---------------------------------------------------------------------------- the view in memory

@dataclass
class Job:
    """A live job (queued or running) as the dispatcher holds it."""
    id: str
    kind: str
    ref: str
    exp: str
    i: int
    owner_id: Optional[str]
    spec: dict
    spec_hash: str
    jobs_build: str
    priority: int
    mem_mb: int
    state: str
    attempts: int
    created: float
    run: Optional[str] = None


@dataclass
class Run:
    """A run in flight on an executor."""
    id: str
    job: str
    executor: Any
    machine: str
    role: str
    state: str                  # offered | running
    offered_at: float
    deadline_s: float
    offered_wall: float
    started_at: Optional[float] = None
    started_wall: Optional[float] = None
    last_progress: Optional[float] = None
    round: Optional[int] = None
    of: Optional[int] = None
    cpu_s: float = 0.0
    wall_s: float = 0.0
    paused_s: float = 0.0           # as its last report gave it
    peak_mb: float = 0.0
    paused: bool = False            # its last report said it was held
    held_since: Optional[float] = None      # when that report arrived (its pause goes on from there)
    bench_since: Optional[float] = None     # when a benchmark began holding its executor, while it does
    bench_s: float = 0.0                    # the benchmarks' holds before that


@dataclass
class Exp:
    """A queued experiment as the dispatcher holds it."""
    name: str
    spec: dict
    owner_id: Optional[str]
    games: int
    priority: int
    submitted: str
    has_job: set = field(default_factory=set)       # games with a job (any state but cancelled)
    settled: set = field(default_factory=set)       # games accepted (or crashed), or with a line from before jobs
    failed: set = field(default_factory=set)
    cursor: int = 0                                 # every game below has a job or settled
    refused: str = ""
    lines: dict = field(default_factory=dict)       # run -> game, for the lines with an executor
    owner_checked: float = 0.0                      # when an experiment with no owner was last looked up
    owner_known: bool = False                       # its item was found (an owner, or the server's collection)
    first_seen: float = 0.0                         # when the dispatcher first read it
    probe: Optional["Job"] = None                   # its next game, which no executor would take now (no row)

    @property
    def kind(self) -> str:
        return lab.kind_of(self.spec)

    def in_grace(self, now: float) -> bool:
        """A member's experiment whose item is not found yet, read within ``OWNER_SECONDS``: its games wait for it."""
        return (self.owner_id is None and not self.owner_known and self.kind not in lab.COLLECTION_KINDS
                and now - self.first_seen < OWNER_SECONDS)

    @property
    def job_kind(self) -> str:
        return lab.job_kind(self.spec)

    @property
    def results(self) -> Path:
        return lab.results_path(self.name, self.kind)

    def missing(self):
        """The games with no job and no line, lowest first (the cursor first moved past those that have one, so a
        long experiment is walked once, not every tick)."""
        self.advance()
        i = self.cursor
        while i < self.games:
            if i not in self.has_job and i not in self.settled:
                yield i
            i += 1

    def advance(self) -> None:
        while self.cursor < self.games and (self.cursor in self.has_job or self.cursor in self.settled):
            self.cursor += 1


class Dispatcher:
    """The dispatcher. ``executors`` are the executors to offer to (the local one by default, made from the host row);
    ``clock`` gives the monotonic time the runs are timed by. ``hooks`` are the tests' (``after_commit(job_id)``: a
    result committed and its line not yet appended)."""

    def __init__(self, executors: Optional[list] = None, *, clock: Callable[[], float] = time.monotonic,
                 local: bool = True):
        self.clock = clock
        if executors is None:
            from citar.jobs.local import LocalExecutor
            executors = [LocalExecutor()] if local else []
        self.executors = list(executors)
        self.costs = store.Costs()
        self.jobs: dict = {}            # job id -> Job (queued or running)
        self.runs: dict = {}            # run id -> Run
        self.exps: dict = {}            # name -> Exp
        self.served: dict = {}          # owner key -> when last served (a counter)
        self._served_n = 0
        self.abandons: dict = defaultdict(deque)
        self.finished: deque = deque()  # wall times of results in the last hour
        self.recovered = False
        self.hooks: dict = {}
        self.ticks = 0
        self.tick_ms = 0.0
        self.published: dict = {"dispatcher": {"alive": False}}
        self._queue_sig = None
        self._paused_exec: set = set()
        self._wake = threading.Event()
        self._stop = threading.Event()
        self._tick_lock = threading.Lock()
        self._thread: Optional[threading.Thread] = None
        self._started = False
        self._prune: list = []
        self._lines: dict = {}          # results path -> set of (game, run) in the file
        self._pending: list = []        # (exp, line, game, run): accepted results whose line the file refused
        self._rejects: dict = {}        # executor -> (offered nothing until, the wait, why)
        self._requests: deque = deque()     # administrators' retries, served at the next tick
        self.jobs_build = engine_api.jobs_build()
        self.started_at = utcnow()
        for e in self.executors:
            bind = getattr(e, "bind", None)
            if bind is not None:
                bind(self.poke)

    # -------------------------------------------------------------- the thread
    def start(self) -> "Dispatcher":
        self._stop.clear()
        self._started = True
        self.ensure_thread()
        from citar.pool import scheduler as prompt_scheduler
        if self.on_benchmark not in prompt_scheduler.benchmark_listeners:
            prompt_scheduler.benchmark_listeners.append(self.on_benchmark)
        return self

    def ensure_thread(self) -> None:
        """Start the thread, or start it again if it died (P3.9.2): a dispatcher that was started (a test's, ticked by
        hand, never is)."""
        if self._stop.is_set() or not self._started:
            return
        if self._thread is None or not self._thread.is_alive():
            if self._thread is not None:
                log.warning("the job dispatcher's thread had stopped; starting it again")
            self._thread = threading.Thread(target=self._loop, name="citar-jobs-dispatcher", daemon=True)
            self._thread.start()

    def poke(self) -> None:
        """Wake the dispatcher now (a job frame, an executor's change, a new experiment)."""
        self._wake.set()

    def on_benchmark(self, server_id: Optional[str]) -> None:
        """The prompt scheduler's word that a benchmark lease on ``server_id`` became live or ended."""
        if any(e.id == server_id for e in self.executors):
            self.poke()

    def _loop(self) -> None:
        while not self._stop.is_set():
            # cleared before the tick, so a poke while it runs (a frame) brings the next one at once
            self._wake.clear()
            try:
                self.tick()
            except Exception:
                log.exception("the job dispatcher's tick failed; it carries on")
            self._wake.wait(TICK_SECONDS)

    def stop(self, timeout: float = 10.0) -> None:
        """Stop the thread and close the executors (the local host ends its runs; the next start plays them again)."""
        self._stop.set()
        self._wake.set()
        from citar.pool import scheduler as prompt_scheduler
        if self.on_benchmark in prompt_scheduler.benchmark_listeners:
            prompt_scheduler.benchmark_listeners.remove(self.on_benchmark)
        if self._thread is not None and self._thread is not threading.current_thread():
            self._thread.join(timeout)
        with self._tick_lock:
            for e in self.executors:
                try:
                    e.close()
                except Exception:
                    log.exception("closing executor %s failed", getattr(e, "id", "?"))
            self.published = {**self.published, "dispatcher": {**self.published.get("dispatcher", {}),
                                                               "alive": False}}

    # -------------------------------------------------------------- one tick
    def tick(self) -> None:
        """One pass (the thread's; tests call it directly)."""
        with self._tick_lock:
            t0 = time.perf_counter()
            from citar import db
            if not db.configured():
                return
            now = self.clock()
            if not self.recovered:
                self.recover()
            self._sync_queue(now)
            self._serve()
            self._owners(now)
            self._fill(now)
            for e in self.executors:
                e.refresh(wanted=bool(self.jobs) or bool(e.in_flight))
            self._drain(now)
            self._expire(now)
            self._benchmarks(now)
            if lab.paused() is None:
                self._match(now)
            self._complete()
            self._flush()
            self.ticks += 1
            self.tick_ms = (time.perf_counter() - t0) * 1000.0
            self._publish(now)

    # -------------------------------------------------------------- start
    def recover(self) -> None:
        """At the server's start: the cost table (seeded where it lacks the committed cells), the local executor's
        runs that were in flight when the last process ended (its host died with it) aborted and their jobs queued
        again, the games that failed for want of attempts queued again with their attempts back to 0 (their failures
        were all retryable: a host's death, a deadline a sleeping laptop passed; 0.1.5's runner too tried them again
        at its next start), the queued jobs given this build's jobs build (a queued job has no result to compare yet),
        every queued experiment loaded and its results file reconciled with the database, and the ledger's events of
        the last runs written where a crash lost them."""
        from sqlalchemy import select, update
        from citar import db
        from citar.db.models import CpuJob, CpuJobRun
        lab._dirs()
        with db.session() as s:
            added = self.costs.load(s)
            if added:
                log.info("the cost table: %d committed cell(s) added", added)
            for run in s.scalars(select(CpuJobRun).where(CpuJobRun.state.in_(("offered", "running")),
                                                         CpuJobRun.executor == "local")):
                run.state, run.ended_at = "aborted", utcnow()
                run.error = {"code": "restart", "message": "the server stopped while it played this run"}
                job = s.get(CpuJob, run.job_id)
                if job is not None and job.state == "running":
                    job.state = "queued"
            # a job fails for good only past MAX_ATTEMPTS retryable failures; one refused for its spec, or failed
            # for good by its executor, fails with fewer and stays failed
            again = s.execute(update(CpuJob).where(CpuJob.state == "failed", CpuJob.item_kind == "lab_experiment",
                                                   CpuJob.attempts >= MAX_ATTEMPTS)
                              .values(state="queued", attempts=0)).rowcount
            s.execute(update(CpuJob).where(CpuJob.state == "queued", CpuJob.jobs_build != self.jobs_build)
                      .values(jobs_build=self.jobs_build))
        if again:
            lab.log(f"{again} game(s) that had failed {MAX_ATTEMPTS} times are queued again (the server started)")
        self._sync_queue(self.clock(), force=True)
        with db.session() as s:
            # an experiment deleted from the queue while the server was down: its waiting and failed jobs go with it
            gone = s.execute(update(CpuJob).where(CpuJob.state.in_(("queued", "failed")),
                                                  CpuJob.item_kind == "lab_experiment",
                                                  CpuJob.item_ref.not_in(list(self.exps) or [""]))
                             .values(state="cancelled")).rowcount
        if gone:
            lab.log(f"{gone} waiting game(s) of experiments no longer queued were cancelled")
        self._repair_events()
        self.recovered = True

    def _repair_events(self) -> None:
        """Write the ``cpu`` event of a recent finished run whose event never reached the ledger (the server ended
        between the job's commit and the writer's batch): the runs that ended within an hour of the last one."""
        from sqlalchemy import exists, func, select
        from citar import db
        from citar.db.models import ComputeEvent, CpuJob, CpuJobRun
        with db.session() as s:
            last = s.scalar(select(func.max(CpuJobRun.ended_at)).where(CpuJobRun.state == "done"))
            if last is None:
                return
            if last.tzinfo is None:
                last = last.replace(tzinfo=timezone.utc)
            since = last.timestamp() - 3600
            rows = []
            q = (select(CpuJobRun, CpuJob).join(CpuJob, CpuJob.id == CpuJobRun.job_id)
                 .where(CpuJobRun.state == "done",
                        CpuJobRun.ended_at >= datetime.fromtimestamp(since, timezone.utc),
                        ~exists().where(ComputeEvent.activity == CpuJob.ref, ComputeEvent.request_id == CpuJobRun.id)))
            for run, job in s.execute(q):
                reference = None if run.executor == "local" else self.costs.of(job.kind, job.spec or {})["cpu_s"]
                rows.append(self._event_row(run.id, job.ref, job.owner_id, run.machine_id, run.executor, run.role,
                                            run.cpu_s, _observed(run), reference, run.ended_at))
        if rows:
            log.info("writing %d CPU event(s) the ledger had not written", len(rows))
            _ledger().events(rows)

    # -------------------------------------------------------------- the queue
    def _queue_signature(self) -> tuple:
        out = []
        try:
            with os.scandir(lab.QUEUE) as it:
                for d in it:
                    if d.name.endswith(".json"):
                        try:
                            st = d.stat()
                        except OSError:
                            continue
                        out.append((d.name, st.st_mtime_ns, st.st_size))
        except OSError:
            pass
        return tuple(sorted(out))

    def _sync_queue(self, now: float, force: bool = False) -> None:
        sig = self._queue_signature()
        if sig == self._queue_sig and not force:
            return
        self._queue_sig = sig
        specs = {e.get("name"): e for e in lab.load_queue() if e.get("name")}
        from citar import db
        with db.session() as s:
            for name, spec in specs.items():
                exp = self.exps.get(name)
                if exp is None:
                    self._load(s, spec)
                elif spec != exp.spec:
                    self._changed(s, exp, spec)
            for name in [n for n in self.exps if n not in specs]:
                if not (lab.QUEUE / f"{name}.json").exists():
                    self._deleted(s, self.exps.pop(name))

    def _owner(self, s, spec: dict) -> tuple:
        """(the consumer of an experiment's jobs, whether that is known): its owned item's owner, or nobody for the
        server's own collection (a ranking or reference run, or an item an administrator marked ``collection``); not
        known while no item registers it (being registered, or queued by hand: then adopted by the server's first
        administrator at its next start)."""
        from citar import items
        if lab.kind_of(spec) in lab.COLLECTION_KINDS:
            return None, True
        row = items.get(s, "lab_experiment", spec["name"])
        if row is None:
            return None, False
        return (None if row.collection else row.owner_id), True

    def _load(self, s, spec: dict) -> None:
        """An experiment newly in the queue (or every one, at the start): its jobs and its results file."""
        from sqlalchemy import select
        from citar.db.models import CpuJob
        owner, known = self._owner(s, spec)
        exp = Exp(name=spec["name"], spec=spec, owner_id=owner, games=int(spec.get("games") or 0),
                  priority=int(spec.get("priority") or 0), submitted=str(spec.get("submitted") or ""),
                  owner_known=known, first_seen=self.clock(), owner_checked=self.clock())
        self.exps[exp.name] = exp
        for job in s.scalars(select(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name,
                                                 CpuJob.state.not_in(("cancelled", "withdrawn")))):
            i = store.game_of(job.ref)
            if i is None:
                continue
            exp.has_job.add(i)
            if owner is not None and job.owner_id is None and job.state in store.LIVE:
                job.owner_id = owner        # made before its item was found (a process that ended meanwhile)
            if job.state in store.SETTLED:
                exp.settled.add(i)
            elif job.state == "failed":
                exp.failed.add(i)
            elif job.state == "queued" and job.id not in self.jobs:
                self.jobs[job.id] = self._view(job, exp)
        self._reconcile(s, exp)
        for j in [j for j in self.jobs.values() if j.exp == exp.name and j.state == "queued" and j.i in exp.settled]:
            # a game with a line from before jobs (or a database started afresh) is played no more
            row = s.get(CpuJob, j.id)
            if row is not None:
                row.state = "cancelled"
            self.jobs.pop(j.id, None)
        exp.advance()

    def _view(self, job, exp: Exp) -> Job:
        created = _utc_ts(job.created_at) if job.created_at else time.time()
        return Job(id=job.id, kind=job.kind, ref=job.ref, exp=exp.name, i=store.game_of(job.ref) or 0,
                   owner_id=job.owner_id, spec=dict(job.spec or {}), spec_hash=job.spec_hash,
                   jobs_build=job.jobs_build, priority=job.priority, mem_mb=int(job.mem_mb or 0), state=job.state,
                   attempts=int(job.attempts or 0), created=created)

    def _changed(self, s, exp: Exp, spec: dict) -> None:
        """A queued experiment's file changed: its priority reaches its waiting jobs (the rest of a queued
        experiment does not change: its games are pinned when it is submitted)."""
        from sqlalchemy import update
        from citar.db.models import CpuJob
        exp.spec, exp.games = spec, int(spec.get("games") or 0)
        priority = int(spec.get("priority") or 0)
        if priority != exp.priority:
            exp.priority = priority
            for j in self.jobs.values():
                if j.exp == exp.name:
                    j.priority = priority
            s.execute(update(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name,
                                           CpuJob.state.in_(store.LIVE)).values(priority=priority))

    def _deleted(self, s, exp: Exp) -> None:
        """An experiment deleted from the queue: its waiting and failed jobs cancelled (so one queued again under its
        name plays its failed games again), its running ones aborted."""
        from sqlalchemy import update
        from citar.db.models import CpuJob
        for j in [j for j in self.jobs.values() if j.exp == exp.name]:
            row = s.get(CpuJob, j.id)
            if row is not None:
                row.state = "cancelled"
            if j.run and j.run in self.runs:
                run = self.runs[j.run]
                run.executor.abort(run.id, "its experiment was deleted")
            self.jobs.pop(j.id, None)
        s.execute(update(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name,
                                       CpuJob.state == "failed").values(state="cancelled"))
        lab.log(f"experiment {exp.name} left the queue: its waiting games were cancelled")

    # -------------------------------------------------------------- results files
    def _read_lines(self, path: Path) -> tuple:
        """A results file's lines (text, parsed or None) and whether its last line was cut short."""
        try:
            raw = path.read_bytes()
        except FileNotFoundError:
            return [], False
        text = raw.decode("utf-8", errors="replace")
        cut = bool(text) and not text.endswith("\n")
        lines = []
        for ln in text.splitlines():
            if not ln.strip():
                continue
            try:
                obj = json.loads(ln)
            except ValueError:
                obj = None
            lines.append((ln, obj if isinstance(obj, dict) else None))
        if cut and lines and lines[-1][1] is None:
            lines.pop()                 # a line the last process did not finish writing
        else:
            cut = False
        return lines, cut

    def _reconcile(self, s, exp: Exp) -> None:
        """The results file of an experiment against the database (P4.5.7): an accepted job with no line gets one
        (from its run's stored result), a line whose run's job is not accepted with that run is removed, a line cut
        short is dropped; a line from before jobs (no ``executor``), or from a run this database does not know (a
        database started afresh), counts its game as settled and stays."""
        from sqlalchemy import select
        from citar.db.models import CpuJob, CpuJobRun
        path = exp.results
        lines, cut = self._read_lines(path)
        jobs = list(s.scalars(select(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name)))
        accepted = {j.accepted_run: j for j in jobs if j.state in store.SETTLED and j.accepted_run}
        known = set()
        if jobs:
            known = {rid for (rid,) in s.execute(select(CpuJobRun.id).where(
                CpuJobRun.job_id.in_([j.id for j in jobs])))}
        keep, dropped, seen = [], 0, set()
        for text, obj in lines:
            run = ((obj or {}).get("executor") or {}).get("run") if obj else None
            if run and run in known and run not in accepted:
                dropped += 1
                continue
            if run and run in seen:
                dropped += 1            # the same run's line twice: once is the rule
                continue
            if run:
                seen.add(run)
            keep.append(text)
            if obj is not None and isinstance(obj.get("i"), int):
                if run:
                    exp.lines[run] = obj["i"]
                if not run or run not in known:
                    exp.settled.add(obj["i"])
        if dropped or cut:
            # rewritten whole (a temporary file, then a replace), its lines ending as the appended ones do
            tmp = path.with_name(path.name + ".tmp")
            tmp.write_bytes("".join(t + "\n" for t in keep).encode("utf-8"))
            fsutil.replace(tmp, path)
            lab.log(f"{exp.name}: {dropped} line(s) whose job was not accepted removed from its results"
                    + (" and a line cut short dropped" if cut else ""))
        self._lines[str(path)] = {(i, r) for r, i in exp.lines.items()}
        for run_id, job in accepted.items():
            if run_id in seen:
                if job is not None:
                    self._prune.append(run_id)
                continue
            run = s.get(CpuJobRun, run_id)
            if run is None or run.result is None:
                # its line is lost and its result pruned: the game is played again
                job.state, job.accepted_run, job.result_hash = "queued", None, None
                exp.settled.discard(store.game_of(job.ref))
                if job.id not in self.jobs:
                    self.jobs[job.id] = self._view(job, exp)
                lab.log(f"{exp.name} #{store.game_of(job.ref)}: its line is missing and its result gone; "
                        "played again")
                continue
            line = self._line(job.kind, exp.name, store.game_of(job.ref) or 0, job.spec or {}, run.result,
                              run.machine_id, run.id, run.executor, run.cpu_s, run.wall_s,
                              run.started_at or run.offered_at, run.ended_at)
            exp.settled.discard(store.game_of(job.ref))      # until its line is written
            self._settle_line(exp, line, store.game_of(job.ref) or 0, run.id)
            lab.log(f"{exp.name} #{store.game_of(job.ref)}: its accepted result's line written at the start")

    @staticmethod
    def _line(kind: str, exp: str, i: int, spec: dict, result: dict, machine: str, run: str, executor: str,
              cpu_s: float, wall_s: float, started, ended) -> dict:
        if isinstance(started, datetime):
            if started.tzinfo is None:
                started = started.replace(tzinfo=timezone.utc)     # the database's times are UTC
            started_ts = started.timestamp()
        else:
            started_ts = started
        if isinstance(ended, datetime):
            if ended.tzinfo is None:
                ended = ended.replace(tzinfo=timezone.utc)
            finished = ended.astimezone().replace(tzinfo=None).isoformat(timespec="seconds")
        else:
            finished = datetime.now().isoformat(timespec="seconds")
        verification = "reference" if executor == "local" else "unverified"
        return lab.result_line(kind, exp, i, spec, result, wall_s=wall_s or 0.0, cpu_s=cpu_s or 0.0,
                               started_ts=started_ts, finished=finished,
                               executor={"machine": machine, "run": run, "verification": verification})

    def _append(self, exp: Exp, line: dict, i: int, run: str) -> bool:
        """Append a line once: skipped when the file already holds one of the same game and run."""
        path = exp.results
        have = self._lines.setdefault(str(path), set())
        if (i, run) in have:
            return False
        self._write_line(path, json.dumps(line) + "\n")
        have.add((i, run))
        exp.lines[run] = i
        return True

    @staticmethod
    def _write_line(path: Path, text: str) -> None:
        """One line onto a results file, in one write, retried while Windows says the file is locked (a sync client
        or a reader holding it a moment, ``fsutil.retry``)."""
        path.parent.mkdir(parents=True, exist_ok=True)

        def write():
            with open(path, "a", encoding="utf-8", newline="\n") as f:
                f.write(text)
        fsutil.retry(write)

    def _settle_line(self, exp: Exp, line: dict, i: int, run: str) -> bool:
        """An accepted result's line written: its game settled and its run's stored result pruned. A line the file
        refused waits on ``_pending``, tried again each tick (``_flush``): its game settles only once it is there, so
        its experiment never moves to done/ without it, and its result is kept meanwhile."""
        try:
            self._append(exp, line, i, run)
        except Exception:
            log.warning("%s #%s: its result's line could not be written; trying again each tick", exp.name, i,
                        exc_info=True)
            if not any(p[3] == run for p in self._pending):
                self._pending.append((exp, line, i, run))
            return False
        exp.settled.add(i)
        self._prune.append(run)
        return True

    # -------------------------------------------------------------- jobs from the queue
    def _threads(self) -> int:
        return sum(int(e.threads) for e in self.executors if getattr(e, "want", {}).get("enabled", True))

    def _order(self) -> list:
        """The queued experiments by priority, highest first, each level's owners round-robin (longest unserved
        first), each owner's by age: the order their games run in."""
        levels: dict = defaultdict(lambda: defaultdict(list))
        for exp in self.exps.values():
            if not exp.refused:
                levels[exp.priority][exp.owner_id or ""].append(exp)
        out = []
        for p in sorted(levels, reverse=True):
            owners = sorted(levels[p], key=lambda o: self.served.get(o, -1))
            out.append((p, [(o, sorted(levels[p][o], key=lambda e: (e.submitted, e.name))) for o in owners]))
        return out

    def _owners(self, now: float) -> None:
        """Look for the owned item of a member's experiment that had none when it was read: every tick within
        ``OWNER_SECONDS`` of first reading it (its games wait meanwhile: ``citar lab submit`` and the A/B route write
        the queue's file just before their item is committed), every ``OWNER_SECONDS`` after (an experiment queued by
        hand, adopted at the next start). Its live jobs, waiting or playing, take the owner found; the view in memory
        changes once the database has."""
        from sqlalchemy import update
        from citar import db
        from citar.db.models import CpuJob
        due = [exp for exp in self.exps.values() if not exp.owner_known and exp.kind not in lab.COLLECTION_KINDS
               and (exp.in_grace(now) or now - exp.owner_checked >= OWNER_SECONDS)]
        if not due:
            return
        found = []
        with db.session() as s:
            for exp in due:
                exp.owner_checked = now
                owner, known = self._owner(s, exp.spec)
                if not known:
                    continue
                found.append((exp, owner))
                if owner is not None:
                    s.execute(update(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name,
                                                   CpuJob.state.in_(store.LIVE)).values(owner_id=owner))
        for exp, owner in found:
            exp.owner_id, exp.owner_known = owner, True
            for j in self.jobs.values():
                if j.exp == exp.name:
                    j.owner_id = owner

    def _takeable(self, job: Job, cache: dict) -> bool:
        """Whether an executor that is on would play ``job`` as things are set now (``refuses``: another jobs build, a
        kind its settings leave out, more memory a run than it has: refusals that last until an administrator changes
        something, unlike a busy thread or a benchmark). With no executor on, every job is (nothing plays, and the
        window fills as it would for one). Cached per tick in ``cache`` by what an executor's ``refuses`` may look at
        (the executor interface: a job's kind, jobs build, memory and owner), so a tick with thousands of jobs of a
        few shapes asks each executor a few times."""
        key = (job.kind, job.jobs_build, job.mem_mb, job.owner_id)
        hit = cache.get(key)
        if hit is None:
            on = [e for e in self.executors if getattr(e, "want", {}).get("enabled", True)]
            hit = cache[key] = not on or any(e.refuses(job) is None for e in on)
        return hit

    @staticmethod
    def _games(exps: list, now: float, stuck: set):
        """An owner's games next in line: each experiment's missing games, oldest experiment first, leaving out an
        experiment that waits for its owner, one a job refuses and one whose next game no executor would take."""
        for e in exps:
            if e.in_grace(now):
                continue
            for i in e.missing():
                if e.refused or e.name in stuck:
                    break
                yield e, i

    def _fill(self, now: float) -> None:
        """Make jobs of the games next in line (the window, P4.5.5). Only the jobs an executor would take hold a
        place: a job no executor would take (its memory raised past every executor's by a death) keeps its row and
        waits, saying why, but the games behind it take its place; a game no executor would take gets no row (its
        experiment's ``probe`` says why, and its later games wait behind it). The view in memory takes the rows once
        the database has them."""
        cap = WINDOW * max(1, self._threads())
        cache: dict = {}
        queued: dict = defaultdict(lambda: defaultdict(int))
        n = 0
        for j in self.jobs.values():
            if j.state == "queued" and self._takeable(j, cache):
                queued[j.priority][j.owner_id or ""] += 1
                n += 1
        if n >= 2 * cap:
            return
        budget = 2 * cap - n
        pos, made, stuck, probes = 0, [], set(), {}
        from citar import db
        with db.session() as s:
            for p, owners in self._order():
                iters = []
                for owner, exps in owners:
                    iters.append([queued[p].get(owner, 0), self._games(exps, now, stuck)])
                while iters and pos < cap and budget > 0:
                    for it in list(iters):
                        if pos >= cap or budget <= 0:
                            break
                        if it[0] > 0:
                            it[0] -= 1
                            pos += 1
                            continue
                        nxt = next(it[1], None)
                        if nxt is None:
                            iters.remove(it)
                            continue
                        exp, i = nxt
                        made_job = self._new_job(exp, i)
                        if made_job is None:
                            continue
                        job, row = made_job
                        if not self._takeable(job, cache):
                            stuck.add(exp.name)
                            probes[exp.name] = job
                            continue
                        s.add(row)
                        made.append((exp, job))
                        pos += 1
                        budget -= 1
                if pos >= cap or budget <= 0:
                    break
        for exp, job in made:
            self.jobs[job.id] = job
            exp.has_job.add(job.i)
            exp.advance()
            exp.probe = None
        for name, job in probes.items():
            self.exps[name].probe = job

    def _new_job(self, exp: Exp, i: int) -> Optional[tuple]:
        """A job of game ``i`` and its row, not yet added to the database (None for a game a job refuses: its
        experiment is then ``refused``, saying why)."""
        from citar.db.models import CpuJob, new_id
        kind, spec = lab.job_of(exp.spec, i)
        try:
            checked = engine_api.check_job_spec(kind, spec)
        except engine_api.JobRefused as e:
            if not exp.refused:
                exp.refused = f"Game {i} of experiment {exp.name} cannot be played: {e}"
                lab.log(f"{exp.refused}; none of its games starts (delete the experiment and queue it again)")
            return None
        except Exception as e:      # an experiment too broken to make specs of
            if not exp.refused:
                exp.refused = f"Game {i} of experiment {exp.name} cannot be made: {type(e).__name__}: {e}"
                lab.log(exp.refused)
            return None
        cost = self.costs.of(kind, checked["spec"])
        mem = min(MEM_MAX, int(math.ceil(cost["peak_mb"])))
        row = CpuJob(id=new_id("cj_"), kind=kind, ref=f"lab:{exp.name}:{i}"[:160], item_kind="lab_experiment",
                     item_ref=exp.name[:120], owner_id=exp.owner_id, spec=checked["spec"],
                     spec_hash=checked["spec_hash"], jobs_build=self.jobs_build, priority=exp.priority,
                     mem_mb=mem, state="queued", attempts=0, created_at=utcnow())
        job = Job(id=row.id, kind=kind, ref=row.ref, exp=exp.name, i=i, owner_id=exp.owner_id, spec=row.spec,
                  spec_hash=row.spec_hash, jobs_build=row.jobs_build, priority=row.priority, mem_mb=mem,
                  state="queued", attempts=0, created=time.time())
        return job, row

    # -------------------------------------------------------------- frames
    def _drain(self, now: float) -> None:
        frames = []
        for e in self.executors:
            try:
                for f in e.poll():
                    frames.append((e, f))
            except Exception:
                log.exception("reading executor %s failed", getattr(e, "id", "?"))
        if not frames:
            return
        from citar import db
        after: list = []
        try:
            with db.session() as s:
                for e, f in frames:
                    try:
                        self._frame(s, e, f, now, after)
                    except Exception:
                        log.exception("a frame of executor %s could not be handled: %s", e.id, str(f)[:300])
        except Exception:
            self._reset("the database refused the runs' changes")
            raise
        for fn in after:
            try:
                fn()
            except Exception:
                log.exception("a result's writing after its commit failed")

    def _frame(self, s, e, f: dict, now: float, after: list) -> None:
        t, run_id = f.get("t"), f.get("run")
        run = self.runs.get(run_id)
        if run is None or run.executor is not e:
            if t == "job_result":
                log.info("a late result for run %s on %s, dropped", run_id, e.id)
            return
        # when the frame arrived, in the dispatcher's clock and as a wall time: it may be read a tick later
        at = H.arrived(f, now)
        wall = time.time() - (now - at)
        if t == "job_accept":
            run.state, run.started_at, run.started_wall, run.last_progress = "running", at, wall, at
            row = self._run_row(s, run)
            row.state, row.started_at = "running", _utc(wall)
        elif t == "job_progress":
            run.last_progress = at
            run.round, run.of = f.get("round"), f.get("of")
            for k in ("cpu_s", "paused_s", "peak_mb"):
                if isinstance(f.get(k), (int, float)):
                    v = max(0.0, float(f[k]))
                    setattr(run, k, max(run.peak_mb, min(v, float(MEM_MAX))) if k == "peak_mb" else v)
            if isinstance(f.get("elapsed_s"), (int, float)):
                run.wall_s = float(f["elapsed_s"])
            run.paused = bool(f.get("paused"))
            # a report that says it is held covers its pause up to its arrival; the pause goes on from there
            run.held_since = at if run.paused else None
        elif t == "job_result":
            self._accept(s, e, run, f, at, wall, after)
        elif t == "job_failed":
            self._failed(s, e, run, f, at, wall)
        elif t == "job_reject":
            self._rejected(s, e, run, f, wall)
        elif t == "job_aborted":
            self._end(s, run, "aborted", {"code": "aborted", "message": "aborted"}, wall)
            self._requeue(s, run, attempt=False)

    def _run_row(self, s, run: Run):
        from citar.db.models import CpuJobRun
        row = s.get(CpuJobRun, run.id)
        if row is None:
            raise RuntimeError(f"run {run.id} has no row")
        return row

    def _end(self, s, run: Run, state: str, error: Optional[dict], wall: Optional[float] = None) -> None:
        """A run's row ended at ``wall`` (when its end arrived; now when not given)."""
        row = self._run_row(s, run)
        row.state, row.ended_at = state, _utc(wall) if wall is not None else utcnow()
        row.cpu_s, row.wall_s, row.paused_s, row.peak_mb = run.cpu_s, run.wall_s, run.paused_s, run.peak_mb
        if error is not None:
            row.error = error
        self.runs.pop(run.id, None)

    @staticmethod
    def _paused(run: Run, at: float) -> float:
        """How long ``run`` has been paused by ``at``: what it reported, with the time since a report that said it was
        held (a pause going on: the job host reports a held run only every ten seconds), or how long benchmarks held
        its executor, whichever is more. A run held past its deadline then has its pause counted though no report
        said so (P4.5.5: an expiry its pause caused never counts)."""
        reported = run.paused_s
        if run.paused and run.held_since is not None:
            reported += max(0.0, at - run.held_since)
        bench = run.bench_s + (max(0.0, at - run.bench_since) if run.bench_since is not None else 0.0)
        return max(reported, bench)

    def _requeue(self, s, run: Run, *, attempt: bool, mem_mb: Optional[int] = None) -> None:
        """A run ended without a result: its job waits again (``failed`` past ``MAX_ATTEMPTS`` when ``attempt``)."""
        from citar.db.models import CpuJob
        job = self.jobs.get(run.job)
        row = s.get(CpuJob, run.job)
        if job is None or row is None:
            return
        job.run = None
        if mem_mb is not None and mem_mb > job.mem_mb:
            job.mem_mb = row.mem_mb = int(mem_mb)
        if attempt:
            job.attempts = row.attempts = (row.attempts or 0) + 1
        if row.state == "cancelled":
            self.jobs.pop(job.id, None)
            return
        if job.attempts >= MAX_ATTEMPTS:
            row.state = "failed"
            self.jobs.pop(job.id, None)
            exp = self.exps.get(job.exp)
            if exp is not None:
                exp.failed.add(job.i)
            lab.log(f"{job.exp} #{job.i} failed {job.attempts} times; given up on")
            return
        job.state = row.state = "queued"

    def _failed(self, s, e, run: Run, f: dict, at: float, wall: Optional[float] = None) -> None:
        code = str(f.get("code") or "internal")
        message = str(f.get("message") or "")[:1000]
        local = e.kind == "local"
        if isinstance(f.get("peak_mb"), (int, float)):
            run.peak_mb = max(run.peak_mb, min(max(0.0, float(f["peak_mb"])), float(MEM_MAX)))
        paused = self._paused(run, at)
        run.paused_s = max(run.paused_s, paused)
        caused = (code == "deadline" and run.started_at is not None
                  and pause_caused(run.started_at, run.deadline_s, paused, at))
        error = {"code": code, "message": message}
        if caused:
            error["pause_caused"] = True
        self._end(s, run, "failed", error, wall)
        job = self.jobs.get(run.job)
        if code == "memory":
            self._memory_death(s, e, run, f, job, local)
            return
        if caused:
            # the job host ends a run held past twice its deadline itself, before the server's backstop: its pause
            # (a benchmark's) ran it out of time, not its play, so neither an attempt nor an abandon (P4.5.5)
            self._requeue(s, run, attempt=False)
            if job is not None:
                lab.log(f"{job.exp} #{job.i} on {e.name}: held by a pause past its deadline ({message}); queued "
                        "again, not counted as an attempt")
            return
        retryable = bool(f.get("retryable", True))
        if retryable:
            self._requeue(s, run, attempt=True)
        else:
            from citar.db.models import CpuJob
            row = s.get(CpuJob, run.job)
            if row is not None:
                row.state = "failed"
            if job is not None:
                self.jobs.pop(job.id, None)
                exp = self.exps.get(job.exp)
                if exp is not None:
                    exp.failed.add(job.i)
        if job is not None:
            lab.log(f"{job.exp} #{job.i} failed on {e.name} ({code}): {message.splitlines()[0] if message else ''}")

    def _memory_death(self, s, e, run: Run, f: dict, job: Optional[Job], local: bool) -> None:
        """A run whose host died of memory: its job's ``mem_mb`` raised to the largest peak the run reported (its
        progress, its failure), and past the executor's memory a run when it reported none or was its host's only
        run (then it alone needed more than the host had, P4.4.3). The executor's memory a run is the dispatcher's
        own figure for it; only the local executor's frames say which runs shared a dead host (``alone``,
        ``per_run_mb``: its host is gone, and with it the figure). A remote run's peak is its helper's word, so it
        raises its job at most past that executor's memory a run: a helper cannot price a job out of every machine.
        Not an attempt while it raises ``mem_mb``; one that raises nothing is, so a job cannot go round for ever."""
        own = e.memory_per_run_mb
        per_run = int((f.get("per_run_mb") if local else None) or own or 0)
        peak = run.peak_mb if run.peak_mb > 0 else None
        want = int(math.ceil(peak)) if peak is not None else per_run + 1
        if local and f.get("alone"):
            want = max(want, per_run + 1)
        if not local and own:
            want = min(want, int(own) + 1)
        want = max(0, min(want, MEM_MAX))
        grew = job is not None and want > job.mem_mb
        self._requeue(s, run, attempt=not grew, mem_mb=want)
        log.info("run %s of %s died of memory (peak %s MB); its job now needs %s MB a run", run.id,
                 job.ref if job else run.job, peak, max(want, job.mem_mb if job else 0))

    def _rejected(self, s, e, run: Run, f: dict, wall: Optional[float] = None) -> None:
        reason = str(f.get("reason") or "")
        self._end(s, run, "failed", {"code": "rejected", "reason": reason, "message": str(f.get("message"))[:1000]},
                  wall)
        job = self.jobs.get(run.job)
        if reason not in ("busy", "memory", "bad_spec"):
            # the executor refuses every job for now (unverifiable, another build): offered nothing for a while,
            # 2 s doubling to 10 minutes (P4.2.5), until it sends a result
            _until, wait, _why = self._rejects.get(e.id, (0.0, REJECT_FIRST / 2, ""))
            wait = min(REJECT_MAX, wait * 2)
            self._rejects[e.id] = (self.clock() + wait, wait, f"it refused a job ({reason}): {f.get('message')}")
        if reason == "bad_spec":
            from citar.db.models import CpuJob
            row = s.get(CpuJob, run.job)
            if row is not None:
                row.state = "failed"
            if job is not None:
                self.jobs.pop(job.id, None)
                exp = self.exps.get(job.exp)
                if exp is not None:
                    exp.failed.add(job.i)
                lab.log(f"{job.exp} #{job.i}: {e.name} refused its spec: {f.get('message')}")
            return
        self._requeue(s, run, attempt=False)

    def _accept(self, s, e, run: Run, f: dict, at: float, wall: float, after: list) -> None:
        """A result: checked, committed (the job accepted), then its line appended once, the run's stored result
        pruned, its ledger event, its cost and its usage rows (P4.5.7). ``wall`` is when it arrived: the end of what
        the server observed of the run, from its accept's arrival (its offer's, when no accept was seen)."""
        from citar.db.models import CpuJob
        job = self.jobs.get(run.job)
        row = s.get(CpuJob, run.job)
        outcome = f.get("outcome") or {}
        result, chain = outcome.get("result"), outcome.get("chain") or {}
        for k in ("cpu_s", "wall_s", "paused_s", "peak_mb"):
            if isinstance(outcome.get(k), (int, float)):
                v = max(0.0, float(outcome[k]))
                setattr(run, k, max(run.peak_mb, min(v, float(MEM_MAX))) if k == "peak_mb" else v)
        if job is None or row is None or row.state == "cancelled":
            self._end(s, run, "done", None, wall)
            return
        try:
            if not isinstance(result, dict):
                raise engine_api.JobRefused("the result is no object")
            got = engine_api.job_result_hash(job.kind, result)
            if got != outcome.get("result_hash"):
                raise engine_api.JobRefused(f"its result hashes to {got}, not {outcome.get('result_hash')}")
            engine_api.job_result_fits(job.kind, job.spec, result, chain, job.spec_hash)
        except (engine_api.JobRefused, ValueError) as err:
            # a correct executor never sends one: an internal fault of the local executor's, played again
            log.error("run %s of %s on %s gave a result that does not check: %s", run.id, job.ref, e.id, err)
            self._failed(s, e, run, {"code": "internal", "message": f"its result does not check: {err}",
                                     "retryable": True}, at, wall)
            return
        crash = "crash" in result
        local = e.kind == "local"
        rrow = self._run_row(s, run)
        rrow.state, rrow.ended_at = "done", _utc(wall)
        rrow.cpu_s, rrow.wall_s, rrow.paused_s, rrow.peak_mb = run.cpu_s, run.wall_s, run.paused_s, run.peak_mb
        rrow.result, rrow.result_hash = result, outcome.get("result_hash")
        rrow.chain_head, rrow.rounds = chain.get("head"), chain.get("rounds")
        rrow.verdict = "reference" if local else "unchecked"
        row.state = "crashed" if crash else "accepted"
        row.accepted_run, row.result_hash = run.id, outcome.get("result_hash")
        self.runs.pop(run.id, None)
        self.jobs.pop(job.id, None)
        ended = rrow.ended_at
        started_wall = run.started_wall or run.offered_wall
        observed = max(0.0, wall - started_wall)
        exp = self.exps.get(job.exp)
        line = self._line(job.kind, job.exp, job.i, job.spec, result, run.machine, run.id, e.kind, run.cpu_s,
                          run.wall_s, started_wall, ended)
        event = self._event_row(run.id, job.ref, job.owner_id, run.machine, e.kind, run.role, run.cpu_s, observed,
                                None if local else self.costs.of(job.kind, job.spec)["cpu_s"], ended)
        self._rejects.pop(e.id, None)

        def crash_log():
            text = line.get("crash") or ""
            with open(lab.LAB / "crashes.log", "a", encoding="utf-8") as fh:
                fh.write(f"--- {datetime.now():%Y-%m-%d %H:%M} {job.exp} #{job.i} final\n{text}\n")
            lab.log(f"{job.exp} #{job.i} crashed (final: not played again): {str(text).splitlines()[0]}")

        def played_log():
            if job.kind == "reference_game":
                lab.log(f"{job.exp} #{job.i}: T{result.get('turns')} {result.get('victory')} "
                        f"({run.wall_s:.0f}s on {e.name})")
            else:
                lab.log(f"{job.exp} #{job.i}: T{result.get('turns')} {result.get('victory')} won by "
                        f"{result.get('winner_label')} ({run.wall_s:.0f}s on {e.name})")

        def written():
            hook = self.hooks.get("after_commit")
            if hook is not None:
                hook(job.id)        # a test's crash here leaves everything below undone, as a dead server would
            # each step alone: a line the file refuses waits for the next tick, and the rest is written all the same
            if exp is not None:
                self._settle_line(exp, line, job.i, run.id)
            else:
                self._prune.append(run.id)
            _step(f"{job.ref}'s CPU event", lambda: _ledger().events([event]))
            # a counted peak enters the cost table from a reference machine only, as its CPU does: a helper's figures
            # are its own word (4-11 adds the verifiers')
            _step(f"{job.ref}'s cost", lambda: self.costs.record(
                job.kind, job.spec, cpu_s=run.cpu_s if local else None,
                peak_mb=(run.peak_mb or None) if local else None))
            self.finished.append(time.time())
            _step(f"{job.ref}'s usage rows", lambda: self._usage(job, line, run.machine))
            _step(f"{job.ref}'s log", crash_log if crash else played_log)
        # after the commit (the session's end): the order of writing
        after.append(written)

    def _usage(self, job: Job, line: dict, machine: str) -> None:
        try:
            from citar import usage
            usage.append("lab", usage.lab_game_rows(job.exp, job.i, line, machine))
        except Exception:
            log.warning("could not write the usage ledger for %s", job.ref, exc_info=True)

    @staticmethod
    def _event_row(run_id: str, activity: str, consumer: Optional[str], machine: str, executor: str, role: str,
                   cpu_s: float, observed: float, reference: Optional[float], at) -> dict:
        """A finished run's ``cpu`` event (P4.5.7): given by the executor's machine's owner (nobody for the host),
        used by the job's owner for a primary run and by nobody for a replica or a tiebreak, its CPU bounded. Its
        ``request_id`` is the run's, so a run has one event (``_repair_events``)."""
        from citar import ledger as compute, servers
        facts = servers.machine_facts(machine) or {}
        provider = None if executor == "local" or facts.get("server_resource") or not facts else facts.get("owner_id")
        consumer = consumer if role == "primary" else None
        cpu, flags = bounded_cpu(cpu_s, observed, reference)
        cls = compute.class_of(facts, "cpu", _ledger().classes())
        if isinstance(at, datetime) and at.tzinfo is None:
            at = at.replace(tzinfo=timezone.utc)
        return {"at": at if isinstance(at, datetime) else utcnow(), "activity": activity[:160],
                "activity_kind": "lab", "task_class": "cpu", "consumer_id": consumer, "provider_id": provider,
                "machine_id": (machine or "host")[:40], "kind": "cpu", "model": None, "gen_s": 0.0,
                "cpu_core_s": round(cpu, 6), "reported_s": round(float(cpu_s or 0.0), 6),
                "observed_s": round(float(observed or 0.0), 3), "tokens_in": 0, "tokens_out": 0,
                "tokens_reasoning": 0, "tokens_cache_read": 0, "tokens_cache_write": 0,
                "credits": round(compute.credits_for("cpu", cpu_core_s=cpu, rate=cls["rate"] if cls else 0.0), 6),
                "flags": flags, "self_use": consumer == provider,
                "verification": "verified" if executor == "local" else "pending",
                "request_id": run_id[:64]}

    # -------------------------------------------------------------- expiry
    def _expire(self, now: float) -> None:
        expired = []
        for run in list(self.runs.values()):
            e = run.executor
            if run.state == "offered" and now - run.offered_at > OFFER_TTL_SECONDS:
                expired.append((run, "no answer to its offer within 30 s", True))
            elif run.state == "running" and run.started_at is not None:
                paused = self._paused(run, now)
                if now > expires_at(run.started_at, run.deadline_s, paused):
                    caused = pause_caused(run.started_at, run.deadline_s, paused, now)
                    run.paused_s = max(run.paused_s, paused)
                    expired.append((run, "past its deadline" + (" (its pauses)" if caused else ""), not caused))
                elif (getattr(e, "online", True) and not run.paused and run.bench_since is None
                      and run.last_progress is not None and now - run.last_progress > SILENT_SECONDS):
                    expired.append((run, "no progress for 5 minutes", True))
        if not expired:
            return
        from citar import db
        try:
            with db.session() as s:
                for run, why, counts in expired:
                    run.executor.abort(run.id, why)
                    self._end(s, run, "expired", {"code": "expired", "message": why})
                    self._requeue(s, run, attempt=counts)
                    if counts and run.executor.kind != "local":
                        self._abandoned(s, run.machine, now)
                    job = self.jobs.get(run.job)
                    lab.log(f"{job.exp + ' #' + str(job.i) if job else run.job} on {run.executor.name}: expired, "
                            f"{why}")
        except Exception:
            self._reset("the database refused the expiries")
            raise

    def _reset(self, why: str) -> None:
        """The database refused changes the view in memory already made: the executors are closed (their runs
        stop) and the view is rebuilt from the database at the next tick, as at a start."""
        log.error("the job dispatcher starts again from the database: %s", why)
        for e in self.executors:
            try:
                e.close()
            except Exception:
                log.exception("closing executor %s failed", getattr(e, "id", "?"))
        self.jobs.clear()
        self.runs.clear()
        self.exps.clear()
        self._lines.clear()
        self._queue_sig = None
        self.recovered = False

    def _abandoned(self, s, machine: str, now: float) -> None:
        """An expiry or abandon of a helper machine's run (P4.5.5): counted on its row (``trust.abandons``), and three
        within the hour keep it from offers for the hour."""
        from citar.db.models import Server
        q = self.abandons[machine]
        q.append(now)
        row = s.get(Server, machine)
        if row is not None:
            trust = dict(row.trust or {})
            trust["abandons"] = int(trust.get("abandons") or 0) + 1
            row.trust = trust

    # -------------------------------------------------------------- availability and matching
    def _available(self, e, now: float) -> Optional[str]:
        """Why an executor is offered nothing now (None: it may be)."""
        if e.state != "ready":
            return e.reason or f"its job host is {e.state}"
        if e.id in self._paused_exec:
            return "a benchmark holds its machine"
        until, _wait, why = self._rejects.get(e.id, (0.0, 0.0, ""))
        if now < until:
            return why
        from citar import servers
        try:
            end = servers.restricted_now(e.id)
        except Exception:
            end = None
        if end is not None:
            return f"its machine's restricted hours, until {end:%H:%M}"
        q = self.abandons.get(e.id)
        if q:
            while q and now - q[0] > ABANDON_WINDOW:
                q.popleft()
            if len(q) >= ABANDONS:
                return f"{len(q)} of its runs expired within the hour"
        return None

    def _benchmarks(self, now: float) -> None:
        """Pause an executor's games while a benchmark holds its machine, and resume them at the end (P4.5.5)."""
        from citar.pool import scheduler as prompt_scheduler
        sched = prompt_scheduler.current()
        for e in self.executors:
            live = bool(sched is not None and sched.benchmark_live(e.id))
            if live and e.id not in self._paused_exec:
                self._paused_exec.add(e.id)
                e.pause()
                for run in self.runs.values():
                    if run.executor is e and run.bench_since is None:
                        run.bench_since = now
                lab.log(f"a benchmark holds {e.name}: its games are paused")
            elif not live and e.id in self._paused_exec:
                self._paused_exec.discard(e.id)
                e.resume()
                for run in self.runs.values():
                    if run.executor is e and run.bench_since is not None:
                        run.bench_s += max(0.0, now - run.bench_since)
                        run.bench_since = None
                lab.log(f"the benchmark on {e.name} ended: its games go on")

    def _match(self, now: float) -> None:
        """Offer queued jobs to executors with free threads: the highest priority first, round-robin across owners
        within it, each executor the first job it does not refuse."""
        ready = [e for e in self.executors if e.free > 0 and self._available(e, now) is None]
        queued = [j for j in self.jobs.values() if j.state == "queued"]
        if not ready or not queued:
            return
        levels: dict = defaultdict(lambda: defaultdict(list))
        for j in sorted(queued, key=lambda j: (j.created, j.i)):
            levels[j.priority][j.owner_id or ""].append(j)
        offers = []
        for e in ready:
            for _ in range(e.free):
                job = self._pick(e, levels)
                if job is None:
                    break
                offers.append((e, job))
        if not offers:
            return
        from citar import db
        from citar.db.models import CpuJob, CpuJobRun, new_id
        made, gone = [], []
        with db.session() as s:
            for e, job in offers:
                row = s.get(CpuJob, job.id)
                if row is None:
                    gone.append(job)
                    continue
                cost = self.costs.of(job.kind, job.spec)["cpu_s"]
                dl = deadline_s(cost, e.cpu_percent, e.cpus, e.threads, e.speed)
                run = Run(id=new_id("cr_"), job=job.id, executor=e, machine=e.id, role="primary", state="offered",
                          offered_at=now, deadline_s=dl, offered_wall=time.time())
                at = utcnow()
                s.add(CpuJobRun(id=run.id, job_id=job.id, machine_id=e.id[:40], executor=e.kind, role="primary",
                                state="offered", offered_at=at,
                                deadline_at=datetime.fromtimestamp(at.timestamp() + dl, timezone.utc)))
                row.state = "running"
                made.append((e, job, run))
        # committed: the view in memory takes the offers, and only then do the executors get them (a database that
        # refused them leaves every job waiting, as it was)
        for job in gone:
            self.jobs.pop(job.id, None)
        for e, job, run in made:
            job.state, job.run = "running", run.id
            self.runs[run.id] = run
            try:
                e.offer(run.id, job, run.deadline_s)
            except Exception:       # never answered: it expires after OFFER_TTL_SECONDS, and the others still go
                log.exception("offering run %s to executor %s failed", run.id, getattr(e, "id", "?"))

    def _pick(self, e, levels: dict) -> Optional[Job]:
        for p in sorted(levels, reverse=True):
            owners = sorted(levels[p], key=lambda o: self.served.get(o, -1))
            for owner in owners:
                jobs = levels[p][owner]
                for k, job in enumerate(jobs):
                    if e.refuses(job) is None:
                        del jobs[k]
                        self._served_n += 1
                        self.served[owner] = self._served_n
                        return job
        return None

    @staticmethod
    def label(e) -> str:
        """An executor as the dispatcher's words name it: the server's own job host is "this server" (its machine's
        name is administrators'), a helper by its machine's name (the routes rename it for each viewer, from
        ``waiting_on``)."""
        return "this server" if e.kind == "local" else e.name

    def waiting(self, job: Job, now: Optional[float] = None) -> str:
        """Why a queued job is not playing: each executor's reason, or that it is next in line."""
        return self._waiting(job, now)[0]

    def _waiting(self, job: Job, now: Optional[float] = None) -> tuple:
        """(``waiting``'s words, the executors' reasons as ``{machine, kind, why}`` for the routes to name each
        executor as the viewer may be told it)."""
        now = self.clock() if now is None else now
        if lab.paused() is not None:
            return "the lab is paused", []
        parts = []
        for e in self.executors:
            why = self._available(e, now) or e.refuses(job)
            if why is None and e.free <= 0:
                why = "every thread is playing"
            if why is not None:
                parts.append((e, why))
        if len(parts) < len(self.executors):
            return "next in line", []
        if not parts:
            return "no executor takes CPU work", []
        return ("; ".join(f"{self.label(e)}: {why}" for e, why in parts),
                [{"machine": e.id, "kind": e.kind, "why": why} for e, why in parts])

    # -------------------------------------------------------------- completion
    def _complete(self) -> None:
        live = {j.exp for j in self.jobs.values()}
        for name, exp in list(self.exps.items()):
            if exp.refused or name in live or exp.games <= 0:
                continue
            if all(i in exp.settled for i in range(exp.games)):
                src = lab.QUEUE / f"{name}.json"
                try:
                    fsutil.replace(src, lab.DONE / f"{name}.json")
                except FileNotFoundError:
                    pass
                self.exps.pop(name, None)
                lab.log(f"experiment {name} complete")

    def _flush(self) -> None:
        """What the tick leaves to write: the lines a results file refused, tried again; the stored results of runs
        whose lines are written (pruned); and the cost cells that changed."""
        if self._pending:
            pending, self._pending = self._pending, []
            for exp, line, i, run in pending:
                if self._settle_line(exp, line, i, run):
                    lab.log(f"{exp.name} #{i}: its result's line is written")
        if not self._prune and not self.costs._dirty:
            return
        from sqlalchemy import update
        from citar import db
        from citar.db.models import CpuJobRun
        prune, self._prune = self._prune, []
        try:
            with db.session() as s:
                if prune:
                    s.execute(update(CpuJobRun).where(CpuJobRun.id.in_(prune)).values(result=None, digests=None))
                self.costs.write(s)
        except Exception:
            self._prune = prune + self._prune       # tried again at the next tick (the cells stay changed too)
            raise

    # -------------------------------------------------------------- administrators' requests
    def retry(self, exp: Optional[str] = None) -> None:
        """An administrator's word (``POST /api/admin/jobs/retry``) that the failed games of ``exp``, or of every
        queued experiment, are to be played again, their attempts back to 0: done at the next tick, on the
        dispatcher's thread (the only writer of the jobs)."""
        self._requests.append(("retry", exp))
        self.poke()

    def _serve(self) -> None:
        """The administrators' requests since the last tick."""
        while self._requests:
            what, exp = self._requests.popleft()
            if what == "retry":
                self._retry_failed([exp] if exp else list(self.exps))

    def _retry_failed(self, names: list) -> int:
        """The failed jobs of these queued experiments queued again, their attempts back to 0 (whatever failed them:
        the administrator may have changed what did)."""
        from sqlalchemy import select
        from citar import db
        from citar.db.models import CpuJob
        names = [n for n in names if n in self.exps]
        if not names:
            return 0
        views = []
        with db.session() as s:
            for row in s.scalars(select(CpuJob).where(CpuJob.item_kind == "lab_experiment",
                                                     CpuJob.item_ref.in_(names), CpuJob.state == "failed")):
                row.state, row.attempts, row.jobs_build = "queued", 0, self.jobs_build
                views.append(self._view(row, self.exps[row.item_ref]))
        for job in views:
            self.jobs[job.id] = job
            self.exps[job.exp].failed.discard(job.i)
        if views:
            lab.log(f"{len(views)} failed game(s) of {', '.join(sorted({j.exp for j in views}))} queued again "
                    "(an administrator's retry)")
        return len(views)

    # -------------------------------------------------------------- what others read
    def _publish(self, now: float) -> None:
        hour = time.time() - 3600
        while self.finished and self.finished[0] < hour:
            self.finished.popleft()
        running = []
        per_exp: dict = defaultdict(lambda: {"queued": 0, "running": 0})
        for run in self.runs.values():
            job = self.jobs.get(run.job)
            if job is None:
                continue
            per_exp[job.exp]["running"] += 1
            started = run.started_at if run.started_at is not None else run.offered_at
            running.append({"exp": job.exp, "i": job.i, "job": job.id, "run": run.id, "kind": job.kind,
                            "machine": run.machine, "executor": run.executor.name,
                            "executor_kind": run.executor.kind, "state": run.state,
                            "minutes": max(0.0, now - started) / 60.0, "turn": run.round, "limit": run.of,
                            "paused": run.paused, "attempt": job.attempts + 1, "owner": job.owner_id,
                            "peak_mb": round(run.peak_mb, 1), "cpu_s": round(run.cpu_s, 1)})
        firsts: dict = {}
        for j in self.jobs.values():
            if j.state == "queued":
                per_exp[j.exp]["queued"] += 1
                if j.exp not in firsts or (j.created, j.i) < (firsts[j.exp].created, firsts[j.exp].i):
                    firsts[j.exp] = j
        exps = {}
        for name, exp in self.exps.items():
            info = dict(per_exp.get(name, {"queued": 0, "running": 0}))
            info.update({"settled": len(exp.settled), "failed": sorted(exp.failed), "refused": exp.refused or None,
                         "owner": exp.owner_id, "job_kind": exp.job_kind})
            if not info["running"]:
                text, parts = None, []
                if name in firsts:
                    text, parts = self._waiting(firsts[name], now)
                elif exp.in_grace(now):
                    text = "looking up its owner"
                elif exp.probe is not None:
                    # its next game, which no executor would take: why (fresh, so a change of settings shows)
                    text, parts = self._waiting(exp.probe, now)
                if text:
                    info["waiting"], info["waiting_on"] = text, parts
            exps[name] = info
        executors = []
        for e in self.executors:
            d = e.describe()
            d["available"] = self._available(e, now)
            d["benchmark"] = e.id in self._paused_exec
            executors.append(d)
        self.published = {
            "dispatcher": {"alive": True, "since": self.started_at.isoformat(timespec="seconds"), "ticks": self.ticks,
                           "tick_ms": round(self.tick_ms, 2), "executors": executors,
                           "queued": sum(1 for j in self.jobs.values() if j.state == "queued"),
                           "running": len(self.runs), "games_last_hour": len(self.finished),
                           "jobs_build": self.jobs_build},
            "running": running, "experiments": exps}

    def queued_view(self, limit: int = 500) -> list:
        """The queued jobs in the order they would run, for administrators: each with why it waits."""
        now = self.clock()
        with self._tick_lock:
            jobs = sorted((j for j in self.jobs.values() if j.state == "queued"),
                          key=lambda j: (-j.priority, j.created, j.i))[:limit]
            return [{"job": j.id, "exp": j.exp, "i": j.i, "kind": j.kind, "priority": j.priority,
                     "owner": j.owner_id, "mem_mb": j.mem_mb, "attempts": j.attempts,
                     "waiting": self.waiting(j, now)} for j in jobs]


def _utc(wall: float) -> datetime:
    """A wall time as the database keeps times (UTC)."""
    return datetime.fromtimestamp(wall, timezone.utc)


def _utc_ts(t: datetime) -> float:
    """A database time as a wall time: a naive one (SQLite gives them so) is UTC."""
    return (t.replace(tzinfo=timezone.utc) if t.tzinfo is None else t).timestamp()


def _step(what: str, fn: Callable[[], Any]) -> None:
    """One step of a result's writing after its commit, alone: its failure is logged and the next step still runs."""
    try:
        fn()
    except Exception:
        log.exception("writing %s failed", what)


def _observed(run) -> float:
    """What the server observed of a finished run's row: from its start (or offer) to its end, in seconds."""
    start = run.started_at or run.offered_at
    end = run.ended_at
    if start is None or end is None:
        return float(run.wall_s or 0.0)
    if start.tzinfo is None:
        start = start.replace(tzinfo=timezone.utc)
    if end.tzinfo is None:
        end = end.replace(tzinfo=timezone.utc)
    return max(0.0, (end - start).total_seconds())


def _ledger():
    from citar import ledger
    return ledger.get()


# ---------------------------------------------------------------------------- the process's dispatcher

_DISPATCHER: Optional[Dispatcher] = None
_LOCK = threading.Lock()


def current() -> Optional[Dispatcher]:
    return _DISPATCHER


def install(dispatcher: Optional[Dispatcher]) -> Optional[Dispatcher]:
    """Make ``dispatcher`` the process's, without starting its thread (tests tick theirs by hand); None forgets it.
    Returns the one before."""
    global _DISPATCHER
    with _LOCK:
        old, _DISPATCHER = _DISPATCHER, dispatcher
    return old


def start(dispatcher: Optional[Dispatcher] = None) -> Dispatcher:
    """Start the process's dispatcher (the server's startup), or install ``dispatcher`` (tests)."""
    global _DISPATCHER
    with _LOCK:
        old, _DISPATCHER = _DISPATCHER, dispatcher or Dispatcher()
    if old is not None and old is not _DISPATCHER:
        old.stop()
    _DISPATCHER.start()
    _DISPATCHER.poke()
    return _DISPATCHER


def stop() -> None:
    """Stop the process's dispatcher (the server's shutdown)."""
    global _DISPATCHER
    with _LOCK:
        old, _DISPATCHER = _DISPATCHER, None
    if old is not None:
        old.stop()


def poke() -> None:
    d = _DISPATCHER
    if d is not None:
        d.ensure_thread()
        d.poke()


def view() -> dict:
    """What the dispatcher last published (the Lab page's live part), or a dispatcher that is not running."""
    d = _DISPATCHER
    if d is None:
        return {"dispatcher": {"alive": False, "executors": []}, "running": [], "experiments": {}}
    d.ensure_thread()
    return d.published
