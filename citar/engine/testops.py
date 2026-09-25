"""Test operations: what a rule script does to a game that no player or editor may.

They replace the internals Python's tests poked (``g.remove_unit``, ``p.auto[...] = ...``, ``g.s.turn = ...``), so
that a script means the same on both engines. The Rust engine's side is ``crates/citar-engine/src/api/testops.rs``,
and ``tests/rules/README.md`` documents each. Reached through ``EngineGame.test_ops`` (not in the facade's
``__all__``: tests only), which also does ``reload``, since that replaces the game.

Only the operations the Rust engine has ported are here: a script using another could not pass on both.
"""
from __future__ import annotations

from typing import Callable

from .game import ActionError, Game
from .state import AUTO_DECISIONS, BOT_MANAGED, HUMANLIKE_CONTROLLERS

OPS: dict[str, tuple[Callable, str]] = {}


def op(name: str, doc: str):
    """Register a test operation with its parameters."""
    def deco(fn):
        """Record the operation and its parameters."""
        OPS[name] = (fn, doc)
        return fn
    return deco


def ops_help() -> list[dict]:
    """Every test operation with its parameters, sorted by name (``reload`` included, which EngineGame does)."""
    rows = [{"op": k, "params": doc} for k, (_, doc) in OPS.items()]
    rows.append({"op": "reload", "params": "the game is saved and the save loaded, as a host does"})
    return sorted(rows, key=lambda r: r["op"])


def names() -> set:
    """The test operations' names."""
    return set(OPS) | {"reload"}


@op("clear_units", "player (id, list of ids, or 'all' for every player, the barbarians included): every unit of "
                   "those players is removed")
def _clear_units(g: Game, o: dict):
    """Remove every unit of the players named."""
    from .scenario import _players
    v = o.get("player")
    pids = [p.id for p in g.s.players] if v == "all" else _players(g, v, majors_only=False)
    removed = sorted(u.id for pid in pids for u in g.player_units(pid))
    for uid in removed:
        u = g.unit(uid)
        if u is not None:
            g.remove_unit(u)
    return {"removed": removed}


@op("set_turn", "turn (1 or more): the game's turn number")
def _set_turn(g: Game, o: dict):
    """Set the turn number."""
    try:
        turn = int(o.get("turn"))
    except (TypeError, ValueError):
        turn = 0
    if turn < 1:
        raise ActionError("turn must be a whole number, 1 or more.")
    g.s.turn = turn
    g.invalidate()
    return {"turn": turn}


@op("unmeet", "a, b: the two no longer know each other")
def _unmeet(g: Game, o: dict):
    """Two players forget they have met."""
    from .scenario import _pid
    a, b = _pid(g, o.get("a"), majors_only=False), _pid(g, o.get("b"), majors_only=False)
    if a == b:
        raise ActionError("A player cannot forget itself.")
    for x, y in ((a, b), (b, a)):
        met = g.player(x).met
        if y in met:
            met.remove(y)
    g.invalidate()
    return {}


@op("set_controller", "player, controller; optional handicap, auto: hands the seat to another driver")
def _set_controller(g: Game, o: dict):
    """Hand a seat to another driver, with the host's checks of the handicap and the automatic decisions."""
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    controller = o.get("controller")
    if controller not in HUMANLIKE_CONTROLLERS + BOT_MANAGED:
        raise ActionError(f"Unknown controller {controller!r}.")
    try:
        g.player(pid).set_controller(controller, o.get("handicap"), o.get("auto"))
    except ValueError as e:
        raise ActionError(str(e))
    g.invalidate()
    return {}


@op("set_auto", "player, decision (un_vote, conquest or free_picks), on (bool): the engine takes the decision for "
                "the civilization, or not, until its controller changes")
def _set_auto(g: Game, o: dict):
    """Change one automatic decision of a seat for now, as play does."""
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    decision = o.get("decision")
    if decision not in AUTO_DECISIONS:
        raise ActionError(f"Unknown decision {decision!r}.")
    if not isinstance(o.get("on"), bool):
        raise ActionError("on must be true or false.")
    g.player(pid).auto[decision] = o["on"]
    g.invalidate()
    return {}


def _clock(g: Game) -> dict:
    """Where the game is in time, as the turn operations report it."""
    return {"turn": g.turn, "current": g.s.current}


@op("end_turn", "optional player (the current one by default): that player's turn ends, and play moves on to the "
                "next major civilization's")
def _end_turn(g: Game, o: dict):
    """End a player's turn, the current one's unless another is named, as the host does."""
    from .scenario import _pid
    pid = g.s.current if o.get("player") is None else _pid(g, o.get("player"), majors_only=False)
    g.end_turn(pid)
    return _clock(g)


@op("end_round", "every remaining turn of the round ends, and the round with them")
def _end_round(g: Game, o: dict):
    """End every turn left in the round, and the round."""
    start = g.turn
    while g.s.phase == "playing" and g.turn == start:
        g.end_turn(g.s.current)
    return _clock(g)


@op("force_turn", "player: it is that player's turn now, started")
def _force_turn(g: Game, o: dict):
    """Make it a player's turn now and start it (EngineGame.force_turn).

    Refused, as the Rust engine refuses it, for a player who has been eliminated and in a game that is over, which
    EngineGame.force_turn allowed (tests/rules/intended.toml: force-turn-only-for-the-living).
    """
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    if not g.player(pid).alive:
        raise ActionError(f"{g.player(pid).name} has been eliminated and plays no turns.")
    if g.s.phase != "playing":
        raise ActionError("The game is over.")
    if g.s.current != pid:
        g.s.current = pid
        g.s.turn_started = False
        g.begin_turn()
    return _clock(g)


@op("complete_construction", "city: what it is building completes now")
def _complete_construction(g: Game, o: dict):
    """Finish what a city builds now, whatever production it has stored, as its turn would."""
    from . import cities
    try:
        c = g.city(int(o.get("city")))
    except (TypeError, ValueError):
        c = None
    if c is None:
        raise ActionError("No such city.")
    name = cities.current_construction(c)
    if name is None or name in cities.PERPETUAL:
        raise ActionError(f"{c.name} is building nothing that completes.")
    if not cities.complete_construction(g, c, name):
        raise ActionError("No room to place the unit.")
    return {"completed": name}


@op("refresh_visibility", "what every civilization sees is brought up to date")
def _refresh_visibility(g: Game, o: dict):
    """Bring what everyone sees up to date."""
    from . import visibility
    visibility.refresh(g, force=True)
    return {}


def apply_one(g: Game, n: int, o) -> dict:
    """Run the ``n``-th test operation of a list; the error names it, as apply_ops' does."""
    if not isinstance(o, dict) or o.get("op") not in OPS:
        what = o.get("op") if isinstance(o, dict) else o
        raise ActionError(f"Test operation {n}: unknown op {what!r}. Known: {', '.join(sorted(names()))}.")
    try:
        return OPS[o["op"]][0](g, o) or {}
    except ActionError as e:
        raise ActionError(f"Test operation {n} ({o['op']}): {e}")
