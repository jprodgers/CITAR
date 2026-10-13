"""A hybrid seat: the bot plays the turns, a language model decides the diplomacy the seat hands it
(design/phase3-features.md P3.6.3-P3.6.6, P3.6.11).

**The turn** (``HybridAgent.play_turn``, on the session's driver thread) is the engine's drive (P3.1.5): under the lock
the session drives the game's bots (``GameSession.drive_bots``: every seat with a bot, this one's with the model's
categories switched off), and the drive stops at ``hybrid_diplomat`` once this seat's bot has played. Then, off the
lock, if a trigger fired (``_triggers``, P3.6.4) and no phase ran this turn, the **diplomat phase** runs (``_phase``):
one ``diplomat`` ticket on the model's machine (30 s to start, ``phase_seconds`` once started, at most 8 steps), the
diplomat's system prompt (``prompts.DIPLOMAT_SYSTEM``, byte-identical for every call of the seat), a message with the
triggers, the diplomatic situation, the empire, the notebook and (with the advisor on) the bot's view, and the tools of
the model's categories (``hybrid.diplomat_tools``) through the session's filter and veto (``GameSession.diplomat_call``).
Under the lock the session drives again: the turn ends, or the drive stops at ``awaiting_reply`` for chats that wait
on others (waited for as a bot seat waits, P3.5.2) or on this seat's model (a chat its bot left to it, which its
responder or the chat clock's fallback answers).

**Chats** (P3.6.5). A chat waiting on the seat goes to its bot first (inside drives, or ``respond_negotiation`` with
``kind="bot"`` on the bot pool): the bot answers what it owns and defers the rest, which goes on the seat's line to the
model pool (``kind="model"``): a ``ChatThread`` (``citar.agents.llm_agent``) on a ``chat`` ticket whose start deadline is
the chat clock's, with the diplomat's prompt and the chat tools of the model's categories. When the ticket is refused or
expires, or the model's answer fails, the **fallback bot** (the same profile with every category the bot's, sharing the
seat's one memory in the game) answers at once, and the metrics record a fallback with its cause; the chat clock's own
fallback (``GameSession.chat_fallback``) answers one whose time ran out. **One voice per seat:** while a diplomat phase
runs, the seat's model answers no chat on its own ticket: a chat that comes to it then is given to the phase (told in its
next message), and when the phase ends the responder dispatches whatever still waits.

Replaces: ``BotAgent.play_turn`` driving a ``hybrid_diplomat`` stop straight on (``citar/agents/bot_agent.py``, before
3-14), when the server had no hybrid seat.
"""
from __future__ import annotations

import json
import logging
import threading
import time
import traceback
from typing import Optional

from .. import engine_api
from ..bots import profiles
from ..engine_api import ActionError, EngineCrash
from . import hybrid, prompts
from .llm_agent import LLMAgent, _serialize, _tool_defs, cause_of
from .providers import make_conversation

log = logging.getLogger("citar.hybrid")

#: How long a diplomat phase's ticket may wait to start before the phase is skipped and the bot's defaults stand.
PHASE_START_SECONDS = 30.0
#: The most model calls one phase makes.
PHASE_STEPS = 8
#: How much a standing must change since the last phase to call the diplomat (P3.6.4): a quarter.
SWING = 0.25
#: The smallest score a swing is measured against, so the first few turns' small scores (10 to 13) are no swing.
SWING_SCORE_FLOOR = 20
#: What is added to each side's military strength before they are compared: the first turns' armies of a warrior or
#: two (strength 8 to 20) would otherwise swing by half at every unit built.
STRENGTH_FLOOR = 20
#: How many of the game's latest events a turn's look reads for its triggers: a round's are far fewer.
EVENT_WINDOW = 2000
#: How long a phase waits for a chat answer of the seat's model already under way before it cancels it (P3.6.5).
VOICE_WAIT_SECONDS = 30.0
#: The most events' triggers a phase is told; older ones, while phases keep not running, are only counted.
MAX_EVENT_TRIGGERS = 20
#: The kinds of trigger that carry a date (``_dated_triggers``), and those measured again at every look (``_swings``).
DATED = ("deal", "treaty", "un")
SWINGS = ("score", "strength")


class DiplomatModel(LLMAgent):
    """A hybrid seat's model as its chat answers use it: ``LLMAgent``'s chat machinery (``ChatThread``: one
    conversation per chat, on a ticket per answer) with the diplomat's system prompt, the chat tools of the model's
    categories, and every call through the session's filter and veto."""

    def __init__(self, cfg: dict, seat_agent: "HybridAgent"):
        super().__init__(cfg, binding=None)
        self.seat_agent = seat_agent

    def chat_system(self) -> str:
        return self.seat_agent.system_prompt

    def chat_tool_names(self) -> set:
        return set(hybrid.chat_tools(self.seat_agent.owners))

    def chat_call(self, session, pid: int, name: str, args: dict) -> dict:
        return session.diplomat_call(pid, name, args, chat=True)

    def chat_note(self, session, pid: int, nid: int) -> str:
        return self.seat_agent.deal_note(session, pid, nid)


class HybridAgent:
    """Plays one hybrid seat (module docstring). Its bot handles are made at once; its model's configuration is read
    off the session's lock, the first time the model is needed (``model``)."""

    def __init__(self, seat, *, started_turn: int = 0, event_mark: Optional[int] = None):
        self.hybrid = dict(seat.hybrid or {})
        owners = self.hybrid.get("owners")
        try:
            self.owners = hybrid.owners_map(owners if owners is not None else "trade_only")
        except ValueError:
            self.owners = hybrid.owners_map("trade_only")
        bot_cfg = dict(seat.bot or {})
        profile = bot_cfg.get("profile")
        aggression = bot_cfg.get("aggression")
        ref = profile or profiles.DEFAULT_PROFILE
        try:
            profiles.resolve(ref)
        except profiles.ProfileError:            # deleted since the game was set up: play the standard bot
            ref = profiles.DEFAULT_PROFILE
        kw = {"aggression": float(aggression)} if aggression is not None else {}
        #: the seat's bot: it plays the turns and leaves the model's categories alone
        self.bot = profiles.make_bot(ref, **kw)
        engine_api.bot_set_diplomacy(self.bot, self.owners)
        #: the fallback: the same profile with every category the bot's, answering what the model could not
        self.fallback_bot = profiles.make_bot(ref, **kw)
        self.llm_block = dict(seat.llm or {})
        self.prompt_every = int(self.hybrid.get("prompt_every") or hybrid.PROMPT_EVERY_DEFAULT)
        self.phase_seconds = float(self.hybrid.get("phase_seconds") or hybrid.PHASE_SECONDS_DEFAULT)
        self.advisor_on = bool(self.hybrid.get("advisor", True))
        self.cancelled = False
        #: the turn's safety cap on its waits for answers, as BotAgent's (None: the game's)
        self.wait_cap: Optional[float] = None
        self.last_error: Optional[str] = None
        self._lock = threading.Lock()
        self._voice = threading.Condition(self._lock)
        self._model: Optional[DiplomatModel] = None
        self._model_error: Optional[str] = None
        self._model_read = False
        self._system: Optional[str] = None
        # the diplomat's state (P3.6.3): the last phase's turn and standings, the last event read, what fired
        self.started_turn = started_turn
        self.phase_turn: Optional[int] = None
        self.tried_turn: Optional[int] = None
        self.event_mark = event_mark
        #: the triggers no phase that ran has been told yet, keyed by what each is about (``_triggers``)
        self.pending: dict = {}
        #: the events' triggers left out of ``pending`` beyond ``MAX_EVENT_TRIGGERS``, since the last phase that ran
        self.dropped_events = 0
        self.baseline: Optional[dict] = None
        #: the dated triggers a phase that ran was told (until their date has passed)
        self.fired: set = set()
        # one voice per seat (P3.6.5): a phase holds the seat's model; chats that come to it then are the phase's
        self.phase_running = False
        self._held: set = set()
        self._answering = 0
        self._phase_conv = None
        self.phases = 0

    # ------------------------------------------------------------------ control
    def cancel(self):
        """Stop at the next point the turn waits, and stop the model's work in progress (a seat change, a suspension,
        the game closing). Non-blocking."""
        self.cancelled = True
        conv = self._phase_conv
        if conv is not None:
            try:
                conv.close()
            except Exception:
                pass
        if self._model is not None:
            self._model.cancel()

    def cancel_chat(self, nid: int) -> None:
        """The chat clock's deadline for chat ``nid``: the model's answer to it stops (non-blocking)."""
        if self._model is not None:
            self._model.cancel_chat(nid)

    def chat_closed(self, nid: int) -> None:
        """Chat ``nid`` closed: its conversation goes (non-blocking)."""
        with self._lock:
            self._held.discard(nid)
        if self._model is not None:
            self._model.chat_closed(nid)

    @property
    def usage_total(self) -> dict:
        return self._model.usage_total if self._model is not None else {}

    # ------------------------------------------------------------------ the model
    def model(self, session, pid: int) -> Optional[DiplomatModel]:
        """The seat's model, its configuration read the first time it is needed (off the session's lock: it may read
        the database); None, with ``last_error``, when the seat has no machine its game may use (P3.2.9)."""
        with self._lock:
            if self._model_read:
                return self._model
        from .. import servers
        block = self.llm_block
        try:
            if not block:
                # never a provider's default: a seat with no model block dials nothing, and its bot answers for it
                raise servers.ServerError("This hybrid seat has no model: its bot decides everything.")
            cfg = servers.resolve_llm(block, inline_ok=session._inline_llm_allowed(block))
            model, error = DiplomatModel(cfg, self), None
        except servers.ServerError as e:
            model, error = None, str(e)
        with self._lock:
            if not self._model_read:
                self._model, self._model_error, self._model_read = model, error, True
                if error:
                    self.last_error = error
            if self.cancelled and self._model is not None:
                self._model.cancelled = True
            return self._model

    def system_text(self, session, pid: int) -> str:
        """The diplomat's system prompt for this seat (``prompts.diplomat_system_prompt``): made once, so every call of
        the seat, its phases and its chats, sends the same text. Reads the game: the session's lock held or not."""
        if self._system is None:
            st = session.chat.settings
            with session.lock:
                civ = session.game.player_name(pid)
            persona = (self._model.cfg.get("persona") if self._model is not None else None) \
                or self.llm_block.get("persona")
            self._system = prompts.diplomat_system_prompt(
                civ, hybrid.model_categories(self.owners), hybrid.bot_categories(self.owners), self.prompt_every,
                st["chat_model_reply_seconds"], st["chat_person_reply_seconds"], st["max_chat_messages"], persona)
        return self._system

    @property
    def system_prompt(self) -> str:
        """The system prompt already made (``system_text``)."""
        return self._system or ""

    def _ticket(self, session, pid: int, cls: str, *, start_within: Optional[float], steps: int):
        """A pooled prompt of the seat's model (P3.3.3, P3.3.7): admitted for the game's owner for ``pooled`` on the
        model's machine, following the game's queue priority, and named by the game. In a benchmark game it waits for
        benchmarks and has no start deadline (P3.6.9). Reads the database: off the session's lock."""
        from ..pool import queue as work_queue, seats as pool_seats
        model = self._model
        cfg = model.cfg if model is not None else {}
        ref = session._lease_ref()
        bench = bool(session.benchmark)
        try:
            priority = work_queue.priority(ref[0], ref[1])
        except Exception:
            priority = 0
        return pool_seats.ticket(
            cfg.get("server_id") or None, cfg.get("model") or "", cls=cls, consumer=session.owner_id,
            activity=session.usage_act, label=session.lease_label or f"game “{session.name}”", ref=ref,
            priority=priority, start_within=None if bench else start_within, steps=steps, wait_for_benchmark=bench)

    # ------------------------------------------------------------------ the turn (P3.6.3)
    def play_turn(self, session, pid: int):
        """Play the seat's turn: the bot's drive, the diplomat phase when a trigger fired, then the drive that ends the
        turn, waiting meanwhile for the chats it stops on as a bot seat waits (``BotAgent.play_turn``): those that wait
        on other seats, and those that wait on this seat's model. Past the turn's safety cap a chat still waiting on
        another seat is closed as expired, and one waiting on this seat's model is answered by its fallback bot."""
        g = session.game
        with session.lock:
            session._await(lambda: not session.paused, float("inf"),
                           halted=lambda: self.cancelled or bool(session.crashed))
            if g.current != pid or g.phase != "playing" or session.crashed or session.stopped or self.cancelled:
                return
            cap = float(self.wait_cap) if self.wait_cap is not None else session.chat.safety_cap()
            stop = session.drive_bots(pid)
        spent = 0.0
        for _ in range(10_000):
            if session.stopped or session.crashed or self.cancelled:
                return
            if stop["stop"] == "hybrid_diplomat":
                try:
                    self._diplomacy(session, pid)
                except EngineCrash as e:
                    session._crashed(str(e))
                    return
                except Exception:
                    # the diplomat's failure is the server's news, never the turn's end: the bot's turn goes on
                    session.errors.append({"t": time.time(), "player": pid, "where": "diplomat phase",
                                           "trace": traceback.format_exc()})
                with session.lock:
                    session._await(lambda: not session.paused, float("inf"),
                                   halted=lambda: self.cancelled or bool(session.crashed))
                    if session.stopped or session.crashed or self.cancelled:
                        return
                    stop = session.drive_bots(pid)
                continue
            if stop["stop"] != "awaiting_reply":
                return
            with session.lock:
                waiting = self._open(session, stop["negotiations"])
                if waiting:
                    # chats left to this seat's model are its responder's: handed out again if no line holds them
                    session._dispatch_negotiation_interrupts([pid], again=True)
                    if spent < cap:
                        # until one of them moves or closes, and then the drive again: what came back to the seat
                        # goes to its bot inside the drive, as a bot seat's does
                        was = self._marks(session, waiting)
                        spent += session._await(lambda nids=waiting, then=was: self._marks(session, nids) != then,
                                                cap - spent, halted=lambda: self.cancelled, hold_paused=True)
                    session._await(lambda: not session.paused, float("inf"),
                                   halted=lambda: self.cancelled or bool(session.crashed))
                    if session.stopped or session.crashed or self.cancelled:
                        return
                    if spent >= cap:
                        # the chat clock should have settled them by now: the safety net
                        for nid in self._open(session, waiting):
                            n = g.negotiation_head(nid)
                            if n["awaiting"] == pid:
                                session.fallback_answer(pid, nid, self.fallback_bot, "timeout")
                            else:
                                session.close_negotiation(nid, "expired", "(no reply in time)")
                stop = session.drive_bots(pid)
        raise RuntimeError(f"player {pid}'s hybrid turn did not end after 10,000 drives")

    @staticmethod
    def _marks(session, nids) -> list:
        """Where each of ``nids`` stands: (status, awaiting, entries). Lock held."""
        out = []
        for nid in nids:
            try:
                n = session.game.negotiation_head(nid)
            except ActionError:
                out.append(None)
                continue
            out.append((n["status"], n["awaiting"], n["entries"]))
        return out

    @staticmethod
    def _open(session, nids) -> list:
        """Those of ``nids`` still open and waiting on somebody. Lock held."""
        out = []
        for nid in nids:
            try:
                n = session.game.negotiation_head(nid)
            except ActionError:
                continue
            if n["status"] == "open" and n["awaiting"] is not None:
                out.append(nid)
        return out

    # ------------------------------------------------------------------ triggers (P3.6.4)
    def _diplomacy(self, session, pid: int) -> None:
        """At the ``hybrid_diplomat`` stop: the triggers since the last look, and a phase when one fired and none ran
        this turn (at most one phase a turn: the turn's later stops do not ask again)."""
        if not hybrid.model_categories(self.owners):
            return
        with session.lock:
            turn = session.game.turn
            if self.tried_turn == turn:
                return
            fired = self._triggers(session, pid)
            if not fired:
                return
            self.tried_turn = turn
        self._phase(session, pid, fired)

    def _triggers(self, session, pid: int) -> list:
        """What calls the diplomat now (P3.6.4), from the events since the last look and the standings since the last
        phase that ran: each in words. Triggers of a phase that did not run (refused, skipped) wait for the next, each
        kept under what it is about (``pending``), so that the wait stays true however long it lasts (a benchmark
        holding the machine for hours, its quiet hours): a swing is measured again at every look (its entry replaced,
        or gone when the standing came back), a dated one (a deal's term or a treaty ending next turn, the United
        Nations vote open) goes once its date has passed or the vote closed or was cast, and the events' own are the
        latest ``MAX_EVENT_TRIGGERS``, with a count of those before. Session's lock held."""
        g = session.game
        turn = g.turn
        events = g.events(last=EVENT_WINDOW)
        mark = self.event_mark if self.event_mark is not None else -1
        name = g.player_name
        for e in events:
            if e.get("id", -1) <= mark:
                continue
            for i, text in enumerate(self._event_trigger(g, pid, e, name)):
                self.pending[("event", e.get("id"), i)] = text
        if events:
            self.event_mark = max(mark, max(e.get("id", -1) for e in events))
        told = [k for k in self.pending if k[0] == "event"]
        for k in told[:max(0, len(told) - MAX_EVENT_TRIGGERS)]:
            del self.pending[k]
            self.dropped_events += 1
        try:
            dip = g.execute(pid, "get_diplomacy", {})
        except ActionError:
            dip = {}
        for kinds, now in ((DATED, self._dated_triggers(pid, turn, dip, name)),
                           (SWINGS, self._swings(g, pid, dip))):
            for k in [k for k in self.pending if k[0] in kinds and k not in now]:
                del self.pending[k]         # past its date, or no longer a swing
            self.pending.update(now)
        # what a phase was told and whose date has passed can never come again
        self.fired = {k for k in self.fired if not (isinstance(k[-1], int) and k[-1] < turn)}
        due = turn - (self.phase_turn if self.phase_turn is not None else self.started_turn) >= self.prompt_every
        out = list(self.pending.values())
        if self.dropped_events:
            out.append(f"and {self.dropped_events} earlier event{'s' if self.dropped_events != 1 else ''} not listed")
        if due:
            out.append(f"it is {self.prompt_every} turn{'s' if self.prompt_every != 1 else ''} since the last call")
        return out

    def _phase_ran(self, session, pid: int, turn: int) -> None:
        """A phase ran (done, or its time ran out): what it was told is settled, and the swings are measured from
        here. Its dated triggers are not told again (``fired``)."""
        self.phase_turn = turn
        self.fired.update(k for k in self.pending if k[0] in DATED)
        self.pending = {}
        self.dropped_events = 0
        self.phases += 1
        with session.lock:
            try:
                dip = session.game.execute(pid, "get_diplomacy", {})
            except ActionError:
                dip = {}
            self.baseline = self._standings(session.game, pid, dip)

    def _event_trigger(self, g, pid: int, e: dict, name) -> list:
        """The triggers one event is (P3.6.4)."""
        t = e.get("type")
        d = e.get("data") or {}
        players = e.get("players")
        o = self.owners
        mine = players is None or pid in players
        if t == "first_contact" and pid in (d.get("a"), d.get("b")):
            other = d.get("b") if d.get("a") == pid else d.get("a")
            try:
                major = g.player(other)["kind"] == "major"
            except Exception:
                major = False
            return [f"first contact with {name(other)}"] if major else []
        if t == "war_declared" and d.get("defender") == pid:
            return [f"{name(d.get('attacker'))} declared war on you"]
        if t == "peace" and pid in (d.get("a"), d.get("b")):
            other = d.get("b") if d.get("a") == pid else d.get("a")
            return [f"peace was made with {name(other)}"]
        if t == "denounce" and d.get("b") == pid and isinstance(d.get("a"), int):
            return [f"{name(d['a'])} denounced you"]
        if t in ("deal_expired", "deal_cut") and players and pid in players:
            others = [p for p in players if p != pid]
            how = "expired" if t == "deal_expired" else "was cut"
            return [f"a deal with {name(others[0])} {how}" if others else f"a deal {how}"]
        if t == "city_captured" and d.get("new_owner") == pid and o.get("captured_cities") == "llm":
            return [f"you captured {e.get('text') or 'a city'}"]
        if t in ("cs_ally_lost", "cs_meet", "cs_quest") and mine and players and o.get("city_states") == "llm":
            return [f"city-states: {e.get('text') or t}"]
        if t == "spy" and mine and players and o.get("espionage") == "llm":
            return [f"espionage: {e.get('text') or 'news of a spy'}"]
        return []

    def _dated_triggers(self, pid: int, turn: int, dip: dict, name) -> dict:
        """Deals and treaties of the seat's that end next turn, and a United Nations vote open that it has not cast
        (when the vote is the model's), as they stand now, each keyed by what it is about with its date last; those a
        phase that ran was told already (``fired``) are left out."""
        out = {}
        ending = turn + 1
        for d in dip.get("active_deals") or []:
            for item in d.get("ongoing") or []:
                key = ("deal", d.get("id"), item.get("type"), item.get("until_turn"))
                if item.get("until_turn") == ending and key not in self.fired:
                    out[key] = (f"your deal with {d.get('with_name')} "
                                f"({item.get('type', 'a term').replace('_', ' ')}) ends next turn")
        for p in dip.get("players") or []:
            for k, v in p.items():
                if k.endswith("_until") and v == ending:
                    key = ("treaty", p.get("id"), k, v)
                    if key not in self.fired:
                        out[key] = f"your {k[:-len('_until')].replace('_', ' ')} with {p.get('name')} ends next turn"
        un = dip.get("united_nations") or {}
        if self.owners.get("un") == "llm" and un.get("voting_open") and un.get("your_vote") == "not cast":
            key = ("un", un.get("next_vote_turn"))
            if key not in self.fired:
                out[key] = "the United Nations vote is open"
        return out

    def _standings(self, g, pid: int, dip: dict) -> dict:
        """The seat's score, and its strength against each civilization it has met: its military strength over
        theirs (the turn's statistics, ``EngineGame.stats``), each side's plus ``STRENGTH_FLOOR`` so that a scout more
        or less in the first turns' armies of a few units is no swing."""
        me = g.standing(pid)
        rows = g.stats(last=1)
        military = {int(k): (v or {}).get("military") or 0
                    for k, v in ((rows[-1].get("players") or {}) if rows else {}).items()}
        mine = military.get(pid, 0)
        ratios = {}
        for p in dip.get("players") or []:
            q = p.get("id")
            if p.get("alive") is False or q is None:
                continue
            ratios[q] = (mine + STRENGTH_FLOOR) / (military.get(q, 0) + STRENGTH_FLOOR)
        return {"score": me.get("score") or 0, "ratios": ratios}

    def _swings(self, g, pid: int, dip: dict) -> dict:
        """The seat's score, or its strength against a civilization it has met, changed by more than a quarter since
        the last phase that ran (P3.6.4), as it stands now: keyed ``("score",)`` and ``("strength", civilization)``.
        The first look sets the baseline."""
        now = self._standings(g, pid, dip)
        if self.baseline is None:
            self.baseline = now
            return {}
        out = {}
        was = self.baseline
        if abs(now["score"] - was["score"]) > SWING * max(SWING_SCORE_FLOOR, was["score"]):
            out[("score",)] = f"your score went from {was['score']} to {now['score']}"
        for q, ratio in now["ratios"].items():
            before = was["ratios"].get(q)
            if before is not None and abs(ratio - before) > SWING * max(before, 0.1):
                out[("strength", q)] = (f"your strength against {g.player_name(q)} changed from {before:.2f} to "
                                        f"{ratio:.2f}")
        return out

    # ------------------------------------------------------------------ the diplomat phase (P3.6.6)
    def _phase(self, session, pid: int, triggers: list) -> None:
        """One diplomat phase, off the session's lock (module docstring). Its row goes on the game's metrics
        (``Metrics.diplomat``): outcome ``done`` (the model stopped, or used its steps), ``timeout`` (its budget ran
        out), ``skipped`` (its ticket did not start within 30 s, or the seat has no machine), ``refused`` (the machine
        or its admission refused the ticket), ``failed`` (the model's error) or ``cancelled`` (the seat halted). The
        row keeps the cause's code, never a refusal's or an error's words, which can name another member's machine
        and which the game's viewers read in its metrics: the model's error is the seat's ``last_error`` and the
        session's errors, which only its managers see.

        The phase takes the seat's voice before its ticket (``_hold_voice``), so the start deadline of its ticket does
        not run while a chat answer of the seat's own model finishes. A pause from the game screen holds it as it holds
        an LLM seat's turn (``GameSession.set_paused``): before its next model call and before each tool call, and its
        ticket goes back meanwhile (the machine is free for others) and is taken again on the resume; paused time does
        not count against ``phase_seconds``."""
        g = session.game
        turn = g.turn
        t0 = time.perf_counter()
        row = {"triggers": list(triggers), "steps": 0, "gen_s": 0.0, "queue_s": 0.0, "input_tokens": 0,
               "output_tokens": 0, "tools": [], "outcome": None, "cause": None}
        model = self.model(session, pid)
        if model is None:
            row.update(outcome="skipped", cause="refused")         # why: the seat's last_error (``model``)
            self._row(session, pid, turn, row, t0)
            return
        system = self.system_text(session, pid)
        from ..pool.scheduler import ScheduledConversation, SchedulerError, TicketExpired
        self._hold_voice(session, pid)
        ticket = conv = inner = None
        before: dict = {}
        try:
            ticket = self._ticket(session, pid, "diplomat", start_within=PHASE_START_SECONDS, steps=PHASE_STEPS)
            if ticket.state != "queued":
                row.update(outcome="skipped" if ticket.state == "expired" else "refused",
                           cause=cause_of(ticket.error))
                return
            steps_cap = ticket.steps
            inner = make_conversation(model.cfg, system, _tool_defs(set(hybrid.diplomat_tools(self.owners))))
            conv = ScheduledConversation(inner, ticket, model.cfg)
            conv.halted = lambda: self.cancelled or session.stopped or bool(session.crashed)
            conv.on_wait = lambda place: self._status(session, pid, "queued" if place is not None else "diplomat",
                                                      place)
            self._phase_conv = conv
            self._status(session, pid, "diplomat")
            told: dict = {}
            started: Optional[float] = None
            paused = [0.0]                  # paused time since the phase started, which its budget does not count
            before = dict(getattr(inner, "usage", {}) or {})

            def spent() -> float:
                return 0.0 if started is None else time.perf_counter() - started - paused[0]

            def hold() -> bool:
                """Wait out a pause (the ticket given back meanwhile and taken again after); False when the phase
                ends instead: the seat halted, or its new ticket did not come."""
                nonlocal ticket
                if not session.paused:
                    return True
                t = time.perf_counter()
                ticket.close()              # the machine's slot goes back for the pause
                self._status(session, pid, "paused")
                with session.lock:
                    session._await(lambda: not session.paused, float("inf"),
                                   halted=lambda: self.cancelled or bool(session.crashed))
                if started is not None:
                    paused[0] += time.perf_counter() - t
                if self.cancelled or session.stopped or session.crashed:
                    row["outcome"] = "cancelled"
                    return False
                ticket = self._ticket(session, pid, "diplomat", start_within=PHASE_START_SECONDS,
                                      steps=max(1, steps_cap - row["steps"]))
                if ticket.state != "queued":
                    row.update(outcome="skipped" if ticket.state == "expired" else "refused",
                               cause=cause_of(ticket.error))
                    return False
                conv.rebind(ticket)
                self._status(session, pid, "diplomat")
                return True

            with session.lock:
                text = self._phase_message(session, pid, triggers, told)
            conv.add_user_text(text)
            while True:
                if self.cancelled or session.stopped or session.crashed:
                    row["outcome"] = "cancelled"
                    break
                if row["steps"] >= steps_cap:
                    row["outcome"] = "done"
                    break
                if not hold():
                    break
                if spent() >= self.phase_seconds:
                    row["outcome"] = "timeout"
                    break
                if row["steps"]:
                    with session.lock:
                        news = self._chat_notes(session, pid, told)
                    if news:
                        conv.add_user_text("\n\n".join(news))
                conv.budget = lambda queued: max(5.0, self.phase_seconds - spent())
                step = conv.step()
                if started is None:
                    started = time.perf_counter() - float(conv.last_gen_s or 0.0)
                row["steps"] += 1
                row["gen_s"] += float(conv.last_gen_s or 0.0)
                row["queue_s"] += float(conv.last_queue_s or 0.0)
                self._thought(session, pid, step.thinking, "thinking")
                self._thought(session, pid, step.text, "diplomacy")
                if not step.tool_calls:
                    row["outcome"] = "done"
                    break
                results = []
                for c in step.tool_calls:
                    if self.cancelled or session.stopped or not hold():
                        break
                    if "__invalid_json__" in (c.args or {}):
                        res = {"ok": False, "error": "your tool arguments were not valid JSON."}
                    else:
                        res = session.diplomat_call(pid, c.name, c.args or {})
                    row["tools"].append({"tool": c.name, "ok": bool(res.get("ok"))})
                    results.append((c.id, _serialize(res["result"]) if res.get("ok") else "Error: " + res["error"],
                                    not res.get("ok")))
                    if c.name not in ("log_thought",):
                        args = json.dumps(c.args, ensure_ascii=False)[:200]
                        outcome = "ok" if res.get("ok") else f"ERROR: {str(res.get('error'))[:200]}"
                        self._thought(session, pid, f"{c.name} {args} → {outcome}", "action")
                if row["outcome"] is not None:
                    break                   # a pause that ended the phase (halted, or no ticket after it)
                conv.add_tool_results(results)
        except TicketExpired:
            row.update(outcome="skipped", cause="timeout")
        except SchedulerError as e:
            halted = self.cancelled or session.stopped
            row.update(outcome="cancelled" if halted else "refused", cause=None if halted else cause_of(e))
        except Exception as e:
            if self.cancelled or session.stopped:
                row["outcome"] = "cancelled"
            else:
                self.last_error = f"{type(e).__name__}: {e}"
                row.update(outcome="failed", cause="failed")
                session.errors.append({"t": time.time(), "player": pid, "where": "diplomat phase",
                                       "trace": traceback.format_exc()})
        finally:
            self._phase_conv = None
            if ticket is not None:
                ticket.close()
            if conv is not None:
                try:
                    conv.close()            # the provider's client goes with the phase
                except Exception:
                    log.debug("closing a diplomat phase's conversation failed", exc_info=True)
            if inner is not None:
                usage = dict(getattr(inner, "usage", {}) or {})
                for k in ("input_tokens", "output_tokens"):
                    row[k] = int(usage.get(k, 0) or 0) - int(before.get(k, 0) or 0)
                for k, v in usage.items():
                    model.usage_total[k] = model.usage_total.get(k, 0) + v - (before.get(k, 0) or 0)
            if row["outcome"] in ("done", "timeout"):
                self._phase_ran(session, pid, turn)
            self._release_voice()
            self._status(session, pid, "thinking")
            row["gen_s"], row["queue_s"] = round(row["gen_s"], 3), round(row["queue_s"], 3)
            self._row(session, pid, turn, row, t0)
            # what came to the seat's model during the phase and still waits goes to its responder (P3.6.5)
            session.dispatch_waiting([pid])

    @staticmethod
    def _row(session, pid: int, turn: int, row: dict, t0: float) -> None:
        """The phase's row on the game's metrics, under the game's lock (a save encodes the metrics under it)."""
        with session.lock:
            session.metrics.diplomat(pid, turn, row, seconds=time.perf_counter() - t0)

    def _hold_voice(self, session, pid: int) -> None:
        """The phase takes the seat's voice (P3.6.5): no chat answer of its model starts on its own ticket from now,
        and one already under way is waited for (at most ``VOICE_WAIT_SECONDS``, then cancelled: its chat, still
        waiting on the model, is then the phase's)."""
        with self._voice:
            self.phase_running = True
            self._held = set()
            deadline = time.monotonic() + VOICE_WAIT_SECONDS
            while self._answering and not self.cancelled and time.monotonic() < deadline:
                self._voice.wait(0.25)
            busy = self._answering
        if busy and self._model is not None:
            with self._model._threads_lock:
                threads = list(self._model._threads.values())
            for t in threads:
                t.cancel()

    def _release_voice(self) -> None:
        with self._voice:
            self.phase_running = False
            self._held = set()
            self._voice.notify_all()

    def _status(self, session, pid: int, status: str, place: Optional[dict] = None) -> None:
        """Tell the game screen what the seat's diplomat does: ``diplomat`` (thinking), ``queued`` (its ticket waits,
        with its place), and ``thinking`` again (the bot plays on)."""
        session.agent_status[pid] = status
        msg = {"type": "agent", "player": pid, "status": status}
        if status == "queued" and place:
            msg.update({"machine": place.get("machine"), "position": place.get("position"), "eta_s": place.get("eta_s")})
        session._broadcast(msg)

    def _thought(self, session, pid: int, text: str, kind: str) -> None:
        text = (text or "").strip()
        if not text:
            return
        with session.lock:
            session.game.add_thought(pid, text[:6000], kind)
        session._broadcast({"type": "thought", "player": pid, "turn": session.game.turn, "text": text[:6000],
                            "kind": kind})

    def _phase_message(self, session, pid: int, triggers: list, told: dict) -> str:
        """A phase's first message (``prompts.DIPLOMAT_PHASE``): the triggers in words, the diplomatic situation
        (``get_diplomacy`` compacted), the empire at a glance, the notebook's last 2,000 characters and, with the advisor
        on, the bot's view; then the chats that wait on the model. Session's lock held."""
        g = session.game
        try:
            dip = g.execute(pid, "get_diplomacy", {})
        except ActionError:
            dip = {}
        emp = g.empire_summary(pid)
        summary = {k: emp.get(k) for k in ("gold", "per_turn", "era", "happiness", "score", "strategic_resources",
                                           "luxuries", "policies", "cities", "at_war_with")}
        advice = ""
        if self.advisor_on and not session.benchmark:
            view = g.bot_advice(pid, self.bot)
            advice = prompts.DIPLOMAT_ADVICE.format(advice=json.dumps(view, separators=(",", ":"), default=str))
        text = prompts.DIPLOMAT_PHASE.format(
            turn=g.turn, triggers="; ".join(triggers) or "it is time",
            situation=json.dumps(compact_diplomacy(dip), separators=(",", ":"), default=str),
            empire=json.dumps(summary, separators=(",", ":"), default=str),
            notebook=(emp.get("notes") or "")[-2000:] or "(empty)", advice=advice)
        chats = self._chat_notes(session, pid, told)
        if chats:
            text += "\n\n" + "\n\n".join(chats)
        return text

    def _chat_notes(self, session, pid: int, told: dict) -> list:
        """The chats that wait on the seat's model (its bot leaves them to it) and that the phase has not been told of
        as they stand (``told``: chat -> entries), each as ``prompts.DIPLOMAT_CHAT``. Session's lock held."""
        g = session.game
        out = []
        for n in g.open_negotiation_heads(pid):
            if n["awaiting"] != pid or told.get(n["id"]) == n["entries"]:
                continue
            try:
                record = g.negotiation(n["id"])
                if engine_api.bot_owns_negotiation(self.bot, record):
                    continue
                view = g.negotiation_view(n["id"], pid)
            except ActionError:
                continue
            told[n["id"]] = n["entries"]
            theirs = [e for e in view.get("history") or [] if not e.get("you")]
            timing = session.chat.deadline_of(n["id"])
            out.append(prompts.DIPLOMAT_CHAT.format(
                other=view.get("with_name"), nid=n["id"],
                said=prompts.entry_text(theirs[-1]) if theirs else "nothing yet",
                proposal=prompts.proposal_text(view), value=self.deal_note(session, pid, n["id"], locked=True),
                time_left=prompts.time_left_text(timing.get("left_s"), bool(timing.get("paused")))))
        return out

    def deal_note(self, session, pid: int, nid: int, locked: bool = False) -> str:
        """With the advisor on (never in a benchmark game), what the bot makes of the proposal on the table in chat
        ``nid``, in gold from the seat's side: a sentence to add to a chat's message; else nothing."""
        if not self.advisor_on or session.benchmark:
            return ""
        try:
            if locked:
                value = session.game.bot_advice(pid, self.bot, nid).get("deal_value")
            else:
                with session.lock:
                    value = session.game.bot_advice(pid, self.bot, nid).get("deal_value")
        except (ActionError, ValueError):
            return ""
        return "" if value is None else f" The bot values the proposal on the table at {value:g} gold for you."

    # ------------------------------------------------------------------ the veto (P3.6.6, plan 6.1)
    def veto(self, session, pid: int, name: str, args: dict) -> Optional[str]:
        """Why the bot refuses a deal its seat's model offers (``open_negotiation``), counters with or accepts
        (``respond_negotiation``), or None: a deal that includes anything in a category the bot decides must be one the
        bot would accept as a whole (``EngineGame.bot_evaluate_call``'s value of the whole deal, from the seat's side,
        below zero refuses). What lies wholly in the model's own categories is the model's to give.

        The deal is the engine's own reading of the call (``EngineGame.bot_evaluate_call``: the arguments coerced and
        checked as ``execute`` does, the action in any case, the items as ``open_negotiation`` reads them), so what the
        model wrote cannot slip a deal past the veto that the engine then makes: keys the engine drops, amounts written
        as text, ``Accept`` for ``accept``. A call the engine would refuse is left to it. Session's lock held."""
        if name not in ("open_negotiation", "respond_negotiation"):
            return None
        g = session.game
        try:
            seen = g.bot_evaluate_call(pid, self.bot, name, args)
        except (ActionError, ValueError, TypeError):
            return None                     # the engine refuses the call itself
        if seen is None:
            return None
        cats, value = seen
        bots = [c for c in hybrid.CATEGORIES if c in cats and self.owners.get(c) != "llm"]
        if not bots or value >= 0:
            return None
        return (f"The bot decides {', '.join(c.replace('_', ' ') for c in bots)} and values this deal at "
                f"{round(value, 1):g} gold for you, so it will not agree. Leave those items out, or make the deal "
                f"better for {g.player_name(pid)}.")

    # ------------------------------------------------------------------ chats (P3.6.5)
    def respond_negotiation(self, session, pid: int, nid: int, kind: str = "bot", epoch: Optional[int] = None) -> str:
        """Answer chat ``nid`` (the seat's responder): its bot first (``kind="bot"``, on the bot pool), which leaves
        what it does not own to the model, queued on the seat's line for the model pool (``kind="model"``). Never
        ``unanswered`` or ``failed``: what the model could not answer, the fallback bot does."""
        if kind == "bot":
            return self._bot_answer(session, pid, nid, epoch)
        return self._model_answer(session, pid, nid)

    def _bot_answer(self, session, pid: int, nid: int, epoch: Optional[int]) -> str:
        from . import responders
        with session.lock:
            if session.crashed or session.stopped:
                return "moved"
            g = session.game
            try:
                n = g.negotiation_head(nid)
            except ActionError:
                return "moved"
            if n["status"] != "open" or n["awaiting"] != pid:
                return "moved"
            before = (g.turn, g.current)
            try:
                r = g.answer(pid, nid, self.bot)
            except ActionError:
                return "moved"
            except EngineCrash as e:
                session._crashed(str(e))
                return "moved"
            session.metrics.bot_actions(pid, r["actions"])
            if r["outcome"] == "done":
                session._after_action(*before)
                return "answered"
            if not responders.halted(session, pid, epoch):
                # left to the model: next on the seat's line, on the model pool
                responders.submit(session, pid, nid, n["entries"], "model")
            return "deferred"

    def _model_answer(self, session, pid: int, nid: int) -> str:
        with self._voice:
            if self.phase_running:
                self._held.add(nid)
                return "held"              # the phase holds the seat's voice: the chat is told to it (P3.6.5)
            self._answering += 1
        try:
            return self._model_answer_now(session, pid, nid)
        finally:
            with self._voice:
                self._answering -= 1
                self._voice.notify_all()

    def _model_answer_now(self, session, pid: int, nid: int) -> str:
        if session.answers_held():
            return "paused"                # the game paused for its machine, or loaded and not resumed: on resume
        with session.lock:
            try:
                n = session.game.negotiation_head(nid)
            except ActionError:
                return "moved"
            if n["status"] != "open" or n["awaiting"] != pid:
                return "moved"
        model = self.model(session, pid)
        if model is None:
            return self._fallback(session, pid, nid, "refused")
        self.system_text(session, pid)
        left = session.chat.deadline_of(nid).get("left_s")
        ticket = self._ticket(session, pid, "chat", start_within=left if left else None,
                              steps=model.max_negotiation_calls)
        if ticket.state != "queued":
            return self._fallback(session, pid, nid, cause_of(ticket.error))
        model.binding = ticket
        try:
            outcome = model.respond_negotiation(session, pid, nid)
        finally:
            model.binding = None
            ticket.close()
        if outcome in ("unanswered", "failed"):
            # the chat's conversation wrote its row (a fallback, with its cause: the machine's refusal, its ticket
            # expired, or the model's failure); the fallback bot answers now. A step the machine cut off (the lobby seat
            # whose idle slot it borrowed took it back) is a refusal too: the bot answers rather than keep the chat
            # waiting for another ticket
            with model._threads_lock:
                thread = model._threads.get(nid)
            cause = getattr(thread, "last_cause", None) or cause_of(ticket.error)
            return self._fallback(session, pid, nid, cause, record=False)
        return outcome

    def _fallback(self, session, pid: int, nid: int, cause: str, record: bool = True) -> str:
        """The fallback bot answers chat ``nid`` (P3.6.5), with the metrics' row of a fallback and its cause unless one
        was written already."""
        moved = session.fallback_answer(pid, nid, self.fallback_bot, cause, record=record)
        return "fallback" if moved else "moved"


def compact_diplomacy(dip: dict) -> dict:
    """``get_diplomacy`` as a diplomat's message gives it (P3.6.6): the civilizations met with their relations and
    treaties' until-turns (no trade options: those are a deal builder's), the open chats with the proposal on the table
    and the last word, the active deals with when their terms end, the last messages and the United Nations."""
    players = [{k: v for k, v in p.items() if k not in ("trade_options", "color")} for p in dip.get("players") or []]
    chats = []
    for n in dip.get("open_negotiations") or []:
        history = n.get("history") or []
        last = history[-1] if history else {}
        chats.append({"id": n.get("id"), "with": n.get("with_name"), "your_move": n.get("your_move"),
                      "proposal": (n.get("current_proposal") or {}).get("summary"),
                      "last": {"by": last.get("by"), "action": last.get("action"), "message": last.get("message")}})
    deals = [{"with": d.get("with_name"), "you_give": d.get("you_give"), "you_receive": d.get("you_receive"),
              "ends": min((o.get("until_turn") for o in d.get("ongoing") or [] if o.get("until_turn") is not None),
                          default=None)}
             for d in dip.get("active_deals") or []]
    out = {"civilizations": players, "open_chats": chats, "deals": deals,
           "recent_messages": (dip.get("messages") or [])[-5:]}
    if dip.get("united_nations"):
        out["united_nations"] = dip["united_nations"]
    return out
