"""Fake time for a game session's chats (design/phase3-features.md P3.5.2: "every timeout test runs in fake time").

``FakeClock`` stands in for the session's ``RealClock`` (``GameSession.clock``, ``ChatClock.clock``): ``wait`` returns
at once with the time moved on by what it was asked to wait, or to the next alarm a test set (``at``), and the
session's chat clock looks once (``ChatClock.tick``) after each move, as its thread would every second. So a wait of
ten minutes in ``GameSession._await`` (a bot's turn, a model's end_turn) passes in milliseconds, and a person's answer
lands at the second the test names, on the waiting thread, with the game's lock held as a person's tool call holds it.

``use(s)`` puts one on a session that is not started (no chat clock thread runs); ``advance(seconds)`` moves time on
from the test's own thread, a second at a time, ticking the chat clock each second.
"""
from __future__ import annotations

import heapq
import itertools


class FakeClock:
    """A session's clock that the test moves (module docstring)."""

    WALL = 1_800_000_000.0

    def __init__(self, session=None, *, ticking: bool = True):
        self.t = 0.0
        self.session = session
        #: whether a move ticks the session's chat clock (False: the chat clock is "stopped for good")
        self.ticking = ticking
        self._alarms: list = []
        self._seq = itertools.count()
        self.waits = 0

    def now(self) -> float:
        return self.t

    def wall(self) -> float:
        return self.WALL + self.t

    def at(self, when: float, fn) -> None:
        """Call ``fn()`` when fake time reaches ``when`` (on whichever thread moves it there)."""
        heapq.heappush(self._alarms, (float(when), next(self._seq), fn))

    def _tick(self) -> None:
        if self.ticking and self.session is not None:
            self.session.chat.tick()

    def _move_to(self, target: float) -> bool:
        """Move to ``target``, or to the first alarm before it (firing it); returns whether an alarm fired."""
        if self._alarms and self._alarms[0][0] <= target:
            when, _, fn = heapq.heappop(self._alarms)
            self.t = max(self.t, when)
            fn()
            self._tick()
            return True
        self.t = target
        self._tick()
        return False

    def wait(self, cond, timeout: float) -> None:
        """``RealClock.wait`` in fake time: back at once, the time moved on (an alarm on the way ends it there)."""
        self.waits += 1
        self._move_to(self.t + max(0.0, float(timeout)))

    def advance(self, seconds: float, step: float = 1.0) -> None:
        """Move time on by ``seconds`` from the test's thread, ``step`` at a time, firing alarms and ticking the chat
        clock as it goes."""
        end = self.t + seconds
        while self.t < end - 1e-9:
            self._move_to(min(end, self.t + step))


def use(s, *, ticking: bool = True) -> FakeClock:
    """Put a fake clock on session ``s`` (not started) and its chat clock, from now; returns it."""
    clock = FakeClock(s, ticking=ticking)
    s.clock = clock
    s.chat.clock = clock
    s.chat._last = clock.now()
    return clock
