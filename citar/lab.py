"""CITAR lab: a resumable queue of bot-vs-bot experiments for tuning the scripted bots, played inside the CITAR server.

    python -m citar.lab submit SPEC.json [...]      # queue experiments (bot versions pinned at submit)
    python -m citar.lab status                      # what is queued and playing, from the server's database
    python -m citar.lab report NAME [NAME ...]      # results, per seat label and head to head
    python -m citar.lab stop                        # pause: running games finish, nothing new starts
    python -m citar.lab resume                      # start new games again

Everything lives in saves/lab/: queue/NAME.json (experiments waiting or playing), results/NAME.jsonl (one line per
finished game), reference/NAME.jsonl (a reference run's games, the statistical baseline's lines), done/NAME.json
(finished experiments), lab.log (what the dispatcher did) and crashes.log (games that crashed, with their traces).

**The games are played by the server** (design/phase4-helper.md P4.5.5-P4.5.7): its job dispatcher
(``citar.jobs.dispatcher``) turns each game of a queued experiment into a CPU job and offers it to an executor, the
server's own job host (``citar.jobs.local``, under the host machine's CPU settings) and, from 0.1.6's next package on,
helpers lending their CPU. ``python -m citar.lab run`` and ``citar lab run``, the separate runner process of 0.1.5,
are retired: they say so and exit 2. Start the server (local or server mode) and the lab plays.

An experiment is a JSON object:

    {"name": "prince-baseline", "priority": 5, "games": 24, "seed": 1000,
     "size": "small", "maps": ["continents", "pangaea"], "speed": "Quick", "turns": 0,
     "difficulty": "Prince", "barbarians": "normal", "barbarian_difficulty": null, "nation": "BenchmarkCiv",
     "seats": [{"label": "new", "bot": "basic", "difficulty": "Prince", "params": {}},
               {"label": "old", "bot": "basic-1"}, {"profile": "my-profile"}, ...],
     "rotate": true}

``kind`` says what it is (P4.4.1): ``lab`` (the default; a member's or an administrator's experiment), ``ranking``
(a game of the site's own ladder) or ``reference`` (a run of the statistical baseline the bots are compared against:
no seats, the baseline's own, with ``params`` overriding every seat's bot; ``turns`` its turn limit). The last two are
an administrator's, and the server's own data collection: nobody's credits pay for them (``citar lab submit``
registers them so, and refuses an owner who is not an administrator).

A seat names a bot version ("bot": "basic" for the latest, "basic-N", "idle") with parameter overrides, or a bot
profile ("profile": id; see citar.bots.profiles): the profile's engine, overrides and aggression are resolved into
the experiment when it is submitted, so later edits to the profile don't change a queued experiment. "params" on such
a seat are layered on top of the profile's. Factorial experiments may name a "profile" as their base bot in the same
way.

Bot versions are pinned when an experiment is submitted: "basic" becomes the version it names then (basic-1), so a
newer version compiled in later does not mix into a queued experiment (use "live" to opt out and play the latest
version at play time). Every result records, per seat, what played: the build id (the engine's and the bot's code and
the ruleset), the bot version, the profile and its revision, the overrides, the fixed aggression as the bot played it
and the fingerprint, all taken when the game is played. A seat queued as {"profile": "best"} is recorded as the
profile "best" stood for at submission.

Optional map generation keys pass straight to the generator: "map_edges" (ice_caps, wrap_x, wrap_y, wrap_both,
boxed), "river_density" (1 = normal) and "resources" (densities and per-resource rules; see mapgen.MapOptions).

Game i uses seed+i and maps[i % len(maps)]; with "rotate" the seat list is rotated by i so every label plays every
start position. "turns" 0 plays to the speed's time-victory turn, and a limit past it is refused. A seat's aggression,
unless its profile fixes one (or the seat does, with "aggression", held to 0..1 at submission), comes from its start
position and the seed, so rotation evens it out.

Every game is played as a job (``engine_api.run_job``'s Rust, P4.4), so ``submit`` checks the experiment's games as
jobs and refuses one that a job would refuse (a turn limit past the speed's, a seed past 2^53 - 1, a label or a name of
more than 200 characters, an override the bot does not take), saying which game and why. A game that crashes (a panic
of the engine or a bot) crashes the same way every time it is played, so its crash is its result: written to the
results file as a crash line, to crashes.log with its trace, and never played again.

A finished game's line is its job's result plus the times its executor measured (``seconds``, ``started_ts``,
``cpu_s``, ``finished``) and ``executor``: ``{"machine", "run", "verification"}``. The dispatcher writes it once
(P4.5.7: the job is committed first, the line appended after, and the two reconciled when the server starts).

``submit`` loads the engine before anything else and stops with exit 2 when it does not load (the extension missing or
out of date, or a ``CITAR_RULESET_DIR`` that does not load): every game would fail the same way.
"""
from __future__ import annotations

import argparse
import json
import logging
import math
import statistics
import sys
import time
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path
from typing import Optional

from .fsutil import newest_first as _fs_newest, read_text as _fs_read, write_text as _fs_write
from . import paths

_log = logging.getLogger("citar.lab")

LAB = paths.saves_path("lab")
QUEUE, RESULTS, DONE, REFERENCE = LAB / "queue", LAB / "results", LAB / "done", LAB / "reference"
#: The pause (``citar lab stop``, the Lab page's Stop): while it exists the dispatcher starts no game.
PAUSED_NAME = "PAUSED"

#: An experiment's kind and the kind of job each of its games is (design/phase4-helper.md P4.4.1).
JOB_KINDS = {"lab": "lab_game", "ranking": "ranking_game", "reference": "reference_game"}
#: The kinds only an administrator queues, which are the server's own data collection: nobody's credits pay for them.
COLLECTION_KINDS = ("ranking", "reference")


def _dirs():
    """Create the lab's directories if they do not exist."""
    for d in (QUEUE, RESULTS, DONE, REFERENCE):
        d.mkdir(parents=True, exist_ok=True)


def log(msg: str):
    """Append to the lab log (the Lab page shows its last lines) and to the server's log."""
    _dirs()
    line = f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}"
    _log.info("%s", msg)
    with open(LAB / "lab.log", "a", encoding="utf-8") as f:
        f.write(line + "\n")


def engine_hash() -> str:
    """The build id of the engine that plays (``engine_api.build_info``), recorded with every result: 12 hex digits
    over the engine's and the bot's code and the ruleset, so that a result from three weeks ago can be identified as
    having been produced by different code, rather than silently compared against today's."""
    from . import engine_api
    return engine_api.build_info()["build_id"]


# ----------------------------------------------------------------------------
# the pause
# ----------------------------------------------------------------------------
def paused() -> Optional[dict]:
    """The pause, ``{"at", "by"}``, while the lab is paused (``citar lab stop`` or the Lab page's Stop): the dispatcher
    offers no game, and running ones finish. None otherwise."""
    try:
        text = _fs_read(LAB / PAUSED_NAME)
    except FileNotFoundError:
        return None
    except OSError:
        return {"at": None, "by": None}
    try:
        data = json.loads(text)
    except ValueError:
        data = {}
    return data if isinstance(data, dict) else {}


def pause(by: str = "citar lab stop") -> dict:
    """Pause the lab: the dispatcher starts no new game (running ones finish) until :func:`resume`."""
    _dirs()
    data = {"at": datetime.now().isoformat(timespec="seconds"), "by": by}
    _fs_write(LAB / PAUSED_NAME, json.dumps(data))
    return data


def resume() -> bool:
    """End the pause; whether there was one."""
    try:
        (LAB / PAUSED_NAME).unlink()
        return True
    except FileNotFoundError:
        return False


# ----------------------------------------------------------------------------
# experiments
# ----------------------------------------------------------------------------
DEFAULTS = {"priority": 0, "games": 12, "seed": 1000, "size": "small", "maps": ["continents", "pangaea", "fractal"],
            "speed": "Quick", "turns": 0, "difficulty": "Prince", "barbarians": "normal", "barbarian_difficulty": None,
            "nation": "BenchmarkCiv", "city_states": None, "rotate": True}
#: A reference run's defaults: the baseline's game, which seats its own bots (P4.4.1).
REFERENCE_DEFAULTS = {"priority": 0, "games": 12, "seed": 1000, "size": "small",
                      "maps": ["continents", "pangaea", "fractal"], "speed": "Quick", "turns": 0,
                      "barbarians": "normal", "params": None}


def kind_of(exp: dict) -> str:
    """An experiment's kind: ``lab`` (also for one queued before kinds existed), ``ranking`` or ``reference``."""
    return exp.get("kind") or "lab"


def job_kind(exp: dict) -> str:
    """The kind of job each game of the experiment is (``lab_game``, ``ranking_game``, ``reference_game``)."""
    return JOB_KINDS[kind_of(exp)]


def _resolve_seat(seat: dict) -> dict:
    """A seat as it will be played: a profile resolved into its version, overrides and aggression, or a raw bot seat
    with its version pinned, its overrides cleaned and its aggression held to 0..1 (a seat that names a frozen
    snapshot, overrides its version does not have or an aggression that is no number is refused now rather than when
    its game is played). Records the profile's id ("best" as the profile it stands for now, so that its games are
    rated as that profile's), revision and name."""
    from .bots import profiles
    seat = dict(seat)
    if seat.get("profile"):
        r = profiles.resolve(seat, pin=True)
        seat.update({"profile": r["profile"], "bot": r["engine"], "params": r["params"], "aggression": r["aggression"],
                     "profile_rev": r["profile_rev"], "profile_name": r["profile_name"]})
        if not seat.get("label"):
            seat["label"] = r["profile_name"]
        return seat
    seat["bot"] = _pin(seat.get("bot"))
    seat["params"] = profiles.clean_params(seat["bot"], seat.get("params"))
    seat["aggression"] = profiles.clean_aggression(seat.get("aggression"))
    return seat


def _pin(bot) -> str:
    """The version a seat's bot names now ("basic" is the latest), or "basic" for a seat that opted out of pinning
    with "live", which then plays the latest version at play time."""
    from .bots import profiles
    return profiles.LATEST if bot == "live" else profiles.version(bot or profiles.LATEST)


def normalize(spec: dict) -> dict:
    """Validate an experiment and fill in its defaults, pinning the bot versions it names."""
    kind = kind_of(spec)
    if kind not in JOB_KINDS:
        raise ValueError(f"An experiment's kind is one of {', '.join(JOB_KINDS)}, not {kind!r}.")
    if kind == "reference":
        return _normalize_reference(spec)
    s = dict(DEFAULTS)
    s.update(spec)
    s["kind"] = kind
    if not s.get("name"):
        raise ValueError("An experiment needs a name.")
    if s.get("factors"):
        from .bots import profiles
        if s.get("profile"):
            r = profiles.resolve(s["profile"], pin=True)
            s["profile"] = r["profile"]                 # "best" as the profile it stands for now
            s["bot"] = r["engine"]
            s["base_params"] = {**r["params"], **(s.get("base_params") or {})}
            if r["aggression"] is not None and s.get("aggression") is None:
                s["aggression"] = r["aggression"]
            s["profile_rev"] = r["profile_rev"]
            s["profile_name"] = r["profile_name"]
        else:
            s["bot"] = _pin(s.get("bot"))
        s["aggression"] = profiles.clean_aggression(s.get("aggression"))
        # every level must be an override the version takes, on the base parameters: refused now, not mid-run
        for f, levels in (s["factors"] or {}).items():
            for level in levels:
                profiles.clean_params(s["bot"], {**(s.get("base_params") or {}), f: level})
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
    check_games(s)
    return s


def _normalize_reference(spec: dict) -> dict:
    """A reference run (P4.4.1): the baseline's games, which seat the baseline's own bots, ``params`` overriding every
    seat's (a profile's overrides, cleaned by the latest version), ``turns`` the turn limit (0: the speed's)."""
    from .bots import profiles
    s = dict(REFERENCE_DEFAULTS)
    s.update(spec)
    s["kind"] = "reference"
    if not s.get("name"):
        raise ValueError("An experiment needs a name.")
    if s.get("seats") or s.get("factors"):
        raise ValueError("A reference run has no seats or factors: it plays the baseline's own seats.")
    if s.get("params"):
        s["params"] = profiles.clean_params(profiles.LATEST, s["params"])
    s["submitted"] = s.get("submitted") or datetime.now().isoformat(timespec="seconds")
    check_games(s)
    return s


#: The most games of one experiment checked one by one as jobs (a tenth of a millisecond each); past it, the last
#: game is checked too, whose seed is the largest.
CHECK_GAMES = 5000


def check_games(exp: dict):
    """Refuse (ValueError) an experiment whose games a job would refuse: each game's spec through the jobs crate's
    own check (``engine_api.check_job_spec``), which holds the bounds of design/phase4-helper.md P4.4.1 (the turn limit
    within the speed's, the seed within 2^53 - 1, text of 200 characters at most, overrides the bot takes, settings the
    engine knows). Every game is checked, as a factorial experiment's labels and overrides differ from game to game; an
    experiment of more than ``CHECK_GAMES`` games has its first ones and its last checked."""
    from . import engine_api
    n = int(exp.get("games") or 0)
    games = list(range(min(n, CHECK_GAMES)))
    if n > CHECK_GAMES:
        games.append(n - 1)
    for i in games:
        try:
            engine_api.check_job_spec(*job_of(exp, i))
        except engine_api.JobRefused as e:
            raise ValueError(f"Game {i} of experiment {exp.get('name')} cannot be played: {e}") from None


def submit(spec: dict) -> dict:
    """Queue an experiment, refusing to overwrite one of the same name. Written whole (a temporary file, then a
    rename): the dispatcher and the Lab page list the queue whenever they like, and must never read half of it."""
    _dirs()
    s = normalize(spec)
    path = QUEUE / f"{s['name']}.json"
    if path.exists():
        raise ValueError(f"Experiment {s['name']} already exists (delete saves/lab/queue/{s['name']}.json first).")
    _fs_write(path, json.dumps(s, indent=1))
    return s


def load_queue() -> list[dict]:
    """Every queued experiment, most important first. A file read while the server changes its priority
    (``set_priority``) is waited for: one skipped would leave its games unstarted."""
    _dirs()
    out = []
    for p in QUEUE.glob("*.json"):
        try:
            out.append(json.loads(_fs_read(p)))
        except (OSError, json.JSONDecodeError):
            continue
    out.sort(key=lambda s: (-s.get("priority", 0), s.get("submitted", "")))
    return out


def set_priority(name: str, priority: int) -> int:
    """Change a queued experiment's priority (the Queue page), held to -100..100; returns it. The file is replaced
    while the dispatcher may be reading it, so both sides retry; the dispatcher gives its queued games the new
    priority within a tick."""
    path = QUEUE / f"{name}.json"
    spec = json.loads(_fs_read(path))
    spec["priority"] = max(-100, min(100, int(priority)))
    _fs_write(path, json.dumps(spec, indent=1))
    return spec["priority"]


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


def results_path(name: str, kind: str = "lab") -> Path:
    """Where an experiment's finished games are: results/NAME.jsonl, or reference/NAME.jsonl for a reference run
    (``scripts/refcheck/summarize.py`` reads those as it reads a baseline's)."""
    return (REFERENCE if kind == "reference" else RESULTS) / f"{name}.jsonl"


def load_results(name: str, kind: str = "lab") -> list[dict]:
    """Every finished game of an experiment, crash lines included (each says ``crash``)."""
    path = results_path(name, kind)
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
             "aggression": exp.get("aggression"), "profile": exp.get("profile"),
             "profile_rev": exp.get("profile_rev"), "profile_name": exp.get("profile_name")}
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


def reference_spec(exp: dict, i: int) -> dict:
    """Game *i* of a reference run as a reference job's spec (``citar_jobs::ReferenceSpec``): its seed, map size and
    type, barbarians, speed, turn limit (None: the speed's) and the overrides of every seat's bot."""
    maps = exp.get("maps") or ["continents"]
    return {"i": i, "seed": exp["seed"] + i, "size": exp["size"], "map_type": maps[i % len(maps)],
            "barbarians": exp.get("barbarians") or "normal", "speed": exp["speed"],
            "turn_limit": exp.get("turns") or None, "params": exp.get("params") or None}


def job_of(exp: dict, i: int) -> tuple[str, dict]:
    """Game *i* of an experiment as a job: (its kind, its spec)."""
    kind = job_kind(exp)
    if kind == "reference_game":
        return kind, reference_spec(exp, i)
    return kind, job_spec(game_spec(exp, i))


# ----------------------------------------------------------------------------
# one game, in this process (tests, and the lab's own checks): the server plays them through its dispatcher
# ----------------------------------------------------------------------------
def make_bot(seat: dict, aggression: float):
    """The bot a seat calls for: its version with its overrides, playing the seat's fixed aggression if it has one
    (a profile's, which its fingerprint hashes) and ``aggression``, its start position's, otherwise."""
    from . import engine_api
    from .bots import profiles
    bot = profiles.check_engine(seat.get("bot") or profiles.LATEST)
    fixed = profiles.clean_aggression(seat.get("aggression"))
    try:
        return engine_api.bot_instance(bot, aggression=aggression, params=seat.get("params") or {},
                                       fixed_aggression=fixed)
    except ValueError as e:
        raise profiles.ProfileError(str(e)) from None


#: The keys of a lab job's spec (``citar_jobs::LabSpec``), game_spec's, and of each of its seats.
JOB_KEYS = ("exp", "i", "seed", "map_type", "size", "speed", "turns", "difficulty", "barbarians",
            "barbarian_difficulty", "nation", "city_states", "seats", "ai_base_values", "map_edges", "river_density",
            "resources")
JOB_SEAT_KEYS = ("label", "bot", "difficulty", "params", "aggression", "profile", "profile_rev", "profile_name",
                 "levels", "nation")


def job_spec(spec: dict) -> dict:
    """``game_spec``'s dict as a lab job's spec (design/phase4-helper.md P4.4.1): its keys and each seat's, a seat's
    fixed aggression held to 0..1 as its bot holds it (an experiment queued before seats were held so may carry
    another). Whatever else an experiment's seat carries (a key of its own) is left out rather than refused: a job's
    spec refuses unknown keys."""
    from .bots import profiles
    out = {k: spec[k] for k in JOB_KEYS if k in spec}
    seats = []
    for seat in spec["seats"]:
        s = {k: seat[k] for k in JOB_SEAT_KEYS if k in seat}
        if "aggression" in s:
            s["aggression"] = profiles.clean_aggression(s["aggression"])
        seats.append(s)
    out["seats"] = seats
    return out


def crash_text(crash: dict) -> str:
    """A lab job's crash outcome (``{"turn", "player", "label", "message"}``) as a crash line: ``T<turn> P<player>
    <label>: panic: <message>``."""
    where = f"T{crash.get('turn')}"
    if crash.get("player") is not None:
        where += f" P{crash['player']}"
    if crash.get("label"):
        where += f" {crash['label']}"
    return f"{where}: panic: {crash.get('message')}"


def play(spec: dict) -> dict:
    """Play one lab game to the end in this process and return its result line (the tests', and a check by hand: the
    server plays the lab's games through its dispatcher and its job hosts).

    The game is a job (``engine_api.run_job``, the jobs crate: the same Rust a job host plays it with), and its result
    is the job's, built in Rust from the game: the deterministic part a result hash covers (design/phase4-helper.md
    P4.4.2), with the times added here, outside it (``seconds``, ``started_ts``, ``cpu_s``, ``finished``). A game that
    panics comes back as ``{"exp", "i", "crash", "final", "finished"}``: its turn, seat and message (and its trace).
    A spec the jobs crate refuses comes back so too, its reason the crash."""
    from . import engine_api
    t0 = time.time()
    try:
        out = engine_api.run_job("lab_game", job_spec(spec))
    except engine_api.JobRefused as e:
        return {"exp": spec["exp"], "i": spec["i"], "crash": f"JobRefused ({e.code}): {e}", "final": True,
                "finished": datetime.now().isoformat(timespec="seconds")}
    finished = datetime.now().isoformat(timespec="seconds")
    result = out["result"]
    if "crash" in result:
        text = crash_text(result["crash"])
        if out.get("trace"):
            text += "\n" + out["trace"]
        return {"exp": spec["exp"], "i": spec["i"], "crash": text, "final": True, "finished": finished}
    return {**result, "seconds": round(out["wall_s"], 1), "started_ts": round(t0, 1),
            "cpu_s": round(out["cpu_s"], 1), "finished": finished}


# ----------------------------------------------------------------------------
# a finished job as a results line (the dispatcher writes it)
# ----------------------------------------------------------------------------
#: The keys that name one reference game, as the baseline's lines carry them (``summarize.py``'s IDENTITY).
REFERENCE_IDENTITY = ("i", "seed", "size", "map_type", "barbarians", "speed", "turn_limit")


def result_line(kind: str, exp: str, i: int, spec: dict, result: dict, *, wall_s: float, cpu_s: float,
                started_ts: Optional[float], finished: str, executor: dict) -> dict:
    """A job's result as its line in the results file (P4.5.7): the deterministic result with the times its executor
    measured and ``executor`` ({"machine", "run", "verification"}). A crash is a line too, final: a lab or ranking
    game's ``{"exp", "i", "crash": "T<turn> P<player> <label>: panic: <message>", "final": true, ...}``, a reference
    game's the baseline's crash line, its identity and ``crash``."""
    if kind == "reference_game":
        if "crash" in result:
            line = {k: spec.get(k) for k in REFERENCE_IDENTITY}
            line["crash"] = str(result["crash"])
        else:
            line = dict(result)
        line.update({"seconds": round(wall_s, 3), "cpu_s": round(cpu_s, 3), "finished": finished,
                     "executor": executor})
        return line
    if "crash" in result:
        crash = result["crash"]
        return {"exp": exp, "i": i, "crash": crash_text(crash) if isinstance(crash, dict) else str(crash),
                "final": True, "finished": finished, "executor": executor}
    return {**result, "seconds": round(wall_s, 1),
            "started_ts": round(started_ts, 1) if started_ts is not None else None,
            "cpu_s": round(cpu_s, 1), "finished": finished, "executor": executor}


# ----------------------------------------------------------------------------
# overview for the web Lab page
# ----------------------------------------------------------------------------
def _tail(path: Path, nbytes: int = 4000) -> str:
    """The last few kilobytes of a file, for showing a log's recent lines."""
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            return f.read().decode("utf-8", errors="replace")
    except OSError:
        return ""


TURN_GROWTH = 2.5   # elapsed time grows about as turn ** 2.5: late turns have many more cities and units


def _projected_total(r: dict):
    """Projected length in minutes of a running game from its round progress, or None if it is too early to tell."""
    if r.get("turn") and r.get("limit") and r["turn"] / r["limit"] > 0.1:
        return r["minutes"] * (r["limit"] / r["turn"]) ** TURN_GROWTH
    return None


def _game_left(r: dict, per_game: float) -> float:
    """Minutes a running game still needs: from its round progress when it reports rounds, otherwise from the median
    game length (never less than a tenth of it, since a slow game past the median is still running)."""
    total = _projected_total(r)
    if total is not None:
        return total - r["minutes"]
    return max(per_game - r["minutes"], 0.1 * per_game)


def _folders_now() -> list:
    """The queue's and done/'s files as they stand: (folder, name, modified, size) each."""
    out = []
    for folder in (QUEUE, DONE):
        for p in folder.glob("*.json"):
            try:
                st = p.stat()
            except FileNotFoundError:
                continue
            out.append((folder.name, p.name, st.st_mtime_ns, st.st_size))
    return sorted(out)


def _experiment_specs(tries: int = 5) -> list[tuple[dict, str]]:
    """Every experiment as (spec, "queued" or "complete"), the queue's first, then done/'s newest first, as one
    reading of the two folders. The dispatcher moves a finished experiment's file to done/ (a rename) whenever it
    finishes, and the Queue page replaces a queued one's: an experiment moved after its queue file was read and before
    done/ is listed would show twice, so a reading the folders changed under is taken again. Not merged by name
    instead: ``lab submit`` may queue an experiment under a finished one's name, and both are shown. A folder still
    changing after ``tries`` readings keeps the last, which at worst shows one experiment twice until the next poll."""
    specs: list = []
    for _ in range(tries):
        before = _folders_now()
        specs = []
        for p in sorted(QUEUE.glob("*.json")):
            try:
                specs.append((json.loads(_fs_read(p)), "queued"))
            except FileNotFoundError:
                continue        # moved to done/ since the listing
        done = []
        for p in DONE.glob("*.json"):
            try:
                done.append((p.stat().st_mtime, json.loads(_fs_read(p))))
            except FileNotFoundError:
                continue
        specs += [(spec, "complete") for _, spec in sorted(done, key=lambda d: d[0], reverse=True)]
        if _folders_now() == before:
            break
    return specs


#: Each results file's figures by its (mtime, size), so a page polled every few seconds reads a file once a change.
_FIGURES: dict = {}


def results_figures(name: str, kind: str = "lab") -> dict:
    """An experiment's results file in figures: the games it holds (``done``, each once, crash lines included), the
    crashed ones, the median minutes and turns of the finished ones and when the last finished. Read again only when
    the file changed."""
    path = results_path(name, kind)
    try:
        st = path.stat()
    except OSError:
        return {"done": 0, "games": set(), "crashed": [], "per_game": None, "median_turns": None, "finished": None}
    key = str(path)
    stamp = (st.st_mtime_ns, st.st_size)
    hit = _FIGURES.get(key)
    if hit is not None and hit[0] == stamp:
        return hit[1]
    res = load_results(name, kind)
    games = {r.get("i") for r in res if isinstance(r.get("i"), int)}
    mins = [r["seconds"] / 60 for r in res if not r.get("crash") and r.get("seconds")]
    turns = [r["turns"] for r in res if not r.get("crash") and r.get("turns")]
    figures = {"done": len(games), "games": games,
               "crashed": sorted({r["i"] for r in res if r.get("crash") and isinstance(r.get("i"), int)}),
               "per_game": statistics.median(mins) if mins else None,
               "median_turns": statistics.median(turns) if turns else None,
               "finished": max((r.get("finished") or "" for r in res), default=None) or None}
    if len(_FIGURES) > 2000:
        _FIGURES.clear()
    _FIGURES[key] = (stamp, figures)
    return figures


def overview() -> dict:
    """Everything the web Lab page shows: the dispatcher's health and its executors, per-experiment progress and
    ETA, the games playing with their round, recent log lines and the side runs (citar.bench jobs writing
    saves/lab/*.out). The live part is the dispatcher's (``citar.jobs.dispatcher.view``): a process with no dispatcher
    (a command, a test) shows the files alone, nothing playing."""
    _dirs()
    from .jobs import dispatcher as jobs_dispatcher
    live = jobs_dispatcher.view()
    disp = live.get("dispatcher") or {}
    threads = max(1, sum(int(e.get("threads") or 0) for e in disp.get("executors") or [] if e.get("on")))
    running = []
    for r in live.get("running") or []:
        running.append({**r, "minutes": round(r.get("minutes") or 0.0, 1)})
    exps_live = live.get("experiments") or {}

    experiments = []
    remaining_minutes = 0.0
    for r in running:
        total = _projected_total(r)
        r["left_minutes"] = round(total - r["minutes"], 1) if total is not None else None
    for exp, state in _experiment_specs():
        kind = kind_of(exp)
        fig = results_figures(exp["name"], kind)
        info = exps_live.get(exp["name"], {}) if state == "queued" else {}
        done = fig["done"]
        per_game = fig["per_game"]
        mine = [r for r in running if r["exp"] == exp["name"]]
        failing = sorted(set(info.get("failed") or []) - fig["games"])
        why = info.get("refused") or ""
        if why:
            state, left = "refused", 0.0
        elif state == "queued":
            if per_game is None:
                # no finished game yet: project the running ones from their round progress
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
            "failing": failing, "crashed": fig["crashed"], "priority": exp.get("priority", 0),
            "note": exp.get("note") or exp.get("description"),
            "kind": "reference" if kind == "reference" else "factorial" if exp.get("factors") else "seats",
            "job_kind": job_kind(exp),
            "factors": list((exp.get("factors") or {}).keys()),
            "seats": [s.get("label") for s in exp.get("seats", [])] if not exp.get("factors") else [],
            "turns": exp.get("turns") or None, "median_turns": fig["median_turns"],
            "minutes_per_game": round(per_game, 1) if per_game else None, "remaining_minutes": round(left, 1),
            "submitted": exp.get("submitted"), "finished": fig["finished"],
            "refused": why or None, "waiting": info.get("waiting") or None,
            # each executor's reason, for the routes to name each one as the viewer may be told it
            "waiting_on": info.get("waiting_on") or [],
        })
    for exp in experiments:
        if exp["state"] in ("queued", "running"):
            exp["eta_minutes"] = None     # filled in below by simulating the queue order
    # the dispatcher fills free threads in priority order, so estimate each experiment's finish as the cumulative work
    # ahead of it divided by the threads
    order = sorted([e for e in experiments if e["state"] in ("queued", "running")], key=lambda e: -e["priority"])
    acc = 0.0
    for e in order:
        acc += e["remaining_minutes"]
        e["eta_minutes"] = round(acc / threads, 1)

    side = []
    for out, out_st in _fs_newest(LAB.glob("*.out")):
        if out.name == "runner.out":
            continue
        text = _tail(out, 6000)
        lines = [ln for ln in text.splitlines() if ln.strip()]
        age = time.time() - out_st.st_mtime
        side.append({"name": out.stem, "modified": datetime.fromtimestamp(out_st.st_mtime).isoformat(timespec="seconds"),
                     "active": age < 900 and not any("Report written" in ln or "stopping" in ln for ln in lines[-6:]),
                     "tail": lines[-12:]})

    return {
        "now": datetime.now().isoformat(timespec="seconds"),
        "dispatcher": {**disp, "paused": paused()},
        "remaining_minutes": round(remaining_minutes, 1), "eta_minutes": round(remaining_minutes / threads, 1),
        "running": sorted(running, key=lambda r: (r["exp"], r["i"])), "experiments": experiments,
        "log": _tail(LAB / "lab.log", 6000).splitlines()[-40:], "side_runs": side,
    }


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
           "crashed": sum(1 for r in results if r.get("crash")),
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
          f" | bot errors {s['bot_errors']}" + (f" | crashed {s['crashed']}" if s.get("crashed") else ""))
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
            exp = json.loads(_fs_read(DONE / f"{name}.json"))      # replaced when a re-queued name finishes
        if exp and kind_of(exp) == "reference":
            res = load_results(name, "reference")
            crashed = sum(1 for r in res if r.get("crash"))
            print(f"=== {name} (reference): {len(res) - crashed} games of {exp['games']}"
                  + (f" | crashed {crashed}" if crashed else "")
                  + f" | the baseline's lines: python scripts/refcheck/summarize.py {results_path(name, 'reference')}")
            continue
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
    """Print what is queued and playing, from the server's database (the dispatcher's jobs), and the pause."""
    p = paused()
    if p is not None:
        print(f"PAUSED since {p.get('at')} by {p.get('by')}: running games finish, nothing new starts "
              "(citar lab resume)")
    from .jobs import store
    status = store.offline_status()
    if status is None:
        print("no server database here yet: the lab plays inside the CITAR server, which keeps its jobs there")
    for name, n in status.get("experiments", {}).items() if status else ():
        print(f"  {name}: " + ", ".join(f"{k} {v}" for k, v in sorted(n.items())))
    for r in (status or {}).get("running", []):
        print(f"  playing {r['ref']} on {r['machine']} since {r['started_at']}")
    for exp in load_queue():
        fig = results_figures(exp["name"], kind_of(exp))
        print(f"queued {exp['name']} ({kind_of(exp)}): {fig['done']}/{exp['games']} finished, priority "
              f"{exp.get('priority', 0)}")


#: What ``citar lab run`` says now.
RETIRED = ("citar lab run: the lab now runs inside the CITAR server (local or server mode): start the server and it "
           "plays the queued experiments under the host machine's CPU settings (Servers page, Machine settings). "
           "Queue experiments with `citar lab submit`, pause and resume with `citar lab stop` and `resume`.")


def _need_engine(cmd: str):
    """Exit 2, saying why, unless the engine loads: ``submit`` checks every game as a job, and without the engine it
    would end in a traceback."""
    try:
        from . import engine_api
        engine_api.build_info()
    except ImportError as e:
        print(f"citar lab {cmd}: the engine does not load: {e}", file=sys.stderr)
        raise SystemExit(2) from None


def _submit_one(path, spec: dict, owner: Optional[str]) -> None:
    """``submit`` of one experiment: refused (exit 2, one line) when a job would refuse a game, or when a ranking or
    reference run's owner is not an administrator; registered as an owned item where the server's database is."""
    from . import items
    kind = kind_of(spec)
    collection = kind in COLLECTION_KINDS
    if collection:
        why = items.offline_admin_refusal(owner)
        if why:
            print(f"citar lab submit: {path}: a {kind} experiment is an administrator's: {why}", file=sys.stderr)
            raise SystemExit(2)
    try:
        s = submit(spec)
    except ValueError as e:      # refused, saying why: a game a job would refuse, a profile, a name
        print(f"citar lab submit: {path}: {e}", file=sys.stderr)
        raise SystemExit(2) from None
    if kind == "reference":
        print(f"queued {s['name']} (reference): {s['games']} games of the baseline, {s['size']} maps")
    else:
        print(f"queued {s['name']}{f' ({kind})' if kind != 'lab' else ''}: {s['games']} games, "
              f"seats {[x['label'] + '=' + x['bot'] for x in s['seats']]}")
    # an owned item (design/phase3-features.md P3.2.4), registered when the server's database is here
    print(f"  {items.register_offline('lab_experiment', s['name'], s['name'], QUEUE / (s['name'] + '.json'), owner, collection=collection)}")


def main(argv=None):
    """The ``citar lab`` command line."""
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("run", help="retired: the lab runs inside the CITAR server")
    s = sub.add_parser("submit")
    s.add_argument("specs", nargs="+")
    s.add_argument("--owner", default=None, help="the account the experiments belong to (default: the first admin)")
    rep = sub.add_parser("report")
    rep.add_argument("names", nargs="+")
    sub.add_parser("status")
    sub.add_parser("stop", help="pause: running games finish, nothing new starts")
    sub.add_parser("resume", help="start new games again after a stop")
    args, extra = ap.parse_known_args(argv)
    if args.cmd == "run":
        print(RETIRED, file=sys.stderr)
        raise SystemExit(2)
    if extra:
        ap.error(f"unrecognized arguments: {' '.join(extra)}")
    if args.cmd == "submit":
        _need_engine(args.cmd)
        for path in args.specs:
            data = json.loads(Path(path).read_text(encoding="utf-8"))
            for spec in (data if isinstance(data, list) else [data]):
                _submit_one(path, spec, args.owner)
    elif args.cmd == "report":
        cmd_report(args.names)
    elif args.cmd == "status":
        cmd_status()
    elif args.cmd == "stop":
        pause("citar lab stop")
        print("paused: running games finish and nothing new starts until `citar lab resume`")
    elif args.cmd == "resume":
        print("resumed" if resume() else "the lab was not paused")


if __name__ == "__main__":
    main()
