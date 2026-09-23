"""The statistical baseline: many seeded all-bot games on the Python engine, one JSON line per game.

    python scripts/refcheck/baseline.py --smoke                       # 2 short games, to prove the pipeline works
    python scripts/refcheck/baseline.py --games 300                   # small maps, every map type in turn
    python scripts/refcheck/baseline.py --games 120 --sizes duel,small,standard --name python-mixed
    python scripts/refcheck/summarize.py refcheck/baseline/<name>.jsonl [other.jsonl]

The Rust engine will not reproduce the Python engine's games (it has its own map generator and RNG), so it is
compared on distributions instead: cities, population, techs and score at turns 100/200/300, victory types, game
length, wars and captures (see refcheck/README.md). This script makes the Python side of that comparison.

Game i of a run uses seed --seed + i and the i-th combination of --sizes x --maps x --barbarians, so a run can be
stopped and resumed: games already in the output file are skipped. Output goes to refcheck/baseline/<name>.jsonl
(git-ignored); the default name carries the engine hash, so results from different code do not mix. Games run in
parallel processes (--workers, default: all cores but four).
"""
from __future__ import annotations

import argparse
import itertools
import json
import sys
import time
import traceback
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import common

CHECKPOINTS = (100, 200, 300)
SMOKE_CHECKPOINTS = (10, 20, 30)
STAT_KEYS = ("cities", "population", "techs", "score", "military", "era", "policies", "land")


def game_specs(a) -> list[dict]:
    """Every game of the run, in order."""
    combos = list(itertools.product(a.sizes, a.maps, a.barbarians))
    out = []
    for i in range(a.games):
        size, map_type, barb = combos[i % len(combos)]
        out.append({"i": i, "seed": a.seed + i, "size": size, "map_type": map_type, "barbarians": barb,
                    "speed": a.speed, "turn_limit": a.turn_limit or None, "checkpoints": list(a.checkpoints)})
    return out


class Tally:
    """Running totals, per civilization, of wars declared and cities captured and lost, kept from the game's events
    (the engine records no such counters itself). Attach it with ``g.listeners.append(tally)``."""
    KEYS = ("wars_declared", "wars_declared_on_majors", "wars_declared_on_others", "cities_captured", "cities_lost")

    def __init__(self, g):
        self.g = g
        self.counts = defaultdict(Counter)

    def __call__(self, ev: dict):
        """Count one event, if it is one the comparison uses."""
        d = ev.get("data") or {}
        if ev["type"] == "war_declared" and d.get("attacker") is not None:
            on = d.get("defender")
            kind = "majors" if on is not None and self.g.player(on).kind == "major" else "others"
            self.counts[d["attacker"]]["wars_declared"] += 1
            self.counts[d["attacker"]][f"wars_declared_on_{kind}"] += 1
        elif ev["type"] == "city_captured":
            if d.get("new_owner") is not None:
                self.counts[d["new_owner"]]["cities_captured"] += 1
            if d.get("old_owner") is not None:
                self.counts[d["old_owner"]]["cities_lost"] += 1

    def of(self, pid: int) -> dict:
        """One civilization's totals so far, every key present."""
        return {k: self.counts[pid].get(k, 0) for k in self.KEYS}


def play_one(spec: dict) -> dict:
    """Play one game and summarise it. Runs in a worker process; a crash is returned, not raised."""
    t0, cpu0 = time.time(), time.process_time()
    try:
        from citar.engine import victory
        cfg = common.game_config(spec["seed"], spec["size"], spec["map_type"], spec["barbarians"],
                                 speed=spec["speed"], turn_limit=spec["turn_limit"])
        g = common.new_game(cfg)
        bots = common.make_bots(g, spec["seed"])
        majors = [p.id for p in g.majors(alive_only=False)]
        tally = Tally(g)
        snaps = {}                             # checkpoint turn -> the totals at the end of that turn

        def on_round(g):
            """At the start of round T+1, the totals are those at the end of turn T, like the engine's stats."""
            done = g.turn - 1
            if done in spec["checkpoints"]:
                snaps[done] = {pid: tally.of(pid) for pid in majors}
            return True

        g.listeners.append(tally)
        errors = []
        common.play(g, bots, on_round=on_round, errors=errors)
        stats = {e["turn"]: e["players"] for e in g.s.stats}
        last = max(stats) if stats else None
        civs = []
        for pid in majors:
            p = g.player(pid)
            at = {}
            for cp in spec["checkpoints"]:
                row = stats.get(cp, {}).get(str(pid))
                if row is not None:
                    at[str(cp)] = _row(row, snaps.get(cp, {}).get(pid, {}))
            if last is not None:
                at["end"] = _row(stats[last].get(str(pid), {"alive": False}), tally.of(pid))
                at["end"]["turn"] = last
            civs.append({"pid": pid, "nation": p.nation, "aggression": round(bots[pid].aggression, 3),
                         "alive": p.alive, "eliminated_turn": p.eliminated_turn,
                         "final_score": victory.score(g, pid)["total"] if p.alive else 0, "at": at})
        return {**{k: spec[k] for k in ("i", "seed", "size", "map_type", "barbarians", "speed")},
                "players": len(majors), "turns": g.turn - 1, "winner": g.s.winner, "victory": g.s.victory,
                "civs": civs, "bot_errors": len(errors), "engine": common.engine_hash(), "bot": common.bot_hash(),
                "seconds": round(time.time() - t0, 1), "cpu_s": round(time.process_time() - cpu0, 1)}
    except Exception as e:
        return {**{k: spec[k] for k in ("i", "seed", "size", "map_type", "barbarians", "speed")},
                "crash": f"{type(e).__name__}: {e}", "trace": traceback.format_exc(limit=8),
                "seconds": round(time.time() - t0, 1)}


def _row(stats_row: dict, totals: dict) -> dict:
    """One civilization at one checkpoint: the engine's recorded stats plus the war and capture totals."""
    out = {"alive": bool(stats_row.get("alive"))}
    if out["alive"]:
        out.update({k: stats_row.get(k) for k in STAT_KEYS if stats_row.get(k) is not None})
    else:
        out["score"] = 0
    out.update({k: totals.get(k, 0) for k in Tally.KEYS})
    return out


def done_games(path: Path) -> set:
    """Game indices already in the output file (crashed games are played again)."""
    out = set()
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not r.get("crash"):
                out.add(r["i"])
    return out


def main():
    """The command line."""
    common.ensure_hash_seed()
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--smoke", action="store_true", help="2 short duel games (40 turns) into <name>-smoke")
    ap.add_argument("--games", type=int, default=100)
    ap.add_argument("--seed", type=int, default=5000, help="seed of game 0; game i uses seed + i")
    ap.add_argument("--sizes", default="small", help="comma-separated map sizes, rotated")
    ap.add_argument("--maps", default=",".join(common.MAP_TYPES), help="comma-separated map types, rotated")
    ap.add_argument("--barbarians", default="normal", help="comma-separated barbarian settings, rotated")
    ap.add_argument("--speed", default="Quick")
    ap.add_argument("--turn-limit", type=int, default=0, help="0 = the speed's own (330 on Quick)")
    ap.add_argument("--name", default=None, help="output file name (default python-<engine hash>)")
    ap.add_argument("--workers", type=int, default=common.default_workers())
    a = ap.parse_args()
    a.sizes, a.maps, a.barbarians = (s.split(",") for s in (a.sizes, a.maps, a.barbarians))
    a.checkpoints = CHECKPOINTS
    name = a.name or f"python-{common.engine_hash()}"
    if a.smoke:
        a.games, a.sizes, a.maps, a.turn_limit, a.checkpoints = 2, ["duel"], ["continents", "pangaea"], 40, \
            SMOKE_CHECKPOINTS
        name = f"{name}-smoke"
    out = common.REFCHECK / "baseline" / f"{name}.jsonl"
    out.parent.mkdir(parents=True, exist_ok=True)
    if a.smoke and out.exists():
        out.unlink()                  # a smoke run proves the pipeline from scratch every time
    have = done_games(out)
    todo = [s for s in game_specs(a) if s["i"] not in have]
    print(f"{len(todo)} game(s) to play into {out} ({len(have)} already there), {a.workers} worker(s).", flush=True)
    timer = common.Timer()
    crashed = 0
    with ProcessPoolExecutor(max_workers=max(1, min(a.workers, len(todo) or 1))) as ex, \
            open(out, "a", encoding="utf-8") as f:
        futs = [ex.submit(play_one, s) for s in todo]
        for n, fut in enumerate(as_completed(futs), 1):
            r = fut.result()
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
            f.flush()
            if r.get("crash"):
                crashed += 1
                print(f"  [{n}/{len(todo)}] game {r['i']} (seed {r['seed']}) CRASHED: {r['crash']}\n{r['trace']}",
                      flush=True)
            else:
                print(f"  [{n}/{len(todo)}] game {r['i']} seed {r['seed']} {r['size']}/{r['map_type']}: "
                      f"{r['turns']} turns, winner {r['winner']} ({r['victory']}), {r['seconds']}s", flush=True)
    print(f"Done in {timer()}s. Summarise with: python scripts/refcheck/summarize.py {out}")
    return 1 if crashed else 0


if __name__ == "__main__":
    sys.exit(main())
