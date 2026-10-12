"""The prompt scheduler: one queue per machine for every model call (design/phase3-features.md P3.3.2-P3.3.4, P3.3.9).

Before it, every model call went straight to its provider: a helper machine whose slots were full answered ``busy`` and
the caller slept and retried, metering a request each time; a machine CITAR dials had no slot accounting at all; and a
cancelled request on a helper freed its slot in the hub while the helper went on generating. Here every call of a
scheduled machine (any row of the ``servers`` table) takes a slot first, and gives it back when it ends.

**Machines** (``Machine``): one per ``servers.id``, made on first use and shared by the process (game sessions, the
benchmark scheduler's games, probe runs and report narratives). Capacity is the row's ``max_concurrent`` (``servers.machine_settings``),
capped by the connected helper's own ceiling; a helper machine that is offline has no known capacity, so its lease
requests pass through (the provider's connection error drives the reconnect window) and its tickets are refused as
temporary. A disabled row refuses everything. The row's settings are read again by a request that finds them more than
``SETTINGS_SECONDS`` old, before it takes the machine's lock, from the registry's own cache (which asks the database at
most that often), or at once through ``refresh``; the helper's state is pushed by the hub when it connects, disconnects
or reports its models (``helper_changed``). The scheduler's own thread never reads the database.

**Bindings.** A ``Lease`` (task class *full*) is a model seat that plays every turn; it belongs to the session's seat
(``GameSession.lease_for``), not to its agent, is made when the session is registered and released when the seat stops
playing that machine (``GameSession.open_leases``, package 3-08), and is *active* while its seat is mid-turn and *idle*
(lending its slot, for a ``game`` lease) otherwise. A probe's turn case holds a ``probe_case`` lease for the case,
lending nothing; the leases live on a machine are what ``pool.seats.occupied`` lists. A ``Ticket`` (task class
*partial*) is one pooled prompt of up to ``steps`` calls: a chat answer, a diplomat phase, a probe's offer or message
case, a report's narrative, the Servers page's test. It is admitted when it is made (``pool.seats.ticket``: the
``pooled`` purpose, capped by the grant's concurrency), follows the queue priority of the work it is part of
(``set_priority``), and is listed on the Queue page (``views``). The rules, which ``tests/test_scheduler.py`` holds as
invariants over random schedules:

1. Requests in flight, draining ones included, never exceed capacity (a running ticket holds its slot between its
   calls).
2. Lease requests are served in arrival order and before any ticket, skipping one whose consumer is at its cap.
3. A ticket starts only when no benchmark lease is live or draining (else it is refused, unless it waits for
   benchmarks), its model is loaded (or the machine allows swaps, or a live lease uses that model), the machine is
   outside its quiet hours, its consumer is under its cap, and ``in flight + 1 <= capacity - reserved``, where
   *reserved* counts the leases that are active or never lend and have no request in flight.
4. So the reservation keeps slots from tickets only; a lease request may take any free slot.
5. A ticket that starts on an idle ``game`` lease's slot *borrows* it: it runs under its class's cap (time from its
   start, and each step's ``max_tokens``), and its next step is refused once the cap is reached or once a lease request
   waits for the slot, so a lobby seat whose turn comes waits for at most one capped step.

Among tickets: priority (the class's base plus the activity's queue priority), then round-robin across consumers
(the one served longest ago first), then first come first served. **The benchmark drain:** when a benchmark lease
becomes live, queued tickets that do not wait for benchmarks are refused and running ones cancelled, and the lease's
first request waits until no ticket request is in flight there (draining ones included), for at most
``drain_seconds``; the wait is its ``drain_s``, and a drain cut short by the bound marks it ``contaminated``.

**Draining.** A cancelled or timed-out request on a helper is still being generated there (the v1 helper ignores
``CANCEL``), so its slot stays taken (*draining*) until the hub hands the scheduler the request's late frame
(``late_frame``) or its request timeout plus the hub's grace has passed. The helper answers only over the connection
the request went out on, so a draining slot whose connection is gone (the helper disconnected, or a new connection
replaced it) is given back at once: a helper process still finishing it answers ``busy``, which the fallback absorbs.
A ``busy`` refusal anyway is never slept on: the machine's capacity is lowered to what is in flight until the next
frame frees a slot, and the request goes back to the head of its queue.

**Threading.** One lock and one condition per machine. Only ``acquire`` blocks, in ticks of at most a second, and in
test mode (``CITAR_TEST_LOCKS``) it asserts that the calling thread holds no game session's lock; ``cancel``,
``release``, ``idle``, ``activate``, ``close`` and the late-frame bookkeeping only update counters, so they are safe
under a session lock. Status callbacks are queued under the machine lock and delivered after it is released, by the
scheduler's one notifier thread, which also does the periodic housekeeping and survives an exception in its loop.

**Metering** (``ScheduledConversation``, P3.3.4): after every step, listeners (``add_listener``) receive a ``StepEvent``
with the generation time (on a helper the smaller of its reported ``Done.duration`` and the time the server observed
from sending the request to its ``Done``; on a machine CITAR dials, the call's wall time after the slot was granted),
the queue time, the token counts capped at the request's ``max_tokens`` and the model's context, and flags. The compute
ledger (``citar.ledger.listener``, package 3-10) is the process scheduler's listener: one ``compute_events`` row per
step, so an attempt that never reached the model meters nothing. A helper whose reported duration exceeds the observed
time by more than 10% plus a second on three requests within an hour gets ``flags.overreports`` on its machine row and
an audit row.

**Budgets** (P3.4.6): a binding made under a grant with a budget carries its id (``budget``). Each request reserves the
step's estimate once its slot is granted (``pool.budgets.Keeper.reserve``, in memory); a hard budget that the request
would take past its amount refuses it (``MachineRefused`` with ``code`` ``budget`` and the period's end as
``retry_at``), its slot given back; the step's event carries the reservation, which the ledger's writer settles. A soft
budget's first request past its amount is told to the binding's ``on_budget`` (on the notifier thread).

Replaces: ``LLMAgent._step``'s busy sleep (``citar/agents/llm_agent.py``, before 3-07), ``WorkerConversation.close``'s
no-op and the hub forgetting a request once it was resolved (``citar/server/workers.py``).
"""
from __future__ import annotations

import collections
import itertools
import logging
import math
import os
import queue as _queue
import secrets
import threading
import time
import weakref
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Callable, Optional

from ..agents.providers.base import Conversation, StepResult

log = logging.getLogger("citar.scheduler")

#: The ticket classes and their base priorities (P3.3.3); an activity's queue priority (-100..100) is added.
BASE_PRIORITY = {"test": 30, "chat": 20, "diplomat": 10, "probe": 0, "report": -10}
TICKET_CLASSES = tuple(BASE_PRIORITY)
#: How long a borrowing ticket may run from its start, by class (P3.3.3, rule 5).
BORROW_SECONDS = {"diplomat": 180.0, "chat": 120.0, "probe": 120.0, "report": 300.0, "test": 30.0}
#: The largest ``max_tokens`` a borrowing ticket's step may ask for, by class (rule 5).
BORROW_MAX_TOKENS = {"diplomat": 2048, "chat": 1024, "probe": 1024, "report": 4096, "test": 256}
#: A lease's kind: a lobby game (lends its slot while idle), a benchmark game, a probe's turn case (neither of the last
#: two lends). A report's narrative and a probe's offer and message cases are tickets (package 3-09).
LEASE_KINDS = ("game", "benchmark", "probe_case")
#: Calls a ticket may make once started, unless its maker says otherwise.
DEFAULT_STEPS = 8
#: How long a benchmark lease's first request waits for pooled requests to drain (``benchmark_drain_seconds``).
BENCHMARK_DRAIN_SECONDS = 120.0
#: The hub's grace beyond a request's own timeout (``WorkerHub.submit``): a draining request is given up after it.
DRAIN_GRACE_SECONDS = 15.0
#: How often a machine's row is read again (P3.3.2).
SETTINGS_SECONDS = 5.0
#: How long a capacity lowered by a ``busy`` refusal holds when no frame frees a slot meanwhile.
BUSY_HOLD_SECONDS = 5.0
#: The longest a blocked ``acquire`` sleeps before it looks at cancellation, a stopped session and its deadlines.
TICK_SECONDS = 1.0
#: Generation time assumed for a (model, class) with no history, for the ETA.
DEFAULT_GEN_SECONDS = 10.0
EWMA_ALPHA = 0.3
#: Over-reporting (P3.3.4): reported > observed * RATIO + SLACK, COUNT times within WINDOW seconds.
OVERREPORT_RATIO, OVERREPORT_SLACK, OVERREPORT_COUNT, OVERREPORT_WINDOW = 1.10, 1.0, 3, 3600.0


# ---------------------------------------------------------------------------- errors

class SchedulerError(RuntimeError):
    """A request the scheduler did not run. ``code`` says why (``disabled``, ``offline``, ``benchmark``,
    ``not_loaded``, ``quiet_hours``, ``duplicate``, ``borrow_cap``, ``steps``, ``yielded``, ``cancelled``,
    ``expired``, ``released``); ``retry_at`` (a naive local datetime) is set when the refusal is temporary and its end
    is known."""

    def __init__(self, message: str, *, code: str = "", retry_at: Optional[datetime] = None):
        super().__init__(message)
        self.code = code
        self.retry_at = retry_at

    @property
    def temporary(self) -> bool:
        """Whether trying again later may succeed."""
        return self.retry_at is not None or self.code == "offline"


class MachineRefused(SchedulerError):
    """The machine refused the request or ticket (P3.3.3, rule 3; a disabled row)."""


class TicketExpired(SchedulerError):
    """A ticket that did not start by its deadline: its caller falls back."""


class Cancelled(SchedulerError):
    """The request was cancelled while it waited (its conversation closed, its lease released, its ticket cancelled,
    its session stopped)."""


# ---------------------------------------------------------------------------- test mode: no session lock in acquire

#: Sessions whose lock ``acquire`` checks in test mode (``CITAR_TEST_LOCKS``); weak, so a session leaves by itself.
_SESSIONS: "weakref.WeakSet" = weakref.WeakSet()


def test_locks() -> bool:
    """Whether ``acquire`` asserts that its caller holds no session lock (P3.3.9; the test suite sets it)."""
    return os.environ.get("CITAR_TEST_LOCKS", "").strip().lower() in ("1", "true", "yes", "on")


def watch_session(session) -> None:
    """Register a game session whose lock ``acquire`` must never be entered with (test mode only)."""
    if test_locks():
        _SESSIONS.add(session)


def _assert_no_session_lock() -> None:
    """In test mode: raise when the calling thread holds a session's lock (an ``RLock`` knows its owner)."""
    if not test_locks():
        return
    for s in list(_SESSIONS):
        lock = getattr(s, "lock", None)
        is_owned = getattr(lock, "_is_owned", None)
        if is_owned is not None and is_owned():
            raise AssertionError(f"scheduler acquire entered with game {getattr(s, 'id', '?')}'s session lock held: "
                                 "a model call waits for a machine with the game locked (P3.3.9)")


# ---------------------------------------------------------------------------- what is known of a machine

@dataclass
class MachineSettings:
    """What the scheduler reads of a machine: the row's part (re-read every ``SETTINGS_SECONDS``) and the helper's
    (pushed by the hub). ``known`` false: no such row, so its calls pass through unscheduled."""
    known: bool = True
    enabled: bool = True
    llm: bool = True
    reach: str = "direct"
    provider: str = ""
    kind: str = ""
    name: str = ""
    slots: int = 1
    allow_swaps: bool = False
    #: Models a ticket may use without a swap; None: every model (a dry run, an online API, unknown).
    loaded: Optional[frozenset] = None
    #: When the quiet hours in force end (naive local time), or None outside them.
    quiet_until: Optional[datetime] = None
    #: A helper machine's state: connected, its own ceiling, the models it reports and their context lengths, and which
    #: connection it is (``WorkerConnection.serial``; None while offline).
    online: Optional[bool] = None
    helper_slots: Optional[int] = None
    helper_loaded: frozenset = frozenset()
    contexts: dict = field(default_factory=dict)
    connection: Optional[int] = None


def read_settings(server_id: str) -> MachineSettings:
    """A machine's row part, from ``servers.machine_settings`` (the registry's cache) and its quiet hours."""
    from .. import servers
    ms = servers.machine_settings(server_id)
    if ms is None:
        return MachineSettings(known=False)
    try:
        quiet = servers.restricted_at(server_id)
    except Exception:
        quiet = None
    provider = ms.get("provider") or ""
    st = MachineSettings(known=True, enabled=bool(ms.get("enabled", True)), llm=bool(ms.get("llm", True)),
                         reach=ms.get("reach") or "direct", provider=provider, kind=ms.get("kind") or "",
                         name=ms.get("name") or server_id, slots=max(1, int(ms.get("slots") or 1)),
                         allow_swaps=bool(ms.get("allow_swaps")), quiet_until=quiet)
    return st


def helper_state(server_id: str) -> dict:
    """A helper machine's state as the hub has it now: ``online``, ``helper_slots``, ``helper_loaded``, ``contexts``
    and ``connection`` (the connection's serial)."""
    try:
        from ..server.workers import hub
        connection = hub().get(server_id)
    except Exception:
        connection = None
    if connection is None:
        return {"online": False, "helper_slots": None, "helper_loaded": frozenset(), "contexts": {},
                "connection": None}
    models = list(connection.models or [])
    return {"online": True, "helper_slots": max(1, int(connection.max_concurrent or 1)),
            "helper_loaded": frozenset(m.get("key") for m in models if m.get("key") and m.get("loaded")),
            "contexts": {m.get("key"): m.get("context") for m in models if m.get("key") and m.get("context")},
            "connection": getattr(connection, "serial", None)}


def _lmstudio_loaded(server_id: str) -> Optional[frozenset]:
    """The models a direct LM Studio machine has loaded (its ``/api/v0/models``), or None when it cannot be asked."""
    from .. import servers
    sv = servers.find(server_id)
    base = ((sv or {}).get("connection") or {}).get("base_url") or ""
    if not base:
        return None
    root = base.rstrip("/")
    root = root[:-3] if root.endswith("/v1") else root
    try:
        import httpx
        r = httpx.get(f"{root}/api/v0/models", timeout=3)
        if r.status_code != 200:
            return None
        return frozenset(m.get("id") for m in r.json().get("data", []) if m.get("state") == "loaded")
    except Exception:
        return None


# ---------------------------------------------------------------------------- events

@dataclass
class StepEvent:
    """One model step, for the listeners (P3.3.4): the usage tracker now, the compute ledger from package 3-10."""
    machine: Optional[str]
    model: Optional[str]
    activity: Optional[str]
    consumer: Optional[str]
    task_class: str                 # full | partial
    cls: str                        # the lease's kind or the ticket's class
    gen_s: float
    reported_s: Optional[float]
    observed_s: Optional[float]
    queue_s: float
    usage: dict
    request_id: str
    at: float
    provider: str = ""
    flags: dict = field(default_factory=dict)
    #: The step's hold on its budget (``pool.budgets.Reservation``), which the compute ledger settles; None for none.
    reservation: Any = None


# ---------------------------------------------------------------------------- slots and requests

class _Slot:
    """A slot of a machine's capacity, taken by a lease request for its one call, or by a ticket from its start to its
    close. ``draining``: its request was abandoned but the helper may still be generating it, to answer over
    ``conn`` (the serial of the connection it went out on)."""
    __slots__ = ("binding", "request_id", "requesting", "draining", "drain_until", "taken", "conn")

    def __init__(self, binding, taken: float):
        self.binding = binding
        self.request_id = ""
        self.requesting = False
        self.draining = False
        self.drain_until = 0.0
        self.taken = taken
        self.conn: Optional[int] = None


class _Request:
    """One wait for a slot: a lease's call, or a ticket's start."""
    __slots__ = ("binding", "seq", "arrived", "state", "error", "slot", "conv", "position", "eta", "queued_s",
                 "reported_s", "pass_through")

    def __init__(self, binding, seq: int, arrived: float, conv=None):
        self.binding = binding
        self.seq = seq
        self.arrived = arrived
        self.state = "waiting"          # waiting | granted | refused | cancelled | expired
        self.error: Optional[SchedulerError] = None
        self.slot: Optional[_Slot] = None
        self.conv = conv
        self.position: Optional[int] = None
        self.eta: Optional[float] = None
        self.queued_s = 0.0          # every wait of it, a ``busy`` requeue's included
        self.reported_s = 0.0        # what of it the grants handed out already
        self.pass_through = False


@dataclass
class Grant:
    """What ``acquire`` hands its caller: the request id to send (the one ``late_frame`` names), the seconds this wait
    queued, whether it took no slot (an unscheduled or offline machine), and a ticket's step cap."""
    request: Optional[_Request]
    request_id: str
    queue_s: float
    pass_through: bool = False
    max_tokens: Optional[int] = None


# ---------------------------------------------------------------------------- bindings

class Binding:
    """What a conversation's calls are scheduled under: a lease or a ticket on one machine (None: unscheduled)."""
    is_lease = False
    task_class = ""

    def __init__(self, sched: "Scheduler", machine: Optional["Machine"], model: str, *, activity: Optional[str],
                 consumer: Optional[str], label: str, cap: int, on_status: Optional[Callable[[dict], None]],
                 budget: Optional[str] = None, on_budget: Optional[Callable[[dict], None]] = None):
        self.sched = sched
        self.machine = machine
        self.model = model or ""
        self.activity = activity
        self.consumer = consumer
        self.label = label
        self.cap = max(0, int(cap or 0))
        self.on_status = on_status
        #: The budget its requests reserve against (``pool.budgets``: the grant's that admitted it), and who hears a
        #: soft budget's warning (``{"warning", "budget"}``, on the notifier thread).
        self.budget = budget
        self.on_budget = on_budget
        self.id = secrets.token_hex(6)

    @property
    def server_id(self) -> Optional[str]:
        """The machine's id, or None when unscheduled."""
        return self.machine.id if self.machine is not None else None

    @property
    def cls(self) -> str:
        """The lease's kind or the ticket's class."""
        raise NotImplementedError

    def acquire(self, conv=None, halted: Optional[Callable[[], bool]] = None,
                on_wait: Optional[Callable[[Optional[dict]], None]] = None) -> Grant:
        """Wait for a slot for one call (blocking; never with a session lock held). ``on_wait``: see
        ``Machine.acquire``."""
        if self.machine is None:
            return Grant(None, _request_id(), 0.0, pass_through=True)
        return self.machine.acquire(self, conv, halted, on_wait)

    def wait(self, request: _Request, conv=None, halted=None) -> Grant:
        """Wait again on a request ``busy`` put back at the head of its queue."""
        return self.machine.wait(request, conv, halted)

    def finish(self, grant: Grant, *, unfinished: Optional[tuple] = None, gen_s: Optional[float] = None) -> None:
        """The call ended: free its slot, or keep it draining when ``unfinished`` (request id, seconds left)."""
        if self.machine is not None and grant.request is not None:
            self.machine.finish(grant, unfinished=unfinished, gen_s=gen_s)

    def busy(self, grant: Grant) -> None:
        """The helper refused the call as ``busy``: lower the capacity and put the request back at its queue's head.
        A request let through as the helper was thought offline reads the helper's state again (off every lock)."""
        if self.machine is not None and grant.request is not None:
            if self.machine.busy(grant):
                self.sched.helper_changed(self.machine.id)


class Lease(Binding):
    """A model seat that plays every turn (task class *full*; P3.3.3). It outlives the seat's agents. ``game`` is the
    game session whose seat holds it (None for a probe run's or a report's), and ``ref`` the work it serves as the
    shared queue names it (``(kind, id)``: a game, a benchmark run, a probe run, a report), for ``pool.seats.occupants``
    and the Queue page."""
    is_lease = True
    task_class = "full"

    def __init__(self, sched, machine, model, *, kind: str, game: Optional[str] = None, ref: Optional[tuple] = None,
                 **kw):
        if kind not in LEASE_KINDS:
            raise ValueError(f"Unknown lease kind {kind!r}.")
        super().__init__(sched, machine, model, **kw)
        self.kind = kind
        self.game = game
        self.ref = tuple(ref) if ref else None
        self.lends = kind == "game"
        self.active = False
        self.inflight = 0
        self.released = False
        # the benchmark drain (P3.3.3): a benchmark lease's first request waits for pooled requests to drain
        self.drained = kind != "benchmark"
        self.drain_s: Optional[float] = None
        self.contaminated = False

    @property
    def cls(self) -> str:
        return self.kind

    def activate(self) -> None:
        """Its seat is mid-turn: the slot is reserved from tickets (non-blocking)."""
        if self.machine is not None:
            self.machine.set_active(self, True)
        else:
            self.active = True

    def idle(self) -> None:
        """Its seat is between turns, or waiting on a chat or a pause: a ``game`` lease lends its slot (non-blocking)."""
        if self.machine is not None:
            self.machine.set_active(self, False)
        else:
            self.active = False

    def set_model(self, model: str) -> None:
        """The seat plays another model on the same machine (non-blocking): the loaded-model rule and the ETA read the
        model it plays now."""
        if self.machine is not None:
            with self.machine.lock:
                self.model = model or ""
        else:
            self.model = model or ""

    def release(self) -> None:
        """The seat no longer plays this machine (its game stopped or crashed, the seat changed): non-blocking."""
        if self.machine is not None:
            self.machine.release_lease(self)
        self.released = True

    def client(self) -> dict:
        """The lease as a status listing shows it."""
        return {"id": self.id, "kind": self.kind, "label": self.label, "model": self.model, "active": self.active,
                "in_flight": self.inflight, "drain_s": self.drain_s, "contaminated": self.contaminated,
                "game": self.game, "ref": list(self.ref) if self.ref else None}


class Ticket(Binding):
    """One pooled prompt of up to ``steps`` calls (task class *partial*; P3.3.3). ``ref`` is the work it is part of as
    the shared queue names it (``(kind, id)``: a report, a probe run, a game), whose queue priority it follows
    (``Scheduler.set_priority``) and which the Queue page decides its viewers' rights on; None for one that is part of
    nothing (the Servers page's test)."""
    task_class = "partial"

    def __init__(self, sched, machine, model, *, cls: str, priority: int, deadline: Optional[float], steps: int,
                 wait_for_benchmark: bool, ref: Optional[tuple] = None, **kw):
        if cls not in TICKET_CLASSES:
            raise ValueError(f"Unknown ticket class {cls!r}.")
        super().__init__(sched, machine, model, **kw)
        self.ticket_class = cls
        self.ref = tuple(ref) if ref else None
        #: the activity's queue priority (-100..100), which its owner or an administrator sets on the Queue page
        self.queue_priority = max(-100, min(100, int(priority or 0)))
        self.priority = BASE_PRIORITY[cls] + self.queue_priority
        self.created = sched.clock()
        self.deadline = deadline                 # the latest start, on the scheduler's clock
        self.steps = max(1, int(steps or DEFAULT_STEPS))
        self.wait_for_benchmark = bool(wait_for_benchmark)
        self.state = "queued"                    # queued | running | done | cancelled | expired | refused
        self.error: Optional[SchedulerError] = None
        self.steps_used = 0
        self.started: Optional[float] = None
        self.borrowing = False
        self.slot: Optional[_Slot] = None
        self.convs: set = set()

    @property
    def cls(self) -> str:
        return self.ticket_class

    @property
    def reason(self) -> str:
        """Why it did not run, when it did not."""
        return str(self.error) if self.error else ""

    @property
    def retry_at(self) -> Optional[datetime]:
        """When a temporary refusal ends, if known."""
        return self.error.retry_at if self.error else None

    def cancel(self, reason: str = "cancelled") -> None:
        """Cancel it (non-blocking): a queued ticket leaves the queue; a running one closes its conversation, whose
        request drains on a helper that keeps generating."""
        if self.machine is None:
            self.state = "cancelled"
            return
        for conv in self.machine.cancel_ticket(self, reason):
            try:
                conv.close()
            except Exception:
                log.debug("closing a cancelled ticket's conversation failed", exc_info=True)

    def close(self) -> None:
        """Its caller is done with it: its slot goes back (non-blocking)."""
        if self.machine is not None:
            self.machine.close_ticket(self)
        elif self.state in ("queued", "running"):
            self.state = "done"

    def client(self) -> dict:
        """The ticket as a status listing shows it."""
        return {"id": self.id, "class": self.ticket_class, "label": self.label, "model": self.model,
                "state": self.state, "priority": self.priority, "borrowing": self.borrowing,
                "steps_used": self.steps_used, "reason": self.reason or None,
                "ref": list(self.ref) if self.ref else None}


def _request_id() -> str:
    return secrets.token_hex(8)


def _call_quietly(fn: Callable, payload: Any) -> None:
    """A caller's hook (``on_wait``): its failure is logged, never the wait's."""
    try:
        fn(payload)
    except Exception:
        log.exception("a scheduler wait hook failed")


def _ended_state(error: SchedulerError) -> str:
    """The state a request or ticket ends in for ``error``."""
    if isinstance(error, TicketExpired):
        return "expired"
    if isinstance(error, Cancelled):
        return "cancelled"
    return "refused"


# ---------------------------------------------------------------------------- the machine

class Machine:
    """One machine's slots, leases and queue. Everything here runs under ``lock``; only ``acquire``/``wait`` block."""

    def __init__(self, sched: "Scheduler", server_id: str, settings: MachineSettings):
        self.sched = sched
        self.id = server_id
        self.lock = threading.Lock()
        self.cond = threading.Condition(self.lock)
        self.settings = settings
        self.read_at = sched.clock()
        self.loaded_at: Optional[float] = None
        self.leases: list[Lease] = []
        self.waiting: list[_Request] = []
        self.slots: list[_Slot] = []
        self.busy_cap: Optional[int] = None
        self.busy_since: Optional[float] = None
        self.last_served: dict = {}
        self.ewma: dict = {}
        self.late: collections.OrderedDict = collections.OrderedDict()   # request id -> clock, frames not yet matched
        self.overreports: collections.deque = collections.deque()
        #: The tickets made on it and not yet over (weak: a ticket its caller dropped leaves by itself), for the
        #: one-per-consumer rule (``Scheduler.ticket``'s ``one_per_consumer``).
        self.open_tickets: "weakref.WeakSet" = weakref.WeakSet()
        self._seq = itertools.count()
        self.counters = collections.Counter()
        self.max_in_flight = 0

    # -------------------------------------------------------------- what the rules read
    def capacity(self) -> Optional[int]:
        """Slots usable now, or None when unknown (no row, or a helper machine whose helper is offline)."""
        st = self.settings
        if not st.known:
            return None
        if st.reach == "worker":
            if not st.online:
                return None
            cap = min(st.slots, st.helper_slots or st.slots)
        else:
            cap = st.slots
        if self.busy_cap is not None:
            cap = min(cap, self.busy_cap)
        return max(0, cap)

    def _ticket_slots(self) -> int:
        return sum(1 for s in self.slots if not s.binding.is_lease)

    def _reserved(self) -> int:
        return sum(1 for lease in self.leases if (lease.active or not lease.lends) and lease.inflight == 0)

    def _idle_lenders(self) -> int:
        return sum(1 for lease in self.leases if lease.lends and not lease.active and lease.inflight == 0)

    def _bench_live(self) -> bool:
        """A benchmark holds the machine: a live benchmark lease, or a request of one still in flight or draining (a
        benchmark game that just stopped is still being generated for)."""
        return any(lease.kind == "benchmark" for lease in self.leases) or \
            any(s.binding.is_lease and s.binding.kind == "benchmark" for s in self.slots)

    def _running(self, consumer: Optional[str]) -> int:
        return sum(1 for s in self.slots if s.binding.consumer == consumer)

    def _at_cap(self, binding: Binding) -> bool:
        return bool(binding.cap) and binding.consumer is not None and self._running(binding.consumer) >= binding.cap

    def _model_ok(self, ticket: Ticket) -> bool:
        st = self.settings
        if st.allow_swaps or any(lease.model == ticket.model for lease in self.leases):
            return True
        loaded = st.helper_loaded if st.reach == "worker" else st.loaded
        return loaded is None or ticket.model in loaded

    def _quiet(self) -> Optional[datetime]:
        end = self.settings.quiet_until
        return end if end is not None and end > datetime.now() else None

    def _ticket_order(self, r: _Request):
        b = r.binding
        return (-b.priority, self.last_served.get(b.consumer, -math.inf), r.seq)

    def _ticket_refusal(self, ticket: Ticket, now: float) -> Optional[SchedulerError]:
        """Why a ticket may not start now, for the reasons that refuse rather than wait (rule 3 (a)-(c), a disabled
        row, an offline helper, its deadline)."""
        st = self.settings
        if not st.enabled or not st.llm:
            return MachineRefused(f"{st.name or self.id} is disabled.", code="disabled")
        if st.reach == "worker" and not st.online:
            return MachineRefused(f"{st.name or self.id} is offline: its helper is not connected.", code="offline")
        if self._bench_live() and not ticket.wait_for_benchmark:
            return MachineRefused(f"{st.name or self.id} is held by a benchmark.", code="benchmark")
        if not self._model_ok(ticket):
            return MachineRefused(f"The model is not loaded on {st.name or self.id} ({ticket.model}), and its owner "
                                  "does not allow pooled prompts to swap models.", code="not_loaded")
        quiet = self._quiet()
        if quiet is not None:
            return MachineRefused(f"{st.name or self.id} is in its quiet hours.", code="quiet_hours", retry_at=quiet)
        if ticket.deadline is not None and now > ticket.deadline:
            return TicketExpired("The prompt did not start in time.", code="expired")
        return None

    # -------------------------------------------------------------- the dispatcher
    def _dispatch(self, now: float) -> None:
        """Grant every waiting request the rules allow now, in the rules' order, then publish the positions of those
        still waiting. Lock held."""
        self._dispatch_loop(now)
        self._positions(now)

    def _dispatch_loop(self, now: float) -> None:
        while True:
            cap = self.capacity()
            if cap is None:
                # unknown capacity: a machine with no row is unscheduled; a helper machine whose helper is offline
                # lets its lease requests through (the provider's connection error drives the reconnect window) and
                # refuses its tickets as temporary. A ``busy`` answer to one let through (the helper was online after
                # all) holds them until a frame or the hold's end, as with a known capacity: never a request loop.
                for r in list(self.waiting):
                    if r.binding.is_lease or not self.settings.known:
                        if self.busy_cap is not None:
                            continue
                        self._grant(r, now, slot=False)
                    else:
                        self._end(r, self._ticket_refusal(r.binding, now) or MachineRefused(
                            "The machine cannot take pooled prompts now.", code="offline"))
                return
            if len(self.slots) >= cap:
                return
            leases = sorted((r for r in self.waiting if r.binding.is_lease), key=lambda r: r.seq)
            chosen, blocked = None, False
            for r in leases:
                if self._at_cap(r.binding):
                    continue
                lease = r.binding
                if not lease.drained:
                    tickets = self._ticket_slots()
                    if tickets and now - r.arrived < self.sched.drain_seconds:
                        blocked = True          # its first request waits for the drain; nothing behind it goes first
                        break
                    lease.drained = True
                    lease.drain_s = round(now - r.arrived, 3)
                    lease.contaminated = tickets > 0
                    if lease.contaminated:
                        log.warning("benchmark lease on %s went ahead after %.0f s with %d pooled request(s) still "
                                    "in flight: its timing is contaminated", self.id, lease.drain_s, tickets)
                    # its game records the drain in the benchmark job's result (package 3-08)
                    self._status(lease, {"state": "drained", "drain_s": lease.drain_s,
                                         "contaminated": lease.contaminated})
                chosen = r
                break
            if chosen is not None:
                self._grant(chosen, now)
                continue
            if blocked or any(not self._at_cap(r.binding) for r in leases):
                return
            tickets = [r for r in self.waiting if not r.binding.is_lease and not self._at_cap(r.binding)]
            if not tickets or self._bench_live():
                return
            reserved = self._reserved()
            if len(self.slots) + 1 > cap - reserved:
                return
            r = min(tickets, key=self._ticket_order)
            refusal = self._ticket_refusal(r.binding, now)
            if refusal is not None:
                self._end(r, refusal)
                continue
            r.binding.borrowing = cap - len(self.slots) - reserved - self._idle_lenders() < 1
            self._grant(r, now)

    def _grant(self, r: _Request, now: float, slot: bool = True) -> None:
        """Give ``r`` its slot (or let it pass through). Lock held."""
        self.waiting.remove(r)
        r.state = "granted"
        r.queued_s += max(0.0, now - r.arrived)
        b = r.binding
        r.pass_through = not slot           # a request let through once and put back by ``busy`` may get a slot now
        if slot:
            s = _Slot(b, now)
            self.slots.append(s)
            r.slot = s
            if b.is_lease:
                b.inflight += 1
            else:
                b.state = "running"
                b.started = now
                b.slot = s
                self.last_served[b.consumer] = now
            self.max_in_flight = max(self.max_in_flight, len(self.slots))
        self.counters["granted"] += 1
        self._status(b, {"state": "thinking" if b.is_lease else "running"})
        self.cond.notify_all()

    def _end(self, r: _Request, error: SchedulerError) -> None:
        """A waiting request that will not be granted: refused, expired or cancelled. Lock held."""
        if r in self.waiting:
            self.waiting.remove(r)
        r.state = _ended_state(error)
        r.error = error
        b = r.binding
        if not b.is_lease and b.state == "queued":
            b.state = r.state
            b.error = error
        self.counters[r.state] += 1
        payload = {"state": r.state, "reason": str(error), "code": error.code}
        if error.retry_at is not None:
            payload["retry_at"] = error.retry_at.isoformat()
        self._status(b, payload)
        self.cond.notify_all()

    def _free(self, s: _Slot, now: float, *, frame: bool) -> None:
        """A slot comes back. ``frame``: the request it held ended with an answer, which also ends a ``busy`` lowering
        of the capacity (the helper has a slot free again). Lock held."""
        if s in self.slots:
            self.slots.remove(s)
            b = s.binding
            if b.is_lease and not s.draining:
                b.inflight = max(0, b.inflight - 1)
            if not b.is_lease and b.slot is s:
                b.slot = None
        if frame and self.busy_cap is not None:
            self.busy_cap = None
            self.busy_since = None
        self.cond.notify_all()

    def _housekeep(self, now: float) -> None:
        """Expire what is due: draining slots past their bound, a ``busy`` lowering held too long, queued tickets past
        their deadline. Lock held."""
        for s in [s for s in self.slots if s.draining and now >= s.drain_until]:
            log.info("draining request %s on %s given up after its timeout", s.request_id, self.id)
            self.counters["drain_timeouts"] += 1
            self._free(s, now, frame=False)
        if self.busy_since is not None and now - self.busy_since >= BUSY_HOLD_SECONDS:
            self.busy_cap = None
            self.busy_since = None
            self.cond.notify_all()
        for r in [r for r in self.waiting if not r.binding.is_lease]:
            deadline = r.binding.deadline
            if deadline is not None and now > deadline:
                self._end(r, TicketExpired("The prompt did not start in time.", code="expired"))
        for rid in [rid for rid, at in self.late.items() if now - at > 300]:
            del self.late[rid]

    def _positions(self, now: float, force: bool = False) -> None:
        """Work out each waiting request's position and ETA and publish them to its status callback, when they changed
        (``force``: work them out even when no callback wants them, for a caller about to wait). Lock held."""
        if not force and not any(r.binding.on_status for r in self.waiting):
            return
        for r, i, eta in self._line(now):
            if (r.position, r.eta) != (i, eta):
                r.position, r.eta = i, eta
                self._status(r.binding, {"state": "queued", "position": i, "eta_s": eta})

    def _line(self, now: float) -> list:
        """The waiting requests in the order the rules serve them (lease requests by arrival, then tickets by priority,
        round-robin and arrival), each as ``(request, position, ETA)``: the expected time of the work ahead of it over
        the slots it may use. Lock held."""
        order = sorted((r for r in self.waiting if r.binding.is_lease), key=lambda r: r.seq) + \
            sorted((r for r in self.waiting if not r.binding.is_lease), key=self._ticket_order)
        ahead = sum(max(0.0, self._expected(s.binding) - (now - s.taken)) for s in self.slots)
        cap = self.capacity() or 1
        reserved = self._reserved()
        out = []
        for i, r in enumerate(order, start=1):
            usable = cap if r.binding.is_lease else max(1, cap - reserved)
            out.append((r, i, round(ahead / usable, 1)))
            ahead += self._expected(r.binding)
        return out

    def _expected(self, b: Binding) -> float:
        return self.ewma.get((b.model, b.cls), DEFAULT_GEN_SECONDS)

    def _status(self, b: Binding, payload: dict) -> None:
        if b.on_status is not None:
            self.sched._notify(b.on_status, {"binding": b.id, "machine": self.id, **payload})

    # -------------------------------------------------------------- the non-blocking core (tests drive it directly)
    def submit(self, binding: Binding, conv=None) -> _Request:
        """Put one call's request in the queue and grant what can be granted; returns the request (its ``state``).
        A row last read more than ``SETTINGS_SECONDS`` ago is read again first, off the lock."""
        self.sched._fresh(self)
        with self.lock:
            now = self.sched.clock()
            r = _Request(binding, next(self._seq), now, conv)
            refusal = self._refuse_at_once(binding, now)
            if refusal is not None:
                r.state = _ended_state(refusal)
                r.error = refusal
                if not binding.is_lease and binding.state == "queued":
                    binding.state, binding.error = r.state, refusal
                self.counters[r.state] += 1
                return r
            self.waiting.append(r)
            self._housekeep(now)
            self._dispatch(now)
            if r.state == "waiting" and r.position is None:
                self._positions(now, force=True)
            return r

    def _refuse_at_once(self, b: Binding, now: float) -> Optional[SchedulerError]:
        """A request refused before it queues: a disabled machine, a released lease, a ticket that cannot start."""
        st = self.settings
        if st.known and (not st.enabled or not st.llm):
            return MachineRefused(f"{st.name or self.id} is disabled.", code="disabled")
        if b.is_lease:
            if b.released or b not in self.leases:
                return Cancelled("This seat's lease was released.", code="released")
            return None
        if b.state != "queued":
            return b.error or Cancelled(f"The prompt is {b.state}.", code=b.state)
        refusal = self._ticket_refusal(b, now)
        if refusal is not None and refusal.code == "benchmark" and b.wait_for_benchmark:
            return None
        return refusal

    def step_ticket(self, ticket: Ticket) -> Grant:
        """A running ticket's next call: it goes at once, on the slot the ticket holds, unless the ticket used its
        steps or, borrowing, its class's time, or a lease request waits for the slot it borrowed (rule 5); the ticket is
        then over, its slot given back, and why is raised."""
        with self.lock:
            now = self.sched.clock()
            error = None
            if ticket.state != "running" or ticket.slot is None:
                error = ticket.error or Cancelled(f"The prompt is {ticket.state}.", code=ticket.state)
            elif ticket.steps_used >= ticket.steps:
                error = MachineRefused("The prompt used all its steps.", code="steps")
            elif ticket.borrowing and now - ticket.started > BORROW_SECONDS[ticket.ticket_class]:
                error = MachineRefused("The prompt ran out of borrowed time on this machine.", code="borrow_cap")
            elif ticket.borrowing and self._lease_waits():
                error = MachineRefused("A game playing on this machine needs its slot back.", code="yielded")
            if error is not None:
                if ticket.state == "running":
                    ticket.state, ticket.error = "done", error
                    if ticket.slot is not None and not ticket.slot.draining:
                        self._free(ticket.slot, now, frame=False)
                    self._dispatch(now)
                raise error
            ticket.steps_used += 1
            rid = _request_id()
            ticket.slot.requesting, ticket.slot.request_id = True, rid
            req = _Request(ticket, -1, now)
            req.state, req.slot = "granted", ticket.slot
            return Grant(req, rid, 0.0, max_tokens=self._step_cap(ticket))

    def _lease_waits(self) -> bool:
        return any(r.binding.is_lease and not self._at_cap(r.binding) for r in self.waiting)

    def _gone(self, conn: Optional[int]) -> bool:
        """Whether a helper request sent over connection ``conn`` can no longer be answered: the helper is offline, or
        another connection has replaced that one (its answer would come only over the socket it went out on). Lock
        held."""
        st = self.settings
        return st.reach == "worker" and (not st.online or conn != st.connection)

    def _drop_drain(self, s: _Slot, now: float) -> None:
        """Give back a draining slot whose request's connection is gone (P3.3.3): no late frame can come for it. A
        helper process still finishing it answers ``busy`` meanwhile, which the fallback absorbs. Lock held."""
        log.info("draining request %s on %s given up: the helper's connection it went out on is gone",
                 s.request_id, self.id)
        self.counters["drains_dropped"] += 1
        self._free(s, now, frame=False)

    def finish(self, grant: Grant, *, unfinished: Optional[tuple] = None, gen_s: Optional[float] = None) -> None:
        """A call ended (non-blocking). ``unfinished`` (request id, seconds left[, the serial of the helper's connection
        it went out on]): the helper may still be generating it, so its slot drains until the late frame, the bound, or
        that connection's end (at once when it has gone already)."""
        r = grant.request
        with self.lock:
            now = self.sched.clock()
            b = r.binding
            if gen_s is not None and gen_s > 0:
                key = (b.model, b.cls)
                old = self.ewma.get(key)
                self.ewma[key] = gen_s if old is None else old + EWMA_ALPHA * (gen_s - old)
            s = r.slot
            if r.pass_through or s is None:
                return
            s.requesting = False
            ticket_over = not b.is_lease and b.state != "running"
            if unfinished is not None:
                rid, left = unfinished[0], unfinished[1]
                conn = unfinished[2] if len(unfinished) > 2 and unfinished[2] is not None else self.settings.connection
                if rid in self.late:                    # its frame came while the caller was still unwinding
                    del self.late[rid]
                    self._free(s, now, frame=True)
                else:
                    if b.is_lease:
                        b.inflight = max(0, b.inflight - 1)
                    s.draining, s.request_id, s.conn = True, rid, conn
                    s.drain_until = now + max(0.0, float(left or 0.0))
                    self.counters["draining"] += 1
                    if not b.is_lease and b.state == "running":
                        b.state = "cancelled"
                    if self._gone(conn):                # its connection went while the caller was unwinding
                        self._drop_drain(s, now)
            elif b.is_lease or ticket_over:
                self._free(s, now, frame=True)
            else:
                # a ticket's step that ended keeps its slot; the step's answer is a frame all the same
                if self.busy_cap is not None:
                    self.busy_cap, self.busy_since = None, None
            self._dispatch(now)

    def busy(self, grant: Grant) -> bool:
        """The helper refused the call as ``busy`` although the scheduler counted a free slot (P3.3.3): no sleep, no
        meter. The capacity is lowered to what else is in flight until a frame frees a slot, and the request goes
        back to the head of its queue (its place kept) to wait like any other. Returns whether the request had been
        let through with the capacity unknown (the helper thought offline): its answer says the helper is there, so
        the caller reads the helper's state again (``Scheduler.helper_changed``, off the lock)."""
        r = grant.request
        with self.lock:
            now = self.sched.clock()
            b = r.binding
            passed = r.pass_through
            if r.slot is not None and r.slot in self.slots:
                self.slots.remove(r.slot)
                if b.is_lease:
                    b.inflight = max(0, b.inflight - 1)
                else:
                    b.state, b.slot, b.started = "queued", None, None
                    b.steps_used = max(0, b.steps_used - 1)
            self.busy_cap = len(self.slots)
            self.busy_since = now
            self.counters["busy_refusals"] += 1
            log.warning("%s refused a request as busy with %d of its slots in use by the scheduler's count: its "
                        "capacity is held at %d until a slot frees", self.id, len(self.slots), self.busy_cap)
            r.state, r.slot, r.arrived, r.pass_through = "waiting", None, now, False
            self.waiting.append(r)
            self._dispatch(now)
            return passed

    def withdraw(self, r: _Request, error: Optional[SchedulerError] = None) -> None:
        """Take a waiting request out of the queue (non-blocking)."""
        with self.lock:
            if r.state == "waiting":
                self._end(r, error or Cancelled("The request was cancelled.", code="cancelled"))
                self._dispatch(self.sched.clock())

    def late_frame(self, request_id: str) -> bool:
        """The helper's ``Done`` or error for a request the hub had already given up (non-blocking): its draining slot
        comes back. Returns whether one did."""
        with self.lock:
            now = self.sched.clock()
            self.counters["late_frames"] += 1
            for s in self.slots:
                if s.draining and s.request_id == request_id:
                    self._free(s, now, frame=True)
                    self._dispatch(now)
                    return True
            self.late[request_id] = now           # the caller has not marked it draining yet (finish)
            if len(self.late) > 1000:
                self.late.popitem(last=False)
            return False

    # -------------------------------------------------------------- the blocking layer
    def acquire(self, binding: Binding, conv=None, halted=None, on_wait=None) -> Grant:
        """Wait for a slot for one call: a lease's request, or a ticket's start (then its later calls go at once, on
        its slot). Raises ``SchedulerError`` when it will not run. Blocks, in ticks of at most a second; never call it
        with a session lock held (asserted in test mode). ``on_wait``, when the request has to wait, is called on this
        thread (no lock held) with its place (``position``, ``eta_s``, ``machine``) before it blocks, and with None
        once the wait is over, however it ended."""
        _assert_no_session_lock()
        if not binding.is_lease and binding.state == "running":
            return self.step_ticket(binding)
        r = self.submit(binding, conv)
        if on_wait is None or r.state != "waiting":
            return self.wait(r, conv, halted)
        _call_quietly(on_wait, {"position": r.position, "eta_s": r.eta, "machine": self.settings.name or self.id})
        try:
            return self.wait(r, conv, halted)
        finally:
            _call_quietly(on_wait, None)

    @staticmethod
    def _step_cap(binding: Binding) -> Optional[int]:
        if binding.is_lease or not binding.borrowing:
            return None
        return BORROW_MAX_TOKENS[binding.ticket_class]

    def wait(self, r: _Request, conv=None, halted=None) -> Grant:
        """Block until ``r`` is granted or ends; returns its grant or raises why it ended."""
        b = r.binding
        with self.lock:
            while r.state == "waiting":
                if (halted is not None and halted()) or getattr(conv, "_closed", False) or \
                        (b.is_lease and b.released):
                    self._end(r, Cancelled("The request was cancelled.", code="cancelled"))
                    self._dispatch(self.sched.clock())
                    break
                self.cond.wait(timeout=TICK_SECONDS)
                now = self.sched.clock()
                self._housekeep(now)
                self._dispatch(now)
            return self._take(r)

    def take(self, r: _Request) -> Optional[Grant]:
        """Non-blocking ``wait``: the grant when ``r`` was granted, None while it waits; raises why it ended."""
        with self.lock:
            return None if r.state == "waiting" else self._take(r)

    def _take(self, r: _Request) -> Grant:
        if r.state != "granted":
            raise r.error or Cancelled("The request was cancelled.", code="cancelled")
        b = r.binding
        rid = _request_id()
        if r.slot is not None:
            r.slot.request_id = rid
            r.slot.requesting = True
            if not b.is_lease:
                b.steps_used += 1
        queued, r.reported_s = r.queued_s - r.reported_s, r.queued_s       # this wait's, not the earlier ones'
        return Grant(r, rid, queued, pass_through=r.pass_through, max_tokens=self._step_cap(b))

    # -------------------------------------------------------------- bindings' bookkeeping (non-blocking)
    def add_lease(self, lease: Lease) -> list:
        """A lease becomes live; a benchmark lease starts the drain (P3.3.3). Returns the conversations of the tickets
        it cancelled, which the caller closes once the lock is let go."""
        closing = []
        with self.lock:
            now = self.sched.clock()
            self.leases.append(lease)
            if lease.kind == "benchmark":
                for r in [r for r in self.waiting if not r.binding.is_lease and not r.binding.wait_for_benchmark]:
                    self._end(r, MachineRefused(f"{self.settings.name or self.id} is held by a benchmark.",
                                                code="benchmark"))
                for s in list(self.slots):
                    t = s.binding
                    if not t.is_lease and t.state == "running":
                        t.state = "cancelled"
                        t.error = MachineRefused("A benchmark took the machine.", code="benchmark")
                        self.counters["cancelled"] += 1
                        if s.requesting:
                            closing.extend(t.convs)         # its request drains on the helper
                        else:
                            self._free(s, now, frame=False)
                        self._status(t, {"state": "cancelled", "reason": str(t.error), "code": "benchmark"})
            self._dispatch(now)
        return closing

    def release_lease(self, lease: Lease) -> None:
        with self.lock:
            if lease in self.leases:
                self.leases.remove(lease)
            lease.released = True
            for r in [r for r in self.waiting if r.binding is lease]:
                self._end(r, Cancelled("This seat's lease was released.", code="released"))
            self._dispatch(self.sched.clock())

    def set_active(self, lease: Lease, active: bool) -> None:
        with self.lock:
            if lease.active != active:
                lease.active = active
                if not active:
                    self._dispatch(self.sched.clock())

    def cancel_ticket(self, ticket: Ticket, reason: str) -> list:
        """Returns the conversations to close (outside the lock)."""
        closing = []
        with self.lock:
            now = self.sched.clock()
            if ticket.state == "queued":
                waiting = [r for r in self.waiting if r.binding is ticket]
                for r in waiting:
                    self._end(r, Cancelled(f"The prompt was {reason}.", code="cancelled"))
                if not waiting:
                    ticket.state, ticket.error = "cancelled", Cancelled(f"The prompt was {reason}.", code="cancelled")
                    self.counters["cancelled"] += 1
            elif ticket.state == "running":
                ticket.state, ticket.error = "cancelled", Cancelled(f"The prompt was {reason}.", code="cancelled")
                self.counters["cancelled"] += 1
                if ticket.slot is not None and ticket.slot.requesting:
                    closing.extend(ticket.convs)
                elif ticket.slot is not None:
                    self._free(ticket.slot, now, frame=False)
                self._status(ticket, {"state": "cancelled", "reason": reason, "code": "cancelled"})
            self._dispatch(now)
        return closing

    def close_ticket(self, ticket: Ticket) -> None:
        with self.lock:
            now = self.sched.clock()
            if ticket.state == "queued":
                for r in [r for r in self.waiting if r.binding is ticket]:
                    self._end(r, Cancelled("The prompt was closed.", code="cancelled"))
            if ticket.state in ("queued", "running"):
                ticket.state = "done"
                self._status(ticket, {"state": "done"})
            if ticket.slot is not None and not ticket.slot.draining and not ticket.slot.requesting:
                self._free(ticket.slot, now, frame=False)
            self._dispatch(now)

    # -------------------------------------------------------------- settings
    def apply(self, settings: MachineSettings) -> None:
        """New row settings (the helper's part kept: the hub pushes that)."""
        with self.lock:
            old = self.settings
            settings.online, settings.helper_slots = old.online, old.helper_slots
            settings.helper_loaded, settings.contexts = old.helper_loaded, old.contexts
            settings.connection = old.connection
            if settings.loaded is None:
                settings.loaded = old.loaded          # a direct LM Studio's, asked of it by ``refresh_loaded``
            self.settings = settings
            now = self.sched.clock()
            self.read_at = now
            self._dispatch(now)

    def apply_helper(self, state: dict) -> None:
        """The helper's state as the hub has it now (a connect, a disconnect, a drop, a catalog). When its connection
        went or another replaced it, what drained on the old one is given back (no late frame can come over a socket
        that is gone), and a ``busy`` hold of the old connection ends: the new one says what it can take."""
        with self.lock:
            st = self.settings
            before = st.connection
            st.online = state.get("online")
            st.helper_slots = state.get("helper_slots")
            st.helper_loaded = frozenset(state.get("helper_loaded") or ())
            st.contexts = dict(state.get("contexts") or {})
            st.connection = state.get("connection") if st.online else None
            now = self.sched.clock()
            if st.reach == "worker":
                for s in [s for s in self.slots if s.draining and self._gone(s.conn)]:
                    self._drop_drain(s, now)
                if st.connection != before and self.busy_cap is not None:
                    self.busy_cap = self.busy_since = None
            self._dispatch(now)

    def note_overreport(self, wall: float) -> Optional[int]:
        """One step whose reported duration exceeded the observed one: returns the count to flag when the machine
        crossed the bar (three within an hour), else None."""
        with self.lock:
            self.overreports.append(wall)
            while self.overreports and wall - self.overreports[0] > OVERREPORT_WINDOW:
                self.overreports.popleft()
            self.counters["overreports"] += 1
            if len(self.overreports) >= OVERREPORT_COUNT:
                return self.counters["overreports"]
            return None

    def status(self) -> dict:
        """The machine as an administrator's status listing shows it."""
        with self.lock:
            return {"id": self.id, "name": self.settings.name, "capacity": self.capacity(),
                    "slots": self.settings.slots, "in_flight": len(self.slots),
                    "draining": sum(1 for s in self.slots if s.draining),
                    "waiting": len(self.waiting), "busy_cap": self.busy_cap,
                    "leases": [lease.client() for lease in self.leases],
                    "tickets": [s.binding.client() for s in self.slots if not s.binding.is_lease]
                    + [r.binding.client() for r in self.waiting if not r.binding.is_lease],
                    "max_in_flight": self.max_in_flight, "counters": dict(self.counters)}

    def view(self) -> dict:
        """The machine as the Queue page lists it (P3.3.8): its capacity, its leases (active or lending, a request of
        theirs waiting), the tickets running on it and those waiting, in the order the rules will serve them, with their
        place in the machine's line (lease requests ahead of them included), how long they have waited, their ETA and
        how long they may still wait to start. Every entry keeps its consumer and the work it is part of (``ref``): the
        route decides what each viewer is told of them."""
        with self.lock:
            now = self.sched.clock()
            line = self._line(now)
            asking = {r.binding.id for r, _, _ in line if r.binding.is_lease}
            leases = [{"id": lease.id, "kind": lease.kind, "label": lease.label, "model": lease.model,
                       "consumer": lease.consumer, "ref": list(lease.ref) if lease.ref else None, "game": lease.game,
                       "active": lease.active, "lending": lease.lends and not lease.active,
                       "in_flight": lease.inflight, "waiting": lease.id in asking} for lease in self.leases]
            running, seen = [], set()
            for s in self.slots:
                t = s.binding
                if t.is_lease or t.id in seen:
                    continue
                seen.add(t.id)
                started = t.started if t.started is not None else s.taken
                running.append({**self._ticket_entry(t), "waited_s": round(max(0.0, started - t.created), 1),
                                "running_s": round(max(0.0, now - started), 1), "draining": s.draining,
                                "borrowing": t.borrowing, "steps_used": t.steps_used})
            waiting = [{**self._ticket_entry(r.binding), "position": position, "eta_s": eta,
                        "waited_s": round(max(0.0, now - r.binding.created), 1),
                        "starts_within_s": None if r.binding.deadline is None
                        else round(max(0.0, r.binding.deadline - now), 1)}
                       for r, position, eta in line if not r.binding.is_lease]
            return {"id": self.id, "name": self.settings.name or self.id, "capacity": self.capacity(),
                    "slots": self.settings.slots, "in_flight": len(self.slots),
                    "draining": sum(1 for s in self.slots if s.draining), "lease_requests": len(asking),
                    "leases": leases, "running": running, "waiting": waiting}

    @staticmethod
    def _ticket_entry(t: "Ticket") -> dict:
        return {"id": t.id, "class": t.ticket_class, "label": t.label, "model": t.model, "consumer": t.consumer,
                "ref": list(t.ref) if t.ref else None, "priority": t.queue_priority, "state": t.state}

    def busy_now(self) -> bool:
        """Whether anything holds, uses or waits for the machine: a lease, a slot, a request."""
        with self.lock:
            return bool(self.leases or self.slots or self.waiting)

    def set_priority(self, ref: tuple, value: int) -> int:
        """The work ``ref``'s queue priority changed (the Queue page): its tickets here follow it (non-blocking).
        Returns how many did."""
        with self.lock:
            value = max(-100, min(100, int(value or 0)))
            n = 0
            for t in self._tickets():
                if t.ref != ref or t.state not in ("queued", "running"):
                    continue
                t.queue_priority = value
                t.priority = BASE_PRIORITY[t.ticket_class] + value
                n += 1
            if n:
                self._dispatch(self.sched.clock())
            return n

    def find_ticket(self, ticket_id: str) -> Optional["Ticket"]:
        """A ticket queued or running here (a draining one's included), by its id."""
        with self.lock:
            return next((t for t in self._tickets() if t.id == ticket_id), None)

    def _tickets(self) -> list:
        """Every ticket made here and not over (queued, made but not yet asking, running, draining). Lock held."""
        out = {id(t): t for t in list(self.open_tickets) if t.state in ("queued", "running")}
        for b in [r.binding for r in self.waiting] + [s.binding for s in self.slots]:
            if not b.is_lease:
                out.setdefault(id(b), b)
        return list(out.values())


# ---------------------------------------------------------------------------- the scheduler

class Scheduler:
    """Every machine's queue, the listeners, and the notifier thread (P3.3.9)."""

    #: The class of its machines (a test's audited one checks the rules at every grant).
    machine_class = Machine
    #: Whether a listener settles the steps' budget holds (``add_listener``).
    _ledger_listens = False

    def __init__(self, *, clock: Callable[[], float] = time.perf_counter, wall: Callable[[], float] = time.time,
                 settings: Optional[Callable[[str], MachineSettings]] = None,
                 helper: Optional[Callable[[str], dict]] = None, drain_seconds: float = BENCHMARK_DRAIN_SECONDS,
                 background: bool = True):
        # perf_counter, not monotonic: both never go back, but on Windows before Python 3.13 monotonic ticks every
        # 15.6 ms, which metered a short step's generation and queue time as 0 and moved a step's edges by a tick
        self.clock = clock
        self.wall = wall
        self._read = settings or read_settings
        self._helper = helper or helper_state
        self.drain_seconds = float(drain_seconds)
        self.background = background
        self.machines: dict[str, Machine] = {}
        self._lock = threading.Lock()
        self._listeners: list = []
        self._notes: _queue.Queue = _queue.Queue()
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self._ticked = 0.0

    # -------------------------------------------------------------- machines
    def machine(self, server_id: Optional[str]) -> Optional[Machine]:
        """The machine for ``server_id``, made on first use (its settings read then, off any machine lock); None for no
        id. A machine with no row is kept too, unscheduled (its capacity is unknown)."""
        if not server_id:
            return None
        with self._lock:
            m = self.machines.get(server_id)
        if m is not None:
            return m
        settings = self._read_all(server_id)
        with self._lock:
            m = self.machines.get(server_id)
            if m is None:
                m = self.machines[server_id] = self.machine_class(self, server_id, settings)
        self._ensure_thread()
        return m

    def _read_all(self, server_id: str) -> MachineSettings:
        try:
            st = self._read(server_id)
        except Exception:
            log.warning("could not read machine %s for the scheduler", server_id, exc_info=True)
            st = MachineSettings(known=False)
        if st.known and st.reach == "worker":
            try:
                h = self._helper(server_id)
            except Exception:
                h = {"online": False}
            st.online, st.helper_slots = h.get("online"), h.get("helper_slots")
            st.helper_loaded = frozenset(h.get("helper_loaded") or ())
            st.contexts = dict(h.get("contexts") or {})
            st.connection = h.get("connection") if st.online else None
        return st

    def refresh(self, server_id: Optional[str] = None) -> None:
        """Read a machine's row again now (or every machine's): a change to its slots, swaps, quiet hours or state
        takes effect at once. Reads the database: never call it with a machine lock held."""
        with self._lock:
            targets = [m for sid, m in self.machines.items() if server_id is None or sid == server_id]
        for m in targets:
            try:
                st = self._read(m.id)
            except Exception:
                log.warning("could not read machine %s again", m.id, exc_info=True)
                continue
            m.apply(st)

    def helper_changed(self, server_id: str, state: Optional[dict] = None) -> None:
        """The hub's push (P3.3.2): a helper connected, disconnected or reported its models. Non-blocking, no
        database: safe from the event loop."""
        with self._lock:
            m = self.machines.get(server_id)
        if m is None:
            return
        if state is None:
            try:
                state = self._helper(server_id)
            except Exception:
                state = {"online": False}
        m.apply_helper(state)

    def refresh_loaded(self, m: Machine) -> None:
        """A direct LM Studio machine's loaded models, asked of it at most every ``SETTINGS_SECONDS`` (before a ticket
        queues, off every lock)."""
        st = m.settings
        if not st.known or st.reach != "direct" or st.provider != "lmstudio":
            return
        if m.loaded_at is not None and self.clock() - m.loaded_at < SETTINGS_SECONDS:
            return
        loaded = _lmstudio_loaded(m.id)
        with m.lock:
            m.settings.loaded = loaded
            m.loaded_at = self.clock()

    def models_changed(self, server_id: Optional[str]) -> None:
        """A direct machine's loaded models changed (CITAR loaded one on it): the next ticket asks it again rather than
        reading the cached list (non-blocking)."""
        with self._lock:
            m = self.machines.get(server_id or "")
        if m is not None:
            with m.lock:
                m.loaded_at = None

    # -------------------------------------------------------------- bindings
    def lease(self, server_id: Optional[str], model: str = "", *, kind: str = "game", activity: Optional[str] = None,
              consumer: Optional[str] = None, label: str = "", cap: int = 0,
              on_status: Optional[Callable[[dict], None]] = None, game: Optional[str] = None,
              ref: Optional[tuple] = None, budget: Optional[str] = None,
              on_budget: Optional[Callable[[dict], None]] = None) -> Lease:
        """A live lease on ``server_id`` (an unscheduled one when the machine has no id): a game seat's (``game``, the
        session's id), a probe run's or a report's; ``ref`` is the work it serves as the shared queue names it. A
        benchmark lease starts the machine's drain, and its ``on_status`` hears ``drained`` (with ``drain_s`` and
        ``contaminated``) when its first request goes ahead."""
        m = self.machine(server_id)
        lease = Lease(self, m, model, kind=kind, activity=activity, consumer=consumer, label=label, cap=cap,
                      on_status=on_status, game=game, ref=ref, budget=budget, on_budget=on_budget)
        if m is not None:
            for conv in m.add_lease(lease):
                try:
                    conv.close()
                except Exception:
                    log.debug("closing a cancelled ticket's conversation failed", exc_info=True)
        return lease

    def ticket(self, server_id: Optional[str], model: str = "", *, cls: str, activity: Optional[str] = None,
               consumer: Optional[str] = None, label: str = "", priority: int = 0,
               start_within: Optional[float] = None, steps: int = DEFAULT_STEPS, wait_for_benchmark: bool = False,
               cap: int = 0, on_status: Optional[Callable[[dict], None]] = None,
               one_per_consumer: bool = False, ref: Optional[tuple] = None,
               refusal: Optional[SchedulerError] = None, budget: Optional[str] = None,
               on_budget: Optional[Callable[[dict], None]] = None) -> Ticket:
        """A pooled prompt on ``server_id``. Refused at once (``state == "refused"`` with ``reason`` and ``retry_at``)
        when ``refusal`` is given (admission's, ``pool.seats.ticket``), when the machine cannot take it (rule 3
        (a)-(c), disabled, offline), or, with ``one_per_consumer``, when its consumer already has a ticket of its class
        queued or running there (``duplicate``: the Servers page's test, which a caller waits on, one at a time); else
        queued: its first ``step`` waits for its start, at most ``start_within`` seconds (then it expires). ``priority``
        is the queue priority of the work it is part of (``ref``), which it follows (``set_priority``)."""
        m = self.machine(server_id)
        if m is not None and refusal is None:
            self._fresh(m)
        deadline = self.clock() + float(start_within) if start_within is not None else None
        t = Ticket(self, m, model, cls=cls, priority=priority, deadline=deadline, steps=steps,
                   wait_for_benchmark=wait_for_benchmark, activity=activity, consumer=consumer, label=label,
                   cap=cap, on_status=on_status, ref=ref, budget=budget, on_budget=on_budget)
        if refusal is not None:
            t.state, t.error = _ended_state(refusal), refusal
            if m is not None:
                with m.lock:
                    m.counters[t.state] += 1
            return t
        if m is None:
            return t
        self.refresh_loaded(m)
        with m.lock:
            refusal = m._refuse_at_once(t, self.clock()) if m.settings.known else None
            if refusal is None and one_per_consumer and consumer is not None and any(
                    o.consumer == consumer and o.ticket_class == cls and o.state in ("queued", "running")
                    for o in list(m.open_tickets)):
                refusal = MachineRefused(f"A {cls} of {m.settings.name or m.id} you started is still waiting or "
                                         "running.", code="duplicate")
            if refusal is not None:
                t.state, t.error = _ended_state(refusal), refusal
                m.counters[t.state] += 1
            else:
                m.open_tickets.add(t)
        return t

    def leases(self, server_id: Optional[str]) -> list[Lease]:
        """The leases live on a machine, oldest first: what ``pool.seats.occupied`` lists, and what a benchmark job
        waits for (P3.3.5)."""
        with self._lock:
            m = self.machines.get(server_id)
        if m is None:
            return []
        with m.lock:
            return list(m.leases)

    def late_frame(self, server_id: str, request_id: str) -> bool:
        """The hub's hand-off of a frame for a request it had already resolved (P3.3.3, draining). Non-blocking."""
        with self._lock:
            m = self.machines.get(server_id)
        return m.late_frame(request_id) if m is not None else False

    def status(self) -> list[dict]:
        """Every machine, for an administrator's status listing."""
        with self._lock:
            machines = list(self.machines.values())
        return [m.status() for m in machines]

    def views(self, busy_only: bool = True) -> dict:
        """Every machine as the Queue page lists it (``Machine.view``), by id; with ``busy_only``, only those something
        holds, uses or waits for."""
        with self._lock:
            machines = list(self.machines.values())
        return {m.id: m.view() for m in machines if not busy_only or m.busy_now()}

    def find_ticket(self, ticket_id: str) -> Optional[Ticket]:
        """A ticket queued or running on any machine, by its id (the Queue page's cancel), or None."""
        with self._lock:
            machines = list(self.machines.values())
        for m in machines:
            t = m.find_ticket(ticket_id)
            if t is not None:
                return t
        return None

    def set_priority(self, ref: tuple, value: int) -> int:
        """The work ``ref`` (``(kind, id)``, as the shared queue names it) has a new queue priority: its tickets on
        every machine follow it (P3.3.3: a ticket's priority is its class's base plus its activity's). Non-blocking.
        Returns how many tickets changed."""
        with self._lock:
            machines = list(self.machines.values())
        return sum(m.set_priority(tuple(ref), value) for m in machines)

    # -------------------------------------------------------------- listeners
    def add_listener(self, fn: Callable[[StepEvent], None]) -> None:
        """Receive a ``StepEvent`` after every step of a scheduled conversation (P3.3.4). A listener that settles the
        steps' budget holds says so (``settles_budgets``: the compute ledger's); without one, a step's hold is given
        back as soon as the step is metered."""
        with self._lock:
            if fn not in self._listeners:
                self._listeners.append(fn)
            if getattr(fn, "settles_budgets", False):
                self._ledger_listens = True

    def remove_listener(self, fn) -> None:
        with self._lock:
            if fn in self._listeners:
                self._listeners.remove(fn)

    def emit(self, event: StepEvent) -> None:
        """Hand a step to every listener, on the caller's thread (no lock held); a failing listener is logged."""
        with self._lock:
            listeners = list(self._listeners)
        for fn in listeners:
            try:
                fn(event)
            except Exception:
                log.exception("a step listener failed")

    # -------------------------------------------------------------- the notifier thread
    def _notify(self, fn: Callable, payload: Any) -> None:
        """Queue a callback (may be called under a machine lock): the notifier thread delivers it after."""
        self._notes.put((fn, payload))
        if self.background:
            self._ensure_thread()

    def deliver(self) -> int:
        """Deliver every queued callback now, on the calling thread (tests without the background thread)."""
        n = 0
        while True:
            try:
                fn, payload = self._notes.get_nowait()
            except _queue.Empty:
                return n
            self._call(fn, payload)
            n += 1

    @staticmethod
    def _call(fn, payload) -> None:
        try:
            fn(payload)
        except Exception:
            log.exception("a scheduler callback failed")

    def tick(self) -> None:
        """The periodic housekeeping (the notifier thread's, every second): due expiries on every machine. It reads
        nothing: a row is read again by the next request once ``SETTINGS_SECONDS`` have passed (``_fresh``), and a
        helper's state is pushed by the hub."""
        now = self.clock()
        with self._lock:
            machines = list(self.machines.values())
        for m in machines:
            with m.lock:
                m._housekeep(now)
                m._dispatch(now)

    def _fresh(self, m: "Machine") -> None:
        """Read a machine's row again when it was read more than ``SETTINGS_SECONDS`` ago (P3.3.2), off every lock:
        the registry's own cache answers, so the database is asked at most that often."""
        if self.clock() - m.read_at < SETTINGS_SECONDS:
            return
        m.read_at = self.clock()
        try:
            m.apply(self._read(m.id))
        except Exception:
            log.warning("could not read machine %s again", m.id, exc_info=True)

    def _ensure_thread(self) -> None:
        """Start the notifier thread, or start it again if it died (P3.9.2)."""
        if not self.background or self._stop.is_set():
            return
        t = self._thread
        if t is not None and t.is_alive():
            return
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            if self._thread is not None:
                log.warning("the scheduler's notifier thread had stopped; starting it again")
            self._thread = threading.Thread(target=self._run, daemon=True, name="scheduler-notifier")
            self._thread.start()

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                try:
                    fn, payload = self._notes.get(timeout=TICK_SECONDS)
                    self._call(fn, payload)
                except _queue.Empty:
                    pass
                if self.clock() - self._ticked >= TICK_SECONDS:
                    self._ticked = self.clock()
                    self.tick()
            except Exception:
                log.exception("the scheduler's notifier loop failed; it carries on")

    def stop(self) -> None:
        """Stop the notifier thread (tests)."""
        self._stop.set()

    def run_db_job(self, fn: Callable[[], None]) -> None:
        """Run a database write off the request path, on the notifier thread (the over-report flag)."""
        self._notify(lambda _payload: fn(), None)


# ---------------------------------------------------------------------------- the conversation wrapper

def _is_busy(e: BaseException) -> bool:
    return getattr(e, "refusal", "") == "busy"


class ScheduledConversation(Conversation):
    """A provider's conversation whose every step takes its binding's slot first (P3.3.4). The request timeout is set
    only once the slot is granted (``budget``, from the caller's remaining time, which the queue does not shorten), and
    each step's generation and queue time are kept (``last_gen_s``, ``last_queue_s``) and handed to the listeners."""

    def __init__(self, inner: Conversation, binding: Binding, cfg: Optional[dict] = None):
        self.inner = inner
        self.binding = binding
        self.cfg = dict(cfg or {})
        #: ``budget(queued_seconds)`` -> the request's timeout once granted (None: the provider's), or raises.
        self.budget: Optional[Callable[[float], Optional[float]]] = None
        #: ``halted()`` -> whether the caller has stopped (a stopped session): a waiting request then gives up.
        self.halted: Optional[Callable[[], bool]] = None
        #: ``on_wait(place or None)``: told on the stepping thread, with no lock held, when a step has to wait for its
        #: machine (its ``position``, ``eta_s`` and the ``machine``'s name) and when that wait is over (None): the
        #: agent shows its seat as waiting for the machine (``Machine.acquire``).
        self.on_wait: Optional[Callable[[Optional[dict]], None]] = None
        self.last_queue_s = 0.0
        self.last_gen_s = 0.0
        self.last_event: Optional[StepEvent] = None
        self._closed = False
        self._last_input = 0                # the last step's input tokens: the next one's budget estimate
        if isinstance(binding, Ticket):
            binding.convs.add(self)
            if isinstance(getattr(inner, "cfg", None), dict):
                inner.cfg = dict(inner.cfg)        # a borrowing step caps its max_tokens there, never the caller's

    # the provider's conversation, as the agent reads it
    @property
    def usage(self) -> dict:
        return self.inner.usage

    @property
    def request_timeout(self):
        return getattr(self.inner, "request_timeout", None)

    @request_timeout.setter
    def request_timeout(self, value):
        self.inner.request_timeout = value

    def __getattr__(self, name):
        # anything else of the provider's (``context_info``, ``served_model``, ``messages``) reads through
        if name == "inner":
            raise AttributeError(name)
        return getattr(self.inner, name)

    def add_user_text(self, text: str):
        self.inner.add_user_text(text)

    def add_tool_results(self, results):
        self.inner.add_tool_results(results)

    def close(self):
        """Cancel (non-blocking, allowed under a session lock): a waiting request leaves the queue; a running one is
        abandoned by its provider (a helper's ``CANCEL``; its slot drains while the helper finishes)."""
        self._closed = True
        m = self.binding.machine
        if m is not None:
            with m.lock:
                m.cond.notify_all()
        try:
            self.inner.close()
        except Exception:
            log.debug("closing a provider's conversation failed", exc_info=True)
        if isinstance(self.binding, Ticket):
            self.binding.convs.discard(self)

    def step(self) -> StepResult:
        """Take the slot, run the provider's step, give the slot back (or keep it draining), meter the step."""
        if self._closed:
            raise Cancelled("The conversation was closed.", code="cancelled")
        b = self.binding
        sched = b.sched
        inner = self.inner
        queued = 0.0
        self.last_queue_s = self.last_gen_s = 0.0
        grant = b.acquire(self, self.halted, self.on_wait)
        hold = None
        while True:
            queued += grant.queue_s
            t0: Optional[float] = None
            if hold is None and getattr(b, "budget", None):
                hold = self._reserve(grant)
            try:
                if self._closed:
                    raise Cancelled("The conversation was closed.", code="cancelled")
                if self.budget is not None:
                    timeout = self.budget(queued)
                    if timeout is not None:
                        inner.request_timeout = timeout
                restore = self._cap_tokens(grant.max_tokens)
                if hasattr(inner, "next_request_id"):
                    inner.next_request_id = grant.request_id
                before = dict(getattr(inner, "usage", {}) or {})
                t0 = sched.clock()
                try:
                    result = inner.step()
                finally:
                    if restore is not None:
                        restore()
                wall = sched.clock() - t0
            except BaseException as e:
                if t0 is not None and _is_busy(e) and not self._closed and grant.request is not None:
                    # the helper had no slot after all: no sleep, nothing metered; wait like any other request
                    queued += sched.clock() - t0
                    b.busy(grant)
                    grant = b.wait(grant.request, self, self.halted)
                    continue
                # a request the provider abandoned (cancelled, timed out) may still be generating on its helper
                b.finish(grant, unfinished=getattr(inner, "unfinished", None) if t0 is not None else None)
                self.last_queue_s = queued
                _release(hold)
                raise
            gen_s, reported, observed = self._gen(inner, wall)
            b.finish(grant, gen_s=gen_s)
            self.last_queue_s, self.last_gen_s = queued, gen_s
            self._meter(grant, before, gen_s, reported, observed, queued, hold)
            return result

    def _reserve(self, grant: Grant):
        """Hold the step's estimate of its binding's budget, now that its slot is granted (P3.4.6). A hard budget that
        would pass its amount refuses the request: its slot goes back and ``MachineRefused`` (``budget``, the
        period's end as ``retry_at``) is raised. A soft budget's warning goes to ``on_budget``. In memory only."""
        from .. import ledger
        from . import budgets
        b = self.binding
        cfg = getattr(self.inner, "cfg", None) or self.cfg or {}
        max_tokens = grant.max_tokens or (cfg.get("max_tokens") if isinstance(cfg, dict) else None)
        estimate, unpriced = ledger.get().estimate(b.server_id, b.model or (cfg or {}).get("model"),
                                                   max_tokens=max_tokens, input_tokens=self._input_estimate())
        try:
            hold = budgets.keeper().reserve(b.budget, b.activity, b.consumer, estimate, unpriced=unpriced)
        except budgets.BudgetExceeded as e:
            b.finish(grant)
            retry = e.retry_at.astimezone().replace(tzinfo=None) if e.retry_at is not None else None
            err = MachineRefused(str(e), code="budget", retry_at=retry)
            err.budget = {"id": e.budget_id, "key": e.key}
            raise err from None
        if hold is not None and hold.warning and b.on_budget is not None:
            b.sched._notify(b.on_budget, {"warning": hold.warning, "budget": b.budget})
        if hold is not None and hold.warning:
            b.sched.run_db_job(lambda bid=b.budget, who=b.consumer, text=hold.warning: _soft_passed(bid, who, text))
        return hold

    def _input_estimate(self) -> int:
        """The next request's input tokens, for an online API's estimate: the last step's, else the conversation's
        text at about four characters a token."""
        if self._last_input:
            return self._last_input
        try:
            import json as _json
            return len(_json.dumps(getattr(self.inner, "messages", []) or [], default=str)) // 4
        except Exception:
            return 0

    def _cap_tokens(self, cap: Optional[int]):
        """A borrowing ticket's step asks for at most its class's ``max_tokens``; returns how to undo it."""
        if cap is None:
            return None
        cfg = getattr(self.inner, "cfg", None)
        if not isinstance(cfg, dict):
            return None
        old = cfg.get("max_tokens")
        cfg["max_tokens"] = min(int(old), cap) if old else cap

        def restore():
            if old is None:
                cfg.pop("max_tokens", None)
            else:
                cfg["max_tokens"] = old
        return restore

    @staticmethod
    def _gen(inner, wall: float) -> tuple:
        """(generation seconds, reported, observed): bounded by what the server saw (P3.3.4)."""
        observed = getattr(inner, "last_observed_s", None)
        if observed is None:
            return wall, None, None
        reported = getattr(inner, "last_reported_s", None)
        if reported is None or reported <= 0:
            return observed, reported, observed
        return min(reported, observed), reported, observed

    def _meter(self, grant: Grant, before: dict, gen_s: float, reported, observed, queued: float,
               hold=None) -> None:
        b = self.binding
        sched = b.sched
        after = dict(getattr(self.inner, "usage", {}) or {})
        usage = {k: max(0, int(after.get(k, 0) or 0) - int(before.get(k, 0) or 0)) for k in after}
        flags: dict = {}
        cfg = getattr(self.inner, "cfg", None) or self.cfg
        limit = (cfg or {}).get("max_tokens") if isinstance(cfg, dict) else None
        if grant.max_tokens is not None:
            limit = min(int(limit), grant.max_tokens) if limit else grant.max_tokens
        if limit:
            for k in ("output_tokens", "reasoning_tokens"):
                if usage.get(k, 0) > int(limit):
                    usage[k] = int(limit)
                    flags["capped_tokens"] = True
        m = b.machine
        model = getattr(self.inner, "served_model", None) or b.model or (cfg or {}).get("model")
        context = m.settings.contexts.get(model) if m is not None else None
        if context and usage.get("input_tokens", 0) > int(context):
            usage["input_tokens"] = int(context)
            flags["capped_tokens"] = True
        if reported is not None and observed is not None and reported > observed * OVERREPORT_RATIO + OVERREPORT_SLACK:
            flags["overreported"] = True
            if m is not None:
                count = m.note_overreport(sched.wall())
                if count is not None:
                    sched.run_db_job(lambda sid=m.id, n=count: flag_overreports(sid, n))
        self._last_input = int(usage.get("input_tokens", 0) or 0) + int(usage.get("cache_read_input_tokens", 0) or 0)
        event = StepEvent(machine=b.server_id, model=model, activity=b.activity, consumer=b.consumer,
                          task_class=b.task_class, cls=b.cls, gen_s=round(gen_s, 3),
                          reported_s=None if reported is None else round(reported, 3),
                          observed_s=None if observed is None else round(observed, 3), queue_s=round(queued, 3),
                          usage=usage, request_id=grant.request_id, at=sched.wall(),
                          provider=str((cfg or {}).get("provider") or ""), flags=flags, reservation=hold)
        self.last_event = event
        sched.emit(event)
        if hold is not None and not getattr(sched, "_ledger_listens", False):
            _release(hold)                 # a scheduler with no ledger listening (a test's): nothing will settle it


def _release(hold) -> None:
    """Give a step's budget hold back (it never ran, or nothing will settle it)."""
    if hold is not None:
        from . import budgets
        budgets.keeper().release(hold)


def _soft_passed(budget_id: Optional[str], consumer: Optional[str], text: str) -> None:
    """One audit row when a request takes a soft budget past its amount (P3.4.6; once a period): the machine owner's
    Servers page shows the meter over it. Runs on the notifier thread."""
    try:
        from .. import db
        from ..auth import audit
        if not db.configured() or not budget_id:
            return
        with db.session() as s:
            audit.record(s, "budget.soft_passed", object_type="budget", object_id=budget_id, consumer=consumer,
                         text=text[:300])
    except Exception:
        log.exception("could not record a soft budget passed")


def flag_overreports(server_id: str, count: int) -> None:
    """Put ``flags.overreports`` on a machine's row and an audit row (P3.3.4): its helper reported more generation
    time than the server observed, three times within an hour. Runs on the notifier thread."""
    from datetime import timezone
    try:
        from .. import db
        from ..auth import audit
        from ..db.models import Server
        if not db.configured():
            return
        with db.session() as s:
            row = s.get(Server, server_id)
            if row is None:
                return
            flags = dict(row.flags or {})
            first = "overreports" not in flags
            flags["overreports"] = {"count": int(count), "last": datetime.now(timezone.utc).isoformat()}
            row.flags = flags
            if first:
                audit.record(s, "server.overreports", object_type="server", object_id=server_id, count=int(count))
    except Exception:
        log.exception("could not flag machine %s for over-reporting", server_id)


# ---------------------------------------------------------------------------- the process's scheduler

_SCHED: Optional[Scheduler] = None
_SCHED_LOCK = threading.Lock()


def get() -> Scheduler:
    """The process's scheduler, made on first use with the compute ledger as its listener (``citar.ledger``), which
    settles each step's budget hold."""
    global _SCHED
    with _SCHED_LOCK:
        if _SCHED is None:
            from .. import ledger
            _SCHED = Scheduler()
            _SCHED.add_listener(ledger.listener)
        return _SCHED


def current() -> Optional[Scheduler]:
    """The process's scheduler if one was made, else None (the hub's pushes need none)."""
    return _SCHED


def reset() -> None:
    """Forget the process's scheduler (tests)."""
    global _SCHED
    with _SCHED_LOCK:
        if _SCHED is not None:
            _SCHED.stop()
        _SCHED = None

