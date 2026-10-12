"""The compute ledger: who gave and who used compute, in credits (design/phase3-features.md P3.4).

Every model step of a scheduled machine and every second of the host's CPU spent on somebody's work becomes one row
of ``compute_events``: the machine, the activity it served, its **consumer** (the activity's owner) and its
**provider** (the machine's owner), the generation seconds (bounded by what the server observed, P3.3.4) or CPU
core-seconds, the tokens, and the **credits** they are worth, priced when written from the machine's hardware class.

- **Consumer** (``owner_of``): the owner of the activity's row (a game's ``games.owner_id``; a probe run's, a report's,
  a lab experiment's or a benchmark run's owned item), null when that item is marked ``collection`` (the server's own
  data gathering, which only administrators mark). An activity with no row (a machine's test, or a game of a process
  with no database rows) takes the consumer the scheduler named.
- **Provider**: the machine's ``owner_id``, null when the machine is ``server_resource`` (the host, the online APIs
  the site provides). ``self_use``: consumer and provider the same member, or both the server; counted on neither side.
  **Given(u)** are the events with ``provider_id = u``, **used(u)** those with ``consumer_id = u``, self-use left out.
- **Credits** (``credits_for``): ``gen_s`` x the class rate on a GPU; ``cpu_core_s`` x the rate of the host's CPU
  class; on an online API (a machine of kind ``api``) the step's token cost at the model's prices in force
  (``costing.price_tokens``) x the API class rate, 0 and flagged ``unpriced`` when the model has no price. A machine's
  class of a kind (``class_of``) is the first class of that kind, by priority, whose ``match`` fits its hardware
  (``gpu_name`` and ``cpu_name`` regular expressions, ``vram_gb_min``, ``cores_min``), else the kind's default; an
  administrator may pin one (``class_override``). A new rate applies from then on; ``reprice`` recomputes a period.

**Writers.** The prompt scheduler's listener (``listener``, every model step: ``gpu`` or ``api``), the usage
tracker's flush (``cpu``: the host's CPU measured per unit of work, ``cpu_unit``, one event per activity a minute,
``observed_s`` the window it was measured over), and the lab's results (``ingest_lab``: one ``cpu`` event per lab game,
``observed_s`` the game's wall time). The listener prices each step and settles its budget hold in memory at once
(``pool.budgets.Keeper.settle``); events go through a queue to **one writer thread**, the only one in the process that
inserts them (so their ids commit in order, which the rollups' watermark relies on), in batches (every
``BATCH_SECONDS`` or ``BATCH_EVENTS``), never on a game's lock, writing each settled step's budget entry in the same
transaction (``pool.budgets.settle_entries``). A batch the database is too busy for is tried again (``WRITE_TRIES``).
It drains on shutdown (``stop``).

**Rollups** (P3.4.5): ``compute_rollups`` holds an hour of one member's given or used compute of one kind. A rollup
thread recomputes, every ``ROLLUP_SECONDS``, each hour an event landed in since its last pass (the events past its
watermark, ``settings_kv.ledger_rollups``), deleting and inserting the hour's rows in one transaction, at most
``ROLLUP_CHUNK`` hours a transaction (the watermark moves after the last). Totals (``totals``) read the rollups for
complete hours and the raw events for the rest (the current hour, the range's first partial hour, and any hour not
rolled up yet), so a total never waits for the pass and always equals the raw sums. The same pass reads the lab's new
results and the budgets' grants again (``pool.budgets.Keeper.sync``).

The ratio is given credits over used credits (``ratio``); ``/api/ledger/me``, ``/api/users/{handle}/ratio`` and the
administrators' pages read it (``citar/server/ledger_api.py``).

Replaces: the usage ledger's ``busy``, tokens and requests in its spans (``citar/usage.py``), which costing now reads
from here (``work``), and ``pool.budgets``' reservations against an hourly cost estimate.
"""
from __future__ import annotations

import json
import logging
import queue as _queue
import re
import threading
import time
from collections import defaultdict
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from typing import Callable, Iterable, Optional

log = logging.getLogger("citar.ledger")

#: The writer inserts what it has every BATCH_SECONDS, or as soon as it has BATCH_EVENTS.
BATCH_EVENTS = 200
BATCH_SECONDS = 2.0
#: How often the rollup thread recomputes the hours touched since its last pass (and reads the lab's results).
ROLLUP_SECONDS = 300.0
#: How long the hardware classes and the activities' owners are kept before they are read again.
CLASS_SECONDS = 5.0
OWNER_SECONDS = 60.0
#: The ranges a total is asked for.
RANGES = {"24h": 86400.0, "7d": 7 * 86400.0, "30d": 30 * 86400.0, "all": None}
#: The machine a CPU event names when no machine is the host yet.
HOST_UNKNOWN = "host"
#: Where the rollups' watermark (the last event id they include) is kept.
WATERMARK_KEY = "ledger_rollups"
SIDES = ("given", "used")
KINDS = ("gpu", "cpu", "api")
#: The least a request reserves of a budget (``pool.budgets.MIN_RESERVE``), and how the mean credits per step move.
EWMA_ALPHA = 0.3
#: An online API request with no ``max_tokens`` is estimated at this much output.
DEFAULT_MAX_TOKENS = 4096
HOUR = timedelta(hours=1)
#: Past this many hours not rolled up, a total reads the raw events alone (``totals``).
MAX_DIRTY_HOURS = 200
#: The most hours one rollup transaction recomputes, and the most events one ``reprice`` transaction reprices: the
#: database's write lock is never held for long, so the writer's batches and the requests' writes go on meanwhile.
ROLLUP_CHUNK = 48
REPRICE_PAGE = 5000
#: How many times the writer tries a batch the database is too busy for (another transaction holds its write lock past
#: its busy timeout), and the wait before the next try (doubling).
WRITE_TRIES = 4
WRITE_BACKOFF = 0.5
#: How long ``ingest_lab`` waits for the writer to write the lab's events it queued.
LAB_WRITE_SECONDS = 60.0


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def hour_of(when: datetime) -> datetime:
    """The UTC hour an instant falls in."""
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return when.astimezone(timezone.utc).replace(minute=0, second=0, microsecond=0)


# ---------------------------------------------------------------------------- what an activity is

#: An activity's kind by its id's prefix (``game:<id>``, ``probe:<id>``, ...), as the usage ledger names them.
PREFIX_KINDS = {"game": "game", "benchmark_run": "benchmark", "probe": "probe", "probe_run": "probe",
                "report": "report", "lab": "lab", "test": "test"}
#: A model step's activity kind by its binding's class, where the class says more than the prefix: a benchmark game's
#: seat plays under ``game:<id>`` with a ``benchmark`` lease, a probe's turn case under ``probe:<id>``.
CLASS_KINDS = {"benchmark": "benchmark", "probe_case": "probe", "probe": "probe", "report": "report", "test": "test"}
#: A ``games`` row's kind as an activity kind.
GAME_KINDS = {"benchmark": "benchmark", "probe": "probe"}


def activity_kind(activity: Optional[str], cls: Optional[str] = None, game_kind: Optional[str] = None) -> str:
    """``game``, ``benchmark``, ``probe``, ``report``, ``lab`` or ``test`` for an activity id (and the class of the
    binding a step ran under, and a game row's kind, when known)."""
    if cls in CLASS_KINDS:
        return CLASS_KINDS[cls]
    prefix = (activity or "").split(":", 1)[0]
    kind = PREFIX_KINDS.get(prefix, "game")
    if kind == "game" and game_kind in GAME_KINDS:
        return GAME_KINDS[game_kind]
    return kind


def purpose_of(task_class: Optional[str], kind: Optional[str]) -> str:
    """The grant purpose (``admission.PURPOSES``) an event's work was admitted for: a pooled prompt's ``pooled``, else
    its activity's (``benchmark``, ``probe``, ``lab``), else ``game``."""
    if task_class == "partial":
        return "pooled"
    return {"benchmark": "benchmark", "probe": "probe", "lab": "lab"}.get(kind or "", "game")


# ---------------------------------------------------------------------------- hardware classes

def _regex(pattern: str, text: str) -> bool:
    try:
        return re.search(pattern, text or "", re.IGNORECASE) is not None
    except re.error:
        return False


def matches(match: Optional[dict], hardware: Optional[dict]) -> bool:
    """Whether a class's ``match`` fits a machine's hardware (``config.hardware``: ``gpus`` with ``name`` and
    ``vram_gb``, ``cpu`` with ``model``, ``cores`` and ``threads``). An empty match fits every machine; each key given
    must hold: ``gpu_name`` (a regular expression any GPU's name matches), ``vram_gb_min`` (the largest GPU's), ``cpu_name``
    (on the CPU's model) and ``cores_min`` (its cores, else its threads)."""
    match = match or {}
    hw = hardware or {}
    gpus = [g for g in hw.get("gpus") or [] if isinstance(g, dict)]
    cpu = hw.get("cpu") if isinstance(hw.get("cpu"), dict) else {}
    if match.get("gpu_name") and not any(_regex(str(match["gpu_name"]), str(g.get("name") or "")) for g in gpus):
        return False
    if match.get("vram_gb_min") is not None:
        vram = max((float(g.get("vram_gb") or 0) for g in gpus), default=0.0)
        if vram < float(match["vram_gb_min"]):
            return False
    if match.get("cpu_name") and not _regex(str(match["cpu_name"]), str(cpu.get("model") or cpu.get("name") or "")):
        return False
    if match.get("cores_min") is not None:
        cores = cpu.get("cores") or cpu.get("threads") or 0
        try:
            if float(cores) < float(match["cores_min"]):
                return False
        except (TypeError, ValueError):
            return False
    return True


def classify(hardware: Optional[dict], classes: list, kind: str) -> Optional[dict]:
    """A machine's class of ``kind`` (``gpu``, ``cpu`` or ``api``) for its hardware: the first class of that kind, by
    priority (highest first), whose ``match`` fits, else the kind's default (None when there is none)."""
    mine = [c for c in classes if c["kind"] == kind]
    for c in sorted((c for c in mine if not c["is_default"]), key=lambda c: (-int(c["priority"] or 0), c["name"], c["id"])):
        if matches(c["match"], hardware):
            return c
    return next((c for c in mine if c["is_default"]), None)


def class_of(facts: Optional[dict], kind: str, classes: list) -> Optional[dict]:
    """The class a machine (``servers.machine_facts``) is priced by for ``kind``: the one an administrator pinned
    (``class_override``: ``gpu_class_id`` for a GPU, or an online API's own class, ``cpu_class_id`` for a CPU), else
    ``classify`` on its hardware, followed afresh whenever the hardware changes."""
    facts = facts or {}
    if facts.get("class_override"):
        pinned = facts.get("cpu_class_id") if kind == "cpu" else facts.get("gpu_class_id")
        hit = next((c for c in classes if c["id"] == pinned and c["kind"] == kind), None)
        if hit is not None:
            return hit
    return classify(facts.get("hardware"), classes, kind)


def class_dict(row) -> dict:
    """A ``HardwareClass`` row as the ledger keeps it."""
    return {"id": row.id, "kind": row.kind, "name": row.name, "rate": float(row.rate or 0.0),
            "match": dict(row.match or {}), "priority": int(row.priority or 0), "is_default": bool(row.is_default)}


def reclassify(session, rows=None) -> int:
    """Put each machine's computed classes on its row (``gpu_class_id``, ``cpu_class_id``) unless an administrator
    pinned them: for the administrators' pages, which show them. Pricing computes the class afresh (``class_of``), so
    this is a record, kept up to date when the hardware or the classes change. Returns the rows changed."""
    from sqlalchemy import select
    from .db.models import HardwareClass, Server
    classes = [class_dict(c) for c in session.scalars(select(HardwareClass))]
    changed = 0
    for row in (rows if rows is not None else session.scalars(select(Server))):
        if row.class_override:
            continue
        hw = (row.config or {}).get("hardware") or {}
        if row.kind == "api":
            gpu, cpu = classify(hw, classes, "api"), None
        else:
            gpu, cpu = classify(hw, classes, "gpu"), classify(hw, classes, "cpu")
        ids = (gpu["id"] if gpu else None, cpu["id"] if cpu else None)
        if (row.gpu_class_id, row.cpu_class_id) != ids:
            row.gpu_class_id, row.cpu_class_id = ids
            changed += 1
    return changed


def _hardware_changed(session, _context, _instances) -> None:
    """A machine whose hardware (or kind) changes is classified again before it is written, unless an administrator
    pinned its classes (P3.4.3: "recomputed when the hardware changes"): a helper's hello, the hardware dialog, a
    scan."""
    from sqlalchemy import inspect as _inspect
    from .db.models import Server
    rows = []
    for obj in list(session.new) + list(session.dirty):
        if not isinstance(obj, Server) or obj.class_override:
            continue
        state = _inspect(obj)
        if obj in session.new or state.attrs.config.history.has_changes() or state.attrs.kind.history.has_changes():
            rows.append(obj)
    if rows:
        try:
            with session.no_autoflush:
                reclassify(session, rows)
        except Exception:
            # a database without the classes yet (a test's, made before its tables): the machine is saved all the
            # same, and priced by ``class_of`` from its hardware whatever its row says
            log.debug("could not classify machine(s) %s", [r.id for r in rows], exc_info=True)


def _listen() -> None:
    from sqlalchemy import event
    from sqlalchemy.orm import Session as _OrmSession
    if not event.contains(_OrmSession, "before_flush", _hardware_changed):
        event.listen(_OrmSession, "before_flush", _hardware_changed)


_listen()


# ---------------------------------------------------------------------------- credits

def token_cost(price: Optional[dict], usage: dict) -> Optional[float]:
    """What a step's tokens cost at a model's prices (the registry's currency), or None for no price."""
    if not price:
        return None
    from .costing import price_tokens
    return price_tokens(price, {"in": usage.get("tokens_in", 0), "out": usage.get("tokens_out", 0),
                                "cr": usage.get("tokens_cache_read", 0), "cw": usage.get("tokens_cache_write", 0)})


def credits_for(kind: str, *, gen_s: float = 0.0, cpu_core_s: float = 0.0, cost: Optional[float] = None,
                rate: float = 0.0) -> float:
    """An event's credits (P3.4.3): generation seconds, core-seconds or token cost, times the class rate."""
    if kind == "api":
        return max(0.0, float(cost or 0.0)) * rate
    if kind == "cpu":
        return max(0.0, float(cpu_core_s or 0.0)) * rate
    return max(0.0, float(gen_s or 0.0)) * rate


def _api_price(machine_id: str, model: Optional[str], when: datetime) -> Optional[dict]:
    """A model's token prices on an online API machine at ``when``, or None."""
    from . import servers
    sv = servers.find(machine_id)
    if sv is None or not model:
        return None
    try:
        return servers.model_price(sv, model, when.astimezone().replace(tzinfo=None))
    except Exception:
        return None


# ---------------------------------------------------------------------------- the ledger

class Ledger:
    """The queue, the writer thread and the rollup thread (P3.9.2: both survive an exception in their loop and are
    started again if they die). ``clock`` gives the wall time (a CPU event's ``at``)."""

    def __init__(self, *, clock: Callable[[], float] = time.time, background: bool = True):
        self.clock = clock
        self.background = background
        self.q: _queue.Queue = _queue.Queue()
        self._lock = threading.Lock()
        self._idle = threading.Condition(self._lock)
        self._pending = 0
        self._hurry = threading.Event()
        self._stop = threading.Event()
        self._writer: Optional[threading.Thread] = None
        self._roller: Optional[threading.Thread] = None
        self._owners: dict = {}               # activity -> (consumer, game kind, collection, found, read at)
        self._classes: Optional[list] = None
        self._classes_at = 0.0
        self._means: dict = {}                # (machine, model) -> mean credits per step
        self._lab_seen: dict = {}             # results file -> (mtime, size) when last read
        self._failed_at = 0.0
        self.written = 0
        self.failures = 0
        #: Called with the rows of every batch committed (tests, the QA's evidence); on the writer thread.
        self.listeners: list = []

    # -------------------------------------------------------------- writers (any thread, never blocking)
    def step(self, event) -> None:
        """A model step of a scheduled machine (the scheduler's ``StepEvent``; ``reservation`` the budget's hold, if
        any). One that ran on no machine row (an inline block's) is the metrics', not the ledger's.

        The step is priced here, on the caller's thread, from what the process has cached (the machine's row, the
        classes, the model's prices), and its budget hold settled in memory at once (P3.4.6: the listener settles):
        the next request of the same budget is checked against what this one cost, not against a reservation the
        writer has yet to settle. The writer then writes the event and the budget's entry."""
        from .pool import budgets
        hold = getattr(event, "reservation", None)
        if not getattr(event, "machine", None):
            budgets.keeper().release(hold)
            return
        try:
            priced = self.price(event)
        except Exception:
            log.warning("could not price a model step on %s; it is written unpriced", event.machine, exc_info=True)
            priced = {"kind": "gpu", "credits": 0.0, "flags": {"unpriced": True}}
        if hold is not None:
            budgets.keeper().settle(hold, priced["credits"])
        if priced["kind"] != "cpu" and event.model:
            key = (event.machine, event.model)
            old = self._means.get(key)
            self._means[key] = priced["credits"] if old is None else old + EWMA_ALPHA * (priced["credits"] - old)
        self._put(("step", event, priced))

    def price(self, ev) -> dict:
        """A model step's kind (``gpu`` or ``api``), credits and flags (P3.4.3), from the cached machine, classes and
        prices."""
        from . import servers
        facts = servers.machine_facts(ev.machine) or {}
        classes = self.classes()
        flags = {k: True for k in ("overreported", "capped_tokens") if (ev.flags or {}).get(k)}
        if facts.get("kind") == "api":
            u = ev.usage or {}
            cls = class_of(facts, "api", classes)
            cost = token_cost(_api_price(ev.machine, ev.model, datetime.fromtimestamp(ev.at, timezone.utc)), {
                "tokens_in": int(u.get("input_tokens", 0) or 0), "tokens_out": int(u.get("output_tokens", 0) or 0),
                "tokens_cache_read": int(u.get("cache_read_input_tokens", 0) or 0),
                "tokens_cache_write": int(u.get("cache_creation_input_tokens", 0) or 0)})
            if cost is None:
                flags["unpriced"] = True
            return {"kind": "api", "credits": round(credits_for("api", cost=cost, rate=cls["rate"] if cls else 0.0), 6),
                    "flags": flags}
        cls = class_of(facts, "gpu", classes)
        return {"kind": "gpu", "credits": round(credits_for("gpu", gen_s=ev.gen_s, rate=cls["rate"] if cls else 0.0),
                                                6), "flags": flags}

    def cpu(self, activity: Optional[str], seconds: float, *, at: Optional[float] = None,
            machine: Optional[str] = None, request_id: Optional[str] = None, window: Optional[float] = None) -> None:
        """``seconds`` of the host's CPU spent on ``activity`` (the usage tracker's flush, one a minute per activity),
        measured over the ``window`` seconds that end ``at`` (its ``observed_s``: costing spreads the seconds over it,
        a minute when not given)."""
        if not activity or not seconds or seconds <= 0:
            return
        self._put(("cpu", {"activity": activity, "seconds": float(seconds), "at": at if at is not None else self.clock(),
                           "machine": machine, "request_id": request_id,
                           "window": float(window) if window and window > 0 else None}))

    def _put(self, item) -> None:
        with self._lock:
            self._pending += 1
        self.q.put(item)
        self.ensure_threads()

    def flush(self, timeout: float = 10.0) -> bool:
        """Wait until everything queued so far is written (or failed). Returns whether it was within ``timeout``. A
        ledger with no threads writes it on the calling thread."""
        if not self.background:
            self.drain()
            return True
        self._hurry.set()
        deadline = time.monotonic() + timeout
        try:
            with self._idle:
                while self._pending > 0:
                    left = deadline - time.monotonic()
                    if left <= 0:
                        return False
                    if self._writer is None or not self._writer.is_alive():
                        self._lock.release()
                        try:
                            self.ensure_threads()
                        finally:
                            self._lock.acquire()
                    self._idle.wait(timeout=min(left, 0.25))
                return True
        finally:
            self._hurry.clear()

    def stop(self, timeout: float = 10.0) -> bool:
        """Drain the queue (the writer writes what it has) and stop both threads: the server's shutdown. A ledger
        with no threads (``background`` off) writes what it has on the calling thread."""
        if not self.background:
            self.drain()
            self._stop.set()
            return True
        ok = self.flush(timeout)
        self._stop.set()
        return ok

    def drain(self) -> int:
        """Write everything queued now, on the calling thread, in batches (a ledger with no threads: tests, and a
        stop without them). Returns the rows written."""
        written = 0
        while True:
            batch = []
            while len(batch) < BATCH_EVENTS:
                try:
                    batch.append(self.q.get_nowait())
                except _queue.Empty:
                    break
            if not batch:
                return written
            written += self.write(batch)

    # -------------------------------------------------------------- threads
    def ensure_threads(self) -> None:
        """Start the writer and the rollup thread, or start them again if they died (P3.9.2)."""
        if not self.background or self._stop.is_set():
            return
        with self._lock:
            if self._writer is None or not self._writer.is_alive():
                if self._writer is not None:
                    log.warning("the ledger's writer had stopped; starting it again")
                self._writer = threading.Thread(target=self._write_loop, daemon=True, name="ledger-writer")
                self._writer.start()
            if self._roller is None or not self._roller.is_alive():
                if self._roller is not None:
                    log.warning("the ledger's rollup thread had stopped; starting it again")
                self._roller = threading.Thread(target=self._rollup_loop, daemon=True, name="ledger-rollups")
                self._roller.start()

    def _write_loop(self) -> None:
        while True:
            try:
                try:
                    first = self.q.get(timeout=0.5)
                except _queue.Empty:
                    if self._stop.is_set():
                        return
                    continue
                batch = [first]
                deadline = time.monotonic() + BATCH_SECONDS
                while len(batch) < BATCH_EVENTS:
                    try:
                        batch.append(self.q.get_nowait())
                        continue
                    except _queue.Empty:
                        pass
                    left = deadline - time.monotonic()
                    if left <= 0 or self._hurry.is_set() or self._stop.is_set():
                        break
                    try:
                        batch.append(self.q.get(timeout=min(left, 0.1)))
                    except _queue.Empty:
                        continue
                self.write(batch)
            except Exception:
                log.exception("the ledger's writer loop failed; it carries on")

    def _rollup_loop(self) -> None:
        first = True
        while not self._stop.is_set():
            if not first and self._stop.wait(ROLLUP_SECONDS):
                return
            first = False
            try:
                self.pass_once()
            except Exception:
                log.exception("the ledger's rollup pass failed; it carries on")

    def pass_once(self) -> int:
        """One rollup pass, the lab's new results, and the budgets' grants read again (``Keeper.sync``: an
        administrator's repair from another process lands here): what the rollup thread does every
        ``ROLLUP_SECONDS``. Returns the hours recomputed."""
        from . import db
        from .pool import budgets
        if not db.configured():
            return 0
        try:
            ingest_lab(self)
        except Exception:
            log.warning("could not read the lab's results into the ledger", exc_info=True)
        try:
            with db.session() as s:
                budgets.keeper().sync(s)
        except Exception:
            log.warning("could not read the budgets again", exc_info=True)
        with db.session() as s:
            return rollup(s)

    # -------------------------------------------------------------- the writer's work
    def write(self, batch: list) -> int:
        """Insert a batch, with the budget entries its steps settled, in one transaction, tried again a few times
        while the database is too busy (``WRITE_TRIES``). Returns the rows written (0 when there is no database: then
        nothing is kept). The budget keeper hears when the entries are the database's (``Keeper.written``), and a
        ``rows`` item's waiter when its rows are written."""
        from . import db
        from .pool import budgets
        from sqlalchemy.exc import OperationalError
        keeper = budgets.keeper()
        holds = [getattr(item[1], "reservation", None) for item in batch if item[0] == "step"]
        keeper.writing()
        try:
            if not db.configured():
                return 0
            from sqlalchemy import insert
            from .db.models import ComputeEvent
            for attempt in range(WRITE_TRIES):
                try:
                    with db.session() as s:
                        rows, settled = self._rows(s, batch)
                        if rows:
                            s.execute(insert(ComputeEvent.__table__), rows)
                        if settled:
                            budgets.settle_entries(s, settled)
                    break
                except OperationalError:
                    # the database's write lock held past its busy timeout (a long transaction elsewhere): wait and try
                    # the whole batch again rather than lose it
                    if attempt == WRITE_TRIES - 1:
                        raise
                    log.info("the database is busy; the ledger tries its batch of %d again", len(batch))
                    time.sleep(WRITE_BACKOFF * (2 ** attempt))
            self.written += len(rows)
            for fn in list(self.listeners):
                try:
                    fn(rows)
                except Exception:
                    log.exception("a ledger listener failed")
            return len(rows)
        except Exception:
            self.failures += 1
            now = time.monotonic()
            if now - self._failed_at > 60:
                self._failed_at = now
                log.warning("the ledger could not write %d event(s); they are lost", len(batch), exc_info=True)
            return 0
        finally:
            keeper.written(holds)
            for item in batch:
                if item[0] == "rows" and item[2] is not None:
                    item[2].set()
            with self._idle:
                self._pending -= len(batch)
                self._idle.notify_all()

    def _rows(self, session, batch: list) -> tuple:
        """The table rows of a batch, and its steps' settled budget holds ``[(reservation, credits)]`` for their
        entries."""
        from . import servers
        classes = self.classes(session)
        host = servers.load().get("host_server_id")
        rows, settled = [], []
        for item in batch:
            if item[0] == "step":
                _kind, ev, priced = item
                rows.append(self._step_row(session, ev, priced))
                r = getattr(ev, "reservation", None)
                if r is not None:
                    settled.append((r, priced["credits"]))
            elif item[0] == "rows":
                rows.extend(dict(r) for r in item[1])        # made whole by their writer (``ingest_lab``)
            else:
                row = self._cpu_row(session, item[1], classes, host)
                if row is not None:
                    rows.append(row)
        self._known_accounts(session, rows)
        return rows, settled

    @staticmethod
    def _known_accounts(session, rows: list) -> None:
        """An account the batch names that the database does not hold (the binding's consumer of work whose row is
        gone) is nobody: the event is kept, on the server's side, rather than the whole batch refused by the foreign
        key."""
        from sqlalchemy import select
        from .db.models import User
        named = {r[k] for r in rows for k in ("consumer_id", "provider_id") if r[k]}
        if not named:
            return
        known = {uid for (uid,) in session.execute(select(User.id).where(User.id.in_(sorted(named))))}
        if named <= known:
            return
        for r in rows:
            for k in ("consumer_id", "provider_id"):
                if r[k] and r[k] not in known:
                    log.info("compute event of %s names no account %s: counted as the server's", r["activity"], r[k])
                    r[k] = None
            r["self_use"] = r["consumer_id"] == r["provider_id"]

    def _step_row(self, session, ev, priced: dict) -> dict:
        from . import servers
        facts = servers.machine_facts(ev.machine) or {}
        activity = ev.activity or (f"test:{ev.request_id}" if ev.cls == "test" else f"unassigned:{ev.request_id}")
        consumer, game_kind = self.owner_of(session, activity, ev.consumer)
        provider = None if facts.get("server_resource") or not facts else facts.get("owner_id")
        u = ev.usage or {}
        return {"at": datetime.fromtimestamp(ev.at, timezone.utc), "activity": activity[:160],
                "activity_kind": activity_kind(activity, ev.cls, game_kind), "task_class": ev.task_class or "full",
                "consumer_id": consumer, "provider_id": provider, "machine_id": ev.machine[:40],
                "kind": priced["kind"], "model": (ev.model or None) and str(ev.model)[:200],
                "gen_s": max(0.0, float(ev.gen_s or 0.0)), "cpu_core_s": 0.0,
                "reported_s": ev.reported_s, "observed_s": ev.observed_s,
                "tokens_in": int(u.get("input_tokens", 0) or 0), "tokens_out": int(u.get("output_tokens", 0) or 0),
                "tokens_reasoning": int(u.get("reasoning_tokens", 0) or 0),
                "tokens_cache_read": int(u.get("cache_read_input_tokens", 0) or 0),
                "tokens_cache_write": int(u.get("cache_creation_input_tokens", 0) or 0),
                "credits": priced["credits"], "flags": dict(priced["flags"]), "self_use": consumer == provider,
                "verification": "unverified", "request_id": (ev.request_id or None) and ev.request_id[:64]}

    def _cpu_row(self, session, item: dict, classes: list, host: Optional[str]) -> Optional[dict]:
        from . import servers
        machine = item.get("machine") or host or HOST_UNKNOWN
        facts = servers.machine_facts(machine) or {}
        consumer, game_kind = self.owner_of(session, item["activity"], None)
        provider = None                       # the host's CPU is the server's (P3.4.3)
        cls = class_of(facts, "cpu", classes)
        seconds = float(item["seconds"])
        return {"at": datetime.fromtimestamp(item["at"], timezone.utc), "activity": item["activity"][:160],
                "activity_kind": activity_kind(item["activity"], None, game_kind), "task_class": "cpu",
                "consumer_id": consumer, "provider_id": provider, "machine_id": machine[:40], "kind": "cpu",
                "model": None, "gen_s": 0.0, "cpu_core_s": round(seconds, 6), "reported_s": None,
                # the wall time the seconds were measured over (the tracker's window), which costing spreads them over
                "observed_s": round(item["window"], 3) if item.get("window") else None,
                "tokens_in": 0, "tokens_out": 0, "tokens_reasoning": 0, "tokens_cache_read": 0,
                "tokens_cache_write": 0, "credits": round(credits_for("cpu", cpu_core_s=seconds,
                                                                      rate=cls["rate"] if cls else 0.0), 6),
                "flags": {}, "self_use": consumer is None, "verification": "verified",
                "request_id": (item.get("request_id") or None) and item["request_id"][:64]}

    # -------------------------------------------------------------- what the writer reads (cached)
    def classes(self, session=None) -> list:
        """The hardware classes, read again every ``CLASS_SECONDS`` (``invalidate_classes`` at once)."""
        now = time.monotonic()
        if self._classes is not None and now - self._classes_at < CLASS_SECONDS:
            return self._classes
        from sqlalchemy import select
        from .db.models import HardwareClass
        try:
            if session is None:
                from . import db
                if not db.configured():
                    return self._classes or []
                with db.session() as s:
                    rows = [class_dict(c) for c in s.scalars(select(HardwareClass))]
            else:
                rows = [class_dict(c) for c in session.scalars(select(HardwareClass))]
        except Exception:
            log.debug("could not read the hardware classes", exc_info=True)
            return self._classes or []
        self._classes, self._classes_at = rows, now
        return rows

    def invalidate_classes(self) -> None:
        self._classes = None

    def owner_of(self, session, activity: str, fallback: Optional[str]) -> tuple:
        """(the consumer of ``activity``, its game row's kind or None), cached for ``OWNER_SECONDS``: its row's owner,
        or nobody when the row is marked ``collection``; ``fallback`` when it has no row."""
        now = time.monotonic()
        hit = self._owners.get(activity)
        if hit is not None and now - hit[3] < OWNER_SECONDS:
            consumer, game_kind, found, _ = hit
            return (consumer if found else fallback), game_kind
        consumer, game_kind, found = owner_of(session, activity)
        # an activity with no row yet (a game whose row is written as it starts) is asked again within seconds
        self._owners[activity] = (consumer, game_kind, found, now if found else now - OWNER_SECONDS + 5.0)
        if len(self._owners) > 20000:
            self._owners.clear()
        return (consumer if found else fallback), game_kind

    def forget_owner(self, activity: str) -> None:
        self._owners.pop(activity, None)

    # -------------------------------------------------------------- budgets' estimate (the request path: memory only)
    def estimate(self, machine_id: Optional[str], model: Optional[str], *, max_tokens: Optional[int] = None,
                 input_tokens: int = 0) -> tuple:
        """What one request will cost, for its budget's reservation (P3.4.6): (credits, unpriced). On an online API the
        price of ``max_tokens`` of output plus its input at the model's prices, times the class rate (unpriced: no
        price); elsewhere the machine's mean credits per step for that model (``pool.budgets.MIN_RESERVE`` at least)."""
        from . import servers
        from .pool import budgets
        facts = servers.machine_facts(machine_id) or {}
        if facts.get("kind") == "api":
            price = _api_price(machine_id, model, utcnow())
            if price is None:
                return 0.0, True
            cls = class_of(facts, "api", self._classes or self.classes())
            cost = token_cost(price, {"tokens_in": int(input_tokens or 0),
                                      "tokens_out": int(max_tokens or DEFAULT_MAX_TOKENS)})
            return max(budgets.MIN_RESERVE, credits_for("api", cost=cost, rate=cls["rate"] if cls else 0.0)), False
        return max(budgets.MIN_RESERVE, float(self._means.get((machine_id, model), 0.0))), False


_LEDGER: Optional[Ledger] = None
_LEDGER_LOCK = threading.Lock()


def get() -> Ledger:
    """The process's ledger, made on first use."""
    global _LEDGER
    with _LEDGER_LOCK:
        if _LEDGER is None:
            _LEDGER = Ledger()
        return _LEDGER


def current() -> Optional[Ledger]:
    return _LEDGER


def reset(**kw) -> Ledger:
    """A fresh ledger (tests): the old one is drained and stopped."""
    global _LEDGER
    with _LEDGER_LOCK:
        old, _LEDGER = _LEDGER, Ledger(**kw)
    if old is not None:
        old.stop(timeout=5.0)
    return _LEDGER


def listener(event) -> None:
    """The prompt scheduler's listener (P3.3.4): one event per model step, priced and its budget hold settled in memory
    at once (``Ledger.step``); the writer writes the event and the budget's entry."""
    get().step(event)


listener.settles_budgets = True


# ---------------------------------------------------------------------------- the host's CPU, per unit of work

_units = threading.local()


@contextmanager
def cpu_unit(activity: Optional[str]):
    """Measure the calling thread's CPU time (``time.thread_time``) over a unit of work and charge it to ``activity``
    (P3.4.3): a drive or an engine call on a game's driver thread, a chat answer on a responder's thread, a report's
    build, a probe case. A thread that serves many games (a responder pool's) is charged unit by unit, so each game pays
    its own time. A unit inside another on the same thread is the outer one's (counted once). The usage tracker sums
    the units per activity and hands the ledger one ``cpu`` event per activity a minute."""
    depth = getattr(_units, "depth", 0)
    if depth or not activity:
        _units.depth = depth + 1
        try:
            yield
        finally:
            _units.depth = depth
        return
    from . import usage
    tracker = usage.tracker()
    tid = threading.get_native_id()
    t0 = time.thread_time()
    _units.depth = 1
    tracker.unit_started(tid, t0)
    try:
        yield
    finally:
        _units.depth = 0
        used = max(0.0, time.thread_time() - t0)
        tracker.unit_ended(tid, activity, used)


# ---------------------------------------------------------------------------- owners

def owner_of(session, activity: str) -> tuple:
    """(consumer, game row kind, found) for an activity id: its row's owner (null for an item marked ``collection``)
    and whether a row was found at all."""
    from sqlalchemy import select
    from .db.models import Game, OwnedItem
    prefix, _, rest = (activity or "").partition(":")
    if not rest:
        return None, None, False

    def item(kind: str, ref: str) -> tuple:
        row = session.scalar(select(OwnedItem).where(OwnedItem.kind == kind, OwnedItem.ref == ref))
        if row is None:
            return None, None, False
        return (None if row.collection else row.owner_id), None, True

    if prefix == "game":
        row = session.get(Game, rest)
        if row is None:
            return None, None, False
        ref = (row.meta or {}).get("item") if isinstance(row.meta, dict) else None
        if isinstance(ref, (list, tuple)) and len(ref) == 2:
            it = session.scalar(select(OwnedItem).where(OwnedItem.kind == ref[0], OwnedItem.ref == ref[1]))
            if it is not None and it.collection:
                return None, row.kind, True
        return row.owner_id, row.kind, True
    if prefix in ("probe", "probe_run"):
        return item("probe_run", rest)
    if prefix == "report":
        return item("report", rest)
    if prefix == "benchmark_run":
        return item("benchmark_run", rest)
    if prefix == "lab":
        return item("lab_experiment", rest.split(":", 1)[0])
    return None, None, False


# ---------------------------------------------------------------------------- rollups

def _watermark(session) -> int:
    from .db.models import Setting
    row = session.get(Setting, WATERMARK_KEY)
    value = row.value if row is not None else None
    try:
        return int((value or {}).get("v") or 0)
    except (TypeError, ValueError, AttributeError):
        return 0


def _set_watermark(session, mark: int) -> None:
    from .db.models import Setting
    row = session.get(Setting, WATERMARK_KEY)
    if row is None:
        session.add(Setting(key=WATERMARK_KEY, value={"v": int(mark)}))
    else:
        row.value = {"v": int(mark)}


def rollup(session, hours: Optional[Iterable[datetime]] = None) -> int:
    """Recompute the rollups of ``hours`` (by default every hour an event landed in since the last pass, which then
    moves the watermark), each hour's rows deleted and inserted again together. ``session`` is committed every
    ``ROLLUP_CHUNK`` hours, so a long backlog (a first start, a rollup thread down for days) never holds the database's
    write lock for long; the watermark moves only with the last chunk, so until then ``totals`` reads those hours raw,
    and a pass cut short is done again whole by the next. Returns the hours recomputed."""
    from sqlalchemy import select
    from .db.models import ComputeEvent
    new_mark = None
    if hours is None:
        mark = _watermark(session)
        found: set = set()
        new_mark = mark
        for eid, at in session.execute(select(ComputeEvent.id, ComputeEvent.at).where(ComputeEvent.id > mark)):
            found.add(hour_of(at))
            new_mark = max(new_mark, eid)
        hours = found
    hours = sorted({hour_of(h) for h in hours})
    for i in range(0, len(hours), ROLLUP_CHUNK):
        for h in hours[i:i + ROLLUP_CHUNK]:
            _rollup_hour(session, h)
        if i + ROLLUP_CHUNK < len(hours):
            session.commit()
    if new_mark is not None and new_mark != _watermark(session):
        _set_watermark(session, new_mark)
    session.flush()
    return len(hours)


def _rollup_hour(session, h: datetime) -> None:
    from sqlalchemy import delete, func, select
    from .db.models import ComputeEvent, ComputeRollup
    session.execute(delete(ComputeRollup).where(ComputeRollup.hour == h))
    for side, col in (("given", ComputeEvent.provider_id), ("used", ComputeEvent.consumer_id)):
        q = select(col, ComputeEvent.kind, func.sum(ComputeEvent.gen_s), func.sum(ComputeEvent.cpu_core_s),
                   func.sum(ComputeEvent.credits), func.sum(ComputeEvent.tokens_in),
                   func.sum(ComputeEvent.tokens_out), func.count()).where(
            ComputeEvent.at >= h, ComputeEvent.at < h + HOUR, col.isnot(None),
            ComputeEvent.self_use.is_(False)).group_by(col, ComputeEvent.kind)
        for user, kind, gen, cpu, credits, tin, tout, n in session.execute(q):
            session.add(ComputeRollup(user_id=user, hour=h, side=side, kind=kind, gen_s=float(gen or 0.0),
                                      cpu_core_s=float(cpu or 0.0), credits=float(credits or 0.0),
                                      tokens_in=int(tin or 0), tokens_out=int(tout or 0), events=int(n or 0)))
    session.flush()


# ---------------------------------------------------------------------------- totals and the ratio

def _blank() -> dict:
    return {"gpu_s": 0.0, "cpu_s": 0.0, "credits": 0.0, "tokens_in": 0, "tokens_out": 0, "events": 0,
            "by_kind": {k: {"seconds": 0.0, "credits": 0.0, "events": 0} for k in KINDS}}


def _add(t: dict, kind: str, gen: float, cpu: float, credits: float, tin: int, tout: int, n: int) -> None:
    t["gpu_s"] += float(gen or 0.0)
    t["cpu_s"] += float(cpu or 0.0)
    t["credits"] += float(credits or 0.0)
    t["tokens_in"] += int(tin or 0)
    t["tokens_out"] += int(tout or 0)
    t["events"] += int(n or 0)
    k = t["by_kind"].setdefault(kind, {"seconds": 0.0, "credits": 0.0, "events": 0})
    k["seconds"] += float(cpu or 0.0) if kind == "cpu" else float(gen or 0.0)
    k["credits"] += float(credits or 0.0)
    k["events"] += int(n or 0)


def range_start(range_: Optional[str], now: Optional[datetime] = None) -> Optional[datetime]:
    """When a range (``24h``, ``7d``, ``30d``, ``all``) starts, or None for all; ValueError for another."""
    if range_ not in RANGES:
        raise ValueError(f"A range is one of {', '.join(RANGES)}.")
    span = RANGES[range_]
    return None if span is None else (now or utcnow()) - timedelta(seconds=span)


def totals(session, user_ids: Iterable[str], since: Optional[datetime] = None, *,
           now: Optional[datetime] = None) -> dict:
    """Each member's given and used compute since ``since`` (None: all time): ``{uid: {"given": t, "used": t}}``,
    each ``t`` with ``gpu_s``, ``cpu_s``, ``credits``, ``tokens_in``, ``tokens_out``, ``events`` and ``by_kind``. The
    rollups give the complete hours they hold; the raw events give the range's first partial hour, the current hour
    and every hour with events newer than the rollups' watermark, so a total equals the raw sums at any moment."""
    from sqlalchemy import and_, func, or_, select
    from .db.models import ComputeEvent, ComputeRollup
    ids = [u for u in dict.fromkeys(user_ids) if u]
    out = {u: {"given": _blank(), "used": _blank()} for u in ids}
    if not ids:
        return out
    now = now or utcnow()
    current = hour_of(now)
    mark = _watermark(session)
    dirty = {hour_of(at) for (at,) in session.execute(select(ComputeEvent.at).where(ComputeEvent.id > mark))}
    first_full = None
    if since is not None:
        first_full = hour_of(since) if hour_of(since) == since else hour_of(since) + HOUR
    if len(dirty) > MAX_DIRTY_HOURS:
        # the rollups are far behind (their thread has not run for days): the raw events alone, whose query stays
        # small, rather than a condition with an hour for each
        raw = [ComputeEvent.at.isnot(None)]
    else:
        rq = select(ComputeRollup).where(ComputeRollup.user_id.in_(ids), ComputeRollup.hour < current)
        if first_full is not None:
            rq = rq.where(ComputeRollup.hour >= first_full)
        if dirty:
            rq = rq.where(ComputeRollup.hour.notin_(sorted(dirty)))
        for r in session.scalars(rq):
            _add(out[r.user_id][r.side], r.kind, r.gen_s, r.cpu_core_s, r.credits, r.tokens_in, r.tokens_out,
                 r.events)
        raw = [ComputeEvent.at >= current] + [and_(ComputeEvent.at >= h, ComputeEvent.at < h + HOUR) for h in dirty]
        if first_full is not None:
            raw.append(ComputeEvent.at < first_full)
    for side, col in (("given", ComputeEvent.provider_id), ("used", ComputeEvent.consumer_id)):
        q = select(col, ComputeEvent.kind, func.sum(ComputeEvent.gen_s), func.sum(ComputeEvent.cpu_core_s),
                   func.sum(ComputeEvent.credits), func.sum(ComputeEvent.tokens_in),
                   func.sum(ComputeEvent.tokens_out), func.count()).where(
            col.in_(ids), ComputeEvent.self_use.is_(False), or_(*raw))
        if since is not None:
            q = q.where(ComputeEvent.at >= since)
        for user, kind, gen, cpu, credits, tin, tout, n in session.execute(q.group_by(col, ComputeEvent.kind)):
            _add(out[user][side], kind, gen, cpu, credits, tin, tout, n)
    return out


def ratio(given: float, used: float) -> dict:
    """The ratio of given to used credits: ``value`` (None when nothing was used) and its words ("2.5", "only
    given", "nothing yet")."""
    given, used = float(given or 0.0), float(used or 0.0)
    if used > 0:
        value = given / used
        return {"value": round(value, 4), "text": f"{value:.2f}"}
    return {"value": None, "text": "only given" if given > 0 else "nothing yet"}


def _rounded(t: dict) -> dict:
    return {"gpu_s": round(t["gpu_s"], 3), "cpu_s": round(t["cpu_s"], 3), "credits": round(t["credits"], 4),
            "tokens_in": t["tokens_in"], "tokens_out": t["tokens_out"], "events": t["events"],
            "by_kind": {k: {"seconds": round(v["seconds"], 3), "credits": round(v["credits"], 4), "events": v["events"]}
                        for k, v in t["by_kind"].items()}}


def summary(session, user_id: str, range_: str = "30d", *, now: Optional[datetime] = None) -> dict:
    """One member's totals over a range: given, used and the ratio (what ``/api/users/{handle}/ratio`` shows)."""
    now = now or utcnow()
    since = range_start(range_, now)
    t = totals(session, [user_id], since, now=now)[user_id]
    return {"range": range_, "since": since.isoformat() if since else None, "given": _rounded(t["given"]),
            "used": _rounded(t["used"]), "ratio": ratio(t["given"]["credits"], t["used"]["credits"])}


def breakdown(session, user, range_: str = "30d", *, now: Optional[datetime] = None) -> dict:
    """``summary`` and where it came from (``/api/ledger/me``): given and used by machine and by activity kind, and
    the member's own use of their own machines (counted on neither side). A machine the member may not see now is
    named only as whose it is."""
    from sqlalchemy import func, or_, select
    from .db.models import ComputeEvent, Server
    from .pool import visible
    now = now or utcnow()
    out = summary(session, user.id, range_, now=now)
    since = range_start(range_, now)
    by_machine: dict = defaultdict(lambda: {"given": 0.0, "used": 0.0, "gpu_s": 0.0, "cpu_s": 0.0, "events": 0})
    by_kind: dict = defaultdict(lambda: {"given": 0.0, "used": 0.0, "events": 0})
    own = {"credits": 0.0, "gpu_s": 0.0, "cpu_s": 0.0, "events": 0}
    q = select(ComputeEvent.machine_id, ComputeEvent.activity_kind, ComputeEvent.provider_id, ComputeEvent.consumer_id,
               ComputeEvent.self_use, func.sum(ComputeEvent.credits), func.sum(ComputeEvent.gen_s),
               func.sum(ComputeEvent.cpu_core_s), func.count()).where(
        or_(ComputeEvent.provider_id == user.id, ComputeEvent.consumer_id == user.id))
    if since is not None:
        q = q.where(ComputeEvent.at >= since)
    q = q.group_by(ComputeEvent.machine_id, ComputeEvent.activity_kind, ComputeEvent.provider_id,
                   ComputeEvent.consumer_id, ComputeEvent.self_use)
    for machine, kind, provider, consumer, self_use, credits, gen, cpu, n in session.execute(q):
        credits, gen, cpu, n = float(credits or 0.0), float(gen or 0.0), float(cpu or 0.0), int(n or 0)
        if self_use:
            own["credits"] += credits
            own["gpu_s"] += gen
            own["cpu_s"] += cpu
            own["events"] += n
            continue
        for side, who in (("given", provider), ("used", consumer)):
            if who != user.id:
                continue
            m = by_machine[machine]
            m[side] += credits
            m["gpu_s"] += gen
            m["cpu_s"] += cpu
            m["events"] += n
            k = by_kind[kind]
            k[side] += credits
            k["events"] += n
    machines = []
    for sid, m in sorted(by_machine.items(), key=lambda kv: -(kv[1]["given"] + kv[1]["used"])):
        row = session.get(Server, sid)
        seen = row is not None and visible(session, user, row)
        if seen:
            name = row.name
        elif row is not None and (row.owner_id is None or row.is_host or row.server_resource):
            name = "a machine of the site's"
        elif row is None:
            name = "a machine since removed"
        else:
            name = "another member's machine"
        machines.append({"id": sid if seen else None, "name": name,
                         **{k: round(v, 4) if isinstance(v, float) else v for k, v in m.items()}})
    out["by_machine"] = machines
    out["by_activity"] = [{"kind": k, **{x: round(y, 4) if isinstance(y, float) else y for x, y in v.items()}}
                          for k, v in sorted(by_kind.items())]
    out["own_use"] = {k: round(v, 4) if isinstance(v, float) else v for k, v in own.items()}
    return out


# ---------------------------------------------------------------------------- reprice

def reprice(session, since: datetime, until: Optional[datetime] = None, *, ledger: Optional[Ledger] = None) -> dict:
    """Recompute the credits of every event in [since, until) with the classes, rates and token prices in force now
    (an administrator's ``citar admin reprice``), then the rollups of the hours touched. ``session`` is committed
    every ``REPRICE_PAGE`` events (with that page's hours rolled up again), so a long period never holds the database's
    write lock for long; a reprice cut short leaves the pages before repriced, and running it again finishes it.
    Returns ``{"events", "changed", "hours"}``."""
    from sqlalchemy import select
    from . import servers
    from .db.models import ComputeEvent, HardwareClass
    classes = [class_dict(c) for c in session.scalars(select(HardwareClass))]
    n = changed = 0
    touched: set = set()
    last = 0
    while True:
        q = select(ComputeEvent).where(ComputeEvent.at >= since, ComputeEvent.id > last)
        if until is not None:
            q = q.where(ComputeEvent.at < until)
        page = list(session.scalars(q.order_by(ComputeEvent.id).limit(REPRICE_PAGE)))
        if not page:
            break
        hours: set = set()
        for ev in page:
            last = ev.id
            n += 1
            facts = servers.machine_facts(ev.machine_id) or {}
            flags = dict(ev.flags or {})
            if ev.kind == "api":
                cls = class_of(facts, "api", classes)
                cost = token_cost(_api_price(ev.machine_id, ev.model, ev.at),
                                  {"tokens_in": ev.tokens_in, "tokens_out": ev.tokens_out,
                                   "tokens_cache_read": ev.tokens_cache_read,
                                   "tokens_cache_write": ev.tokens_cache_write})
                if cost is None:
                    flags["unpriced"] = True
                else:
                    flags.pop("unpriced", None)
                credits = credits_for("api", cost=cost, rate=cls["rate"] if cls else 0.0)
            elif ev.kind == "cpu":
                cls = class_of(facts, "cpu", classes)
                credits = credits_for("cpu", cpu_core_s=ev.cpu_core_s, rate=cls["rate"] if cls else 0.0)
            else:
                cls = class_of(facts, "gpu", classes)
                credits = credits_for("gpu", gen_s=ev.gen_s, rate=cls["rate"] if cls else 0.0)
            credits = round(credits, 6)
            if abs(credits - (ev.credits or 0.0)) > 1e-9 or flags != (ev.flags or {}):
                ev.credits, ev.flags = credits, flags
                changed += 1
                hours.add(hour_of(ev.at))
        session.flush()
        rollup(session, hours)
        session.commit()
        for ev in page:
            session.expunge(ev)             # a long period's events are not all kept in the session
        touched |= hours
    if ledger is not None:
        ledger.invalidate_classes()
    return {"events": n, "changed": changed, "hours": len(touched)}


# ---------------------------------------------------------------------------- the lab's results

_LAB_LOCK = threading.Lock()


def ingest_lab(ledger: Optional[Ledger] = None, results_dir=None) -> int:
    """Each finished lab game's ``cpu_s`` (its result line, ``lab.py``) as one ``cpu`` event on the host, its
    experiment's owner the consumer (P3.4.3), its ``observed_s`` the game's wall time (``seconds``: costing spreads the
    CPU over the game, not over a minute). The lab runner is a process of its own, often without the server's database;
    the server reads its results files here (the rollup thread's pass, and a report before it gathers), each game once
    (its activity and ``request_id`` are ``lab:<experiment>:<game>``).

    One reader at a time (``_LAB_LOCK``), in a transaction of its own begun under that lock: the games already in the
    ledger are read after the last reader's events were written, so none is written twice. With a ``ledger`` its
    writer thread writes the events, as it writes every other (one inserting thread: the ids commit in order, which the
    rollups' watermark relies on), and the reader waits for them; without one they are inserted here. Returns the
    events written."""
    from pathlib import Path
    from sqlalchemy import insert
    from . import db, paths
    from .db.models import ComputeEvent
    folder = Path(results_dir) if results_dir is not None else paths.saves_path("lab", "results")
    if not db.configured() or not folder.is_dir():
        return 0
    seen = ledger._lab_seen if ledger is not None else {}
    with _LAB_LOCK:
        with db.session() as s:
            rows, stamps = _lab_rows(s, ledger, folder, seen)
        if rows and ledger is not None and not ledger._stop.is_set():
            done = []
            for i in range(0, len(rows), BATCH_EVENTS):
                ev = threading.Event()
                done.append(ev)
                ledger._put(("rows", rows[i:i + BATCH_EVENTS], ev))
            if not ledger.background:
                ledger.drain()
            deadline = time.monotonic() + LAB_WRITE_SECONDS
            for ev in done:
                if not ev.wait(max(0.0, deadline - time.monotonic())):
                    # not written yet (the writer is far behind): the files are read again next time, and whatever the
                    # writer has written by then is not written twice
                    log.warning("the ledger's writer has not written the lab's events yet")
                    return 0
        elif rows:
            with db.session() as s:
                s.execute(insert(ComputeEvent.__table__), rows)
        seen.update(stamps)
    return len(rows)


def _lab_rows(session, ledger: Optional[Ledger], folder, seen: dict) -> tuple:
    """``ingest_lab``'s events of the results files changed since they were last read, and those files' stamps."""
    from sqlalchemy import select
    from . import servers
    from .db.models import ComputeEvent, HardwareClass
    classes = (ledger.classes(session) if ledger is not None else
               [class_dict(c) for c in session.scalars(select(HardwareClass))])
    host = servers.load().get("host_server_id") or HOST_UNKNOWN
    facts = servers.machine_facts(host) or {}
    cls = class_of(facts, "cpu", classes)
    rate = cls["rate"] if cls else 0.0
    rows, stamps = [], {}
    for path in sorted(folder.glob("*.jsonl")):
        try:
            st = path.stat()
        except OSError:
            continue
        stamp = (st.st_mtime, st.st_size)
        if seen.get(str(path)) == stamp:
            continue
        exp = path.stem
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        pattern = "lab:" + exp.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + ":%"
        have = {act for (act,) in session.execute(select(ComputeEvent.activity).where(
            ComputeEvent.activity_kind == "lab", ComputeEvent.activity.like(pattern, escape="\\")))}
        consumer, _game_kind, _found = owner_of(session, f"lab:{exp}")
        for line in lines:
            try:
                res = json.loads(line)
            except ValueError:
                continue
            if not isinstance(res, dict) or res.get("crash") or res.get("i") is None:
                continue
            # the usage ledger's activity of the game (``usage.lab_game_rows``), so costing joins its held time
            activity = f"lab:{exp}:{res['i']}"[:160]
            rid = activity[:64]
            cpu_s = float(res.get("cpu_s") or res.get("seconds") or 0.0)
            if activity in have or cpu_s <= 0:
                continue
            try:
                at = datetime.fromisoformat(res["finished"]).astimezone(timezone.utc) if res.get("finished") else utcnow()
            except (TypeError, ValueError):
                at = utcnow()
            try:
                wall = float(res.get("seconds") or 0.0)
            except (TypeError, ValueError):
                wall = 0.0
            rows.append({"at": at, "activity": activity[:160], "activity_kind": "lab", "task_class": "cpu",
                         "consumer_id": consumer, "provider_id": None, "machine_id": host[:40], "kind": "cpu",
                         "model": None, "gen_s": 0.0, "cpu_core_s": round(cpu_s, 6), "reported_s": None,
                         "observed_s": round(wall, 3) if wall > 0 else None, "tokens_in": 0, "tokens_out": 0,
                         "tokens_reasoning": 0, "tokens_cache_read": 0, "tokens_cache_write": 0,
                         "credits": round(credits_for("cpu", cpu_core_s=cpu_s, rate=rate), 6), "flags": {},
                         "self_use": consumer is None, "verification": "verified", "request_id": rid})
            have.add(activity)
        stamps[str(path)] = stamp
    return rows, stamps
