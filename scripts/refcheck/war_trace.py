"""The Python bot's war pipeline in the statistical baseline's own games, traced round by round (package 2-07).

    python scripts/refcheck/war_trace.py --games 24 --out war-trace-python.jsonl            # whole games
    python scripts/refcheck/war_trace.py --games 60 --until 130 --out early.jsonl           # the first 130 turns
    cargo run --profile ci -p citar-testkit --example paired -- --trace 120 --out war-trace-rust.jsonl

Plays baseline game i (seed --first + i offset from 5000, small, the five map types in turn, barbarians normal,
Quick) exactly as baseline.py does: the same games, round for round (the tracing reads and never writes). Writes one
JSON line per game: per round, at its start (so the state as the round before ended, labelled with that round's
turn), each major civilization's war being prepared (`_war_prep`: whom and since when), whether it has a war plan,
whom it is at war with, its friends, its treaties, its opinion of each civilization met and its military power;
and, past `war_min_turn` while it is at peace and preparing nothing, what `consider_war` would see (basic.py:2445-
2451): its cities' threat and its power, and per living major met and at peace whether a treaty is in force, whether
they are friends, whether it is strong enough, and whether one of their cities is in reach. Every war declared,
peace made and city captured is listed from the events.

The Rust counterpart is crates/citar-testkit/examples/paired.rs --trace, which writes the same lines from the Rust
engine's games, so the two pipelines can be compared stage by stage. This needs the Python engine, so it goes with
it (package 2-12); its findings are in crates/citar-engine/DESIGN.md "As built in 2-07".
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import common

MAPS = common.MAP_TYPES


def _civ_row(g, b, pid: int, majors: list) -> dict:
    """One civilization at the start of a round."""
    from citar.engine import diplomacy as D
    p = g.player(pid)
    prep = b._war_prep.get(pid)
    plan = b._war_plan.get(pid)
    others = [q for q in majors if q != pid and g.player(q).alive]
    elig = None
    if p.alive and g.turn > b.p["war_min_turn"] and not prep:
        ctx = b.context(g, pid)
        if not ctx["wars"]:
            P, a = b.p, b.aggression
            mine = b.military_power(g, pid)
            elig = {"threat": round(sum(ctx["threat"].values()), 2), "mine": mine, "q": {}}
            for q in others:
                rel = g.relation(pid, q)
                if not g.has_met(pid, q) or rel is None or rel["war"]:
                    continue
                theirs = b.military_power(g, q)
                elig["q"][q] = [rel["treaty_until"] >= g.turn, D.is_friends(g, pid, q),
                                mine > theirs * (P["war_power_ratio"] - P["war_power_ratio_aggr"] * a),
                                b._reachable_city(g, pid, q, ctx) is not None]
    return {"alive": p.alive, "prep": [prep["player"], prep["since"]] if prep else None,
            "plan": plan["city"] if plan else None,
            "wars": [q for q in others if g.at_war(pid, q)],
            "friends": [q for q in others if D.is_friends(g, pid, q)],
            "treaty": [q for q in others if (g.relation(pid, q) or {}).get("treaty_until", 0) >= g.turn],
            "opinion": {q: round(D.opinion(g, pid, q), 1) for q in others if g.has_met(pid, q)},
            "power": b.military_power(g, pid) if p.alive else 0, "elig": elig}


def play(i: int, until: int) -> dict:
    """Baseline game i, traced to its end or to turn `until`."""
    seed = 5000 + i
    cfg = common.game_config(seed, "small", MAPS[i % len(MAPS)], "normal", speed="Quick", turn_limit=None)
    g = common.new_game(cfg)
    bots = common.make_bots(g, seed)
    majors = [p.id for p in g.majors(alive_only=False)]
    rounds, events = [], []

    def listen(ev):
        """The wars, peace and captures, as they happen."""
        d = ev.get("data") or {}
        kind = {"war_declared": "war", "peace": "peace", "city_captured": "capture"}.get(ev["type"])
        if kind == "war":
            events.append({"t": g.turn, "kind": kind, "a": d.get("attacker"), "b": d.get("defender")})
        elif kind == "peace":
            events.append({"t": g.turn, "kind": kind, "a": d.get("a"), "b": d.get("b")})
        elif kind == "capture":
            events.append({"t": g.turn, "kind": kind, "a": d.get("new_owner"), "b": d.get("old_owner")})

    g.listeners.append(listen)

    def on_round(g):
        """Each civilization as the round before ended."""
        rounds.append({"t": g.turn - 1, "civs": {pid: _civ_row(g, bots[pid], pid, majors) for pid in majors}})
        return g.turn <= until

    t0 = time.time()
    common.play(g, bots, on_round=on_round)
    return {"i": i, "seed": seed, "map": MAPS[i % len(MAPS)], "turns": g.turn - 1,
            "seconds": round(time.time() - t0, 1),
            "aggression": {pid: round(bots[pid].aggression, 3) for pid in majors}, "rounds": rounds,
            "events": events}


def main() -> int:
    """The command line."""
    common.ensure_hash_seed()
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--games", type=int, default=24)
    ap.add_argument("--first", type=int, default=0, help="the first game's index (seed 5000 + index)")
    ap.add_argument("--until", type=int, default=330, help="stop after this turn")
    ap.add_argument("--workers", type=int, default=common.default_workers())
    ap.add_argument("--out", type=Path, required=True, help="the JSON lines file; games already in it are skipped")
    a = ap.parse_args()
    done = set()
    if a.out.exists():
        done = {json.loads(line)["i"] for line in a.out.read_text(encoding="utf-8").splitlines() if line.strip()}
    todo = [i for i in range(a.first, a.first + a.games) if i not in done]
    with ProcessPoolExecutor(max_workers=max(1, a.workers)) as ex, open(a.out, "a", encoding="utf-8") as f:
        futures = {ex.submit(play, i, a.until): i for i in todo}
        for fut in as_completed(futures):
            r = fut.result()
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(f"game {r['i']}: {r['turns']} turns in {r['seconds']}s", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
