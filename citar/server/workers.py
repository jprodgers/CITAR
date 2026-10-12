"""The worker hub: connected `citar-worker` agents, and routing inference to them.

The interesting problem here is threading. CITAR's turn driver is a plain thread calling a
synchronous `step()`, while the websockets live in the asyncio event loop. So a request crosses
threads twice:

    turn thread          submit()  ->  run_coroutine_threadsafe  ->  event loop  ->  websocket
    turn thread   <- Event.wait()  <-  future.set_result         <-  event loop  <-  websocket

`submit()` blocks the calling thread on a `threading.Event`, which is exactly right: that thread is
driving one seat's turn and has nothing else to do until the model answers. What must not happen is
blocking the *event loop*, so everything on that side is `await`ed and the only blocking wait is on
the caller's own thread.

Liveness is a held connection plus a recent pong, not a config flag. A server is "online" only while
a worker is actually there — anything else means the lobby offers a machine that will not answer.

A connection lasts only as long as what let it in: revoking its token, disabling or deleting its machine, or
suspending the machine's owner drops it at once (``WorkerHub.drop``, called by those routes once their change is
committed), and the keep-alive checks every connection again at each ping, so a change made elsewhere (``admin_cli``,
another process) drops it within a ping (design/phase3-features.md P3.2.8).
"""
from __future__ import annotations

import asyncio
import logging
import threading
import time
from dataclasses import dataclass, field
from typing import Optional

from ..worker import protocol as P

log = logging.getLogger("citar.workers")

#: How long after the last frame a worker is presumed gone. Three missed pings.
STALE_AFTER = 70.0


class WorkerError(RuntimeError):
    """The worker could not do it. `refusal` distinguishes "busy" from "broken"."""

    def __init__(self, message: str, *, refusal: str = "", retryable: bool = False):
        super().__init__(message)
        self.refusal = refusal
        self.retryable = retryable


@dataclass
class Pending:
    """A request in flight, waiting for its answer on another thread."""
    id: str
    event: threading.Event = field(default_factory=threading.Event)
    result: Optional[dict] = None
    error: Optional[WorkerError] = None
    started: float = field(default_factory=time.time)


class WorkerConnection:
    """One connected agent, serving one registered server."""

    def __init__(self, websocket, server_id: str, hello: dict, loop, token_id: Optional[str] = None):
        self.ws = websocket
        self.server_id = server_id
        #: The worker token it presented: revoking that one drops it (``WorkerHub.drop``).
        self.token_id = token_id
        self.loop = loop
        self.hello = hello
        self.hostname = hello.get("hostname") or ""
        self.provider = hello.get("provider") or ""
        self.worker_version = hello.get("worker_version") or ""
        self.max_concurrent = max(1, int(hello.get("max_concurrent") or 1))
        self.models = list(hello.get("models") or [])
        self.hardware = hello.get("hardware")
        self.connected_at = time.time()
        self.last_seen = time.time()
        self.pending: dict = {}
        self.closed = False
        self._lock = threading.RLock()

    # -------------------------------------------------------------- state
    @property
    def alive(self) -> bool:
        """Whether this connection is still usable."""
        return not self.closed and (time.time() - self.last_seen) < STALE_AFTER

    @property
    def in_flight(self) -> int:
        """How many requests are waiting on this worker."""
        with self._lock:
            return len(self.pending)

    def touch(self):
        """Record that the worker was heard from, for the keep-alive."""
        self.last_seen = time.time()

    def model_keys(self) -> list:
        """The models this worker offers."""
        return [m.get("key") for m in self.models if m.get("key")]

    def client(self) -> dict:
        """The worker as the Servers page shows it."""
        return {
            "server_id": self.server_id,
            "hostname": self.hostname,
            "provider": self.provider,
            "worker_version": self.worker_version,
            "models": self.models,
            "max_concurrent": self.max_concurrent,
            "in_flight": self.in_flight,
            "connected_at": self.connected_at,
            "last_seen": self.last_seen,
            "alive": self.alive,
            "uptime_seconds": round(time.time() - self.connected_at),
        }

    # -------------------------------------------------------------- sending
    async def send(self, text: str):
        """Send a frame to the worker."""
        await self.ws.send_text(text)

    def send_threadsafe(self, text: str):
        """Queue a frame from a non-async thread."""
        asyncio.run_coroutine_threadsafe(self.send(text), self.loop)

    # -------------------------------------------------------------- requests
    def open_request(self, request_id: str) -> Pending:
        """Register a request and return something to wait on."""
        with self._lock:
            pending = Pending(id=request_id)
            self.pending[request_id] = pending
            return pending

    def resolve(self, request_id: str, *, result: Optional[dict] = None,
                error: Optional[WorkerError] = None):
        """Complete a request with its result or its error."""
        with self._lock:
            pending = self.pending.pop(request_id, None)
        if pending is None:
            # A late answer to something already timed out or cancelled. Dropping it is correct —
            # the caller has moved on — but it is worth knowing it happened.
            log.debug("worker %s answered unknown request %s", self.server_id, request_id)
            return
        pending.result = result
        pending.error = error
        pending.event.set()

    def fail_all(self, message: str, *, retryable: bool = True):
        """Called when the connection drops: nothing in flight can ever be answered. ``retryable`` is false when the
        machine was taken away (``WorkerHub.drop``): waiting for it to come back would be waiting for nothing."""
        with self._lock:
            pending, self.pending = list(self.pending.values()), {}
        for item in pending:
            item.error = WorkerError(message, refusal="" if retryable else "dropped", retryable=retryable)
            item.event.set()


class WorkerHub:
    """Every connected worker, and the routing between them and the game threads."""

    def __init__(self):
        self._workers: dict = {}          # server_id -> WorkerConnection
        self._lock = threading.RLock()
        self._loop: Optional[asyncio.AbstractEventLoop] = None

    def attach_loop(self, loop):
        """Attach the event loop, so synchronous callers can schedule sends onto it."""
        self._loop = loop

    # -------------------------------------------------------------- registry
    def register(self, connection: WorkerConnection) -> Optional[WorkerConnection]:
        """Add a worker, returning any previous one for the same server so it can be closed.

        A reconnect after a network drop arrives before the old socket has timed out, so the newest
        connection wins rather than being refused as a duplicate — otherwise a flapping link locks
        the owner out of their own server for a minute at a time.
        """
        with self._lock:
            previous = self._workers.get(connection.server_id)
            self._workers[connection.server_id] = connection
        if previous is not None:
            previous.closed = True
            previous.fail_all("Replaced by a newer connection from the same worker.")
        return previous

    def unregister(self, connection: WorkerConnection):
        """Forget a worker that has disconnected, failing anything still waiting on it."""
        with self._lock:
            if self._workers.get(connection.server_id) is connection:
                del self._workers[connection.server_id]
        connection.closed = True
        connection.fail_all("The worker disconnected.")

    def get(self, server_id: str) -> Optional[WorkerConnection]:
        """The connection for a server id, if it is online."""
        with self._lock:
            connection = self._workers.get(server_id)
        return connection if (connection and connection.alive) else None

    def is_online(self, server_id: str) -> bool:
        """Whether a server has a worker connected."""
        return self.get(server_id) is not None

    def in_flight(self, server_id: str) -> int:
        """How many requests are in flight to a server."""
        connection = self.get(server_id)
        return connection.in_flight if connection else 0

    def drop(self, server_id: str, reason: str, *, token_id: Optional[str] = None) -> bool:
        """Disconnect a machine's helper because it may no longer serve: its token revoked (``token_id``: only a
        connection made with that token), its machine disabled or deleted, its owner suspended. Pending requests fail
        as not retryable, the helper is told why (a ``shutdown`` frame, which stops it rather than have it reconnect
        to a refusal) and its socket is closed. Call it after the change that took the machine away is committed:
        the helper's next hello must find it. Returns whether a connection was dropped."""
        with self._lock:
            connection = self._workers.get(server_id)
            if connection is None or (token_id is not None and connection.token_id != token_id):
                return False
            del self._workers[server_id]
        connection.closed = True
        connection.fail_all(f"The machine's helper was disconnected: {reason}", retryable=False)
        log.info("dropped the worker for %s: %s", server_id, reason)
        if connection.loop is not None and connection.ws is not None:
            async def close():
                try:
                    await connection.ws.send_text(P.frame(P.SHUTDOWN, reason=reason))
                except Exception:
                    pass
                try:
                    await connection.ws.close(code=4403)
                except Exception:
                    pass
            try:
                asyncio.run_coroutine_threadsafe(close(), connection.loop)
            except RuntimeError:            # the loop is gone: so is the socket
                pass
        return True

    def drop_many(self, server_ids, reason: str) -> int:
        """``drop`` each machine of ``server_ids`` (an account's, when it is suspended); returns how many were."""
        return sum(1 for sid in server_ids if self.drop(sid, reason))

    def online_ids(self) -> set:
        """The ids of every connected server."""
        with self._lock:
            return {sid for sid, c in self._workers.items() if c.alive}

    def status(self) -> list:
        """Every connection, for the Servers page."""
        with self._lock:
            return [c.client() for c in self._workers.values()]

    # -------------------------------------------------------------- the bridge
    def submit(self, server_id: str, request: P.Request, *, timeout: Optional[float] = None) -> dict:
        """Send a completion to a worker and wait for the answer. Blocking; call from a game thread.

        Never call this from the event loop — it blocks the calling thread by design.
        """
        connection = self.get(server_id)
        if connection is None:
            raise WorkerError(
                "That server's worker is not connected. Start the CITAR helper (citar-worker) on the machine.",
                refusal="closed", retryable=True)
        if connection.in_flight >= connection.max_concurrent:
            raise WorkerError(f"{connection.hostname or server_id} is busy "
                              f"({connection.in_flight}/{connection.max_concurrent} in flight).",
                              refusal="busy", retryable=True)

        deadline = timeout if timeout is not None else request.timeout
        pending = connection.open_request(request.id)
        try:
            connection.send_threadsafe(P.request_frame(request))
        except Exception as exc:
            connection.resolve(request.id)
            raise WorkerError(f"Could not reach the worker: {exc}", retryable=True) from exc

        # A little grace beyond the worker's own deadline, so the worker's error — which says what
        # actually went wrong — normally arrives before this fires.
        if not pending.event.wait(deadline + 15):
            connection.resolve(request.id)
            try:
                connection.send_threadsafe(P.frame(P.CANCEL, id=request.id))
            except Exception:
                pass
            raise WorkerError(f"The worker did not answer within {deadline:.0f}s.", retryable=True)

        if pending.error is not None:
            raise pending.error
        return pending.result or {}

    def broadcast(self, frame_text: str):
        """Send a frame to every connected worker."""
        with self._lock:
            connections = list(self._workers.values())
        for connection in connections:
            try:
                connection.send_threadsafe(frame_text)
            except Exception:
                pass

    async def ping_loop(self, interval: float = 20.0):
        """Keep connections warm and notice dead ones, and ones that may no longer serve.

        A websocket through a home router and a proxy can be silently dead for minutes; an
        application-level ping is what turns that into a prompt, honest "offline". Each round also checks
        every connection's token, machine and owner again (``still_allowed``), so a change made where this
        process could not drop the helper at once (``admin_cli``, another server process) takes effect
        within a ping.
        """
        while True:
            await asyncio.sleep(interval)
            with self._lock:
                connections = list(self._workers.values())
            try:
                refused = await asyncio.to_thread(still_allowed, [(c.server_id, c.token_id) for c in connections])
            except Exception:
                log.exception("could not check the workers' tokens again")
                refused = {}
            for (server_id, token_id), reason in refused.items():
                self.drop(server_id, reason, token_id=token_id)
            for connection in connections:
                if connection.closed:
                    continue
                if not connection.alive:
                    log.info("worker for %s went stale, dropping", connection.server_id)
                    self.unregister(connection)
                    try:
                        await connection.ws.close(code=4408)
                    except Exception:
                        pass
                    continue
                try:
                    await connection.send(P.frame(P.PING))
                except Exception:
                    self.unregister(connection)


_hub: Optional[WorkerHub] = None
_hub_lock = threading.Lock()


def hub() -> WorkerHub:
    """The worker hub, created on first use."""
    global _hub
    with _hub_lock:
        if _hub is None:
            _hub = WorkerHub()
        return _hub


# ---------------------------------------------------------------------------- authentication

def _refusal(s, token_row, server) -> Optional[str]:
    """Why a token may not let a helper in, or None: revoked, its machine gone or switched off, the machine's owner
    out of good standing (a suspended account's machine serves nobody)."""
    from ..db.models import User
    if token_row is None or token_row.revoked_at is not None:
        return "its worker token was revoked"
    if server is None:
        return "its machine was deleted"
    if not server.enabled:
        return "its machine was disabled"
    owner = s.get(User, server.owner_id) if server.owner_id else None
    if owner is not None and owner.status in ("suspended", "deleted", "pending"):
        return "its owner's account is not active"
    return None


def still_allowed(pairs) -> dict:
    """``{(server_id, token_id): reason}`` for the connected helpers whose token, machine or owner no longer lets
    them in (``pairs`` is (server id, token id) per connection); the others are left out."""
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
                reason = _refusal(s, token, server)
            else:           # a connection made without a token row (a test's): only its machine is checked
                reason = None if server is not None and server.enabled else "its machine was disabled or deleted"
            if reason:
                out[(server_id, token_id)] = reason
    return out


def authenticate(token: str) -> Optional[tuple]:
    """Resolve a worker token to (server_id, server_name, token_id), or None.

    Only the hash is stored, so a leaked database yields no usable worker credential. A revoked or
    unknown token is indistinguishable from the outside, and so is one whose machine is disabled or
    whose owner is suspended.
    """
    from .. import db
    from ..auth import tokens
    from ..db.models import Server, WorkerToken
    from datetime import datetime, timezone
    from sqlalchemy import select

    if not token:
        return None
    try:
        with db.session() as s:
            row = s.scalar(select(WorkerToken).where(
                WorkerToken.token_hash == tokens.hash_token(token)))
            server = s.get(Server, row.server_id) if row is not None else None
            if _refusal(s, row, server) is not None:
                return None
            row.last_seen_at = datetime.now(timezone.utc)
            return server.id, server.name, row.id
    except Exception:
        log.exception("worker authentication failed")
        return None


def record_connection(server_id: str, *, ip: str = "", hardware: Optional[dict] = None,
                      models: Optional[list] = None):
    """Persist what a worker reported, so the Servers page is accurate after a restart."""
    from .. import db
    from ..db.models import Server
    from datetime import datetime, timezone

    try:
        with db.session() as s:
            server = s.get(Server, server_id)
            if server is None:
                return
            config = dict(server.config or {})
            if hardware:
                # The worker collected this on the machine itself, which beats anything typed in.
                config["hardware"] = hardware
            if models:
                existing = {m.get("key"): m for m in (config.get("models") or [])}
                merged = []
                for model in models:
                    key = model.get("key")
                    if not key:
                        continue
                    entry = dict(existing.get(key) or {})
                    entry.update({"key": key, "label": model.get("label") or entry.get("label") or key})
                    if model.get("context"):
                        entry["context"] = model["context"]
                    merged.append(entry)
                config["models"] = merged
            server.config = config
            server.updated_at = datetime.now(timezone.utc)
    except Exception:
        log.exception("could not record worker connection for %s", server_id)
