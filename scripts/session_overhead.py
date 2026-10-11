"""The session's overhead (design/phase3-features.md P3.9.7): a 4-bot small game played through the session driver
against the same game driven by ``EngineGame.drive`` alone, measured in the same run.

    python scripts/session_overhead.py [--rounds 100] [--runs 5] [--seed 5002] [--json OUT] [--no-index]

Each run plays ``--rounds`` rounds of the same game (same seed, same four default bots) twice, alternating: once
through ``SessionManager.create`` and the session's driver thread (its drives, metrics, responders, broadcasts,
autosaves and their writer, and the game's row in the games index, kept once a round from the writer, in a database
of the run's own), once in a plain loop of ``EngineGame.drive`` with every bot seat, one seat's turn a call.
The ratio of the two medians is the session's overhead. This laptop throttles, so a ratio is only ever compared with
one measured the same way; package 3-01 recorded the first in its as-built note, and later packages may let it grow by
at most 25%.

Every file it writes goes to a temporary directory of its own (saves, data, database, configuration): it never touches
the default data directory, the checkout's ``config/`` or ``saves/``, and starts no server.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _isolate() -> str:
    """Point every state directory at a new temporary one, before citar is imported."""
    base = tempfile.mkdtemp(prefix="citar-overhead-")
    for var, sub in (("CITAR_SAVE_DIR", "saves"), ("CITAR_DATA_DIR", "data"), ("CITAR_CONFIG_DIR", "config"),
                     ("CITAR_BENCH_DIR", "bench")):
        os.environ[var] = os.path.join(base, sub)
        os.makedirs(os.environ[var], exist_ok=True)
    os.environ["CITAR_DB_URL"] = "sqlite:///" + os.path.join(base, "data", "overhead.db").replace("\\", "/")
    os.environ["CITAR_KEYS_FILE"] = os.path.join(base, "config", "keys.enc")
    os.environ.pop("CITAR_RULESET_DIR", None)
    return base


def _config(seed: int) -> dict:
    """The game both paths play."""
    return {"map_size": "small", "seed": seed}


def through_session(seed: int, rounds: int, timeout: float = 900.0, index: bool = True) -> float:
    """Seconds for ``rounds`` rounds of the game under a session's driver, from its start to the round after. With
    ``index`` the game has its row in the games index (in the run's own database), as a lobby game has, so the
    session keeps it as a server does: once a round, from its save writer."""
    from citar import db
    from citar.server import ownership
    from citar.server import session as sess
    manager = sess.SessionManager()
    s = manager.create(_config(seed), [{"type": "bot"}] * 4, name="overhead", track=False, start=False)
    if index:
        with db.session() as sdb:
            ownership.register(sdb, s, None)
    try:
        t0 = time.perf_counter()
        s.start()
        deadline = t0 + timeout
        while True:
            with s.lock:
                if s.game.turn > rounds or s.game.phase != "playing":
                    break
                s.cond.wait(0.05)
            if time.perf_counter() > deadline:
                raise RuntimeError(f"the session reached turn {s.game.turn} of {rounds + 1} in {timeout:.0f} s")
        took = time.perf_counter() - t0
        if s.errors:
            raise RuntimeError(f"the session failed: {s.errors[-1]}")
        return took
    finally:
        manager.delete(s.id)
        if s._driver is not None:
            s._driver.join(30)
        shutil.rmtree(sess.SAVE_DIR / s.id, ignore_errors=True)


def through_drive(seed: int, rounds: int) -> float:
    """Seconds for the same rounds in a plain loop of ``EngineGame.drive``, one seat's turn a call."""
    from citar.bots import profiles
    from citar.engine_api import EngineGame
    cfg = dict(_config(seed))
    cfg["players"] = [{"controller": "bot"} for _ in range(4)]
    g = EngineGame.new(cfg)
    bots = {pid: profiles.make_bot(None) for pid in range(4)}
    t0 = time.perf_counter()
    while g.turn <= rounds and g.phase == "playing":
        g.drive(bots, seat_limit=1)
    return time.perf_counter() - t0


def main(argv=None) -> int:
    """Measure and print the medians and their ratio."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--rounds", type=int, default=100)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--seed", type=int, default=5002)
    ap.add_argument("--json", help="also write the numbers to this file")
    ap.add_argument("--no-index", action="store_true",
                    help="no games-index row and no database, as the sessions of 3-01's first measurement")
    args = ap.parse_args(argv)
    base = _isolate()
    sys.path.insert(0, str(ROOT))
    try:
        from citar import db, engine_api
        if not args.no_index:
            db.configure(os.environ["CITAR_DB_URL"])
            db.create_all()
        build = engine_api.build_info()
        index = not args.no_index

        def session_path(seed, rounds):
            """The session's path, with or without its row."""
            return through_session(seed, rounds, index=index)

        through_drive(args.seed, 5)                 # warm both paths once: the first game loads the ruleset
        session_path(args.seed, 5)
        session_s, drive_s = [], []
        for i in range(args.runs):
            # alternate which goes first, so a throttle that sets in during a run falls on both
            order = (session_path, through_drive) if i % 2 == 0 else (through_drive, session_path)
            for fn in order:
                t = fn(args.seed, args.rounds)
                (session_s if fn is session_path else drive_s).append(t)
                print(f"run {i + 1} {'through_session' if fn is session_path else 'through_drive'}: {t:.2f} s",
                      flush=True)
        if index:
            db.dispose()
        ms, md = statistics.median(session_s), statistics.median(drive_s)
        out = {"rounds": args.rounds, "runs": args.runs, "seed": args.seed, "index": index,
               "build_id": build.get("build_id"),
               "label": build.get("label"), "session_s": session_s, "drive_s": drive_s,
               "session_median_s": ms, "drive_median_s": md, "ratio": ms / md}
        print(f"session median {ms:.2f} s, drive median {md:.2f} s, ratio {ms / md:.3f}", flush=True)
        if args.json:
            Path(args.json).write_text(json.dumps(out, indent=1), encoding="utf-8")
        return 0
    finally:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
