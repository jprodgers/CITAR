"""Shared plumbing for the reference-check tools (record.py, baseline.py, summarize.py).

These tools talk to the Python engine directly (citar.engine, citar.bots.basic). They carry their own game loop
rather than borrowing citar.sim, citar.lab or citar.balance, so that the corpus and the baseline are produced by
the same code however those modules change.

Everything here is deterministic given a seed: games are seeded, the bots are seeded from the game's seed, and the
scripts re-run themselves with PYTHONHASHSEED=0 so that anything iterating a set of strings comes out in the same
order on every run (tests/test_bots.py checks that games themselves do not depend on it).
"""
from __future__ import annotations

import base64
import gzip
import hashlib
import json
import os
import subprocess
import sys
import time
import traceback
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

REFCHECK = ROOT / "refcheck"
MAP_TYPES = ("continents", "pangaea", "archipelago", "inland_sea", "fractal")
BARBARIANS = ("off", "normal", "raging")


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


def engine_hash() -> str:
    """A hash of the engine and ruleset (the same recipe as citar.lab.engine_hash, so the two agree)."""
    from citar import paths
    files = []
    for d in (paths.PACKAGE / "engine", paths.package_data()):
        files += [p for p in sorted(d.rglob("*")) if p.suffix in (".py", ".json") and "__pycache__" not in p.parts]
    return _hash_files(files)


def bot_hash() -> str:
    """A hash of the live bot, which decides what the recorded games look like."""
    from citar import paths
    return _hash_files([paths.PACKAGE / "bots" / "basic.py"])


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


def play(g, bots: dict, on_round=None, errors: list = None, max_errors: int = 20):
    """Play an all-bot game until it ends or *on_round* returns False.

    ``on_round(g)`` is called once at the start of every round, when the first living major civilization's turn
    has begun and nothing else has happened yet: that is the moment checkpoints are taken, and the moment a
    scenario may edit the game. A bot that raises is logged into *errors* and its turn ends, as in the lab; more
    than *max_errors* crashes raise BotErrors.
    """
    errors = errors if errors is not None else []
    seen = None
    while g.s.phase == "playing":
        if g.turn != seen:
            seen = g.turn
            g.frames.clear()           # replay frames are not state, and a long game's would fill memory
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
                errors.append(f"T{g.turn} P{pid}: {type(e).__name__}: {e}\n{traceback.format_exc(limit=5)}")
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
