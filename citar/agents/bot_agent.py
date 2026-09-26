"""Adapter that lets the scripted BasicBot occupy a seat in a live session."""
from __future__ import annotations

import time

from .. import engine_api
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

    @staticmethod
    def _executor(session):
        """How the bot's tool calls reach the game: through the session, so each one is recorded like a model's."""
        def ex(p, tool, args):
            """Execute one tool call on the bot's behalf; None when it is refused."""
            res = session.call_tool(p, tool, args)
            return res["result"] if res["ok"] else None
        return ex

    def play_turn(self, session, pid: int):
        """Play the bot's turn."""
        ex = self._executor(session)
        g = session.game
        with session.lock:
            if g.current != pid:
                return
            g.play_bot_turn(pid, self.bot, end_turn=False, execute=ex)
        # give counterparts a chance to answer negotiations we opened; what is still open after this the driver
        # closes before it ends the turn
        deadline = time.time() + 90
        while time.time() < deadline and not session.stopped:
            with session.lock:
                mine = [n for n in g.open_negotiations(pid) if self._owns(n)]
                if not mine:
                    break
                for nid in [n["id"] for n in mine]:
                    n = g.negotiation_head(nid)     # as it stands now: an earlier answer may have settled it
                    if n["status"] == "open" and n["awaiting"] == pid:
                        g.bot_respond(pid, nid, self.bot, execute=ex)
                        now = g.negotiation_head(nid)
                        if now["status"] == "open" and now["awaiting"] == pid and now["entries"] == n["entries"]:
                            # the bot's answer was refused and nothing moved: end it rather than stall the game
                            session.call_tool(pid, "respond_negotiation",
                                              {"negotiation_id": n["id"], "action": "reject",
                                               "message": "We have nothing further to discuss."})
                session.cond.wait(timeout=1.0)

    def _owns(self, n: dict) -> bool:
        """Whether the bot answers this negotiation itself, rather than the language model its seat hands it to."""
        return engine_api.bot_owns_negotiation(self.bot, n)

    def respond_negotiation(self, session, pid: int, nid: int):
        """Answer a negotiation as the bot, unless it belongs to the seat's language model."""
        with session.lock:
            if self._owns(session.game.negotiation(nid)):
                session.game.bot_respond(pid, nid, self.bot, execute=self._executor(session))
