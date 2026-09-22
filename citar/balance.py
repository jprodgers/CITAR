"""Batch bot-vs-bot simulations for balancing the rules and measuring bot strength.

    python -m citar.balance --games 48 --players 4 --size small --label baseline
    python -m citar.balance --games 60 --players 2 --size duel --bots basic,snapshot_old --label ab-test

Games run in parallel (one process per game). `--bots` assigns bot types to seats in rotation, and the seat order is
rotated between games so no bot type always gets the same start. The report covers pacing (techs, eras, cities,
population by turn), economy (bankruptcy, unhappiness, starvation), conflict (wars, captures, eliminations,
barbarians), what gets built, victory types, and win rates per bot type. A JSON copy is written to saves/balance/.
"""
from __future__ import annotations

import argparse
import json
import statistics
import time
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor, as_completed
from . import paths

MAP_TYPES = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"]
CHECKPOINTS = tuple(range(25, 501, 25))
STAT_KEYS = ("score", "cities", "population", "land", "techs", "era", "military", "gold", "gold_per_turn", "science",
             "production", "happiness", "units")


class IdleBot:
    """Founds its capital and then does nothing: a stand-in for a player that neglects the game (tests conquest)."""

    def play_turn(self, g, pid, end_turn=True):
        """Do nothing and end the turn. A control for measuring what the real bot is worth."""
        from .engine import tools
        from .engine.game import ActionError
        for u in g.player_units(pid):
            if not g.player_cities(pid):
                try:
                    tools.execute(g, pid, "unit_action", {"unit_id": u.id, "action": "found_city"})
                except ActionError:
                    pass

    def respond(self, g, pid, nid):
        """Refuse every negotiation."""
        from .engine import tools
        from .engine.game import ActionError
        try:
            tools.execute(g, pid, "respond_negotiation", {"negotiation_id": nid, "action": "reject"})
        except ActionError:
            pass


def make_bot(kind: str, seed: int, aggression: float):
    """Instantiate a bot by name: a bot profile id, the idle bot, or a module in citar/bots (live or frozen)."""
    if kind == "idle":
        return IdleBot()
    from .bots import profiles
    try:
        return profiles.make_bot(kind, seed=seed, aggression=aggression)
    except profiles.ProfileError:
        pass
    if kind.startswith(("snapshot", "frozen_")):
        # a copy of basic.py saved as citar/bots/<kind>.py, for A/B testing bot changes
        import importlib
        return importlib.import_module(f"citar.bots.{kind}").BasicBot(aggression=aggression, seed=seed)
    from .bots.basic import BasicBot
    return BasicBot(aggression=aggression, seed=seed)


def play_game(spec: dict) -> dict:
    """Run one game to the end (in a worker process) and return its statistics."""
    from .engine.game import Game
    from .sim import resolve_negotiations
    t0 = time.time()
    n = spec["players"]
    g = Game.new({"map_type": spec["map_type"], "map_size": spec["size"], "seed": spec["seed"], "barbarians": spec["barbarians"],
                  "speed": spec["speed"], "players": [{"controller": "bot", "nation": spec.get("nation")} for _ in range(n)],
                  "turn_limit": spec["turns"] or None})
    kinds = {p.id: spec["seat_bots"][p.id % len(spec["seat_bots"])] for p in g.majors()}
    bots = {pid: make_bot(k, spec["seed"] * 101 + pid, 0.25 + 0.5 * ((pid * 37 + spec["seed"]) % 10) / 9) for pid, k in kinds.items()}
    events = defaultdict(Counter)       # pid -> counter
    built = defaultdict(Counter)        # pid -> item counter
    era_turn = defaultdict(dict)        # pid -> {era: turn}
    kills = Counter()                   # (killer_kind, victim_kind)
    game_events = Counter()
    errors = []

    def listen(ev):
        """Record the events the report needs as the game emits them."""
        t = ev["type"]
        d = ev.get("data") or {}
        game_events[t] += 1
        if t in ("unit_built", "building_built"):
            built[d.get("player")][d.get("item")] += 1
        elif t == "wonder_built" and d.get("item"):
            built[d.get("player")][d["item"]] += 1
        elif t == "era":
            era_turn[d["player"]].setdefault(d["era"], ev["turn"])
        elif t == "unit_killed":
            killer, owner = d.get("killer"), d.get("owner")
            kk = "barbarian" if killer is not None and g.player(killer).kind == "barbarian" else "player"
            vk = "barbarian" if owner is not None and g.player(owner).kind == "barbarian" else "player"
            kills[f"{kk}>{vk}"] += 1
        elif t in ("city_captured",):
            events[d.get("new_owner")]["captured_city"] += 1
            events[d.get("old_owner")]["lost_city"] += 1
        elif t == "war_declared":
            events[d.get("attacker")]["declared_war"] += 1
        elif t == "eliminated":
            events[d.get("player")]["eliminated_turn"] = ev["turn"]
        elif ev.get("players") and len(ev["players"]) >= 1 and t in (
                "bankrupt", "city_starving", "camp_cleared", "ruins", "unit_captured", "pillaged", "city_founded",
                "production_blocked", "city_idle", "research_needed"):
            events[ev["players"][0]][t] += 1

    g.listeners.append(listen)
    while g.s.phase == "playing":
        pid = g.s.current
        try:
            bots[pid].play_turn(g, pid, end_turn=False)
        except Exception as e:  # a bot crash is a finding, not a reason to lose the batch
            import traceback
            errors.append(f"T{g.turn} P{pid} {kinds.get(pid)}: {type(e).__name__}: {e}\n{traceback.format_exc(limit=4)}")
            if len(errors) > 20:
                break
        resolve_negotiations(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)

    stats = {e["turn"]: e["players"] for e in g.s.stats}
    players = {}
    unhappy_turns = defaultdict(int)
    very_unhappy_turns = defaultdict(int)
    negative_gpt_turns = defaultdict(int)
    for turn, row in stats.items():
        for k, v in row.items():
            if v.get("alive"):
                if v.get("happiness", 0) < 0:
                    unhappy_turns[k] += 1
                if v.get("happiness", 0) <= -10:
                    very_unhappy_turns[k] += 1
                if v.get("gold_per_turn", 0) < 0:
                    negative_gpt_turns[k] += 1
    for p in [p for p in g.s.players if p.kind == "major"]:
        k = str(p.id)
        checkpoints = {}
        for cp in CHECKPOINTS:
            row = stats.get(cp, {}).get(k)
            if row and row.get("alive"):
                checkpoints[cp] = {key: row.get(key) for key in STAT_KEYS}
        players[p.id] = {
            "bot": kinds[p.id], "alive": p.alive, "techs": len(p.techs), "future_techs": p.future_techs,
            "checkpoints": checkpoints, "era_turn": era_turn.get(p.id, {}), "events": dict(events.get(p.id, {})),
            "built": dict(built.get(p.id, {})), "unhappy_turns": unhappy_turns[k], "very_unhappy_turns": very_unhappy_turns[k],
            "negative_gpt_turns": negative_gpt_turns[k], "spaceship": dict(g.s.spaceship.get(p.id, {})),
            "policies": len(p.policies), "religion": p.religion_state, "great_people": p.great_people_earned,
        }
    from .engine.victory import score
    final = {p.id: (score(g, p.id)["total"] if p.alive else 0) for p in g.s.players if p.kind == "major"}
    ranking = sorted(final, key=lambda pid: -final[pid])
    return {"spec": {k: v for k, v in spec.items() if k != "seat_bots"}, "seat_bots": kinds, "turns": g.turn - 1,
            "winner": g.s.winner, "winner_bot": kinds.get(g.s.winner), "victory": g.s.victory, "final_scores": final,
            "ranking": ranking, "players": players, "kills": dict(kills), "events": dict(game_events),
            "seconds": round(time.time() - t0, 1), "errors": errors}


# ----------------------------------------------------------------------------
# report
# ----------------------------------------------------------------------------
def _med(xs):
    """The median of the values that are not None."""
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 1) if xs else None


def _mean(xs):
    """The mean of the values that are not None."""
    xs = [x for x in xs if x is not None]
    return round(statistics.mean(xs), 2) if xs else None


def report(results: list[dict]) -> dict:
    """Summarise a batch of games: pacing, economy, conflict, construction and win rates."""
    out = {"games": len(results)}
    all_players = [p for r in results for p in r["players"].values()]
    out["avg_game_seconds"] = _mean([r["seconds"] for r in results])
    out["victories"] = dict(Counter(r["victory"] or "none" for r in results))
    out["median_end_turn"] = _med([r["turns"] for r in results])
    out["bot_crashes"] = sum(len(r["errors"]) for r in results)

    pacing = {}
    for cp in CHECKPOINTS:
        rows = [p["checkpoints"][cp] for p in all_players if cp in p["checkpoints"]]
        if rows:
            pacing[cp] = {k: _med([r.get(k) for r in rows]) for k in STAT_KEYS}
            pacing[cp]["max_techs"] = max(r.get("techs") or 0 for r in rows)
    out["pacing_median"] = pacing
    eras = defaultdict(list)
    for p in all_players:
        for era, turn in p["era_turn"].items():
            eras[int(era)].append(turn)
    out["era_entry_turn"] = {era: {"median": _med(v), "first": min(v), "share_reached": round(len(v) / len(all_players), 2)}
                             for era, v in sorted(eras.items())}
    n = len(all_players)
    ev = Counter()
    for p in all_players:
        for k, v in p["events"].items():
            if k != "eliminated_turn":
                ev[k] += v
    out["per_player_events"] = {k: round(v / n, 2) for k, v in sorted(ev.items())}
    out["eliminated_share"] = round(sum(1 for p in all_players if not p["alive"]) / n, 3)
    out["unhappy_turn_share"] = _mean([p["unhappy_turns"] / max(1, r["turns"]) for r in results for p in r["players"].values()])
    out["very_unhappy_turn_share"] = _mean([p["very_unhappy_turns"] / max(1, r["turns"]) for r in results for p in r["players"].values()])
    out["negative_gpt_turn_share"] = _mean([p["negative_gpt_turns"] / max(1, r["turns"]) for r in results for p in r["players"].values()])
    stuck = [p for p in all_players if (p["checkpoints"].get(100) or {}).get("cities", 0) <= 1]
    out["one_city_at_t100_share"] = round(len(stuck) / n, 3)
    kills = Counter()
    for r in results:
        kills.update(r["kills"])
    out["kills_per_game"] = {k: round(v / len(results), 1) for k, v in kills.items()}
    built = Counter()
    for p in all_players:
        built.update(p["built"])
    out["built_per_player"] = {k: round(v / n, 2) for k, v in built.most_common(45)}
    spaceship = Counter()
    for p in all_players:
        spaceship.update(p["spaceship"])
    out["projects_per_player"] = {k: round(v / n, 2) for k, v in spaceship.items()}

    by_bot = defaultdict(lambda: {"seats": 0, "wins": 0, "rank_sum": 0.0, "score_sum": 0, "techs": [], "cities100": [], "eliminated": 0})
    for r in results:
        m = len(r["ranking"])
        for pid, p in r["players"].items():
            b = by_bot[p["bot"]]
            b["seats"] += 1
            rank = r["ranking"].index(pid) if pid in r["ranking"] else m - 1
            b["rank_sum"] += rank / max(1, m - 1)          # 0 = first, 1 = last
            b["score_sum"] += r["final_scores"].get(pid, 0)
            b["techs"].append(p["techs"])
            b["cities100"].append((p["checkpoints"].get(100) or {}).get("cities", 0))
            b["eliminated"] += 0 if p["alive"] else 1
        wb = r.get("winner_bot") or (r["players"][r["ranking"][0]]["bot"] if r["ranking"] else None)
        by_bot[wb]["wins"] += 1
    per_bot_detail = defaultdict(lambda: defaultdict(list))
    for r in results:
        for pid, p in r["players"].items():
            d = per_bot_detail[p["bot"]]
            for cp in (100, 200):
                row = p["checkpoints"].get(cp) or {}
                for k in ("cities", "population", "techs", "military", "gold_per_turn", "happiness"):
                    d[f"{k}_t{cp}"].append(row.get(k))
            d["unhappy_share"].append(p["unhappy_turns"] / max(1, r["turns"]))
            d["negative_gpt_share"].append(p["negative_gpt_turns"] / max(1, r["turns"]))
            for k in ("bankrupt", "declared_war", "captured_city", "lost_city", "camp_cleared", "city_founded"):
                d[k].append(p["events"].get(k, 0))
            d["era5_turn"].append(p["era_turn"].get(5))
            d["era4_turn"].append(p["era_turn"].get(4))
    out["bot_detail"] = {b: {k: (_mean(v) if not k.startswith("era") else _med(v)) for k, v in d.items()} for b, d in per_bot_detail.items()}
    out["bots"] = {k: {"seats": v["seats"], "win_share": round(v["wins"] / len(results), 3),
                       "avg_rank_pct": round(v["rank_sum"] / v["seats"], 3), "avg_final_score": round(v["score_sum"] / v["seats"]),
                       "median_techs": _med(v["techs"]), "median_cities_t100": _med(v["cities100"]),
                       "eliminated_share": round(v["eliminated"] / v["seats"], 3)} for k, v in by_bot.items() if v["seats"]}
    return out


def print_report(rep: dict):
    """Print a balance report."""
    print(f"\n=== {rep['games']} games, avg {rep['avg_game_seconds']}s each; bot crashes: {rep['bot_crashes']} ===")
    print("Victories:", rep["victories"], "| median end turn:", rep["median_end_turn"])
    print("\nPacing (median per player):")
    keys = ("score", "cities", "population", "techs", "era", "military", "gold", "gold_per_turn", "science", "production", "happiness", "units")
    print("turn  " + " ".join(f"{k[:8]:>8}" for k in keys) + "  maxtech")
    for cp, row in rep["pacing_median"].items():
        print(f"{cp:>4}  " + " ".join(f"{str(row.get(k)):>8}" for k in keys) + f"  {row['max_techs']:>7}")
    print("\nEra entry turns:", dict(rep["era_entry_turn"].items()))
    print("Eliminated share:", rep["eliminated_share"], "| one city at T100:", rep["one_city_at_t100_share"])
    print("Unhappy turn share:", rep["unhappy_turn_share"], "| very unhappy:", rep["very_unhappy_turn_share"],
          "| negative gold/turn:", rep["negative_gpt_turn_share"])
    print("Per-player events:", rep["per_player_events"])
    print("Kills per game:", rep["kills_per_game"])
    print("Built per player:", rep["built_per_player"])
    print("Projects per player:", rep["projects_per_player"])
    print("\nBots:")
    for k, v in rep["bots"].items():
        print(f"  {k:>8}: {v}")
        print(f"  {'':>8}  {rep['bot_detail'].get(k)}")


def main():
    """The ``citar balance`` command line."""
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--games", type=int, default=24)
    ap.add_argument("--players", type=int, default=4)
    ap.add_argument("--size", default="small")
    ap.add_argument("--maps", default=",".join(MAP_TYPES), help="comma-separated map types to cycle through")
    ap.add_argument("--turns", type=int, default=0, help="0 = the speed's time-victory turn")
    ap.add_argument("--speed", default="Quick")
    ap.add_argument("--nation", default=None, help="e.g. BenchmarkCiv for every seat (default: random civilizations)")
    ap.add_argument("--barbarians", default="normal")
    ap.add_argument("--bots", default="basic", help="comma-separated bot types assigned to seats in rotation "
                                                    "(basic, idle, snapshot_<name>)")
    ap.add_argument("--seed", type=int, default=1000, help="first seed")
    ap.add_argument("--workers", type=int, default=0, help="parallel games (default: CPU count - 1)")
    ap.add_argument("--label", default="run")
    a = ap.parse_args()
    import os
    workers = a.workers or max(1, (os.cpu_count() or 2) - 1)
    maps = a.maps.split(",")
    bot_types = a.bots.split(",")
    specs = []
    for i in range(a.games):
        rot = i % len(bot_types)
        seat_bots = bot_types[rot:] + bot_types[:rot]
        specs.append({"seed": a.seed + i, "map_type": maps[i % len(maps)], "size": a.size, "players": a.players, "turns": a.turns,
                      "speed": a.speed, "barbarians": a.barbarians, "seat_bots": seat_bots, "nation": a.nation})
    t0 = time.time()
    results = []
    with ProcessPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(play_game, s) for s in specs]
        for i, f in enumerate(as_completed(futures), 1):
            r = f.result()
            results.append(r)
            print(f"[{i}/{len(specs)}] seed {r['spec']['seed']} {r['spec']['map_type']}: T{r['turns']} {r['victory']} "
                  f"winner {r['winner']} ({r['winner_bot']}) in {r['seconds']}s" + (f" ERRORS {len(r['errors'])}" if r["errors"] else ""), flush=True)
    rep = report(results)
    rep["args"] = vars(a)
    rep["wall_seconds"] = round(time.time() - t0, 1)
    print_report(rep)
    for r in results:
        for e in r["errors"][:2]:
            print("BOT ERROR:", e)
    out = paths.sub("balance")
    path = out / f"{a.label}-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps({"report": rep, "games": results}, default=str), encoding="utf-8")
    print(f"\nWrote {path} ({rep['wall_seconds']}s)")


if __name__ == "__main__":
    main()
