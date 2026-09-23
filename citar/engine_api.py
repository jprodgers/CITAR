"""The only door to the engine.

Everything outside ``citar/engine`` and ``citar/bots`` - the server, the agents, probes, benchmarks, the lab, balance
runs, ``citar sim`` - reaches the game through this module and nothing else (``tests/test_engine_boundary.py``
enforces it). The reason is the engine swap: the Python engine is a proof of concept that a Rust engine (the PyO3
module ``citar._engine``) replaces, and with one door the swap is a change of backend in this file rather than a hunt
through two hundred call sites. So the surface is shaped like the coarse Rust API:

Rust (``citar._engine``)                    here
------------------------------------------  ---------------------------------------------------------------------------
``new(config_json)``                        EngineGame.new
``load(bytes)``                             EngineGame.from_save, EngineGame.from_state
``save_snapshot()``                         EngineGame.to_save, EngineGame.state_dict
``execute(pid, tool, args_json)``           EngineGame.execute
``view(pid|None)``                          EngineGame.view
``summary()``                               EngineGame.summary and its narrower reads (turn, phase, player,
                                            standings, stats, events, thoughts, negotiation heads, ...)
``briefing(pid)``, ``turn_progress(pid)``   EngineGame.briefing, EngineGame.turn_progress
``negotiation_view(nid, pid)``              EngineGame.negotiation_view, negotiation, open_negotiations, ...
``empire_summary(pid)``                     EngineGame.empire_summary
``run_ai`` / ``bot_turn``                   EngineGame.play_bot_turn, bot_respond, bot_advice; run_game (a whole
                                            headless game); bot_instance
scenario, map-editor and debug ops          EngineGame.apply_ops, force_turn, debug, ...; the map and scenario
                                            functions below (their file I/O stays in Python)
``tool_schemas()``                          tool_list, tool_kind

Each method's docstring names the Rust call it maps to ("Rust: ...") where that is not obvious from the table.

Rules for callers:

- What comes back is plain data - dicts, lists, strings, numbers - never a live engine object, so changing it changes
  nothing in the game. Where a copy would be wasteful on a hot path the docstring says "live": treat those values as
  read-only. Events, thoughts and stats rows are appended by the engine and never changed afterwards, so they are
  handed out as they are.
- A refusal the caller can fix raises :class:`ActionError`, the engine's own class, so ``except ActionError`` works
  on either side of the door.
- An ``EngineGame`` is no more thread-safe than the game it wraps: the session's lock is the caller's business.
"""
from __future__ import annotations

import copy
import re
from typing import Any, Callable, Optional

from .engine import tools as _tools
from .engine import diplomacy as _diplomacy
from .engine.briefing import MAP_LEGEND, briefing as _briefing, turn_progress as _turn_progress
from .engine.game import ActionError, Game
from .engine.maps import MapError
from .engine.rules import RULES_VERSION, get_rules
from .engine.state import GameState
from .engine.views import RULES_OVERVIEW, client_view as _client_view, empire_info as _empire_info

# The whole public surface. Anything else this module holds (Game, GameState, get_rules, the engine modules imported
# under private names) is backend, and tests/test_engine_boundary.py fails a caller that reaches for it.
__all__ = [
    # errors, and text the prompts quote
    "ActionError", "MapError", "RULES_OVERVIEW", "MAP_LEGEND",
    # the ruleset and the tools
    "rules_version", "rules_client", "max_players", "map_sizes", "map_types", "speeds", "difficulties",
    "resolve_name", "ruleset_counts", "tool_list", "tool_kind", "state_summary",
    # maps and scenarios on disk
    "list_maps", "load_map", "save_map", "delete_map", "validate_map", "map_summary", "blank_map", "generate_map",
    "scenario_ops_help", "list_scenarios", "load_scenario", "scenario_summary", "delete_scenario",
    # bots and headless games
    "bot_instance", "DIPLOMACY_CATEGORIES", "item_category", "proposal_categories", "bot_set_diplomacy",
    "bot_owns_negotiation", "run_game",
    # one game
    "DEBUG_ACTIONS", "EngineGame",
]


def _plain(v):
    """A deep copy of JSON-like data (dicts, lists, scalars): what a Rust call would hand back, and much faster than
    copy.deepcopy on the small records it is used for."""
    if isinstance(v, dict):
        return {k: _plain(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_plain(x) for x in v]
    return v


def _head(n: dict) -> dict:
    """Where a negotiation stands, without the history and proposals that make a copy of it cost."""
    return {"id": n["id"], "initiator": n["initiator"], "responder": n["responder"], "status": n["status"],
            "awaiting": n["awaiting"], "entries": len(n["history"])}


# ----------------------------------------------------------------------------
# Rules, tools and constants (Rust: the ruleset the module was built with, and tool_schemas())
# ----------------------------------------------------------------------------
def rules_version() -> str:
    """The ruleset's version, recorded in every save. Rust: a constant of the module."""
    return RULES_VERSION


def rules_client() -> dict:
    """The whole ruleset as the browser needs it. Rust: the ruleset the module was built with, as JSON."""
    return get_rules().to_client()


def max_players() -> int:
    """The most seats a game can have. Rust: a ruleset query."""
    return get_rules().const["max_players"]


def map_sizes() -> dict:
    """Map size name -> {"width", "height", "players", "city_states", ...}. Rust: a ruleset query."""
    return _plain(get_rules().const["map_sizes"])


def map_types() -> list[str]:
    """The map generator's map types. Rust: a map-generator query."""
    from .engine.mapgen import MAP_TYPES
    return list(MAP_TYPES)


def speeds() -> list[str]:
    """The game speeds, by name. Rust: a ruleset query."""
    return list(get_rules().speeds)


def difficulties() -> list[str]:
    """The difficulty levels, easiest first. Rust: a ruleset query."""
    return list(get_rules().difficulty_list)


def resolve_name(kind: str, name) -> Optional[str]:
    """A ruleset name as the ruleset spells it (e.g. "quick" -> "Quick" for kind "speed"), or None if unknown.
    Rust: a ruleset query."""
    return get_rules().resolve(kind, name)


def ruleset_counts() -> dict:
    """How much the loaded ruleset holds, by kind (techs, units, buildings, nations, policies).
    Rust: a ruleset query."""
    R = get_rules()
    return {k: len(getattr(R, k)) for k in ("techs", "units", "buildings", "nations", "policies")
            if getattr(R, k, None) is not None}


_TOOL_KINDS: dict[str, str] = {}


def tool_list(kind: Optional[str] = None) -> list[dict]:
    """Every player tool with its JSON schema ({"name", "description", "input_schema", "kind", "any_time",
    "category"}), optionally only the ``query`` or ``action`` ones. Rust: tool_schemas()."""
    return _tools.tool_list(kind)


def tool_kind(name: str) -> Optional[str]:
    """Whether a tool is an "action" or a "query"; None for a name that is no tool. Rust: tool_schemas()."""
    if not _TOOL_KINDS:
        _TOOL_KINDS.update({t.name: t.kind for t in _tools.REGISTRY.values()})
    return _TOOL_KINDS.get(name)


def state_summary(state: dict) -> dict:
    """The headline facts of a saved game state (a save's or a scenario's "state"), without loading it:
    {"turn", "phase", "turn_limit" (the configured one, or None), "map_size", "map_type", "winner" (a name),
    "winner_id", "names" ({player id: name}, every civilization), "majors" ([{"id", "name", "nation", "alive"}]),
    "scores" ({major id: score in the last per-turn stats row}; empty before the first row is recorded)}.
    This is all a caller may know of the saved layout. Rust: none; it reads the saved JSON (the new save format will
    need its own)."""
    players = state.get("players", [])
    majors = [p for p in players if p.get("kind") == "major"]
    config = state.get("config") or {}
    winner = state.get("winner")
    stats = state.get("stats") or []
    last = stats[-1].get("players", {}) if stats else None
    return {"turn": state.get("turn"), "phase": state.get("phase"), "turn_limit": config.get("turn_limit"),
            "map_size": config.get("map_size"), "map_type": config.get("map_type"),
            "winner": players[winner].get("name") if winner is not None and 0 <= winner < len(players) else None,
            "winner_id": winner, "names": {p.get("id"): p.get("name") for p in players},
            "majors": [{"id": p.get("id"), "name": p.get("name"), "nation": p.get("nation"),
                        "alive": p.get("alive", True)} for p in majors],
            "scores": {} if last is None else {p.get("id"): (last.get(str(p.get("id"))) or {}).get("score", 0)
                                               for p in majors}}


# ----------------------------------------------------------------------------
# Maps and scenarios on disk (the file I/O stays in Python; the engine builds, checks and reads the values)
# ----------------------------------------------------------------------------
def list_maps() -> list[dict]:
    """Saved maps, in summary. Rust: none, file I/O."""
    from .engine import maps
    return maps.list_maps()


def load_map(map_id: str) -> dict:
    """One saved map. Raises MapError. Rust: none, file I/O."""
    from .engine import maps
    return maps.load_map(map_id)


def save_map(data: dict) -> tuple[dict, list[str]]:
    """Validate and save a map; returns (the map as saved, the problems that were fixed). Raises MapError.
    Rust: a map-editor op (validate) plus file I/O here."""
    from .engine import maps
    return maps.save_map(get_rules(), data)


def delete_map(map_id: str):
    """Delete a saved map. Rust: none, file I/O."""
    from .engine import maps
    maps.delete_map(map_id)


def validate_map(data: dict) -> tuple[dict, list[str]]:
    """Check a map without saving it: (the map with its problems fixed, what was fixed). Raises MapError.
    Rust: a map-editor op."""
    from .engine import maps
    return maps.validate(get_rules(), data)


def map_summary(data: dict) -> dict:
    """A map's headline facts, for lists. Rust: none, it reads the map's JSON."""
    from .engine import maps
    return maps.summary(data)


def blank_map(width: int, height: int, terrain: str, name: str = "") -> dict:
    """An empty map of one base terrain, for the editor to start from. Raises MapError. Rust: a map-editor op."""
    from .engine import maps
    R = get_rules()
    if terrain not in R.terrains or R.terrains[terrain]["type"] not in ("Land", "Water"):
        raise MapError(f"Unknown base terrain '{terrain}'.")
    return maps.blank_map(width, height, terrain, name)


def generate_map(width: int, height: int, map_type: str, players: int, city_states: int, seed: Optional[int] = None,
                 ruins: bool = True, name: str = "", options: Optional[dict] = None) -> dict:
    """A map from the random generator, for the editor to start from. Raises MapError.
    Rust: a map-editor op (the generator)."""
    from .engine import maps
    return maps.generated_map(get_rules(), width, height, map_type, players, city_states, seed, ruins, name, options)


def scenario_ops_help() -> list[dict]:
    """Every scenario edit operation with its parameters. Rust: a scenario op (the op list)."""
    from .engine import scenario
    return scenario.ops_help()


def list_scenarios() -> list[dict]:
    """Saved scenarios, in summary. Rust: none, file I/O."""
    from .engine import scenario
    return scenario.list_scenarios()


def load_scenario(sid: str) -> dict:
    """A saved scenario: {"id", "name", "description", "seats", "state", ...}. Raises ActionError.
    Rust: none, file I/O."""
    from .engine import scenario
    return scenario.load_scenario(sid)


def scenario_summary(data: dict) -> dict:
    """A scenario's headline facts, for lists. Rust: none, it reads the scenario's JSON."""
    from .engine import scenario
    return scenario.summary(data)


def delete_scenario(sid: str):
    """Delete a saved scenario. Raises ActionError for an invalid id. Rust: none, file I/O."""
    from .engine import scenario
    scenario.delete_scenario(sid)


# ----------------------------------------------------------------------------
# Bots (Rust: bots are compiled in, named by version, and take parameter-only profiles)
# ----------------------------------------------------------------------------
_BOT_ENGINE = re.compile(r"^(basic|idle|frozen_\w+|snapshot\w*)$")


def bot_instance(engine: str = "basic", *, seed: Optional[int] = None, aggression: float = 0.4,
                 params: Optional[dict] = None):
    """A bot to seat in a game: ``engine`` is "basic" (the live bot), "idle", or a frozen snapshot's module name.

    The bot is an opaque handle: give it to :meth:`EngineGame.play_bot_turn` or :func:`run_game`, never call it.
    With ``params`` None the bot is built with its own defaults; snapshots from before parameters existed ignore
    them. Raises ValueError for a name that is not a bot engine. (Profiles resolve to one of these in
    citar.bots.profiles.)
    Rust: a bot spec (version, parameters, seed) for run_ai / bot_turn.
    """
    if not _BOT_ENGINE.match(engine or ""):
        raise ValueError(f"'{engine}' is not a bot engine (basic, idle or a frozen_<hash> snapshot).")
    if engine == "idle":
        from .bots.idle import IdleBot
        return IdleBot()
    import importlib
    mod = importlib.import_module(f"citar.bots.{engine}")
    if params is None:
        return mod.BasicBot(aggression=aggression, seed=seed)
    try:
        return mod.BasicBot(aggression=aggression, seed=seed, params=params)
    except TypeError:                     # snapshots from before parameters existed
        return mod.BasicBot(aggression=aggression, seed=seed)


#: The kinds of diplomacy a hybrid seat hands to its bot or its language model, one by one
DIPLOMACY_CATEGORIES = _diplomacy.CATEGORIES


def item_category(item: dict) -> str:
    """The diplomacy category a deal item belongs to (gold -> "trades", peace_treaty -> "peace", ...).
    Rust: a diplomacy query."""
    return _diplomacy.item_category(item)


def proposal_categories(proposal: Optional[dict]) -> set:
    """The diplomacy categories a proposal touches; empty for none (a negotiation that is only talk).
    Rust: a diplomacy query."""
    return _diplomacy.proposal_categories(proposal)


def bot_set_diplomacy(bot, owners: Optional[dict]):
    """Say who decides each diplomacy category for a bot: {category: "bot" | "llm"}, "bot" for any not named. A
    category the language model owns is one the bot leaves alone. Raises ValueError for an unknown category or owner,
    or for a bot from before the switch existed (the archived snapshots). Rust: part of the bot spec."""
    if not hasattr(bot, "set_diplomacy"):
        raise ValueError("This bot predates the diplomacy switches; use the live bot or a newer snapshot.")
    bot.set_diplomacy(owners)


def bot_owns_negotiation(bot, negotiation: dict) -> bool:
    """Whether a bot answers this negotiation itself rather than the language model its seat hands that kind of
    diplomacy to. ``negotiation`` is a record from :meth:`EngineGame.negotiation`. Bots from before the switch (the
    archived snapshots) answer everything. Rust: a bot query."""
    owns = getattr(bot, "owns_negotiation", None)
    return owns is None or bool(owns(negotiation))


def run_game(spec: dict, on_turn: Optional[Callable[[dict], None]] = None,
             on_event: Optional[Callable[[dict], None]] = None) -> dict:
    """Play a whole headless game with a bot in every seat and return how it went. Rust: the headless runner.

    ``spec``:
      - ``config``: the game configuration, as for :meth:`EngineGame.new`;
      - ``bots``: {player id: a bot from bot_instance or citar.bots.profiles.make_bot};
      - ``max_errors`` (20): stop after more bot crashes than this; ``traceback_limit`` (5) frames per crash;
      - ``labels``: {player id: text} added to each crash line after the player;
      - ``raise_errors`` (False): let the first bot crash propagate instead of recording it, for runs whose point
        is that the bot does not crash (``citar sim`` and its test).

    ``on_turn(info)`` is called when a turn begins and once more if the game ended on a new turn, with {"turn",
    "phase", "turn_limit", "last_stats"}. ``on_event(event)`` gets every event after the game is created.

    Returns {"turn", "turns" (played), "phase", "winner", "victory", "turn_limit", "stats" (a row per turn),
    "players" (every civ; majors add techs, future_techs, policies, religion, great_people, cities, spaceship,
    difficulty and score, 0 once eliminated), "errors" (a line and a traceback per crash)}.
    """
    from .bots import headless
    return headless.play(spec["config"], spec["bots"], on_turn=on_turn, on_event=on_event,
                         max_errors=spec.get("max_errors", 20), traceback_limit=spec.get("traceback_limit", 5),
                         labels=spec.get("labels"), raise_errors=bool(spec.get("raise_errors")))


# ----------------------------------------------------------------------------
# One game
# ----------------------------------------------------------------------------
DEBUG_ACTIONS = ("meet_all", "reveal", "gold")


class EngineGame:
    """One game, behind the door. Build it with :meth:`new`, :meth:`from_save` or :meth:`from_state`."""

    def __init__(self, game: Game):
        self._g = game

    # ------------------------------------------------------------------ creation and saving
    @classmethod
    def new(cls, config: dict) -> "EngineGame":
        """A new game from a lobby configuration (map, speed, difficulty, "players": [{"controller", "nation",
        "handicap", "auto", "difficulty", ...}]). Raises ValueError or ActionError for a bad one. Rust: new()."""
        return cls(Game.new(config))

    @classmethod
    def from_save(cls, data: dict) -> "EngineGame":
        """A game from a save's engine part: {"state", "frames", "action_log"} (see to_save). Rust: load()."""
        from .engine import visibility
        g = Game(GameState.from_dict(data["state"]))
        g.frames = data.get("frames", [])
        g.action_log = data.get("action_log", [])
        visibility.refresh(g, force=True)
        return cls(g)

    @classmethod
    def from_state(cls, state: dict) -> "EngineGame":
        """A fresh, independent game from a state dict (a scenario's, a save's, or state_dict()'s). Rust: load()."""
        from .engine.scenario import game_from_state
        return cls(game_from_state(state))

    def to_save(self) -> dict:
        """The game's part of a save: {"state", "frames", "action_log"}. The state is built fresh; frames and the
        action log are live (they are large, and only ever appended to). Rust: save_snapshot()."""
        g = self._g
        g.save_rng()
        return {"state": g.s.to_dict(), "frames": g.frames, "action_log": g.action_log}

    def state_dict(self) -> dict:
        """An independent copy of the whole state, to undo to or to start a scenario from. Rust: save_snapshot()."""
        self._g.save_rng()
        return copy.deepcopy(self._g.s.to_dict())

    @property
    def python_game(self) -> Game:
        """The Python engine's live Game. For tests and engine-side tools only: the Rust backend has no such thing,
        and nothing in citar/ outside this module may touch it (the boundary test checks). Rust: none, by design."""
        return self._g

    # ------------------------------------------------------------------ summary reads (Rust: summary())
    @property
    def turn(self) -> int:
        """The current turn number. Rust: summary()."""
        return self._g.s.turn

    @property
    def current(self) -> int:
        """Whose turn it is, as a player id. Rust: summary()."""
        return self._g.s.current

    @property
    def phase(self) -> str:
        """The game's phase: "playing", or the phase a finished game is in. Rust: summary()."""
        return self._g.s.phase

    @property
    def winner(self) -> Optional[int]:
        """The winning player's id, once there is one. Rust: summary()."""
        return self._g.s.winner

    @property
    def victory(self) -> Optional[str]:
        """How the game was won, once it has been. Rust: summary()."""
        return self._g.s.victory

    @property
    def turn_limit(self) -> int:
        """The turn the game ends on: the configured limit, or the speed's own. Rust: summary()."""
        return self._g.total_turns()

    @property
    def config(self) -> dict:
        """The game's configuration (a copy). Rust: summary()."""
        return _plain(self._g.s.config)

    def _player_row(self, p) -> dict:
        """One civilization's public identity and settings."""
        d = {"id": p.id, "kind": p.kind, "name": p.name, "color": p.color, "leader": p.leader, "nation": p.nation,
             "alive": p.alive, "eliminated_turn": p.eliminated_turn, "controller": p.controller,
             "handicap": p.handicap, "auto": dict(p.auto), "overrides": _plain(p.overrides),
             "difficulty": p.difficulty, "founded_city": p.founded_city}
        if p.kind == "major":
            d["difficulty"] = p.difficulty or self._g.s.config.get("difficulty")
        return d

    def summary(self) -> dict:
        """The game at a glance: {"turn", "current", "phase", "winner", "victory", "turn_limit", "players"}, where
        each player is as :meth:`player` gives it. Rust: summary()."""
        s = self._g.s
        return {"turn": s.turn, "current": s.current, "phase": s.phase, "winner": s.winner, "victory": s.victory,
                "turn_limit": self._g.total_turns(), "players": [self._player_row(p) for p in s.players]}

    def player(self, pid: int) -> dict:
        """One civilization: {"id", "kind", "name", "color", "leader", "nation", "alive", "eliminated_turn",
        "controller", "handicap", "auto", "overrides" (the handicap/auto its seat set explicitly), "difficulty" (a
        major's falls back to the game's), "founded_city"}. Rust: summary()."""
        return self._player_row(self._g.player(pid))

    def player_name(self, pid: int) -> str:
        """A civilization's name. Rust: summary()."""
        return self._g.player(pid).name

    def is_alive(self, pid: int) -> bool:
        """Whether a civilization is still in the game. Rust: summary()."""
        return bool(self._g.player(pid).alive)

    def majors(self, alive_only: bool = True) -> list[dict]:
        """The major civilizations (not city-states or barbarians), as :meth:`player` gives them. Rust: summary()."""
        return [self._player_row(p) for p in self._g.majors(alive_only)]

    def standing(self, pid: int) -> dict:
        """How a civilization is doing: {"score", "cities", "units", "population", "techs", "gold", "era"}.
        ``score`` is the score formula's total even for a civilization that has been eliminated.
        Rust: summary() (standings)."""
        from .engine import research, victory
        g = self._g
        p = g.player(pid)
        cities = g.player_cities(pid)
        return {"score": victory.score(g, pid)["total"], "cities": len(cities), "units": len(g.player_units(pid)),
                "population": sum(c.pop for c in cities), "techs": len(p.techs), "gold": int(p.gold),
                "era": g.rules.era_list[research.player_era(g, pid)]}

    def standings(self) -> dict:
        """{player id: standing} for every major civilization, eliminated ones included. Rust: summary() (standings)."""
        return {p.id: self.standing(p.id) for p in self._g.majors(alive_only=False)}

    def stats(self, last: Optional[int] = None) -> list[dict]:
        """The per-turn statistics rows ({"turn", "players": {str(id): {...}}}), oldest first; the last ``last`` of
        them if given. Live rows (never changed once recorded). Rust: summary() (stats)."""
        rows = self._g.s.stats
        return list(rows[-last:] if last else rows)

    # ------------------------------------------------------------------ tools and views
    def execute(self, pid: int, tool: str, args: Optional[dict] = None) -> Any:
        """Run a player tool, with every rule the tool registry enforces. Raises ActionError. The result is the
        tool's own return value, built for the call but live in places (get_empire's happiness is the engine's
        cached figure): serialise it, don't edit it. Rust: execute()."""
        return _tools.execute(self._g, pid, tool, args or {})

    def view(self, pid: Optional[int], event_limit: int = 150) -> dict:
        """The game as one player sees it (None: everything), which is what the browser renders from. Built fresh
        on each call, but its event, thought, message and negotiation entries and each empire's happiness are live.
        Rust: view()."""
        return _client_view(self._g, pid, event_limit)

    def briefing(self, pid: int) -> str:
        """A language model's start-of-turn briefing. Rust: briefing()."""
        return _briefing(self._g, pid)

    def turn_progress(self, pid: int) -> str:
        """What is still unhandled this turn, as a short note. Rust: turn_progress()."""
        return _turn_progress(self._g, pid)

    def empire_summary(self, pid: int) -> dict:
        """The empire at a glance (get_empire's data), plus "cities" (how many), "at_war_with" (names) and "notes"
        (the civilization's notebook). A copy: read once per negotiation answer, not per frame.
        Rust: empire_summary()."""
        g = self._g
        p = g.player(pid)
        d = _empire_info(g, pid)
        d["cities"] = len(g.player_cities(pid))
        d["at_war_with"] = [g.player(q).name for q in p.met if g.at_war(pid, q)]
        d["notes"] = p.notes
        # empire_info hands out the engine's cached happiness, and the notebook is the player's own
        return _plain(d)

    # ------------------------------------------------------------------ negotiations
    def negotiation(self, nid: int) -> dict:
        """A copy of one negotiation: {"id", "initiator", "responder", "status", "awaiting", "proposal",
        "proposal_by", "history", "turn", ...}. Raises ActionError for an unknown id.
        Rust: negotiation_view() in the engine's own terms."""
        return _plain(_diplomacy.get_negotiation(self._g, nid))

    def open_negotiations(self, pid: Optional[int] = None) -> list[dict]:
        """Copies of the negotiations still open, oldest first; only those ``pid`` is a party to, if given. Code that
        only needs to know who owes an answer wants open_negotiation_heads, which copies no history.
        Rust: negotiation_view() in the engine's own terms."""
        return [_plain(n) for n in self._g.s.negotiations
                if n["status"] == "open" and (pid is None or pid in (n["initiator"], n["responder"]))]

    def negotiation_head(self, nid: int) -> dict:
        """Where one negotiation stands: {"id", "initiator", "responder", "status", "awaiting", "entries" (how many
        history entries; a new one means somebody spoke)}. Cheap enough to poll in a wait. Raises ActionError for an
        unknown id. Rust: summary() (negotiation heads)."""
        return _head(_diplomacy.get_negotiation(self._g, nid))

    def open_negotiation_heads(self, pid: Optional[int] = None) -> list[dict]:
        """negotiation_head for each negotiation still open, oldest first; only those ``pid`` is a party to, if
        given. For the session's after-every-action checks, which a copy of each chat's history would slow down.
        Rust: summary() (negotiation heads)."""
        return [_head(n) for n in self._g.s.negotiations
                if n["status"] == "open" and (pid is None or pid in (n["initiator"], n["responder"]))]

    def negotiations(self, pid: Optional[int] = None) -> list[dict]:
        """Copies of every negotiation of the game, settled ones included, oldest first; only ``pid``'s if given.
        Rust: negotiation_view() in the engine's own terms."""
        return [_plain(n) for n in self._g.s.negotiations if pid is None or pid in (n["initiator"], n["responder"])]

    def negotiation_view(self, nid: int, pid: int) -> dict:
        """A negotiation as one side sees it, in its own terms. Raises ActionError. A copy (the engine's view shares
        the proposal's item lists). Rust: negotiation_view()."""
        g = self._g
        return _plain(_diplomacy.negotiation_view(g, _diplomacy.get_negotiation(g, nid), pid))

    def end_turn_refusal(self, pid: int) -> Optional[str]:
        """Why the end_turn tool would refuse this player because of an open negotiation, or None.
        Rust: execute('end_turn') gives the same text as its error."""
        return _diplomacy.end_turn_refusal(self._g, pid)

    def close_negotiation(self, nid: int, status: str, note: str, by: Optional[int] = None) -> dict:
        """Close an open negotiation from outside it (a timeout, a forced close); returns a copy of it. Raises
        ActionError when it is not open. Rust: an op on execute()'s path, not a tool."""
        return _plain(_diplomacy.close_negotiation(self._g, nid, status, note, by))

    def max_chat_messages(self) -> int:
        """How many messages a negotiation may hold in this game. Rust: summary() (config)."""
        return _diplomacy.max_chat_messages(self._g)

    def deal(self, deal_id) -> Optional[dict]:
        """A copy of one concluded deal, or None. Rust: summary() (deals)."""
        return next((_plain(d) for d in self._g.s.deals if d.get("id") == deal_id), None)

    def describe_items(self, items: list) -> str:
        """Deal items as text ("50 gold and open borders for 30 turns"). Rust: a diplomacy query."""
        return _diplomacy.describe_items(self._g, items or [])

    def validate_items(self, giver: int, receiver: int, items: list, proposal: dict):
        """Check that ``giver`` can give ``items`` to ``receiver`` in this proposal. Raises ActionError.
        Rust: a diplomacy query."""
        _diplomacy.validate_items(self._g, giver, receiver, items, proposal)

    def open_negotiation_as(self, pid: int, to: int, message: str, give=None, receive=None) -> dict:
        """Open a negotiation for ``pid`` whether or not it is its turn (a probe's scripted counterparty). Negotiations
        are opened on the opener's turn, so the turn is lent to it for the call. Raises ActionError. Rust: a debug op."""
        g = self._g
        saved = g.s.current
        g.s.current = pid
        try:
            return _diplomacy.open_negotiation(g, pid, to, message, give, receive)
        finally:
            g.s.current = saved

    # ------------------------------------------------------------------ events, thoughts
    def subscribe(self, fn: Callable[[dict], None]):
        """Call ``fn(event)`` for every event the game emits from now on (on the thread that caused it). A
        subscriber that raises is ignored. Rust: events come back with each call and are fanned out here."""
        self._g.listeners.append(fn)

    def unsubscribe(self, fn: Callable[[dict], None]):
        """Stop calling a subscriber. Rust: as subscribe."""
        if fn in self._g.listeners:
            self._g.listeners.remove(fn)

    def emit(self, etype: str, text: str, players: Optional[list] = None, **data):
        """Record an event of the server's own (an AI error, a pause) and tell every subscriber. ``players`` None
        makes it public. Rust: an op (server events go into the game's event list)."""
        self._g.emit(etype, text, players, **data)

    def events(self, last: Optional[int] = None) -> list[dict]:
        """Every event, oldest first, as it happened (unscrubbed); the last ``last`` if given. Live entries.
        Rust: summary() (events), or the events each call returns."""
        ev = self._g.s.events
        return list(ev[-last:] if last else ev)

    def event_view(self, ev: dict, pid: Optional[int]) -> dict:
        """One event as a player may see it: civilizations it has not met anonymised, their locations dropped.
        Rust: view()'s event scrubbing."""
        return self._g.event_view(ev, pid)

    def add_thought(self, pid: int, text: str, kind: str):
        """Record an AI seat's reasoning (or an action or a system note) for spectators and the replay.
        Rust: an op (thoughts are saved with the game)."""
        self._g.s.thoughts.append({"turn": self._g.s.turn, "player": pid, "text": text, "kind": kind})

    def thoughts(self, pid: Optional[int] = None, since: int = 0) -> list[dict]:
        """Recorded thoughts from index ``since`` on, oldest first; only ``pid``'s if given. Live entries.
        Rust: summary() (thoughts)."""
        th = self._g.s.thoughts
        return [t for t in (th[since:] if since else th) if pid is None or t.get("player") == pid]

    def thought_count(self) -> int:
        """How many thoughts have been recorded (a mark for thoughts(since=...)). Rust: summary() (thoughts)."""
        return len(self._g.s.thoughts)

    # ------------------------------------------------------------------ seats
    def set_controller(self, pid: int, controller: str, handicap: Optional[str] = None,
                       auto: Optional[dict] = None):
        """Hand a civilization to a different turn driver. Its handicap and the decisions the engine takes for it
        follow, except those set explicitly (see Player.set_controller). Raises ValueError for a bad setting.
        Rust: a seat op."""
        self._g.player(pid).set_controller(controller, handicap, auto)
        self._g.invalidate()          # cached yields and costs depend on the handicap

    def set_difficulty(self, pid: int, name: str) -> bool:
        """Give one seat its own difficulty level. Returns False (and changes nothing) for an unknown level.
        Rust: a seat op."""
        level = self._g.rules.resolve("difficulty", name)
        if not level:
            return False
        self._g.player(pid).difficulty = level
        self._g.invalidate()
        return True

    # ------------------------------------------------------------------ bots (Rust: run_ai / bot_turn)
    @staticmethod
    def _bind(bot, execute):
        """Route a bot's tool calls through ``execute(pid, tool, args) -> result or None`` (a session records each
        one), rather than straight into the engine."""
        if execute is not None:
            bot.ex = lambda _g, p, _tool, **args: execute(p, _tool, args)

    def play_bot_turn(self, pid: int, bot, end_turn: bool = False, execute=None):
        """Play one bot turn for ``pid`` (the bot from bot_instance or a profile). ``execute`` routes its tool
        calls, as in _bind. With ``end_turn`` the bot settles its chats and ends the turn itself.
        Rust: bot_turn(pid, bot spec) / run_ai()."""
        self._bind(bot, execute)
        bot.play_turn(self._g, pid, end_turn=end_turn)

    def bot_respond(self, pid: int, nid: int, bot, execute=None):
        """Let a bot answer a negotiation waiting on ``pid``: accept, counter or reject, always with a line.
        Rust: bot_turn's negotiation answer."""
        self._bind(bot, execute)
        bot.respond(self._g, pid, nid)

    def bot_advice(self, pid: int, bot, nid: Optional[int] = None) -> dict:
        """What the bot makes of the diplomatic situation, for a language model to weigh (see BasicBot.advice).
        Rust: a bot query."""
        return bot.advice(self._g, pid, nid)

    # ------------------------------------------------------------------ scenario, map and debug ops
    def apply_ops(self, ops: list[dict]) -> list[dict]:
        """Apply scenario edit operations in order (see scenario_ops_help). Raises ActionError at the first one
        that fails, naming it; the ones before it have been applied. Rust: scenario ops."""
        from .engine import scenario
        return scenario.apply_ops(self._g, ops)

    def scenario_overview(self) -> dict:
        """The scenario editor's summary: civilizations, relations, cities. Rust: a scenario op."""
        from .engine import scenario
        return scenario.overview(self._g)

    def default_seats(self) -> list[dict]:
        """Seat types for a scenario, from how its civilizations were being played. Rust: a scenario op."""
        from .engine import scenario
        return scenario.default_seats(self._g)

    def normalize_seats(self, seats: Optional[list]) -> list[dict]:
        """Check a scenario's seat list against its civilizations. Raises ActionError. Rust: a scenario op."""
        from .engine import scenario
        return scenario.normalize_seats(self._g, seats)

    def save_scenario(self, sid: str, name: str, description: str = "", seats: Optional[list] = None) -> dict:
        """Save this game as a scenario; returns its summary. Raises ActionError.
        Rust: save_snapshot() plus file I/O here."""
        from .engine import scenario
        return scenario.save_scenario(self._g, sid, name, description, seats)

    def export_map(self, name: str = "") -> dict:
        """The game's terrain as a reusable map (cities, borders and units dropped). Rust: a map-editor op."""
        from .engine import maps
        return maps.map_from_game(self._g, name)

    def path_preview(self, pid: int, unit_id: int, x: int, y: int) -> dict:
        """The route a move order would take for one of ``pid``'s units: {"path": [[x, y], ...], "turns"}, or
        {"path": None} when there is none (or the unit is not theirs). Rust: a query op (the movement planner)."""
        from .engine import movement
        g = self._g
        u = g.unit(unit_id)
        if u is None or u.owner != pid or not g.grid.in_bounds(x, y):
            return {"path": None}
        path = movement.find_path(g, u, g.grid.idx(x, y))
        if not path:
            return {"path": None}
        return {"path": [list(g.grid.xy(i)) for i in path], "turns": movement.path_turns(g, u, path)}

    def has_met(self, a: int, b: int) -> bool:
        """Whether two civilizations know each other. Rust: summary()."""
        return self._g.has_met(a, b)

    def meet(self, a: int, b: int):
        """Make two civilizations meet, with everything a first contact brings. Rust: a scenario/debug op."""
        self._g.meet(a, b)

    def force_turn(self, pid: int):
        """Make it ``pid``'s turn now and start it (a probe's single-turn case). Rust: a debug op."""
        g = self._g
        if g.s.current != pid:
            g.s.current = pid
            g.s.turn_started = False
            g.begin_turn()

    def debug(self, action: str):
        """A developer shortcut (see DEBUG_ACTIONS): "meet_all", "reveal" (the whole map to everyone) or "gold"
        (500 to every major). Raises ValueError for anything else. Rust: debug ops."""
        from .engine import visibility
        g = self._g
        if action == "meet_all":
            for a in g.majors():
                for b in g.majors():
                    g.meet(a.id, b.id)
        elif action == "reveal":
            for p in g.majors():
                visibility.reveal_tiles(g, p.id, range(g.grid.size))
        elif action == "gold":
            for p in g.majors():
                p.gold += 500
        else:
            raise ValueError("Unknown debug action.")

    # ------------------------------------------------------------------ replay
    def replay_data(self) -> dict:
        """Everything the recap needs: the map, the players, and the whole game's frames, stats, events, messages,
        thoughts, negotiations and deals. Live lists: the replay is the largest read there is, and a copy would
        double it. Rust: the replay frames and summary()."""
        g = self._g
        s = g.s
        return {
            "width": s.width, "height": s.height, "wrap_x": g.grid.wrap_x, "wrap_y": g.grid.wrap_y,
            "terrain": [[t.terrain, 1 if t.hills else 0, int(t.river or 0), t.resource, t.wonder] for t in s.tiles],
            "improvement_ids": list(g.rules.improvements), "feature_ids": list(g.rules.terrains),
            "players": [{"id": p.id, "name": p.name, "leader": p.leader, "color": p.color, "kind": p.kind,
                         "alive": p.alive, "eliminated_turn": p.eliminated_turn} for p in s.players],
            "frames": g.frames, "stats": s.stats, "events": s.events, "messages": s.messages,
            "thoughts": s.thoughts, "negotiations": s.negotiations, "deals": s.deals,
            "winner": s.winner, "victory": s.victory, "phase": s.phase, "turn": s.turn, "config": s.config,
        }
