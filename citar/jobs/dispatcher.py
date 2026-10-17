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
3. **Executors** (``citar.jobs.local``'s, and the connected helpers', ``citar.jobs.remote``) refresh their settings
   and hand over their frames: accepts, progress, results, failures, aborts. A host that died fails its runs
   ``memory`` or ``host``. A helper that reconnected says which of its runs it still plays (``welcome_jobs``, asked by
   the hub for the welcome's ``jobs``): those the dispatcher holds go on, their silence counted from then; those it
   holds that the helper no longer plays fail ``host``.
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

**Helpers** (P4.5.6, P4.2.5): a remote run's result is acknowledged (``job_result_ack``) once it is committed; a
second result for a settled run is acknowledged again when its hashes are the same and recorded as a claim failure on
the machine when they differ; a late result (its run expired, failed or aborted, or its job cancelled while it played)
is acknowledged and dropped, neither counted nor credited; a late accept (its offer expired) is answered ``job_abort``.
Re-offers to a helper back off per reject reason, 2 s doubling to 10 minutes: ``busy`` and ``quiet`` until a
``status`` shows room, the reasons its settings decide until its next ``settings_ack`` or hello. Its run's event is
given by the machine's owner (``pending`` while a replica checks it, then ``verified``, or ``unverified`` when none
does), its CPU also bounded by 1.5 times the job's reference cost; a run under a grant with a budget holds the job's
expected credits from the moment it is picked (a hard budget with no room keeps that owner's jobs from the machine for
the tick) and settles its credits into that budget (a run carried across a restart too). An offer that could not be sent, or that went with its helper's
connection, is nobody's abandon and no attempt.

**A helper's word is not the game's fault** (P4.6): the server built and checked every spec and offers a job only to a
helper of its own jobs build, so a helper's ``bad_spec`` or a run it fails as not retryable queues the job again (an
attempt), counts ``trust.helper_faults`` on its machine and backs it off; a ``job_aborted`` the dispatcher never asked
for is an abandon and an attempt; a helper that stops (``aborted_locally``) gives its games back at no cost and is
offered nothing until its next hello; a run its own pauses ran out of time (no attempt, no abandon) is not offered to
that machine again for an hour unless a benchmark's pause explains it, and a ``deadline`` failure is pause-caused only
when the server saw the run last twice its deadline. So no helper can keep a game from the other machines.

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
from citar.jobs import host as H, store, trust as T

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
    #: A ``settling`` job's verification (P4.5.8): ``replica`` or ``tiebreak``, the run it waits for (None once one
    #: is offered: ``run`` names it); its primary's run and machine; the replica that disagreed (``tiebreak``); whether
    #: only the local executor may play it (a ``new`` machine's replica, a tiebreak, a re-opened job); whether it
    #: replicates a ``new`` machine's result (the local executor's share of those, rule 3); and whether its result is
    #: already in the results file (a job re-opened when its verifier was quarantined).
    need: Optional[str] = None
    primary: Optional[str] = None
    primary_machine: Optional[str] = None
    replica: Optional[str] = None
    local_only: bool = False
    of_new: bool = False
    reopened: bool = False


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
    conn: Any = None                        # a helper's connection its offer went over (an offer is lost with it)


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
                 local: bool = True, remotes=None):
        self.clock = clock
        if executors is None:
            from citar.jobs import remote
            from citar.jobs.local import LocalExecutor
            executors = [LocalExecutor()] if local else []
            if remotes is None:
                remotes = remote.registry()
        self._own = list(executors)
        #: The helpers' executors (``citar.jobs.remote.Remotes``), which come and go with their connections; None for a
        #: dispatcher of fixed executors (tests).
        self.remotes = remotes
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
        self._rejects: dict = {}        # executor -> (offered nothing until, the wait, why); a helper's: per reason
        self._held_remote: dict = {}    # job -> a helper's run in flight when the server started (``recover``)
        self._asked: set = set()        # the helpers' runs the dispatcher asked to stop (``job_abort``), in flight
        self._shun: dict = {}           # (job, machine) -> until when the job is not offered to that machine again
        self._forced = 0
        self._requests: deque = deque()     # administrators' retries and trust changes, served at the next tick
        self._share: dict = {}              # executor -> (new machines' replicas, runs) while both waited (rule 3)
        self._hands_at, self._hands_cache, self._settling_by = -1, None, {}
        self._load_verdicts: list = []      # what ``_load`` settled (an older jobs build's results), sent after its commit
        self.jobs_build = engine_api.jobs_build()
        self.started_at = utcnow()
        for e in self._own:
            bind = getattr(e, "bind", None)
            if bind is not None:
                bind(self.poke)
        if self.remotes is not None:
            self.remotes.bind(self.poke, self.clock)

    @property
    def executors(self) -> list:
        """Every executor: the fixed ones (the local executor) and the helpers'."""
        if self.remotes is None:
            return self._own
        return self._own + self.remotes.all()

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
            for run in s.scalars(select(CpuJobRun).where(CpuJobRun.state.in_(("offered", "running")))):
                if run.executor != "local" and self.remotes is not None:
                    # a helper's run goes on without the server: kept until its helper reconnects (its hello says
                    # whether it still plays it) or its deadline passes
                    self._held_remote[run.job_id] = self._held_run(run)
                    continue
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
        if self._held_remote:
            # a helper's run whose experiment left the queue while the server was down
            with db.session() as s:
                for job_id, run in self._held_remote.items():
                    row = s.get(CpuJobRun, run.id)
                    if row is not None:
                        row.state, row.ended_at = "aborted", utcnow()
                        row.error = {"code": "restart", "message": "its experiment left the queue"}
                    job = s.get(CpuJob, job_id)
                    if job is not None and job.state == "running":
                        job.state = "cancelled"
            self._held_remote = {}
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

    def _held_run(self, row) -> "Run":
        """A helper's run in flight when the server started, as the view holds it: its times on the dispatcher's clock
        (from the wall clock's), its silence counted from now (P4.2.5: the clock restarts at the server's start)."""
        now, wall = self.clock(), time.time()

        def mono(t):
            return None if t is None else now - max(0.0, wall - _utc_ts(t))
        offered = mono(row.offered_at) or now
        deadline = DEADLINE_MIN
        if row.deadline_at is not None and row.offered_at is not None:
            deadline = max(1.0, _utc_ts(row.deadline_at) - _utc_ts(row.offered_at))
        e = self.remotes.get(row.machine_id)
        started = mono(row.started_at)
        run = Run(id=row.id, job=row.job_id, executor=e, machine=row.machine_id, role=row.role or "primary",
                  state="running" if row.state == "running" else "offered", offered_at=offered, deadline_s=deadline,
                  offered_wall=_utc_ts(row.offered_at) if row.offered_at else wall, started_at=started,
                  started_wall=_utc_ts(row.started_at) if row.started_at else None,
                  last_progress=now if started is not None else None, cpu_s=float(row.cpu_s or 0.0),
                  wall_s=float(row.wall_s or 0.0), paused_s=float(row.paused_s or 0.0),
                  peak_mb=float(row.peak_mb or 0.0))
        e.in_flight.add(run.id)
        e._remember(run.id)
        return run

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
            for run, job in list(s.execute(q)):
                reference = None if run.executor == "local" else self.costs.of(job.kind, job.spec or {})["cpu_s"]
                if run.executor != "local" and run.role == "primary" and run.verdict == "match":
                    # settled by a replica: its reference is the replica's own (P4.5.7), as its settling wrote it
                    checks = list(s.scalars(select(CpuJobRun).where(
                        CpuJobRun.job_id == job.id, CpuJobRun.role.in_(("replica", "tiebreak")),
                        CpuJobRun.verdict.in_(("reference", "match")))))
                    reference = self._reference_of(checks) or reference
                verification = _event_verification(run, job)
                row = self._event_row(run.id, job.ref, job.owner_id, run.machine_id, run.executor, run.role,
                                      run.cpu_s, _observed(run), reference, run.ended_at, verification)
                if verification == "rejected":
                    row["credits"], row["cpu_core_s"] = 0.0, 0.0
                rows.append(row)
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
        verdicts: list = []
        with db.session() as s:
            for name, spec in specs.items():
                exp = self.exps.get(name)
                if exp is None:
                    self._load(s, spec)
                elif spec != exp.spec:
                    self._changed(s, exp, spec)
            for name in [n for n in self.exps if n not in specs]:
                if not (lab.QUEUE / f"{name}.json").exists():
                    verdicts += self._deleted(s, self.exps.pop(name))
        verdicts += self._load_verdicts
        self._load_verdicts = []
        if verdicts:
            _step("a deleted experiment's verdicts", lambda: _ledger().verdicts(verdicts))

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
            elif job.state == "running" and job.id in self._held_remote and job.id not in self.jobs:
                run = self._held_remote.pop(job.id)
                view = self._view(job, exp)
                view.run = run.id
                self.jobs[job.id] = view
                self.runs[run.id] = run
            elif job.state == "settling" and job.id not in self.jobs and job.jobs_build != self.jobs_build                     and self._settle_old_build(s, job, exp):
                exp.settled.add(i)
            elif job.state == "settling" and job.id not in self.jobs:
                view = self._settling_view(s, job, exp)
                if view is not None:
                    held = self._held_remote.pop(job.id, None)
                    if held is not None:
                        view.run = held.id
                        self.runs[held.id] = held
                    self.jobs[job.id] = view
        self._reconcile(s, exp)
        for j in [j for j in self.jobs.values() if j.exp == exp.name and j.state == "queued" and j.i in exp.settled]:
            # a game with a line from before jobs (or a database started afresh) is played no more
            row = s.get(CpuJob, j.id)
            if row is not None:
                row.state = "cancelled"
            self.jobs.pop(j.id, None)
        exp.advance()

    def _settle_old_build(self, s, job, exp: Exp) -> bool:
        """A result that waited for its replica when the server last stopped, from a jobs build this server no longer
        plays (it was upgraded meanwhile): no verifier would ever reproduce it (this server's job host plays its own
        build, and every helper's follows), so it is accepted as an unsampled result is, ``unverified`` (P4.5.8 rule
        4), its line written by the reconciliation that follows. It counts no match; were its machine caught later, it
        is withdrawn with the rest of its unverified results (rule 8). Returns whether it was settled (False: no
        primary result to accept, and the job is played again as ``_settling_view`` says)."""
        from sqlalchemy import select
        from citar.db.models import CpuJobRun
        runs = list(s.scalars(select(CpuJobRun).where(CpuJobRun.job_id == job.id).order_by(CpuJobRun.offered_at)))
        primary = next((r for r in runs if r.id == job.accepted_run), None) if job.accepted_run else None
        if primary is None:
            done = [r for r in runs if r.role == "primary" and r.state == "done" and r.verdict == "unchecked"]
            primary = done[-1] if done else None
        if primary is None:
            return False
        mark = primary.error if isinstance(primary.error, dict) else {}
        crashed = "crash" in (primary.result or {}) or bool(mark.get("crashed"))
        job.state = "crashed" if crashed else "accepted"
        job.accepted_run, job.result_hash = primary.id, primary.result_hash
        primary.verdict, primary.error = "unchecked", None
        for r in runs:
            if r.role != "primary" and r.state in ("offered", "running"):
                held = self._held_remote.pop(job.id, None)
                if held is not None and held.executor is not None:
                    held.executor.in_flight.discard(held.id)
                r.state, r.ended_at = "aborted", utcnow()
                r.error = {"code": "old_build", "message": "its job's jobs build is no longer played"}
        self._load_verdicts.append({"activity": job.ref, "request_id": primary.id, "verification": "unverified"})
        lab.log(f"{exp.name} #{store.game_of(job.ref)}: its result waited for a replica of jobs build "
                f"{job.jobs_build}, which this server no longer plays: accepted unverified")
        return True

    def _settling_view(self, s, job, exp: Exp) -> Optional[Job]:
        """A ``settling`` job as the view holds it after a start (P4.5.8): its primary (its accepted run when it was
        re-opened, else its last primary that is done and unchecked), what it waits for (a tiebreak when a replica
        disagreed with it and none settled it, else a replica), and whether this server alone may play that (a new
        machine's replica, a tiebreak, a re-opened job: what its primary's ``replicating`` mark says)."""
        from sqlalchemy import select
        from citar.db.models import CpuJobRun
        runs = list(s.scalars(select(CpuJobRun).where(CpuJobRun.job_id == job.id).order_by(CpuJobRun.offered_at)))
        primary = next((r for r in runs if r.id == job.accepted_run), None) if job.accepted_run else None
        if primary is None:
            done = [r for r in runs if r.role == "primary" and r.state == "done" and r.verdict == "unchecked"]
            primary = done[-1] if done else None
        if primary is None:
            # nothing to check: played again
            job.state = "queued"
            return self._view(job, exp)
        mark = primary.error if isinstance(primary.error, dict) else {}
        replica = next((r for r in reversed(runs) if r.role == "replica" and r.state == "done"
                        and r.verdict == "mismatch"), None)
        view = self._view(job, exp)
        view.state, view.need = "settling", "tiebreak" if replica is not None else "replica"
        view.primary, view.primary_machine = primary.id, primary.machine_id
        view.replica = replica.id if replica is not None else None
        view.local_only = bool(mark.get("local_only")) or replica is not None
        view.of_new = mark.get("why") == "new"
        view.reopened = job.accepted_run == primary.id
        return view

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

    def _deleted(self, s, exp: Exp) -> list:
        """An experiment deleted from the queue: its waiting and failed jobs cancelled (so one queued again under its
        name plays its failed games again), its running ones aborted. A job waiting for its replica is cancelled too:
        its primary's event, ``pending``, and a remote replica's waiting for a tiebreak are given ``unverified`` (the
        ledger's verdicts it returns, queued once the session commits): nothing will check them now."""
        from sqlalchemy import update
        from citar.db.models import CpuJob
        verdicts = []
        for j in [j for j in self.jobs.values() if j.exp == exp.name]:
            row = s.get(CpuJob, j.id)
            if row is not None:
                row.state = "cancelled"
            if j.run and j.run in self.runs:
                run = self.runs[j.run]
                self._abort(run, "its experiment was deleted")
            if j.state == "settling":
                verdicts += [{"activity": j.ref, "request_id": rid, "verification": "unverified",
                              "why": "its experiment was deleted before its result was checked"}
                             for rid in (j.primary, j.replica if j.need == "tiebreak" else None) if rid]
            self.jobs.pop(j.id, None)
        s.execute(update(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == exp.name,
                                       CpuJob.state == "failed").values(state="cancelled"))
        lab.log(f"experiment {exp.name} left the queue: its waiting games were cancelled")
        return verdicts

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
        # an accepted job's run, and a re-opened one's (settling again, its line in the file: P4.5.8 rule 9)
        accepted = {j.accepted_run: j for j in jobs if j.state in store.SETTLED + ("settling",) and j.accepted_run}
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
                if job is not None and job.state != "settling":
                    self._prune.append(run_id)
                continue
            if job.state == "settling":
                continue                # a re-opened job's line is lost: it is written when its replica settles it
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
                              run.started_at or run.offered_at, run.ended_at, _line_verification(run))
            exp.settled.discard(store.game_of(job.ref))      # until its line is written
            self._settle_line(exp, line, store.game_of(job.ref) or 0, run.id)
            lab.log(f"{exp.name} #{store.game_of(job.ref)}: its accepted result's line written at the start")

    @staticmethod
    def _line(kind: str, exp: str, i: int, spec: dict, result: dict, machine: str, run: str, executor: str,
              cpu_s: float, wall_s: float, started, ended, verification: str) -> dict:
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
        if t == "_reconnect":
            self._reconnected(s, e, f, now)
            return
        run = self.runs.get(run_id)
        if run is None or run.executor is not e:
            if e.kind != "local" and t == "job_result":
                self._late_result(s, e, f, after)
            elif e.kind != "local" and t == "job_accept" and getattr(e, "knows", lambda r: False)(run_id):
                # its offer expired: the helper is told to let the run go (P4.2.5)
                e.abort(run_id, "its offer expired before it was accepted")
            elif t == "job_result":
                log.info("a late result for run %s on %s, dropped", run_id, e.id)
            else:
                log.debug("a %s for run %s, which %s does not play now: dropped", t, run_id, e.id)
            return
        # when the frame arrived, in the dispatcher's clock and as a wall time: it may be read a tick later (a helper's
        # frame carries its arrival on the dispatcher's own clock)
        at = f["_at"] if isinstance(f.get("_at"), (int, float)) else H.arrived(f, now)
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
            self._rejected(s, e, run, f, wall, at)
        elif t == "job_aborted":
            if e.kind == "local" or run.id in self._asked:
                self._end(s, run, "aborted", {"code": "aborted", "message": "aborted"}, wall)
                self._requeue(s, run, attempt=False)
            else:
                # a helper stopped a run nobody asked it to stop: an abandon, and an attempt (a correct helper answers
                # only the server's ``job_abort`` so, and lets a run go otherwise with ``job_failed``)
                self._end(s, run, "aborted", {"code": "abandoned",
                                              "message": "its helper stopped it though the server did not ask"}, wall)
                self._requeue(s, run, attempt=True)
                self._abandoned(s, run.machine, at)

    def _reconnected(self, s, e, f: dict, now: float) -> None:
        """A helper reconnected: the runs its hello still plays and the dispatcher holds go on, their silence counted
        from now (a run whose accept was lost with the connection is running); those it holds that the helper no longer
        plays (nor holds a result of) fail ``host``, retryable (the helper restarted, or its job host died while it
        was away)."""
        at = f["_at"] if isinstance(f.get("_at"), (int, float)) else H.arrived(f, now)
        wall = time.time() - (now - at)
        for rid in f.get("keep") or []:
            run = self.runs.get(rid)
            if run is None or run.executor is not e:
                continue
            if run.state == "offered":
                run.state, run.started_at, run.started_wall = "running", at, wall
                row = self._run_row(s, run)
                row.state, row.started_at = "running", _utc(wall)
            run.last_progress = at
        for rid in f.get("lost") or []:
            run = self.runs.get(rid)
            if run is None or run.executor is not e:
                continue
            if run.state == "offered":
                # its offer went with the connection, unanswered: the game never started, so it is offered again as
                # it was, neither an attempt nor an abandon
                self._end(s, run, "failed", {"code": "lost", "message": "its offer was lost with its helper's "
                                                                        "connection"}, wall)
                self._requeue(s, run, attempt=False)
                continue
            self._failed(s, e, run, {"code": "host", "retryable": True,
                                     "message": "its helper reconnected without it (it restarted, or its job host "
                                                "ended while it was away)"}, at, wall)

    def _late_result(self, s, e, f: dict, after: list) -> None:
        """A helper's result for a run the dispatcher does not hold in flight (P4.2.5): for a run already settled,
        acknowledged again when its hashes are the same, and a claim failure when they differ; for one that expired,
        failed or was aborted, acknowledged and dropped (not counted, not credited); for a run never offered to this
        machine, dropped (P4.7.3)."""
        from citar.db.models import CpuJobRun
        rid = f.get("run")
        row = s.get(CpuJobRun, rid) if isinstance(rid, str) else None
        if row is None or row.machine_id != str(e.id)[:40]:
            log.warning("the helper of %s sent a result for run %s, which was not offered to it: dropped", e.id, rid)
            return
        outcome = f.get("outcome") or {}
        if row.state == "done":
            same = (outcome.get("result_hash") == row.result_hash
                    and (outcome.get("chain") or {}).get("head") == row.chain_head)
            if same:
                after.append(lambda: e.ack(rid))
                log.info("the helper of %s sent run %s's result again: acknowledged again", e.id, rid)
            else:
                why = "a second result for a settled run, with other hashes"
                self._claim_failure(s, e, rid, why)
                if (row.verdict or "") != "rejected":
                    self._quarantine(s, str(e.id), T.evidence("claim", run=rid, job=row.job_id, why=why),
                                     owner_too=True, withdraw=True, after=after)
            return
        after.append(lambda: e.ack(rid))
        lab.log(f"run {rid} on {e.name}: its result came after it {row.state}; acknowledged and dropped")

    def _claim_failure(self, s, e, rid: str, why: str) -> None:
        """A helper's claim that does not hold (P4.5.8's ``claim`` verdict, which quarantines it): counted on its
        machine's row (``trust.claim_failures``, the last one's run and reason), the first one audited."""
        from citar.auth import audit
        from citar.db.models import Server
        row = s.get(Server, e.id)
        log.warning("claim failure on %s, run %s: %s", e.id, rid, why)
        if row is None:
            return
        trust = dict(row.trust or {})
        trust["claim_failures"] = int(trust.get("claim_failures") or 0) + 1
        trust["last_claim_failure"] = {"run": rid, "why": why, "at": utcnow().isoformat()}
        row.trust = trust
        if trust["claim_failures"] == 1:
            audit.record(s, "server.claim_failure", object_type="server", object_id=e.id, run=rid, why=why)

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
        self._asked.discard(run.id)
        release = getattr(run.executor, "release", None)
        if release is not None:
            release(run.id)

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
        if job.state == "settling":
            # a verification run that ended without a result: offered again (rule 4: an infrastructure failure says
            # nothing about the primary), never an attempt
            return
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
        # a helper's word that its pause ran a run out of time holds only when the server saw it last that long: its job
        # host ends a run held by a pause at twice its deadline in all (P4.3.6)
        caused = (code == "deadline" and run.started_at is not None
                  and pause_caused(run.started_at, run.deadline_s, paused, at)
                  and (local or at - run.started_at >= 2.0 * run.deadline_s - EXPIRY_GRACE_SECONDS))
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
            self._shun_unless_benched(run, at)
            if job is not None:
                lab.log(f"{job.exp} #{job.i} on {e.name}: held by a pause past its deadline ({message}); queued "
                        "again, not counted as an attempt")
            return
        if code == "aborted_locally":
            # its helper stopped (its owner stopped lending the CPU): the game did nothing wrong, so it is played
            # again elsewhere at once, neither an attempt nor an abandon; the machine is offered nothing until its
            # helper's next hello (an honest one closes its connection as it stops)
            self._requeue(s, run, attempt=False)
            if not local:
                self._back_off(e, "stopped", at, "its helper stopped", f.get("_seq"))
            if job is not None:
                lab.log(f"{job.exp} #{job.i} on {e.name}: its helper stopped ({message}); queued again")
            return
        retryable = bool(f.get("retryable", True))
        if retryable or not local:
            self._requeue(s, run, attempt=True)
            if not retryable:
                # the server built and checked the spec itself, and the helper has its jobs build: a run no machine
                # could play is its fault (broken or lying), not the game's, which is played again elsewhere
                self._helper_fault(s, e, run.id, f"it failed a run as not retryable ({code}): {message[:200]}")
                self._back_off(e, "fault", at, "its helper failed a run it says no machine could play",
                               f.get("_seq"))
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

    def _shun_unless_benched(self, run: Run, at: float) -> None:
        """A remote run that a pause ran out of time (no attempt, no abandon, P4.5.5): unless the server's own pause (a
        benchmark's) explains it, its job is not offered to that machine again for ``ABANDON_WINDOW``, so a helper whose
        own pauses hold a game (its model's work, or a helper that says so falsely) cannot keep it from other
        machines."""
        if run.executor.kind == "local" or run.started_at is None:
            return
        bench = run.bench_s + (max(0.0, at - run.bench_since) if run.bench_since is not None else 0.0)
        if pause_caused(run.started_at, run.deadline_s, bench, at):
            return
        self._shun[(run.job, run.machine)] = at + ABANDON_WINDOW
        log.info("job %s is not offered to %s again for an hour: its own pauses held it past its deadline", run.job,
                 run.machine)

    def _back_off(self, e, reason: str, at: float, why: str, seq: Optional[int] = None) -> None:
        """A helper is offered nothing for a while for ``reason`` (P4.2.5's re-offer backoff): 2 s, doubling to 10
        minutes while the reason recurs, until what clears it comes (``_backed_off``): what came after the frame that
        caused it (its place ``seq``, ``remote.SEQ``; its arrival ``at`` for a frame with none)."""
        per = self._rejects.setdefault(e.id, {})
        _until, wait, *_rest = per.get(reason, (0.0, REJECT_FIRST / 2, "", 0.0, None))
        wait = min(REJECT_MAX, wait * 2)
        per[reason] = (at + wait, wait, why, at, seq if isinstance(seq, int) else None)

    def _helper_fault(self, s, e, rid: str, why: str) -> None:
        """A helper's word against the server's own checks (a spec it refused that the server checked with the same jobs
        build, a run it says no machine could play): counted on its machine's row (``trust.helper_faults``, the last
        one's run and reason), the first one audited, as a claim failure is."""
        from citar.auth import audit
        from citar.db.models import Server
        row = s.get(Server, e.id)
        log.warning("helper fault on %s, run %s: %s", e.id, rid, why)
        if row is None:
            return
        trust = dict(row.trust or {})
        trust["helper_faults"] = int(trust.get("helper_faults") or 0) + 1
        trust["last_helper_fault"] = {"run": rid, "why": why, "at": utcnow().isoformat()}
        row.trust = trust
        if trust["helper_faults"] == 1:
            audit.record(s, "server.helper_fault", object_type="server", object_id=e.id, run=rid, why=why)

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

    def _rejected(self, s, e, run: Run, f: dict, wall: Optional[float] = None, at: Optional[float] = None) -> None:
        reason = str(f.get("reason") or "")
        self._end(s, run, "failed", {"code": "rejected", "reason": reason, "message": str(f.get("message"))[:1000]},
                  wall)
        job = self.jobs.get(run.job)
        if e.kind != "local":
            # a helper: offered nothing for a while, per reason, 2 s doubling to 10 minutes (P4.2.5), until what
            # clears that reason comes (``_available``)
            self._back_off(e, reason, self.clock() if at is None else at, f"its helper refused a job ({reason})",
                           f.get("_seq"))
            if reason == "bad_spec":
                # the server built and checked the spec itself, and offers a job only to a helper of its jobs build:
                # a helper's ``bad_spec`` is the helper's fault (broken or lying), not the game's, which is played
                # again elsewhere (an attempt, so a job cannot go round for ever)
                self._helper_fault(s, e, run.id, "it refused a spec the server checked (bad_spec)")
                self._requeue(s, run, attempt=True)
                return
        elif reason not in ("busy", "memory", "bad_spec"):
            # the executor refuses every job for now (unverifiable, another build): offered nothing for a while,
            # 2 s doubling to 10 minutes (P4.2.5), until it sends a result
            _until, wait, _why = self._rejects.get(e.id, (0.0, REJECT_FIRST / 2, ""))
            wait = min(REJECT_MAX, wait * 2)
            self._rejects[e.id] = (self.clock() + wait, wait, f"it refused a job ({reason}): {f.get('message')}")
        if reason == "bad_spec":
            # the server's own job host refused a spec the server made: the game cannot be played
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
        """A result (P4.5.7, P4.5.8): checked, committed, then what follows it. A helper's is checked by its signed
        claim (``engine_api.verify_claim``: the signature against its machine's registered key, the claim against the
        server's records, the result's hash, the chain's head and fold, the result against its spec), and one that does
        not hold is a tampering verdict (``_refused_claim``); the local executor's by its hash and its fit, a failure
        an internal fault played again. A primary's result is then accepted at once (``reference`` from the local
        executor, ``unverified`` from a helper the rules leave unchecked: its job ``accepted``, its line appended once,
        its event, cost and usage rows), or held while a replica checks it (rule 1: its job ``settling``, its event
        ``pending``, no line yet); a verification run's result settles its job (``_verified``). ``wall`` is when it
        arrived: the end of what the server observed of the run, from its accept's arrival (its offer's, when no
        accept was seen)."""
        from citar.db.models import CpuJob
        job = self.jobs.get(run.job)
        row = s.get(CpuJob, run.job)
        outcome = f.get("outcome") or {}
        result, chain = outcome.get("result"), outcome.get("chain") or {}
        for k in ("cpu_s", "wall_s", "paused_s", "peak_mb"):
            if isinstance(outcome.get(k), (int, float)):
                v = max(0.0, float(outcome[k]))
                setattr(run, k, max(run.peak_mb, min(v, float(MEM_MAX))) if k == "peak_mb" else v)
        local = e.kind == "local"
        if job is None or row is None or row.state in ("cancelled", "withdrawn"):
            # its job was cancelled while it played (its experiment deleted, its abort sent), or withdrawn: a result
            # that crossed the abort is acknowledged and dropped (PROTOCOL.md), neither lined nor credited
            self._end(s, run, "aborted", {"code": "cancelled", "message": "its job was cancelled while it played"},
                      wall)
            ack = None if local else getattr(e, "ack", None)
            if ack is not None:
                after.append(lambda: ack(run.id))
            lab.log(f"run {run.id} on {e.name}: its result came after its job was cancelled; dropped")
            return
        try:
            if not isinstance(result, dict):
                raise engine_api.JobRefused("the result is no object")
            if local:
                got = engine_api.job_result_hash(job.kind, result)
                if got != outcome.get("result_hash"):
                    raise engine_api.JobRefused(f"its result hashes to {got}, not {outcome.get('result_hash')}")
                engine_api.job_result_fits(job.kind, job.spec, result, chain, job.spec_hash)
            else:
                unchecked = self._check_claim(s, e, run, job, f)
                if unchecked is not None:
                    self._key_moved(s, e, run, job, unchecked, wall)
                    return
        except engine_api.ClaimRefused as err:
            self._refused_claim(s, e, run, job, f, err, wall, after)
            return
        except (engine_api.JobRefused, ValueError) as err:
            # the local executor's result that does not check, or (a helper's claim having held) the server's own
            # record of the job that does not: an internal fault, played again, nobody's verdict
            log.error("run %s of %s on %s gave a result that does not check: %s", run.id, job.ref, e.id, err)
            self._failed(s, e, run, {"code": "internal", "message": f"its result does not check: {err}",
                                     "retryable": True}, at, wall)
            ack = None if local else getattr(e, "ack", None)
            if ack is not None:
                after.append(lambda: ack(run.id))
            return
        # the hash kept (and compared by verification) is one checked against the result itself: this server's by
        # hashing it, a helper's by its claim's check, which holds the claim's, the outcome's and the result's as one
        result_hash = outcome["result_hash"]
        rrow = self._run_row(s, run)
        rrow.state, rrow.ended_at = "done", _utc(wall)
        rrow.cpu_s, rrow.wall_s, rrow.paused_s, rrow.peak_mb = run.cpu_s, run.wall_s, run.paused_s, run.peak_mb
        rrow.result, rrow.result_hash = result, result_hash
        rrow.chain_head, rrow.rounds = chain.get("head"), chain.get("rounds")
        if not local:
            rrow.claim, rrow.signature = f.get("claim"), f.get("signature")
        if not local or run.role != "primary":
            # a helper's digests (the evidence of a mismatch) and a verification run's (where two runs parted)
            rrow.digests = chain.get("digests")
        self.runs.pop(run.id, None)
        self._rejects.pop(e.id, None)
        if run.role != "primary":
            self._verified(s, e, run, job, row, rrow, wall, after)
            return
        crash = "crash" in result
        why = None if local else self._replication(s, e, job)
        if why is not None:
            # rule 1: checked by a replica before it is accepted, its replica made only now; its job is ``done`` to
            # everyone but administrators from here (rule 2)
            rrow.verdict = "unchecked"
            rrow.error = {"code": "replicating", "why": why, "local_only": why == "new"}
            row.state = "settling"
            job.state, job.run, job.need = "settling", None, "replica"
            job.primary, job.primary_machine, job.replica = run.id, run.machine, None
            job.local_only = job.of_new = why == "new"
            job.reopened = False
        else:
            rrow.verdict = "reference" if local else "unchecked"
            row.state = "crashed" if crash else "accepted"
            row.accepted_run, row.result_hash = run.id, result_hash
            self.jobs.pop(job.id, None)
        started_wall = run.started_wall or run.offered_wall
        observed = max(0.0, wall - started_wall)
        exp = self.exps.get(job.exp)
        ended = rrow.ended_at
        line = None
        if why is None:
            line = self._line(job.kind, job.exp, job.i, job.spec, result, run.machine, run.id, e.kind, run.cpu_s,
                              run.wall_s, started_wall, ended, "reference" if local else "unverified")
        verification = "verified" if local else ("pending" if why else "unverified")
        reference = None if local else self.costs.of(job.kind, job.spec)["cpu_s"]
        event = self._event_row(run.id, job.ref, job.owner_id, run.machine, e.kind, run.role, run.cpu_s, observed,
                                reference, ended, verification)
        if not local:
            self._note_cpu(s, run.machine, run.id, event["flags"], run.cpu_s, observed, reference)
        take = getattr(e, "hold", None)
        hold = take(run.id) if take is not None else None
        usage_line = line or self._line(job.kind, job.exp, job.i, job.spec, result, run.machine, run.id, e.kind,
                                        run.cpu_s, run.wall_s, started_wall, ended, "pending")

        def crash_log():
            text = (line or {}).get("crash") or ""
            with open(lab.LAB / "crashes.log", "a", encoding="utf-8") as fh:
                fh.write(f"--- {datetime.now():%Y-%m-%d %H:%M} {job.exp} #{job.i} final\n{text}\n")
            lab.log(f"{job.exp} #{job.i} crashed (final: not played again): {str(text).splitlines()[0]}")

        def played_log():
            what = "crashed" if crash else f"T{result.get('turns')} {result.get('victory')}"
            if why is not None:
                lab.log(f"{job.exp} #{job.i}: {what} on {e.name}; checked by a replica before it is accepted "
                        f"({why})")
            elif job.kind == "reference_game":
                lab.log(f"{job.exp} #{job.i}: {what} ({run.wall_s:.0f}s on {e.name})")
            else:
                lab.log(f"{job.exp} #{job.i}: {what} won by {result.get('winner_label')} "
                        f"({run.wall_s:.0f}s on {e.name})")

        def settle():
            # the run's credits into its grant's budget, as a model step's are (P4.5.7): in memory now, its entry
            # written with the event, which keeps where it went (``flags.budget``) so a rejection can take it back; a
            # run carried across the server's restart lost its hold with the old process, and takes one now, whatever
            # the budget has left (the work is done)
            h = hold
            if h is None and not local:
                late = getattr(e, "late_hold", None)
                h = late(job) if late is not None else None
            if h is None:
                return []
            from citar.pool import budgets
            budgets.keeper().settle(h, event["credits"])
            event["flags"] = {**event["flags"], "budget": {"id": h.budget_id, "key": h.key}}
            return [(h, event["credits"])]

        def written():
            ack = None if local else getattr(e, "ack", None)
            if ack is not None:
                ack(run.id)         # committed: the helper may forget it (P4.2.5)
            hook = self.hooks.get("after_commit")
            if hook is not None:
                hook(job.id)        # a test's crash here leaves everything below undone, as a dead server would
            if why is not None:
                hook = self.hooks.get("replicated")
                if hook is not None:
                    hook(job.id, run.id)    # decided once its primary's result is committed (rule 1)
            # each step alone: a line the file refuses waits for the next tick, and the rest is written all the same
            if line is not None:
                if exp is not None:
                    self._settle_line(exp, line, job.i, run.id)
                else:
                    self._prune.append(run.id)
            settled = []
            try:
                settled = settle()
            except Exception:
                log.exception("settling %s's credits into its budget failed", job.ref)
            _step(f"{job.ref}'s CPU event", lambda: _ledger().events([event], settled=settled))
            # a counted peak enters the cost table from a reference machine only, as its CPU does: a helper's figures
            # are its own word
            _step(f"{job.ref}'s cost", lambda: self.costs.record(
                job.kind, job.spec, cpu_s=run.cpu_s if local else None,
                peak_mb=(run.peak_mb or None) if local else None))
            self.finished.append(time.time())
            _step(f"{job.ref}'s usage rows", lambda: self._usage(job, usage_line, run.machine))
            _step(f"{job.ref}'s log", crash_log if crash and line is not None else played_log)
        # after the commit (the session's end): the order of writing
        after.append(written)

    # -------------------------------------------------------------- verification (P4.5.8)
    def _secret(self) -> str:
        from citar import settings as site
        return site.get().secret_key

    def _check_claim(self, s, e, run: Run, job: Job, f: dict) -> Optional[str]:
        """A helper's signed claim checked against the key its machine registered and the server's records (P4.4.4):
        ``engine_api.ClaimRefused`` with its verdict when it does not hold, None when it holds. A check that cannot
        be made returns why: the machine has no key now (its owner reset it, P4.3.7), or its signature fails against a
        key registered after the run was offered (reset, and registered again by another helper). Neither says the
        helper lied: the run was offered under a key that is gone."""
        from citar.db.models import Server
        srow = s.get(Server, e.id)
        key = (srow.public_key if srow is not None else None) or ""
        if not key:
            return "its machine's key was reset before its result was checked"
        outcome = f.get("outcome") or {}
        try:
            engine_api.verify_claim(job.kind, job.spec, job.spec_hash, job=job.id, run=run.id, machine=str(e.id),
                                    jobs_build=job.jobs_build, public_key=key, claim=f.get("claim"),
                                    signature=f.get("signature"), result=outcome.get("result"),
                                    result_hash=outcome.get("result_hash"), chain=outcome.get("chain") or {})
        except engine_api.ClaimRefused as err:
            if (getattr(err, "verdict", None) == "signature" and srow.key_at is not None
                    and _utc_ts(srow.key_at) > run.offered_wall):
                return "its machine registered another key after the run was offered"
            raise
        return None

    def _key_moved(self, s, e, run: Run, job: Job, why: str, wall: float) -> None:
        """A helper's result whose claim could not be checked because its machine's key was reset or replaced since the
        run was offered (``_check_claim``): no verdict on anyone. The run ends ``lost`` (neither an attempt, an abandon
        nor a fault), is not acknowledged, and its game is offered again (a verification run's too). The helper's
        next hello, under the key registered then, finds the run gone from what the server holds."""
        log.warning("run %s on %s: its claim was not checked: %s", run.id, e.id, why)
        self._end(s, run, "failed", {"code": "lost", "message": why}, wall)
        self._requeue(s, run, attempt=False)
        lab.log(f"{job.exp} #{job.i} on {e.name}: its result was not checked ({why}); its game is offered again")

    def _replication(self, s, e, job: Job) -> Optional[str]:
        """Why a helper's primary result is checked by a replica (rule 1: decided only now, its result committed with
        this): ``new`` (every result of a new machine), ``asked`` (an administrator's ``replicate``, counted down),
        ``sampled`` (the server's keyed sample at the machine's tier's rate when the result came), or None."""
        from citar.db.models import Server
        srow = s.get(Server, e.id)
        if srow is None:
            return "new"
        tr = dict(srow.trust or {})
        asked = int(tr.get("replicate") or 0)
        why = T.replicate(srow.trust_tier or "new", job.id, T.settings(), self._secret(), asked)
        if why == "asked":
            tr["replicate"] = asked - 1
            srow.trust = tr
        return why

    def _refused_claim(self, s, e, run: Run, job: Job, f: dict, err, wall: float, after: list) -> None:
        """A helper's result whose claim does not hold (P4.4.4): a tampering verdict. The run is rejected (its claim and
        result kept as the evidence), acknowledged (decided on: it is not wanted again), its event written rejected
        with no credits; its game is played elsewhere, neither an attempt nor its fault; its machine and its owner are
        quarantined, and everything unverified it gave withdrawn (rule 7, 8)."""
        verdict = getattr(err, "verdict", None) or "claim"
        rrow = self._run_row(s, run)
        ev = T.evidence(verdict, run=run.id, job=job.id, why=str(err))
        rrow.state, rrow.ended_at = "done", _utc(wall)
        rrow.cpu_s, rrow.wall_s, rrow.paused_s, rrow.peak_mb = run.cpu_s, run.wall_s, run.paused_s, run.peak_mb
        outcome = f.get("outcome") or {}
        rrow.result = outcome.get("result") if isinstance(outcome.get("result"), dict) else None
        rrow.result_hash = str(outcome.get("result_hash") or "")[:64] or None
        rrow.claim = f.get("claim") if isinstance(f.get("claim"), dict) else None
        rrow.signature = str(f.get("signature") or "")[:128] or None
        rrow.digests = (outcome.get("chain") or {}).get("digests") if isinstance(outcome.get("chain"), dict) else None
        rrow.verdict, rrow.error = "rejected", ev
        self.runs.pop(run.id, None)
        self._asked.discard(run.id)
        release = getattr(e, "release", None)
        if release is not None:
            release(run.id)             # its thread, and its budget hold (nothing spent)
        if run.role == "primary":
            self._requeue(s, run, attempt=False)
        else:
            job.run, job.local_only = None, True        # another verifier: this server
        started_wall = run.started_wall or run.offered_wall
        event = self._event_row(run.id, job.ref, job.owner_id if run.role == "primary" else None, run.machine,
                                e.kind, run.role, run.cpu_s, max(0.0, wall - started_wall),
                                self.costs.of(job.kind, job.spec)["cpu_s"], rrow.ended_at, "rejected")
        event["credits"], event["cpu_core_s"] = 0.0, 0.0
        event["flags"] = {**event["flags"], "rejected": verdict}
        from citar.db.models import Server
        srow = s.get(Server, e.id)
        if srow is not None:
            tr = dict(srow.trust or {})
            tr["claim_failures"] = int(tr.get("claim_failures") or 0) + 1
            tr["last_claim_failure"] = {"run": run.id, "why": str(err)[:300], "at": utcnow().isoformat()}
            srow.trust = tr
        log.warning("run %s on %s: its claim does not hold (%s): %s", run.id, e.id, verdict, err)
        self._quarantine(s, str(e.id), ev, owner_too=T.tampering(verdict), withdraw=True, after=after)
        ack = getattr(e, "ack", None)

        def written():
            if ack is not None:
                ack(run.id)
            _step(f"{job.ref}'s CPU event", lambda: _ledger().events([event]))
            lab.log(f"{job.exp} #{job.i} on {e.name}: its claim does not hold ({ev['words']}): the machine is "
                    "quarantined and the game played elsewhere")
        after.append(written)

    def _verified(self, s, e, run: Run, job: Job, row, rrow, wall: float, after: list) -> None:
        """A verification run's result (rule 4, 5): its own event (given by its machine's owner, used by nobody), then
        the comparison with what it checks. A replica equal to its primary (spec, jobs build, result hash, chain head
        and rounds) settles the job ``verified``; one that differs, from this server, quarantines the primary's machine;
        from a remote verifier, it asks this server for a tiebreak, whose result says who is wrong."""
        from citar.db.models import CpuJobRun
        local = e.kind == "local"
        rrow.verdict = "reference" if local else "unchecked"
        started_wall = run.started_wall or run.offered_wall
        observed = max(0.0, wall - started_wall)
        reference = None if local else self.costs.of(job.kind, job.spec)["cpu_s"]
        event = self._event_row(run.id, job.ref, None, run.machine, e.kind, run.role, run.cpu_s, observed, reference,
                                rrow.ended_at, "verified" if local else "pending")
        if not local:
            self._note_cpu(s, run.machine, run.id, event["flags"], run.cpu_s, observed, reference)
        primary = s.get(CpuJobRun, job.primary) if job.primary else None
        job.run = None
        take = getattr(e, "hold", None)
        if take is not None:
            from citar.pool import budgets
            budgets.keeper().release(take(run.id))      # verification is the server's own: no budget
        if local:
            # a reference machine's figures enter the cost table, as its primaries' do
            after.append(lambda: _step(f"{job.ref}'s cost", lambda: self.costs.record(
                job.kind, job.spec, cpu_s=run.cpu_s, peak_mb=run.peak_mb or None)))
        ack = None if local else getattr(e, "ack", None)
        after.append(lambda: ack(run.id) if ack is not None else None)
        # its event is written once what it decides is known (the list is read after the commit)
        after.append(lambda: _step(f"{job.ref}'s verification event", lambda: _ledger().events([event])))
        if primary is None or primary.state != "done":
            log.error("job %s's verification run %s has no primary result to compare with", job.id, run.id)
            event["verification"] = "verified" if local else "unverified"
            self._prune.append(run.id)
            return
        if not local and not self._verifies_now(s, e):
            # rule 2: only a verifier's replica checks a primary, and this machine stopped being one (unmarked, or its
            # tier changed) while it played: its word decides nothing, and the job waits for a verifier's replica
            rrow.error = {"code": "void", "why": "its machine no longer verifies others' results"}
            event["verification"] = "unverified"
            self._prune.append(run.id)
            lab.log(f"{job.exp} #{job.i}: {e.name}'s replica checks nothing (it no longer verifies); another is "
                    "offered")
            return
        if run.role == "tiebreak":
            self._tiebroken(s, job, row, primary, rrow, after)
            return
        if _same(primary, rrow):
            self._settle_match(s, job, row, primary, [rrow], True, after)
            return
        if local:
            self._caught(s, job, row, primary, rrow, after)
            return
        # a remote verifier disagrees: this server breaks the tie (rule 5)
        rrow.verdict = "mismatch"
        job.need, job.replica, job.local_only = "tiebreak", rrow.id, True
        lab.log(f"{job.exp} #{job.i}: {e.name}'s replica disagrees with its primary; this server breaks the tie")

    def _verifier_now(self, e) -> bool:
        """Whether a remote executor verifies now, as its executor last read its machine (the picks of a tick): its
        machine ``trusted`` and marked ``verifier``."""
        row = getattr(e, "_row", None)
        trust = row.get("trust") if isinstance(row, dict) else _machine_trust(e.id)
        return T.is_verifier(_facts_of(e).get("trust_tier"), trust)

    @staticmethod
    def _verifies_now(s, e) -> bool:
        """Whether a remote executor's machine verifies as this transaction reads its row (what a replica's result is
        judged by: an administrator's change made this tick counts)."""
        from citar.db.models import Server
        srow = s.get(Server, e.id)
        return srow is not None and T.is_verifier(srow.trust_tier, srow.trust)

    def _settle_match(self, s, job: Job, row, primary, checks: list, counts: bool, after: list) -> None:
        """A job whose primary its verification runs reproduced (rule 4): accepted, ``verified``, its line written
        (or, re-opened, its line's verification restored), the primary's machine's counters and tier moved (rule 6).
        The runs' results and digests are pruned once its line is written."""
        from citar.db.models import Server
        # a re-opened job's result was pruned with its line written: its mark says whether it was a crash
        crash = "crash" in (primary.result or {}) or bool((primary.error or {}).get("crashed"))
        exp = self.exps.get(job.exp)
        # a re-opened job whose result was pruned keeps the line its results file holds; one the file has lost (and
        # whose result is gone) cannot be lined again, and its game is played again, as a start's reconciliation does
        kept = (job.reopened and primary.result is None and exp is not None
                and (job.i, primary.id) in self._lines.get(str(exp.results), set()))
        lost = job.reopened and primary.result is None and exp is not None and not kept
        if lost:
            row.state, row.accepted_run, row.result_hash = "queued", None, None
        else:
            row.state = "crashed" if crash else "accepted"
            row.accepted_run, row.result_hash = primary.id, primary.result_hash
        primary.verdict, primary.error = "match", None
        for c in checks:
            c.verdict = "reference" if c.executor == "local" else "match"
        self.jobs.pop(job.id, None)
        if lost:
            view = self._view(row, exp)
            self.jobs[view.id] = view
            exp.settled.discard(job.i)
            lab.log(f"{job.exp} #{job.i}: verified, but its line is missing and its result gone; played again")
        updates = [{"activity": job.ref, "request_id": primary.id, "verification": "verified"}]
        updates += [{"activity": job.ref, "request_id": c.id, "verification": "verified"} for c in checks
                    if c.executor != "local"]
        # its reference cost is now its replica's own CPU seconds (P4.5.7: this job's, not its shape's in the cost
        # table): the primary's credits bounded again by it, and the machine's speed measured against it
        reference = self._reference_of(checks)
        remote_primary = primary.executor != "local"
        if remote_primary and reference is not None:
            updates[0].update(self._rebound(primary, reference))
        srow = s.get(Server, primary.machine_id)
        promoted = None
        if srow is not None:
            # the match is counted when the last check of it came (a promotion is dated so, rule 9 reading it)
            at = max((_aware(c.ended_at) for c in checks if c.ended_at is not None), default=None)
            tier, tr, promoted = T.on_match(srow.trust_tier or "new", dict(srow.trust or {}), T.settings(),
                                            counts=counts, flags=dict(srow.flags or {}), now=at)
            measured = remote_primary and reference is not None and counts
            if measured:
                tr = T.speed_sample(tr, reference, primary.cpu_s)
            if promoted:
                self._set_tier(s, srow, tier, f"{tr.get('verified')} verified, streak {tr.get('streak')}", tr)
            else:
                srow.trust = tr
            if measured and T.speed_of(tr) is not None:
                self._class_check(s, srow)
        prune = [c.id for c in checks]
        if job.reopened and not lost:
            # its line is in the results file already: its verification restored (the line below is appended only
            # when the file has lost it, as a start's reconciliation leaves it for its replica to settle)
            after.append(lambda: self._relabel(job.exp, job.kind, {primary.id: "verified"}))
        self._prune.extend(prune)
        line = None
        if primary.result is not None:
            started = primary.started_at or primary.offered_at
            line = self._line(job.kind, job.exp, job.i, job.spec, primary.result, primary.machine_id, primary.id,
                              primary.executor, primary.cpu_s, primary.wall_s, started, primary.ended_at, "verified")
        elif not job.reopened:
            log.error("job %s settled verified, but its primary's result is gone: played again at the next start",
                      job.id)

        def written():
            if line is not None:
                if exp is not None:
                    self._settle_line(exp, line, job.i, primary.id)
                else:
                    self._prune.append(primary.id)
                if crash and not job.reopened:
                    # a crash is final, recorded once it is accepted, as a crash this server played is
                    with open(lab.LAB / "crashes.log", "a", encoding="utf-8") as fh:
                        fh.write(f"--- {datetime.now():%Y-%m-%d %H:%M} {job.exp} #{job.i} final\n"
                                 f"{line.get('crash') or ''}\n")
            elif kept:
                # its line was there all along: its game is settled again (a start leaves a re-opened game out of
                # the settled ones until its replica settles it)
                exp.settled.add(job.i)
            _ledger().verdicts(updates)
            lab.log(f"{job.exp} #{job.i}: verified (its replica reproduced it)"
                    + (f"; {srow.name if srow is not None else primary.machine_id} is now {promoted}"
                       if promoted else ""))
        after.append(lambda: _step(f"{job.ref}'s settling", written))

    # -------------------------------------------------------------- credits (P4.5.7)
    @staticmethod
    def _reference_of(checks: list) -> Optional[float]:
        """A replicated job's reference cost: the CPU seconds of the verification run that reproduced it (this
        server's when one of them is: the reference machine's), or None when none says."""
        ordered = sorted(checks, key=lambda c: c.executor != "local")
        for c in ordered:
            if isinstance(c.cpu_s, (int, float)) and c.cpu_s > 0:
                return float(c.cpu_s)
        return None

    def _rebound(self, primary, reference: float) -> dict:
        """A remote primary's event bounded again once a replica reproduced it (P4.5.7): ``cpu_core_s = min(reported,
        observed x 1.05 + 1, 1.5 x the replica's own CPU seconds)``, its credits at its machine's class rate, its
        flags as that bound sets them. What ``Ledger.verdicts`` writes over the event with its verdict (a budget
        settlement moves by the difference)."""
        cpu, flags = bounded_cpu(primary.cpu_s, _observed(primary), reference)
        return {"cpu_core_s": round(cpu, 6), "credits": round(self._cpu_credits(primary.machine_id, cpu), 6),
                "flags": {**flags, "reference_s": round(reference, 6)},
                "unflag": [k for k in ("overreported_cpu", "capped_cpu") if k not in flags]}

    @staticmethod
    def _cpu_credits(machine: Optional[str], cpu: float) -> float:
        """Credits for ``cpu`` core-seconds on ``machine``: at its CPU class's rate (3-10), the class an administrator
        pinned or its hardware's."""
        from citar import ledger as compute, servers
        facts = (servers.machine_facts(machine) or {}) if machine else {}
        cls = compute.class_of(facts, "cpu", _ledger().classes())
        return compute.credits_for("cpu", cpu_core_s=cpu, rate=cls["rate"] if cls else 0.0)

    def _note_cpu(self, s, machine: str, run_id: str, flags: dict, reported: float, observed: float,
                  reference: Optional[float]) -> None:
        """A helper's run whose CPU seconds passed a bound (P4.5.7), counted on its machine's row with the figures:
        ``flags.overreported_cpu`` (more than the server observed of it, ``observed x 1.05 + 1``: a game is one thread,
        so no honest helper reaches it; it blocks the machine's promotion to ``trusted`` for ``overreport_days``, rule
        6) and ``flags.capped_cpu`` (more than 1.5 times the job's reference cost: informational, since a slow honest
        machine passes it too). The first of each kind writes an audit row."""
        hits = [k for k in ("overreported_cpu", "capped_cpu") if flags.get(k)]
        if not hits:
            return
        from citar.auth import audit
        from citar.db.models import Server
        srow = s.get(Server, machine)
        if srow is None:
            return
        mf = dict(srow.flags or {})
        detail = {"run": run_id, "reported_s": round(float(reported or 0.0), 3),
                  "observed_s": round(float(observed or 0.0), 3),
                  "reference_s": round(float(reference), 3) if reference else None}
        for k in hits:
            old = mf.get(k) if isinstance(mf.get(k), dict) else {}
            mf[k] = {**detail, "count": int(old.get("count") or 0) + 1, "last": utcnow().isoformat()}
            if not old:
                audit.record(s, f"server.{k}", object_type="server", object_id=machine, **detail)
        srow.flags = mf
        if "overreported_cpu" in hits:
            lab.log(f"machine {srow.name}: its helper reported {detail['reported_s']} CPU s for run {run_id}, which "
                    f"the server observed {detail['observed_s']} s of; credited the observed bound, and it is not "
                    "promoted to trusted meanwhile")

    def _class_check(self, s, srow) -> None:
        """``flags.class_suspect`` on the measured machines of ``srow``'s CPU class (P4.5.7): each one whose measured
        speed (``trust.speed``) is under half the median of the others' is flagged, for an administrator to pin its
        class (``class_override``); one back in line loses the flag. Quarantined machines are left out of it. The
        first flag of a machine writes an audit row."""
        from sqlalchemy import select
        from citar import ledger as compute
        from citar.auth import audit
        from citar.db.models import Server
        classes = _ledger().classes()

        def class_id(row) -> Optional[str]:
            facts = {"hardware": (row.config or {}).get("hardware") or {}, "class_override": bool(row.class_override),
                     "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id}
            c = compute.class_of(facts, "cpu", classes)
            return c["id"] if c else None
        mine = class_id(srow)
        peers = [r for r in s.scalars(select(Server).where(Server.reach == "worker"))
                 if (r.trust_tier or "new") != "quarantined" and T.speed_of(r.trust) is not None
                 and class_id(r) == mine]
        name = next((c["name"] for c in classes if c["id"] == mine), mine)
        at = utcnow().isoformat()
        for r in peers:
            speed = T.speed_of(r.trust)
            median = T.class_suspect(speed, [T.speed_of(o.trust) for o in peers if o.id != r.id])
            flags = dict(r.flags or {})
            had = isinstance(flags.get("class_suspect"), dict)
            if median is not None:
                flags["class_suspect"] = {"speed": speed, "class_median": round(median, 3), "class": mine,
                                          "class_name": name, "peers": len(peers) - 1, "at": at}
                if not had:
                    audit.record(s, "server.class_suspect", object_type="server", object_id=r.id, speed=speed,
                                 class_median=round(median, 3), cpu_class=mine)
                    lab.log(f"machine {r.name}: measured at {speed} of the reference, under half its CPU class's "
                            f"median ({median:.3g}): its class may be wrong (an administrator can pin it)")
            elif had:
                flags.pop("class_suspect", None)
            else:
                continue
            r.flags = flags

    def _caught(self, s, job: Job, row, primary, reference, after: list, others: tuple = ()) -> None:
        """A primary this server's own run disagrees with (rule 5): the primary's machine is quarantined with the
        verdict (``result``: the same chain and another result, tampering or an incompatible build; ``chain``: the
        round where they parted, hardware or an incompatible build), its owner too on tampering, everything unverified
        it gave since ``trust.since`` withdrawn, and this job played again elsewhere."""
        verdict, parted = _verdict(primary, reference)
        ev = T.evidence(verdict, run=primary.id, job=job.id, round=parted, against="this server")
        primary.verdict, primary.error = "mismatch", ev
        reference.verdict = "reference"
        for o in others:
            o.verdict = o.verdict or "unchecked"
        self._withdraw_job(s, row, primary, ev["words"], after, rejected=True)
        self._quarantine(s, primary.machine_id, ev, owner_too=T.tampering(verdict), withdraw=True, after=after)

    def _tiebroken(self, s, job: Job, row, primary, tiebreak, after: list) -> None:
        """This server's tiebreak between a primary and a remote verifier's replica that disagree (rule 5): every
        machine that disagrees with it is quarantined (a verifier can be wrong too); a primary that agrees is
        accepted, ``verified`` against this server."""
        from citar.db.models import CpuJobRun
        replica = s.get(CpuJobRun, job.replica) if job.replica else None
        tiebreak.verdict = "reference"
        if replica is not None and not _same(replica, tiebreak):
            verdict, parted = _verdict(replica, tiebreak)
            ev = T.evidence(verdict, run=replica.id, job=job.id, round=parted, against="this server (a tiebreak)")
            replica.verdict, replica.error = "mismatch", ev
            after.append(lambda r=replica.id: _ledger().verdicts([{"activity": job.ref, "request_id": r,
                                                                   "verification": "rejected",
                                                                   "why": ev["words"]}]))
            self._quarantine(s, replica.machine_id, ev, owner_too=T.tampering(verdict), withdraw=True, after=after)
        elif replica is not None:
            replica.verdict = "match"
            after.append(lambda r=replica.id: _ledger().verdicts([{"activity": job.ref, "request_id": r,
                                                                   "verification": "verified"}]))
        job.replica = None              # judged: its event has its verdict
        if _same(primary, tiebreak):
            self._settle_match(s, job, row, primary, [tiebreak], True, after)
            return
        self._caught(s, job, row, primary, tiebreak, after)

    def _set_tier(self, s, srow, tier: str, why: str, trust: dict, actor=None) -> None:
        """A machine's tier moved (promotion, demotion, quarantine, release): its row and one audit row
        (``server.trust``, the system's unless an administrator's)."""
        from citar.auth import audit
        before = srow.trust_tier or "new"
        srow.trust_tier, srow.trust = tier, trust
        if before != tier:
            audit.record(s, "server.trust", actor=actor, object_type="server", object_id=srow.id,
                         changed={"from": before, "to": tier}, why=why[:300])
            lab.log(f"machine {srow.name}: {before} -> {tier} ({why})")
        for ex in self.executors:
            if getattr(ex, "id", None) == srow.id and hasattr(ex, "_row_at"):
                ex._row_at = -1e18          # its executor reads the row (its trust) again at its next refresh

    def _quarantine(self, s, machine: str, ev: dict, *, owner_too: bool, withdraw: bool, after: list,
                    actor=None) -> list:
        """A machine quarantined (rule 7): no CPU job, and admission refuses it for anyone but its owner; its runs in
        flight aborted and their games played elsewhere; its results waiting for a replica withdrawn; with
        ``withdraw`` (always, but for an administrator who says otherwise), everything unverified it gave since
        ``trust.since`` withdrawn (rule 8); on tampering its owner quarantined too; every match it made as a verifier
        re-opened (rule 9), whether or not it is marked one now (an administrator may unmark a suspect verifier, or
        take its tier, before quarantining it). Returns the jobs withdrawn (the evidence lists them)."""
        from citar.db.models import Server
        srow = s.get(Server, machine)
        if srow is None:
            return []
        was = (srow.trust_tier or "new", dict(srow.trust or {}))
        tier, tr = T.on_mismatch(dict(srow.trust or {}), ev)
        withdrawn: list = []
        # its runs in flight: stopped, their games played elsewhere (nothing they did counts against the games)
        for run in [r for r in self.runs.values() if r.machine == machine]:
            self._asked.discard(run.id)
            try:
                run.executor.abort(run.id, "its machine is quarantined")
            except Exception:
                log.debug("could not abort run %s", run.id, exc_info=True)
            self._end(s, run, "aborted", {"code": "quarantined", "message": "its machine was quarantined"})
            self._requeue(s, run, attempt=False)
        # its results waiting for their replicas: never accepted, withdrawn
        from citar.db.models import CpuJob, CpuJobRun
        for job in [j for j in list(self.jobs.values()) if j.state == "settling" and j.primary_machine == machine]:
            jrow, prow = s.get(CpuJob, job.id), s.get(CpuJobRun, job.primary)
            if jrow is not None and prow is not None:
                withdrawn.append(self._withdraw_job(s, jrow, prow, ev["words"], after, rejected=True))
        if withdraw:
            withdrawn += self._withdraw_unverified(s, machine, T.since(was[1]), ev["words"], after)
        withdrawn = [w for w in withdrawn if w]
        if withdrawn:
            ev = {**ev, "withdrawn": len(withdrawn), "withdrawn_jobs": withdrawn[:100]}
            tr["last_mismatch"] = ev
        self._set_tier(s, srow, tier, ev.get("words") or ev.get("verdict") or "quarantined", tr, actor=actor)
        if owner_too and srow.owner_id:
            self._quarantine_owner(s, srow.owner_id, ev, actor=actor)
        # its matches as a verifier (none for a machine that never verified: the query finds nothing)
        self._reopen(s, machine, after)
        return withdrawn

    def _quarantine_owner(self, s, owner: str, ev: dict, actor=None) -> None:
        """A tampering verdict's owner quarantined (rule 7): every machine of theirs refused CPU work, a new one too,
        until an administrator releases them (``users.trust_tier``, the user administration's)."""
        from sqlalchemy import update
        from citar.auth import audit
        from citar.db.models import User
        from citar.jobs import remote
        changed = s.execute(update(User).where(User.id == owner, User.trust_tier != "quarantined")
                            .values(trust_tier="quarantined")).rowcount
        if changed:
            audit.record(s, "user.trust", actor=actor, object_type="user", object_id=owner,
                         changed={"to": "quarantined"}, why=str(ev.get("words") or "")[:300],
                         machine_run=ev.get("run"))
        remote.forget_owner_tiers()

    def _withdraw_unverified(self, s, machine: str, since: Optional[datetime], why: str, after: list) -> list:
        """Every result ``machine`` gave that was accepted without a replica (or whose replica's verifier was since
        quarantined), since ``since`` (its registration when None): withdrawn (rule 8). A matched replica proves only
        its own job, and a selective cheater is honest exactly when it guesses it is watched."""
        from sqlalchemy import select
        from citar.db.models import CpuJob, CpuJobRun
        q = (select(CpuJob, CpuJobRun).join(CpuJobRun, CpuJobRun.id == CpuJob.accepted_run)
             .where(CpuJob.state.in_(store.SETTLED + ("settling",)), CpuJobRun.machine_id == machine,
                    CpuJobRun.role == "primary", CpuJobRun.verdict == "unchecked"))
        if since is not None:
            q = q.where(CpuJobRun.ended_at >= since)
        out = []
        for jrow, prow in list(s.execute(q)):
            out.append(self._withdraw_job(s, jrow, prow, why, after, rejected=True))
        return out

    def _withdraw_job(self, s, jrow, prow, why: str, after: list, *, rejected: bool) -> Optional[dict]:
        """One job's result withdrawn (rule 8): the job ``withdrawn`` (its game played again elsewhere: a new job), its
        primary ``rejected`` and its event rejected (no credits, its budget settlement reversed), its line taken from
        the results file, the experiment back in the queue if it had gone to done/, and its verification runs in
        flight stopped. Returns what the evidence lists of it."""
        from citar.db.models import CpuJobRun
        from sqlalchemy import select
        name = jrow.item_ref or ""
        i = store.game_of(jrow.ref)
        jrow.state = "withdrawn"
        view = self.jobs.get(jrow.id)
        if view is not None and view.need == "tiebreak" and view.replica:
            # a remote verifier's replica that disagreed and waits for a tiebreak that will not come: its event,
            # ``pending``, is given ``unverified`` (one the tiebreak judged has its verdict already, and no replica)
            after.append(lambda rid=view.replica: _ledger().verdicts([{
                "activity": jrow.ref, "request_id": rid, "verification": "unverified",
                "why": "its job was withdrawn before a tiebreak"}]))
        if prow.verdict != "mismatch":
            prow.verdict = "rejected"       # a mismatch keeps its verdict, beside the withdrawal
        prow.error = {**(prow.error if isinstance(prow.error, dict) and prow.error.get("verdict") else {}),
                      "code": "withdrawn", "why": why[:300]}
        for r in s.scalars(select(CpuJobRun).where(CpuJobRun.job_id == jrow.id,
                                                   CpuJobRun.state.in_(("offered", "running")))):
            live = self.runs.get(r.id)
            if live is not None:
                self._asked.discard(live.id)
                try:
                    live.executor.abort(live.id, "its job was withdrawn")
                except Exception:
                    pass
                self._end(s, live, "aborted", {"code": "withdrawn", "message": "its job was withdrawn"})
            else:
                r.state, r.ended_at = "aborted", utcnow()
        self.jobs.pop(jrow.id, None)
        exp = self.exps.get(name)
        if exp is not None and i is not None:
            exp.settled.discard(i)
            exp.has_job.discard(i)
            exp.cursor = min(exp.cursor, i)
            exp.lines.pop(prow.id, None)
        kind = jrow.kind
        if rejected:
            after.append(lambda: _ledger().verdicts([{"activity": jrow.ref, "request_id": prow.id,
                                                      "verification": "rejected", "why": why}]))
        after.append(lambda: self._unline(name, kind, prow.id, back=exp is None))
        lab.log(f"{name} #{i}: its result from {prow.machine_id} withdrawn ({why}); it is played again")
        return {"job": jrow.id, "exp": name, "i": i, "run": prow.id}

    def _reopen(self, s, verifier: str, after: list) -> None:
        """A quarantined verifier's matches re-opened (rule 9): every job its replica or tiebreak runs verified returns
        to ``unverified`` (its line and its primary's event say so) and waits for a fresh replica on this server; every
        machine whose counters held those matches loses them (those it made before its last release were reset with
        it), and one whose promotion to its present tier counted any of them (a match made at or before that
        promotion) returns to ``new`` when it falls below that tier's threshold. A tier an administrator set was no
        promotion: it stays, its counters alone taken back (``trust.lose_matches``)."""
        from collections import defaultdict
        from sqlalchemy import select
        from citar.db.models import CpuJob, CpuJobRun, Server
        runs = list(s.scalars(select(CpuJobRun).where(CpuJobRun.machine_id == verifier,
                                                      CpuJobRun.role.in_(("replica", "tiebreak")),
                                                      CpuJobRun.verdict == "match")))
        lost: dict = defaultdict(list)      # machine -> when each of its lost matches was counted
        relabel: dict = {}
        for r in runs:
            r.verdict = "rejected"
            r.error = {"code": "reopened", "why": "its verifier was quarantined"}
            jrow = s.get(CpuJob, r.job_id)
            if jrow is None or jrow.state not in store.SETTLED or not jrow.accepted_run:
                continue
            prow = s.get(CpuJobRun, jrow.accepted_run)
            if prow is None or prow.verdict != "match":
                continue
            lost[prow.machine_id].append(_aware(r.ended_at) if r.ended_at is not None else None)
            crashed = jrow.state == "crashed"
            jrow.state = "settling"
            prow.verdict = "unchecked"
            prow.error = {"code": "replicating", "why": "reopened", "local_only": True, "crashed": crashed}
            relabel.setdefault((jrow.item_ref or "", jrow.kind), {})[prow.id] = "unverified"
            after.append(lambda ref=jrow.ref, rid=prow.id: _ledger().verdicts(
                [{"activity": ref, "request_id": rid, "verification": "unverified"}]))
            exp = self.exps.get(jrow.item_ref or "")
            if exp is not None:
                view = self._view(jrow, exp)
                view.state, view.need, view.primary, view.primary_machine = "settling", "replica", prow.id, \
                    prow.machine_id
                view.local_only, view.reopened = True, True
                self.jobs[view.id] = view
            else:
                after.append(lambda name=jrow.item_ref or "": self._back_to_queue(name))
        for (name, kind), labels in relabel.items():
            after.append(lambda n=name, k=kind, lb=labels: self._relabel(n, k, lb))
        sets = T.settings()
        for machine, times in lost.items():
            srow = s.get(Server, machine)
            if srow is None or srow.trust_tier == "quarantined":
                continue
            tier, tr = srow.trust_tier or "new", dict(srow.trust or {})
            since = T.since(tr)
            if since is not None:
                times = [t for t in times if t is None or t >= since]
            if not times:
                continue
            promoted = T.promoted_at(tier, tr)
            counted = sum(1 for t in times if promoted is not None and (t is None or t <= promoted))
            tier, tr, _demoted = T.lose_matches(tier, tr, len(times), sets, counted=counted)
            self._set_tier(s, srow, tier, f"{len(times)} match(es) against quarantined verifier {verifier} "
                                          "re-opened", tr)
        if runs:
            lab.log(f"verifier {verifier} quarantined: {sum(len(t) for t in lost.values())} job(s) it verified wait "
                    "for a fresh replica on this server")

    # -------------------------------------------------------------- results files after a verdict
    def _rewrite(self, name: str, kind: str, change) -> None:
        """An experiment's results file rewritten whole (a temporary file, then a replace, retried while Windows says
        it is held), each line given to ``change(obj)``, which returns the line to keep (None drops it); the lines
        the dispatcher knows the file holds follow."""
        path = lab.results_path(name, "reference" if kind == "reference_game" else "lab")
        lines, _cut = self._read_lines(path)
        keep, dropped, changed = [], set(), False
        for text, obj in lines:
            new = change(obj) if obj is not None else obj
            if obj is not None and new is None:
                changed = True
                dropped.add((obj.get("executor") or {}).get("run"))
                continue
            if obj is not None and new is not obj:
                changed = True
                keep.append(json.dumps(new))
            else:
                keep.append(text)
        if not changed:
            return
        tmp = path.with_name(path.name + ".tmp")

        def write():
            tmp.write_bytes("".join(t + "\n" for t in keep).encode("utf-8"))
            fsutil.replace(tmp, path)
        fsutil.retry(write)
        have = self._lines.get(str(path))
        if have is not None and dropped:
            for entry in [x for x in have if x[1] in dropped]:
                have.discard(entry)

    def _unline(self, name: str, kind: str, run: str, back: bool = False) -> None:
        """A withdrawn run's line taken from its experiment's results file (the ratings, keyed on the files, follow);
        with ``back``, its experiment, gone to done/, queued again so the game is played again."""
        try:
            self._rewrite(name, kind, lambda o: None if ((o.get("executor") or {}).get("run") == run) else o)
        except Exception:
            log.exception("could not remove run %s's line from %s's results; the next start does", run, name)
        if back:
            self._back_to_queue(name)

    def _relabel(self, name: str, kind: str, labels: dict) -> None:
        """Lines of an experiment's results file given another verification (``{run: verification}``)."""
        def change(o):
            ex = o.get("executor") or {}
            want = labels.get(ex.get("run"))
            if want is None or ex.get("verification") == want:
                return o
            return {**o, "executor": {**ex, "verification": want}}
        try:
            self._rewrite(name, kind, change)
        except Exception:
            log.exception("could not relabel %s's results", name)

    def _back_to_queue(self, name: str) -> None:
        """An experiment gone to done/ that has a game to play again: moved back to the queue, read at the next tick."""
        src, dst = lab.DONE / f"{name}.json", lab.QUEUE / f"{name}.json"
        if not name or dst.exists() or not src.exists():
            return
        try:
            fsutil.replace(src, dst)
            self._queue_sig = None
            lab.log(f"experiment {name} is queued again: a game of it is played again")
        except OSError:
            log.exception("could not queue experiment %s again", name)

    def _usage(self, job: Job, line: dict, machine: str) -> None:
        try:
            from citar import usage
            usage.append("lab", usage.lab_game_rows(job.exp, job.i, line, machine))
        except Exception:
            log.warning("could not write the usage ledger for %s", job.ref, exc_info=True)

    @staticmethod
    def _event_row(run_id: str, activity: str, consumer: Optional[str], machine: str, executor: str, role: str,
                   cpu_s: float, observed: float, reference: Optional[float], at,
                   verification: Optional[str] = None) -> dict:
        """A finished run's ``cpu`` event (P4.5.7): given by the executor's machine's owner (nobody for the host),
        used by the job's owner for a primary run and by nobody for a replica or a tiebreak (verification is the
        server's own work), its CPU bounded (by what the server observed and, for a helper's run, by 1.5 times
        ``reference``: the cost table's figure for the job's shape, until a replica gives the job's own). Its
        ``verification``: ``verified`` for the host's, else what verification has made of it so far (``pending``
        while a replica checks it, ``unverified`` when accepted without one); the writer's later verdicts move it
        (``Ledger.verdicts``). Its ``request_id`` is the run's, so a run has one event (``_repair_events``)."""
        from citar import servers
        facts = servers.machine_facts(machine) or {}
        provider = None if executor == "local" or facts.get("server_resource") or not facts else facts.get("owner_id")
        consumer = consumer if role == "primary" else None
        cpu, flags = bounded_cpu(cpu_s, observed, reference)
        if reference is not None and reference > 0:
            flags["reference_s"] = round(float(reference), 6)
        if isinstance(at, datetime) and at.tzinfo is None:
            at = at.replace(tzinfo=timezone.utc)
        return {"at": at if isinstance(at, datetime) else utcnow(), "activity": activity[:160],
                "activity_kind": "lab", "task_class": "cpu", "consumer_id": consumer, "provider_id": provider,
                "machine_id": (machine or "host")[:40], "kind": "cpu", "model": None, "gen_s": 0.0,
                "cpu_core_s": round(cpu, 6), "reported_s": round(float(cpu_s or 0.0), 6),
                "observed_s": round(float(observed or 0.0), 3), "tokens_in": 0, "tokens_out": 0,
                "tokens_reasoning": 0, "tokens_cache_read": 0, "tokens_cache_write": 0,
                "credits": round(Dispatcher._cpu_credits(machine, cpu), 6),
                "flags": flags, "self_use": consumer == provider,
                "verification": verification or ("verified" if executor == "local" else "unverified"),
                "request_id": run_id[:64]}

    # -------------------------------------------------------------- expiry
    def _expire(self, now: float) -> None:
        expired = []
        for run in list(self.runs.values()):
            e = run.executor
            if run.state == "offered" and now - run.offered_at > OFFER_TTL_SECONDS:
                if e.kind != "local" and (not getattr(e, "online", True) or getattr(e, "conn", None) is not run.conn):
                    # its offer went with its helper's connection, unanswered: nobody's abandon, no attempt
                    expired.append((run, "its offer was lost with its helper's connection", False))
                else:
                    expired.append((run, "no answer to its offer within 30 s", True))
            elif run.state == "running" and run.started_at is not None:
                paused = self._paused(run, now)
                if now > expires_at(run.started_at, run.deadline_s, paused):
                    caused = pause_caused(run.started_at, run.deadline_s, paused, now)
                    run.paused_s = max(run.paused_s, paused)
                    if caused:
                        self._shun_unless_benched(run, now)
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
        self._asked.clear()
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
    #: What clears a helper's re-offer backoff (P4.2.5): a ``status`` that shows room, its next acknowledgement of
    #: settings (or hello), or its next hello (a helper that stopped). The others (``bad_spec``, ``fault``: the helper's
    #: word against the server's checks) only their time clears.
    ROOM_CLEARS = ("busy", "quiet")
    ACK_CLEARS = ("build", "kind", "disabled", "limits", "unverifiable", "memory")
    HELLO_CLEARS = ("stopped",)

    def _backed_off(self, e, now: float) -> Optional[str]:
        """Why a helper is offered nothing for now after it refused a job (its reasons' backoff), dropping the reasons
        cleared since."""
        per = self._rejects.get(e.id)
        if not isinstance(per, dict):
            return None
        def since(what: str, at: float, seq: Optional[int]) -> bool:
            # by arrival where both have a place in it (a clock that has not moved orders nothing), else by time
            place = getattr(e, f"seq_{what}", None)
            if seq is not None and isinstance(place, int):
                return place > seq
            return getattr(e, f"last_{what}", 0.0) > at
        for reason, (until, _wait, why, at, seq) in list(per.items()):
            if ((reason in self.ROOM_CLEARS and since("room", at, seq))
                    or (reason in self.ACK_CLEARS and since("ack", at, seq))
                    or (reason in self.HELLO_CLEARS and since("hello", at, seq))):
                del per[reason]
            elif now < until:
                return why
        return None

    def backoff(self, machine: str) -> list:
        """The reasons a helper machine's re-offers are backed off for (tests, the administrators' view)."""
        per = self._rejects.get(machine)
        return sorted(per) if isinstance(per, dict) else []

    def _available(self, e, now: float) -> Optional[str]:
        """Why an executor is offered nothing now (None: it may be)."""
        if e.state != "ready":
            return e.reason or f"its job host is {e.state}"
        if e.id in self._paused_exec:
            return "a benchmark holds its machine"
        if e.kind != "local":
            why = self._backed_off(e, now) or self._held_new(e)
            if why is not None:
                return why
        else:
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

    def _hands(self) -> dict:
        """Each helper machine's work in hand (its runs in flight and its results waiting for a replica), counted once
        a tick (with the results waiting alone as ``_settling_by``)."""
        if self._hands_at == self.ticks and self._hands_cache is not None:
            return self._hands_cache
        hands: dict = defaultdict(int)
        settling: dict = defaultdict(int)
        for r in self.runs.values():
            if r.role == "primary":
                hands[r.machine] += 1
        for j in self.jobs.values():
            if j.state == "settling" and j.primary_machine:
                hands[j.primary_machine] += 1
                settling[j.primary_machine] += 1
        self._hands_at, self._hands_cache, self._settling_by = self.ticks, hands, settling
        return hands

    def _held_new(self, e) -> Optional[str]:
        """Why a ``new`` helper machine is offered nothing now (P4.5.8 rule 3), or None: its results wait for a
        verifier (none can take them: this server's job host does not verify; or as many wait as it has threads), or
        its owner already has ``new_per_owner`` other new machines with work in hand (a third waits)."""
        facts = _facts_of(e)
        if (facts.get("trust_tier") or "new") != "new":
            return None
        hands = self._hands()
        local = [x for x in self._own if x.kind == "local"]
        if not any(x.verifiable and x.want.get("enabled", True) for x in local):
            # its every result waits for a replica on this server, which cannot play one now
            return "waiting for a verifier: this server's job host does not verify now"
        waiting = self._settling_by.get(e.id, 0)
        if waiting >= max(1, int(e.threads)):
            return f"waiting for a verifier: {waiting} of its results wait for their replicas"
        owner = facts.get("owner_id")
        if owner:
            from citar import servers
            limit = int(T.settings()["new_per_owner"])
            others = 0
            for machine, n in hands.items():
                if machine == e.id or not n:
                    continue
                f = servers.machine_facts(machine) or {}
                if f.get("owner_id") == owner and (f.get("trust_tier") or "new") == "new":
                    others += 1
            if others >= limit:
                return (f"its owner's other new machines have work in hand ({others}; at most {limit} new machines "
                        "of one owner play at once until they are promoted)")
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
        within it, each executor the first job it does not refuse (and whose budget holds it, for a helper's). An offer
        that could not be sent ends at once, its job waiting again, nobody's abandon."""
        for key in [k for k, until in self._shun.items() if now >= until]:
            del self._shun[key]
        self._hands_cache = None            # counted afresh from this tick's runs, then with its offers
        ready = [e for e in self.executors if e.free > 0 and self._available(e, now) is None]
        queued = [j for j in self.jobs.values() if j.state == "queued"]
        verify = sorted((j for j in self.jobs.values() if j.state == "settling" and j.need and j.run is None),
                        key=lambda j: (j.created, j.i))
        if not ready or not (queued or verify):
            return
        levels: dict = defaultdict(lambda: defaultdict(list))
        for j in sorted(queued, key=lambda j: (j.created, j.i)):
            levels[j.priority][j.owner_id or ""].append(j)
        offers, memo = [], {}
        for e in ready:
            for _ in range(e.free):
                if e.kind != "local" and self._held_new(e) is not None:
                    break                   # a new machine's caps, with this tick's offers counted (rule 3)
                picked = self._next_for(e, levels, verify, memo, now)
                if picked is None:
                    break
                offers.append((e, *picked))
                if picked[2] == "primary":
                    self._hands()[e.id] += 1
        if not offers:
            return
        from citar import db
        from citar.db.models import CpuJob, CpuJobRun, new_id
        made, gone = [], []
        try:
            with db.session() as s:
                for e, job, hold, role in offers:
                    row = s.get(CpuJob, job.id)
                    if row is None:
                        gone.append((e, job, hold))
                        continue
                    cost = self.costs.of(job.kind, job.spec)["cpu_s"]
                    dl = deadline_s(cost, e.cpu_percent, e.cpus, e.threads, e.speed)
                    run = Run(id=new_id("cr_"), job=job.id, executor=e, machine=e.id, role=role, state="offered",
                              offered_at=now, deadline_s=dl, offered_wall=time.time(), conn=getattr(e, "conn", None))
                    at = utcnow()
                    s.add(CpuJobRun(id=run.id, job_id=job.id, machine_id=e.id[:40], executor=e.kind, role=role,
                                    state="offered", offered_at=at,
                                    deadline_at=datetime.fromtimestamp(at.timestamp() + dl, timezone.utc)))
                    if role == "primary":
                        row.state = "running"
                    made.append((e, job, run, hold))
        except Exception:
            for e, _job, hold, _role in offers:
                _let_go(hold)
            raise
        # committed: the view in memory takes the offers, and only then do the executors get them (a database that
        # refused them leaves every job waiting, as it was)
        for _e, job, hold in gone:
            self.jobs.pop(job.id, None)
            _let_go(hold)
        unsent = []
        for e, job, run, hold in made:
            if run.role == "primary":
                job.state = "running"
            job.run = run.id
            self.runs[run.id] = run
            adopt = getattr(e, "adopt", None)
            if adopt is not None:
                adopt(run.id, hold)
            try:
                e.offer(run.id, job, run.deadline_s)
            except Exception as exc:
                log.warning("offering run %s to executor %s failed: %s", run.id, getattr(e, "id", "?"), exc)
                unsent.append((run, exc))
        if unsent:
            try:
                with db.session() as s:
                    for run, exc in unsent:
                        self._end(s, run, "failed", {"code": "not_sent",
                                                     "message": f"its offer could not be sent: {exc}"[:1000]})
                        self._requeue(s, run, attempt=False)
            except Exception:
                self._reset("the database refused the offers that could not be sent")
                raise

    def _next_for(self, e, levels: dict, verify: list, memo: dict, now: float) -> Optional[tuple]:
        """The next run ``e`` plays: ``(job, budget hold, role)``, or None. A verification run first where ``e``
        verifies (P4.5.8: a sampled job's replica, a tiebreak, a re-opened job: work already given, waiting to settle);
        then, on this server, a ``new`` machine's replica or other work, the replicas taking at most
        ``new_verify_share`` of its runs while other work waits (rule 3: with one thread and a share of a half, every
        other run); then the best queued job ``e`` takes (``_pick``)."""
        mine = [j for j in verify if self._verifies(e, j)]
        settles = [j for j in mine if not j.of_new]
        if settles:
            verify.remove(settles[0])
            return settles[0], None, settles[0].need
        news = [j for j in mine if j.of_new]
        if news:
            other = self._pick(e, levels, memo, now, take=False)
            if other is not None:
                share = float(T.settings()["new_verify_share"])
                r, t = self._share.get(e.id, (0, 0))
                if r + 1 > share * (t + 1):
                    taken = self._pick(e, levels, memo, now)
                    if taken is not None:
                        self._share[e.id] = (r, t + 1)
                        return (*taken, "primary")
                else:
                    self._share[e.id] = (r + 1, t + 1)
            verify.remove(news[0])
            return news[0], None, news[0].need
        taken = self._pick(e, levels, memo, now)
        return (*taken, "primary") if taken is not None else None

    def _verifies(self, e, job: Job) -> bool:
        """Whether ``e`` may play ``job``'s verification run (P4.5.8 rule 2): this server's job host when its canary
        and self-check pass; otherwise a ``trusted`` machine an administrator marked ``verifier``, never the primary's
        own nor another of its owner's, and never for a job this server alone verifies (a new machine's replica, a
        tiebreak, a re-opened job). Either way it plays the job's kind, jobs build and memory."""
        if e.kind == "local":
            if not e.verifiable:
                return False
        else:
            if job.local_only or not e.verifiable or not self._verifier_now(e) or e.id == job.primary_machine:
                return False
            from citar import servers
            owner = (servers.machine_facts(job.primary_machine) or {}).get("owner_id")
            if owner is not None and owner == _facts_of(e).get("owner_id"):
                return False
        view = Job(id=job.id, kind=job.kind, ref=job.ref, exp=job.exp, i=job.i, owner_id=None, spec=job.spec,
                   spec_hash=job.spec_hash, jobs_build=job.jobs_build, priority=job.priority, mem_mb=job.mem_mb,
                   state=job.state, attempts=job.attempts, created=job.created)
        return e.refuses(view) is None

    def _pick(self, e, levels: dict, memo: dict, now: float, take: bool = True) -> Optional[tuple]:
        """The next job ``e`` takes and its budget hold (None for none), or None; with ``take`` False, only whether
        there is one (nothing taken, nothing held). ``memo`` keeps, for the tick, each executor's ``refuses`` by what
        it may look at (a job's kind, jobs build, memory and owner), so a tick with thousands of queued jobs asks each
        executor a few times, not thousands (P4.7.9)."""
        from citar.pool.budgets import BudgetExceeded
        reserve = getattr(e, "reserve", None)
        for p in sorted(levels, reverse=True):
            owners = sorted(levels[p], key=lambda o: self.served.get(o, -1))
            for owner in owners:
                jobs = levels[p][owner]
                for k, job in enumerate(jobs):
                    key = (e.id, job.kind, job.jobs_build, job.mem_mb, job.owner_id)
                    why = memo.get(key, _UNASKED)
                    if why is _UNASKED:
                        why = memo[key] = e.refuses(job)
                    if why is not None:
                        continue
                    until = self._shun.get((job.id, e.id))
                    if until is not None and now < until:
                        continue
                    if not take:
                        return job, None
                    hold = None
                    if reserve is not None:
                        try:
                            hold = reserve(job, self.costs.of(job.kind, job.spec)["cpu_s"])
                        except BudgetExceeded as exc:
                            # its owner's budget on this machine has no room: none of their jobs goes to it this tick
                            for other in [m for m in memo if m[0] == e.id and m[4] == job.owner_id]:
                                memo[other] = str(exc)
                            memo[key] = str(exc)
                            log.info("%s's budget on %s has no room for another run: %s", job.owner_id, e.id, exc)
                            continue
                    del jobs[k]
                    self._served_n += 1
                    self.served[owner] = self._served_n
                    return job, hold
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
            what, arg = self._requests.popleft()
            if what == "retry":
                self._retry_failed([arg] if arg else list(self.exps))
            elif what == "call":
                fn, done = arg
                self._call(fn, done)

    def call(self, fn: Callable, timeout: float = 30.0):
        """``fn(dispatcher, session, after)`` run on the dispatcher's thread at its next tick, in a session committed
        before ``after``'s callables run, and its value returned (or its exception raised): an administrator's change
        of a machine's trust (P4.5.8 rule 10), which the dispatcher, the only writer of the machines' trust and of the
        jobs a quarantine withdraws, makes. A dispatcher whose thread is not running (a test's) runs it at once on
        the caller's thread, under the tick's lock."""
        from concurrent.futures import Future
        done: Future = Future()
        if self._thread is None or not self._thread.is_alive():
            with self._tick_lock:
                self._call(fn, done)
        else:
            self._requests.append(("call", (fn, done)))
            self.poke()
        return done.result(timeout)

    def _call(self, fn: Callable, done) -> None:
        from citar import db
        after: list = []
        try:
            with db.session() as s:
                value = fn(self, s, after)
        except BaseException as exc:            # the caller's to see
            done.set_exception(exc)
            return
        for step in after:
            _step("an administrator's change", step)
        done.set_result(value)

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

    # -------------------------------------------------------------- the hub's questions
    def welcome_jobs(self, machine: str, running: list, results: list) -> tuple:
        """The welcome's ``jobs`` for a helper's hello (P4.2.2, P4.2.5): ``keep``, the runs of its ``running`` the
        dispatcher still holds for that machine (the others are ``drop``: given elsewhere, or expired); ``resend``, the
        runs of its ``results`` it still holds in flight (it wants their results). The rest of its held results it no
        longer wants (settled, expired or never its). What changes (a kept run's silence counted from now, a held run
        the helper no longer plays failed) is the dispatcher's thread's, at its next tick. Under the tick's lock: never
        on the event loop."""
        with self._tick_lock:
            from citar import db
            if db.configured() and not self.recovered:
                self.recover()
            if self.remotes is None:
                return [], list(running), []
            e = self.remotes.get(machine)
            mine = {rid for rid, r in self.runs.items() if r.executor is e}
            keep = [r for r in running if r in mine]
            drop = [r for r in running if r not in mine]
            resend = [r for r in results if r in mine]
            lost = sorted(mine - set(running) - set(results))
            e.frame({"t": "_reconnect", "keep": keep, "lost": lost})
            return keep, drop, resend

    # -------------------------------------------------------------- test hooks (the conformance transcripts' acts)
    def force_offer(self, machine: str, *, kind: str, spec: dict, spec_hash: str, jobs_build: str, mem_mb: int,
                    owner: Optional[str] = None) -> str:
        """An offer to a helper past the dispatcher's own checks (a test hook: ``force_offer``, for the helper's refusal
        of an offer a correct dispatcher never makes): a job of its own (``lab:forced-N:0``) and its run, offered at
        once. Returns the run's id."""
        from citar import db
        from citar.db.models import CpuJob, CpuJobRun, new_id
        with self._tick_lock:
            e = self.remotes.get(machine)
            self._forced += 1
            now, at = self.clock(), utcnow()
            job_id, run_id = new_id("cj_"), new_id("cr_")
            with db.session() as s:
                s.add(CpuJob(id=job_id, kind=kind, ref=f"lab:forced-{self._forced}:0", item_kind="lab_experiment",
                             item_ref=f"forced-{self._forced}", owner_id=owner, spec=spec, spec_hash=spec_hash,
                             jobs_build=jobs_build, priority=0, mem_mb=int(mem_mb), state="running", attempts=0,
                             created_at=at))
                s.add(CpuJobRun(id=run_id, job_id=job_id, machine_id=str(machine)[:40], executor=e.kind,
                                role="primary", state="offered", offered_at=at,
                                deadline_at=datetime.fromtimestamp(at.timestamp() + DEADLINE_MIN, timezone.utc)))
            job = Job(id=job_id, kind=kind, ref=f"lab:forced-{self._forced}:0", exp=f"forced-{self._forced}", i=0,
                      owner_id=owner, spec=spec, spec_hash=spec_hash, jobs_build=jobs_build, priority=0,
                      mem_mb=int(mem_mb), state="running", attempts=0, created=time.time(), run=run_id)
            self.jobs[job_id] = job
            self.runs[run_id] = Run(id=run_id, job=job_id, executor=e, machine=machine, role="primary",
                                    state="offered", offered_at=now, deadline_s=DEADLINE_MIN, offered_wall=time.time())
            e.offer(run_id, job, DEADLINE_MIN)
            return run_id

    def expire(self, run_id: str, why: str = "past its deadline") -> bool:
        """Expire a run as its deadline would (a test hook: ``expire_run``): it is aborted on its executor, ended
        ``expired`` and its job queued again. Returns whether it was in flight."""
        from citar import db
        with self._tick_lock:
            run = self.runs.get(run_id)
            if run is None:
                return False
            with db.session() as s:
                run.executor.abort(run.id, why)
                self._end(s, run, "expired", {"code": "expired", "message": why})
                self._requeue(s, run, attempt=True)
            return True

    def abort_run(self, run_id: str, why: str = "the server stopped it") -> bool:
        """Ask a run's executor to stop it (a test hook: ``abort_run``); its ``job_aborted`` ends it and queues its job
        again. Returns whether it was in flight."""
        with self._tick_lock:
            run = self.runs.get(run_id)
            if run is None:
                return False
            self._abort(run, why)
            return True

    def _abort(self, run: Run, why: str) -> None:
        """Ask a run's executor to stop it, the run kept until its ``job_aborted`` (or its result, which crossed the
        abort) comes: a helper's answer it asked for."""
        if run.executor.kind != "local":
            self._asked.add(run.id)
        run.executor.abort(run.id, why)

    # -------------------------------------------------------------- what others read
    def _publish(self, now: float) -> None:
        hour = time.time() - 3600
        while self.finished and self.finished[0] < hour:
            self.finished.popleft()
        running = []
        per_exp: dict = defaultdict(lambda: {"queued": 0, "running": 0, "settling": 0, "verifying": 0})
        for run in self.runs.values():
            job = self.jobs.get(run.job)
            if job is None:
                continue
            # a verification run is administrators' alone (P4.5.8 rule 2): counted apart, its role published
            per_exp[job.exp]["running" if run.role == "primary" else "verifying"] += 1
            started = run.started_at if run.started_at is not None else run.offered_at
            # its deadline, and how far its pauses have pushed the server's backstop on (P4.5.5: up to twice)
            paused = self._paused(run, now)
            extended = (expires_at(run.started_at, run.deadline_s, paused)
                        - expires_at(run.started_at, run.deadline_s, 0.0)) if run.started_at is not None else 0.0
            running.append({"exp": job.exp, "i": job.i, "job": job.id, "run": run.id, "kind": job.kind,
                            "role": run.role, "machine": run.machine, "executor": run.executor.name,
                            "executor_kind": run.executor.kind, "state": run.state,
                            "minutes": max(0.0, now - started) / 60.0, "turn": run.round, "limit": run.of,
                            "paused": run.paused, "attempt": job.attempts + 1, "owner": job.owner_id,
                            "peak_mb": round(run.peak_mb, 1), "cpu_s": round(run.cpu_s, 1),
                            "deadline_s": round(run.deadline_s, 1), "paused_s": round(paused, 2),
                            "extended_s": round(extended, 2)})
        firsts: dict = {}
        for j in self.jobs.values():
            if j.state == "settling":
                per_exp[j.exp]["settling"] += 1
            if j.state == "queued":
                per_exp[j.exp]["queued"] += 1
                if j.exp not in firsts or (j.created, j.i) < (firsts[j.exp].created, firsts[j.exp].i):
                    firsts[j.exp] = j
        exps = {}
        for name, exp in self.exps.items():
            info = dict(per_exp.get(name, {"queued": 0, "running": 0, "settling": 0, "verifying": 0}))
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
            # its verification runs, which a member's count of its runs leaves out (rule 2)
            d["verifying"] = sum(1 for r in self.runs.values() if r.executor is e and r.role != "primary")
            executors.append(d)
        self.published = {
            "dispatcher": {"alive": True, "since": self.started_at.isoformat(timespec="seconds"), "ticks": self.ticks,
                           "tick_ms": round(self.tick_ms, 2), "executors": executors,
                           "queued": sum(1 for j in self.jobs.values() if j.state == "queued"),
                           "running": len(self.runs), "games_last_hour": len(self.finished),
                           "jobs_build": self.jobs_build},
            "running": running, "experiments": exps}

    def hands_view(self) -> dict:
        """The caps' figures for administrators (P4.5.8 rule 3): each helper machine's work in hand (runs in flight and
        results waiting for a replica), the results waiting alone, and each verifier's share of new machines'
        replicas against its runs while both waited."""
        with self._tick_lock:
            hands = dict(self._hands())
            return {"in_hand": hands, "settling": dict(self._settling_by),
                    "share": {k: {"new_replicas": r, "runs": t} for k, (r, t) in self._share.items()}}

    def queued_view(self, limit: int = 500) -> list:
        """The queued jobs in the order they would run, for administrators: each with why it waits."""
        now = self.clock()
        with self._tick_lock:
            jobs = sorted((j for j in self.jobs.values() if j.state == "queued"),
                          key=lambda j: (-j.priority, j.created, j.i))[:limit]
            return [{"job": j.id, "exp": j.exp, "i": j.i, "kind": j.kind, "priority": j.priority,
                     "owner": j.owner_id, "mem_mb": j.mem_mb, "attempts": j.attempts,
                     "waiting": self.waiting(j, now)} for j in jobs]


#: ``_pick``'s memo: an executor not asked yet about a shape of job.
_UNASKED = object()


def _facts_of(e) -> dict:
    """An executor's machine's facts (its trust tier, its owner): a remote executor's, read once a tick, else the
    registry's."""
    facts = getattr(e, "_facts", None)
    if callable(facts):
        return facts() or {}
    from citar import servers
    return servers.machine_facts(e.id) or {}


def _machine_trust(machine: str) -> dict:
    """A machine row's ``trust`` (an executor that keeps no copy of it)."""
    from citar import db
    from citar.db.models import Server
    with db.session() as s:
        row = s.get(Server, machine)
        return dict(row.trust or {}) if row is not None else {}


def _same(a, b) -> bool:
    """Whether two runs of one job came to the same thing (P4.5.8 rule 4: their spec and jobs build are the job's):
    the same result hash, chain head and rounds."""
    return (a.result_hash == b.result_hash and a.chain_head == b.chain_head
            and int(a.rounds or 0) == int(b.rounds or 0))


def _parted(a: Optional[list], b: Optional[list]) -> Optional[int]:
    """The first round (1 for the first digest) where two chains' digests differ (``citar_jobs::parted_at``), or None
    when they hold the same digests or either side's are gone (a re-opened primary's were pruned with its line: no
    round can be named)."""
    if not a or not b:
        return None
    a, b = list(a), list(b)
    for n, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return n + 1
    return None if len(a) == len(b) else min(len(a), len(b)) + 1


def _verdict(primary, reference) -> tuple:
    """What a mismatch says (P4.5.8 rule 5): ``("result", None)`` for the same chain and another result (a correct
    engine of one jobs build cannot give it: tampering or an incompatible build), else ``("chain", round)``, the round
    where the two runs parted (hardware or an incompatible build)."""
    if primary.chain_head == reference.chain_head and int(primary.rounds or 0) == int(reference.rounds or 0):
        return "result", None
    return "chain", _parted(primary.digests, reference.digests)


def _line_verification(run) -> str:
    """A results line's ``executor.verification`` from its run: ``reference`` (this server's), ``verified`` (a
    replica reproduced it), ``unverified`` (accepted without one)."""
    if run.executor == "local":
        return "reference"
    return "verified" if run.verdict == "match" else "unverified"


def _event_verification(run, job) -> str:
    """A finished run's event's verification from what verification made of it (an event written again at a
    start)."""
    if run.verdict == "rejected":
        return "rejected"
    if run.executor == "local" or run.verdict in ("match", "reference"):
        return "verified"
    if run.role != "primary" and run.verdict == "unchecked":
        return "unverified"         # a helper's verification run that checked nothing (its machine stopped verifying)
    if job.state == "settling" and job.accepted_run != run.id:
        return "pending"
    return "unverified"


def _let_go(hold) -> None:
    """A budget hold whose run was never made or offered: let go, nothing spent."""
    if hold is not None:
        from citar.pool import budgets
        budgets.keeper().release(hold)


def _utc(wall: float) -> datetime:
    """A wall time as the database keeps times (UTC)."""
    return datetime.fromtimestamp(wall, timezone.utc)


def _utc_ts(t: datetime) -> float:
    """A database time as a wall time: a naive one (SQLite gives them so) is UTC."""
    return _aware(t).timestamp()


def _aware(t: datetime) -> datetime:
    """A database time with its zone: a naive one (SQLite gives them so) is UTC."""
    return t.replace(tzinfo=timezone.utc) if t.tzinfo is None else t


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


# ---------------------------------------------------------------------------- administrators' trust changes (P4.5.8)

def _applier() -> Dispatcher:
    """The dispatcher that makes an administrator's change: the process's, on its thread; else (a process with none:
    a command, a test's app) one of no executors, run here, whose withdrawals the server's dispatcher finds at its
    next start (a withdrawn job's line is removed and its game played again then)."""
    d = current()
    return d if d is not None else Dispatcher(executors=[], local=False)


def change_trust(machine: str, *, tier: Optional[str] = None, withdraw: bool = False, verifier: Optional[bool] = None,
                 actor_id: Optional[str] = None) -> dict:
    """An administrator's change of a machine's trust (P4.5.8 rule 10): its tier (``quarantined``: by hand, its results
    withdrawn only with ``withdraw``; ``new``: released, its counters reset and ``since`` now; ``provisional`` or
    ``trusted``: set), and its ``verifier`` mark (a trusted machine only). Made by the dispatcher, the only writer of
    the machines' trust. Returns ``{"tier", "verifier", "withdrawn"}``; ``ValueError`` for a change the rules refuse."""
    def change(d: Dispatcher, s, after):
        from citar.db.models import Server, User
        srow = s.get(Server, machine)
        if srow is None:
            raise LookupError(machine)
        actor = s.get(User, actor_id) if actor_id else None
        before_tier, before = srow.trust_tier or "new", dict(srow.trust or {})
        who = actor.handle if actor is not None else "an administrator"
        withdrawn: list = []
        if tier is not None and tier != before_tier:
            if tier == "quarantined":
                ev = T.evidence("administrator", why=f"quarantined by {who}")
                withdrawn = d._quarantine(s, machine, ev, owner_too=False, withdraw=withdraw, after=after,
                                          actor=actor)
            elif tier == "new":
                _t, tr = T.release(before)
                d._set_tier(s, srow, "new", "released from quarantine" if before_tier == "quarantined"
                            else f"set back to new by {who}", tr, actor=actor)
            else:
                tr = dict(before)
                if tier != "trusted":
                    tr["verifier"] = False
                # a tier an administrator set is no promotion: a quarantined verifier's matches never take it back
                tr.pop("promoted", None)
                d._set_tier(s, srow, tier, f"set by {who}", tr, actor=actor)
        if verifier is not None:
            tr = dict(srow.trust or {})
            if verifier and (srow.trust_tier or "new") != "trusted":
                raise ValueError("Only a trusted machine verifies others' results.")
            if bool(tr.get("verifier")) != bool(verifier):
                from citar.auth import audit
                tr["verifier"] = bool(verifier)
                srow.trust = tr
                audit.record(s, "server.verifier", actor=actor, object_type="server", object_id=machine,
                             verifier=bool(verifier))
                d._set_tier(s, srow, srow.trust_tier or "new", "", tr, actor=actor)
        return {"tier": srow.trust_tier or "new", "verifier": bool((srow.trust or {}).get("verifier")),
                "withdrawn": len(withdrawn)}
    return _applier().call(change)


def replicate_next(machine: str, n: int, *, actor_id: Optional[str] = None) -> dict:
    """An administrator's request that a machine's next ``n`` results be checked by replicas (P4.5.8 rule 10),
    whatever its tier's rate: kept on its row (``trust.replicate``) and counted down as its results come. Made by the
    dispatcher, the only writer of the machines' trust."""
    def change(d: Dispatcher, s, after):
        from citar.auth import audit
        from citar.db.models import Server, User
        srow = s.get(Server, machine)
        if srow is None:
            raise LookupError(machine)
        tr = dict(srow.trust or {})
        tr["replicate"] = int(n)
        srow.trust = tr
        audit.record(s, "server.replicate", actor=s.get(User, actor_id) if actor_id else None, object_type="server",
                     object_id=machine, n=int(n))
        return {"replicate": int(n)}
    return _applier().call(change)
