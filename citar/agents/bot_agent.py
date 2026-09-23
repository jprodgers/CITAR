"""Adapter that lets the scripted BasicBot occupy a seat in a live session."""
from __future__ import annotations

import time

from ..bots import profiles


class BotAgent:
    """Wraps the scripted bot in the same interface a model uses.

    So that a bot seat and a model seat are driven identically by the session: same turn loop, same
    negotiation handling, same metrics. A benchmark comparing a model against the bot is then comparing
    two players of the same game rather than two code paths.
    """
    def __init__(self, aggression: float = 0.4, profile: str = None, seed: int = None):
        # the profile decides what plays; the seat's aggression applies when the profile leaves it open
        ref = profile or profiles.DEFAULT_PROFILE
        try:
            self.profile = profiles.resolve(ref)
        except profiles.ProfileError:            # deleted since the game was set up: play the standard bot
            ref = profiles.DEFAULT_PROFILE
            self.profile = profiles.resolve(ref)
        self.bot = profiles.make_bot(ref, seed=seed, aggression=float(aggression) if aggression is not None else None)

    def _bind(self, session, pid):
        """Give the bot a way to call tools against this session."""
        def ex(g, p, _tool, **args):
            """Execute one tool call on the bot's behalf."""
            res = session.call_tool(p, _tool, args)
            return res["result"] if res["ok"] else None
        self.bot.ex = ex

    def play_turn(self, session, pid: int):
        """Play the bot's turn."""
        self._bind(session, pid)
        with session.lock:
            g = session.game
            if g.s.current != pid:
                return
            self.bot.play_turn(g, pid, end_turn=False)
        # give counterparts a chance to answer negotiations we opened
        deadline = time.time() + 90
        while time.time() < deadline:
            with session.lock:
                g = session.game
                mine = [n for n in g.s.negotiations if n["status"] == "open" and pid in (n["initiator"], n["responder"])]
                if not mine:
                    break
                for n in mine:
                    if n["awaiting"] == pid:
                        before = len(n["history"])
                        self.bot.respond(g, pid, n["id"])
                        if n["status"] == "open" and n["awaiting"] == pid and len(n["history"]) == before:
                            # the bot's answer was refused and nothing moved: end it rather than stall the game
                            session.call_tool(pid, "respond_negotiation",
                                              {"negotiation_id": n["id"], "action": "reject",
                                               "message": "We have nothing further to discuss."})
                session.cond.wait(timeout=1.0)

    def respond_negotiation(self, session, pid: int, nid: int):
        """Answer a negotiation as the bot."""
        self._bind(session, pid)
        with session.lock:
            self.bot.respond(session.game, pid, nid)
