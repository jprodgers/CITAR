"""Adapter that lets the compiled bot occupy a seat in a live session (crates/citar-engine/DESIGN.md P2.7.1).

A bot seat's turn is a *drive* (``EngineGame.drive``): the engine hands the turn to the bot, puts every negotiation
that waits on a bot seat to that seat's bot, and ends the turn itself. Each drive goes through the session's side
effects (``GameSession.drive_bots``: the version, the metrics, the responders, the broadcasts and the autosave), so a
bot seat is watched, measured and saved as a model's seat is, though its actions never pass through ``call_tool``.
"""
from __future__ import annotations

from .. import engine_api
from ..engine_api import ActionError, EngineCrash
from ..bots import profiles


class BotAgent:
    """Plays one bot seat: its turns through the session's drives, its answers through ``EngineGame.answer``.

    So that a bot seat and a model seat are driven alike by the session: the same turn loop, the same negotiation
    responders, the same metrics rows (a bot's turn row carries its action counts, ``bot_actions``, instead of
    per-tool rows). A benchmark comparing a model against the bot is then comparing two players of the same game
    rather than two code paths.
    """
    #: How long a bot's turn waits for a seat it is in a negotiation with (a person, a model, an MCP client) to
    #: answer before the chat is closed as expired and the turn goes on. Phase 3's lobby timeouts replace it.
    REPLY_WAIT_SECONDS = 90.0

    def __init__(self, aggression: float = 0.4, profile: str = None):
        # the profile decides what plays; the seat's aggression applies when the profile leaves it open
        ref = profile or profiles.DEFAULT_PROFILE
        try:
            self.profile = profiles.resolve(ref)
        except profiles.ProfileError:            # deleted since the game was set up: play the standard bot
            ref = profiles.DEFAULT_PROFILE
            self.profile = profiles.resolve(ref)
        self.bot = profiles.make_bot(ref, aggression=float(aggression) if aggression is not None else None)
        self.cancelled = False

    def cancel(self):
        """Stop at the next point the turn waits (a seat change, a suspension, the game closing)."""
        self.cancelled = True

    def play_turn(self, session, pid: int):
        """Play the bot's turn: drive it, and while it waits on another seat's answer, wait for that answer.

        The drive passes every bot seat of the game, so a negotiation one bot opens with another is answered inside
        it. A drive that stops because chats wait on seats the session plays (a person, a model, an MCP client) has
        already started those seats' responders (``_after_action``); this waits on the session's condition until any
        of them answers (the chat is settled, or back with the bot) and drives again at once, so an answer is taken
        up as it comes rather than when the slowest seat has answered. The turn has one budget for all its waits,
        :attr:`REPLY_WAIT_SECONDS` of unpaused time: once it is spent, the chats still waiting on others are closed
        as expired, through the session, and the turn is driven on to its end. A chat left waiting on the bot's own
        seat (one its model owns, which only a hybrid seat answers) is not waited for: the session's driver closes it
        and ends the turn, as it ends any turn an agent leaves open. A pause holds the turn before its first drive as
        between drives.
        """
        g = session.game
        with session.lock:
            # The driver looks at the pause before it calls this, without the lock: a pause that lands in between
            # would otherwise see the whole turn played, and autosaved, in a game already paused.
            session._await(lambda: not session.paused, float("inf"),
                           halted=lambda: self.cancelled or bool(session.crashed))
            if (g.current != pid or g.phase != "playing" or session.crashed or session.stopped
                    or self.cancelled):
                return
            budget = float(self.REPLY_WAIT_SECONDS)
            stop = session.drive_bots(pid)
            for _ in range(10_000):
                if session.stopped or session.crashed or self.cancelled:
                    return
                if stop["stop"] == "hybrid_diplomat":
                    # a hybrid seat's model would have its diplomacy now; a session has no hybrid seats yet
                    stop = session.drive_bots(pid)
                    continue
                if stop["stop"] != "awaiting_reply":
                    return
                theirs = self._waiting_on_others(session, pid, stop["negotiations"])
                if not theirs:
                    return
                if budget > 0:
                    budget -= session._await(
                        lambda nids=theirs: len(self._waiting_on_others(session, pid, nids)) < len(nids),
                        budget, halted=lambda: self.cancelled, hold_paused=True)
                # a pause holds the turn where it is: the game does not move on while paused
                session._await(lambda: not session.paused, float("inf"),
                               halted=lambda: self.cancelled or bool(session.crashed))
                if session.stopped or session.crashed or self.cancelled:
                    return
                if budget <= 0:
                    for nid in self._waiting_on_others(session, pid, theirs):
                        session.close_negotiation(nid, "expired", "(no reply in time)")
                stop = session.drive_bots(pid)
            raise RuntimeError(f"player {pid}'s bot turn did not end after 10,000 drives")

    @staticmethod
    def _waiting_on_others(session, pid: int, nids) -> list[int]:
        """Those of ``nids`` still open and waiting on a seat other than ``pid``'s."""
        out = []
        for nid in nids:
            try:
                n = session.game.negotiation_head(nid)
            except ActionError:
                continue
            if n["status"] == "open" and n["awaiting"] is not None and n["awaiting"] != pid:
                out.append(nid)
        return out

    def _owns(self, n: dict) -> bool:
        """Whether the bot answers this negotiation itself, rather than the language model its seat hands it to."""
        return engine_api.bot_owns_negotiation(self.bot, n)

    def respond_negotiation(self, session, pid: int, nid: int):
        """Answer a negotiation that waits on this seat (``EngineGame.answer``), unless the seat's model owns it.

        A refused answer means the chat moved on before the bot got to it (answered, closed, or no longer waiting on
        this seat), which is no error. An answer that did something goes through the session's side effects, and an
        engine that stops in it crashes the session (``_crashed``). Returns the answer (``{"outcome", "actions"}``,
        which the probes record), or None when there was none."""
        with session.lock:
            if session.crashed or session.stopped:
                return None
            g = session.game
            before = (g.turn, g.current)
            try:
                r = g.answer(pid, nid, self.bot)
            except ActionError:
                return None
            except EngineCrash as e:          # the engine stopped: the session stops with it, as for a drive
                session._crashed(str(e))
                return None
            session.metrics.bot_actions(pid, r["actions"])
            if r["outcome"] == "done":
                session._after_action(*before)
            return r
