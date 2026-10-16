"""Remote executors (design/phase4-helper.md P4.5.6, P4.5.5): the connected CITAR helpers that lend their CPU, as the
job dispatcher's executors.

One :class:`RemoteExecutor` a machine, made the first time its helper says it plays jobs (a hello with a jobs build)
and kept while the dispatcher holds runs on it, connected or not: a run goes on across its helper's lost connection
(P4.2.5), and the next hello says whether it still plays it. The hub (``citar.server.workers``) gives it its
connection, the job frames its helper sends (each stamped when it arrived, ``citar.jobs.host.RX``), its ``status``
frames and its acknowledgements; the dispatcher's thread reads them (``poll``) and offers, aborts, pauses and resumes
through it, each as a protocol 2 frame sent over the helper's connection. It wakes the dispatcher (``bind``) on every
job frame.

**What it takes** (``refuses``, P4.5.5), from what the helper acknowledged (``WorkerConnection.effective``: the site's
settings held to the machine's own ceiling, CPU work on only when both say so):

- the same jobs build as the server's (a helper of another version or flavour is offered nothing);
- a kind its effective work lists (the site's opt-in and the machine owner's own);
- a job's ``mem_mb`` within the memory its job host leaves each run (``memory_per_run_mb``, its acknowledgement's);
- **until verification arrives (4-11), only a machine an administrator set ``trusted``**, and never a quarantined
  machine or one whose owner is quarantined;
- **who may use it:** a job of the server's own collection (no owner: ranking and reference runs, collection items) on
  any machine whose work lists its kind; a job of the machine's own owner; otherwise a grant on the machine's group
  whose purposes **list ``lab``** (``admission.check(..., purpose="lab")``: an empty list never lends CPU, and an
  administrator's own experiment needs a grant as a member's does), its window and budget included. The grants'
  answers are read once a tick, per owner (the dispatcher asks ``refuses`` of every queued job).

**When it is offered nothing** (``state``): its helper not connected, CPU work off in effect (the site's or the
machine's own switch: "not lent by this machine's owner"), its canary or self-check failed, its helper's own quiet
hours. The dispatcher adds the site's restricted hours, a live benchmark lease, the re-offer backoff (P4.2.5: cleared
for ``busy`` and ``quiet`` by a ``status`` that shows room, ``last_room``; for the reasons of its settings by its
next ``settings_ack`` or hello, ``last_ack``; for a helper that stopped by its next hello, ``last_hello``) and three
expired runs within the hour.

**What it reads is read once a tick** (``refresh``): the machine's facts (its trust tier, its owner) and its owner's
tier; what its helper acknowledged is clamped once for each acknowledgement and settings it has (``_effective``). The
dispatcher asks ``refuses`` of every queued job, so each answer costs a few lookups (P4.7.9's tick bound).

**A budget** of the grant a member's job runs under holds the job's estimated credits (its reference cost at the
machine's class rate, ``estimate``) from the moment the dispatcher picks the job for it (``reserve``: a hard budget that
would pass its amount refuses it, and that owner's jobs go to it no more this tick) to its end, when the run's event
settles it (``hold``) or it is let go. A run carried across the server's restart has no hold: its credits are settled
into its grant's budget all the same (``late_hold``), the work being done.
"""
from __future__ import annotations

import collections
import logging
import threading
import time
from typing import Callable, Optional

from citar.jobs import host as H

log = logging.getLogger("citar.jobs.remote")

#: How long an offer stands (``job_offer.offer_ttl_s``): the dispatcher expires it after ``OFFER_TTL_SECONDS``.
OFFER_TTL = 30.0
#: How many runs an executor remembers having offered, for the frames that come late (an accept after its offer
#: expired gets ``job_abort``).
REMEMBERED = 4096
#: How long the machine's row (its measured speed) is trusted before it is read again.
ROW_SECONDS = 30.0
#: A job frame's arrival on the dispatcher's clock (beside ``citar.jobs.host.RX``, the process's).
AT = "_at"


class RemoteExecutor:
    """A helper machine as the dispatcher's executor."""

    kind = "remote"

    def __init__(self, machine: str, name: str = "", *, clock: Callable[[], float] = time.monotonic):
        self.id = self.machine = machine
        self.name = name or machine
        self.clock = clock
        self.conn = None                                # its helper's connection, while one is connected
        self._frames: collections.deque = collections.deque()
        self.in_flight: set = set()
        self.offered: collections.OrderedDict = collections.OrderedDict()
        self.paused = False
        self._wake: Optional[Callable[[], None]] = None
        #: When (the dispatcher's clock) a ``status`` last showed room, a ``settings_ack`` or hello last came, and a hello
        #: last came: what clears the re-offer backoff (P4.2.5).
        self.last_room = 0.0
        self.last_ack = 0.0
        self.last_hello = 0.0
        self._decisions: dict = {}                      # owner -> (refusal or None, budget id), this tick
        self._facts_now: Optional[dict] = None          # the machine's facts, this tick
        self._eff: tuple = (None, None, None, None)     # (conn, its settings, its ack, their clamp's cpu)
        self._row: dict = {}
        self._row_at = -1e18
        self.holds: dict = {}                           # run -> its budget's hold
        self._lock = threading.Lock()
        #: How many of its helper's frames (job frames, statuses, acknowledgements) the hub has handed it.
        self.received = 0

    # -------------------------------------------------------------- the dispatcher's side
    def bind(self, wake: Callable[[], None]) -> None:
        self._wake = wake

    def _woken(self) -> None:
        wake = self._wake
        if wake is not None:
            wake()

    def _effective(self) -> Optional[dict]:
        """The CPU work its helper acknowledged, held to the site's settings (``WorkerConnection.effective``): clamped
        again only when the connection, its settings or its acknowledgement is another object (each is replaced whole
        when it changes, never changed in place)."""
        conn = self.conn
        if conn is None:
            return None
        settings, ack = getattr(conn, "settings", None), getattr(conn, "ack", None)
        c, s, a, cpu = self._eff
        if c is conn and s is settings and a is ack:
            return cpu
        eff = conn.effective()
        cpu = eff["cpu"] if eff is not None else None
        self._eff = (conn, settings, ack, cpu)
        return cpu

    @property
    def online(self) -> bool:
        conn = self.conn
        return conn is not None and conn.alive

    @property
    def want(self) -> dict:
        cpu = self._effective()
        return {"enabled": bool(cpu and cpu["enabled"])}

    @property
    def threads(self) -> int:
        cpu = self._effective()
        return int(cpu["threads"]) if cpu and cpu["enabled"] else 0

    @property
    def work(self) -> tuple:
        cpu = self._effective()
        return tuple(cpu["work"]) if cpu else ()

    @property
    def cpu_percent(self) -> int:
        cpu = self._effective()
        return int(cpu["cpu_percent"]) if cpu else 0

    @property
    def cpus(self) -> int:
        """The machine's logical CPUs, from its helper's scan (its hello's or its last ``hardware``), else 1."""
        conn = self.conn
        threads = getattr(conn, "cpu_threads", None) if conn is not None else None
        return int(threads) if isinstance(threads, int) and threads > 0 else 1

    @property
    def memory_per_run_mb(self) -> Optional[int]:
        conn = self.conn
        return getattr(conn, "memory_per_run_mb", None) if conn is not None else None

    @property
    def verifiable(self) -> bool:
        conn = self.conn
        return bool(conn is not None and (conn.hello.get("cpu") or {}).get("verifiable"))

    @property
    def jobs_build(self) -> Optional[str]:
        conn = self.conn
        return conn.jobs_build if conn is not None else None

    @property
    def speed(self) -> float:
        speed = (self._row.get("trust") or {}).get("speed")
        return float(speed) if isinstance(speed, (int, float)) and speed > 0 else 1.0

    @property
    def state(self) -> str:
        return self._state()[0]

    @property
    def reason(self) -> str:
        return self._state()[1]

    def _state(self) -> tuple:
        conn = self.conn
        if conn is None or not conn.alive:
            return "offline", "its helper is not connected"
        if conn.jobs_build is None:
            return "off", "its helper plays no CPU job (a build without jobs)"
        if not (conn.hello.get("cpu") or {}).get("enabled"):
            return "off", "not lent by this machine's owner (CPU lending is off in its helper)"
        cpu = self._effective()
        if not cpu or not cpu["enabled"]:
            return "off", "CPU work is off in its machine settings"
        if not self.verifiable:
            return "unverifiable", "its helper failed its canary or self-check"
        quiet = conn.quiet_until
        if quiet is not None and quiet > conn.clock.time():
            return "quiet", "its helper's own quiet hours"
        return "ready", ""

    @property
    def free(self) -> int:
        if self.state != "ready":
            return 0
        return max(0, self.threads - len(self.in_flight))

    def refresh(self, wanted: bool) -> None:
        """A tick begins: the grants' answers and the machine's facts are read again (once a tick), and the machine's
        row now and then."""
        self._decisions = {}
        self._facts_now = None
        now = self.clock()
        if now - self._row_at >= ROW_SECONDS:
            self._row_at = now
            self._row = _row_of(self.id)

    def _facts(self) -> dict:
        """The machine's facts (``servers.machine_facts``: its trust tier, its owner), read once a tick."""
        facts = self._facts_now
        if facts is None:
            from citar import servers
            facts = self._facts_now = servers.machine_facts(self.id) or {}
        return facts

    def refuses(self, job) -> Optional[str]:
        """Why this executor will not play ``job`` (None: it will). It looks at nothing of a job but its kind, jobs
        build, ``mem_mb`` and owner (the dispatcher caches its answers by those within a tick)."""
        build = self.jobs_build
        if build is None or job.jobs_build != build:
            return f"the job is for jobs build {job.jobs_build}, its helper's is {build or 'none'}"
        if job.kind not in self.work:
            return f"its CPU settings leave out {job.kind.replace('_', ' ')}s"
        per_run = self.memory_per_run_mb
        if per_run is not None and job.mem_mb > per_run:
            return f"it needs {job.mem_mb} MB a run, and its job host leaves each run {per_run} MB"
        facts = self._facts()
        tier = facts.get("trust_tier") or "new"
        if tier == "quarantined":
            return "it is quarantined"
        if tier != "trusted":
            return ("until results can be verified, only a machine an administrator trusted takes CPU work "
                    f"(this one is {tier})")
        owner = facts.get("owner_id")
        if owner is not None and _owner_tier(owner) == "quarantined":
            return "its owner is quarantined"
        if job.owner_id is None or job.owner_id == owner:
            return None
        return self._grant(job.owner_id)[0]

    def _grant(self, user_id: str) -> tuple:
        """(the refusal, or None, and the budget of the grant that decided it: the one a run of ``user_id``'s holds, or
        the one whose budget refused it), from the machine's grants for the lab (``admission.check(...,
        purpose="lab")``), once a tick per owner."""
        hit = self._decisions.get(user_id)
        if hit is not None:
            return hit
        from citar import db
        from citar.db.models import Server, User
        from citar.pool import admission
        try:
            with db.session() as s:
                user = s.get(User, user_id)
                server = s.get(Server, self.id)
                if server is None:
                    hit = ("its machine is gone", None)
                else:
                    d = admission.check(s, user, server, "lab", online=self.online)
                    budget = None
                    if d.grant is not None:
                        # its budget's account loaded and followed by the keeper, as a lease's is
                        from citar.pool import budgets
                        budget = budgets.keeper().handle(s, d.grant)
                    hit = (None if d.allowed else (d.reason or "no grant lends it for lab runs"), budget)
        except Exception:
            log.warning("could not check %s's grants for %s", self.id, user_id, exc_info=True)
            hit = ("its grants could not be read", None)
        self._decisions[user_id] = hit
        return hit

    def _budget_of(self, job) -> Optional[str]:
        """The budget a run of ``job`` counts against on this machine: its owner's grant's, for a member's job on another
        member's machine (none for the machine owner's own and the server's collection)."""
        if job.owner_id is None or job.owner_id == self._facts().get("owner_id"):
            return None
        return self._grant(job.owner_id)[1]

    def estimate(self, cost_cpu_s: float) -> float:
        """The credits a run of a job whose reference cost is ``cost_cpu_s`` is expected to take on this machine (at its
        CPU class's rate, as its event will be priced)."""
        from citar import ledger as compute
        cls = compute.class_of(self._facts(), "cpu", compute.get().classes())
        return compute.credits_for("cpu", cpu_core_s=max(0.0, float(cost_cpu_s or 0.0)),
                                   rate=cls["rate"] if cls else 0.0)

    def reserve(self, job, cost_cpu_s: float):
        """The budget hold of a run of ``job`` about to be offered here (None when it counts against no budget), or
        ``BudgetExceeded`` when a hard budget has no room for it: then its owner is refused here for the rest of the
        tick (``refuses`` says why)."""
        budget = self._budget_of(job)
        if not budget:
            return None
        from citar.pool import budgets
        try:
            return budgets.keeper().reserve(budget, job.ref, job.owner_id, self.estimate(cost_cpu_s))
        except budgets.BudgetExceeded as exc:
            self._decisions[job.owner_id] = (str(exc), budget)
            raise

    def adopt(self, run: str, hold) -> None:
        """``run`` holds ``hold`` (its offer's, from ``reserve``) until its event settles it or it is let go."""
        if hold is not None:
            self.holds[run] = hold

    def late_hold(self, job):
        """A hold for a run that has none (it was carried across the server's restart, whose holds were in memory):
        taken whatever the budget has left, since the work is done, so its credits still reach its grant's budget."""
        budget = self._budget_of(job)
        if not budget:
            return None
        from citar.pool import budgets
        return budgets.keeper().reserve(budget, job.ref, job.owner_id, 0.0, force=True)

    def describe(self) -> dict:
        conn = self.conn
        cpu = self._effective() or {}
        limits = (conn.ack or {}).get("limits") if conn is not None else None
        return {"machine": self.id, "name": self.name, "kind": self.kind, "on": self.want["enabled"],
                "state": self.state, "reason": self.reason or None, "threads": self.threads,
                "cpu_percent": cpu.get("cpu_percent", 0), "memory_mb": cpu.get("memory_mb", 0),
                "work": list(self.work), "running": len(self.in_flight), "paused": self.paused,
                "verifiable": self.verifiable if conn is not None else None, "held": limits,
                "memory_per_run_mb": self.memory_per_run_mb, "jobs_build": self.jobs_build, "online": self.online}

    def offer(self, run: str, job, deadline_s: float) -> None:
        """Sends ``run``'s ``job_offer``, or raises when its helper's connection is gone (the dispatcher then ends the
        run at once: an offer never sent is nobody's abandon)."""
        from citar.server import helper_protocol as HP
        conn = self.conn
        if conn is None or getattr(conn, "closed", False):
            raise RuntimeError(f"{self.id}'s helper is not connected")
        frame = HP.job_offer(job.id, run, job.kind, job.spec, job.spec_hash, job.jobs_build,
                             float(min(86400.0, max(1.0, deadline_s))), int(job.mem_mb),
                             int(max(-100, min(100, job.priority))), OFFER_TTL)
        conn.send_threadsafe(frame)
        self.in_flight.add(run)
        self._remember(run)

    def _remember(self, run: str) -> None:
        with self._lock:
            self.offered[run] = True
            while len(self.offered) > REMEMBERED:
                self.offered.popitem(last=False)

    def knows(self, run: str) -> bool:
        """Whether this executor's helper was ever offered ``run`` (a late frame of its own)."""
        with self._lock:
            return run in self.offered

    def hold(self, run: str):
        """The budget hold of ``run``'s offer, taken (the run's event settles it, or it is let go)."""
        return self.holds.pop(run, None)

    def release(self, run: str) -> None:
        """``run`` ended without a result: its budget hold goes, nothing spent, and its thread counts as free (a run
        the dispatcher ended itself, expired or lost, may never be answered: its helper may not know it, or have let
        it go silently as its welcome said; one it still plays answers its ``job_abort``, and an offer meanwhile is at
        worst refused ``busy``)."""
        self.in_flight.discard(run)
        hold = self.holds.pop(run, None)
        if hold is not None:
            from citar.pool import budgets
            budgets.keeper().release(hold)

    def _send(self, text: str) -> bool:
        conn = self.conn
        if conn is None or conn.closed:
            return False
        try:
            conn.send_threadsafe(text)
        except Exception:
            log.debug("could not send to %s's helper", self.id, exc_info=True)
            return False
        return True

    def abort(self, run: str, reason: str = "") -> None:
        from citar.server import helper_protocol as HP
        if run in self.in_flight or self.knows(run):
            self._send(HP.job_abort(run, (reason or "the server stopped it")[:200]))

    def ack(self, run: str) -> None:
        """``job_result_ack``: the result of ``run`` is committed (or settled already)."""
        from citar.server import helper_protocol as HP
        self._send(HP.job_result_ack(run))

    def pause(self) -> None:
        from citar.server import helper_protocol as HP
        self.paused = True
        self._send(HP.jobs_pause("benchmark"))

    def resume(self) -> None:
        from citar.server import helper_protocol as HP
        self.paused = False
        self._send(HP.jobs_resume())

    def poll(self) -> list:
        """The job frames its helper sent since the last look (never waiting)."""
        out = []
        while self._frames:
            f = self._frames.popleft()
            if f.get("t") in ("job_reject", "job_result", "job_failed", "job_aborted"):
                self.in_flight.discard(f.get("run"))
            out.append(f)
        return out

    def close(self) -> None:
        """Nothing to end: the connection is the hub's."""

    # -------------------------------------------------------------- the hub's side (its event loop: never blocks)
    def connected(self, conn) -> None:
        """Its helper connected (its welcome is on its way): offers go to it from now on; a pause in force is said
        again, after the welcome (a helper's ``jobs_pause`` holds for the connection it came on: one that ended while
        it was paused plays on, and is paused again here while the benchmark lasts)."""
        from citar.server import helper_protocol as HP
        self.conn = conn
        self.name = conn.server_name or self.name
        self.last_ack = self.last_hello = self.clock()
        if self.paused:
            conn.send_threadsafe(HP.jobs_pause("benchmark"))
        self._woken()

    def disconnected(self, conn) -> None:
        if self.conn is conn:
            self.conn = None
            self._woken()

    def frame(self, frame: dict) -> None:
        """A job frame its helper sent, stamped when it arrived: ``citar.jobs.host.RX`` (the process's monotonic
        clock) and ``AT``, the dispatcher's own clock (a test's runs on a clock of its own, which moves faster than the
        dispatcher's thread reads)."""
        frame.setdefault(H.RX, time.monotonic())
        frame[AT] = self.clock()
        self._frames.append(frame)
        self.received += 1
        self._woken()

    def status(self, frame: dict) -> None:
        """Its helper's ``status``: one that shows room (no quiet hours, a thread free) clears the backoff of ``busy``
        and ``quiet``."""
        if frame.get("quiet_until") is None and int(frame.get("jobs_running") or 0) < max(1, self.threads):
            self.last_room = self.clock()
            self._woken()
        self.received += 1

    def acknowledged(self) -> None:
        """Its helper acknowledged settings: the backoff of what its settings decide is cleared."""
        self.last_ack = self.clock()
        self.received += 1
        self._woken()


def _row_of(machine: str) -> dict:
    """The machine row's ``trust`` (its measured speed), read off the dispatcher's thread's tick."""
    try:
        from citar import db
        from citar.db.models import Server
        with db.session() as s:
            row = s.get(Server, machine)
            return {"trust": dict(row.trust or {})} if row is not None else {}
    except Exception:
        return {}


_OWNER_TIERS: dict = {}
_OWNER_AT: dict = {}


def _owner_tier(user_id: str) -> str:
    """An account's trust tier (``users.trust_tier``), read at most every ``ROW_SECONDS``."""
    now = time.monotonic()
    if now - _OWNER_AT.get(user_id, -1e18) < ROW_SECONDS:
        return _OWNER_TIERS.get(user_id, "new")
    tier = "new"
    try:
        from citar import db
        from citar.db.models import User
        with db.session() as s:
            u = s.get(User, user_id)
            tier = (u.trust_tier if u is not None else None) or "new"
    except Exception:
        pass
    _OWNER_TIERS[user_id], _OWNER_AT[user_id] = tier, now
    return tier


def forget_owner_tiers() -> None:
    """Read every account's trust tier again at the next look (an administrator changed one; tests)."""
    _OWNER_TIERS.clear()
    _OWNER_AT.clear()


class Remotes:
    """Every helper machine that plays jobs, as the dispatcher's executors: made when its helper first says it plays
    jobs, kept while the server runs."""

    def __init__(self):
        self._lock = threading.Lock()
        self._by_id: dict = {}
        self._wake: Optional[Callable[[], None]] = None
        self.clock: Callable[[], float] = time.monotonic

    def bind(self, wake: Callable[[], None], clock: Optional[Callable[[], float]] = None) -> None:
        """The dispatcher's ``poke`` (and its clock), given to every executor, now and when one is made."""
        with self._lock:
            self._wake = wake
            if clock is not None:
                self.clock = clock
            for e in self._by_id.values():
                e.bind(wake)
                e.clock = self.clock

    def get(self, machine: str, name: str = "") -> RemoteExecutor:
        with self._lock:
            e = self._by_id.get(machine)
            if e is None:
                e = self._by_id[machine] = RemoteExecutor(machine, name, clock=self.clock)
                if self._wake is not None:
                    e.bind(self._wake)
            return e

    def find(self, machine: str) -> Optional[RemoteExecutor]:
        with self._lock:
            return self._by_id.get(machine)

    def all(self) -> list:
        with self._lock:
            return list(self._by_id.values())

    def clear(self) -> None:
        with self._lock:
            self._by_id.clear()


_REMOTES = Remotes()


def registry() -> Remotes:
    """The process's remote executors."""
    return _REMOTES
