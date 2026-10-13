"""The seats' chat answers, off the turn driver (design/phase3-features.md P3.5.3).

When a chat comes to wait on a seat a program plays (a bot, an LLM seat, a hybrid seat), the session hands the answer
here (``submit``) rather than starting a thread of its own for it: each seat has a line (``SeatLine``) and answers one
chat at a time, in the order they came, and the answers run on **two executors** per process, so that how many
threads chats take is bounded and a bot is never stuck behind a model:

- model answers (an LLM seat's, a hybrid seat's model) on a pool of ``CITAR_RESPONDERS`` threads (16 by default):
  each may wait minutes for a busy machine;
- bot answers (a bot seat's, and a hybrid seat's bot-first pass) on their own small pool of ``CITAR_BOT_RESPONDERS``
  threads (2 by default): instant engine calls, which a pool full of model answers waiting on a GPU must not delay.

A seat's line drains on one thread of its pool at a time (``_drain``): one chat at a time per seat, and only as many
threads busy as there are seats with chats to answer. Each answer in a line says which pool it is for, so a line
whose next answer is of the other kind moves to that pool (a hybrid seat's bot-first pass, then its model's answer).

**A machine's answers take no more threads than it has slots** (``_Machines``). A model answer waits for its
machine's slot on the thread that asks; were a pool thread to wait so, sixteen answers queued on one busy machine
would hold every thread, and the chats of every other machine's seats would time out unasked. So a line takes a model
pool thread only while its machine has fewer of the responders' answers in flight than it has slots (and never more
than half the pool, however large the machine): otherwise it is parked, holding no thread, and the next of that
machine's answers to end hands its place to the first parked line. A parked chat is told its place as a request in
the machine's own line is (``negotiation_status`` ``queued``: behind what waits on the machine, then the lines parked
before it; with ``benchmark`` while a benchmark holds the machine), and told again as the line moves up. A seat with
no machine (an inline block) is not counted.

**A halt never declines** (``halt``): cancelling a seat's agent (a suspension for quiet hours, make-way or a
disconnect, a seat change, a crash) moves its line to a new epoch: what was queued is dropped and what is running
finds itself halted when it returns, so the session declines nothing for it (``GameSession._run_responder`` declines
only an answer that ran and failed outright), and the chat waits for the seat's next responder (dispatched again on
resume, load, ``restore_live`` and a seat change) or for the chat clock. A parked line that is halted leaves its
machine's queue, so a seat moved to another machine answers there without waiting for the old one.

Replaces: the daemon thread per ``(negotiation, entries)`` that ``GameSession._dispatch_negotiation_interrupts``
started (``citar/server/session.py``, before 3-11), and its ``_responding`` set that was never pruned.
"""
from __future__ import annotations

import logging
import os
import queue
import threading
from collections import Counter, deque
from typing import Callable, Optional

log = logging.getLogger("citar.responders")

#: The default sizes of the two pools.
DEFAULT_MODEL_RESPONDERS = 16
DEFAULT_BOT_RESPONDERS = 2
#: The pools' kinds.
KINDS = ("model", "bot")


def _size(name: str, default: int) -> int:
    """A pool's size from the environment, at least 1."""
    try:
        return max(1, int(os.environ.get(name) or default))
    except ValueError:
        return default


class Pool:
    """At most ``size`` daemon threads taking tasks from one queue, started as the work needs them (when the tasks
    waiting outnumber the threads free to take them) and kept. Daemon threads, unlike ``concurrent.futures``' workers,
    which the interpreter joins at exit: a server shutting down must not wait for a model answer stuck behind a busy
    machine."""

    def __init__(self, size: int, prefix: str):
        self.size = max(1, int(size))
        self.prefix = prefix
        self._tasks: queue.SimpleQueue = queue.SimpleQueue()
        self._lock = threading.Lock()
        self._threads: list = []
        self._idle = 0               # threads waiting for a task
        self._queued = 0             # tasks no thread has taken yet

    def submit(self, fn: Callable, *args) -> None:
        """Run ``fn(*args)`` on one of the pool's threads, as soon as one is free: a thread is started whenever the
        tasks waiting outnumber the threads waiting for them, up to ``size``."""
        with self._lock:
            self._tasks.put((fn, args))
            self._queued += 1
            if self._queued > self._idle and len(self._threads) < self.size:
                t = threading.Thread(target=self._work, daemon=True,
                                     name=f"{self.prefix}-{len(self._threads) + 1}")
                self._threads.append(t)
                self._idle += 1          # it waits for a task from its start
                t.start()

    def _work(self) -> None:
        while True:
            fn, args = self._tasks.get()
            with self._lock:
                self._idle -= 1
                self._queued -= 1
            try:
                fn(*args)
            except BaseException:
                log.exception("a responder task failed")
            with self._lock:
                self._idle += 1

    @property
    def threads(self) -> int:
        """How many threads it has started."""
        with self._lock:
            return len(self._threads)


_POOLS: dict = {}
_POOLS_LOCK = threading.Lock()


def pool(kind: str) -> Pool:
    """The process's pool for ``kind`` (``model`` or ``bot``), made on first use with its size from
    ``CITAR_RESPONDERS`` or ``CITAR_BOT_RESPONDERS``."""
    with _POOLS_LOCK:
        ex = _POOLS.get(kind)
        if ex is None:
            if kind == "bot":
                ex = Pool(_size("CITAR_BOT_RESPONDERS", DEFAULT_BOT_RESPONDERS), "bot-responder")
            else:
                ex = Pool(_size("CITAR_RESPONDERS", DEFAULT_MODEL_RESPONDERS), "model-responder")
            _POOLS[kind] = ex
        return ex


class _Machines:
    """The responders' model answers in flight on each machine, and the seat lines parked until one of them ends
    (module docstring). Its lock is never held while anything else is taken."""

    def __init__(self):
        self.lock = threading.Lock()
        self.inflight: Counter = Counter()
        self.parked: dict = {}          # machine -> deque of (session, pid, line) waiting for one of its answers to end

    def enter(self, sid: str, limit: int, waiter: tuple) -> bool:
        """Take one of ``sid``'s places for an answer (True), or park ``waiter`` (its line) in its queue (False), its
        chat told its place."""
        with self.lock:
            if self.inflight[sid] < limit:
                self.inflight[sid] += 1
                return True
            waiting = self.parked.setdefault(sid, deque())
            waiting.append(waiter)
            place = len(waiting)
        _tell_places(sid, [(place, waiter)])
        return False

    def leave(self, sid: str) -> None:
        """An answer on ``sid`` ended: its place goes to the first parked line (which drains again on the model pool,
        holding it), or is given back."""
        with self.lock:
            waiting = self.parked.get(sid)
            nxt = waiting.popleft() if waiting else None
            if waiting is not None and not waiting:
                del self.parked[sid]
            if nxt is None:
                self.inflight[sid] -= 1
                if self.inflight[sid] <= 0:
                    del self.inflight[sid]
            behind = list(enumerate(waiting, 1)) if waiting else []
        if nxt is not None:
            session, pid, ln = nxt
            pool("model").submit(_drain, session, pid, ln, "model", sid, self)
            _tell_places(sid, behind)           # each line behind it moved up a place

    def unpark(self, ln: "SeatLine") -> bool:
        """Take line ``ln`` out of every machine's queue (a halt); returns whether it was parked."""
        with self.lock:
            for sid, waiting in list(self.parked.items()):
                for w in list(waiting):
                    if w[2] is ln:
                        waiting.remove(w)
                        if not waiting:
                            del self.parked[sid]
                        return True
        return False

    def status(self, sid: str) -> dict:
        """``in_flight`` (the responders' answers on ``sid`` now) and ``parked`` (lines waiting for one to end)."""
        with self.lock:
            return {"in_flight": self.inflight.get(sid, 0), "parked": len(self.parked.get(sid) or ())}


_MACHINES = _Machines()


def machines() -> _Machines:
    """The process's count of model answers per machine."""
    return _MACHINES


def _tell_places(sid: str, places: list) -> None:
    """Tell each parked line's chat its place in machine ``sid``'s line (``places``: (place among the parked, (session,
    pid, line))): a ``queued`` status, as the machine tells the requests in its own line, behind what waits there.
    Called with no lock held; a failure is logged, never raised (it is only news). Each is told with its line's lock
    held, and only while its answer has not started, so a place never arrives after the answer's ``thinking`` (the
    line marks its answer started under that lock before asking it)."""
    if not places:
        return
    waiting, bench, name = 0, False, sid
    try:
        from ..pool import scheduler
        m = scheduler.get().machine(sid)
        if m is not None:
            info = m.line_info()
            waiting, bench, name = info["waiting"], info["benchmark"], info["name"]
    except Exception:
        log.debug("could not read machine %s's line", sid, exc_info=True)
    for place, (session, pid, ln) in places:
        tell = getattr(getattr(session, "chat", None), "answer_status", None)
        if tell is None:
            continue
        with ln.lock:
            head = next((item for item in ln.items if item[2] == ln.epoch), None)
            if head is None or ln.current is not None:
                continue
            try:
                tell(head[0], pid, {"state": "queued", "position": waiting + place, "eta_s": None,
                                    "machine_name": name, "benchmark": bench})
            except Exception:
                log.exception("could not tell negotiation %s its place", head[0])


def reset() -> None:
    """Forget the pools and the machines' counts (tests that change the pools' sizes): new work goes to new ones, and
    the old ones' threads finish what they have, then wait idle (daemon threads)."""
    global _MACHINES
    with _POOLS_LOCK:
        _POOLS.clear()
        _MACHINES = _Machines()


def machine_of(session, pid: int) -> Optional[str]:
    """The machine seat ``pid``'s model answers run on (its block's ``server_id``), or None (no machine: an inline
    block, a bot)."""
    try:
        llm = getattr(session.seats[pid], "llm", None) or {}
    except (IndexError, TypeError):
        return None
    sid = llm.get("server_id") if isinstance(llm, dict) else None
    return sid if isinstance(sid, str) and sid else None


def machine_limit(sid: str) -> int:
    """How many of the responders' answers machine ``sid`` takes at once: its slots now (1 when they are unknown: a
    helper that is offline), and never more than half the model pool, so that no one machine, however large, holds
    every thread."""
    cap = None
    try:
        from ..pool import scheduler
        m = scheduler.get().machine(sid)
        if m is not None:
            with m.lock:
                cap = m.capacity()
    except Exception:
        log.debug("could not read machine %s's capacity", sid, exc_info=True)
    share = max(1, pool("model").size // 2)
    return max(1, min(cap or 1, share))


class SeatLine:
    """One seat's chats to answer, in order, one at a time (``items``: ``(nid, entries, epoch, kind)``, ``kind`` the
    pool the answer is for). ``epoch`` moves on a halt; ``current`` is the chat being answered now (its id and the
    epoch it was taken in)."""

    __slots__ = ("lock", "items", "running", "epoch", "current")

    def __init__(self):
        self.lock = threading.Lock()
        self.items: deque = deque()
        self.running = False
        self.epoch = 0
        self.current: Optional[tuple] = None

    def holds(self, nid: int) -> bool:
        """Whether chat ``nid`` is queued or being answered on this line (of its current epoch)."""
        with self.lock:
            if self.current is not None and self.current[0] == nid and self.current[1] == self.epoch:
                return True
            return any(item[0] == nid and item[2] == self.epoch for item in self.items)

    def pending(self) -> bool:
        """Whether an answer of the current epoch is queued (not counting the one under way)."""
        with self.lock:
            return any(item[2] == self.epoch for item in self.items)


def line(session, pid: int) -> SeatLine:
    """Seat ``pid``'s line in ``session``, made on first use."""
    lines = session._lines
    ln = lines.get(pid)
    if ln is None:
        ln = lines.setdefault(pid, SeatLine())
    return ln


def submit(session, pid: int, nid: int, entries: int, kind: str) -> None:
    """Queue seat ``pid``'s answer to chat ``nid`` (as it stood at ``entries``) on its line, for ``kind``'s pool
    (``model`` or ``bot``), and start the line there if it is not running. Non-blocking: called with the session's
    lock held."""
    if kind not in KINDS:
        raise ValueError(f"Unknown responder kind {kind!r}.")
    ln = line(session, pid)
    with ln.lock:
        ln.items.append((nid, entries, ln.epoch, kind))
        if ln.running:
            return
        ln.running = True
    pool(kind).submit(_drain, session, pid, ln, kind)


def _drain(session, pid: int, ln: SeatLine, kind: str, held: Optional[str] = None,
           gate: Optional[_Machines] = None) -> None:
    """Answer the seat's queued chats one at a time on ``kind``'s pool, then let the line go. A line whose next answer
    is for the other pool moves there; a model answer first takes one of its machine's places (``held``: one handed
    over by the answer that ended before it), or parks the line until one is free."""
    gate = gate or _MACHINES
    try:
        while True:
            with ln.lock:
                while ln.items and ln.items[0][2] != ln.epoch:
                    ln.items.popleft()      # queued before a halt: the chat waits for the next dispatch or the clock
                if not ln.items or session.stopped:
                    ln.items.clear()
                    ln.running = False
                    ln.current = None
                    return
                head = ln.items[0]
            nid, entries, epoch, want = head
            if want != kind:
                # the next answer is the other pool's (a hybrid seat's bot-first pass, then its model's answer): the
                # line goes on there, still running
                if held is not None:
                    gate.leave(held)
                    held = None
                pool(want).submit(_drain, session, pid, ln, want)
                return
            if kind == "model":
                sid = machine_of(session, pid)
                if held is not None and held != sid:
                    gate.leave(held)        # the seat moved to another machine since its place was handed over
                    held = None
                if sid is not None and held is None:
                    if not gate.enter(sid, machine_limit(sid), (session, pid, ln)):
                        return              # parked, still running, its answer at the head: no thread held
                    held = sid
            with ln.lock:
                if not ln.items or ln.items[0] is not head:
                    continue                # halted meanwhile: look again
                ln.items.popleft()
                ln.current = (nid, epoch)
            try:
                session._run_responder(pid, nid, (nid, entries), epoch=epoch)
            except BaseException:
                log.exception("seat %s's answer to negotiation %s failed", pid, nid)
            finally:
                with ln.lock:
                    ln.current = None
                if held is not None:
                    gate.leave(held)
                    held = None
    finally:
        if held is not None:
            gate.leave(held)


def halt(session, pid: int) -> None:
    """Halt seat ``pid``'s answers (non-blocking; allowed under the session's lock): its line moves to a new epoch, so
    what is queued is dropped and what runs returns as halted, without declining; and its chats leave
    ``_responding``, so they are dispatched again on the seat's return. A parked line leaves its machine's queue: what
    is submitted to it next starts it again."""
    ln = session._lines.get(pid)
    if ln is not None:
        with ln.lock:
            ln.epoch += 1
            ln.items.clear()
        if _MACHINES.unpark(ln):
            with ln.lock:
                restart = ln.items[0][3] if ln.items else None
                if restart is None:
                    ln.running = False
            if restart is not None:
                pool(restart).submit(_drain, session, pid, ln, restart)
    responding = session._responding
    for key in [k for k, who in list(responding.items()) if who == pid]:
        responding.pop(key, None)


def halted(session, pid: int, epoch: Optional[int]) -> bool:
    """Whether seat ``pid``'s answer taken at ``epoch`` has been halted since (None: not a line's, never halted)."""
    if epoch is None:
        return False
    ln = session._lines.get(pid)
    return ln is not None and ln.epoch != epoch


def busy_with(session, pid: int, nid: int) -> bool:
    """Whether seat ``pid`` has chat ``nid`` queued or under way."""
    ln = session._lines.get(pid)
    return ln is not None and ln.holds(nid)
