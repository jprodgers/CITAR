"""Shared plumbing for the reference-check tools (record.py, baseline.py, summarize.py).

These tools talk to the Python engine directly (citar.engine, citar.bots.basic). They carry their own game loop
rather than borrowing citar.sim, citar.lab or citar.balance, so that the corpus and the baseline are produced by
the same code however those modules change.

Everything here is deterministic given a seed: games are seeded, the bots are seeded from the game's seed, and the
scripts re-run themselves with PYTHONHASHSEED=0 so that anything iterating a set of strings comes out in the same
order on every run (tests/test_bots.py checks that games themselves do not depend on it).

Runs are long and unattended, so no single game may hold one up: every game has a time budget (Deadline), and the
parallel runner (run_parallel) stops cleanly when a worker is stuck beyond that or dies.
"""
from __future__ import annotations

import base64
import ctypes
import functools
import gzip
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import traceback
from concurrent.futures import FIRST_COMPLETED, ProcessPoolExecutor, wait
from concurrent.futures.process import BrokenProcessPool
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

REFCHECK = ROOT / "refcheck"
MAP_TYPES = ("continents", "pangaea", "archipelago", "inland_sea", "fractal")
BARBARIANS = ("off", "normal", "raging")
# a Quick game on a small map takes about four minutes; the budget is there to catch a game that hangs, not a slow one
BUDGET_MINUTES_SMALL = 60
HARD_GRACE = 300          # seconds past the budget before the watchdog interrupts a game stuck inside one turn


def ensure_hash_seed():
    """Re-run the current script with PYTHONHASHSEED=0 unless it already has it.

    The hash seed only takes effect at interpreter start, so it cannot be set from inside; re-running is the only
    way to make "python scripts/refcheck/record.py" produce the same bytes every time without the caller having to
    remember an environment variable. Worker processes inherit the environment.
    """
    if os.environ.get("PYTHONHASHSEED") == "0":
        return
    env = dict(os.environ, PYTHONHASHSEED="0")
    sys.exit(subprocess.call([sys.executable, *sys.argv], env=env))


def default_workers() -> int:
    """Worker processes to use by default: all cores but four, which leaves the machine usable meanwhile."""
    return max(1, (os.cpu_count() or 2) - 4)


def _hash_files(paths) -> str:
    """A short sha1 over the names and contents of some files, in a fixed order."""
    h = hashlib.sha1()
    for p in paths:
        h.update(p.name.encode())
        h.update(p.read_bytes())
    return h.hexdigest()[:10]


@functools.cache
def engine_hash() -> str:
    """A hash of the engine and ruleset (the same recipe as citar.lab.engine_hash, so the two agree).

    Taken once per process: a worker imports the engine once and plays every later game with that code, so its
    first reading is the one that describes all its games, even if the files change on disk meanwhile.
    """
    from citar import paths
    files = []
    for d in (paths.PACKAGE / "engine", paths.package_data()):
        files += [p for p in sorted(d.rglob("*")) if p.suffix in (".py", ".json") and "__pycache__" not in p.parts]
    return _hash_files(files)


@functools.cache
def bot_hash() -> str:
    """A hash of the live bot, which decides what the recorded games look like (once per process, as above)."""
    from citar import paths
    return _hash_files([paths.PACKAGE / "bots" / "basic.py"])


def error_text(e: BaseException) -> str:
    """An exception as one line for a file: its type, its message, and the innermost function of this repository
    it passed through, as module.function.

    No file paths or line numbers: files are compared across machines and across edits that only move code, and
    a path would also leak the recording machine's folders into committed fixtures. Callers print the full
    traceback to the console instead.
    """
    where = None
    for fr in reversed(traceback.extract_tb(e.__traceback__)):
        path = Path(fr.filename)
        if not path.is_absolute():            # "<string>", "<frozen ...>": no module of ours
            continue
        try:
            rel = path.resolve().relative_to(ROOT)
        except ValueError:
            continue
        where = ".".join(rel.with_suffix("").parts) + "." + fr.name
        break
    return f"{type(e).__name__}: {e}" + (f" (in {where})" if where else "")


def print_trace(header: str):
    """The traceback of the exception being handled, to the console (stderr), under a one-line header."""
    print(f"{header}\n{traceback.format_exc()}", file=sys.stderr, flush=True)


# ----------------------------------------------------------------------------
# time budgets and the parallel runner
# ----------------------------------------------------------------------------
def time_budget(size: str, minutes: float = 0) -> float:
    """Seconds one game on a *size* map may run: *minutes* if given, else BUDGET_MINUTES_SMALL scaled by the map's
    area against a small map's, and never under 20 minutes."""
    if minutes:
        return minutes * 60
    from citar.engine.rules import get_rules
    sizes = get_rules().const["map_sizes"]

    def area(s):
        """Tiles on a map of size *s*."""
        return sizes[s]["width"] * sizes[s]["height"]
    return max(20.0, BUDGET_MINUTES_SMALL * area(size) / area("small")) * 60


def stall_limit(budgets) -> float:
    """Seconds the parallel runner waits for any job to finish before it calls the run stuck: the longest budget,
    plus the watchdog's grace, plus five minutes. Every running job ends within its budget and grace of starting,
    and one starts only when another finishes, so a longer silence means a worker is stuck beyond its watchdog."""
    return max(budgets, default=0) + HARD_GRACE + 300


class GameTimeout(BaseException):
    """A game ran past its time budget. A BaseException, like KeyboardInterrupt, so that the engine's and the bot's
    own ``except Exception`` handlers cannot swallow it on its way out."""


def _async_raise(tid: int, exc):
    """Raise the exception class *exc* in thread *tid* at its next bytecode, or withdraw one not yet delivered
    (*exc* None)."""
    ctypes.pythonapi.PyThreadState_SetAsyncExc(ctypes.c_ulong(tid), ctypes.py_object(exc) if exc else None)


class Deadline:
    """One game's time budget, enforced twice. Use it as a context manager around the game.

    ``check(g)``, which play() calls at the start of every round, raises GameTimeout at a clean point once the
    budget is spent. A game stuck inside one turn (a bot or engine loop that never ends) never reaches that point,
    so a watchdog thread also raises GameTimeout inside the game's own thread, wherever it is, HARD_GRACE seconds
    later. Pure-Python code takes it at its next bytecode, so the worker is free for the next game and the
    traceback shows where the game was stuck. Code blocked inside C is out of its reach: run_parallel's stall
    limit covers that.
    """

    def __init__(self, seconds: float):
        self.seconds = seconds
        self.at = time.monotonic() + seconds
        self._lock = threading.Lock()
        self._armed = self._fired = False
        self._timer = self._tid = None

    def check(self, g):
        """Raise GameTimeout if the budget is spent."""
        if time.monotonic() > self.at:
            raise GameTimeout(f"still playing at turn {g.turn} after its {self.seconds / 60:.3g}-minute budget")

    def _fire(self):
        """The watchdog: interrupt the game's thread, unless the game has already finished."""
        with self._lock:
            if self._armed:
                self._fired = True
                _async_raise(self._tid, GameTimeout)

    def __enter__(self):
        self._tid = threading.get_ident()
        self._armed = True
        self._timer = threading.Timer(self.seconds + HARD_GRACE, self._fire)
        self._timer.daemon = True
        self._timer.start()
        return self

    def __exit__(self, *exc):
        with self._lock:
            self._armed = False
            if self._fired:
                _async_raise(self._tid, None)      # it fired as the game finished: don't let it land after this
        self._timer.cancel()
        return False


def _terminate(ex: ProcessPoolExecutor):
    """Stop a pool now, without waiting for a worker that may never return."""
    if hasattr(ex, "terminate_workers"):          # Python 3.14+
        ex.terminate_workers()
        return
    procs = list((ex._processes or {}).values())
    ex.shutdown(wait=False, cancel_futures=True)
    for p in procs:
        if p.is_alive():
            p.terminate()


def run_parallel(fn, jobs: list, workers: int, stall_seconds: float):
    """Run ``fn(job)`` for every job in worker processes, yielding ``(job, result, None)`` as each finishes.

    Two failures that the jobs' own deadlines cannot handle stop the run, and the jobs caught in them are yielded
    as ``(job, None, why)``:

    - no job finishing for *stall_seconds* (see stall_limit): a worker is stuck where its watchdog cannot reach;
    - a worker process dying (killed, or out of memory), which takes every running job with it.

    Jobs not yet started are not yielded; the caller's resume plays them. Either way, and if the caller stops
    early, the worker processes are terminated, so a stuck one cannot keep the script alive.
    """
    ex = ProcessPoolExecutor(max_workers=max(1, min(workers, len(jobs))))
    futs = {ex.submit(fn, j): j for j in jobs}
    pending, finished = set(futs), False
    try:
        while pending:
            running = {f for f in pending if f.running()}
            done, pending = wait(pending, timeout=stall_seconds, return_when=FIRST_COMPLETED)
            if not done:
                why = (f"unfinished when the run stopped: no job finished for {stall_seconds / 60:.0f} minutes, so a "
                       f"worker is stuck where its own deadline cannot stop it (blocked in C code?)")
                for f in running | {f for f in pending if f.running()}:
                    yield futs[f], None, why
                return
            broken = False
            for f in done:
                try:
                    r = f.result()
                except BrokenProcessPool:
                    broken = True
                    if f in running:
                        yield futs[f], None, "its worker process died (killed, or out of memory?)"
                    continue
                except Exception as e:
                    yield futs[f], None, f"the job raised {type(e).__name__}: {e}"
                    continue
                yield futs[f], r, None
            if broken:
                return
        finished = True
    finally:
        if finished:
            ex.shutdown()
        else:
            _terminate(ex)


# ----------------------------------------------------------------------------
# games
# ----------------------------------------------------------------------------
def default_players(size: str) -> int:
    """The number of major civilizations a map size is made for."""
    from citar.engine.rules import get_rules
    return get_rules().const["map_sizes"][size]["players"]


def game_config(seed: int, size: str, map_type: str, barbarians: str = "normal", players: int = 0,
                speed: str = "Quick", difficulty: str = "Prince", nation=None, turn_limit=None) -> dict:
    """The Game.new configuration of an all-bot game. Recorded with every result, so a game can be replayed."""
    n = players or default_players(size)
    return {"map_type": map_type, "map_size": size, "seed": seed, "barbarians": barbarians, "speed": speed,
            "difficulty": difficulty, "turn_limit": turn_limit,
            "players": [{"controller": "bot", "nation": nation} for _ in range(n)]}


def new_game(config: dict):
    """A new game from a configuration made by game_config."""
    from citar.engine.game import Game
    return Game.new(json.loads(json.dumps(config)))


def make_bots(g, seed: int) -> dict:
    """One live bot per major civilization, seeded from the game's seed.

    Aggression is spread by seat and seed (the lab's formula), so a baseline covers peaceful and warlike bots in
    every start position rather than one temperament.
    """
    from citar.bots.basic import BasicBot
    return {p.id: BasicBot(aggression=0.25 + 0.5 * ((p.id * 37 + seed) % 10) / 9, seed=seed * 101 + p.id)
            for p in g.majors(alive_only=False)}


def resolve_negotiations(g, bots: dict, max_rounds: int = 40):
    """Answer every open negotiation waiting on a bot, so a headless game cannot stall on one."""
    for _ in range(max_rounds):
        pending = [n for n in g.s.negotiations if n["status"] == "open" and n["awaiting"] in bots]
        if not pending:
            return
        for n in pending:
            bots[n["awaiting"]].respond(g, n["awaiting"], n["id"])


class BotErrors(Exception):
    """Too many bot crashes in one game: the game is no longer a fair sample."""


def play(g, bots: dict, on_round=None, errors: list = None, max_errors: int = 20, deadline: Deadline = None):
    """Play an all-bot game until it ends or *on_round* returns False.

    ``on_round(g)`` is called once at the start of every round, when the first living major civilization's turn
    has begun and nothing else has happened yet: that is the moment checkpoints are taken, and the moment a
    scenario may edit the game. A bot that raises is logged into *errors* (one line each, see error_text; the
    traceback goes to the console) and its turn ends, as in the lab; more than *max_errors* crashes raise
    BotErrors. A *deadline* is checked at the start of every round.
    """
    errors = errors if errors is not None else []
    seen = None
    while g.s.phase == "playing":
        if g.turn != seen:
            seen = g.turn
            g.frames.clear()           # replay frames are not state, and a long game's would fill memory
            if deadline is not None:
                deadline.check(g)
            if on_round is not None and on_round(g) is False:
                return
            if g.s.phase != "playing":
                return
        pid = g.s.current
        bot = bots.get(pid)
        if bot is not None:
            try:
                bot.play_turn(g, pid, end_turn=False)
            except Exception as e:
                errors.append(f"T{g.turn} P{pid}: {error_text(e)}")
                print_trace(f"bot error, seed {g.s.config.get('seed')} turn {g.turn} player {pid}:")
                if len(errors) > max_errors:
                    raise BotErrors(f"{len(errors)} bot errors; the last: {errors[-1]}")
            resolve_negotiations(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)


# ----------------------------------------------------------------------------
# state and files
# ----------------------------------------------------------------------------
def _default(o):
    """JSON for the few non-JSON values answers contain: sets (sorted) and bytes (base64)."""
    if isinstance(o, (set, frozenset)):
        return sorted(o, key=lambda x: (str(type(x)), x))
    if isinstance(o, (bytes, bytearray)):
        return base64.b64encode(bytes(o)).decode("ascii")
    raise TypeError(f"not JSON serialisable: {type(o).__name__}")


def dumps(obj) -> str:
    """Compact JSON, key order as the engine produced it, and no NaN or Infinity (Rust's parser rejects them)."""
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False, allow_nan=False, default=_default)


def state_text(g) -> str:
    """The saved part of a game as JSON text: what a fresh load starts from."""
    return dumps(g.s.to_dict())


def load_game(text: str):
    """A fresh game from state JSON, with every cache cold. Visibility is computed lazily on first use."""
    from citar.engine.game import Game
    from citar.engine.state import GameState
    return Game(GameState.from_dict(json.loads(text)))


def settle(g) -> str:
    """The game's state after a load and a visibility refresh, as JSON text.

    A state is recorded in this form because it is a fixed point: loading it and refreshing visibility (which
    marks tiles explored and meets civilizations) changes nothing more, so the Python answers and any other
    engine's start from exactly the recorded bytes.
    """
    from citar.engine import visibility
    g2 = load_game(state_text(g))
    visibility.refresh(g2, force=True)
    return state_text(g2)


def write_gz(path: Path, text: str):
    """Write gzip JSON with a zero timestamp and no file name, so the same content gives the same bytes."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as f:
        f.write(text.encode("utf-8"))
    os.replace(tmp, path)


def read_gz(path: Path) -> dict:
    """Read one gzip JSON file."""
    with gzip.open(path, "rt", encoding="utf-8") as f:
        return json.load(f)


class Timer:
    """Wall-clock seconds since creation, rounded for logs."""
    def __init__(self):
        self.t0 = time.time()

    def __call__(self) -> float:
        return round(time.time() - self.t0, 1)
