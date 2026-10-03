"""Exits the interpreter while a daemon thread is inside a call to the Rust engine (DESIGN.md P2.6.4).

A session's driver is a daemon thread, and it may be inside ``Game.drive`` when the server exits. A thread that comes
back from a call with the GIL released and re-attaches to a finalizing interpreter is ended from inside
``PyEval_RestoreThread`` (``pthread_exit`` on Python 3.11 to 3.13), an unwind through the extension's Rust frames that
aborts the process. ``citar._engine`` registers ``shutdown`` with atexit so no thread does; this script is the check.
tests/test_engine_module.py runs it with the interpreter it runs under, and CI's interpreter-exit job on each Python
version; it needs only ``citar._engine`` (built by ``cargo xtask develop`` or unpacked into ``citar/``).

    python tests/engine_exit_child.py long|short [--unguarded]

``long``: the thread is inside one long drive (a whole game in a call) when the interpreter exits; ``short``: it drives
one seat per call, so it is as likely to be between calls, or entering one, as inside one. ``--unguarded`` takes the
atexit hook away, to show what it guards against. Prints ``exiting`` and exits 0; anything else is a failure.
"""
import atexit
import json
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))     # the checkout's citar, run as a script

from citar import _engine as E


def _game(seed: int):
    cfg = {"seed": seed, "map_size": "small", "players": [{"controller": "bot"}] * 4}
    return E.Game.new(json.dumps(cfg).encode())


def _drive(seat_limit: int):
    bots = {p: E.Bot("idle") for p in range(4)}
    seed = 1
    while True:
        g = _game(seed)
        while g.phase == "playing":
            g.drive(bots, seat_limit)
        seed += 1


def main(argv: list) -> int:
    mode = argv[0] if argv else "long"
    if "--unguarded" in argv:
        atexit.unregister(E.shutdown)
    seat_limit = 0 if mode == "long" else 1
    threading.Thread(target=_drive, args=(seat_limit,), daemon=True).start()
    # Exit while a call is in flight: wait until one is, and a little more in the short mode, so the thread is
    # anywhere in its loop.
    deadline = time.monotonic() + 30
    while E.calls_in_flight() == 0 and time.monotonic() < deadline:
        time.sleep(0.001)
    if mode != "long":
        time.sleep(0.05)
    print("exiting", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
