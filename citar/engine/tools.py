"""The single registry of player tools (queries and actions).

Every interface — the browser UI, REST API, MCP server and LLM adapter — dispatches through `execute()`,
so rules are enforced identically for humans and AIs, and new tools automatically reach every client.
"""
from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any, Callable, Optional

from .game import Game, ActionError


@dataclass
class Tool:
    """One registered action or query: what it is called, what it does, and what it accepts.

    ``description`` and ``properties`` are not documentation in the usual sense - they are sent to
    language models as the tool definition, and shown in the browser. A vague description here is a
    model playing badly.

    ``kind`` separates a ``query``, which may be called freely and changes nothing, from an ``action``,
    which mutates the game, is written to the action log and is refused when it is not the caller's
    turn. ``any_time`` lifts that last restriction for the few actions that genuinely belong out of
    turn: answering a negotiation, voting at the United Nations, renaming a city.
    """
    name: str
    description: str
    properties: dict
    required: list
    fn: Callable
    kind: str = "action"          # action (mutates, logged) | query
    any_time: bool = False        # usable when it is not your turn
    category: str = "general"

    def schema(self) -> dict:
        """The JSON Schema for this tool's arguments.

        ``additionalProperties: false`` is deliberate: a model that invents an argument should be told, not
        quietly obeyed with the argument dropped.
        """
        return {"type": "object", "properties": self.properties, "required": self.required, "additionalProperties": False}

    def to_dict(self) -> dict:
        """The wire form, as sent to a model, an MCP client and the browser."""
        return {"name": self.name, "description": self.description, "input_schema": self.schema(),
                "kind": self.kind, "any_time": self.any_time, "category": self.category}


REGISTRY: dict[str, Tool] = {}


def tool(name: str, description: str, properties: Optional[dict] = None, required=(), kind="action", any_time=False,
         category="general"):
    """Register a function as a player tool.

    This decorator is the whole interface. Registering a function makes it callable from the browser,
    from MCP and from the LLM adapter at once, with the schema generated from *properties* - so there is
    no way to add an action to one interface and forget another, and no way for a model to have a power
    a human does not.
    """
    def deco(fn):
        """Record the function in the registry and return it unchanged."""
        REGISTRY[name] = Tool(name, description, properties or {}, list(required), fn, kind, any_time, category)
        return fn
    return deco


INT = {"type": "integer"}
STR = {"type": "string"}
BOOL = {"type": "boolean"}
XY = {"x": {"type": "integer", "description": "column"}, "y": {"type": "integer", "description": "row"}}
STRS = {"type": "array", "items": {"type": "string"}}
ITEMS = {"type": "array", "items": {"type": "object"},
         "description": "Deal items, e.g. [{\"type\":\"gold\",\"amount\":50}, {\"type\":\"gold_per_turn\",\"amount\":3,"
                        "\"turns\":30}, {\"type\":\"resource\",\"resource\":\"Iron\",\"amount\":1,\"turns\":30}, "
                        "{\"type\":\"open_borders\",\"turns\":30}, {\"type\":\"embassy\"}, {\"type\":\"peace_treaty\"}, "
                        "{\"type\":\"declaration_of_friendship\"}, {\"type\":\"research_agreement\"}, "
                        "{\"type\":\"defensive_pact\"}, {\"type\":\"declare_war\",\"target\":3}, "
                        "{\"type\":\"city\",\"city_id\":12}, {\"type\":\"share_map\"}, {\"type\":\"tech\",\"tech\":\"Writing\"}]"}


def execute(g: Game, pid: int, name: str, args: Optional[dict] = None) -> Any:
    """Call a tool by name, after checking that the caller is allowed to.

    Every interface goes through here, which is why the rules are identical for a human clicking a
    button and a model emitting a tool call. The checks, in order: the tool exists, the player is a
    real major civilization, they are alive, the game is still running, and it is their turn unless the
    tool is marked ``any_time``.

    Arguments are then coerced rather than rejected on type alone - models routinely send ``"3"`` for
    an integer and a comma-separated string for an array - and unknown keys are dropped. An action is
    appended to the action log and the random state is saved, so a replay can be reconstructed exactly.

    Raises:
        ActionError: for an unknown tool, a caller who may not act, or an argument that is wrong in a
            way the caller can fix. The message is written to be read by whoever caused it, model or
            person.
    """
    t = REGISTRY.get(name)
    if t is None:
        raise ActionError(f"Unknown tool '{name}'.")
    args = dict(args or {})
    if pid is None or pid < 0 or pid >= len(g.s.players) or g.player(pid).kind != "major":
        raise ActionError("Invalid player.")
    if not g.player(pid).alive and t.kind == "action":
        raise ActionError("Your civilization has been eliminated.")
    if t.kind == "action" and g.s.phase != "playing":
        raise ActionError("The game is over.")
    if t.kind == "action" and not t.any_time and g.s.current != pid:
        raise ActionError(f"It is not your turn (it is {g.player(g.s.current).name}'s turn).")
    missing = [r for r in t.required if r not in args or args[r] is None]
    if missing:
        raise ActionError(f"Missing required parameter(s): {', '.join(missing)}.")
    for k in [k for k in args if k not in t.properties]:
        args.pop(k)
    for k, spec in t.properties.items():
        if k in args and spec.get("type") == "integer" and args[k] is not None:
            try:
                args[k] = int(args[k])
            except (TypeError, ValueError):
                raise ActionError(f"Parameter '{k}' must be an integer.")
        if k in args and spec.get("type") == "boolean" and isinstance(args[k], str):
            args[k] = args[k].lower() in ("true", "1", "yes")
        if k in args and spec.get("type") == "array" and isinstance(args[k], str):
            args[k] = [s.strip() for s in args[k].split(",") if s.strip()]
    result = t.fn(g, pid, **args)
    if t.kind == "action":
        g.action_log.append({"t": round(time.time(), 2), "turn": g.turn, "player": pid, "tool": name, "args": args})
        g.save_rng()
    return result


def tool_list(kind: Optional[str] = None) -> list[dict]:
    """Every registered tool as a dictionary, optionally filtered to ``query`` or ``action``."""
    return [t.to_dict() for t in REGISTRY.values() if kind is None or t.kind == kind]


# ----------------------------------------------------------------------------
# helpers
# ----------------------------------------------------------------------------
def _idx(g: Game, x: int, y: int) -> int:
    """Turn map coordinates into a tile index, refusing anything off the map.

    The error names the map's dimensions, because the usual cause is a model that has assumed a
    different map size and will keep making the same mistake until it is told.
    """
    if not g.grid.in_bounds(x, y):
        raise ActionError(f"({x},{y}) is off the map (map is {g.s.width}x{g.s.height}).")
    return g.grid.idx(x, y)


def _own_unit(g: Game, pid: int, unit_id: int):
    """Fetch one of the caller's own units.

    The refusal lists the units they do have. Units vanish - consumed by founding a city, killed in
    combat - and a model that has lost track of one recovers much faster from a list than from "no such
    unit".
    """
    u = g.unit(unit_id)
    if u is None or u.owner != pid:
        ids = ", ".join(f"#{x.id} {x.type}" for x in g.player_units(pid))
        raise ActionError(f"You have no unit with id {unit_id} (units are used up by some actions and lost when "
                          f"killed). Your units now: {ids or 'none'}.")
    return u


def _own_city(g: Game, pid: int, city_id: int):
    """Fetch one of the caller's own cities, listing the real ones if that id is not theirs."""
    c = g.city(city_id)
    if c is None or c.owner != pid:
        ids = ", ".join(f"#{x.id} {x.name}" for x in g.player_cities(pid))
        raise ActionError(f"You have no city with id {city_id}. Your cities: {ids or 'none'}.")
    return c


def _city_state(g: Game, pid: int, player_id: int):
    """Fetch a city-state the caller has met, by player id."""
    if player_id < 0 or player_id >= len(g.s.players) or g.player(player_id).kind != "city_state":
        raise ActionError(f"Player {player_id} is not a city-state.")
    if not g.has_met(pid, player_id):
        raise ActionError(f"You have not met {g.player(player_id).name}.")
    return g.player(player_id)


def _refresh(g: Game):
    """Recompute what every player can see.

    Called after anything that moves a unit, changes a border or destroys something. Imported inside
    the function because visibility imports back into this module at load time.
    """
    from .visibility import refresh
    refresh(g)


# ============================================================================
# QUERIES
# ============================================================================
@tool("get_briefing", "Your turn briefing: empire status, cities, units (idle ones marked), events since your last turn, "
      "diplomacy, and a to-do list. Start every turn with this.", kind="query", any_time=True, category="info")
def get_briefing(g: Game, pid: int):
    """Delegates to :func:`citar.engine.briefing.briefing`.

    The briefing is deliberately self-sufficient: an agent that reads it should not need a dozen follow
    up queries before it can act. Earlier versions were terse and turns took seven to fifteen minutes
    because of it.
    """
    from .briefing import briefing
    return briefing(g, pid)


@tool("get_map", "ASCII hex map of the area around (x,y) (defaults to your capital) showing terrain, features, "
      "units, cities, camps, ruins and resources you know about. Radius 2-20.",
      {"x": INT, "y": INT, "radius": {"type": "integer", "description": "rows above/below center (default 8)"},
       "legend": {"type": "boolean", "description": "include the map legend (default false)"}},
      kind="query", any_time=True, category="info")
def get_map(g: Game, pid: int, x: Optional[int] = None, y: Optional[int] = None, radius: int = 8, legend: bool = False):
    """Render the area around a point as ASCII, for an agent that has no canvas.

    Coordinates are validated before rendering so that a bad centre is an error rather than a map of
    somewhere unexpected. The legend is optional because it is long and identical every time, which
    matters when it is being sent to a model with a context limit.
    """
    from .briefing import ascii_map, MAP_LEGEND
    if x is not None and y is not None:
        _idx(g, x, y)
    text = ascii_map(g, pid, x, y, radius or 8)
    return (MAP_LEGEND + "\n\n" + text) if legend else text


@tool("get_tile", "Details of one tile you have explored: terrain, yields, resource, improvement, owner, units.",
      {**XY}, ["x", "y"], kind="query", any_time=True, category="info")
def get_tile(g: Game, pid: int, x: int, y: int):
    """Delegates to :func:`citar.engine.views.tile_info`, for one explored tile."""
    from .views import tile_info
    return tile_info(g, _idx(g, x, y), pid)


@tool("get_unit", "Full details of one of your units: special actions (unit_action), build options, reachable tiles, "
      "attack targets with predicted damage, upgrade and promotion options.", {"unit_id": INT}, ["unit_id"],
      kind="query", any_time=True, category="info")
def get_unit(g: Game, pid: int, unit_id: int):
    """Full detail for one of the caller's units, including what it could do from where it stands."""
    from .views import unit_info
    return unit_info(g, _own_unit(g, pid, unit_id), pid, detail=True)


@tool("get_units", "Compact list of all your units.", kind="query", any_time=True, category="info")
def get_units(g: Game, pid: int):
    """Every unit the caller owns, in the compact form."""
    from .views import unit_info
    return [unit_info(g, u, pid) for u in g.player_units(pid)]


@tool("get_city", "Full details of one of your cities: yields, growth, production queue, everything it can build or "
      "buy (gold/faith), specialists, worked and buyable tiles, religion.", {"city_id": INT}, ["city_id"],
      kind="query", any_time=True, category="info")
def get_city(g: Game, pid: int, city_id: int):
    """Full detail for one of the caller's cities, including everything it could build or buy."""
    from .views import city_info
    return city_info(g, _own_city(g, pid, city_id), pid, detail=True)


@tool("get_cities", "Summary of your cities.", kind="query", any_time=True, category="info")
def get_cities(g: Game, pid: int):
    """Every city the caller owns, in the compact form."""
    from .views import city_info
    return [city_info(g, c, pid) for c in g.player_cities(pid)]


@tool("get_empire", "Empire overview: gold, science, culture and faith per turn with breakdowns, happiness, golden age, "
      "resources, era, score and spaceship progress.", kind="query", any_time=True, category="info")
def get_empire(g: Game, pid: int):
    """The empire summary: yields with their breakdowns, happiness, resources, score, victory progress."""
    from .views import empire_info
    return empire_info(g, pid)


@tool("get_players", "All civilizations and city-states you know of (ids, names, war/peace status, score).",
      kind="query", any_time=True, category="info")
def get_players(g: Game, pid: int):
    """Everyone the caller has met, with war and peace status and score."""
    from .views import players_overview
    return players_overview(g, pid)


@tool("get_diplomacy", "Diplomacy: relations and agreements with each civ, what could be traded, open negotiations "
      "(with history and the current proposal from your perspective), active deals and recent messages.",
      {"message_limit": INT}, kind="query", any_time=True, category="diplomacy")
def get_diplomacy(g: Game, pid: int, message_limit: int = 30):
    """Relations, open negotiations, active deals and recent messages.

    Proposals are rendered from the caller's perspective - what they would give and receive - because
    the other formulation is a reliable source of accepted deals nobody meant to accept.
    """
    from .views import diplomacy_info
    return diplomacy_info(g, pid, message_limit)


@tool("get_city_states", "City-states you have met: type, personality, influence, ally, your bonuses, quests, and "
      "what tribute they would pay.", kind="query", any_time=True, category="diplomacy")
def get_city_states(g: Game, pid: int):
    """Met city-states with influence, allies, quests and what tribute they would pay."""
    from .views import city_states_info
    return city_states_info(g, pid)


@tool("get_tech_tree", "Technologies with status (known/available/locked), cost, prerequisites and what each unlocks. "
      "filter: 'available' (default), 'all', or 'locked'.", {"filter": STR}, kind="query", any_time=True, category="info")
def get_tech_tree(g: Game, pid: int, filter: str = "available"):
    """The technology tree, filtered to ``available`` by default.

    Available rather than everything, because the whole tree is 80 entries of mostly locked techs and
    the question being asked is almost always "what can I research now".
    """
    from .views import tech_tree
    tt = tech_tree(g, pid)
    if filter != "all":
        tt["techs"] = [t for t in tt["techs"] if t["status"] == (filter or "available")]
    return tt


@tool("get_policies", "Social policies: culture, cost of the next policy, branches (open/locked/completed) with their "
      "policies and effects, and what you can adopt now.", kind="query", any_time=True, category="info")
def get_policies(g: Game, pid: int):
    """Social policies: culture, the next policy's cost, and what can be adopted now."""
    from .views import policies_info
    return policies_info(g, pid)


@tool("get_religion", "Religion: your faith, pantheon/religion, available beliefs, faith needed for the next Great "
      "Prophet, religions in the world and what you can buy with faith.", kind="query", any_time=True, category="info")
def get_religion(g: Game, pid: int):
    """Faith, pantheon and religion, available beliefs, and what faith can buy."""
    from .views import religion_info
    return religion_info(g, pid)


@tool("get_great_people", "Great person points and progress per type, golden age progress and free great people to "
      "choose.", kind="query", any_time=True, category="info")
def get_great_people(g: Game, pid: int):
    """Great person progress per type, and any free choice waiting to be made."""
    from .views import great_people_info
    return great_people_info(g, pid)


@tool("get_espionage", "Your spies, where they are and what they are doing.", kind="query", any_time=True,
      category="info")
def get_espionage(g: Game, pid: int):
    """The caller's spies and what each is doing."""
    from .espionage import espionage_view
    return espionage_view(g, pid)


@tool("get_rules", "Look up game rules (UnCiv 'Civ V - Gods & Kings' data). topic: units | buildings | techs | "
      "improvements | resources | promotions | terrains | policies | beliefs | specialists | eras | nations | "
      "city_state_types | speeds | difficulties | deal_items | combat | overview. Optionally name for one entry.",
      {"topic": STR, "name": STR}, ["topic"], kind="query", any_time=True, category="info")
def get_rules(g: Game, pid: int, topic: str, name: Optional[str] = None):
    """Look up the ruleset: a whole topic, or one entry by name.

    The reason a player can query the rules at all is that a model's memory of Civ V is approximate and
    often wrong about numbers. Being able to check is the difference between a plan and a guess.
    """
    from .views import rules_lookup
    return rules_lookup(g, topic, name)


@tool("read_notes", "Read your private strategy notebook (persists across turns and saves).", kind="query",
      any_time=True, category="meta")
def read_notes(g: Game, pid: int):
    """The caller's private notebook, or a marker when it is empty."""
    return g.player(pid).notes or "(empty)"


@tool("get_events", "Your notifications since a given event id (default: the last 40).", {"since_id": INT},
      kind="query", any_time=True, category="info")
def get_events(g: Game, pid: int, since_id: int = 0):
    """Notifications since an event id, capped at the last forty.

    The cap is not politeness - an agent that has ignored its notifications for fifty turns would
    otherwise be handed a wall of text in place of its turn.
    """
    evs = g.events_for(pid, since_id)
    return [{"id": e["id"], "turn": e["turn"], "type": e["type"], "text": e["text"]} for e in evs[-40:]]


@tool("preview_attack", "Predict an attack (strengths, modifiers, damage range) without performing it.",
      {"unit_id": INT, **XY}, ["unit_id", "x", "y"], kind="query", any_time=True, category="unit")
def preview_attack(g: Game, pid: int, unit_id: int, x: int, y: int):
    """Predict a fight without starting one: strengths, modifiers and the damage range.

    A query rather than an action, so it can be called repeatedly while deciding. This is the tool that
    lets a careful player check before throwing a unit away.
    """
    from .combat import preview
    return preview(g, _own_unit(g, pid, unit_id), _idx(g, x, y))


@tool("get_victory_status", "Victory conditions and progress: scores, milestones per victory type, spaceship, "
      "United Nations vote and the turn limit.", kind="query", any_time=True, category="info")
def get_victory_status(g: Game, pid: int):
    """Progress toward every victory condition, including the turn limit."""
    from .views import victory_info
    return victory_info(g, pid)


# ============================================================================
# UNIT ACTIONS
# ============================================================================
@tool("move_unit", "Move a unit toward (x,y) using the best path. Moves as far as possible this turn and keeps going "
      "on later turns automatically. Aircraft rebase to a city or carrier instead. Moving a military unit onto an "
      "undefended enemy civilian captures it (requires war); onto a barbarian camp clears it; onto ruins explores them.",
      {"unit_id": INT, **XY}, ["unit_id", "x", "y"], category="unit")
def move_unit(g: Game, pid: int, unit_id: int, x: int, y: int):
    """Move a unit toward a tile, as far as this turn's movement allows.

    The destination is remembered, so a unit ordered somewhere distant continues on later turns without
    being told again.

    Three cases are handled here rather than in the mover. Aircraft rebase instead of walking. A unit
    with no movement left gets an error that says *why* it has none - its own explore or automation
    order usually spent it - because "no moves left" on a unit the caller has not touched this turn
    reads as a bug. And a move that achieves nothing is only an error when the unit will not try again:
    if the standing order survives, it is a note, not a failure.
    """
    from . import movement, combat
    u = _own_unit(g, pid, unit_id)
    idx = _idx(g, x, y)
    if g.udef(u)["_domain"] == "Air":
        return combat.rebase(g, u, idx)
    name = f"{u.type} #{u.id}"
    if idx == u.idx:
        raise ActionError(f"{name} is already at ({x},{y}).")
    if u.moves <= 0:
        why = {"explore": " (its explore order already moved it this turn)",
               "goto": " (its standing move order already moved it this turn)",
               "automate": " (its automated orders already used them)"}.get(u.activity, "")
        raise ActionError(f"{name} has no moves left this turn{why}. Nothing happened. Moves refresh next turn.")
    u.activity = None
    res = movement.move_toward(g, u, idx)
    _refresh(g)
    if res["from"] == res["to"] and not res["arrived"]:
        if g.unit(u.id) is not None and u.goto == idx:
            res["note"] = f"{name} could not move this turn ({res['stopped']}); it will keep trying next turn."
        else:
            raise ActionError(f"{name} could not move toward ({x},{y}): {res['stopped']}.")
    return res


@tool("attack", "Attack the tile (x,y): melee units must be adjacent, ranged units within range and line of sight, "
      "aircraft within their range (air strike), nuclear weapons detonate on the target. Target may be an enemy unit "
      "or city (you must be at war; nukes also declare war). A melee attack on a city at 0 HP captures it.",
      {"unit_id": INT, **XY}, ["unit_id", "x", "y"], category="unit")
def attack_tool(g: Game, pid: int, unit_id: int, x: int, y: int):
    """Attack a tile, dispatching to the right kind of attack for the unit.

    Nuclear weapons and aircraft do not attack the way a swordsman does, and asking the caller to know
    which verb to use would be a rule about the interface rather than about the game. The unit's own
    definition decides.
    """
    from . import combat
    from . import unique_types as U
    u = _own_unit(g, pid, unit_id)
    idx = _idx(g, x, y)
    ud = g.udef(u)
    if ud["_umap"].get(U.NuclearWeapon):
        return combat.nuke(g, u, idx)
    if ud["_domain"] == "Air":
        return combat.air_strike(g, u, idx)
    res = combat.attack(g, u, idx)
    _refresh(g)
    return res


@tool("air_sweep", "A fighter sweeps the tile (x,y), attacking enemy interceptors before your bombers go in.",
      {"unit_id": INT, **XY}, ["unit_id", "x", "y"], category="unit")
def air_sweep(g: Game, pid: int, unit_id: int, x: int, y: int):
    """Clear enemy interceptors over a tile before bombers go in."""
    from . import combat
    return combat.air_sweep(g, _own_unit(g, pid, unit_id), _idx(g, x, y))


@tool("unit_action", "Use a unit's special ability. get_unit lists its actions with ids, e.g. found_city (Settler; "
      "optional name), found_religion (Great Prophet in your city; name + beliefs), enhance_religion (beliefs), "
      "spread_religion, remove_heresy, hurry_research, hurry_construction, trade_mission, create:<Improvement> "
      "(Academy, Citadel, Holy site, Landmark, Manufactory, Customs house, Fishing Boats with Work Boats...), "
      "add_to_spaceship, paradrop (Paratrooper; x, y of the target tile), and one-time effects such as a Great "
      "Artist's golden age (trigger:<n>).",
      {"unit_id": INT, "action": STR, "name": STR, "beliefs": STRS, "x": INT, "y": INT}, ["unit_id", "action"],
      category="unit")
def unit_action(g: Game, pid: int, unit_id: int, action: str, name: Optional[str] = None, beliefs=None,
                x: Optional[int] = None, y: Optional[int] = None):
    """Use a unit's special ability, whatever that unit happens to be.

    One tool rather than thirty, because the set is long, mostly unit-specific, and grows with the
    ruleset. ``get_unit`` lists the actions available to a particular unit with their exact ids, so a
    caller discovers them rather than memorising them.
    """
    from .actions import do_unit_action
    target = g.grid.idx(x, y) if x is not None and y is not None and g.grid.in_bounds(x, y) else None
    res = do_unit_action(g, _own_unit(g, pid, unit_id), action, name=name, beliefs=beliefs, target=target)
    _refresh(g)
    return res


@tool("found_city", "Found a city with a Settler on its current tile (same as unit_action found_city).",
      {"unit_id": INT, "name": STR}, ["unit_id"], category="unit")
def found_city_tool(g: Game, pid: int, unit_id: int, name: Optional[str] = None):
    """Found a city, and return what it can build straight away.

    The same action as ``unit_action found_city``, given its own tool because it is the first thing
    anybody does and it deserves to be findable. The construction options come back with it so that
    founding and setting production are one exchange instead of two - which matters when each exchange
    is a model call.
    """
    from .actions import do_unit_action
    from . import cities
    res = do_unit_action(g, _own_unit(g, pid, unit_id), "found_city", name=name)
    c = g.city(res["city_id"])
    _refresh(g)
    items = cities.buildable_items(g, c)
    res["production"] = cities.current_construction(c) or "nothing (use set_production)"
    res["can_build"] = {k: v for k, v in items.items() if v}
    return res


@tool("build_improvement", "Order a Worker to build on its current tile (farm, mine, pasture, plantation, camp, "
      "quarry, lumber mill, trading post, road, railroad, fort, remove forest/jungle/marsh, repair...). If a feature "
      "must be removed first, that is queued automatically. get_unit lists the valid options with turns. Work "
      "progresses at the end of each turn the worker spends on the tile with movement left.",
      {"unit_id": INT, "improvement": STR}, ["unit_id", "improvement"], category="unit")
def build_improvement(g: Game, pid: int, unit_id: int, improvement: str):
    """Order a worker to build on the tile it is standing on."""
    from . import workers
    return workers.start_build(g, _own_unit(g, pid, unit_id), improvement)


@tool("unit_order", "Give a standing order: fortify (military), sleep (until woken or enemies near), wake, skip "
      "(end this unit's turn), heal (rest until healed), explore (automatic), automate (automatic worker), pillage "
      "(enemy improvement here), setup (siege units), disband (delete; refunds a little gold inside your borders), "
      "cancel (clear orders).", {"unit_id": INT, "order": STR}, ["unit_id", "order"], category="unit")
def unit_order(g: Game, pid: int, unit_id: int, order: str):
    """Give a unit a standing order, or clear one.

    One tool for all of them because they are the same kind of decision, and the invalid combinations
    are refused with the reason: civilians cannot fortify, only workers can be automated, only siege
    units set up. Each refusal names what to use instead.
    """
    from . import automation, workers, units
    from . import unique_types as U
    u = _own_unit(g, pid, unit_id)
    ud = g.udef(u)
    order = order.lower().strip()
    if order == "fortify":
        if not ud["_military"]:
            raise ActionError("Civilians cannot fortify; use sleep.")
        u.activity, u.goto = "fortify", None
        return {"ok": True, "note": "Fortification builds up by 20% per turn (max 40%) while the unit stays put."}
    if order in ("sleep", "heal"):
        u.activity, u.goto = order, None
        return {"ok": True}
    if order in ("wake", "cancel"):
        u.activity, u.goto = None, None
        return {"ok": True}
    if order == "skip":
        u.moves = 0
        return {"ok": True}
    if order == "explore":
        if not ud["_military"] and ud["_domain"] == "Land":
            raise ActionError("Civilians cannot explore.")
        u.goto = None
        res = automation.explore(g, u)
        _refresh(g)
        return res
    if order == "automate":
        if not units.unit_uniques(g, u, U.BuildImprovements):
            raise ActionError("Only units that build improvements (Workers) can be automated.")
        res = automation.automate_worker(g, u)
        _refresh(g)
        return res
    if order == "pillage":
        res = workers.pillage(g, u)
        _refresh(g)
        return res
    if order == "setup":
        if not units.unit_has(g, u, U.MustSetUp):
            raise ActionError("This unit does not need to set up.")
        if "Set Up" in u.status:
            return {"ok": True, "note": "Already set up."}
        if u.moves <= 0:
            raise ActionError("No movement left.")
        u.moves = max(0, u.moves - g.rules.move_scale)
        u.status.append("Set Up")
        return {"ok": True}
    if order == "disband":
        res = units.disband(g, u)
        _refresh(g)
        return res
    raise ActionError("Unknown order. Use fortify, sleep, wake, skip, heal, explore, automate, pillage, setup, disband, "
                      "cancel.")


@tool("upgrade_unit", "Upgrade an obsolete unit (costs gold; must be in your territory with moves left).",
      {"unit_id": INT}, ["unit_id"], category="unit")
def upgrade_unit(g: Game, pid: int, unit_id: int):
    """Upgrade an obsolete unit, for gold, in friendly territory."""
    from . import units
    return units.upgrade(g, _own_unit(g, pid, unit_id))


@tool("promote_unit", "Choose a promotion for a unit that has enough XP (see get_unit for options).",
      {"unit_id": INT, "promotion": STR}, ["unit_id", "promotion"], category="unit")
def promote_unit(g: Game, pid: int, unit_id: int, promotion: str):
    """Spend accumulated experience on a promotion.

    The name is resolved through the ruleset first, so a near miss - a lower-case name, a missing space -
    finds the promotion rather than failing.
    """
    from . import units
    u = _own_unit(g, pid, unit_id)
    name = g.rules.resolve("promotion", promotion) or promotion
    units.promote(g, u, name)
    return {"promotions": u.promotions}


# ============================================================================
# CITY ACTIONS
# ============================================================================
@tool("set_production", "Set what a city builds (unit, building, wonder, project, or Gold/Science conversion). "
      "append=true adds to the end of the queue instead of replacing the current item. Stored production carries over.",
      {"city_id": INT, "item": STR, "append": BOOL}, ["city_id", "item"], category="city")
def set_production(g: Game, pid: int, city_id: int, item: str, append: bool = False):
    """Set or queue what a city builds."""
    from . import cities
    return cities.set_production(g, _own_city(g, pid, city_id), item, append=append)


@tool("change_queue", "Edit a city's production queue: move the entry at position index (0 = in production) up, "
      "down, first or last, remove it, or clear the whole queue.",
      {"city_id": INT, "index": INT, "action": {"type": "string", "enum": ["up", "down", "first", "last", "remove", "clear"]}},
      ["city_id", "action"], category="city")
def change_queue(g: Game, pid: int, city_id: int, action: str, index: int = 0):
    """Reorder or trim a city's production queue."""
    from . import cities
    return cities.change_queue(g, _own_city(g, pid, city_id), index, action)


@tool("set_auto_production", "Turn automatic production on or off for a city: when its queue runs empty, the built-in "
      "advisor picks the next item.", {"city_id": INT, "enabled": BOOL}, ["city_id", "enabled"], category="city")
def set_auto_production(g: Game, pid: int, city_id: int, enabled: bool):
    """Hand a city's production choices to the built-in advisor when its queue empties.

    Turning it on with an empty queue picks something immediately, rather than leaving the city idle
    for a turn waiting for the condition to occur.
    """
    from . import cities
    c = _own_city(g, pid, city_id)
    c.auto_production = bool(enabled)
    picked = cities.auto_pick_production(g, c) if c.auto_production and not c.queue else None
    return {"city": c.name, "auto_production": c.auto_production, **({"started": picked} if picked else {})}


@tool("buy", "Buy a unit or building in a city immediately. currency: Gold (default) or Faith (religious units, "
      "some buildings and, with the right beliefs or policies, other items). Puppets cannot buy.",
      {"city_id": INT, "item": STR, "currency": STR}, ["city_id", "item"], category="city")
def buy(g: Game, pid: int, city_id: int, item: str, currency: str = "Gold"):
    """Buy a unit or building outright with gold or faith."""
    from . import cities
    cur = {"gold": "Gold", "faith": "Faith"}.get(str(currency).lower())
    if cur is None:
        raise ActionError("currency must be Gold or Faith.")
    res = cities.purchase(g, _own_city(g, pid, city_id), item, cur)
    _refresh(g)
    return res


@tool("set_city_focus", "Citizen focus: balanced, food, production, gold, science, culture, faith, happiness, "
      "gold_growth, production_growth, or manual (keep your tile and specialist choices). avoid_growth=true stops the "
      "city from growing.", {"city_id": INT, "focus": STR, "avoid_growth": BOOL}, ["city_id"], category="city")
def set_city_focus(g: Game, pid: int, city_id: int, focus: Optional[str] = None, avoid_growth: Optional[bool] = None):
    """Set what a city's citizens optimise for, and whether it should avoid growing.

    Citizens are reassigned immediately, so the caller sees the consequence of the choice in the same
    response rather than next turn.
    """
    from . import cities
    c = _own_city(g, pid, city_id)
    if focus is not None:
        if focus not in cities.FOCUSES:
            raise ActionError(f"Focus must be one of {', '.join(cities.FOCUSES)}.")
        c.focus = focus
        c.manual_specialists = focus == "manual" and c.manual_specialists
    if avoid_growth is not None:
        c.avoid_growth = bool(avoid_growth)
    cities.assign_citizens(g, c)
    g.invalidate()
    return {"focus": c.focus, "avoid_growth": c.avoid_growth, "worked_tiles": [list(g.grid.xy(i)) for i in c.worked],
            "specialists": c.specialists}


@tool("work_tile", "Lock (or unlock) a citizen onto a specific tile of a city.",
      {"city_id": INT, **XY, "locked": BOOL}, ["city_id", "x", "y"], category="city")
def work_tile(g: Game, pid: int, city_id: int, x: int, y: int, locked: bool = True):
    """Lock a citizen onto a tile, or release one.

    The lock list is trimmed to the city's population: locking more tiles than there are citizens is a
    request that cannot be honoured, and silently keeping the oldest would be the wrong half.
    """
    from . import cities
    c = _own_city(g, pid, city_id)
    idx = _idx(g, x, y)
    if locked:
        if idx not in cities.workable_tiles(g, c):
            raise ActionError("That tile is not workable by this city.")
        if idx not in c.locked:
            c.locked.append(idx)
            c.locked = c.locked[-c.pop:]
    else:
        c.locked = [i for i in c.locked if i != idx]
    cities.assign_citizens(g, c)
    g.invalidate()
    return {"locked_tiles": [list(g.grid.xy(i)) for i in c.locked], "worked_tiles": [list(g.grid.xy(i)) for i in c.worked]}


@tool("set_specialists", "Assign specialists manually, e.g. {\"Scientist\": 2, \"Engineer\": 1} (limited by the "
      "city's buildings; the remaining citizens work tiles). Pass {} to return to automatic specialists.",
      {"city_id": INT, "specialists": {"type": "object"}}, ["city_id", "specialists"], category="city")
def set_specialists(g: Game, pid: int, city_id: int, specialists: dict):
    """Assign specialists by hand, or pass an empty object to go back to automatic.

    Every slot is checked against the city's buildings and population before anything is changed, so a
    partly-valid request changes nothing rather than applying the half that fits.
    """
    from . import cities
    c = _own_city(g, pid, city_id)
    if not isinstance(specialists, dict):
        raise ActionError("specialists must be an object like {\"Scientist\": 1}.")
    maxs = cities.max_specialists(g, c)
    clean = {}
    for k, v in specialists.items():
        name = g.rules.resolve("specialist", k) or k
        if name not in maxs:
            raise ActionError(f"{c.name} has no slots for {k}. Available: {maxs or 'none'}.")
        n = int(v)
        if n < 0 or n > maxs[name]:
            raise ActionError(f"{c.name} has {maxs[name]} {name} slot(s).")
        if n:
            clean[name] = n
    if sum(clean.values()) > c.pop:
        raise ActionError(f"{c.name} has only {c.pop} citizens.")
    c.specialists = clean
    c.manual_specialists = bool(clean)
    cities.assign_citizens(g, c)
    g.invalidate()
    return {"specialists": c.specialists, "worked_tiles": [list(g.grid.xy(i)) for i in c.worked]}


@tool("buy_tile", "Buy an unowned tile next to a city's borders (within 3 tiles of the city) with gold.",
      {"city_id": INT, **XY}, ["city_id", "x", "y"], category="city")
def buy_tile(g: Game, pid: int, city_id: int, x: int, y: int):
    """Buy an unowned tile near one of the caller's cities."""
    from . import cities
    res = cities.buy_tile(g, _own_city(g, pid, city_id), _idx(g, x, y))
    _refresh(g)
    return res


@tool("city_attack", "A city bombards an enemy unit within range (once per turn).", {"city_id": INT, **XY},
      ["city_id", "x", "y"], category="city")
def city_attack(g: Game, pid: int, city_id: int, x: int, y: int):
    """Bombard an enemy in range from a city. Once per turn, and free.

    The most commonly forgotten action in the game, which is why the turn-progress note reminds agents
    that a city still has its attack.
    """
    from . import combat
    return combat.city_bombard(g, _own_city(g, pid, city_id), _idx(g, x, y))


@tool("rename_city", "Rename one of your cities.", {"city_id": INT, "name": STR}, ["city_id", "name"], any_time=True,
      category="city")
def rename_city(g: Game, pid: int, city_id: int, name: str):
    """Rename a city, refusing a rename that changes nothing.

    Renaming to the existing name is almost always a confused agent repeating itself, and telling it so
    is more useful than a cheerful success it will not learn from.
    """
    from . import cities
    c = _own_city(g, pid, city_id)
    old = c.name
    new = cities.rename_city(g, c, name)
    if new == old:
        raise ActionError(f"The city is already named {old}.")
    g.emit("city_renamed", f"{old} is now called {new}.", [pid], idx=c.idx)
    return {"name": new}


@tool("city_status", "Decide what to do with a conquered city: annex (full control; unhappiness until a Courthouse), "
      "puppet (keeps its own production, lower unhappiness), raze (burn it down 1 population per turn; not original "
      "capitals or holy cities), stop_razing, or liberate (return it to its original owner for their gratitude).",
      {"city_id": INT, "status": {"type": "string", "enum": ["annex", "puppet", "raze", "stop_razing", "liberate"]}},
      ["city_id", "status"], category="city")
def city_status(g: Game, pid: int, city_id: int, status: str):
    """Decide what to do with a conquered city: annex, puppet, raze, stop razing, or liberate."""
    from . import conquest
    c = _own_city(g, pid, city_id)
    res = {"annex": lambda: conquest.annex(g, pid, c), "puppet": lambda: conquest.puppet(g, pid, c),
           "raze": lambda: conquest.raze(g, pid, c), "stop_razing": lambda: conquest.raze(g, pid, c, stop=True),
           "liberate": lambda: conquest.liberate(g, pid, c)}.get(status)
    if res is None:
        raise ActionError("status must be annex, puppet, raze, stop_razing or liberate.")
    out = res()
    _refresh(g)
    return out


# ============================================================================
# EMPIRE
# ============================================================================
@tool("set_research", "Research a technology. If it is not available yet, it becomes your goal and prerequisites are "
      "researched automatically in order. append=true adds it to the end of your research queue instead of "
      "replacing the queue.", {"tech": STR, "append": BOOL}, ["tech"], category="empire")
def set_research(g: Game, pid: int, tech: str, append: bool = False):
    """Choose what to research, or set a distant technology as a goal.

    A technology that is not yet available becomes a goal and its prerequisites are researched in order,
    which means a caller can name what it wants rather than planning the path.
    """
    from . import research
    return research.set_research(g, pid, tech, append=bool(append))


@tool("dequeue_research", "Remove a technology from your research queue, together with any queued technology that "
      "needs it.", {"tech": STR}, ["tech"], category="empire")
def dequeue_research(g: Game, pid: int, tech: str):
    """Take a technology (and whatever queued depends on it) off the research queue."""
    from . import research
    return research.dequeue_research(g, pid, tech)


@tool("choose_free_tech", "Pick a free technology you have been granted (it must be researchable now).",
      {"tech": STR}, ["tech"], category="empire")
def choose_free_tech(g: Game, pid: int, tech: str):
    """Spend a granted free technology."""
    from . import research
    return research.free_tech(g, pid, tech)


@tool("adopt_policy", "Adopt a social policy or open a policy branch with your culture (or a free policy). "
      "get_policies lists the costs and what is adoptable.", {"policy": STR}, ["policy"], category="empire")
def adopt_policy(g: Game, pid: int, policy: str):
    """Adopt a social policy or open a branch, resolving the name through the ruleset."""
    from . import policies
    name = g.rules.resolve("policy", policy) or policy
    return policies.adopt(g, pid, name)


@tool("found_pantheon", "Found a pantheon with enough faith by choosing a pantheon belief (see get_religion).",
      {"belief": STR}, ["belief"], category="empire")
def found_pantheon(g: Game, pid: int, belief: str):
    """Found a pantheon by choosing a belief."""
    from . import religion
    return religion.found_pantheon(g, pid, belief)


@tool("choose_great_person", "Choose a free Great Person you have been granted (e.g. Great Scientist).",
      {"great_person": STR}, ["great_person"], category="empire")
def choose_great_person(g: Game, pid: int, great_person: str):
    """Take a Great Person that has been granted for free."""
    from . import great_people
    return great_people.choose_free(g, pid, great_person)


@tool("un_vote", "Vote in the United Nations world leader election (player id of a civilization, or 'abstain'). "
      "Voting opens the turn before the vote.", {"candidate": {"type": ["integer", "string"]}}, ["candidate"],
      any_time=True, category="diplomacy")
def un_vote(g: Game, pid: int, candidate):
    """Cast a United Nations vote, or abstain.

    Usable out of turn: the vote opens the turn before it is counted, and requiring a player to be on
    their own turn to take part in a global vote would be an artefact of the implementation.
    """
    from . import victory
    return victory.cast_vote(g, pid, candidate)


@tool("set_civ_name", "Name your civilization (and optionally your leader). Anything you like; your civilization's "
      "bonuses do not change.", {"name": STR, "leader": STR}, ["name"], any_time=True, category="empire")
def set_civ_name(g: Game, pid: int, name: str, leader: Optional[str] = None):
    """Name the civilization and optionally its leader.

    Purely cosmetic - the civilization's bonuses do not change - but names must stay unique or the
    diplomacy screens and every message become ambiguous. Renaming to the current name is refused for
    the same reason as renaming a city to itself.
    """
    from .cities import clean_name
    name = clean_name(name, 48)
    if not name:
        raise ActionError("Name cannot be empty (plain text only).")
    if any(p.name.lower() == name.lower() and p.id != pid for p in g.s.players):
        raise ActionError("Another civilization already uses that name.")
    p = g.player(pid)
    old = p.name
    if old == name and (not leader or clean_name(leader, 48) == p.leader):
        raise ActionError(f"Your civilization is already named {name}.")
    p.name = name
    if leader:
        p.leader = clean_name(leader, 48)
    if old != name:
        g.emit("civ_renamed", f"{old} is now known as {name}" + (f", led by {p.leader}" if p.leader else "") + ".", None,
               mentions={old: pid}, player=pid)
    return {"name": p.name, "leader": p.leader}


# ============================================================================
# DIPLOMACY
# ============================================================================
@tool("send_message", "Send a free-text message to a civilization you have met (player id) or 'all' you have met. "
      "Nothing said is binding.", {"to": {"type": ["integer", "string"]}, "text": STR}, ["to", "text"], any_time=True,
      category="diplomacy")
def send_message(g: Game, pid: int, to, text: str):
    """Send free text to one civilization, or to everyone the caller has met. Nothing said is binding."""
    from . import diplomacy
    return diplomacy.send_message(g, pid, to, text)


@tool("open_negotiation", "Start a negotiation (on your turn) with a met civilization: a message plus an optional "
      "concrete proposal (give = what you give, receive = what you want). Mutual agreements (peace, friendship, "
      "research agreement, defensive pact) go on both sides automatically. They reply (accept, reject, counter, or "
      "reply) and you go back and forth until a deal or rejection.",
      {"to": INT, "message": STR, "give": ITEMS, "receive": ITEMS}, ["to", "message"], category="diplomacy")
def open_negotiation(g: Game, pid: int, to: int, message: str, give=None, receive=None):
    """Start a negotiation: a message plus an optional concrete proposal.

    The other side answers before play continues, which is what makes diplomacy in CITAR worth probing -
    it is a real exchange rather than a pair of one-way announcements.
    """
    from . import diplomacy
    return diplomacy.open_negotiation(g, pid, to, message, give, receive)


@tool("respond_negotiation", "Respond when it is your move in a negotiation. action: accept (the other side's current "
      "proposal), reject (end it), counter (new proposal via give/receive from your perspective, plus message), or reply "
      "(message only).", {"negotiation_id": INT, "action": STR, "message": STR, "give": ITEMS, "receive": ITEMS},
      ["negotiation_id", "action"], any_time=True, category="diplomacy")
def respond_negotiation(g: Game, pid: int, negotiation_id: int, action: str, message: Optional[str] = None,
                        give=None, receive=None):
    """Accept, reject, counter or reply in a negotiation that is waiting on the caller.

    Usable out of turn, because a negotiation opened on somebody else's turn blocks their turn until it
    is answered. An agent that only checks its own turn would deadlock the game.
    """
    from . import diplomacy
    return diplomacy.respond_negotiation(g, pid, negotiation_id, action, message, give, receive)


@tool("declare_war", "Declare war on a civilization or city-state you have met (breaks deals and agreements; allies, "
      "defensive pacts and city-state allies may join).", {"player_id": INT, "message": STR}, ["player_id"],
      category="diplomacy")
def declare_war(g: Game, pid: int, player_id: int, message: Optional[str] = None):
    """Declare war, with everything that follows: broken deals, allies and defensive pacts."""
    from . import diplomacy
    res = diplomacy.declare_war(g, pid, player_id, message)
    _refresh(g)
    return res


@tool("denounce", "Publicly denounce a civilization (ends friendship; others take note).", {"player_id": INT},
      ["player_id"], category="diplomacy")
def denounce(g: Game, pid: int, player_id: int):
    """Publicly denounce a civilization."""
    from . import diplomacy
    return diplomacy.denounce(g, pid, player_id)


@tool("city_state_action", "Interact with a city-state: gift_gold (amount), gift_unit (unit_id; the unit must be in or "
      "next to its territory), pledge (protection), withdraw (protection), tribute_gold or tribute_worker (demand; "
      "they must fear you), make_peace, or marry (annex a long-time ally with gold, if your civilization can).",
      {"player_id": INT, "action": STR, "amount": INT, "unit_id": INT}, ["player_id", "action"], category="diplomacy")
def city_state_action(g: Game, pid: int, player_id: int, action: str, amount: Optional[int] = None,
                      unit_id: Optional[int] = None):
    """Every way of dealing with a city-state, behind one tool.

    Gifts, protection, tribute, peace and marriage are one topic from the caller's point of view, and
    splitting them into eight tools would make the list longer without making any of them clearer. The
    final refusal lists the valid actions.
    """
    from . import city_states, diplomacy
    cs = _city_state(g, pid, player_id)
    a = action.lower()
    if a == "gift_gold":
        if not amount:
            raise ActionError("Give an amount of gold.")
        return city_states.gift_gold(g, pid, cs.id, amount)
    if a == "gift_unit":
        if unit_id is None:
            raise ActionError("Give the unit_id to gift.")
        return city_states.gift_unit(g, pid, _own_unit(g, pid, unit_id))
    if a == "pledge":
        return city_states.pledge(g, pid, cs.id)
    if a == "withdraw":
        return city_states.withdraw_protection(g, pid, cs.id)
    if a in ("tribute_gold", "tribute"):
        return city_states.demand_tribute(g, pid, cs.id, worker=False)
    if a == "tribute_worker":
        return city_states.demand_tribute(g, pid, cs.id, worker=True)
    if a == "make_peace":
        return diplomacy.make_peace_with_city_state(g, pid, cs.id)
    if a == "marry":
        return city_states.buyout(g, pid, cs.id)
    raise ActionError("action must be gift_gold, gift_unit, pledge, withdraw, tribute_gold, tribute_worker, "
                      "make_peace or marry.")


@tool("move_spy", "Send a spy to a city you have explored (foreign city: steal technology; city-state capital: rig "
      "elections; your own city: counter-intelligence), or to 'hideout'. It arrives next turn and needs a few turns "
      "to establish a network.", {"spy": STR, "city_id": {"type": ["integer", "string"]}}, ["spy", "city_id"],
      category="diplomacy")
def move_spy(g: Game, pid: int, spy: str, city_id):
    """Send a spy to a city, or recall it to the hideout."""
    from . import espionage
    return espionage.move_spy(g, pid, spy, city_id)


@tool("stage_coup", "Order a spy in a city-state capital to stage a coup at the end of your turn (on success you "
      "become their ally; on failure the spy dies).", {"spy": STR}, ["spy"], category="diplomacy")
def stage_coup(g: Game, pid: int, spy: str):
    """Order a spy to attempt a coup in a city-state capital at the end of this turn."""
    from . import espionage
    return espionage.stage_coup(g, pid, spy)


# ============================================================================
# META
# ============================================================================
@tool("write_notes", "Write to your private strategy notebook (kept across turns; shown in your briefing). "
      "mode: replace (default) or append.", {"text": STR, "mode": STR}, ["text"], any_time=True, category="meta")
def write_notes(g: Game, pid: int, text: str, mode: str = "replace"):
    """Write to the caller's private notebook, replacing or appending.

    The notebook is the only memory an agent has that is not reconstructed from the game each turn, and
    it appears in the briefing. It is capped at 8,000 characters, keeping the *end*: notes grow by
    appending, so the recent half is the useful half.
    """
    p = g.player(pid)
    if mode == "append":
        p.notes = (p.notes + "\n" + text).strip()
    else:
        p.notes = text
    p.notes = p.notes[-8000:]
    return {"notes_length": len(p.notes)}


@tool("log_thought", "Record your reasoning for this turn (shown to spectators and in the game replay, never to other "
      "players).", {"text": STR}, ["text"], any_time=True, category="meta")
def log_thought(g: Game, pid: int, text: str):
    """Record the caller's reasoning for this turn.

    Not visible to other players - it goes to spectators and the replay. Listener failures are swallowed
    on purpose: a browser that has disconnected mid-turn must not be able to fail a player's turn.
    """
    g.s.thoughts.append({"turn": g.turn, "player": pid, "text": str(text)[:4000]})
    for fn in list(g.listeners):
        try:
            fn({"id": 0, "turn": g.turn, "type": "thought", "text": str(text)[:4000], "players": [], "idx": None,
                "data": {"player": pid}})
        except Exception:
            pass
    return {"ok": True}


@tool("end_turn", "End your turn. Units with standing orders keep executing them.", category="turn")
def end_turn(g: Game, pid: int):
    """End the caller's turn and report whose it is now."""
    g.end_turn(pid)
    return {"ended": True, "next_player": g.player(g.s.current).name, "turn": g.turn, "phase": g.s.phase}
