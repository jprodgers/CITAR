"""The lab game's result building as it was before package 4-07 moved it to Rust (`citar/lab.py`'s `play()` at
d6658df): the oracle `tests/test_jobs.py` holds the Rust LabResult to on fresh games, beside the seven games recorded
from it (`tests/data/lab_results_*.json`), and times `engine_api.run_job` against (4-07's gate 8).

It plays the game through `engine_api.run_game` and builds the result line in Python from the events it hears, as
the lab did for 0.1.5 and Phase 3. Kept verbatim (its imports, and a switch for its PROGRESS lines, aside), so do not
fix it: it is the old behaviour by definition.
"""
from __future__ import annotations

import time
from collections import Counter, defaultdict
from datetime import datetime

from citar import engine_api
from citar.lab import engine_hash, make_bot

CHECKPOINTS = (50, 100, 150, 200, 250, 300)
STAT_KEYS = ("score", "cities", "population", "techs", "era", "military", "gold", "gold_per_turn", "science",
             "production", "happiness", "culture")


def play(spec: dict, progress_lines: bool = False) -> dict:
    """Play one lab game to the end and return its result (``progress_lines`` prints the runner's PROGRESS lines, as
    the old play always did)."""
    t0 = time.time()
    cpu0 = time.process_time()
    seats = spec["seats"]
    config = {"map_type": spec["map_type"], "map_size": spec["size"], "seed": spec["seed"],
              "barbarians": spec["barbarians"], "barbarian_difficulty": spec.get("barbarian_difficulty"),
              "difficulty": spec["difficulty"], "speed": spec["speed"], "city_states": spec.get("city_states"),
              "ai_base_values": spec.get("ai_base_values") or "unciv",
              "turn_limit": spec["turns"] or None,
              **{k: spec[k] for k in ("map_edges", "river_density", "resources") if spec.get(k) is not None},
              "players": [{"controller": "bot", "nation": seat.get("nation") or spec.get("nation"),
                           "difficulty": seat.get("difficulty")} for seat in seats]}
    build = engine_hash()
    bots = {}
    for pid, seat in enumerate(seats):
        # aggression depends on the start position and seed, not the label, so rotation evens it out
        agg = 0.25 + 0.5 * ((pid * 37 + spec["seed"]) % 10) / 9
        bots[pid] = make_bot(seat, agg)
    # what each seat plays, as the build that plays it says: the experiment may have been queued on another build
    played = {pid: {"build": build, "version": bot.version, "fingerprint": engine_api.bot_fingerprint(bot),
                    "params": engine_api.bot_clean_params(bot.version, seats[pid].get("params") or {})}
              for pid, bot in bots.items()}
    events = defaultdict(Counter)
    built = defaultdict(Counter)
    era_turn = defaultdict(dict)
    errors = []

    def listen(ev):
        """Record the events a result needs as the game emits them."""
        t = ev["type"]
        d = ev.get("data") or {}
        if t in ("unit_built", "building_built", "wonder_built") and d.get("item"):
            built[d.get("player")][d["item"]] += 1
        elif t == "era":
            era_turn[d["player"]].setdefault(d["era"], ev["turn"])
        elif t == "city_captured":
            events[d.get("new_owner")]["captured_city"] += 1
            events[d.get("old_owner")]["lost_city"] += 1
        elif t == "war_declared":
            events[d.get("attacker")]["declared_war"] += 1
        elif t == "eliminated":
            events[d.get("player")]["eliminated_turn"] = ev["turn"]
        elif ev.get("players") and t in ("bankrupt", "city_starving", "camp_cleared", "city_founded", "unit_captured",
                                          "religion_founded", "golden_age", "great_person_born", "pillaged"):
            events[ev["players"][0]][t] += 1

    def progress(info):
        """The runner reads these lines from the game's process to show how far it has got."""
        if progress_lines and info["phase"] == "playing":
            print(f"PROGRESS {info['turn']} {info['turn_limit'] or 0} {time.time() - t0:.0f}", flush=True)

    r = engine_api.run_game({"config": config, "bots": bots}, on_turn=progress, on_event=listen)
    errors.extend(r["errors"])
    stats = {e["turn"]: e["players"] for e in r["stats"]}
    majors = [p for p in r["players"] if p["kind"] == "major"]   # eliminated civs too: a lost seat is a result
    final = {p["id"]: p["score"] for p in majors}
    total = sum(final.values()) or 1
    ranking = sorted(final, key=lambda q: -final[q])
    players = {}
    for p in majors:
        pid = p["id"]
        k = str(pid)
        cps = {}
        for cp in CHECKPOINTS:
            row = stats.get(cp, {}).get(k)
            if row and row.get("alive"):
                cps[cp] = {key: row.get(key) for key in STAT_KEYS if row.get(key) is not None}
        unhappy = sum(1 for row in stats.values() if row.get(k, {}).get("alive") and row[k].get("happiness", 0) < 0)
        seat = seats[pid]
        ship = p["spaceship"]
        players[k] = {
            "label": seat.get("label"), "bot": seat.get("bot"), "difficulty": p["difficulty"], "alive": p["alive"],
            "levels": seat.get("levels"), "profile": seat.get("profile"), "profile_rev": seat.get("profile_rev"),
            "profile_name": seat.get("profile_name"), **played[pid],
            # the handle's own: what played and what the fingerprint hashed, not the value the seat asked for
            "fixed_aggression": bots[pid].fixed_aggression, "aggression": round(bots[pid].aggression, 3),
            "score": final[pid], "score_share": round(final[pid] / total, 4), "rank": ranking.index(pid),
            "techs": p["techs"], "cities": p["cities"], "policies": p["policies"],
            "religion": p["religion"], "great_people": p["great_people"],
            "spaceship": sum((ship or {}).values()) if isinstance(ship, dict) else 0,
            "unhappy_share": round(unhappy / max(1, len(stats)), 3), "checkpoints": cps,
            "era_turn": era_turn.get(pid, {}), "events": dict(events.get(pid, {})),
            "built_top": dict(built.get(pid, Counter()).most_common(25)),
        }
    winner = r["winner"]
    return {"exp": spec["exp"], "i": spec["i"], "seed": spec["seed"], "map": spec["map_type"], "turns": r["turns"],
            "winner": winner, "winner_label": seats[winner]["label"] if winner is not None and
            winner < len(seats) else None, "victory": r["victory"], "players": players,
            "seconds": round(time.time() - t0, 1), "started_ts": round(t0, 1),
            "cpu_s": round(time.process_time() - cpu0, 1), "errors": errors, "engine": build,
            "finished": datetime.now().isoformat(timespec="seconds")}
