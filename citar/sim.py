"""Headless bot-vs-bot simulation for testing and balance.

    python -m citar.sim --players 4 --turns 200 --map continents --seed 1 --speed Quick
"""
from __future__ import annotations

import argparse
import time

from . import engine_api

# the events worth printing at the end of a game
_HEADLINES = ("war_declared", "peace", "city_captured", "eliminated", "deal")


def run(players: int = 4, turns: int = 0, map_type: str = "continents", map_size: str = "small", seed: int = 1,
        barbarians: str = "normal", verbose: bool = True, speed: str = "Quick", nation: str = None) -> dict:
    """Play one headless bot-vs-bot game, printing what happens. Returns engine_api.run_game's result."""
    config = {"map_type": map_type, "map_size": map_size, "seed": seed, "barbarians": barbarians, "speed": speed,
              "players": [{"controller": "bot", "nation": nation} for _ in range(players)], "turn_limit": turns or None}
    # the majors are the first players of a new game
    bots = {pid: engine_api.bot_instance("basic", aggression=0.3 + 0.2 * pid, seed=seed * 100 + pid)
            for pid in range(players)}
    headlines = []
    t0 = time.time()
    turn_t = [time.time()]

    def on_turn(info):
        """Print a line of standings every ten turns, and at the end."""
        if verbose and info["turn"] > 1 and (info["turn"] % 10 == 0 or info["phase"] != "playing"):
            st = info["last_stats"]["players"]
            row = " | ".join(f"P{k}: sc{v.get('score', 0)} c{v.get('cities', 0)} pop{v.get('population', 0)} "
                             f"t{v.get('techs', 0)} e{v.get('era', 0)} m{v.get('military', 0)} g{v.get('gold', 0)} "
                             f"s{v.get('science', 0)} h{v.get('happiness', 0)}"
                             for k, v in st.items() if v.get("alive"))
            print(f"T{info['turn'] - 1:>3} ({time.time() - turn_t[0]:.1f}s) {row}", flush=True)
            turn_t[0] = time.time()

    def on_event(ev):
        """Keep the headlines for the summary."""
        if ev["type"] in _HEADLINES:
            headlines.append(ev["text"])

    r = engine_api.run_game({"config": config, "bots": bots}, on_turn=on_turn, on_event=on_event)
    if verbose:
        print(f"Game over on turn {r['turn']}: winner {r['winner']} by {r['victory']} in {time.time() - t0:.1f}s")
        for w in headlines[-25:]:
            print("  ", w)
    return r


def main():
    """The ``citar sim`` command line."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--players", type=int, default=4)
    ap.add_argument("--turns", type=int, default=0, help="0 = the speed's time-victory turn")
    ap.add_argument("--map", default="continents")
    ap.add_argument("--size", default="small")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--barbarians", default="normal")
    ap.add_argument("--speed", default="Quick")
    ap.add_argument("--nation", default=None, help="e.g. BenchmarkCiv (default: random civilizations)")
    a = ap.parse_args()
    run(a.players, a.turns, a.map, a.size, a.seed, a.barbarians, speed=a.speed, nation=a.nation)


if __name__ == "__main__":
    main()
