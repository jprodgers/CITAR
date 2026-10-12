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
threads busy as there are seats with chats to answer.

**A halt never declines** (``halt``): cancelling a seat's agent (a suspension for quiet hours, make-way or a
disconnect, a seat change, a crash) moves its line to a new epoch: what was queued is dropped and what is running
finds itself halted when it returns, so the session declines nothing for it (``GameSession._run_responder`` declines
only an answer that ran and failed outright), and the chat waits for the seat's next responder (dispatched again on
resume, load, ``restore_live`` and a seat change) or for the chat clock.

Replaces: the daemon thread per ``(negotiation, entries)`` that ``GameSession._dispatch_negotiation_interrupts``
started (``citar/server/session.py``, before 3-11), and its ``_responding`` set that was never pruned.
"""
from __future__ import annotations

import logging
import os
import queue
import threading
from collections import deque
from typing import Callable, Optional

log = logging.getLogger("citar.responders")

#: The default sizes of the two pools.
DEFAULT_MODEL_RESPONDERS = 16
DEFAULT_BOT_RESPONDERS = 2


def _size(name: str, default: int) -> int:
    """A pool's size from the environment, at least 1."""
    try:
        return max(1, int(os.environ.get(name) or default))
    except ValueError:
        return default


class Pool:
    """At most ``size`` daemon threads taking tasks from one queue, started as the work needs them (when the tasks
    waiting outnumber the threads free to take them) and kept. Daemon threads, unlike ``concurrent.futures``' workers, which the
    interpreter joins at exit: a server shutting down must not wait for a model answer stuck behind a busy machine."""

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


def reset() -> None:
    """Forget the pools (tests that change their sizes): new work goes to new ones, and the old ones' threads finish
    what they have, then wait idle (daemon threads)."""
    with _POOLS_LOCK:
        _POOLS.clear()


class SeatLine:
    """One seat's chats to answer, in order, one at a time (``items``: ``(nid, entries, epoch)``). ``epoch`` moves on
    a halt; ``current`` is the chat being answered now (its id and the epoch it was taken in)."""

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


def line(session, pid: int) -> SeatLine:
    """Seat ``pid``'s line in ``session``, made on first use."""
    lines = session._lines
    ln = lines.get(pid)
    if ln is None:
        ln = lines.setdefault(pid, SeatLine())
    return ln


def submit(session, pid: int, nid: int, entries: int, kind: str) -> None:
    """Queue seat ``pid``'s answer to chat ``nid`` (as it stood at ``entries``) on its line, and start the line on
    ``kind``'s pool (``model`` or ``bot``) if it is not running. Non-blocking: called with the session's lock held."""
    ln = line(session, pid)
    with ln.lock:
        ln.items.append((nid, entries, ln.epoch))
        if ln.running:
            return
        ln.running = True
    pool(kind).submit(_drain, session, pid, ln)


def _drain(session, pid: int, ln: SeatLine) -> None:
    """Answer the seat's queued chats one at a time, then let the line go."""
    while True:
        with ln.lock:
            if not ln.items or session.stopped:
                ln.items.clear()
                ln.running = False
                ln.current = None
                return
            nid, entries, epoch = ln.items.popleft()
            if epoch != ln.epoch:
                continue                    # queued before a halt: the chat waits for the next dispatch or the clock
            ln.current = (nid, epoch)
        try:
            session._run_responder(pid, nid, (nid, entries), epoch=epoch)
        except BaseException:
            log.exception("seat %s's answer to negotiation %s failed", pid, nid)
        finally:
            with ln.lock:
                ln.current = None


def halt(session, pid: int) -> None:
    """Halt seat ``pid``'s answers (non-blocking; allowed under the session's lock): its line moves to a new epoch, so
    what is queued is dropped and what runs returns as halted, without declining; and its chats leave
    ``_responding``, so they are dispatched again on the seat's return."""
    ln = session._lines.get(pid)
    if ln is not None:
        with ln.lock:
            ln.epoch += 1
            ln.items.clear()
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
