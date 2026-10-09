"""What ``citar.engine_api`` hands out: every name of the facade over the extension ``citar._engine``.

Every name of the facade, with the shapes its docstrings promise, over the Rust engine's bindings
(crates/citar-py; crates/citar-engine/DESIGN.md P2.6). Only ``citar.engine_api`` imports this module, and only this
module imports the extension (tests/test_engine_boundary.py). What this layer does, and the binding does not:

- **Values.** The binding hands back every dict or list as JSON bytes; this decodes them, and turns the keys JSON
  could only write as text (``standings``, ``state_summary``'s ``names`` and ``scores``, a drive's actions) back into
  the player ids the facade promises.
- **Events.** Every call that changes the game returns the events it appended; this hands each to the game's
  subscribers, in order, after the call returns and on the calling thread. (The Python engine called its listeners
  during the call: the one documented change of timing.) A subscriber that raises is logged and the next is still
  called, as the Python engine ignored one.
- **Seeds and files.** The engine has no randomness of its own and no I/O (DESIGN.md 8.1): a game, a run or a
  generated map without a seed gets one drawn here, ``random.randrange(1, 2**31)`` as Python drew it; an editor map
  named by id is read from ``saves/maps`` and given to the engine inline; maps and scenarios are read and written
  here, in the files Python wrote (``maps.py`` and ``scenario.py``'s storage functions, whose layout this keeps).
- **Bots** are handles (``citar._engine.Bot``): compiled versions with parameters, never Python objects. A bot seat's
  turn is a one-seat drive; its answer to a negotiation is ``Game.answer``.
- **Saves** are the Rust engine's own (DESIGN.md P2.5): a ``.citar`` container (a small JSON header, the session's
  record, its metrics and the state, zstd) beside the journal that holds the game's history, written by
  ``EngineGame.save_snapshot`` and its ``write`` and read by ``read_save`` and ``from_save``; ``save_header`` reads a
  header alone, which is what listing saves costs. ``to_save`` is the game as one value, ``{"state": <the state>,
  "journal": <the whole history as one journal chunk, base64>}``, which ``from_save`` also reads. A save or scenario
  the Python engine wrote does not load (``LoadError``, a ValueError).

The other documented change: ``apply_ops`` is all or nothing (``atomic-apply-ops``), where Python left the operations
before a failing one applied.
"""
from __future__ import annotations

import base64
import datetime
import gzip
import json
import logging
import random
import re
import time
from pathlib import Path
from typing import Any, Callable, Optional

from . import _engine as _E
from . import paths
from .fsutil import replace as _fs_replace

# citar/engine_api.py's list, which its import holds this module to.
__all__ = [
    "ActionError", "MapError", "EngineCrash", "RULES_OVERVIEW", "MAP_LEGEND",
    "rules_version", "rules_client", "max_players", "map_sizes", "map_types", "speeds", "difficulties",
    "resolve_name", "ruleset_counts", "tool_list", "tool_kind", "state_summary",
    "list_maps", "load_map", "save_map", "delete_map", "validate_map", "map_summary", "blank_map", "generate_map",
    "scenario_ops_help", "list_scenarios", "load_scenario", "scenario_summary", "delete_scenario",
    "bot_instance", "bot_versions", "bot_schema", "bot_clean_params", "bot_fingerprint", "DIPLOMACY_CATEGORIES",
    "item_category", "proposal_categories", "bot_set_diplomacy", "bot_owns_negotiation", "run_game",
    "build_info",
    "DEBUG_ACTIONS", "EngineGame",
    "open_journal", "fork_journal", "journal_in_use", "read_save", "save_header",
]

_log = logging.getLogger("citar.engine")

#: A refusal the caller can fix; ``.code`` names its kind (``not_your_turn``, ``bad_param``, ...).
ActionError = _E.ActionError
#: A map document that cannot be a map, or a map the generator cannot make (a ValueError).
MapError = _E.MapError
#: The engine stopped after an internal error (a RuntimeError, never an ActionError).
EngineCrash = _E.EngineCrash

RULES_OVERVIEW: str = _E.RULES_OVERVIEW
MAP_LEGEND: str = _E.MAP_LEGEND
#: The kinds of diplomacy a hybrid seat hands to its bot or its language model, one by one
DIPLOMACY_CATEGORIES: tuple = tuple(_E.DIPLOMACY_CATEGORIES)
DEBUG_ACTIONS: tuple = tuple(_E.DEBUG_ACTIONS)

MAP_DIR = paths.saves_path("maps")
SCENARIO_DIR = paths.saves_path("scenarios")
#: A map's or a scenario's id: a slug, never a path (it arrives from a URL).
_ID = re.compile(r"[a-z0-9][a-z0-9-]{0,79}")


def _dumps(v: Any) -> bytes:
    """A value as the JSON bytes the binding reads."""
    return json.dumps(v, separators=(",", ":")).encode()


def _loads(b: Optional[bytes]) -> Any:
    """JSON bytes from the binding, decoded; None stays None."""
    return None if b is None else json.loads(b)


def _int_keys(d: dict) -> dict:
    """A dict whose keys JSON wrote as text, keyed by the ints they were."""
    return {int(k): v for k, v in d.items()}


def _action_error(message: str, code: str = "bad_param") -> Exception:
    """An ActionError made on this side of the binding, with its code as the engine's carry one."""
    e = ActionError(message)
    e.code = code
    return e


def _seed(v) -> int:
    """A seed as given, or one drawn as Python drew it."""
    return random.randrange(1, 2**31) if v is None else v


def _slug(name: str, fallback: str) -> str:
    """A filesystem-safe identifier from a name (maps.py and scenario.py's slug)."""
    s = re.sub(r"[^a-z0-9]+", "-", (name or "").lower()).strip("-")
    return s[:60] or f"{fallback}-{int(time.time())}"


def _as_map_error(fn: Callable, *args):
    """Calls a map function of the binding, any refusal raised as MapError, as the facade promises for maps: the
    binding refuses settings that name nothing (an unknown map type, a count that is not one) with ValueError."""
    try:
        return fn(*args)
    except MapError:
        raise
    except ValueError as e:
        raise MapError(str(e)) from None


# ----------------------------------------------------------------------------
# Rules, tools and constants (the process's ruleset)
# ----------------------------------------------------------------------------
def rules_version() -> str:
    """The ruleset's version, recorded in every save: the format and the start of its id ("2-0123456789ab")."""
    return _E.rules_version()


def rules_client() -> dict:
    """The whole ruleset as the browser and the agents read it."""
    return json.loads(_E.rules_client())


def max_players() -> int:
    """The most seats a game can have."""
    return _E.max_players()


def map_sizes() -> dict:
    """Map size name -> {"name", "width", "height", "players", "city_states"}."""
    return json.loads(_E.map_sizes())


def map_types() -> list[str]:
    """The map generator's map types."""
    return list(_E.map_types())


def speeds() -> list[str]:
    """The game speeds, by name."""
    return list(_E.speeds())


def difficulties() -> list[str]:
    """The difficulty levels, easiest first."""
    return list(_E.difficulties())


def resolve_name(kind: str, name) -> Optional[str]:
    """A ruleset name as the ruleset spells it (e.g. "quick" -> "Quick" for kind "speed"), or None if unknown.
    Raises ValueError for a kind that is no table of the ruleset."""
    return None if name is None else _E.resolve_name(kind, str(name))


def ruleset_counts() -> dict:
    """How much the loaded ruleset holds, by kind (techs, units, buildings, nations, policies)."""
    return json.loads(_E.ruleset_counts())


def tool_list(kind: Optional[str] = None) -> list[dict]:
    """Every player tool with its JSON schema ({"name", "description", "input_schema", "kind", "any_time",
    "category"}), optionally only the ``query`` or ``action`` ones."""
    return json.loads(_E.tool_list(kind))


def tool_kind(name: str) -> Optional[str]:
    """Whether a tool is an "action" or a "query"; None for a name that is no tool."""
    return _E.tool_kind(name) if isinstance(name, str) else None


def state_summary(state: dict) -> dict:
    """The headline facts of a saved game state (a save's or a scenario's "state"), without loading it:
    {"turn", "phase", "turn_limit", "map_size", "map_type", "winner" (a name), "winner_id", "names" ({player id:
    name}, every civilization), "majors" ([{"id", "name", "nation", "alive"}]), "scores" ({major id: score in the
    last per-turn stats row}; empty before the first row is recorded)}. Raises LoadError (a ValueError) for a state
    the Rust engine did not write."""
    s = json.loads(_E.state_summary(_dumps(state)))
    s["names"] = _int_keys(s.get("names") or {})
    s["scores"] = _int_keys(s.get("scores") or {})
    return s


# ----------------------------------------------------------------------------
# Maps and scenarios on disk (the file I/O is here; the engine builds, checks and reads the values)
# ----------------------------------------------------------------------------
def _map_path(map_id: str) -> Path:
    """The file for a map id, rejecting anything that is not a plain slug."""
    if not isinstance(map_id, str) or not _ID.fullmatch(map_id):
        raise MapError(f"Invalid map id '{map_id}'.")
    return MAP_DIR / f"{map_id}.json"


def list_maps() -> list[dict]:
    """Saved maps, in summary, the newest first. A file that does not read as a map is left out."""
    MAP_DIR.mkdir(parents=True, exist_ok=True)
    out = []
    for p in sorted(MAP_DIR.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            out.append(map_summary(json.loads(p.read_text(encoding="utf-8"))))
        except (OSError, ValueError, KeyError):
            continue
    return out


def load_map(map_id: str) -> dict:
    """One saved map. Raises MapError."""
    p = _map_path(map_id)
    if not p.exists():
        raise MapError(f"No map '{map_id}'.")
    return json.loads(p.read_text(encoding="utf-8"))


def save_map(data: dict) -> tuple[dict, list[str]]:
    """Validate and save a map; returns (the map as saved, the problems that were fixed). Raises MapError."""
    clean, warnings = validate_map(data)
    clean["id"] = _slug(clean.get("id") or clean.get("name"), "map")
    p = _map_path(clean["id"])
    now = time.strftime("%Y-%m-%dT%H:%M:%S")
    clean["modified"] = now
    if p.exists():
        try:
            clean.setdefault("created", json.loads(p.read_text(encoding="utf-8")).get("created", now))
        except (OSError, ValueError):
            clean.setdefault("created", now)
    else:
        clean.setdefault("created", now)
    MAP_DIR.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(clean, separators=(",", ":")), encoding="utf-8")
    _fs_replace(tmp, p)
    return clean, warnings


def delete_map(map_id: str):
    """Delete a saved map. Raises MapError for an invalid id."""
    p = _map_path(map_id)
    if p.exists():
        p.unlink()


def validate_map(data: dict) -> tuple[dict, list[str]]:
    """Check a map without saving it: (the map with its problems fixed, what was fixed). Raises MapError."""
    clean, fixed = _as_map_error(_E.validate_map, _dumps(data))
    return json.loads(clean), list(fixed)


def map_summary(data: dict) -> dict:
    """A map's headline facts, for lists. Raises MapError for a document with no width, height or tiles."""
    return json.loads(_as_map_error(_E.map_summary, _dumps(data)))


def blank_map(width: int, height: int, terrain: str, name: str = "") -> dict:
    """An empty map of one base terrain, for the editor to start from. Raises MapError."""
    return json.loads(_as_map_error(_E.blank_map, width, height, terrain, name or ""))


def generate_map(width: int, height: int, map_type: str, players: int, city_states: int, seed: Optional[int] = None,
                 ruins: bool = True, name: str = "", options: Optional[dict] = None) -> dict:
    """A map from the random generator, for the editor to start from; ``options`` are the lobby's map settings
    (``map_edges``, ``river_density``, ``resources``). A seed is drawn when none is given. Raises MapError."""
    settings = dict(options or {})
    settings.update({"width": width, "height": height, "map_type": map_type, "players": players,
                     "city_states": city_states, "ruins": bool(ruins), "name": name or ""})
    return json.loads(_as_map_error(_E.generate_map, _seed(seed), _dumps(settings)))


def scenario_ops_help() -> list[dict]:
    """Every scenario edit operation with its parameters."""
    return json.loads(_E.scenario_ops_help())


def _scenario_path(sid: str) -> Path:
    """The file for a scenario id, rejecting anything that is not a plain slug: the id arrives from a URL, and a path
    separator here would be a way out of the scenario directory."""
    if not isinstance(sid, str) or not _ID.fullmatch(sid):
        raise _action_error(f"Invalid scenario id '{sid}'.")
    return SCENARIO_DIR / f"{sid}.citarscn"


def list_scenarios() -> list[dict]:
    """Saved scenarios, in summary, the newest first. A file that does not read as a scenario is left out."""
    SCENARIO_DIR.mkdir(parents=True, exist_ok=True)
    out = []
    for p in sorted(SCENARIO_DIR.glob("*.citarscn"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            with gzip.open(p, "rt", encoding="utf-8") as f:
                out.append(scenario_summary(json.load(f)))
        except (OSError, ValueError, KeyError, ActionError):
            continue
    return out


def load_scenario(sid: str) -> dict:
    """A saved scenario: {"id", "name", "description", "seats", "state", ...}. Raises ActionError."""
    p = _scenario_path(sid)
    if not p.exists():
        raise _action_error(f"No scenario '{sid}'.")
    with gzip.open(p, "rt", encoding="utf-8") as f:
        return json.load(f)


def scenario_summary(data: dict) -> dict:
    """A scenario's headline facts, for lists. Raises ActionError for a document that is no scenario."""
    return json.loads(_E.scenario_summary(_dumps(data)))


def delete_scenario(sid: str):
    """Delete a saved scenario. Raises ActionError for an invalid id."""
    p = _scenario_path(sid)
    if p.exists():
        p.unlink()


# ----------------------------------------------------------------------------
# Bots: compiled versions with parameters, as handles
# ----------------------------------------------------------------------------
def bot_instance(engine: str = "basic", *, aggression: float = 0.4, params: Optional[dict] = None,
                 fixed_aggression: Optional[float] = None):
    """A bot to seat in a game: ``engine`` is a bot version, "basic" (the latest), "basic-N" or "idle".

    The bot is an opaque handle (``citar._engine.Bot``): give it to :meth:`EngineGame.play_bot_turn`,
    :meth:`EngineGame.drive` or :func:`run_game`. ``params`` are overrides of the version's parameters, cleaned
    against its schema (unknown names and fractional ints refused). ``aggression`` is the seat's; a profile that fixes
    its own gives ``fixed_aggression``, which wins and is what :func:`bot_fingerprint` hashes. A bot has no seed of
    its own: it draws from the game's seed (DESIGN.md P2.3.5). Raises ValueError for a name that is no version (the
    snapshots of the Python bot were archived with 0.1.5) or parameters that do not clean.
    """
    if not isinstance(engine, str) or not engine:
        raise ValueError(f"'{engine}' is not a bot engine (basic, basic-N or idle).")
    a = None if aggression is None else float(aggression)
    fixed = None if fixed_aggression is None else float(fixed_aggression)
    return _E.Bot(engine, None if params is None else _dumps(params), a, fixed)


def _bot(bot):
    """A compiled bot's handle, or TypeError: only compiled bots run on Rust."""
    if not isinstance(bot, _E.Bot):
        raise TypeError(f"Only compiled bots run on Rust: {type(bot).__name__} is not a bot from bot_instance.")
    return bot


def item_category(item: dict) -> str:
    """The diplomacy category a deal item belongs to (gold -> "trades", peace_treaty -> "peace", ...). Raises
    ValueError for an item that does not read as one."""
    return _E.item_category(_dumps(item))


def proposal_categories(proposal: Optional[dict]) -> set:
    """The diplomacy categories a proposal touches; empty for none (a negotiation that is only talk)."""
    return set(_E.proposal_categories(_dumps(proposal)))


def bot_set_diplomacy(bot, owners: Optional[dict]):
    """Say who decides each diplomacy category for a bot: {category: "bot" | "llm"}, "bot" for any not named. A
    category the language model owns is one the bot leaves alone. Changes the handle in place: a seat holding it
    follows from its next drive. Raises ValueError for an unknown category or owner."""
    _bot(bot).set_diplomacy(None if owners is None else _dumps(owners))


def bot_owns_negotiation(bot, negotiation: dict) -> bool:
    """Whether a bot answers this negotiation itself rather than the language model its seat hands that kind of
    diplomacy to. ``negotiation`` is a record from :meth:`EngineGame.negotiation`."""
    return bool(_bot(bot).owns_negotiation(_dumps(negotiation)))


def _run_config(config: dict) -> dict:
    """A game's settings as the engine takes them: a seed drawn if none is given, an editor map named by id read
    from disk and given inline (game.py:156-170 did both inside Game.new)."""
    cfg = {k: v for k, v in (config or {}).items() if v is not None}
    cfg["seed"] = _seed(cfg.get("seed"))
    m = cfg.get("map")
    if m and not isinstance(m, dict):
        cfg["map"] = load_map(str(m))
    return cfg


def run_game(spec: dict, on_turn: Optional[Callable[[dict], None]] = None,
             on_event: Optional[Callable[[dict], None]] = None) -> dict:
    """Play a whole headless game with a bot in every seat and return how it went: the citar-sim runner.

    ``spec``:
      - ``config``: the game configuration, as for :meth:`EngineGame.new` (a seed is drawn when none is given);
      - ``bots``: {player id: a bot from bot_instance or citar.bots.profiles.make_bot}; a major with none passes;
      - ``traceback_limit`` (5) frames per crash; ``max_errors`` is ignored (a Rust bot does not raise: a crash ends
        the game);
      - ``labels``: {player id: text} added to each crash line after the player;
      - ``raise_errors`` (False): raise a crash as EngineCrash instead of recording it, for runs whose point is that
        the bot does not crash (``citar sim`` and its test).

    ``on_turn(info)`` is called when a turn begins and once more if the game ended on a new turn, with {"turn",
    "phase", "turn_limit", "last_stats"}; an exception from it ends the run and is raised. ``on_event(event)`` gets
    every event after the game is created, each step's before that step's ``on_turn``; it is a listener, as it was
    on the Python engine: an Exception it raises is reported (``sys.unraisablehook``) and the next event is still
    delivered.

    Returns {"turn", "turns" (played), "phase", "winner", "victory", "turn_limit", "stats" (a row per turn),
    "players" (every civ; majors add techs, future_techs, policies, religion, great_people, cities, spaceship,
    difficulty and score, 0 once eliminated), "errors" (a line and a traceback per crash)}.
    """
    run = {k: v for k, v in spec.items() if k != "bots"}
    run["config"] = _run_config(spec["config"])
    bots = {int(pid): _bot(b) for pid, b in (spec.get("bots") or {}).items() if b is not None}
    return json.loads(_E.run_game(_dumps(run), bots, on_turn, on_event))


# ----------------------------------------------------------------------------
# Phase 2's names: the build and the bot versions
# ----------------------------------------------------------------------------
def build_info() -> dict:
    """What this build is: {"version", "build_id" (12 hex digits over the engine's and the bot's code and the
    ruleset), "label" (the git describe it was built from, or "unknown"), "rules", "engine_code", "bot_code"}."""
    return json.loads(_E.build_info())


def bot_versions() -> list[dict]:
    """Every bot version compiled in, the latest first: [{"id", "label", "description", "latest", "memory_kind"}]."""
    return json.loads(_E.bot_versions())


def bot_schema(version: str = "basic") -> dict:
    """A version's parameter schema in the Bots page's shape, {"engine", "groups"}; "basic" names the latest. Raises
    ValueError for an unknown version."""
    return json.loads(_E.bot_schema(version))


def bot_clean_params(version: str, params: Optional[dict] = None) -> dict:
    """Parameter overrides cleaned against a version's schema: unknown names refused, values coerced to their
    parameter's type, defaults dropped, keys sorted. Raises ValueError for overrides that do not clean."""
    return json.loads(_E.bot_clean_params(version, None if params is None else _dumps(params)))


def bot_fingerprint(bot) -> str:
    """What a bot plays, hashed: 12 hex digits over the build id, its version, its overrides and the profile's fixed
    aggression ("seat" when the seat decides; DESIGN.md P2.8.6)."""
    return _E.bot_fingerprint(_bot(bot))


# ----------------------------------------------------------------------------
# Saves v2 (package 2-11, DESIGN.md P2.5): the container and the journal
# ----------------------------------------------------------------------------
def _listing(summary: dict) -> dict:
    """A header's summary with the player ids state_summary gives as ints."""
    summary["names"] = _int_keys(summary.get("names") or {})
    summary["scores"] = _int_keys(summary.get("scores") or {})
    return summary


def _header(raw: bytes) -> dict:
    """A container's header, its summary's ids as ints."""
    h = json.loads(raw)
    _listing(h["summary"])
    return h


class Journal:
    """One journal of a session's timeline (DESIGN.md P2.5.3), open for appending: it holds the file's OS lock until
    :meth:`close`, so a second session opening it is refused, and the chunks of history the game's snapshots took
    that are not on the disk yet (``pending``), which the next save appends first. Made by :func:`open_journal`."""

    def __init__(self, raw):
        self._j = raw

    @property
    def path(self) -> Path:
        return Path(self._j.path)

    @property
    def records(self) -> int:
        """The records on the disk."""
        return self._j.records

    @property
    def pending(self) -> int:
        """The chunks taken from the game and not yet appended (a failed or unwritten save leaves them here)."""
        return self._j.pending

    @property
    def is_closed(self) -> bool:
        return self._j.is_closed

    def reference(self) -> dict:
        """The prefix on the disk, {"file", "records", "bytes", "head"}: what a container names. Waits for a write in
        progress; OSError once closed."""
        return json.loads(self._j.reference())

    def truncate_to(self, ref: dict):
        """Cut the journal back to ``ref``, one of its own prefixes (a save's), dropping the records past it: the
        chunks of a save that never completed. LoadError (a ValueError) for another timeline's prefix or a corrupt
        journal; OSError when the disk refuses, after which the cut is made again before the next append."""
        self._j.truncate_to(_dumps(ref))

    def close(self) -> int:
        """Let go of the file and its lock, after the write in progress: the chunks still pending, which no save
        names. Closing twice is nothing."""
        return self._j.close()

    def _hooks(self, fail_appends: int = 0, stop_before_container: int = 0):
        """For the tests (a build with the test operations): the next ``fail_appends`` saves fail at their first
        append, the next ``stop_before_container`` stop once their chunks are on the disk, before the container."""
        self._j._hooks(fail_appends, stop_before_container)

    def __repr__(self) -> str:
        return repr(self._j)


def open_journal(path) -> tuple[Journal, dict]:
    """Open (or create) the journal at ``path`` for appending, taking its OS lock. Returns the journal and what opening
    found, {"records", "bytes", "torn" (the length of an incomplete tail it cut off: a crash mid-append), "corrupt_at"
    (where a damaged record starts: nothing was cut, appends are refused, and the session forks from the good prefix),
    "reference" (the good prefix)}. Raises LoadError (a ValueError) when another session holds it or it is no
    journal."""
    raw, found = _E.Journal.open(str(path))
    return Journal(raw), json.loads(found)


def fork_journal(path, ref: dict, new_path):
    """Copy the prefix ``ref`` of the journal at ``path`` to a new journal at ``new_path``, which must not exist, and
    sync it: a new timeline from a save. LoadError when the prefix cannot be read (a session holds the journal, it is
    damaged inside the prefix, or of another timeline); OSError when the copy cannot be written (FileExistsError's
    case included)."""
    _E.fork_journal(str(path), _dumps(ref), str(new_path))


def journal_in_use(path) -> bool:
    """Whether a session (of this process or another) holds the journal at ``path`` now."""
    return _E.journal_in_use(str(path))


class SaveFile:
    """A ``.citar`` container read back (:func:`read_save`): ``header`` (as :func:`save_header` gives it), ``journal``
    (the prefix of its journal it names, or None), the session's record and metrics (``session``, ``metrics``), and
    the state, which :meth:`EngineGame.from_save` loads."""

    def __init__(self, raw):
        self._raw = raw
        self.path = Path(raw.path)
        self.header = _header(raw.header())
        self._session: Optional[dict] = None
        self._metrics: Optional[dict] = None

    @property
    def journal(self) -> Optional[dict]:
        return self.header["journal"]

    @property
    def summary(self) -> dict:
        """The state's headline facts, as state_summary gives them, and "seats" (each seat's type, by seat)."""
        return self.header["summary"]

    @property
    def session(self) -> dict:
        if self._session is None:
            self._session = json.loads(self._raw.session())
        return self._session

    @property
    def metrics(self) -> dict:
        if self._metrics is None:
            self._metrics = json.loads(self._raw.metrics())
        return self._metrics

    def __repr__(self) -> str:
        return f"<SaveFile {self.path.name} turn {self.summary.get('turn')}>"


def read_save(path) -> SaveFile:
    """Read the save at ``path``: its header and its body, decompressed and checked. Raises LoadError (a ValueError)
    for a file that is damaged or no save of this version; a save of the Python engine (version 1) says so: "saved by
    the Python engine; archived with 0.1.5"."""
    return SaveFile(_E.read_save(str(path)))


def save_header(path) -> dict:
    """The header of the save at ``path``, without reading its body (what listing saves costs: never the state):
    {"format", "version", "saved_at", "engine_build", "rules", "summary" (state_summary's keys, and "seats": each
    seat's type), "session" ({"id", "name", "benchmark"}), "journal" ({"file", "records", "bytes", "head"} or
    None)}. Raises LoadError as read_save."""
    return _header(_E.save_header(str(path)))


class SaveSnapshot:
    """A game taken for a save under the session's lock (:meth:`EngineGame.save_snapshot`): a copy of its state, its
    history since the last save queued in the journal, and the time it was taken. :meth:`write` writes it off the
    lock."""

    def __init__(self, raw):
        self._s = raw
        self.saved_at = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="milliseconds")

    @property
    def turn(self) -> int:
        return self._s.turn

    @property
    def records(self) -> int:
        """The chunks of history its state counts: the journal records its container names."""
        return self._s.records

    def write(self, path, journal: Journal, session, metrics):
        """Write the save to ``path`` beside ``journal`` (the GIL released): the journal's pending chunks up to this
        snapshot appended and synced, then the container naming them, through a temporary file and a rename.
        ``session`` and ``metrics`` are the session's record and metrics, as dicts or JSON bytes; the record's "id",
        "name" and "benchmark" name the save's session in its header, and its seats' types go into the header's summary.
        Raises OSError when the save was not written: its chunks stay pending for the next save, and the old file
        stays."""
        session = session if isinstance(session, bytes) else _dumps(session)
        metrics = metrics if isinstance(metrics, bytes) else _dumps(metrics)
        self._s.write(str(path), journal._j, session, metrics, self.saved_at)

    def __repr__(self) -> str:
        return repr(self._s)


# ----------------------------------------------------------------------------
# One game
# ----------------------------------------------------------------------------
class EngineGame:
    """One game, behind the door. Build it with :meth:`new`, :meth:`from_save` or :meth:`from_state`."""

    def __init__(self, game):
        self._g = game
        self._subscribers: list[Callable[[dict], None]] = []

    def _fan(self, events: Optional[bytes]):
        """Hands a call's events to every subscriber, in order, after the call: the game's lock is free by now, so a
        subscriber may read the game."""
        if not self._subscribers or not events or events == b"[]":
            return
        for ev in json.loads(events):
            for fn in list(self._subscribers):
                try:
                    fn(ev)
                except Exception:
                    _log.error("A subscriber of the game raised on a %s event; the others still hear it.",
                               ev.get("type"), exc_info=True)

    # ------------------------------------------------------------------ creation and saving
    @classmethod
    def new(cls, config: dict) -> "EngineGame":
        """A new game from a lobby configuration (map, speed, difficulty, "players": [{"controller", "nation",
        "handicap", "auto", "difficulty", ...}]); a seed is drawn when none is given, and an editor map named by id
        ("map") is read from disk. Raises ValueError (MapError for a map) for a bad one."""
        return cls(_E.Game.new(_dumps(_run_config(config))))

    @classmethod
    def _loaded(cls, g, report: bytes, history: bool) -> "EngineGame":
        """A loaded game, with what the load found logged: a game another ruleset made, and a save whose history did not
        come back whole. A state alone (a scenario, an undo) has no history to miss."""
        r = json.loads(report)
        if r.get("rules_changed"):
            _log.warning("This game was made with another ruleset (%s, engine %s): it plays on with this one's.",
                         *r["rules_changed"])
        if history and r.get("chronicle_incomplete"):
            _log.warning("This game's history did not come back whole: its replay and statistics are incomplete.")
        return cls(g)

    @classmethod
    def _load(cls, state: dict, journal: Optional[str]) -> "EngineGame":
        """A game from a state and, for a value save (to_save), the journal chunk of its history."""
        g, report = _E.Game.load(_dumps(state), [base64.b64decode(journal)] if journal else [])
        return cls._loaded(g, report, bool(journal))

    @classmethod
    def from_save(cls, data, journal=None, history: bool = True) -> "EngineGame":
        """A game from a save: a ``.citar`` container (:func:`read_save`'s :class:`SaveFile`), or the game as one value
        ({"state", "journal"}, see to_save). A container's history is read from the journal its header names, beside
        it, or from ``journal`` (the path of a copy: a fork); with ``history=False`` it loads the state alone, with no
        history (a scenario's start). Reading a journal takes a shared lock, so a journal a session is writing cannot be
        read: stop that session first. Raises LoadError (a ValueError) for a save that does not load: one whose history
        is missing, damaged inside the records it names or of another timeline, which never loads with a history it was
        not saved with, and a save of the Python engine."""
        if isinstance(data, SaveFile):
            ref = data.journal if history else None
            path = None if ref is None else Path(journal) if journal is not None else data.path.parent / ref["file"]
            g, report = _E.Game.load_save(data._raw, None if path is None else str(path))
            return cls._loaded(g, report, path is not None)
        return cls._load(data["state"], data.get("journal"))

    @classmethod
    def from_state(cls, state: dict) -> "EngineGame":
        """A fresh, independent game from a state dict (a scenario's, a save's, or state_dict()'s), with no history.
        Raises LoadError (a ValueError)."""
        return cls._load(state, None)

    def to_save(self) -> dict:
        """The game as one value: {"state" (the state as state_dict gives it, its journal position counting the
        chunk), "journal" (the whole history as one journal chunk, base64 text, or None before there is any)}, built
        fresh under the game's lock; :meth:`from_save` reads it back. The game's own journal does not move. A session
        saves through :meth:`save_snapshot` and the container instead."""
        state, chunk = self._g.save()
        return {"state": json.loads(state), "journal": None if chunk is None else base64.b64encode(chunk).decode()}

    def save_snapshot(self, journal: Journal) -> SaveSnapshot:
        """Take the game for a save, under the session's lock (DESIGN.md P2.5.3): its history since the last save goes
        into ``journal``'s pending chunks, and the snapshot is a copy of the state, which :meth:`SaveSnapshot.write`
        writes off the lock with the container. A journal with no records and none pending is a new one: the game's
        journal starts over, its first chunk the whole history the game keeps. A crashed game is saved too, for
        whoever debugs it. Raises OSError for a closed journal and RuntimeError for a journal of another timeline."""
        return SaveSnapshot(self._g.save_snapshot(journal._j))

    def state_dict(self) -> dict:
        """An independent copy of the whole state (without its history), to undo to or to start a scenario from."""
        return json.loads(self._g.state_json())

    # ------------------------------------------------------------------ summary reads (Heads: never wait)
    @property
    def turn(self) -> int:
        """The current turn number."""
        return self._g.turn

    @property
    def current(self) -> int:
        """Whose turn it is, as a player id."""
        return self._g.current

    @property
    def phase(self) -> str:
        """The game's phase: "playing", or the phase a finished game is in."""
        return self._g.phase

    @property
    def winner(self) -> Optional[int]:
        """The winning player's id, once there is one."""
        return self._g.winner

    @property
    def victory(self) -> Optional[str]:
        """How the game was won, once it has been."""
        return self._g.victory

    @property
    def turn_limit(self) -> int:
        """The turn the game ends on: the configured limit, or the speed's own."""
        return self._g.turn_limit

    @property
    def config(self) -> dict:
        """The game's configuration, as Python's Game.new normalised it (a copy)."""
        return json.loads(self._g.config())

    def summary(self) -> dict:
        """The game at a glance: {"turn", "current", "phase", "winner", "victory", "turn_limit", "players"}, where
        each player is as :meth:`player` gives it."""
        return json.loads(self._g.summary())

    def player(self, pid: int) -> dict:
        """One civilization: {"id", "kind", "name", "color", "leader", "nation", "alive", "eliminated_turn",
        "controller", "handicap", "auto", "overrides" (the handicap/auto its seat set explicitly), "difficulty" (a
        major's falls back to the game's), "founded_city"}. Raises ValueError for a player the game lacks."""
        return json.loads(self._g.player(pid))

    def player_name(self, pid: int) -> str:
        """A civilization's name."""
        return self._g.player_name(pid)

    def is_alive(self, pid: int) -> bool:
        """Whether a civilization is still in the game."""
        return self._g.is_alive(pid)

    def majors(self, alive_only: bool = True) -> list[dict]:
        """The major civilizations (not city-states or barbarians), as :meth:`player` gives them."""
        return json.loads(self._g.majors(alive_only))

    def standing(self, pid: int) -> dict:
        """How a civilization is doing: {"score", "cities", "units", "population", "techs", "gold", "era"}. ``score`` is
        the score formula's total even for a civilization that has been eliminated."""
        return json.loads(self._g.standing(pid))

    def standings(self) -> dict:
        """{player id: standing} for every major civilization, eliminated ones included."""
        return _int_keys(json.loads(self._g.standings()))

    def stats(self, last: Optional[int] = None) -> list[dict]:
        """The per-turn statistics rows ({"turn", "players": {str(id): {...}}}), oldest first; the last ``last`` of
        them if given."""
        return json.loads(self._g.stats(last or None))

    # ------------------------------------------------------------------ tools and views
    def execute(self, pid: int, tool: str, args: Optional[dict] = None) -> Any:
        """Run a player tool, with every rule the tool registry enforces. Raises ActionError. The result is the tool's
        own return value."""
        result, events = self._g.execute(pid, tool, _dumps(args or {}))
        self._fan(events)
        return json.loads(result)

    def view(self, pid: Optional[int], event_limit: int = 150) -> dict:
        """The game as one player sees it (None: everything), which is what the browser renders from. The god view's
        ``empires`` is keyed by player id, as Python's was (JSON has it as text: view_json)."""
        v = json.loads(self._g.view_json(pid, event_limit))
        if isinstance(v.get("empires"), dict):
            v["empires"] = _int_keys(v["empires"])
        return v

    def view_json(self, pid: Optional[int], extra: Optional[dict] = None, event_limit: int = 150) -> bytes:
        """:meth:`view` as JSON bytes, with ``extra``'s keys (the server's ``seat``, ``session``, ``version``,
        ``spectator``) spliced in, for a route to return as they are: a gargantuan god view would otherwise cost a
        parse and a re-dump of megabytes per refresh. Raises ValueError for a key of the view's own."""
        return self._g.view_json(pid, event_limit, None if extra is None else _dumps(extra))

    def briefing(self, pid: int) -> str:
        """A language model's start-of-turn briefing."""
        return self._g.briefing(pid)

    def turn_progress(self, pid: int) -> str:
        """What is still unhandled this turn, as a short note."""
        return self._g.turn_progress(pid)

    def empire_summary(self, pid: int) -> dict:
        """The empire at a glance (get_empire's data), plus "cities" (how many), "at_war_with" (names) and "notes"
        (the civilization's notebook)."""
        return json.loads(self._g.empire_summary(pid))

    # ------------------------------------------------------------------ negotiations
    def negotiation(self, nid: int) -> dict:
        """A copy of one negotiation: {"id", "initiator", "responder", "status", "awaiting", "proposal",
        "proposal_by", "history", "turn", ...}. Raises ActionError for an unknown id."""
        return json.loads(self._g.negotiation(nid))

    def open_negotiations(self, pid: Optional[int] = None) -> list[dict]:
        """Copies of the negotiations still open, oldest first; only those ``pid`` is a party to, if given."""
        return json.loads(self._g.open_negotiations(pid))

    def negotiation_head(self, nid: int) -> dict:
        """Where one negotiation stands: {"id", "initiator", "responder", "status", "awaiting", "entries"}. An open one
        is read without waiting for the game. Raises ActionError for an unknown id."""
        return self._g.negotiation_head(nid)

    def open_negotiation_heads(self, pid: Optional[int] = None) -> list[dict]:
        """negotiation_head for each negotiation still open, oldest first; only those ``pid`` is a party to, if
        given. Read without waiting for the game."""
        return self._g.open_negotiation_heads(pid)

    def negotiations(self, pid: Optional[int] = None) -> list[dict]:
        """Copies of every negotiation of the game, settled ones included, oldest first; only ``pid``'s if given."""
        return json.loads(self._g.negotiations(pid))

    def negotiation_view(self, nid: int, pid: int) -> dict:
        """A negotiation as one side sees it, in its own terms. Raises ActionError."""
        return json.loads(self._g.negotiation_view(nid, pid))

    def end_turn_refusal(self, pid: int) -> Optional[str]:
        """Why the end_turn tool would refuse this player because of an open negotiation, or None."""
        return self._g.end_turn_refusal(pid)

    def close_negotiation(self, nid: int, status: str, note: str, by: Optional[int] = None) -> dict:
        """Close an open negotiation from outside it (a timeout, a forced close); returns a copy of it. Raises
        ActionError when it is not open."""
        record, events = self._g.close_negotiation(nid, status, note, by)
        self._fan(events)
        return json.loads(record)

    def max_chat_messages(self) -> int:
        """How many messages a negotiation may hold in this game."""
        return self._g.max_chat_messages()

    def deal(self, deal_id) -> Optional[dict]:
        """A copy of one concluded deal, or None."""
        try:
            n = int(deal_id)
        except (TypeError, ValueError):
            return None
        return _loads(self._g.deal(n))

    def describe_items(self, items: list) -> str:
        """Deal items as text ("50 gold and open borders for 30 turns")."""
        return self._g.describe_items(_dumps(items or []))

    def validate_items(self, giver: int, receiver: int, items: list, proposal: dict):
        """Check that ``giver`` can give ``items`` to ``receiver`` in this proposal. Raises ActionError."""
        self._g.validate_items(giver, receiver, _dumps(items or []), _dumps(proposal))

    def open_negotiation_as(self, pid: int, to: int, message: str, give=None, receive=None) -> dict:
        """Open a negotiation for ``pid`` whether or not it is its turn (a probe's scripted counterparty). Raises
        ActionError."""
        done, events = self._g.open_negotiation_as(pid, to, message, None if give is None else _dumps(give),
                                                   None if receive is None else _dumps(receive))
        self._fan(events)
        return json.loads(done)

    # ------------------------------------------------------------------ events, thoughts
    def subscribe(self, fn: Callable[[dict], None]):
        """Call ``fn(event)`` for every event the game records from now on, on the thread that caused it, after the
        call that caused it returns (the Python engine called it during the call). A subscriber that raises is
        logged; the others still hear the event."""
        self._subscribers.append(fn)

    def unsubscribe(self, fn: Callable[[dict], None]):
        """Stop calling a subscriber."""
        if fn in self._subscribers:
            self._subscribers.remove(fn)

    def emit(self, etype: str, text: str, players: Optional[list] = None, **data):
        """Record an event of the server's own (an AI error, a pause) and tell every subscriber. ``players`` None
        makes it public. ``data`` names the players and objects the event concerns by id (``player=2``; also ``a``,
        ``b``, ``owner``, ``sender``, ``unit``, ``city``, ``deal``, ``negotiation``): ValueError for anything else.
        A crashed game refuses it with EngineCrash."""
        self._fan(self._g.emit(etype, text, None if players is None else list(players),
                               _dumps(data) if data else None))

    def events(self, last: Optional[int] = None) -> list[dict]:
        """Every event, oldest first, as it happened (unscrubbed); the last ``last`` if given."""
        return json.loads(self._g.events(last or None))

    def event_view(self, ev: dict, pid: Optional[int]) -> dict:
        """One event (as :meth:`events` or a subscriber got it) as a player may see it: civilizations it has not met
        anonymised, their locations dropped. Raises ValueError for an event the game does not have."""
        return json.loads(self._g.event_view(ev["id"], pid))

    def add_thought(self, pid: int, text: str, kind: str):
        """Record an AI seat's reasoning (or an action or a system note) for spectators and the replay."""
        self._g.add_thought(pid, text, kind)

    def thoughts(self, pid: Optional[int] = None, since: int = 0) -> list[dict]:
        """Recorded thoughts from index ``since`` on, oldest first; only ``pid``'s if given."""
        return json.loads(self._g.thoughts(pid, since))

    def thought_count(self) -> int:
        """How many thoughts have been recorded (a mark for thoughts(since=...))."""
        return self._g.thought_count()

    # ------------------------------------------------------------------ seats
    def set_controller(self, pid: int, controller: str, handicap: Optional[str] = None,
                       auto: Optional[dict] = None):
        """Hand a civilization to a different turn driver. Its handicap and the decisions the engine takes for it
        follow, except those set explicitly. Raises ValueError for a bad setting."""
        self._fan(self._g.set_controller(pid, controller, handicap, None if auto is None else _dumps(auto)))

    def set_difficulty(self, pid: int, name: str) -> bool:
        """Give one seat its own difficulty level. Returns False (and changes nothing) for an unknown level."""
        return self._g.set_difficulty(pid, name)

    # ------------------------------------------------------------------ bots
    def drive(self, bots: dict, seat_limit: int = 0) -> dict:
        """The seats in ``bots`` ({player id: a bot handle}) play, turn after turn, until the host has something to
        do or the game is over; ``seat_limit`` driven turns at most, 0 for no limit. Each handle is read once, at the
        start. Passing every bot seat lets a negotiation one bot opens with another be answered inside the drive.

        Returns {"stop", "player", "negotiations", "actions"}: ``stop`` is "external" (the turn of a seat nobody
        drives), "awaiting_reply" (a negotiation waits on such a seat: ``negotiations``), "hybrid_diplomat" (a hybrid
        seat's model has diplomacy to do), "seat_limit" or "game_over"; ``player`` the seat it stopped at; ``actions``
        {player id: {tool: [taken, refused]}}, each bot's actions. Raises TypeError for a bot that is not a compiled
        one, ValueError for one keyed to a player that is no major civilization, EngineCrash if a bot or the engine
        panicked (the game then refuses every command)."""
        stop, events, actions = self._g.drive({int(p): _bot(b) for p, b in bots.items()}, seat_limit)
        self._fan(events)
        out = json.loads(stop)
        out["actions"] = _int_keys(json.loads(actions))
        return out

    def answer(self, pid: int, nid: int, bot) -> dict:
        """A seat's bot answers one negotiation that waits on it (the session's responder for a bot seat): accepts,
        counters or rejects, always with a line, or leaves it to the seat's model when that owns its kind. Returns
        {"outcome": "done" | "deferred", "actions": {tool: [taken, refused]}}. Raises ActionError when the
        negotiation does not wait on ``pid`` (a responder that lost a race ignores it)."""
        outcome, events, actions = self._g.answer(pid, nid, _bot(bot))
        self._fan(events)
        return {"outcome": outcome, "actions": json.loads(actions)}

    def play_bot_turn(self, pid: int, bot, end_turn: bool = False, execute=None):
        """Play one bot turn for ``pid`` (the bot from bot_instance or a profile): a drive of that seat alone, which
        ends the turn itself. A negotiation the seat is in that waits on another seat stops it: with ``end_turn`` the
        ones the bot opened are closed as expired, as a host closes them when its wait runs out, and the turn goes
        on to its end (Python's bot withdrew them); one the seat's model owns keeps the turn open, and ActionError
        says why, in end_turn's words. Without ``end_turn`` the turn is left where the drive stopped. ``execute`` is
        ignored: a bot's actions do not pass through the host's tool calls on Rust (:meth:`drive` counts them)."""
        for _ in range(1000):
            r = self.drive({pid: bot}, seat_limit=1)
            if r["stop"] == "hybrid_diplomat":
                continue
            if r["stop"] != "awaiting_reply" or not end_turn:
                return
            models = False
            for nid in r["negotiations"]:
                n = self.negotiation(nid)
                if n["status"] != "open":
                    continue
                # one waiting on the seat itself is one its bot left to the model
                if n["awaiting"] != pid and bot.owns_negotiation(_dumps(n)):
                    self.close_negotiation(nid, "expired", "No answer came before the turn ended.")
                else:
                    models = True
            if models:
                why = self.end_turn_refusal(pid) or "a negotiation waits on the seat's language model"
                raise _action_error(why, "negotiation")
        raise RuntimeError(f"player {pid}'s bot turn did not end after 1,000 drives")

    def bot_respond(self, pid: int, nid: int, bot, execute=None):
        """Let a bot answer a negotiation waiting on ``pid``: accept, counter or reject, always with a line; one the
        seat's model owns is left to it. ``execute`` is ignored, as for play_bot_turn. Raises ActionError when the
        negotiation does not wait on ``pid``."""
        self.answer(pid, nid, bot)

    def bot_advice(self, pid: int, bot, nid=None) -> dict:
        """What the bot makes of the diplomatic situation, for a language model to weigh: {"deal_value" (the proposal
        on the table in negotiation ``nid``, in gold from ``pid``'s side, or None), "war_readiness",
        "spare_luxuries", "wants"}. Reads only."""
        if isinstance(nid, dict):
            nid = nid.get("id")
        return json.loads(self._g.bot_advice(pid, _bot(bot), None if nid is None else int(nid)))

    # ------------------------------------------------------------------ scenario, map and debug ops
    def apply_ops(self, ops: list[dict]) -> list[dict]:
        """Apply scenario edit operations in order (see scenario_ops_help), all or nothing: ActionError names the
        first that fails, and the game is then as it was before the call."""
        done, events = self._g.apply_ops(_dumps(ops))
        self._fan(events)
        return json.loads(done)

    def scenario_overview(self) -> dict:
        """The scenario editor's summary: civilizations, relations, cities."""
        return json.loads(self._g.scenario_overview())

    def default_seats(self) -> list[dict]:
        """Seat types for a scenario, from how its civilizations were being played."""
        return json.loads(self._g.default_seats())

    def normalize_seats(self, seats: Optional[list]) -> list[dict]:
        """Check a scenario's seat list against its civilizations. Raises ActionError."""
        return json.loads(self._g.normalize_seats(None if seats is None else _dumps(seats)))

    def save_scenario(self, sid: str, name: str, description: str = "", seats: Optional[list] = None) -> dict:
        """Save this game as a scenario; returns its summary. Raises ActionError."""
        SCENARIO_DIR.mkdir(parents=True, exist_ok=True)
        sid = _slug(sid or name, "scenario")
        p = _scenario_path(sid)
        now = time.strftime("%Y-%m-%dT%H:%M:%S")
        created = now
        if p.exists():
            try:
                created = load_scenario(sid).get("created", now)
            except Exception:
                pass
        data = {"format": "citar-scenario", "version": 1, "id": sid, "name": name or sid,
                "description": description or "", "seats": self.normalize_seats(seats), "state": self.state_dict(),
                "created": created, "modified": now}
        tmp = p.with_suffix(".tmp")
        with gzip.open(tmp, "wt", encoding="utf-8") as f:
            json.dump(data, f)
        _fs_replace(tmp, p)
        return scenario_summary(data)

    def export_map(self, name: str = "") -> dict:
        """The game's terrain as a reusable map (cities, borders and units dropped)."""
        return json.loads(self._g.export_map(name or ""))

    def path_preview(self, pid: int, unit_id: int, x: int, y: int) -> dict:
        """The route a move order would take for one of ``pid``'s units: {"path": [[x, y], ...], "turns"}, or
        {"path": None} when there is none (or the unit is not theirs)."""
        try:
            return json.loads(self._g.path_preview(pid, unit_id, x, y))
        except OverflowError:   # an id or a coordinate past the binding's integers (a route's query) names nothing
            return {"path": None}

    def has_met(self, a: int, b: int) -> bool:
        """Whether two civilizations know each other."""
        return self._g.has_met(a, b)

    def meet(self, a: int, b: int):
        """Make two civilizations meet, with everything a first contact brings."""
        self._fan(self._g.meet(a, b))

    def force_turn(self, pid: int):
        """Make it ``pid``'s turn now and start it (a probe's single-turn case)."""
        self._fan(self._g.force_turn(pid))

    def debug(self, action: str):
        """A developer shortcut (see DEBUG_ACTIONS): "meet_all", "reveal" (the whole map to everyone) or "gold"
        (500 to every major). Raises ValueError for anything else."""
        self._fan(self._g.debug(action))

    # ------------------------------------------------------------------ rule scripts (tests only)
    def _test_ops_build(self, what: str):
        if not _E.HAS_TEST_OPS:
            raise NotImplementedError(f"EngineGame.{what}: this build of citar._engine has no test operations "
                                      "(cargo xtask develop, or a test-ops wheel).")

    def inspect(self, query: dict):
        """What a rule script reads of the game: ``query`` is ``{"what": ..., ...}`` and the answer a small shape, the
        same from both engines, as tests/rules/README.md documents. Reads only. Raises ActionError for a bad query.
        For tests: only a build with the test operations has it (NotImplementedError otherwise)."""
        self._test_ops_build("inspect")
        return json.loads(self._g.inspect(_dumps(query)))

    def test_ops(self, ops: list[dict]) -> list[dict]:
        """Apply test operations in order (``{"op": name, ...}``, see tests/rules/README.md), all or nothing: what a
        rule script does to a game that no player or editor may. Returns what each did. Raises ActionError naming the
        first that fails. For tests: only a build with the test operations has it (NotImplementedError otherwise)."""
        self._test_ops_build("test_ops")
        done, events = self._g.test_ops(_dumps(ops or []))
        self._fan(events)
        return json.loads(done)

    # ------------------------------------------------------------------ replay
    def replay_data(self) -> dict:
        """Everything the recap needs: the map, the players, and the whole game's frames, stats, events, messages,
        thoughts, negotiations and deals, and the game's ``config`` as :attr:`config` gives it. (The engine's own
        replay, which replay_json serves, carries the settings the client reads, its ``format`` and the unit palette
        a client of its delta frames needs.)"""
        data = json.loads(self._g.replay_data("full"))
        data.pop("format", None)
        data.pop("unit_ids", None)
        data["config"] = self.config
        return data

    def replay_json(self, extra: Optional[dict] = None) -> bytes:
        """:meth:`replay_data` as JSON bytes for a route to return as they are, ``extra``'s ``id`` and ``name`` first
        and each player given its ``seat`` from ``extra``'s ``seats`` (a list by player id)."""
        return self._g.replay_json(None if extra is None else _dumps(extra))

    def __repr__(self) -> str:
        return f"<EngineGame turn {self.turn} {self.phase}>"
