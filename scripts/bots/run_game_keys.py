"""Record the shape of a Python ``engine_api.run_game`` result: crates/citar-sim/tests/data/run_game_keys.json
(crates/citar-engine DESIGN.md P2.4.1, package 2-04).

    python scripts/bots/run_game_keys.py            # writes the file
    python scripts/bots/run_game_keys.py --check    # exits 1 when the file is not what run_game gives now

The Rust runner (``citar_sim::run_game``) must return ``run_game``'s dict, so that the lab, ``citar sim`` and the
balance runner read either backend's result alike. Values differ between the engines (another map generator, another
bot), so what is held equal is the shape: every key, at every depth, and the type of every value. The file holds the
duel the shape was taken from (``config``, played by ``basic`` bots in both seats) and its shape, which the Rust test
(crates/citar-sim/tests/run_game.rs) compares with the shape of its own result on the same configuration.

A shape is written as JSON:

- ``"null"``, ``"bool"``, ``"number"`` (an int or a float: the engines may write ``3`` or ``3.0``) or ``"string"``;
- ``{"list": [shape, ...]}``: the distinct shapes of the elements, sorted by their JSON text, so a list of rows of
  two kinds (the majors' and the others' in ``players``) gives two;
- ``{"by_id": [shape, ...]}``: a dict keyed by ids (every key a decimal integer, as the stats rows' ``players``), with
  the distinct shapes of its values: the number of players is not part of the shape;
- ``{"object": {key: shape, ...}}``: any other dict, its keys sorted.

This script goes with the Python engine (package 2-12).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

OUT = ROOT / "crates" / "citar-sim" / "tests" / "data" / "run_game_keys.json"
#: The duel the shape is taken from: city-states and barbarians at their defaults, so every kind of player has a row,
#: and a turn limit, so the game ends with a winner and a victory
CONFIG = {"seed": 11, "map_size": "duel", "map_type": "pangaea", "speed": "Quick", "turn_limit": 15,
          "players": [{"controller": "bot"}, {"controller": "bot"}]}


def shape(v):
    """The shape of a JSON-like value (see the module's doc)."""
    if v is None:
        return "null"
    if isinstance(v, bool):                 # before int: a bool is an int in Python
        return "bool"
    if isinstance(v, (int, float)):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, (list, tuple)):
        return {"list": distinct(shape(x) for x in v)}
    if isinstance(v, dict):
        if v and all(isinstance(k, str) and k.isdigit() for k in v):
            return {"by_id": distinct(shape(x) for x in v.values())}
        return {"object": {str(k): shape(x) for k, x in sorted(v.items())}}
    raise TypeError(f"not a JSON value: {type(v).__name__}")


def distinct(shapes) -> list:
    """Shapes without repeats, sorted by their JSON text."""
    seen = {canonical(s): s for s in shapes}
    return [seen[k] for k in sorted(seen)]


def canonical(v) -> str:
    """Compact JSON with sorted keys, the text shapes are compared and sorted by."""
    return json.dumps(v, sort_keys=True, separators=(",", ":"))


def record() -> dict:
    """Play the duel through run_game and take its result's shape."""
    # The Python engine's facade whatever CITAR_ENGINE says: the file is the Python result's shape, which the Rust
    # runner is held to (recorded through citar.engine_api on the Rust backend, it would hold Rust to itself).
    from citar.engine import facade as engine_api
    bots = {pid: engine_api.bot_instance("basic", aggression=0.4, seed=1 + pid) for pid in range(2)}
    result = engine_api.run_game({"config": json.loads(json.dumps(CONFIG)), "bots": bots, "raise_errors": True})
    return {"_doc": "The key and type shape of engine_api.run_game's result on this duel (scripts/bots/"
                    "run_game_keys.py); citar-sim's run_game must give the same shape.",
            "config": CONFIG, "bots": "basic", "shape": shape(result)}


def main() -> int:
    """The command line."""
    if os.environ.get("PYTHONHASHSEED") != "0":
        return subprocess.call([sys.executable, *sys.argv], env=dict(os.environ, PYTHONHASHSEED="0"))
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="exit 1 when the file is not what run_game gives now")
    a = ap.parse_args()
    text = json.dumps(record(), indent=1, sort_keys=True) + "\n"
    if a.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print(f"{OUT.relative_to(ROOT)}: {'up to date' if same else 'differs from what run_game gives now'}")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
