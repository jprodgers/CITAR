"""The hub: the CITAR helpers connected to this server, on protocol 2 only (design/phase4-helper.md P4.5.1).

``crates/citar-helper/PROTOCOL.md`` is the protocol; ``citar.server.helper_protocol`` checks and builds its frames, and
the conformance transcripts (``crates/citar-helper/protocol/v2/transcripts``) are played against this hub over a real
socket by ``tests/test_helper_conformance.py``. Protocol 1 and the Python worker that spoke it are gone (package 4-05):
a protocol 1 hello gets one refusal telling its owner to download the new helper, and nothing else.

The threading problem is the same as it always was. A seat's turn is a plain thread calling a synchronous ``step()``,
while the sockets live on the event loop, so a completion crosses threads twice::

    turn thread          submit()  ->  run_coroutine_threadsafe  ->  event loop  ->  socket
    turn thread   <- Event.wait()  <-  resolve()                 <-  event loop  <-  socket

``submit()`` blocks the calling thread, which has nothing else to do until the model answers; nothing on the event
loop ever blocks (P4.7.4): token lookups, the hello's report, catalogs and hardware scans go through
``asyncio.to_thread``, and a frame longer than ``PARSE_INLINE_CHARS`` is parsed on a thread too (a first frame, sent
before any token is checked, on a thread of its own, ``_HELLO_PARSER``).

**The handshake** (``serve``), in PROTOCOL.md's order: (0) a first frame over ``HP.MAX_HELLO_BYTES`` is closed with
4400 ``too_large`` unread; (1) the protocol, a hello that is not protocol 2 getting the v1 refusal and close 4426 with
no lookup, no audit row, no rate-limit count and at most one log line per address an hour; (2) the address's rate limit,
before any lookup: an address with ``AuthLimiter.LIMIT`` distinct failing token hashes in the window is ``refused``
``rate`` (P4.0's defects 1 and 2: the old limit never refused, and the address was whatever ``X-Forwarded-For`` said),
and the check, the lookup and the count of its failure are one step per address (``WorkerHub.gate``), so hellos sent at
once cannot all pass the check before the first failure is counted; (3) the token, a failure counted and audited (a
database that fails is not the helper's failure: close 1011, nothing counted); (4) the machine key, registered by the
first protocol 2 hello of a machine (trust on first use, by a conditional write, so two first hellos at once cannot
both register) and refused when it differs (``refused`` ``key``) until its owner resets it, which closes the machine's
connection with 1012 so its next hello registers at once (``WorkerHub.disconnect``); (5) registration, then ``welcome``
with the site's settings, and the hello's report kept off the loop.

**What a helper reports** (P4.2.3, P4.3.5): its hello's hardware report and models, each ``catalog`` and each ``hardware``
frame are the machine's last report (``record_report``, applied as its modes say); each ``power`` frame, a minute of
10-second samples, is one power row of the machine (``record_power``, ``usage.PowerSampler``'s format, which costing
prices), whatever its power mode, since samples are measurements and the mode governs the static figures. The row is
placed by the hub's clock, never the helper's, and at most one a ``POWER_MIN_SECONDS`` per connection; samples that do not
read as a machine's (a draw over ``POWER_W_MAX``) are left out; and an owner who turned the machine's live sampling off
(``power.sampling`` ``off``) gets no rows.

**Settings** are acknowledged: the scheduler reads ``WorkerConnection.effective``, the helper's newest acknowledgement
clamped to what the site asks now (each number the smaller, the kinds both list, work on only when both say so). Each
acknowledgement answers the oldest ``settings`` frame not yet answered (the welcome's included), and a claim of more
than that frame asked flags the machine ``settings_overclaim``. A change of the machine's settings is pushed when it is
committed (the settings route, the machine editor's routes: ``WorkerHub.push_settings``), and the keep-alive pushes any
difference it finds between what the site asks and what a helper was last sent (a change made by another process).

**Cancel is acknowledged** (P4.2.4): ``cancel`` ends the caller's wait at once and sends ``cancel``; the helper's
``cancelled`` (or a ``done`` or ``error`` that crossed it) is handed to the scheduler as the request's late frame, which
frees its slot. The scheduler drains the slot, and flags the machine ``cancel_unacked``, only when no answer came within
``CANCEL_ACK_SECONDS`` (``citar.pool.scheduler``).

A connection lasts only as long as what let it in: revoking its token, disabling or deleting its machine, or suspending
the machine's owner drops it at once (``WorkerHub.drop``, ``shutdown`` with the matching code), and the keep-alive
checks every connection again at each ping, so a change made elsewhere drops it within a ping (P3.2.8). The newest
connection of a machine replaces an older one (``shutdown`` ``replaced``).

Time is the hub's ``clock``: the real one, or a test's, which the conformance player moves forward (the keep-alive, the
30 s for a hello, the rate limit's window and staleness read it).

Replaces: ``citar/worker/protocol.py`` (protocol 1) and the hub's protocol 1 handling here and in ``app.py``'s
``/ws/worker`` route.
"""
from __future__ import annotations

import asyncio
import collections
import concurrent.futures
import contextlib
import functools
import itertools
import logging
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Optional

from . import helper_protocol as HP

log = logging.getLogger("citar.workers")

#: How long after the last frame a helper is presumed gone (PROTOCOL.md, "Keep-alive"): three missed pings.
STALE_AFTER = 70.0
#: How long a new connection has to send its hello.
HELLO_SECONDS = 30.0
#: The longest the keep-alive naps between looks at the clock (a test's clock is moved forward meanwhile).
NAP_SECONDS = 1.0
#: How often a protocol 1 helper's refusal is logged per address.
V1_LOG_SECONDS = 3600.0
#: A frame longer than this many characters is parsed on a thread, never on the event loop: the parse's worst case
#: (text that is not JSON, or millions of tiny values) is about 0.1 s a MiB on the owner's laptop, so no frame holds the
#: loop for more than a few milliseconds (P4.7.4).
PARSE_INLINE_CHARS = 64 * 1024
#: How many ``settings`` frames may wait for their acknowledgement before the oldest is forgotten: each push is a
#: change of the site's settings, so only a helper that never acknowledges comes near it.
MAX_UNACKED_SETTINGS = 32

#: The one thread that parses long first frames, the ones sent before any token is checked: however many arrive, they
#: cost at most one core and never wait in front of the default pool's token lookups and database writes.
_HELLO_PARSER = concurrent.futures.ThreadPoolExecutor(max_workers=1, thread_name_prefix="citar-hello")

#: Each connection's own number (``WorkerConnection.serial``): the prompt scheduler tells a helper's reconnect from the
#: connection it had, since a request sent over a socket that has gone can never be answered.
_SERIALS = itertools.count(1)

#: The frame types this package's hub does not act on yet (P4.11): jobs from 4-09 and 4-10, updates from 4-13. A job
#: frame names a run, and no run is offered to any helper before 4-10, so each would be dropped anyway (PROTOCOL.md:
#: frames name only the helper's own runs).
NOT_YET = ("job_accept", "job_reject", "job_progress", "job_result", "job_failed", "job_aborted", "update_status")
#: How often a helper takes a power reading (PROTOCOL.md: "samples taken every 10 s"): a frame's samples cover this
#: long each.
POWER_SAMPLE_SECONDS = 10.0
#: The shortest time between two power rows of one connection: a helper sends a frame a minute, so a faster one is
#: dropped (logged once), and no helper can write rows faster than costing reads them.
POWER_MIN_SECONDS = 30.0
#: A draw above this, in watts, is no machine's reading and is left out of its row.
POWER_W_MAX = 20000.0


class Clock:
    """The hub's time: wall-clock seconds. A test's clock (``tests/helperfake.FakeClock``) has the same ``time`` and is
    moved forward by its player, which pokes the hub (``WorkerHub.poke``) so the keep-alive looks again at once."""

    def time(self) -> float:
        return time.time()


class WorkerError(RuntimeError):
    """The helper could not do it. ``refusal`` is the helper's error code (``busy``, ``closed``, ``unknown_model``,
    ``stopping``, ``endpoint``, ``timeout``, ``bad_request``), or the hub's own: ``closed`` (no helper connected),
    ``dropped`` (the machine was taken away), ``cancelled``. ``unfinished``: the request was sent and then given up
    (cancelled, or the wait ran out) without the helper's answer, which may still come over ``connection`` (the
    serial of the connection it went out on): the scheduler holds its slot until it does (P4.5.3)."""

    def __init__(self, message: str, *, refusal: str = "", retryable: bool = False, unfinished: bool = False,
                 connection: Optional[int] = None):
        super().__init__(message)
        self.refusal = refusal
        self.retryable = retryable
        self.unfinished = unfinished
        self.connection = connection


@dataclass
class Pending:
    """A request in flight, waiting for its answer on another thread. ``sent`` and ``done_at`` (``perf_counter``) are
    when its frame went out and when the helper's answer came: the generation time the server observed (P3.3.4)."""
    id: str
    event: threading.Event = field(default_factory=threading.Event)
    result: Optional[dict] = None
    error: Optional[WorkerError] = None
    sent: float = field(default_factory=time.perf_counter)
    done_at: Optional[float] = None


def _scheduler():
    """The process's prompt scheduler, if one was made (the hub's pushes need none otherwise)."""
    from ..pool import scheduler
    return scheduler.current()


def _helper_changed(server_id: str) -> None:
    """Tell the scheduler a helper connected, disconnected, acknowledged settings, reported its models or its quiet
    hours (P3.3.2: at once, not at its next read). Never raises."""
    try:
        sched = _scheduler()
        if sched is not None:
            sched.helper_changed(server_id)
    except Exception:
        log.exception("could not tell the scheduler about %s's helper", server_id)


# ---------------------------------------------------------------------------- the settings, as the site asks them

def site_settings(server_id: str) -> dict:
    """The machine's settings as the ``settings`` frame carries them (P4.2.3), from ``servers.machine_settings`` (the
    registry's cache: call it off the event loop). A machine that takes no model work, or a row that is gone or
    disabled, asks for none; a CPU figure of 0 is "the helper's own ceiling"."""
    from .. import servers
    ms = servers.machine_settings(server_id) or {}
    cpu = ms.get("cpu") or {}
    return {"llm": {"enabled": bool(ms.get("enabled") and ms.get("llm")),
                    "slots": max(0, min(HP.SLOTS.hi, int(ms.get("slots") or 1))),
                    "allow_swaps": bool(ms.get("allow_swaps"))},
            "cpu": {"enabled": bool(ms.get("enabled") and cpu.get("enabled")),
                    "threads": max(0, min(HP.THREADS.hi, int(cpu.get("threads") or 0))),
                    "cpu_percent": max(0, min(100, int(cpu.get("cpu_percent") or 0))),
                    "memory_mb": max(0, min(HP.MEMORY_MB.hi, int(cpu.get("memory_mb") or 0))),
                    "work": [k for k in HP.KINDS_OF_WORK if k in (cpu.get("work") or [])],
                    "yield_to_llm": bool(cpu.get("yield_to_llm", True))}}


def clamp(site: dict, ack: dict) -> tuple[dict, list]:
    """A helper's acknowledgement (``settings_ack``) held to what the site asked (PROTOCOL.md, "Settings"): each
    number the smaller of the site's and the helper's (a site 0, "the helper's ceiling", takes the helper's), the kinds
    both list, model or CPU work on only when both say so, ``yield_to_llm`` the site's. Returns the clamped
    ``{"llm", "cpu"}`` and the fields where the helper claimed more than the site asked (a correct helper never does)."""
    eff = ack["effective"]
    over: list = []

    def num(name: str, asked: int, said: int) -> int:
        if asked == 0:
            return said
        if said > asked:
            over.append(name)
        return min(asked, said)

    def on(name: str, asked: bool, said: bool) -> bool:
        if said and not asked:
            over.append(name)
        return asked and said

    s_llm, s_cpu, e_llm, e_cpu = site["llm"], site["cpu"], eff["llm"], eff["cpu"]
    llm = {"enabled": on("llm.enabled", s_llm["enabled"], e_llm["enabled"]),
           "slots": num("llm.slots", s_llm["slots"], e_llm["slots"])}
    if any(k not in s_cpu["work"] for k in e_cpu["work"]):
        over.append("cpu.work")
    cpu = {"enabled": on("cpu.enabled", s_cpu["enabled"], e_cpu["enabled"]),
           "threads": num("cpu.threads", s_cpu["threads"], e_cpu["threads"]),
           "cpu_percent": num("cpu.cpu_percent", s_cpu["cpu_percent"], e_cpu["cpu_percent"]),
           "memory_mb": num("cpu.memory_mb", s_cpu["memory_mb"], e_cpu["memory_mb"]),
           "work": [k for k in HP.KINDS_OF_WORK if k in s_cpu["work"] and k in e_cpu["work"]],
           "yield_to_llm": s_cpu["yield_to_llm"]}
    return {"llm": llm, "cpu": cpu}, [c for c in HP.CAPPED if c in over]


@functools.lru_cache(maxsize=1)
def server_block() -> dict:
    """``welcome.server``: this server's version, its engine build id (the extension's, as the helper's ``--version``
    prints its own) and its jobs build (none before package 4-07)."""
    from .. import __version__
    try:
        from .. import engine_api
        build = str(engine_api.build_info().get("build_id") or "")
    except Exception:
        build = ""
    if len(build) != 12:
        build = "0" * 12
    return {"version": str(__version__)[:64] or "unknown", "build": build, "jobs_build": None}


# ---------------------------------------------------------------------------- the rate limit before the lookup

class AuthLimiter:
    """The ``worker_auth`` limit (``citar.auth.ratelimit.RULES``), counted in **distinct** failing token hashes per
    address within the window (P4.5.1): a helper retrying one revoked token counts once and cannot lock its owner's
    other machines behind the same address out, while a brute force, whose tokens all differ, is counted in full. It
    is checked before any token lookup. In memory, bounded: at most ``MAX_ADDRESSES`` addresses, the oldest dropped."""

    MAX_ADDRESSES = 10000

    def __init__(self, clock: Optional[Clock] = None):
        from ..auth.ratelimit import RULES
        rule = RULES["worker_auth"]
        self.limit, self.window = rule.limit, float(rule.window)
        self.clock = clock or Clock()
        self._lock = threading.Lock()
        self._failures: dict = {}          # address -> {token hash: when it last failed}

    def _live(self, address: str, now: float) -> dict:
        seen = self._failures.get(address)
        if seen is None:
            return {}
        for h in [h for h, at in seen.items() if now - at >= self.window]:
            del seen[h]
        if not seen:
            self._failures.pop(address, None)
        return seen

    def retry_after(self, address: str) -> Optional[float]:
        """Seconds until the address may try again, or None when it may now."""
        with self._lock:
            now = self.clock.time()
            seen = self._live(address, now)
            if len(seen) < self.limit:
                return None
            # it may try again once enough failures have left the window to bring it under the limit
            times = sorted(seen.values())
            return max(1.0, round(times[len(seen) - self.limit] + self.window - now, 1))

    def failed(self, address: str, token_hash: str) -> None:
        with self._lock:
            now = self.clock.time()
            seen = self._live(address, now)
            if not seen:
                if len(self._failures) >= self.MAX_ADDRESSES:
                    self._failures.pop(next(iter(self._failures)))
                seen = self._failures[address] = {}
            seen[token_hash] = now

    def count(self, address: str) -> int:
        """The distinct failing tokens counted for an address now."""
        with self._lock:
            return len(self._live(address, self.clock.time()))

    def clear(self) -> None:
        with self._lock:
            self._failures.clear()


# ---------------------------------------------------------------------------- one connection

class SettingsSent:
    """A ``settings`` frame sent to a helper (the welcome's included) and not yet acknowledged: the site's settings it
    carried, which its acknowledgement is held to (``WorkerConnection.acknowledge``); ``settled`` is set when the
    acknowledgement came (``answered``) or the connection ended."""

    __slots__ = ("settings", "settled", "answered")

    def __init__(self, settings: dict):
        self.settings = settings
        self.settled = threading.Event()
        self.answered = False


class WorkerConnection:
    """One connected helper, serving one machine row: its hello, the site's settings it was sent, its acknowledgement,
    its models and quiet hours, and the requests in flight on it."""

    def __init__(self, websocket, server_id: str, hello: dict, loop, token_id: Optional[str] = None,
                 server_name: str = "", clock: Optional[Clock] = None, settings: Optional[dict] = None):
        self.ws = websocket
        self.server_id = server_id
        self.server_name = server_name
        #: The worker token it presented: revoking that one drops it (``WorkerHub.drop``).
        self.token_id = token_id
        self.loop = loop
        self.clock = clock or Clock()
        self.hello = hello
        helper, machine, llm = hello.get("helper") or {}, hello.get("machine") or {}, hello.get("llm") or {}
        self.helper_version = str(helper.get("version") or "")
        self.build = str(helper.get("build") or "")
        self.jobs_build = helper.get("jobs_build")
        self.target = str(helper.get("target") or "")
        self.hostname = str(machine.get("hostname") or "")
        self.public_key = str(machine.get("public_key") or "")
        self.provider = str(llm.get("provider") or "")
        self.hello_llm = {"enabled": bool(llm.get("enabled")), "slots": int(llm.get("slots") or 0)}
        self.models = list(llm.get("models") or [])
        #: When the helper's own quiet hours in force end (Unix seconds), from its hello and its ``status`` frames.
        self.quiet_until: Optional[float] = hello.get("quiet_until")
        self.status_frame: dict = {}
        #: The site's settings it was last sent, and its last acknowledgement as it sent it.
        self.settings: Optional[dict] = settings
        self.ack: Optional[dict] = None
        #: The ``settings`` frames sent and not yet acknowledged, oldest first: the helper answers them in order over
        #: its one socket, so each acknowledgement settles the oldest (``acknowledge``). Recorded and queued for
        #: sending under one lock, so this order is the order the frames go out in. Nothing else is done under it.
        self._unacked: collections.deque = collections.deque()
        self._settings_lock = threading.RLock()
        #: Held by the one caller waiting for an acknowledgement (``WorkerHub.push_settings``).
        self.ack_waiter = threading.Lock()
        self.connected_at = self.clock.time()
        self.last_seen = self.connected_at
        #: This connection's own number (unique in the process): a request's late answer comes back only over the
        #: socket it went out on, so the scheduler gives back what waits on a connection that is gone (P3.3.3).
        self.serial = next(_SERIALS)
        self.pending: dict = {}
        self.closed = False
        #: Why it closed, once it did (``fail_all``): (message, refusal, retryable). A request opened after that fails at
        #: once with the same error, rather than wait out its deadline for an answer nothing will send.
        self._failure: Optional[tuple] = None
        self._lock = threading.RLock()
        #: Whether the helper has its welcome: a frame sent before it waits here and follows it (``welcome``). Only the
        #: connection's loop touches these.
        self.welcomed = False
        self._early: list = []
        self._logged: set = set()
        #: When the hub's clock wrote this connection's last power row (``record_power``), or None.
        self.power_at: Optional[float] = None

    # -------------------------------------------------------------- state
    @property
    def alive(self) -> bool:
        """Whether this connection is still usable."""
        return not self.closed and (self.clock.time() - self.last_seen) < STALE_AFTER

    @property
    def in_flight(self) -> int:
        """How many requests are waiting on this helper."""
        with self._lock:
            return len(self.pending)

    def touch(self):
        """Record that the helper was heard from, for the keep-alive."""
        self.last_seen = self.clock.time()

    def effective(self) -> Optional[dict]:
        """What the helper will do, held to what the site asked (``clamp``): from its last acknowledgement, or, before
        one came, from its hello (its model slots, and no CPU work). None before the settings are known."""
        site = self.settings
        if site is None:
            return None
        if self.ack is not None:
            return clamp(site, self.ack)[0]
        llm = self.hello_llm
        return {"llm": {"enabled": site["llm"]["enabled"] and llm["enabled"],
                        "slots": min(site["llm"]["slots"], llm["slots"])},
                "cpu": {"enabled": False, "threads": 0, "cpu_percent": 0, "memory_mb": 0, "work": [],
                        "yield_to_llm": site["cpu"]["yield_to_llm"]}}

    @property
    def llm_on(self) -> bool:
        """Whether it serves completions (its hello's word until the settings are known)."""
        eff = self.effective()
        return bool(eff["llm"]["enabled"]) if eff is not None else bool(self.hello_llm["enabled"])

    @property
    def llm_slots(self) -> int:
        """The completions it takes at once, as the scheduler counts them: what it acknowledged, held to the site's
        figure (whether it serves any at all is ``llm_on``)."""
        eff = self.effective()
        if eff is None:
            return max(0, int(self.hello_llm["slots"]))
        return int(eff["llm"]["slots"])

    # -------------------------------------------------------------- the settings and their acknowledgements
    def _awaiting(self, settings: dict) -> SettingsSent:
        entry = SettingsSent(settings)
        self._unacked.append(entry)
        while len(self._unacked) > MAX_UNACKED_SETTINGS:
            self._unacked.popleft().settled.set()
        return entry

    def sent_in_welcome(self, settings: dict) -> SettingsSent:
        """Record the welcome's settings, the first the helper acknowledges. Call it before the connection is
        registered, so no push can be recorded ahead of it."""
        with self._settings_lock:
            self.settings = settings
            return self._awaiting(settings)

    def push_settings(self, settings: dict) -> tuple:
        """Send the helper the site's ``settings`` when they differ from the last it was sent. Returns the entry its
        acknowledgement settles (when nothing new went: the newest still unacknowledged, or None when the helper has
        acknowledged what the site asks) and whether a frame was sent. Safe from any thread, the loop's included."""
        with self._settings_lock:
            if self.closed:
                return None, False
            if settings == self.settings:
                return (self._unacked[-1] if self._unacked else None), False
            self.settings = settings
            entry = self._awaiting(settings)
            self.send_threadsafe(HP.settings(settings["llm"], settings["cpu"]))
            return entry, True

    def acknowledge(self, ack: dict) -> list:
        """Apply the helper's ``settings_ack``: it answers the oldest ``settings`` frame not yet answered, and is
        held to what that frame asked. Returns the fields where it claimed more than that (a correct helper never
        does). The scheduler reads the newest acknowledgement held to the site's newest settings (``effective``), and
        is told before a caller waiting for this acknowledgement wakes (``WorkerHub.push_settings``), so what that
        caller reads is in effect."""
        with self._settings_lock:
            entry = self._unacked.popleft() if self._unacked else None
            asked = entry.settings if entry is not None else self.settings
            self.ack = ack
        over = clamp(asked, ack)[1] if asked is not None else []
        _helper_changed(self.server_id)
        if entry is not None:
            entry.answered = True
            entry.settled.set()
        return over

    def _settle_unacked(self) -> None:
        with self._settings_lock:
            waiting, self._unacked = list(self._unacked), collections.deque()
        for entry in waiting:
            entry.settled.set()

    def model_keys(self) -> list:
        """The models this helper offers."""
        return [m.get("key") for m in self.models if m.get("key")]

    def key_fingerprint(self) -> str:
        return fingerprint(self.public_key)

    def client(self) -> dict:
        """The helper as the Servers page shows it."""
        eff = self.effective()
        limits = (self.ack or {}).get("limits")
        return {
            "server_id": self.server_id, "hostname": self.hostname, "provider": self.provider,
            "protocol": HP.PROTOCOL, "version": self.helper_version,
            "build": self.build, "jobs_build": self.jobs_build, "target": self.target,
            "key_fingerprint": self.key_fingerprint(), "models": self.models,
            "slots": self.llm_slots, "in_flight": self.in_flight,
            "effective": eff, "capped": list((self.ack or {}).get("capped") or []), "limits": limits,
            "quiet_until": self.quiet_until, "connected_at": self.connected_at, "last_seen": self.last_seen,
            "alive": self.alive, "uptime_seconds": round(self.clock.time() - self.connected_at),
        }

    def log_once(self, key: str, level: int, message: str, *args) -> None:
        """A line logged once for this connection (a frame type it is not served for, an overclaim)."""
        if key in self._logged or len(self._logged) > 256:
            return
        self._logged.add(key)
        log.log(level, message, *args)

    # -------------------------------------------------------------- sending
    async def send(self, text: str):
        """Send a frame to the helper; one sent before its welcome waits for it (``welcome``)."""
        if not self.welcomed:
            self._early.append(text)
            return
        await self.ws.send_text(text)

    async def welcome(self, text: str):
        """Send the welcome frame, then the frames sent before it, in the order they were sent (one sent while these
        go out joins them), and from then on send frames as they come."""
        await self.ws.send_text(text)
        while self._early:
            await self.ws.send_text(self._early.pop(0))
        self.welcomed = True

    def send_threadsafe(self, text: str):
        """Queue a frame from a non-async thread."""
        asyncio.run_coroutine_threadsafe(self.send(text), self.loop)

    # -------------------------------------------------------------- requests
    def open_request(self, request_id: str) -> Pending:
        """Register a request and return something to wait on. Raises WorkerError when the connection has closed
        meanwhile (``submit`` took it just before a drop or a disconnect failed everything in flight)."""
        with self._lock:
            if self.closed:
                message, refusal, retryable = self._failure or ("The helper disconnected.", "closed", True)
                raise WorkerError(message, refusal=refusal, retryable=retryable)
            pending = Pending(id=request_id)
            self.pending[request_id] = pending
            return pending

    def resolve(self, request_id: str, *, result: Optional[dict] = None, error: Optional[WorkerError] = None):
        """Complete a request with its result or its error. An answer for a request already given up (a ``cancelled``,
        or a ``done`` or ``error`` that crossed the cancel) is the request's late frame: the scheduler frees the slot it
        held for it (P4.5.3)."""
        with self._lock:
            pending = self.pending.pop(request_id, None)
        if pending is None:
            log.debug("helper %s answered request %s after it was given up", self.server_id, request_id)
            try:
                sched = _scheduler()
                if sched is not None and request_id:
                    sched.late_frame(self.server_id, request_id)
            except Exception:
                log.exception("could not hand a late frame to the scheduler")
            return
        pending.done_at = time.perf_counter()
        pending.result = result
        pending.error = error
        pending.event.set()

    def cancel(self, request_id: str) -> bool:
        """Give up a request in flight (non-blocking): its caller's wait ends with an ``unfinished`` error, and the
        helper is sent ``cancel``, which it acknowledges with ``cancelled`` once its endpoint's connection is closed.
        Returns whether it was still in flight."""
        with self._lock:
            pending = self.pending.pop(request_id, None)
        if pending is None:
            return False
        pending.error = WorkerError("The request was cancelled.", refusal="cancelled", retryable=False,
                                    unfinished=True, connection=self.serial)
        pending.event.set()
        try:
            self.send_threadsafe(HP.cancel(request_id))
        except Exception:
            pass
        return True

    def fail_all(self, message: str, *, retryable: bool = True):
        """Called when the connection drops: nothing in flight can ever be answered, and nothing more is taken
        (``closed``, set under the same lock ``open_request`` checks it under). ``retryable`` is false when the machine
        was taken away (``WorkerHub.drop``): waiting for it to come back would be waiting for nothing."""
        refusal = "closed" if retryable else "dropped"
        with self._lock:
            self.closed = True
            if self._failure is None:
                self._failure = (message, refusal, retryable)
            pending, self.pending = list(self.pending.values()), {}
        for item in pending:
            item.error = WorkerError(message, refusal=refusal, retryable=retryable)
            item.event.set()
        self._settle_unacked()              # nobody waits for an acknowledgement that cannot come


def fingerprint(public_key: str) -> str:
    """A machine key's fingerprint (P4.3.7), as ``citar-helper --version`` prints it; "" for no key or one that does
    not read."""
    if not public_key:
        return ""
    try:
        from .. import engine_api
        return engine_api.key_fingerprint(public_key)
    except Exception:
        return ""


# ---------------------------------------------------------------------------- the hub

class WorkerHub:
    """Every connected helper, and the routing between them and the game threads."""

    def __init__(self):
        self._workers: dict = {}          # server_id -> WorkerConnection
        self._lock = threading.RLock()
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self.clock: Clock = Clock()
        self.limiter = AuthLimiter(self)
        #: The keep-alive's interval, ``welcome.ping_s`` (``CITAR_WORKER_PING_SECONDS``).
        self.ping_s = 20
        self._ticks: dict = {}            # event loop -> the future the next poke resolves on it
        self._gates: dict = {}            # (event loop, address) -> [asyncio.Lock, how many hold or wait for it]
        self._v1_logged: dict = {}        # address -> when its refusal was last logged
        self._ignored: set = set()
        #: Test hooks' counters (the conformance player reads them): token lookups made.
        self.lookups = 0

    def time(self) -> float:
        """The hub's clock (``AuthLimiter`` reads it through the hub, so a test's clock swap reaches it)."""
        return self.clock.time()

    def attach_loop(self, loop):
        """Attach the event loop, so synchronous callers can schedule sends onto it."""
        self._loop = loop

    # -------------------------------------------------------------- waiting on the hub's clock
    def tick(self) -> asyncio.Future:
        """A future of the running loop that the next ``poke`` resolves: what a wait on the hub's clock waits on
        besides its own timeout, so a test's clock moved forward is seen at once and the real clock costs no polling."""
        loop = asyncio.get_running_loop()
        with self._lock:
            fut = self._ticks.get(loop)
            if fut is None or fut.done():
                fut = self._ticks[loop] = loop.create_future()
        return fut

    def poke(self) -> None:
        """Wake whatever waits on the hub's clock (the keep-alive, a connection's 30 s for its hello) to look at it now:
        a test's clock moved. Safe from any thread."""
        with self._lock:
            ticks, self._ticks = list(self._ticks.items()), {}
        for loop, fut in ticks:
            try:
                loop.call_soon_threadsafe(_settle, fut)
            except RuntimeError:            # that loop is closed: so is everything that waited on it
                pass

    async def nap(self, seconds: float) -> None:
        """Sleep up to ``seconds`` of real time (at most ``NAP_SECONDS``), or until poked."""
        await asyncio.wait({self.tick()}, timeout=max(0.0, min(seconds, NAP_SECONDS)))

    @contextlib.asynccontextmanager
    async def gate(self, address: str):
        """One hello at a time per address through the rate limit's check, the token's lookup and the count of its
        failure: otherwise hellos sent together would all pass the check before the first failure was counted, and a
        brute force would get as many lookups as it opens connections. A lookup takes milliseconds, so helpers behind
        one address barely wait; an address over the limit is answered from memory, without a lookup, once through."""
        key = (asyncio.get_running_loop(), address)
        with self._lock:
            entry = self._gates.get(key)
            if entry is None:
                entry = self._gates[key] = [asyncio.Lock(), 0]
            entry[1] += 1
        try:
            async with entry[0]:
                yield
        finally:
            with self._lock:
                entry[1] -= 1
                if entry[1] == 0 and self._gates.get(key) is entry:
                    del self._gates[key]

    # -------------------------------------------------------------- registry
    def register(self, connection: WorkerConnection) -> Optional[WorkerConnection]:
        """Add a helper, returning any previous one of the same machine, which is closed with ``shutdown``
        ``replaced``: the newest connection wins, so a reconnect after a network drop, which arrives before the old
        socket has gone stale, is never refused as a duplicate."""
        with self._lock:
            previous = self._workers.get(connection.server_id)
            self._workers[connection.server_id] = connection
        if previous is not None:
            previous.fail_all("Replaced by a newer connection of the same machine.")
        _helper_changed(connection.server_id)
        return previous

    async def admit(self, connection: WorkerConnection) -> Optional[tuple]:
        """Register a helper whose hello ``authenticate`` let in, then check its token, machine and owner again
        (``still_allowed``): a revoke, a disable, a delete or a suspension committed between that check and the
        registration found nothing to drop (``drop`` runs after its commit), and this second look, made once the
        connection can be found, sees it. Returns ``(code, reason)`` when the helper may not serve (it is dropped, its
        socket left to the caller to close), or None. A previous connection of the machine is told ``shutdown``
        ``replaced`` and closed with 4409."""
        previous = self.register(connection)
        if previous is not None and previous.ws is not None:
            try:
                await previous.ws.send_text(HP.shutdown("replaced", "Another helper connected with this machine's "
                                                                     "token."))
            except Exception:
                pass
            try:
                await previous.ws.close(code=HP.CLOSE_REPLACED)
            except Exception:
                pass
        try:
            refused = await asyncio.to_thread(still_allowed, [(connection.server_id, connection.token_id)])
        except Exception:
            log.exception("could not check the new helper of %s again", connection.server_id)
            refused = {}
        found = next(iter(refused.values()), None)
        if found:
            self.drop(connection.server_id, found[1], code=found[0], token_id=connection.token_id, close=False)
        return found

    def unregister(self, connection: WorkerConnection):
        """Forget a helper that has disconnected, failing anything still waiting on it."""
        with self._lock:
            if self._workers.get(connection.server_id) is connection:
                del self._workers[connection.server_id]
        connection.fail_all("The helper disconnected.")
        _helper_changed(connection.server_id)

    def models_changed(self, connection: WorkerConnection):
        """A helper reported its models again (a catalog frame): the scheduler's loaded-model rule reads them."""
        _helper_changed(connection.server_id)

    def get(self, server_id: str) -> Optional[WorkerConnection]:
        """The connection for a server id, if it is online."""
        with self._lock:
            connection = self._workers.get(server_id)
        return connection if (connection and connection.alive) else None

    def is_online(self, server_id: str) -> bool:
        """Whether a server has a helper connected."""
        return self.get(server_id) is not None

    def in_flight(self, server_id: str) -> int:
        """How many requests are in flight to a server."""
        connection = self.get(server_id)
        return connection.in_flight if connection else 0

    def drop(self, server_id: str, reason: str, *, code: str = "", token_id: Optional[str] = None,
             close: bool = True) -> bool:
        """Disconnect a machine's helper because it may no longer serve: its token revoked (``token_id``: only a
        connection made with that token), its machine disabled or deleted, its owner suspended. Pending requests fail
        as not retryable, the helper is told why (``shutdown`` with ``code``, which stops it rather than have it
        reconnect into a refusal) and its socket is closed with 4403 (``close`` false: the caller, the socket's own
        handler, does that). Call it after the change that took the machine away is committed. Returns whether a
        connection was dropped."""
        with self._lock:
            connection = self._workers.get(server_id)
            if connection is None or (token_id is not None and connection.token_id != token_id):
                return False
            del self._workers[server_id]
        connection.fail_all(f"The machine's helper was disconnected: {reason}", retryable=False)
        _helper_changed(server_id)
        log.info("dropped the helper of %s: %s", server_id, reason)
        code = code or _code_of(reason)
        if close and connection.loop is not None and connection.ws is not None:
            async def close_it():
                try:
                    await connection.ws.send_text(HP.shutdown(code, reason))
                except Exception:
                    pass
                try:
                    await connection.ws.close(code=HP.CLOSE_SHUTDOWN)
                except Exception:
                    pass
            try:
                asyncio.run_coroutine_threadsafe(close_it(), connection.loop)
            except RuntimeError:            # the loop is gone: so is the socket
                pass
        return True

    def drop_many(self, server_ids, reason: str, *, code: str = "") -> int:
        """``drop`` each machine of ``server_ids`` (an account's, when it is suspended); returns how many were."""
        return sum(1 for sid in server_ids if self.drop(sid, reason, code=code))

    def online_ids(self) -> set:
        """The ids of every connected server."""
        with self._lock:
            return {sid for sid, c in self._workers.items() if c.alive}

    def status(self) -> list:
        """Every connection, for the Servers page."""
        with self._lock:
            connections = list(self._workers.values())
        return [c.client() for c in connections]

    def effective(self, server_id: str) -> Optional[dict]:
        """What a connected machine's helper will do, held to the site's settings (``WorkerConnection.effective``)."""
        connection = self.get(server_id)
        return connection.effective() if connection else None

    # -------------------------------------------------------------- pushes to a helper
    def push_settings(self, server_id: str, wait: float = 0.0) -> bool:
        """Send a machine's helper the site's settings as they are now, when they differ from what it was last sent:
        after any committed change of the machine's slots, capabilities or switch (the settings route, the machine
        editor's routes, the keep-alive's look for a change another process made). With ``wait``, wait up to that many
        seconds for its acknowledgement, so the caller reads what is in effect; only one caller waits on a machine at a
        time (another returns at once), so a helper that never acknowledges holds at most one thread a machine. Reads
        the registry's cache and may wait: never on the event loop. Returns whether the helper has acknowledged what
        the site asks (False with no helper connected)."""
        connection = self.get(server_id)
        if connection is None:
            return False
        try:
            entry, sent = connection.push_settings(site_settings(server_id))
        except Exception:
            log.exception("could not push settings to %s's helper", server_id)
            return False
        if sent:
            _helper_changed(server_id)      # the capacity is held to the new figures until they are acknowledged
        if connection.closed:
            return False
        if entry is None:
            return True
        if wait > 0 and not entry.settled.is_set() and connection.ack_waiter.acquire(blocking=False):
            try:
                entry.settled.wait(wait)
            finally:
                connection.ack_waiter.release()
        return entry.answered

    def disconnect(self, server_id: str, reason: str) -> bool:
        """Close a machine's helper connection so that the helper connects again at once: close 1012 with no
        ``shutdown`` (PROTOCOL.md: a reconnect with backoff), what is in flight failed as worth retrying. After its
        machine key was reset (``machines_api.key_reset``, once committed), so the next hello registers its key at once
        instead of a connection serving under a key the row no longer holds. Returns whether a connection was closed."""
        with self._lock:
            connection = self._workers.pop(server_id, None)
        if connection is None:
            return False
        connection.fail_all(f"The machine's helper is reconnecting: {reason}")
        _helper_changed(server_id)
        log.info("closed the helper of %s to have it reconnect: %s", server_id, reason)
        if connection.loop is not None and connection.ws is not None:
            async def close_it():
                try:
                    await connection.ws.close(code=HP.CLOSE_RECONNECT, reason="key_reset")
                except Exception:
                    pass
            try:
                asyncio.run_coroutine_threadsafe(close_it(), connection.loop)
            except RuntimeError:            # the loop is gone: so is the socket
                pass
        return True

    def _sync_settings(self, connections: list) -> None:
        """Push to each connection whatever the site now asks that differs from what it was last sent (a change made
        by ``admin_cli`` or another process, which no route here pushed). In a thread: the registry may be read."""
        for connection in connections:
            if connection.closed:
                continue
            try:
                entry, sent = connection.push_settings(site_settings(connection.server_id))
            except Exception:
                log.exception("could not push settings to %s's helper", connection.server_id)
                continue
            if sent:
                log.info("the settings of %s changed elsewhere: pushed to its helper", connection.server_id)
                _helper_changed(connection.server_id)

    def rescan(self, server_id: str) -> bool:
        """Ask a machine's helper to scan its hardware now (the dialog's "Rescan now"). Returns whether a helper was
        there to ask."""
        connection = self.get(server_id)
        if connection is None:
            return False
        connection.send_threadsafe(HP.rescan())
        return True

    # -------------------------------------------------------------- the bridge
    def submit(self, server_id: str, request_id: str, model: str, messages: list, tools: Optional[list],
               params: dict, timeout: float, *, check_slots: bool = True) -> dict:
        """Send a completion to a helper and wait for its ``done``. Blocking; call from a seat's thread, never from
        the event loop. The answer is the ``done`` frame's fields with ``observed_s``: the seconds from sending the
        request to receiving its answer, the bound on the generation time the scheduler credits (P3.3.4). The slot
        check is a safety net only (``check_slots`` false: a test's request past it, for the helper's own refusal):
        the scheduler counts the helper's acknowledged slots and sends nothing a helper has no slot for."""
        connection = self.get(server_id)
        if connection is None:
            raise WorkerError("That machine's helper is not connected. Start the CITAR helper (citar-helper) on it.",
                              refusal="closed", retryable=True)
        if check_slots and connection.in_flight >= max(1, connection.llm_slots):
            raise WorkerError(f"{connection.hostname or server_id} is busy "
                              f"({connection.in_flight}/{connection.llm_slots} in flight).",
                              refusal="busy", retryable=True)
        timeout = max(1.0, min(86400.0, float(timeout)))
        frame = HP.request(request_id, model, messages, tools, params, timeout)
        pending = connection.open_request(request_id)
        try:
            pending.sent = time.perf_counter()
            connection.send_threadsafe(frame)
        except Exception as exc:
            connection.resolve(request_id)
            raise WorkerError(f"Could not reach the helper: {exc}", refusal="closed", retryable=True) from exc

        # A little grace beyond the helper's own deadline, so its ``timeout``, which says what went wrong, normally
        # comes first. When this fires the request is given up as a cancel is (``unfinished``): the helper is told to
        # stop, and the scheduler holds its slot until the helper's answer (P4.5.3).
        if not pending.event.wait(timeout + 15):
            if connection.cancel(request_id):
                raise WorkerError(f"The helper did not answer within {timeout:.0f}s.", refusal="timeout",
                                  retryable=True, unfinished=True, connection=connection.serial)

        if pending.error is not None:
            raise pending.error
        result = dict(pending.result or {})
        if pending.done_at is not None:
            result["observed_s"] = round(max(0.0, pending.done_at - pending.sent), 3)
        return result

    def cancel(self, server_id: str, request_id: str) -> bool:
        """Give up a request in flight on a machine's helper (``WorkerConversation.close``; non-blocking): its caller
        stops waiting and the helper is sent ``cancel``. Returns whether it was still in flight."""
        with self._lock:
            connection = self._workers.get(server_id)
        return connection.cancel(request_id) if connection is not None else False

    def broadcast(self, frame_text: str):
        """Send a frame to every connected helper."""
        with self._lock:
            connections = list(self._workers.values())
        for connection in connections:
            try:
                connection.send_threadsafe(frame_text)
            except Exception:
                pass

    # -------------------------------------------------------------- the keep-alive
    async def ping_loop(self):
        """Keep connections warm and notice dead ones, and ones that may no longer serve: every ``ping_s`` on the
        hub's clock, a ``ping`` to each helper, a 4408 close for one silent for ``STALE_AFTER``, and every
        connection's token, machine and owner checked again (``still_allowed``), so a change made where this process
        could not drop the helper at once (``admin_cli``, another server process) takes effect within a ping.
        Survives its own exceptions."""
        interval = self.ping_s
        next_at = self.clock.time() + interval
        while True:
            try:
                now = self.clock.time()
                if self.ping_s != interval or next_at - now > interval:
                    # a new interval, or a clock that went back (a test's swapped, the wall clock set): count again
                    interval = self.ping_s
                    next_at = now + interval
                if now < next_at:
                    await self.nap(next_at - now)
                    continue
                next_at = now + interval
                await self._ping_round()
            except asyncio.CancelledError:
                raise
            except Exception:
                log.exception("the helpers' keep-alive failed; it carries on")
                await asyncio.sleep(NAP_SECONDS)

    async def _ping_round(self):
        with self._lock:
            connections = list(self._workers.values())
        try:
            refused = await asyncio.to_thread(still_allowed, [(c.server_id, c.token_id) for c in connections])
        except Exception:
            log.exception("could not check the helpers' tokens again")
            refused = {}
        for (server_id, token_id), (code, reason) in refused.items():
            self.drop(server_id, reason, code=code, token_id=token_id)
        try:
            await asyncio.to_thread(self._sync_settings, [c for c in connections if not c.closed])
        except Exception:
            log.exception("could not look for settings changed elsewhere")
        for connection in connections:
            if connection.closed:
                continue
            if not connection.alive:
                log.info("the helper of %s went stale, dropping it", connection.server_id)
                self.unregister(connection)
                try:
                    await connection.ws.close(code=HP.CLOSE_STALE)
                except Exception:
                    pass
                continue
            try:
                await connection.send(HP.ping())
            except Exception:
                self.unregister(connection)

    # -------------------------------------------------------------- logging that cannot flood
    def log_v1(self, address: str) -> None:
        """At most one log line per address an hour for a protocol 1 helper's refusal (a 0.1.5 system unit retries
        every 15 s until it is disabled, P4.2.8)."""
        now = self.clock.time()
        last = self._v1_logged.get(address)
        if last is not None and now - last < V1_LOG_SECONDS:
            return
        if len(self._v1_logged) >= 4096:
            for a in [a for a, at in self._v1_logged.items() if now - at >= V1_LOG_SECONDS] or \
                    list(self._v1_logged)[:1024]:
                del self._v1_logged[a]
        self._v1_logged[address] = now
        log.warning("refused a protocol 1 (0.1.5) helper from %s: it must be replaced by the CITAR helper "
                    "(citar-helper); a 0.1.5 system unit retries every 15 s until it is disabled", address)


_hub: Optional[WorkerHub] = None
_hub_lock = threading.Lock()


def hub() -> WorkerHub:
    """The hub, created on first use."""
    global _hub
    with _hub_lock:
        if _hub is None:
            _hub = WorkerHub()
        return _hub


# ---------------------------------------------------------------------------- the socket

def _bad_frame_id(raw: str) -> tuple:
    """The type and the request id of a frame that failed its checks, when they can be read (PROTOCOL.md: a bad
    ``done``, ``error`` or ``cancelled`` whose ``id`` reads fails its request)."""
    try:
        data, t = HP.envelope(raw)
    except HP.ProtocolError:
        return "?", None
    rid = data.get("id")
    try:
        rid = HP.ID.read(rid, ("id",)) if isinstance(rid, str) else None
    except HP.ProtocolError:
        rid = None
    return t[:64], rid


#: The checks a frame fails before its type and id can be read (``HP.envelope``'s): no id to fail.
_ENVELOPE_CODES = frozenset(("too_large", "too_deep", "not_json", "not_object", "no_type"))


def _settle(fut) -> None:
    if not fut.done():
        fut.set_result(None)


def _read(raw: str) -> tuple:
    """A helper's frame parsed: ``(frame, None)``, or ``(None, (error, type, request id))`` when it failed its checks
    (``_bad_frame_id``). One call, so a long frame is read on one thread, once or (failing a field) twice."""
    try:
        return HP.parse_helper_frame(raw), None
    except HP.ProtocolError as exc:
        if exc.code in _ENVELOPE_CODES:
            return None, (exc, "?", None)
        return None, (exc, *_bad_frame_id(raw))


async def _parse(raw: str, *, first: bool = False) -> tuple:
    """``_read`` without holding the event loop: on the loop for a short frame, on a thread for a long one (a first
    frame on ``_HELLO_PARSER``)."""
    if len(raw) <= PARSE_INLINE_CHARS:
        return _read(raw)
    if first:
        return await asyncio.get_running_loop().run_in_executor(_HELLO_PARSER, _read, raw)
    return await asyncio.to_thread(_read, raw)


async def _first_frame(websocket, the_hub: WorkerHub) -> Optional[str]:
    """The connection's first frame, or None when none came within ``HELLO_SECONDS`` on the hub's clock. It waits on
    the frame and on the hub's ``tick`` (a test's clock moved), never polls: a socket that sends nothing costs nothing
    until its time is up."""
    recv = asyncio.ensure_future(websocket.receive_text())
    deadline = the_hub.clock.time() + HELLO_SECONDS
    try:
        while not recv.done():
            left = deadline - the_hub.clock.time()
            if left <= 0:
                return None
            await asyncio.wait({recv, the_hub.tick()}, timeout=left, return_when=asyncio.FIRST_COMPLETED)
        return recv.result()
    finally:
        if not recv.done():
            recv.cancel()


async def serve(websocket) -> None:
    """``/ws/worker``: one helper's connection, from its hello to its end (the module's doc has the order)."""
    from ..auth import ratelimit, tokens
    the_hub = hub()
    await websocket.accept()
    address = ratelimit.client_ip(websocket)
    connection = None
    try:
        raw = await _first_frame(websocket, the_hub)
        if raw is None:
            await websocket.close(code=HP.CLOSE_STALE)
            return
        if HP.hello_too_large(raw):
            # (0) longer than any hello a helper sends: refused unread, before anything costs more than its length
            await websocket.close(code=HP.CLOSE_BAD_HELLO, reason="too_large")
            return
        hello, bad = await _parse(raw, first=True)
        if bad is not None:
            exc = bad[0]
            if exc.code == "protocol":
                # (1) not protocol 2: the one refusal, which costs nothing and reaches a 0.1.5 helper in the one
                # shape it reads (P4.2.8)
                the_hub.log_v1(address)
                await websocket.send_text(HP.v1_refusal())
                await websocket.close(code=HP.CLOSE_PROTOCOL)
                return
            await websocket.close(code=HP.CLOSE_BAD_HELLO, reason=HP.close_reason(exc))
            return
        if isinstance(hello, HP.Ignored) or hello.get("t") != "hello":
            await websocket.close(code=HP.CLOSE_BAD_HELLO, reason="not_hello")
            return

        # (2) the address's limit, before any lookup, (3) the token, (4) the key: one hello of an address at a time
        wait, found = None, None
        async with the_hub.gate(address):
            wait = the_hub.limiter.retry_after(address)
            if wait is None:
                found = await asyncio.to_thread(authenticate, hello["token"], hello["machine"]["public_key"], address)
                if found.refusal == "token":
                    the_hub.limiter.failed(address, tokens.hash_token(hello["token"]))
        if wait is not None:
            await websocket.send_text(HP.refused("rate", "Too many refused worker tokens from this address. "
                                                 "Try again later.", retry_after_s=wait))
            await websocket.close(code=HP.CLOSE_RATE)
            return
        if found.refusal == "error":
            # the database failed, not the helper: no refusal (which would stop it for good), no count, no audit row;
            # 1011 has it connect again with backoff (PROTOCOL.md, "Close codes")
            await websocket.close(code=HP.CLOSE_SERVER_ERROR)
            return
        if found.refusal == "token":
            await asyncio.to_thread(_auth_failed, address)
            await websocket.send_text(HP.refused("token", "That worker token was refused: it is unknown or revoked, "
                                                 "or its machine is disabled or deleted, or its owner's account is "
                                                 "not active. Issue a new token on the Servers page and run "
                                                 "citar-helper setup."))
            await websocket.close(code=HP.CLOSE_REFUSED)
            return
        if found.refusal == "key":
            await websocket.send_text(HP.refused("key", "This machine already has another helper's key registered. "
                                                 "If this is your machine, reset its key on the Servers page, then "
                                                 "start the helper again."))
            await websocket.close(code=HP.CLOSE_REFUSED)
            return

        # (5) registration, then the welcome
        loop = asyncio.get_running_loop()
        settings = await asyncio.to_thread(site_settings, found.server_id)
        connection = WorkerConnection(websocket, found.server_id, hello, loop, token_id=found.token_id,
                                      server_name=found.server_name, clock=the_hub.clock)
        connection.sent_in_welcome(settings)
        refused = await the_hub.admit(connection)
        if refused:
            # taken away between the hello's check and the registration: told why, as a drop tells a helper
            await websocket.send_text(HP.shutdown(refused[0], refused[1]))
            await websocket.close(code=HP.CLOSE_SHUTDOWN)
            return
        # a change committed between the settings' read and the registration found no connection to push to: it goes
        # now, right after the welcome
        if connection.push_settings(await asyncio.to_thread(site_settings, found.server_id))[1]:
            _helper_changed(found.server_id)
        # no run is offered to a helper before package 4-10, so none it still plays is kept, and no held result wanted
        welcome = HP.welcome(found.server_id, found.server_name[:120], settings, max(5, min(300, int(the_hub.ping_s))),
                             await asyncio.to_thread(server_block), None, [], list(hello.get("running") or []), [])
        # what was sent to it since it was registered (a request waiting for the machine) follows the welcome
        await connection.welcome(welcome)
        await asyncio.to_thread(record_hello, found.server_id, hello)
        log.info("helper connected: %s (%s, %d models, helper %s)", found.server_name, connection.hostname,
                 len(connection.models), connection.helper_version)

        while True:
            raw = await websocket.receive_text()
            connection.touch()
            await _frame(the_hub, connection, raw)
    except Exception as exc:
        from starlette.websockets import WebSocketDisconnect, WebSocketState
        if isinstance(exc, WebSocketDisconnect):
            pass
        elif not (connection is not None and connection.closed
                  and websocket.application_state != WebSocketState.CONNECTED):
            log.exception("a helper's connection failed")
        # else the hub dropped the helper (a revoked token, a disabled machine) and closed its socket before this
        # loop's next receive: an ordinary end, not an error
    finally:
        if connection is not None:
            the_hub.unregister(connection)
            log.info("helper disconnected: %s", connection.server_id)


async def _frame(the_hub: WorkerHub, connection: WorkerConnection, raw: str) -> None:
    """One frame from a welcomed helper. Never blocks the loop: what writes the database goes to a thread, and so
    does the parse of a long frame."""
    data, bad = await _parse(raw)
    if bad is not None:
        exc, t, rid = bad
        connection.log_once(f"bad:{t}", logging.WARNING, "helper of %s sent a bad %s frame: %s",
                            connection.server_id, t, exc)
        if t in ("done", "error", "cancelled") and rid:
            # its request fails as the endpoint's failure, not retried, and its slot is freed (PROTOCOL.md)
            connection.resolve(rid, error=WorkerError(f"The helper's answer did not read ({exc.code} at "
                                                      f"{exc.path or 'the frame'}).", refusal="endpoint",
                                                      retryable=False))
        return
    if isinstance(data, HP.Ignored):
        connection.log_once(f"ignored:{data.t[:64]}", logging.DEBUG, "ignored a %s frame from the helper of %s",
                            data.t[:64], connection.server_id)
        return
    t = data["t"]
    sid = connection.server_id
    if t == "done":
        connection.resolve(data["id"], result=data)
    elif t == "error":
        connection.resolve(data["id"], error=WorkerError(data["message"] or "The helper reported an error.",
                                                         refusal=data["code"], retryable=bool(data["retryable"])))
    elif t == "cancelled":
        # the acknowledgement of a cancel: the request was given up already, so this is its late frame, which frees its
        # slot; one for a request still awaited (no cancel was sent: a helper misbehaving) fails it
        connection.resolve(data["id"], error=WorkerError("The helper stopped the request.", refusal="cancelled",
                                                         retryable=False))
    elif t == "settings_ack":
        # held to the settings frame it answers, not to newer ones it has not seen yet: an acknowledgement that
        # crossed a push is not an overclaim
        over = connection.acknowledge(data)
        if over:
            connection.log_once("overclaim", logging.WARNING, "the helper of %s acknowledged more than the site "
                                "asked (%s): held to the site's settings", sid, ", ".join(over))
            await asyncio.to_thread(flag_machine, sid, "settings_overclaim", {"fields": over})
        await asyncio.to_thread(record_ack, sid, data)
    elif t == "catalog":
        connection.models = list(data["models"])
        the_hub.models_changed(connection)
        await asyncio.to_thread(record_report, sid, models=connection.models)
    elif t == "hardware":
        await asyncio.to_thread(record_report, sid, hardware=data["scan"])
    elif t == "power":
        now = connection.clock.time()
        last = connection.power_at
        if last is not None and now - last < POWER_MIN_SECONDS:
            connection.log_once("power:fast", logging.WARNING, "the helper of %s sent power samples %.0f s after its "
                                "last ones: dropped (one frame a minute)", sid, now - last)
            return
        connection.power_at = now
        since = last if last is not None else connection.connected_at
        await asyncio.to_thread(record_power, sid, data["samples"], now, since)
    elif t == "status":
        connection.status_frame = data
        connection.quiet_until = data["quiet_until"]
        _helper_changed(sid)
    elif t == "ping":
        await connection.send(HP.pong())
    elif t == "pong":
        pass
    elif t == "hello":
        connection.log_once("hello", logging.WARNING, "the helper of %s sent a second hello: ignored", sid)
    elif t in NOT_YET:
        connection.log_once(f"later:{t}", logging.DEBUG, "the helper of %s sent %s, which this server does not act "
                            "on yet", sid, t)


# ---------------------------------------------------------------------------- authentication

_CODES = {"its worker token was revoked": "revoked", "its machine was deleted": "deleted",
          "its machine was disabled": "disabled", "its owner's account is not active": "suspended",
          "its owner's account was suspended": "suspended"}


def _code_of(reason: str) -> str:
    """The ``shutdown`` code of a drop's reason (the callers' words)."""
    return _CODES.get(reason, "revoked")


def _refusal(s, token_row, server) -> Optional[tuple]:
    """Why a token may not let a helper in, as ``(shutdown code, reason)``, or None: revoked, its machine gone or
    switched off, the machine's owner out of good standing (a suspended account's machine serves nobody)."""
    from ..db.models import User
    if token_row is None or token_row.revoked_at is not None:
        return "revoked", "its worker token was revoked"
    if server is None:
        return "deleted", "its machine was deleted"
    if not server.enabled:
        return "disabled", "its machine was disabled"
    owner = s.get(User, server.owner_id) if server.owner_id else None
    if owner is not None and owner.status in ("suspended", "deleted", "pending"):
        return "suspended", "its owner's account is not active"
    return None


def still_allowed(pairs) -> dict:
    """``{(server_id, token_id): (code, reason)}`` for the connected helpers whose token, machine or owner no longer
    lets them in (``pairs`` is (server id, token id) per connection); the others are left out."""
    from .. import db
    from ..db.models import Server, WorkerToken
    out: dict = {}
    if not pairs:
        return out
    with db.session() as s:
        for server_id, token_id in pairs:
            server = s.get(Server, server_id)
            token = s.get(WorkerToken, token_id) if token_id else None
            if token is not None and token.server_id != server_id:
                token = None
            if token_id:
                found = _refusal(s, token, server)
            else:           # a connection made without a token row (a test's): only its machine is checked
                found = None if server is not None and server.enabled else ("disabled", "its machine was disabled "
                                                                                       "or deleted")
            if found:
                out[(server_id, token_id)] = found
    return out


@dataclass
class Identity:
    """What ``authenticate`` found: the machine, or ``refusal`` (``token`` or ``key``; ``error`` when the database
    failed, which is not the helper's fault and refuses nothing)."""
    refusal: str = ""
    server_id: str = ""
    server_name: str = ""
    token_id: str = ""


def authenticate(token: str, public_key: str, address: str = "") -> Identity:
    """Resolve a worker token and a machine key (P4.5.1, P4.3.7). The token: only its hash is stored, so a leaked
    database yields no usable worker credential, and a revoked or unknown token is indistinguishable from the outside,
    as is one whose machine is disabled or whose owner is suspended (``refusal`` ``token``). The key: the first
    protocol 2 hello of a machine registers its key on the row (trust on first use, bound to a valid token); another
    key is ``refusal`` ``key`` until the owner or an administrator resets it. The registration is a conditional write
    (only while the row has no key), so of two first hellos at once with different keys one registers and the other is
    refused ``key``. On success the token's ``last_seen_at`` and ``last_ip`` are written. A database that fails (locked,
    a snapshot gone stale under a concurrent registration) is ``refusal`` ``error``: the helper is told nothing it
    would take as permanent, and nothing is counted against its address."""
    from datetime import datetime, timezone

    from sqlalchemy import select, update

    from .. import db
    from ..auth import tokens
    from ..db.models import Server, WorkerToken

    hub().lookups += 1
    if not token:
        return Identity("token")
    try:
        with db.session() as s:
            row = s.scalar(select(WorkerToken).where(WorkerToken.token_hash == tokens.hash_token(token)))
            server = s.get(Server, row.server_id) if row is not None else None
            if _refusal(s, row, server) is not None:
                return Identity("token")
            now = datetime.now(timezone.utc)
            if not server.public_key:
                registered = s.execute(
                    update(Server).where(Server.id == server.id, Server.public_key.is_(None))
                    .values(public_key=public_key, key_at=now).execution_options(synchronize_session=False)).rowcount
                s.refresh(server, ["public_key", "key_at"])     # this write, or the one that won
                if registered:
                    log.info("machine %s registered its helper's key %s", server.id, fingerprint(public_key))
            if server.public_key != public_key:
                return Identity("key", server.id, server.name, row.id)
            row.last_seen_at = now
            row.last_ip = (address or "")[:64] or None
            return Identity("", server.id, server.name, row.id)
    except Exception:
        log.exception("helper authentication failed (the database, not the helper)")
        return Identity("error")


def _auth_failed(address: str) -> None:
    """Audit a refused worker token (the address's limit counts it in memory, ``AuthLimiter``)."""
    from .. import db
    from ..auth import audit
    try:
        with db.session() as s:
            audit.record(s, "worker.auth_failed", actor_label="worker", ip=address)
    except Exception:
        log.debug("could not audit a refused worker token", exc_info=True)


# ---------------------------------------------------------------------------- what a helper reports

def record_hello(server_id: str, hello: dict) -> None:
    """Keep the hello on the machine's row (off the event loop; never raises): ``servers.helper`` gets the helper's
    block (its version, builds, target and label, its machine's name, the key's fingerprint, its model and CPU
    capabilities, its update policy), and its hardware scan and models are its last report, applied as the row's
    modes say (``record_report``)."""
    from datetime import datetime, timezone

    from .. import db
    from ..db.models import Server
    helper, machine, llm, cpu = (hello.get(k) or {} for k in ("helper", "machine", "llm", "cpu"))
    block = {"protocol": HP.PROTOCOL, "version": helper.get("version"), "build": helper.get("build"),
             "jobs_build": helper.get("jobs_build"), "target": helper.get("target"), "label": helper.get("label"),
             "hostname": machine.get("hostname"), "name": machine.get("name"), "os": machine.get("os"),
             "arch": machine.get("arch"), "key_fingerprint": fingerprint(str(machine.get("public_key") or "")),
             "llm": {k: llm.get(k) for k in ("enabled", "provider", "slots", "stream")},
             "cpu": {k: cpu.get(k) for k in ("enabled", "ceiling", "limits", "verifiable", "bots", "ruleset")},
             "updates": hello.get("updates"), "connected_at": datetime.now(timezone.utc).isoformat()}
    try:
        with db.session() as s:
            row = s.get(Server, server_id)
            if row is None:
                return
            old = dict(row.helper) if isinstance(row.helper, dict) else {}
            row.helper = {**block, **{k: old[k] for k in ("effective", "capped", "limits") if k in old}}
    except Exception:
        log.exception("could not keep the hello of %s's helper", server_id)
    report = hello.get("report") or {}
    record_report(server_id, hardware=report.get("hardware"), models=llm.get("models"),
                  helper={"hostname": machine.get("hostname"), "version": helper.get("version")})


def record_ack(server_id: str, ack: dict) -> None:
    """Keep the helper's last acknowledgement on its row (``servers.helper``: what it acknowledged, where its ceiling
    won, how hard its OS holds the limits). Off the event loop; never raises."""
    from .. import db
    from ..db.models import Server
    try:
        with db.session() as s:
            row = s.get(Server, server_id)
            if row is None:
                return
            block = dict(row.helper) if isinstance(row.helper, dict) else {}
            block.update(effective=ack.get("effective"), capped=ack.get("capped"), limits=ack.get("limits"))
            row.helper = block
    except Exception:
        log.exception("could not keep the settings acknowledgement of %s's helper", server_id)


def record_report(server_id: str, *, hardware: Optional[dict] = None, models: Optional[list] = None,
                  helper: Optional[dict] = None) -> dict:
    """Keep what a machine's helper reported (its hello's hardware scan and models, a ``catalog``'s models, a
    ``hardware`` frame's scan) as the machine's last report, and apply it as the machine's modes say
    (``citar.machine_settings.record_report``, P3.8.3). Off the event loop; never raises. Returns what was kept and
    applied (``{"kept", "applied"}``)."""
    from .. import db
    from ..db.models import Server
    from ..machine_settings import record_report as keep

    try:
        with db.session() as s:
            server = s.get(Server, server_id)
            if server is None:
                return {"kept": [], "applied": []}
            return keep(server, hardware=hardware, models=models, helper=helper)
    except Exception:
        log.exception("could not keep the report of %s's helper", server_id)
        return {"kept": [], "applied": []}


def power_row(server_id: str, samples: list, now: float, since: float) -> Optional[dict]:
    """One power row (``usage.PowerSampler``'s format: ``{"k": "pw", "srv", "t0", "t1", "n", "gpu_w", "cpu_util"}``)
    from a helper's ``power`` frame that came at ``now`` on the hub's clock: the means of the GPUs' draw and of the CPU's
    utilisation over the samples that read (a draw over ``POWER_W_MAX`` does not), covering the
    ``POWER_SAMPLE_SECONDS`` each sample took before ``now``, and never before ``since`` (the connection's start or its
    last row), so a helper's rows never overlap and never reach back before it was connected. The helper's own times
    are not used: its clock is not the server's. None when no sample reads."""
    gpu = [x["gpu_w"] for x in samples if x.get("gpu_w") is not None and 0 <= x["gpu_w"] <= POWER_W_MAX]
    cpu = [x["cpu_util"] for x in samples if x.get("cpu_util") is not None]
    if not gpu and not cpu:
        return None
    t1 = now
    t0 = min(max(t1 - POWER_SAMPLE_SECONDS * len(samples), since), t1 - POWER_SAMPLE_SECONDS)
    return {"k": "pw", "srv": server_id, "t0": round(t0, 1), "t1": round(t1, 1), "n": len(samples),
            "cpu_util": round(sum(cpu) / len(cpu), 4) if cpu else None,
            "gpu_w": round(sum(gpu) / len(gpu), 2) if gpu else None}


def record_power(server_id: str, samples: list, now: float, since: float) -> Optional[dict]:
    """Write a helper's ``power`` frame as the machine's power row (``power_row``; ``usage``'s
    ``power-<machine>-YYYY-MM.jsonl``, which costing reads), unless its owner turned the machine's live sampling off
    (``power.sampling`` ``off``) or it is gone. Off the event loop; never raises. Returns the row written, or None."""
    from .. import servers, usage
    try:
        sv = servers.find(server_id)
        if sv is None or (sv.get("power") or {}).get("sampling") == "off":
            return None
        row = power_row(server_id, samples, now, since)
        if row is not None:
            usage.append(f"power-{server_id}", [row])
        return row
    except Exception:
        log.exception("could not write the power samples of %s's helper", server_id)
        return None


def flag_machine(server_id: str, flag: str, detail: Optional[dict[str, Any]] = None) -> None:
    """Count a flag on a machine's row (``servers.flags[flag] = {"count", "last", ...}``), shown to administrators:
    ``settings_overclaim`` (an acknowledgement that claimed more than the site asked), ``cancel_unacked`` (a cancel
    the helper never acknowledged). The first one of a kind writes an audit row. Off the event loop; never raises."""
    from datetime import datetime, timezone

    from .. import db
    from ..auth import audit
    from ..db.models import Server
    try:
        with db.session() as s:
            row = s.get(Server, server_id)
            if row is None:
                return
            flags = dict(row.flags or {})
            first = flag not in flags
            old = flags.get(flag) if isinstance(flags.get(flag), dict) else {}
            flags[flag] = {**(detail or {}), "count": int(old.get("count") or 0) + 1,
                           "last": datetime.now(timezone.utc).isoformat()}
            row.flags = flags
            if first:
                audit.record(s, f"server.{flag}", object_type="server", object_id=server_id, **(detail or {}))
    except Exception:
        log.exception("could not flag machine %s with %s", server_id, flag)
