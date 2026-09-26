"""Headless all-bot games: no session, no threads, a bot in every seat.

The loop the lab, the balance runner and ``citar sim`` play, reached through :func:`citar.engine_api.run_game`.
It lives beside the bots because it hands them the live game, which nothing outside the engine may hold.
"""
from __future__ import annotations

import traceback
from typing import Callable, Optional

from ..engine.game import Game


def resolve_negotiations(g: Game, bots: dict, max_rounds: int = 40):
    """Answer every open negotiation, so a headless game cannot stall on one."""
    for _ in range(max_rounds):
        pending = [n for n in g.s.negotiations if n["status"] == "open" and n["awaiting"] in bots]
        if not pending:
            return
        for n in pending:
            bots[n["awaiting"]].respond(g, n["awaiting"], n["id"])


def play(config: dict, bots: dict, on_turn: Optional[Callable[[dict], None]] = None,
         on_event: Optional[Callable[[dict], None]] = None, max_errors: int = 20, traceback_limit: int = 5,
         labels: Optional[dict] = None, raise_errors: bool = False) -> dict:
    """Play a new game to its end with a bot in each seat; see citar.engine_api.run_game for the arguments.

    A bot that raises loses the rest of its turn, not the game: the error is recorded and play goes on, until more
    than ``max_errors`` have piled up, which means something is broken rather than unlucky. With ``raise_errors``
    the first one propagates instead, for a run that exists to show the bot does not crash.
    """
    g = Game.new(config)
    if on_event is not None:
        g.listeners.append(on_event)
    errors: list[str] = []
    shown = [g.turn]

    def turn_changed(first: bool = False):
        """Tell on_turn about a turn it has not seen yet."""
        if on_turn is not None and (first or g.turn != shown[0]):
            shown[0] = g.turn
            on_turn({"turn": g.turn, "phase": g.s.phase, "turn_limit": g.s.config.get("turn_limit"),
                     "last_stats": g.s.stats[-1] if g.s.stats else None})

    turn_changed(first=True)
    while g.s.phase == "playing":
        turn_changed()
        pid = g.s.current
        if pid in bots:
            try:
                bots[pid].play_turn(g, pid, end_turn=False)
            except Exception as e:
                if raise_errors:
                    raise
                label = f" {labels.get(pid)}" if labels is not None else ""
                errors.append(f"T{g.turn} P{pid}{label}: {type(e).__name__}: {e}\n"
                              f"{traceback.format_exc(limit=traceback_limit)}")
                if len(errors) > max_errors:
                    break
            resolve_negotiations(g, bots)
        if g.s.phase == "playing" and g.s.current == pid:
            g.end_turn(pid)
    turn_changed()
    return result(g, errors)


def result(g: Game, errors: list) -> dict:
    """What a finished headless game reports: how it ended, the per-turn stats, and each civilization's standing."""
    from ..engine.victory import score
    players = []
    for p in g.s.players:
        row = {"id": p.id, "kind": p.kind, "name": p.name, "alive": p.alive}
        if p.kind == "major":
            ship = g.s.spaceship.get(p.id)
            row.update({"difficulty": p.difficulty, "techs": len(p.techs), "future_techs": p.future_techs,
                        "policies": len(p.policies), "religion": p.religion_state,
                        "great_people": p.great_people_earned, "cities": len(g.player_cities(p.id)),
                        "spaceship": dict(ship) if isinstance(ship, dict) else ship,
                        # an eliminated civ scores nothing, whatever it had built
                        "score": score(g, p.id)["total"] if p.alive else 0})
        players.append(row)
    return {"turn": g.turn, "turns": g.turn - 1, "phase": g.s.phase, "winner": g.s.winner, "victory": g.s.victory,
            "turn_limit": g.s.config.get("turn_limit"), "stats": g.s.stats, "players": players, "errors": errors}
