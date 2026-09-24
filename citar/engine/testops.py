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
