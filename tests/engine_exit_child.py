"""Exits the interpreter while a daemon thread is inside a call to the Rust engine (DESIGN.md P2.6.4).

A session's driver is a daemon thread, and it may be inside ``Game.drive`` when the server exits. If that call ends
once the interpreter has begun to finalize, Python 3.11 to 3.13 end the thread from inside ``PyEval_RestoreThread``
as it re-attaches (``pthread_exit`` on Linux, a forced unwind through the extension's Rust frames; 3.14 hangs it).
``citar._engine`` registers ``shutdown`` with atexit: it waits for the calls in flight and then parks any other
thread at its next call, before it re-attaches. PyO3 0.29 also parks a thread the unwind reaches (crates/citar-py's
calls.rs), so even without the hook the process exits 0; ``--unguarded`` shows that.

    python tests/engine_exit_child.py [check | long | short | barred] [--unguarded]

``check`` (the default; CI's interpreter-exit job and tests/test_engine_module.py run it) runs each mode below in a
child interpreter and checks what it printed: ``long``, ``short`` and ``barred`` must pass, and the ``--unguarded``
runs of ``long`` and ``short`` are reported, not judged. It prints a line per run and exits 0 when every judged run
passed. It needs only ``citar._engine`` (built by ``cargo xtask develop``, or unpacked into ``citar/``).

``long``: a daemon thread is inside one long drive (a whole game in a call) when the interpreter exits; ``short``: it
drives one seat per call, so it is as likely to be between calls, or entering one, as inside one. Either waits until
a call is in flight, prints ``exiting after a call was in flight``, and then, during finalization, opens the window: the interpreter's last collection
(it runs with ``sys.is_finalizing()`` true) frees an object whose finalizer releases the GIL for half a second, long
enough for any call to end, when a waiting thread would re-attach. The finalizer prints ``window: finalizing=1
in_flight=A->B``, the calls in flight as it opened and as it closed. Guarded, both are 0: the hook waited for every
call, and each thread parked without re-attaching. A child that never opened the window prints no such line, and the
check fails. ``barred`` calls ``shutdown`` itself and checks the mechanism: a call another thread then makes parks
that thread for good, while this thread's calls go on; it prints ``barred ok``.
"""
import atexit
import gc
import json
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))     # the checkout's citar, run as a script

from citar import _engine as E

WINDOW_SECONDS = 0.5
"""How long the finalizer holds the window open: well over a call (a 60-round game in one drive takes 0.1 s)."""


def _game(seed: int, turn_limit: int = 60):
    cfg = {"seed": seed, "map_size": "small", "players": [{"controller": "bot"}] * 4, "turn_limit": turn_limit}
    return E.Game.new(json.dumps(cfg).encode())


def _drive(seat_limit: int):
    bots = {p: E.Bot("idle") for p in range(4)}
    seed = 1
    while True:
        g = _game(seed)
        while g.phase == "playing":
            g.drive(bots, seat_limit)
        seed += 1


class _Window:
    """Releases the GIL for ``seconds`` while the interpreter finalizes, and says so on stdout.

    The collector is off and the object is its own cycle, so only the interpreter's last collection frees it, once
    finalization has begun. (A module global would not do: a daemon thread's frame keeps ``__main__``'s globals alive
    past the end, so their finalizers never run.) Builtins and module globals are being torn down by then, so it
    keeps everything it calls. It waits on a lock it holds rather than in ``time.sleep``, which raises during
    finalization on Windows' 3.14 (its timer handle is gone); either wait releases the GIL."""

    def __init__(self, seconds: float):
        self.cycle = self
        self.seconds = seconds
        self.held = threading.Lock()
        self.held.acquire()
        self.write, self.finalizing, self.in_flight = os.write, sys.is_finalizing, E.calls_in_flight

    def __del__(self):
        opened = self.in_flight()
        self.held.acquire(True, self.seconds)
        self.write(1, b"window: finalizing=%d in_flight=%d->%d\n" % (self.finalizing(), opened, self.in_flight()))


def _barred() -> int:
    """shutdown with nothing in flight returns True at once; afterwards another thread's call parks it (its game
    never moves), and this thread's calls still answer."""
    g = _game(1)
    if not E.shutdown(1.0) or E.calls_in_flight() != 0:
        print("shutdown did not finish with nothing in flight")
        return 1
    t = threading.Thread(target=g.drive, args=({p: E.Bot("idle") for p in range(4)}, 0), daemon=True)
    t.start()
    t.join(0.5)
    if not t.is_alive() or g.turn != 1:
        print("another thread's call was not parked")
        return 1
    if json.loads(g.summary())["turn"] != 1 or not E.Game.new(json.dumps({"seed": 2}).encode()).turn:
        print("this thread's calls did not go on")
        return 1
    print("barred ok", flush=True)
    return 0


def _exit_mid_drive(mode: str, unguarded: bool) -> int:
    if unguarded:
        atexit.unregister(E.shutdown)
    seat_limit = 0 if mode == "long" else 1
    threading.Thread(target=_drive, args=(seat_limit,), daemon=True).start()
    # Exit while a call is in flight: wait until one is, and a little more in the short mode, so the thread is
    # anywhere in its loop.
    deadline = time.monotonic() + 30
    while E.calls_in_flight() == 0 and time.monotonic() < deadline:
        time.sleep(0.001)
    if E.calls_in_flight() == 0:
        print("no call began in 30 s", flush=True)
        return 1
    if mode != "long":
        time.sleep(0.05)
    gc.disable()
    _Window(WINDOW_SECONDS)                 # unreachable at once; only finalization's last collection frees it
    print("exiting after a call was in flight", flush=True)
    return 0


_WINDOW = re.compile(rb"^window: finalizing=(\d) in_flight=(\d+)->(\d+)$", re.M)
_EXITING = b"exiting after a call was in flight"


def run(mode: str, unguarded: bool = False, python: str = sys.executable) -> tuple:
    """Runs one mode in a child interpreter: (passed, one line saying how it went)."""
    args = [python, str(Path(__file__).resolve()), mode] + (["--unguarded"] if unguarded else [])
    try:
        out = subprocess.run(args, capture_output=True, timeout=120)
    except subprocess.TimeoutExpired:
        return False, f"{mode}{' unguarded' if unguarded else ''}: no exit in 120 s"
    name = f"{mode}{' unguarded' if unguarded else ''}"
    said = out.stdout.decode(errors="replace").strip().replace("\n", "; ")
    if out.returncode != 0:
        tail = out.stderr.decode(errors="replace").strip().splitlines()[-3:]
        return False, f"{name}: exit {out.returncode}: {said} {' | '.join(tail)}"
    if mode == "barred":
        return b"barred ok" in out.stdout, f"{name}: exit 0: {said}"
    window = _WINDOW.search(out.stdout)
    if _EXITING not in out.stdout:
        return False, f"{name}: it never exited mid-drive: {said}"
    if window is None:
        return False, f"{name}: the window never opened (no finalizer ran during finalization): {said}"
    if window[1] != b"1":
        return False, f"{name}: the window opened before finalization: {said}"
    opened, closed = int(window[2]), int(window[3])
    if not unguarded and (opened, closed) != (0, 0):
        return False, f"{name}: calls still in flight in the window ({opened}->{closed}): {said}"
    return True, f"{name}: exit 0: {said}"


def check(python: str = sys.executable) -> int:
    """Every mode, guarded (judged) and unguarded (reported); 0 when every judged run passed."""
    failed = 0
    for mode, unguarded in (("long", False), ("short", False), ("barred", False), ("long", True), ("short", True)):
        ok, line = run(mode, unguarded, python)
        if unguarded:
            print("report  " + line, flush=True)
        else:
            print(("ok      " if ok else "FAILED  ") + line, flush=True)
            failed += 0 if ok else 1
    print(f"{failed} failed", flush=True)
    return 1 if failed else 0


def main(argv: list) -> int:
    mode = argv[0] if argv else "check"
    if mode == "check":
        return check()
    if mode == "barred":
        return _barred()
    if mode not in ("long", "short"):
        print(__doc__)
        return 2
    return _exit_mid_drive(mode, "--unguarded" in argv)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
