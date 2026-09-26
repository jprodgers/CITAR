"""The idle bot: founds its capital and then does nothing.

A stand-in for a player that neglects the game, used as a control (what is the real bot worth against nobody?) and
as a punching bag in conquest tests. It lives with the bots rather than with the balance runner because it plays
through the engine directly, as every bot does.
"""
from __future__ import annotations

from ..engine import tools
from ..engine.game import ActionError


class IdleBot:
    """Founds its capital and then does nothing: a stand-in for a player that neglects the game (tests conquest)."""

    def play_turn(self, g, pid, end_turn=True):
        """Do nothing and end the turn. A control for measuring what the real bot is worth."""
        for u in g.player_units(pid):
            if not g.player_cities(pid):
                try:
                    tools.execute(g, pid, "unit_action", {"unit_id": u.id, "action": "found_city"})
                except ActionError:
                    pass

    def respond(self, g, pid, nid):
        """Refuse every negotiation."""
        try:
            tools.execute(g, pid, "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                          "message": "We are not interested."})
        except ActionError:
            pass
