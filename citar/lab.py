"""CITAR lab: a long-running, resumable queue of bot-vs-bot experiments for tuning the scripted bots.

    python -m citar.lab run [--workers N] [--night-workers M]   # the runner: keep it going for days; safe to restart
    python -m citar.lab submit SPEC.json [...]                   # queue experiments (bot code is frozen at submit time)
    python -m citar.lab status                                   # what is running, progress per experiment
    python -m citar.lab report NAME [NAME ...]                   # results, per seat label and head to head
    python -m citar.lab stop                                     # ask the runner to exit (running games are redone later)

Everything lives in saves/lab/: queue/NAME.json (experiments), results/NAME.jsonl (one line per finished game),
frozen bot sources are citar/bots/frozen_<hash>.py, status.json and lab.log describe the runner.

An experiment is a JSON object:

    {"name": "prince-baseline", "priority": 5, "games": 24, "seed": 1000,
     "size": "small", "maps": ["continents", "pangaea"], "speed": "Quick", "turns": 0,
     "difficulty": "Prince", "barbarians": "normal", "barbarian_difficulty": null, "nation": "BenchmarkCiv",
     "seats": [{"label": "new", "bot": "basic", "difficulty": "Prince", "params": {}},
               {"label": "old", "bot": "frozen_ab12cd34"}, {"profile": "my-profile"}, ...],
     "rotate": true}

A seat can name a bot profile ("profile": id; see citar.bots.profiles) instead of a bot and parameters: the profile's
engine, overrides and aggression are resolved and frozen into the experiment when it is submitted, so later edits to
the profile don't change a queued experiment. "params" on such a seat are layered on top of the profile's. Factorial
experiments may name a "profile" as their base bot in the same way.

Optional map generation keys pass straight to the generator: "map_edges" (ice_caps, wrap_x, wrap_y, wrap_both,
boxed), "river_density" (1 = normal) and "resources" (densities and per-resource rules; see mapgen.MapOptions).

Game i uses seed+i and maps[i % len(maps)]; with "rotate" the seat list is rotated by i so every label plays every
start position. "turns" 0 plays to the speed's time-victory turn. Seats with "bot": "basic" are frozen when the
experiment is submitted, so later edits to basic.py don't mix into a running experiment (use "live" to opt out).
During the restricted hours of the CITAR host server (Servers page) the runner uses --night-workers (fan noise).
Every finished game is written to the usage ledger (saves/usage/lab-*.jsonl) so reports can cost experiments.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import statistics
import sys
import time
import traceback
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path

from .fsutil import replace as _fs_replace
from . import paths

LAB = paths.saves_path("lab")
QUEUE, RESULTS, DONE = LAB / "queue", LAB / "results", LAB / "done"
BOTS = paths.PACKAGE / "bots"
CHECKPOINTS = (50, 100, 150, 200, 250, 300)
STAT_KEYS = ("score", "cities", "population", "techs", "era", "military", "gold", "gold_per_turn", "science",
             "production", "happiness", "culture")


def _dirs():
    """Create the lab's directories if they do not exist."""
    for d in (QUEUE, RESULTS, DONE):
        d.mkdir(parents=True, exist_ok=True)


def log(msg: str):
    """Append to the lab log, and print it. The Lab page reads this file."""
    _dirs()
    line = f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}"
    print(line, flush=True)
    with open(LAB / "lab.log", "a", encoding="utf-8") as f:
        f.write(line + "\n")


def engine_hash() -> str:
    """A hash of the engine and ruleset, recorded with every result.

    So that a result from three weeks ago can be identified as having been produced by different code,
    rather than silently compared against today's.
    """
    h = hashlib.sha1()
    for d in (paths.PACKAGE / "engine", paths.package_data()):
        for p in sorted(d.rglob("*")):
            if p.suffix in (".py", ".json") and "__pycache__" not in p.parts:
                h.update(p.name.encode())
                h.update(p.read_bytes())
    return h.hexdigest()[:10]


# ----------------------------------------------------------------------------
# experiments
# ----------------------------------------------------------------------------
DEFAULTS = {"priority": 0, "games": 12, "seed": 1000, "size": "small", "maps": ["continents", "pangaea", "fractal"],
            "speed": "Quick", "turns": 0, "difficulty": "Prince", "barbarians": "normal", "barbarian_difficulty": None,
            "nation": "BenchmarkCiv", "city_states": None, "rotate": True}


def freeze_bot(src: str = "basic") -> str:
    """Copy the live bot to a frozen_<hash> module (see citar.bots.profiles.freeze) and return the module name."""
    from .bots import profiles
    return profiles.freeze(src)


def _resolve_seat(seat: dict) -> dict:
    """A seat as it will be played: a profile resolved into engine, parameters and aggression (frozen now), or a raw
    bot seat with its live code frozen. Records the profile revision and the fingerprint of what plays."""
    from .bots import profiles
    seat = dict(seat)
    if seat.get("profile"):
        r = profiles.resolve(seat, freeze_code=True)
        seat.update({"bot": r["engine"], "params": r["params"], "aggression": r["aggression"],
                     "profile_rev": r["profile_rev"], "profile_name": r["profile_name"],
                     "fingerprint": r["fingerprint"]})
        seat.setdefault("label", r["profile_name"])
        if not seat.get("label"):
            seat["label"] = r["profile_name"]
        return seat
    seat.setdefault("bot", "basic")
    if seat["bot"] == "basic":
        seat["bot"] = freeze_bot("basic")
    elif seat["bot"] == "live":
        seat["bot"] = "basic"
    if seat["bot"] != "idle":
        try:
            seat["fingerprint"] = profiles.fingerprint(seat["bot"], seat.get("params"), seat.get("aggression"))
        except profiles.ProfileError:
            pass
    return seat


def normalize(spec: dict) -> dict:
    """Validate an experiment and fill in its defaults, freezing the bot code it names."""
    s = dict(DEFAULTS)
    s.update(spec)
    if not s.get("name"):
        raise ValueError("An experiment needs a name.")
    if s.get("factors"):
        if s.get("profile"):
            from .bots import profiles
            r = profiles.resolve(s["profile"], freeze_code=True)
            s["bot"] = r["engine"]
            s["base_params"] = {**r["params"], **(s.get("base_params") or {})}
            if r["aggression"] is not None and s.get("aggression") is None:
                s["aggression"] = r["aggression"]
            s["profile_rev"] = r["profile_rev"]
        elif s.get("bot", "basic") == "basic":
            s["bot"] = freeze_bot("basic")
        s["seats"] = [{"label": "factorial"}]
    if not s.get("seats"):
        raise ValueError("An experiment needs seats.")
    seats = []
    for i, seat in enumerate(s["seats"]):
        if s.get("factors"):
            seats.append(dict(seat))
            continue
        seat = _resolve_seat(seat)
        seat.setdefault("label", seat["bot"])
        seat.setdefault("difficulty", None)
        seat.setdefault("params", {})
        seats.append(seat)
    s["seats"] = seats
    s["submitted"] = s.get("submitted") or datetime.now().isoformat(timespec="seconds")
    return s


def submit(spec: dict) -> dict:
    """Queue an experiment, refusing to overwrite one of the same name."""
    _dirs()
    s = normalize(spec)
    path = QUEUE / f"{s['name']}.json"
    if path.exists():
        raise ValueError(f"Experiment {s['name']} already exists (delete saves/lab/queue/{s['name']}.json first).")
    path.write_text(json.dumps(s, indent=1), encoding="utf-8")
    return s


def load_queue() -> list[dict]:
    """Every queued experiment, most important first."""
    _dirs()
    out = []
    for p in QUEUE.glob("*.json"):
        try:
            out.append(json.loads(p.read_text(encoding="utf-8")))
        except (OSError, json.JSONDecodeError):
            continue
    out.sort(key=lambda s: (-s.get("priority", 0), s.get("submitted", "")))
    return out


def complete_players(exp: dict, r: dict) -> dict:
    """A result with every seat present. Results written before 2026-09-22 left out civilizations that had been
    eliminated (the writer only looked at living ones); who sat in the missing seats is known from the experiment's
    seat list and rotation, so they are put back as eliminated with a score of 0, sharing last place."""
    if not exp or r.get("crash") or not r.get("players"):
        return r
    seats = game_spec(exp, r["i"])["seats"]
    if len(r["players"]) >= len(seats):
        return r
    r = dict(r)
    players = dict(r["players"])
    last = len(seats) - 1
    for pid, seat in enumerate(seats):
        if str(pid) in players:
            continue
        players[str(pid)] = {"label": seat.get("label"), "bot": seat.get("bot"), "alive": False,
                             "difficulty": seat.get("difficulty") or exp.get("difficulty") or "Prince",
                             "levels": seat.get("levels"), "profile": seat.get("profile"),
                             "profile_rev": seat.get("profile_rev"), "fingerprint": seat.get("fingerprint"),
                             "score": 0, "score_share": 0.0, "rank": last, "techs": None, "cities": 0,
                             "policies": None, "religion": None, "great_people": None, "spaceship": 0,
                             "unhappy_share": None, "checkpoints": {}, "era_turn": {}, "events": {}, "built_top": {},
                             "reconstructed": True}
    r["players"] = players
    return r


def load_results(name: str) -> list[dict]:
    """Every finished game of an experiment."""
    path = RESULTS / f"{name}.jsonl"
    if not path.exists():
        return []
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def factorial_seats(exp: dict, i: int) -> list[dict]:
    """Factorial experiments: every seat plays the base bot with its own mix of factor levels. For each factor the
    levels are spread evenly over the seats of each game (shuffled per game), so a factor's effect can be measured
    within games."""
    import random
    rng = random.Random(f"{exp['name']}:{i}")
    n = int(exp.get("players", 4))
    factors = exp["factors"]
    seat_params = [dict(exp.get("base_params") or {}) for _ in range(n)]
    seat_levels = [{} for _ in range(n)]
    for f, levels in factors.items():
        order = [levels[k % len(levels)] for k in range(n)]
        rng.shuffle(order)
        for j in range(n):
            seat_params[j][f] = order[j]
            seat_levels[j][f] = order[j]
    bot = exp.get("bot") or "basic"
    return [{"label": ",".join(f"{f}={v}" for f, v in seat_levels[j].items()), "bot": bot,
             "difficulty": exp.get("seat_difficulty"), "params": seat_params[j], "levels": seat_levels[j],
             "aggression": exp.get("aggression"), "profile": exp.get("profile")}
            for j in range(n)]


def game_spec(exp: dict, i: int) -> dict:
    """Build game *i* of an experiment: its seed, map and seat order.

    Seat rotation is what removes position advantage from the comparison - each label plays every start
    position across the run, so a result is about the bot rather than about where it began.
    """
    seats = factorial_seats(exp, i) if exp.get("factors") else list(exp["seats"])
    if exp.get("rotate", True) and seats and not exp.get("factors"):
        k = i % len(seats)
        seats = seats[k:] + seats[:k]
    maps = exp.get("maps") or ["continents"]
    return {"exp": exp["name"], "i": i, "seed": exp["seed"] + i, "map_type": maps[i % len(maps)], "size": exp["size"],
            "speed": exp["speed"], "turns": exp.get("turns") or 0, "difficulty": exp.get("difficulty") or "Prince",
            "barbarians": exp.get("barbarians", "normal"), "barbarian_difficulty": exp.get("barbarian_difficulty"),
            "nation": exp.get("nation"), "city_states": exp.get("city_states"), "seats": seats,
            "ai_base_values": exp.get("ai_base_values"),
            # map generation (see mapgen.MapOptions); absent means the generator's defaults
            **{k: exp[k] for k in ("map_edges", "river_density", "resources") if exp.get(k) is not None}}


# ----------------------------------------------------------------------------
# one game (runs in a worker process)
# ----------------------------------------------------------------------------
def make_bot(seat: dict, seed: int, aggression: float):
    """Instantiate the bot a seat calls for, live or frozen."""
    kind = seat.get("bot", "basic")
    if kind == "idle":
        from .balance import IdleBot
        return IdleBot()
    import importlib
    mod = importlib.import_module(f"citar.bots.{kind}")
    agg = seat["aggression"] if seat.get("aggression") is not None else aggression
    try:
        return mod.BasicBot(aggression=agg, seed=seed, params=seat.get("params") or {})
    except TypeError:                     # bots frozen before params existed
        return mod.BasicBot(aggression=agg, seed=seed)


def _seat_fingerprint(seat: dict):
    """The fingerprint of what a seat plays (recorded with its result, so ratings need not re-derive it)."""
    if seat.get("fingerprint"):
        return seat["fingerprint"]
    if seat.get("bot") in (None, "idle"):
        return "idle" if seat.get("bot") == "idle" else None
    try:
        from .bots import profiles
        return profiles.fingerprint(seat["bot"], seat.get("params"), seat.get("aggression"))
    except Exception:
        return None


def play(spec: dict) -> dict:
    """Play one lab game to the end and return its result."""
    from .engine.game import Game
    from .engine.victory import score
    from .sim import resolve_negotiations
    t0 = time.time()
    cpu0 = time.process_time()
    seats = spec["seats"]
    g = Game.new({"map_type": spec["map_type"], "map_size": spec["size"], "seed": spec["seed"],
                  "barbarians": spec["barbarians"], "barbarian_difficulty": spec.get("barbarian_difficulty"),
                  "difficulty": spec["difficulty"], "speed": spec["speed"], "city_states": spec.get("city_states"),
                  "ai_base_values": spec.get("ai_base_values") or "unciv",
                  "turn_limit": spec["turns"] or None,
                  **{k: spec[k] for k in ("map_edges", "river_density", "resources") if spec.get(k) is not None},
                  "players": [{"controller": "bot", "nation": seat.get("nation") or spec.get("nation"),
                               "difficulty": seat.get("difficulty")} for seat in seats]})
    bots = {}
    for pid, seat in enumerate(seats):
        # aggression depends on the start position and seed, not the label, so rotation evens it out
        agg = 0.25 + 0.5 * ((pid * 37 + spec["seed"]) % 10) / 9
        bots[pid] = make_bot(seat, spec["seed"] * 101 + pid, agg)
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

    g.listeners.append(listen)
    limit = g.s.config.get("turn_limit")
    shown = -1
    while g.s.phase == "playing":
        if g.turn != shown:
            shown = g.turn
            print(f"PROGRESS {g.turn} {limit or 0} {time.time() - t0:.0f}", flush=True)
        pid = g.s.current
        if pid in bots:
            try:
                bots[pid].play_turn(g, pid, end_turn=False)
            except Exception as e:
                errors.append(f"T{g.turn} P{pid}: {type(e).__name__}: {e}\n{traceback.format_exc(limit=5)}")
                if len(errors) > 20:
                    break
            resolve_negotiations(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)

    stats = {e["turn"]: e["players"] for e in g.s.stats}
    majors = g.majors(alive_only=False)      # eliminated civs too: a lost seat is a result, not a missing row
    final = {p.id: (score(g, p.id)["total"] if p.alive else 0) for p in majors}
    total = sum(final.values()) or 1
    ranking = sorted(final, key=lambda q: -final[q])
    players = {}
    for p in majors:
        k = str(p.id)
        cps = {}
        for cp in CHECKPOINTS:
            row = stats.get(cp, {}).get(k)
            if row and row.get("alive"):
                cps[cp] = {key: row.get(key) for key in STAT_KEYS if row.get(key) is not None}
        unhappy = sum(1 for row in stats.values() if row.get(k, {}).get("alive") and row[k].get("happiness", 0) < 0)
        seat = seats[p.id]
        players[k] = {
            "label": seat.get("label"), "bot": seat.get("bot"), "difficulty": p.difficulty, "alive": p.alive,
            "levels": seat.get("levels"), "profile": seat.get("profile"), "profile_rev": seat.get("profile_rev"),
            "fingerprint": _seat_fingerprint(seat), "aggression": round(bots[p.id].aggression, 3)
            if hasattr(bots[p.id], "aggression") else None,
            "score": final[p.id], "score_share": round(final[p.id] / total, 4), "rank": ranking.index(p.id),
            "techs": len(p.techs), "cities": len(g.player_cities(p.id)), "policies": len(p.policies),
            "religion": p.religion_state, "great_people": p.great_people_earned,
            "spaceship": sum((g.s.spaceship.get(p.id) or {}).values()) if isinstance(g.s.spaceship.get(p.id), dict) else 0,
            "unhappy_share": round(unhappy / max(1, len(stats)), 3), "checkpoints": cps,
            "era_turn": era_turn.get(p.id, {}), "events": dict(events.get(p.id, {})),
            "built_top": dict(built.get(p.id, Counter()).most_common(25)),
        }
    return {"exp": spec["exp"], "i": spec["i"], "seed": spec["seed"], "map": spec["map_type"], "turns": g.turn - 1,
            "winner": g.s.winner, "winner_label": seats[g.s.winner]["label"] if g.s.winner is not None and
            g.s.winner < len(seats) else None, "victory": g.s.victory, "players": players,
            "seconds": round(time.time() - t0, 1), "started_ts": round(t0, 1),
            "cpu_s": round(time.process_time() - cpu0, 1), "errors": errors, "engine": engine_hash(),
            "finished": datetime.now().isoformat(timespec="seconds")}


def _play_safe(spec: dict) -> dict:
    """Play a game, returning a crash record instead of raising.

    One bad game must not end a run that has been going for two days.
    """
    try:
        return play(spec)
    except Exception as e:
        return {"exp": spec["exp"], "i": spec["i"], "crash": f"{type(e).__name__}: {e}\n{traceback.format_exc(limit=8)}",
                "finished": datetime.now().isoformat(timespec="seconds")}


# ----------------------------------------------------------------------------
# the runner
# ----------------------------------------------------------------------------
def quiet_now() -> bool:
    """True during the restricted hours of the CITAR host server (the machine the lab games run on)."""
    try:
        from . import servers
        return servers.restricted(servers.host()) is not None
    except Exception:
        return False


def lab_config(args) -> dict:
    """The runner's configuration, from arguments and the config file."""
    cfg = {"workers": args.workers, "night_workers": args.night_workers}
    try:
        cfg.update(json.loads((LAB / "config.json").read_text(encoding="utf-8")))
    except (OSError, json.JSONDecodeError):
        pass
    return cfg


JOBS = LAB / "jobs"          # per-game spec/result/log files of games in flight


def _job_paths(name: str, i: int):
    """The spec, result and log paths for one game of an experiment."""
    base = JOBS / f"{name}__{i}"
    return base.with_suffix(".spec.json"), base.with_suffix(".result.json"), base.with_suffix(".log")


def _record(name: str, i: int, res: dict, finished_times: list):
    """Append a finished game's result, or its crash, to the experiment's results."""
    if res.get("crash"):
        log(f"{name} #{i} crashed: {res['crash'].splitlines()[0]}")
        with open(LAB / "crashes.log", "a", encoding="utf-8") as f:
            f.write(f"--- {datetime.now():%Y-%m-%d %H:%M} {name} #{i}\n{res['crash']}\n")
        return
    if i in {r["i"] for r in load_results(name)}:
        return
    with open(RESULTS / f"{name}.jsonl", "a", encoding="utf-8") as f:
        f.write(json.dumps(res) + "\n")
    finished_times.append(time.time())
    try:
        from . import servers, usage
        usage.append("lab", usage.lab_game_rows(name, i, res, servers.load().get("host_server_id")))
    except Exception:
        log("could not write the usage ledger: " + traceback.format_exc(limit=2).replace("\n", " | "))
    if res.get("errors"):
        with open(LAB / "crashes.log", "a", encoding="utf-8") as f:
            f.write(f"--- {datetime.now():%Y-%m-%d %H:%M} {name} #{i} bot errors\n" + "\n".join(res["errors"][:3]) + "\n")
    log(f"{name} #{i}: T{res['turns']} {res['victory']} won by {res['winner_label']} "
        f"({res['seconds']:.0f}s{', ' + str(len(res['errors'])) + ' bot errors' if res['errors'] else ''})")


def _retry(fn, tries: int = 20, delay: float = 0.25):
    """Windows refuses to replace or delete a file another process has open (the web Lab page reading status.json,
    OneDrive syncing); such locks last milliseconds, so retry instead of dying."""
    for k in range(tries):
        try:
            return fn()
        except PermissionError:
            if k == tries - 1:
                raise
            time.sleep(delay)


def _unlink(path: Path):
    """Delete a file, ignoring the failure if it is not there or is locked."""
    try:
        _retry(lambda: path.unlink(missing_ok=True))
    except OSError:
        pass


class _Adopted:
    """A game subprocess started by an earlier runner that is still playing; polled by pid."""

    def __init__(self, pid: int):
        self.pid = pid
        self.returncode = None

    def poll(self):
        """Whether the adopted process is still running."""
        if _pid_alive(self.pid):
            return None
        self.returncode = 0
        return 0

    def kill(self):
        """Stop an adopted process."""
        import subprocess
        subprocess.run(["taskkill", "/F", "/PID", str(self.pid)], capture_output=True)

    terminate = kill


def _find_running_games() -> dict:
    """(experiment, game) -> (pid, start time) for `citar.lab play` processes already running (Windows)."""
    import subprocess
    if os.name != "nt":
        return {}
    ps = ("Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | Where-Object { $_.CommandLine -like '*citar.lab play*' } | "
          "ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, [int64](($_.CreationDate.ToUniversalTime() - [datetime]'1970-01-01').TotalSeconds), $_.CommandLine }")
    try:
        out = subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True, text=True, timeout=60).stdout
    except (OSError, subprocess.SubprocessError):
        return {}
    found = {}
    for line in out.splitlines():
        try:
            pid, started, cmd = line.split("|", 2)
            res = cmd.strip().split()[-1].strip('"')
            name, _, i = Path(res).name[: -len(".result.json")].rpartition("__")
            found[(name, int(i))] = (int(pid), float(started))
        except ValueError:
            continue
    return found


def _collect_orphans(finished_times: list):
    """Results written by games whose runner has gone (a previous runner that was stopped or crashed)."""
    for res_path in JOBS.glob("*.result.json"):
        name, _, i = res_path.name[: -len(".result.json")].rpartition("__")
        try:
            res = json.loads(res_path.read_text(encoding="utf-8"))
            _record(name, int(i), res, finished_times)
        except (OSError, ValueError, json.JSONDecodeError):
            pass
        for q in _job_paths(name, int(i)):
            _unlink(q)


def run(args):
    """Each game is an independent subprocess (`python -m citar.lab play SPEC OUT`), so a hung or crashed game can't
    take the runner down, every game runs the code on disk when it starts, and games that finish after the runner
    exits are collected by the next runner."""
    import subprocess
    _dirs()
    JOBS.mkdir(parents=True, exist_ok=True)
    (LAB / "STOP").unlink(missing_ok=True)
    max_workers = max(args.workers, args.night_workers, 1)
    log(f"runner started (pid {os.getpid()}, up to {max_workers} workers)")
    try:
        from . import usage
        usage.start_power_sampler()        # samples this machine's power unless the CITAR server already does
    except Exception:
        pass
    running = {}           # (exp, i) -> (Popen, started)
    attempts = Counter()
    finished_times: list = []
    _collect_orphans(finished_times)
    for key, (pid, started) in _find_running_games().items():
        running[key] = (_Adopted(pid), started)
        attempts[key] += 1
        log(f"adopted {key[0]} #{key[1]} (pid {pid}, running {(time.time() - started) / 60:.0f} min)")
    for stale in JOBS.glob("*.spec.json"):
        name, _, i = stale.name[: -len(".spec.json")].rpartition("__")
        if (name, int(i) if i.isdigit() else -1) not in running:
            _unlink(stale)
    flags = subprocess.CREATE_NO_WINDOW | subprocess.BELOW_NORMAL_PRIORITY_CLASS if os.name == "nt" else 0
    while True:
        try:
            if _runner_step(args, running, attempts, finished_times, flags, max_workers):
                return
        except Exception:
            # never let one bad pass (a locked file, a malformed spec) kill a multi-day runner
            log("runner error (continuing): " + traceback.format_exc(limit=4).replace("\n", " | "))
        time.sleep(5)


def _runner_step(args, running, attempts, finished_times, flags, max_workers) -> bool:
    """One pass of the runner loop. Returns True when the runner should exit."""
    import subprocess
    if True:
        if (LAB / "STOP").exists():
            log("stop requested: exiting (unfinished games will be replayed by the next runner)")
            for (name, i), (proc, _) in running.items():
                proc.terminate()
            _unlink(LAB / "STOP")
            _write_status(running, finished_times, 0, stopped=True)
            return True
        # finished games
        for key in list(running):
            proc, started = running[key]
            if proc.poll() is None:
                if time.time() - started > args.max_minutes * 60:
                    proc.kill()
                    log(f"{key[0]} #{key[1]} killed after {args.max_minutes} min")
                continue
            del running[key]
            name, i = key
            spec_p, res_p, log_p = _job_paths(name, i)
            try:
                res = json.loads(res_p.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                tail = log_p.read_text(encoding="utf-8", errors="replace")[-3000:] if log_p.exists() else ""
                res = {"exp": name, "i": i, "crash": f"exit code {proc.returncode}\n{tail}"}
            _record(name, i, res, finished_times)
            for q in (spec_p, res_p, log_p):
                _unlink(q)
        # start new games
        cfg = lab_config(args)
        quiet = quiet_now() and not cfg.get("ignore_quiet_hours")
        target = int(cfg["night_workers"] if quiet else cfg["workers"])
        target = max(0, min(target, max_workers))
        queue = load_queue()
        for exp in queue:
            if len(running) >= target:
                break
            done = {r["i"] for r in load_results(exp["name"])}
            if len(done) >= exp["games"]:
                if not any(k[0] == exp["name"] for k in running):
                    _retry(lambda exp=exp: shutil.move(str(QUEUE / f"{exp['name']}.json"),
                                                       str(DONE / f"{exp['name']}.json")))
                    log(f"experiment {exp['name']} complete")
                continue
            for i in range(exp["games"]):
                if len(running) >= target:
                    break
                if i in done or (exp["name"], i) in running or attempts[(exp["name"], i)] >= 3:
                    continue
                attempts[(exp["name"], i)] += 1
                spec_p, res_p, log_p = _job_paths(exp["name"], i)
                spec_p.write_text(json.dumps(game_spec(exp, i)), encoding="utf-8")
                _unlink(res_p)
                with open(log_p, "w", encoding="utf-8") as lf:
                    # Started from the directory that contains the package, so that `-m citar.lab`
                    # resolves to this copy of CITAR rather than to some other one on the path.
                    proc = subprocess.Popen([sys.executable, "-m", "citar.lab", "play", str(spec_p), str(res_p)],
                                            cwd=str(paths.ROOT), stdout=lf, stderr=subprocess.STDOUT,
                                            creationflags=flags)
                running[(exp["name"], i)] = (proc, time.time())
        _write_status(running, finished_times, target)
        if not running:
            if args.exit_when_idle and not load_queue():
                log("queue empty: exiting")
                return True
    return False


def cmd_play(spec_path: str, out_path: str):
    """Play one game as a subprocess. The entry point the runner spawns.

    A subprocess per game, rather than a process pool: the pool deadlocked on Windows under Python
    3.14, and a separate process also means a game that hangs or crashes cannot take the runner with
    it.
    """
    import faulthandler
    faulthandler.enable()       # a hard crash leaves a stack trace in the job log
    spec = json.loads(Path(spec_path).read_text(encoding="utf-8"))
    res = _play_safe(spec)
    tmp = Path(out_path + ".tmp")
    tmp.write_text(json.dumps(res), encoding="utf-8")
    _fs_replace(tmp, out_path)


def _write_status(running, finished_times, target, stopped=False):
    """Write the runner's status file, which the Lab page polls."""
    now = time.time()
    recent = [t for t in finished_times if now - t < 3600]
    exps = {}
    for exp in load_queue():
        exps[exp["name"]] = [len({r["i"] for r in load_results(exp["name"])}), exp["games"]]
    st = {"pid": os.getpid(), "updated": datetime.now().isoformat(timespec="seconds"), "stopped": stopped,
          "quiet_hours": quiet_now(), "target_workers": target,
          "running": [{"exp": e, "i": i, "minutes": round((now - st) / 60, 1)} for (e, i), (_, st) in running.items()],
          "games_last_hour": len(recent), "queue": exps}
    tmp = LAB / "status.json.tmp"
    try:
        tmp.write_text(json.dumps(st, indent=1), encoding="utf-8")
        _fs_replace(tmp, LAB / "status.json")
    except OSError:
        pass            # the next pass writes it again


# ----------------------------------------------------------------------------
# overview for the web Lab page
# ----------------------------------------------------------------------------
def _tail(path: Path, nbytes: int = 4000) -> str:
    """The last few kilobytes of a file, for showing a game's recent output."""
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            return f.read().decode("utf-8", errors="replace")
    except OSError:
        return ""


def _runner_started() -> str:
    """Start time ("YYYY-MM-DD HH:MM") of the latest runner, from lab.log; attempts are counted per runner."""
    for line in reversed(_tail(LAB / "lab.log", 200_000).splitlines()):
        if " runner started " in line:
            return line[:16]
    return ""


def _crash_counts(since: str = "") -> Counter:
    """How many games crashed, by reason, for the status display."""
    counts = Counter()
    for line in _tail(LAB / "crashes.log", 200_000).splitlines():
        if line.startswith("--- ") and not line.endswith("bot errors") and line[4:20] >= since:
            parts = line.split()
            if len(parts) >= 5 and parts[-1].startswith("#"):
                counts[(parts[-2], int(parts[-1][1:]))] += 1
    return counts


TURN_GROWTH = 2.5   # elapsed time grows about as turn ** 2.5: late turns have many more cities and units


def _projected_total(r: dict):
    """Projected length in minutes of a running game from its turn progress, or None if it is too early to tell."""
    if r.get("turn") and r.get("limit") and r["turn"] / r["limit"] > 0.1:
        return r["minutes"] * (r["limit"] / r["turn"]) ** TURN_GROWTH
    return None


def _game_left(r: dict, per_game: float) -> float:
    """Minutes a running game still needs: from its turn progress when it reports turns, otherwise from the median
    game length (never less than a tenth of it, since a slow game past the median is still running)."""
    total = _projected_total(r)
    if total is not None:
        return total - r["minutes"]
    return max(per_game - r["minutes"], 0.1 * per_game)


def overview() -> dict:
    """Everything the web Lab page shows: runner health, per-experiment progress and ETA, running games with their
    current turn, recent log lines and the side runs (citar.bench jobs writing saves/lab/*.out)."""
    _dirs()
    now = time.time()
    try:
        st = json.loads((LAB / "status.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        st = {}
    alive = bool(st.get("pid")) and _pid_alive(st["pid"]) and not st.get("stopped")
    try:
        updated_age = now - datetime.fromisoformat(st["updated"]).timestamp()
    except (KeyError, ValueError):
        updated_age = None
    workers = max(1, int(st.get("target_workers") or 1))
    crashes = _crash_counts(_runner_started())

    running = []
    for r in st.get("running", []):
        _, _, log_p = _job_paths(r["exp"], r["i"])
        turn = limit = None
        for line in reversed(_tail(log_p, 2000).splitlines()):
            if line.startswith("PROGRESS "):
                bits = line.split()
                turn, limit = int(bits[1]), int(bits[2]) or None
                break
        running.append({"exp": r["exp"], "i": r["i"], "minutes": r["minutes"], "turn": turn, "limit": limit,
                        "attempt": crashes.get((r["exp"], r["i"]), 0) + 1})

    experiments = []
    specs = [(json.loads(p.read_text(encoding="utf-8")), "queued") for p in sorted(QUEUE.glob("*.json"))]
    specs += [(json.loads(p.read_text(encoding="utf-8")), "complete")
              for p in sorted(DONE.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)]
    remaining_minutes = 0.0
    for r in running:
        total = _projected_total(r)
        r["left_minutes"] = round(total - r["minutes"], 1) if total is not None else None
    for exp, state in specs:
        res = load_results(exp["name"])
        done = len({r["i"] for r in res})
        mins = [r["seconds"] / 60 for r in res if r.get("seconds")]
        per_game = statistics.median(mins) if mins else None
        turns = [r["turns"] for r in res if r.get("turns")]
        mine = [r for r in running if r["exp"] == exp["name"]]
        failing = sorted(i for (e, i), n in crashes.items() if e == exp["name"] and n >= 3
                         and i not in {r["i"] for r in res} and i not in {r["i"] for r in mine})
        if state == "queued":
            if per_game is None:
                # no finished game yet: project the running ones from their turn progress
                proj = [t for t in map(_projected_total, mine) if t is not None]
                per_game = statistics.median(proj) if proj else 30.0 if not exp.get("turns") else 30.0 * exp["turns"] / 330
            todo = exp["games"] - done - len(mine) - len(failing)
            left = max(0, todo) * per_game + sum(_game_left(r, per_game) for r in mine)
            remaining_minutes += left
            if failing and done + len(failing) >= exp["games"] and not mine:
                state = "stalled"
            elif mine:
                state = "running"
        else:
            left = 0.0
        experiments.append({
            "name": exp["name"], "state": state, "games": exp["games"], "done": done, "running": len(mine),
            "failing": failing, "priority": exp.get("priority", 0), "note": exp.get("note") or exp.get("description"),
            "kind": "factorial" if exp.get("factors") else "seats",
            "factors": list((exp.get("factors") or {}).keys()),
            "seats": [s.get("label") for s in exp.get("seats", [])] if not exp.get("factors") else [],
            "turns": exp.get("turns") or None, "median_turns": statistics.median(turns) if turns else None,
            "minutes_per_game": round(per_game, 1) if per_game else None, "remaining_minutes": round(left, 1),
            "submitted": exp.get("submitted"),
            "finished": max((r.get("finished") or "" for r in res), default=None),
        })
    for exp in experiments:
        if exp["state"] in ("queued", "running"):
            exp["eta_minutes"] = None     # filled in below by simulating the queue order
    # queue simulation: the runner fills free workers in priority order, so estimate each experiment's finish
    # as the cumulative work ahead of it divided by the worker count
    order = sorted([e for e in experiments if e["state"] in ("queued", "running")], key=lambda e: -e["priority"])
    acc = 0.0
    for e in order:
        acc += e["remaining_minutes"]
        e["eta_minutes"] = round(acc / workers, 1)

    side = []
    for out in sorted(LAB.glob("*.out"), key=lambda p: p.stat().st_mtime, reverse=True):
        if out.name == "runner.out":
            continue
        text = _tail(out, 6000)
        lines = [ln for ln in text.splitlines() if ln.strip()]
        age = now - out.stat().st_mtime
        side.append({"name": out.stem, "modified": datetime.fromtimestamp(out.stat().st_mtime).isoformat(timespec="seconds"),
                     "active": age < 900 and not any("Report written" in ln or "stopping" in ln for ln in lines[-6:]),
                     "tail": lines[-12:]})

    return {
        "now": datetime.now().isoformat(timespec="seconds"),
        "runner": {"pid": st.get("pid"), "alive": alive, "updated": st.get("updated"), "updated_age": updated_age,
                   "workers": st.get("target_workers"), "games_last_hour": st.get("games_last_hour"),
                   "quiet_hours": st.get("quiet_hours"), "config": lab_config_file()},
        "remaining_minutes": round(remaining_minutes, 1), "eta_minutes": round(remaining_minutes / workers, 1),
        "running": sorted(running, key=lambda r: (r["exp"], r["i"])), "experiments": experiments,
        "log": _tail(LAB / "lab.log", 6000).splitlines()[-40:], "side_runs": side,
    }


def lab_config_file() -> dict:
    """The lab's configuration file, if it exists."""
    try:
        return json.loads((LAB / "config.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def report_text(name: str) -> str:
    """The `lab report NAME` text, for the web page."""
    import contextlib
    import io
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        try:
            cmd_report([name])
        except Exception as e:   # a malformed result line should not break the page
            print(f"report failed: {type(e).__name__}: {e}")
    return buf.getvalue()


# ----------------------------------------------------------------------------
# reports
# ----------------------------------------------------------------------------
def _mean(xs):
    """The mean of the values that are not None."""
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else None


def _ci(xs):
    """Mean and 95% half-width."""
    xs = [x for x in xs if x is not None]
    if not xs:
        return None, None
    m = statistics.mean(xs)
    if len(xs) < 2:
        return m, None
    return m, 1.96 * statistics.stdev(xs) / math.sqrt(len(xs))


def summarize(results: list[dict]) -> dict:
    """Summarise an experiment's results: per label, per checkpoint, and head to head."""
    res = [r for r in results if not r.get("crash")]
    out = {"games": len(res), "victories": dict(Counter(r["victory"] or "none" for r in res)),
           "median_end_turn": statistics.median([r["turns"] for r in res]) if res else None,
           "avg_minutes": round(_mean([r["seconds"] for r in res]) / 60, 1) if res else None,
           "bot_errors": sum(len(r.get("errors") or []) for r in res),
           "engines": dict(Counter(r.get("engine") for r in res))}
    by_label = defaultdict(list)
    for r in res:
        for p in r["players"].values():
            by_label[p["label"]].append((r, p))
    labels = {}
    for lab, rows in sorted(by_label.items()):
        ps = [p for _, p in rows]
        # share of this label's games that a seat of this label won (a label with two seats in a game wins it once)
        wins = len({r["i"] for r, p in rows if r["winner_label"] == lab}) / max(1, len({r["i"] for r, _ in rows}))
        share, share_ci = _ci([p["score_share"] for p in ps])
        d = {"seats": len(ps), "win_share": round(wins, 3),
             "wins_by_type": dict(Counter(r["victory"] for r, p in rows if r["winner_label"] == lab and r["winner"] is not None
                                          and str(r["winner"]) in r["players"] and r["players"][str(r["winner"])] is p)),
             "score_share": round(share, 3), "score_share_ci": round(share_ci, 3) if share_ci else None,
             "mean_rank": round(_mean([p["rank"] for p in ps]), 2),
             "eliminated": round(sum(1 for p in ps if not p["alive"]) / len(ps), 3),
             "final_techs": round(_mean([p["techs"] for p in ps]) or 0, 1),
             "final_cities": round(_mean([p["cities"] for p in ps]), 1),
             "unhappy_share": round(_mean([p["unhappy_share"] for p in ps]) or 0, 3),
             "religion_share": round(sum(1 for p in ps if p["religion"] in ("religion", "enhancing", "enhanced")) / len(ps), 2),
             "captured": round(_mean([p["events"].get("captured_city", 0) for p in ps]), 2),
             "wars": round(_mean([p["events"].get("declared_war", 0) for p in ps]), 2)}
        for cp in (100, 200, 300):
            rows_cp = [p["checkpoints"].get(str(cp)) or p["checkpoints"].get(cp) for p in ps]
            rows_cp = [x for x in rows_cp if x]
            if rows_cp:
                d[f"t{cp}"] = {k: round(_mean([x.get(k) for x in rows_cp]), 1) for k in ("techs", "cities", "population", "score")}
        labels[lab] = d
    out["labels"] = labels
    # head to head: per game, the mean score share of each label; pairwise differences
    pairs = {}
    names = sorted(by_label)
    for a in names:
        for b in names:
            if a >= b:
                continue
            diffs = []
            for r in res:
                sa = [p["score_share"] for p in r["players"].values() if p["label"] == a]
                sb = [p["score_share"] for p in r["players"].values() if p["label"] == b]
                if sa and sb:
                    diffs.append(statistics.mean(sa) - statistics.mean(sb))
            m, ci = _ci(diffs)
            if m is not None:
                pairs[f"{a} - {b}"] = {"games": len(diffs), "score_share_diff": round(m, 4),
                                       "ci95": round(ci, 4) if ci else None,
                                       "significant": bool(ci and abs(m) > ci)}
    out["head_to_head"] = pairs
    return out


def print_report(name: str, s: dict, exp: dict | None = None):
    """Print an experiment's summary."""
    print(f"=== {name}: {s['games']} games" + (f" of {exp['games']}" if exp else "") +
          f" | victories {s['victories']} | median end T{s['median_end_turn']} | {s['avg_minutes']} min/game"
          f" | bot errors {s['bot_errors']}")
    if len(s["engines"]) > 1:
        print(f"  ! games ran on {len(s['engines'])} engine versions: {s['engines']}")
    for lab, d in s["labels"].items():
        t = " ".join(f"T{cp}:{d[f't{cp}']['techs']:.0f}t/{d[f't{cp}']['cities']:.1f}c" for cp in (100, 200, 300) if f"t{cp}" in d)
        print(f"  {lab:>16}: win {d['win_share']:.2f} {d['wins_by_type']} share {d['score_share']:.3f}+-{d['score_share_ci'] or 0:.3f} "
              f"rank {d['mean_rank']} techs {d['final_techs']} cities {d['final_cities']} elim {d['eliminated']} "
              f"unhappy {d['unhappy_share']} rel {d['religion_share']} cap {d['captured']} | {t}")
    for k, v in s["head_to_head"].items():
        print(f"  {k}: {v['score_share_diff']:+.4f} +-{v['ci95'] or 0:.4f} over {v['games']} games"
              f"{' *' if v['significant'] else ''}")


def factor_effects(results: list[dict], metric=lambda p: p["score_share"]) -> dict:
    """Within-game effect of each factor level versus the factor's first level (mean over games, 95% CI)."""
    res = [r for r in results if not r.get("crash")]
    out = {}
    factors = {}
    for r in res:
        for p in r["players"].values():
            for f, v in (p.get("levels") or {}).items():
                factors.setdefault(f, [])
                if v not in factors[f]:
                    factors[f].append(v)
    for f, levels in factors.items():
        levels = sorted(levels, key=lambda v: (str(type(v)), v))
        base = levels[0]
        for lv in levels[1:]:
            diffs = []
            for r in res:
                a = [metric(p) for p in r["players"].values() if (p.get("levels") or {}).get(f) == lv]
                b = [metric(p) for p in r["players"].values() if (p.get("levels") or {}).get(f) == base]
                if a and b:
                    diffs.append(statistics.mean(a) - statistics.mean(b))
            m, ci = _ci(diffs)
            if m is not None:
                out[f"{f}: {lv} vs {base}"] = (round(m, 4), round(ci, 4) if ci else None, len(diffs))
    return out


def cmd_report(names):
    """Report on one or more experiments."""
    queue = {e["name"]: e for e in load_queue()}
    for name in names:
        exp = queue.get(name)
        if exp is None and (DONE / f"{name}.json").exists():
            exp = json.loads((DONE / f"{name}.json").read_text(encoding="utf-8"))
        results = [complete_players(exp, r) for r in load_results(name)]
        if exp and exp.get("factors"):
            s = summarize(results)
            print(f"=== {name} (factorial): {s['games']} games of {exp['games']} | victories {s['victories']} | "
                  f"median end T{s['median_end_turn']} | {s['avg_minutes']} min/game | bot errors {s['bot_errors']}")
            for label, metric in (("score share", lambda p: p["score_share"]), ("final techs", lambda p: p["techs"]),
                                  ("final cities", lambda p: p["cities"]),
                                  ("T200 techs", lambda p: (p["checkpoints"].get("200") or {}).get("techs"))):
                eff = factor_effects(results, lambda p, m=metric: m(p) if m(p) is not None else 0)
                print(f"  {label}:")
                for k, (m, ci, n) in eff.items():
                    print(f"    {k:40s} {m:+.4f} +-{ci or 0:.4f} ({n} games){' *' if ci and abs(m) > ci else ''}")
            continue
        print_report(name, summarize(results), exp)


def cmd_status():
    """Print what the runner is doing, or say that it is not running."""
    try:
        st = json.loads((LAB / "status.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        print("no status (runner never started?)")
        return
    alive = _pid_alive(st.get("pid"))
    print(f"runner pid {st.get('pid')} {'ALIVE' if alive else 'NOT RUNNING'} | updated {st['updated']} | "
          f"quiet {st['quiet_hours']} | workers {st['target_workers']} | games last hour {st['games_last_hour']}")
    for r in st["running"]:
        print(f"  running {r['exp']} #{r['i']} for {r['minutes']} min")
    for name, (d, n) in st["queue"].items():
        print(f"  queued {name}: {d}/{n}")


def _pid_alive(pid) -> bool:
    """Whether a process id is still running."""
    if not pid:
        return False
    if os.name == "nt":
        import subprocess
        out = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/NH"], capture_output=True, text=True).stdout
        return str(pid) in out
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def main(argv=None):
    """The ``citar lab`` command line."""
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) - 1))
    r.add_argument("--night-workers", type=int, default=2)
    r.add_argument("--exit-when-idle", action="store_true")
    r.add_argument("--max-minutes", type=float, default=150, help="kill a game that runs longer than this")
    pl = sub.add_parser("play")
    pl.add_argument("spec")
    pl.add_argument("out")
    s = sub.add_parser("submit")
    s.add_argument("specs", nargs="+")
    rep = sub.add_parser("report")
    rep.add_argument("names", nargs="+")
    sub.add_parser("status")
    sub.add_parser("stop")
    args = ap.parse_args(argv)
    if args.cmd == "run":
        run(args)
    elif args.cmd == "play":
        cmd_play(args.spec, args.out)
    elif args.cmd == "submit":
        for path in args.specs:
            data = json.loads(Path(path).read_text(encoding="utf-8"))
            for spec in (data if isinstance(data, list) else [data]):
                s = submit(spec)
                print(f"queued {s['name']}: {s['games']} games, seats {[x['label'] + '=' + x['bot'] for x in s['seats']]}")
    elif args.cmd == "report":
        cmd_report(args.names)
    elif args.cmd == "status":
        cmd_status()
    elif args.cmd == "stop":
        _dirs()
        (LAB / "STOP").write_text("stop", encoding="utf-8")
        print("stop requested")


if __name__ == "__main__":
    main()
