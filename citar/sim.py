"""Headless bot-vs-bot simulation for testing and balance.

    python -m citar.sim --players 4 --turns 200 --map continents --seed 1 --speed Quick
"""
from __future__ import annotations

import argparse
import time

from .bots.basic import BasicBot
from .engine.game import Game


def resolve_negotiations(g: Game, bots: dict, max_rounds: int = 40):
    """Answer every open negotiation, so a headless game cannot stall on one."""
    for _ in range(max_rounds):
        pending = [n for n in g.s.negotiations if n["status"] == "open" and n["awaiting"] in bots]
        if not pending:
            return
        for n in pending:
            bots[n["awaiting"]].respond(g, n["awaiting"], n["id"])


def run(players: int = 4, turns: int = 0, map_type: str = "continents", map_size: str = "small", seed: int = 1,
        barbarians: str = "normal", verbose: bool = True, speed: str = "Quick", nation: str = None) -> Game:
    """Play one headless bot-vs-bot game, printing what happens."""
    g = Game.new({"map_type": map_type, "map_size": map_size, "seed": seed, "barbarians": barbarians, "speed": speed,
                  "players": [{"controller": "bot", "nation": nation} for _ in range(players)],
                  "turn_limit": turns or None})
    bots = {p.id: BasicBot(aggression=0.3 + 0.2 * p.id, seed=seed * 100 + p.id) for p in g.majors()}
    t0 = time.time()
    last_turn = g.turn
    turn_t = time.time()
    while g.s.phase == "playing":
        pid = g.s.current
        bot = bots[pid]
        bot.play_turn(g, pid, end_turn=False)
        resolve_negotiations(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)
        if g.turn != last_turn:
            if verbose and (g.turn % 10 == 0 or g.s.phase != "playing"):
                st = g.s.stats[-1]["players"]
                row = " | ".join(f"P{k}: sc{v.get('score', 0)} c{v.get('cities', 0)} pop{v.get('population', 0)} "
                                 f"t{v.get('techs', 0)} e{v.get('era', 0)} m{v.get('military', 0)} g{v.get('gold', 0)} "
                                 f"s{v.get('science', 0)} h{v.get('happiness', 0)}"
                                 for k, v in st.items() if v.get("alive"))
                print(f"T{g.turn - 1:>3} ({time.time() - turn_t:.1f}s) {row}", flush=True)
                turn_t = time.time()
            last_turn = g.turn
    if verbose:
        print(f"Game over on turn {g.turn}: winner {g.s.winner} by {g.s.victory} in {time.time() - t0:.1f}s")
        wars = [e["text"] for e in g.s.events if e["type"] in ("war_declared", "peace", "city_captured", "eliminated", "deal")]
        for w in wars[-25:]:
            print("  ", w)
    return g


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
