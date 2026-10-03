"""citar._engine: the Rust engine, its bots and its runner (crates/citar-py; DESIGN.md P2.6).

Only the facade (citar/engine_api.py) imports it. Every value that would be a dict or a list comes back as JSON bytes,
which the facade decodes; JSON arguments go in as bytes. Counts, ids and names are ints and strs. Heavy calls release the
GIL; the properties and the negotiation heads read a small copy of where the game stands and never wait for it.

Errors: ActionError (a refusal, with .code), MapError and LoadError (both ValueError), EngineCrash (RuntimeError: a
panic poisoned the game, which then refuses every command and still answers reads and saves), ValueError for a bad
argument, TypeError for a bot that is not a compiled one.
"""
from typing import Callable, Final, Optional

HAS_TEST_OPS: Final[bool]
"""Whether this build has the test operations (Game.inspect, Game.test_ops, Game.set_checks, run_game's test_panic)."""
DIPLOMACY_CATEGORIES: Final[tuple[str, ...]]
DEBUG_ACTIONS: Final[tuple[str, ...]]
RULES_OVERVIEW: Final[str]
MAP_LEGEND: Final[str]


class ActionError(Exception):
    """A refusal the caller can fix. The message is what a model reads."""
    code: str
    """not_your_turn, bad_param, invalid_player, eliminated, game_over, missing_param, off_map, no_such_unit,
    no_such_city, no_path, negotiation, rule or unknown_tool."""


class MapError(ValueError):
    """A map document that cannot be a map, or a map the generator cannot make."""


class LoadError(ValueError):
    """A save, or a save's part, that does not load."""


class EngineCrash(RuntimeError):
    """The engine stopped after an internal error: the message says where it panicked."""


# ---------------------------------------------------------------------------- the process
def build_info() -> bytes:
    """{version, build_id, label, rules, engine_code, bot_code}."""
def calls_in_flight() -> int:
    """How many heavy calls are running now."""
def shutdown(timeout: float = 5.0) -> bool:
    """Marks the process as exiting and waits for the calls in flight; registered with atexit at import. Afterwards a
    call another thread starts or ends parks that thread for good, so none re-attaches to a finalizing interpreter."""


# ---------------------------------------------------------------------------- the ruleset and the tools
def rules_version() -> str: ...
def rules_client() -> bytes: ...
def max_players() -> int: ...
def map_sizes() -> bytes:
    """{size: {name, width, height, players, city_states}}."""
def map_types() -> list[str]: ...
def speeds() -> list[str]: ...
def difficulties() -> list[str]: ...
def resolve_name(kind: str, name: Optional[str]) -> Optional[str]:
    """kind: tech, unit, building, promotion, terrain, resource, improvement, belief, policy, nation, era, specialist,
    speed, difficulty, unit_type or victory. ValueError for another."""
def ruleset_counts() -> bytes:
    """{techs, units, buildings, nations, policies}."""
def tool_list(kind: Optional[str] = None) -> bytes:
    """Every tool, or the "query" or "action" ones: [{name, description, input_schema, kind, any_time, category}]."""
def tool_kind(name: str) -> Optional[str]: ...


# ---------------------------------------------------------------------------- saves, maps and scenarios as values
def state_summary(state_json: bytes) -> bytes:
    """{turn, phase, turn_limit, map_size, map_type, winner, winner_id, names, majors, scores}. LoadError."""
def validate_map(doc_json: bytes) -> tuple[bytes, list[str]]:
    """(the map with its problems fixed, what was fixed). MapError."""
def map_summary(doc_json: bytes) -> bytes: ...
def blank_map(width: int, height: int, terrain: str, name: str = "") -> bytes: ...
def generate_map(seed: int, settings_json: Optional[bytes] = None) -> bytes:
    """The generator's map as the editor's document; the caller draws the seed."""
def scenario_ops_help() -> bytes: ...
def scenario_summary(doc_json: bytes) -> bytes:
    """ActionError for a document that is no scenario."""


# ---------------------------------------------------------------------------- diplomacy categories
def item_category(item_json: bytes) -> str: ...
def proposal_categories(proposal_json: bytes) -> list[str]:
    """The categories the terms touch (as the game stores them, {giver id: [items]}); [] for null."""


# ---------------------------------------------------------------------------- bots
def bot_versions() -> bytes:
    """[{id, label, description, latest, memory_kind}], the latest first."""
def bot_schema(version: str) -> bytes:
    """{engine, groups}; "basic" names the latest version. ValueError for an unknown one."""
def bot_clean_params(version: str, params_json: Optional[bytes] = None) -> bytes:
    """The overrides cleaned against the version's schema. ValueError for ones that do not clean."""
def bot_fingerprint(bot: "Bot") -> str: ...


class Bot:
    """A compiled bot's handle: version, parameters, aggression and who owns each kind of diplomacy."""

    def __init__(self, version: str = "basic", params_json: Optional[bytes] = None,
                 aggression: Optional[float] = None, fixed_aggression: Optional[float] = None) -> None:
        """aggression is the seat's; fixed_aggression the profile's, which wins and is what the fingerprint hashes;
        else 0.4. Held to 0..1. ValueError for an unknown version or parameters that do not clean."""
    def set_diplomacy(self, owners_json: Optional[bytes] = None) -> None:
        """{category: "bot" | "llm"}, "bot" for any not named; in place: a seat holding the handle follows from its
        next drive. ValueError for an unknown category or owner."""
    def owns_negotiation(self, negotiation_json: bytes) -> bool:
        """Whether the bot answers this negotiation (its record, as Game.negotiation gives it) itself."""
    def fingerprint(self) -> str: ...
    @property
    def aggression(self) -> float: ...
    @property
    def fixed_aggression(self) -> Optional[float]: ...
    @property
    def version(self) -> str: ...
    @property
    def owners(self) -> bytes: ...
    @property
    def params(self) -> bytes: ...


def run_game(spec_json: bytes, bots: dict[int, Bot], on_turn: Optional[Callable[[dict], object]] = None,
             on_event: Optional[Callable[[dict], object]] = None) -> bytes:
    """A whole headless game: {turn, turns, phase, winner, victory, turn_limit, stats, players, errors}.

    spec_json: config (as for Game.new), labels ({pid: text}), raise_errors (raise the first crash as EngineCrash),
    traceback_limit (5); max_errors is ignored; test_panic ({player, turn}) with test operations only. on_turn hears
    {turn, phase, turn_limit, last_stats} on the first turn, each new one and the one the game ended on; on_event
    every event after the game's creation, each step's before that step's on_turn. on_event is a listener, as it was
    on the Python engine: an Exception it raises is reported through sys.unraisablehook and the next event is still
    delivered. An exception from on_turn ends the run and is raised, and so does a KeyboardInterrupt (checked for
    between steps) or any other BaseException from either hook."""


# ---------------------------------------------------------------------------- one game
class Game:
    """One game behind a lock. Commands return the events they appended, as JSON bytes, each in Python's dict."""

    @staticmethod
    def new(config_json: bytes) -> "Game":
        """The lobby's settings with a seed and, for an editor map, its document inline. ValueError, MapError."""
    @staticmethod
    def load(state_json: bytes, chunks: list[bytes] = ...) -> tuple["Game", bytes]:
        """A save's state and the journal chunks of its history: (the game, {rules_changed, chronicle_incomplete,
        engine}). LoadError."""
    def save(self) -> tuple[bytes, Optional[bytes]]:
        """(the state, the whole history as one journal chunk or None): what Game.load reads back."""
    def state_json(self) -> bytes: ...
    def digest(self) -> str: ...

    # read without waiting for the game
    @property
    def turn(self) -> int: ...
    @property
    def current(self) -> int: ...
    @property
    def phase(self) -> str: ...
    @property
    def winner(self) -> Optional[int]: ...
    @property
    def victory(self) -> Optional[str]: ...
    @property
    def turn_limit(self) -> int: ...
    @property
    def revision(self) -> int: ...
    @property
    def poisoned(self) -> Optional[str]: ...
    def is_alive(self, pid: int) -> bool: ...
    def negotiation_head(self, nid: int) -> dict:
        """{id, initiator, responder, status, awaiting, entries}. ActionError for an unknown id."""
    def open_negotiation_heads(self, pid: Optional[int] = None) -> list[dict]: ...

    # reads
    def config(self) -> bytes: ...
    def summary(self) -> bytes: ...
    def player(self, pid: int) -> bytes: ...
    def player_name(self, pid: int) -> str: ...
    def majors(self, alive_only: bool = True) -> bytes: ...
    def standing(self, pid: int) -> bytes: ...
    def standings(self) -> bytes:
        """{str(pid): standing}."""
    def stats(self, last: Optional[int] = None) -> bytes: ...
    def events(self, last: Optional[int] = None) -> bytes: ...
    def event_view(self, event_id: int, pid: Optional[int] = None) -> bytes: ...
    def thoughts(self, pid: Optional[int] = None, since: int = 0) -> bytes: ...
    def thought_count(self) -> int: ...
    def add_thought(self, pid: int, text: str, kind: Optional[str] = None) -> None: ...
    def emit(self, kind: str, text: str, players: Optional[list[int]] = None,
             data_json: Optional[bytes] = None) -> bytes:
        """data_json names the players and objects it concerns by id: player, a, b, owner, sender, unit, city, deal,
        negotiation."""
    def take_violations(self) -> list[str]: ...

    # tools and views
    def execute(self, pid: int, tool: str, args_json: Optional[bytes] = None) -> tuple[bytes, bytes]:
        """(the tool's result, the events). ActionError."""
    def view_json(self, pid: Optional[int] = None, event_limit: int = 150,
                  extra_json: Optional[bytes] = None) -> bytes:
        """The client view, with extra_json's keys added (a key of the view's own is a ValueError)."""
    def briefing(self, pid: int) -> str: ...
    def turn_progress(self, pid: int) -> str: ...
    def empire_summary(self, pid: int) -> bytes: ...
    def end_turn_refusal(self, pid: int) -> Optional[str]: ...

    # negotiations
    def negotiation(self, nid: int) -> bytes: ...
    def open_negotiations(self, pid: Optional[int] = None) -> bytes: ...
    def negotiations(self, pid: Optional[int] = None) -> bytes: ...
    def negotiation_view(self, nid: int, pid: int) -> bytes: ...
    def close_negotiation(self, nid: int, status: str, note: str, by: Optional[int] = None) -> tuple[bytes, bytes]: ...
    def max_chat_messages(self) -> int: ...
    def deal(self, deal_id: int) -> Optional[bytes]: ...
    def describe_items(self, items_json: bytes) -> str: ...
    def validate_items(self, giver: int, receiver: int, items_json: bytes, proposal_json: bytes) -> None: ...
    def open_negotiation_as(self, pid: int, to: int, message: str, give_json: Optional[bytes] = None,
                            receive_json: Optional[bytes] = None) -> tuple[bytes, bytes]: ...

    # seats, scenario, map and debug commands
    def set_controller(self, pid: int, controller: str, handicap: Optional[str] = None,
                       auto_json: Optional[bytes] = None) -> bytes: ...
    def set_difficulty(self, pid: int, name: str) -> bool: ...
    def apply_ops(self, ops_json: bytes) -> tuple[bytes, bytes]:
        """All or nothing. ActionError names the operation that failed."""
    def scenario_overview(self) -> bytes: ...
    def default_seats(self) -> bytes: ...
    def normalize_seats(self, seats_json: Optional[bytes] = None) -> bytes: ...
    def export_map(self, name: str = "") -> bytes: ...
    def path_preview(self, pid: int, unit_id: int, x: int, y: int) -> bytes: ...
    def has_met(self, a: int, b: int) -> bool: ...
    def meet(self, a: int, b: int) -> bytes: ...
    def force_turn(self, pid: int) -> bytes: ...
    def end_turn(self, pid: int) -> bytes: ...
    def debug(self, action: str) -> bytes: ...

    # the replay
    def replay_data(self, format: str = "full") -> bytes: ...
    def replay_json(self, extra_json: Optional[bytes] = None) -> bytes:
        """extra_json's id and name first, then replay_data's keys; each player with its seat from extra_json's seats
        (a list by player id)."""

    # bots
    def drive(self, bots: dict[int, Bot], seat_limit: int = 0) -> tuple[bytes, bytes, bytes]:
        """(the stop {stop, player, negotiations}, the events, each bot's actions {pid: {tool: [taken, refused]}}).
        stop: external, hybrid_diplomat, awaiting_reply, seat_limit or game_over. TypeError for a Python bot,
        ValueError for a bot keyed to a player that is no major civilization."""
    def answer(self, pid: int, nid: int, bot: Bot) -> tuple[str, bytes, bytes]:
        """("done" or "deferred", the events, the bot's actions). ActionError when the negotiation does not wait on
        pid."""
    def bot_advice(self, pid: int, bot: Bot, nid: Optional[int] = None) -> bytes:
        """{deal_value, war_readiness, spare_luxuries, wants}."""

    # tests (HAS_TEST_OPS)
    def inspect(self, query_json: bytes) -> bytes: ...
    def test_ops(self, ops_json: bytes) -> tuple[bytes, bytes]: ...
    def set_checks(self, on: bool) -> None: ...
    def _lock_poisoned(self) -> bool:
        """Whether the game's own lock is poisoned (never: a panic is caught inside it). Test operations only."""
