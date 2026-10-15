"""The seven lab games whose Python results the parity test (``tests/test_jobs.Parity``) holds the Rust LabResult to,
and their recorder: ``tests/data/lab_results_<name>.json``, each ``{"about", "recorded_at", "build", "spec",
"result"}``.

Package 4-07 recorded them before it deleted the Python result building (``citar/lab.py``'s ``play()`` at d6658df),
with that building; ``tests/lab_play_oracle.py`` keeps it verbatim, and this recorder plays them with it. Six games of
40 rounds, three duels and three four-seat small games, each from an experiment run through ``lab.normalize`` and
``lab.game_spec`` as the lab queues it, every seat and map key a lab game can carry used by at least one; and a duel of
130 rounds, for what a short game never reaches (the checkpoints at 50 and 100, eras, wars and captures).

A result records the build that played it (``engine``, each seat's ``build`` and ``fingerprint``), which moves with
any edit of the engine's or the bot's sources and with CITAR's version. The parity test compares the recorded lines
only while the build is the one that recorded them, and otherwise compares the old building, live, with the new on
the same games (which it does on every build). To record them again under the build at hand, from the repository's
root with the extension ``cargo xtask develop`` built::

    python -m tests.lab_results --force

and commit the files, saying why. Without ``--force`` a file that exists is kept.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

DATA = Path(__file__).resolve().parent / "data"
TURNS = 40
LONG = 130

#: (name, game index, experiment): the file is ``lab_results_<name>.json``.
EXPERIMENTS = [
    ("duel-long", 0, {
        "name": "parity-duel-long", "games": 2, "seed": 4400, "size": "duel", "maps": ["pangaea"],
        "speed": "Quick", "turns": LONG, "difficulty": "Prince", "barbarians": "normal",
        "seats": [{"label": "warlike", "bot": "basic", "aggression": 1.0},
                  {"label": "steady", "bot": "basic", "aggression": 0.6}]}),
    ("duel-continents", 0, {
        "name": "parity-duel-a", "games": 3, "seed": 4100, "size": "duel", "maps": ["continents"],
        "speed": "Quick", "turns": TURNS, "difficulty": "Prince", "barbarians": "normal",
        "seats": [{"label": "alpha", "bot": "basic"}, {"label": "beta", "bot": "basic-1"}]}),
    ("duel-pangaea", 1, {
        "name": "parity-duel-b", "games": 3, "seed": 4200, "size": "duel", "maps": ["continents", "pangaea"],
        "speed": "Quick", "turns": TURNS, "difficulty": "King", "barbarians": "off", "nation": "BenchmarkCiv",
        "seats": [{"label": "fixed", "bot": "basic", "aggression": 0.8, "difficulty": "Warlord",
                   "params": {"tech_noise": 0.2}},
                  {"label": "idle", "bot": "idle", "nation": "Rome"}]}),
    ("duel-fractal", 2, {
        "name": "parity-duel-c", "games": 3, "seed": 4300, "size": "duel", "maps": ["fractal"],
        "speed": "Quick", "turns": TURNS, "difficulty": "Prince", "barbarians": "raging",
        "barbarian_difficulty": "Chieftain", "map_edges": "wrap_x", "river_density": 1.5,
        "seats": [{"profile": "classic-production"}, {"label": "tuned", "bot": "basic",
                                                       "params": {"prod_mode": "classic", "u_food": 4.5}}]}),
    ("small-continents", 0, {
        "name": "parity-small-a", "games": 4, "seed": 5100, "size": "small", "maps": ["continents"],
        "speed": "Quick", "turns": TURNS, "difficulty": "Prince", "barbarians": "normal", "city_states": 2,
        "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}, {"label": "c", "bot": "basic"},
                  {"label": "d", "bot": "basic"}]}),
    ("small-archipelago", 1, {
        "name": "parity-small-b", "games": 4, "seed": 5200, "size": "small", "maps": ["archipelago"],
        "speed": "Quick", "turns": TURNS, "difficulty": "Prince", "barbarians": "normal",
        "ai_base_values": "monotonic", "players": 4, "bot": "basic",
        "factors": {"tech_noise": [0.0, 0.3], "aggressive_above": [0.4, 0.7]}}),
    ("small-inland-sea", 3, {
        "name": "parity-small-c", "games": 4, "seed": 5300, "size": "small", "maps": ["inland_sea"],
        "speed": "Quick", "turns": TURNS, "difficulty": "Prince", "barbarians": "normal", "city_states": 1,
        "resources": {"density": 1.5, "luxury": {"density": 0.5},
                      "strategic": {"each": {"Iron": {"mode": "cap", "value": 2}, "Horses": {"mode": "off"}}}},
        "seats": [{"profile": "standard"}, {"label": "idle", "bot": "idle"},
                  {"label": "warlike", "bot": "basic", "aggression": 1},
                  {"label": "calm", "bot": "basic", "aggression": 0.1, "difficulty": "Settler"}]}),
]

ABOUT = ("A lab game played by the Python result building (citar/lab.py play() before package 4-07 moved it to Rust; "
         "tests/lab_play_oracle.py keeps it), recorded by tests/lab_results.py for the parity test: the Rust LabResult "
         "of `spec` equals `result` on every deterministic key (all but seconds, started_ts, cpu_s and finished) while "
         "the build is `build`.")


def record(force: bool = False, out=print) -> list[Path]:
    """Plays each game with the old building and writes its file (kept when it exists, unless ``force``)."""
    from citar import engine_api, lab
    from tests import lab_play_oracle
    build = engine_api.build_info()
    written = []
    for name, i, exp in EXPERIMENTS:
        path = DATA / f"lab_results_{name}.json"
        if path.exists() and not force:
            out(f"{path.name} exists: kept")
            continue
        spec = lab.game_spec(lab.normalize(exp), i)
        t0 = time.perf_counter()
        result = lab_play_oracle.play(spec)
        doc = {"about": ABOUT, "recorded_at": time.strftime("%Y-%m-%d"),
               "build": {k: build[k] for k in ("version", "build_id", "engine_code", "bot_code", "rules")},
               "spec": spec, "result": result}
        path.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8", newline="\n")
        written.append(path)
        out(f"{name}: {result.get('turns')} turns, winner {result.get('winner_label')}, "
            f"{time.perf_counter() - t0:.1f} s -> {path.name}")
    return written


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--force", action="store_true", help="record every game again, over the files that exist")
    args = ap.parse_args(argv)
    record(args.force)


if __name__ == "__main__":
    sys.exit(main())
