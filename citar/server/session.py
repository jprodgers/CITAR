"""Game sessions: seats, turn driver thread, AI agents, negotiation interrupts, push updates and saves."""
from __future__ import annotations

import json
import logging
import secrets
import threading
import time
import traceback
import weakref
from collections import deque
from dataclasses import dataclass, field, asdict
from pathlib import Path
from datetime import datetime
from typing import Any, Callable, Optional

from .. import engine_api, fsutil, ledger
from ..engine_api import ActionError, EngineCrash, EngineGame
from .chat import NO_REPLY, ChatClock, RealClock
from .metrics import Metrics
from .. import paths
from ..diplomacy_stats import EVENT_KINDS as DIPLOMACY_EVENTS

# CITAR_SAVE_DIR lets the test suite keep its throwaway games out of the real save folder;
# citar.paths decides the rest (a checkout writes beside the code, an install writes per-user).
SAVE_DIR = paths.saves_path()
log = logging.getLogger("citar.session")
#: Who plays a seat (P3.6.2 added ``hybrid``: a bot plays the turns, a model decides the diplomacy it is given).
SEAT_TYPES = ("human", "mcp", "llm", "bot", "hybrid")
#: The pauses the server makes, which a restart does not keep (``mark_live``): the check that made each runs again (a
#: model server unreachable, quiet hours, making way in the queue, a machine's budget spent).
SERVER_PAUSES = ("disconnect", "restricted", "queue", "budget")
#: What a player is told when the engine has stopped after an internal error (GameSession._crashed).
CRASHED = ("The game engine stopped after an internal error, so this game takes no more moves. It is paused as it "
           "was: its last good autosave is kept, and the state at the crash was saved for debugging.")


@dataclass
class Seat:
    """One seat in a game: who plays it, and the token that proves it.

    A seat token grants play on exactly this seat and nothing else, which is what lets an external
    agent join a game with no account at all.
    """
    player: int
    type: str = "bot"                 # human | mcp | llm | bot | hybrid (see SEAT_TYPES)
    token: str = field(default_factory=lambda: secrets.token_urlsafe(12))
    name: str = ""
    llm: dict = field(default_factory=dict)   # provider, model, base_url, api_key_env, tool_mode, persona, ...
    bot: dict = field(default_factory=dict)   # profile, aggression
    connected: bool = False
    #: A hybrid seat's settings (``citar.agents.hybrid.normalize_hybrid``: owners, advisor, prompt_every,
    #: phase_seconds); saved with the session record, so it needs no save-format change (P3.1.5).
    hybrid: dict = field(default_factory=dict)
    #: The engine settings the seat's own entry chose (its lobby or scenario entry, or a seat change that named one):
    #: ``handicap`` and ``auto`` (some of the engine's automatic decisions), never a hybrid seat's, which follow its
    #: owners. A change of controller sets every setting explicitly from these and the new controller's defaults
    #: (``GameSession.update_seat``), since the engine keeps any setting once set explicitly.
    engine: dict = field(default_factory=dict)

    # Every place that asks "is this an LLM seat?" asks one of these (design/phase3-features.md P3.6.7).
    @property
    def model_seat(self) -> bool:
        """Whether a model plays the seat: an LLM seat, or a hybrid seat with a model."""
        return self.type == "llm" or (self.type == "hybrid" and bool(self.llm))

    @property
    def task_class(self):
        """The seat's task class (P3.3.7): ``full`` for an LLM seat (a lease), ``partial`` for a hybrid seat (tickets),
        None for a seat that plays no model."""
        from ..pool import seats as pool_seats
        return pool_seats.seat_class(self)

    @property
    def has_bot(self) -> bool:
        """Whether a bot plays the seat's turns: a bot seat, or a hybrid seat."""
        return self.type in ("bot", "hybrid")

    def public(self, include_token: bool = False) -> dict:
        """The seat as the client sees it. The token is included only for whoever may have it."""
        d = {"player": self.player, "type": self.type, "name": self.name, "connected": self.connected,
             "llm": {k: v for k, v in self.llm.items() if k not in ("api_key",)}, "bot": self.bot}
        if self.type == "hybrid":
            d["hybrid"] = self.hybrid
        if self.model_seat and self.llm.get("server_id"):
            from .. import servers
            d["llm_info"] = servers.describe_seat(self.llm)
        if include_token:
            d["token"] = self.token
        return d


class GameSession:
    """One live game: its seats, the thread that drives it, and everything that watches it.

    The session is where a game stops being a value and starts being something happening. It owns the
    turn driver - the thread that runs AI seats to completion, applies their orders, records metrics
    and autosaves - and the subscriber list that pushes updates to browsers.

    One lock guards the game. Every mutation takes it, including the driver's, so a tool call from an
    HTTP request cannot interleave with an AI's turn. Readers that build a large payload take it too,
    which is why the replay is assembled under the lock rather than streamed. Saves are not: a save takes a
    snapshot under the lock (a copy of the state, and the history since the last save) and the session's writer
    thread writes it off the lock, into the game's folder, beside the journal that holds its history (``save``).

    An internal error of the engine (``EngineCrash``, wherever it surfaces: a tool call, a bot's drive, a
    responder, a view) stops the session for good (``_crashed``): the engine refuses every command of a
    crashed game, so the session pauses it, stops its driver and agents, and keeps answering reads.
    """
    def __init__(self, game: EngineGame, seats: list[Seat], name: str = "", session_id: Optional[str] = None):
        self.id = session_id or secrets.token_hex(4)
        self.name = name or f"Game {self.id}"
        self.game = game
        self.seats = seats
        self.lock = threading.RLock()
        self.cond = threading.Condition(self.lock)
        self.spectator_token = secrets.token_urlsafe(12)
        self.paused = False
        self.pause_reason: Optional[dict] = None   # why the game is paused, when the game paused itself
        self.ai_delay = 0.0
        self.created = time.time()
        self.subscribers: list[Callable[[dict], None]] = []
        self.agents: dict[int, Any] = {}
        self.agent_status: dict[int, str] = {}
        self.errors: list[dict] = []
        # the chats handed to the seats' responders: (negotiation id, entries) -> the seat answering (P3.5.3); a key
        # goes when its chat moves or closes (_on_event), or when its seat is halted (responders.halt)
        self._responding: dict = {}
        self._lines: dict = {}          # seat -> its line of chats to answer (citar.agents.responders.SeatLine)
        self._answering: dict = {}      # seat -> its chat answers running now (_run_responder; under the lock)
        # a loaded game's model seats answer no chat until it is first resumed (answers_held; _register_loaded)
        self.held_from_load = False
        self._stop = False
        self._mark_lock = threading.Lock()   # a live mark is never written after the game is closed
        self._driver: Optional[threading.Thread] = None
        self._last_round = game.turn
        self.version = 0
        self.metrics = Metrics()
        self._metrics_current: Optional[tuple] = None
        self.benchmark: Optional[dict] = None   # {"run_id", "job_id", "suite", "scenario", "model", ...} for benchmark games
        self.engine_config: Optional[dict] = None   # the configuration the engine made a new game from (create)
        # The account the game belongs to, from its row in the games index (ownership.register and index_loaded), or the
        # benchmark or probe run's owner; None for the server's own (tests, scripts). Read by the model seats' gate
        # (_inline_llm_allowed) and, from Phase 3's scheduler and ledger on, as the consumer of the game's compute.
        self.owner_id: Optional[str] = None
        self.usage_act: Optional[str] = None    # usage ledger activity id (usage.py)
        self.crashed: Optional[dict] = None     # {"message", "turn", "at"} once the engine stopped (_crashed)
        self._autosaved: Optional[tuple] = None  # (turn, phase) of the last autosave
        self.read_only = False                   # opened only to be read (from_save(read_only=True)): never saved
        self.journal = None                      # this session's timeline, opened at its first save (_timeline)
        # what the saves cost under the lock (_snapshot): {"saves", "total_s", "max_s", "last_s"}; P2.5.3's budget is 10 ms
        self.save_lock = {"saves": 0, "total_s": 0.0, "max_s": 0.0, "last_s": 0.0}
        self._indexed: Optional[tuple] = None    # (turn, phase) last put on the games index row (_autosave_landed)
        # The model seats' leases on their machines (design/phase3-features.md P3.3.3): a lease belongs to the seat,
        # not to its agent, and every request the seat makes is one of its (lease_for). They are made when the session
        # is registered (``start``: open_leases) and released when a seat changes or is eliminated, the game ends,
        # stops or crashes, or makes way for other work. ``lease_kind`` overrides the kind a game's leases take, and
        # ``lease_ref`` and ``lease_label`` the work they serve and how it is named (a probe's turn case: probe_case,
        # its probe run).
        self.leases: dict = {}
        # Each machine a model seat of this game ran on, with its standing when it ran (design/phase4-helper.md
        # P4.5.8 rule 11: its tier, and whether its model results count toward the global boards): the worst seen
        # (a machine quarantined mid-game is recorded so). Kept in the save, read by the boards (``scoring``).
        self.machines: dict = {}
        self.lease_kind: Optional[str] = None
        self.lease_ref: Optional[tuple] = None
        self.lease_label: Optional[str] = None
        self._lease_lock = threading.Lock()
        # set once every seat's lease is released for good (the game ended, stopped or crashed): a lease made after
        # that would hold its machine for a game that plays no more, and nothing would ever release it
        self._leases_closed = False
        # seats eliminated since the session began: no lease is made for them again
        self._seats_out: set = set()
        # open_leases asked for by a thread that held the game's lock (it reads the database): the driver does it
        self._leases_due = False
        # False for a game opened only to be looked at (a finished benchmark job's, a probe case's end state): it
        # holds no machine until one of its seats is asked something (``SessionManager.load(leases=False)``)
        self.holds_machines = True
        self._writer = SaveWriter(self)
        # the session's time (a test's fake clock replaces it) and the chat clock that times every open chat by the
        # side it waits on (design/phase3-features.md P3.5.2); its thread starts with the driver
        self.clock = RealClock()
        self.chat = ChatClock(self)
        # each hybrid seat's diplomacy record for the leaderboard (citar.diplomacy_stats, design/phase3-features.md
        # P3.6.10): made when first needed (``_recorder``), its rows kept in the metrics so a save keeps them
        self.recorders: dict = {}
        game.subscribe(self._on_event)
        self._track_turn()
        from ..pool import scheduler as prompt_scheduler
        prompt_scheduler.watch_session(self)     # test mode: the scheduler's acquire is never entered with self.lock

    # ------------------------------------------------------------------
    # Seats & auth
    # ------------------------------------------------------------------
    def seat_for_token(self, token: Optional[str]) -> Optional[Seat]:
        """The seat a token belongs to, compared in constant time."""
        if not token:
            return None
        for s in self.seats:
            if secrets.compare_digest(s.token, token):
                return s
        return None

    def is_spectator(self, token: Optional[str]) -> bool:
        """Whether a token is this game's spectator link."""
        return bool(token) and secrets.compare_digest(self.spectator_token, token)

    def god_view_allowed(self) -> bool:
        """Whether anyone may watch with full vision.

        Only when the game is over or nobody human is playing. Otherwise a spectator link would be a way
        to see through the fog of war on somebody else's behalf.
        """
        return self.game.phase != "playing" or not any(s.type == "human" for s in self.seats)

    def info(self, include_tokens: bool = False) -> dict:
        """The game's summary, as the lobby shows it."""
        summ = self.game.summary()
        config = self.game.config
        d = {
            "id": self.id, "name": self.name, "turn": summ["turn"], "phase": summ["phase"],
            "current_player": summ["current"], "winner": summ["winner"], "victory": summ["victory"],
            "paused": self.paused, "ai_delay": self.ai_delay,
            "pause_reason": self.pause_reason if self.paused else None,
            "created": self.created, "config": {k: config.get(k) for k in (
                "map_size", "map_type", "speed", "difficulty", "barbarian_difficulty", "barbarians", "barbarian_aggression", "turn_limit", "victories", "city_states", "religion",
                "espionage", "tech_trading", "ruins", "seed", "map_edges", "wrap_x", "wrap_y", "river_density",
                "resources", "on_disconnect", "reconnect_seconds")}
            # the lobby's chat settings as the game plays them: the defaults when it gave none (P3.5.2)
            | self.chat.settings,
            # a major's difficulty level and whose numbers it plays with at it (the Join/Seats dialog shows both)
            "players": [{"id": p["id"], "name": p["name"], "color": p["color"], "alive": p["alive"], "kind": p["kind"],
                         **({"difficulty": p["difficulty"], "handicap": p.get("handicap")} if p["kind"] == "major"
                            else {})}
                        for p in summ["players"]],
            "seats": [s.public(include_tokens) for s in self.seats],
            "agent_status": {str(k): v for k, v in self.agent_status.items()},
            "agent_errors": {str(k): a.last_error for k, a in self.agents.items() if getattr(a, "last_error", None)},
            "god_view_allowed": self.god_view_allowed(),
            "benchmark": self.benchmark,
            "crashed": self.crashed,
        }
        if include_tokens:
            d["spectator_token"] = self.spectator_token
        return d

    # ------------------------------------------------------------------
    # Tool calls
    # ------------------------------------------------------------------
    def call_tool(self, pid: int, name: str, args: Optional[dict] = None, wait_negotiation: float = 0.0) -> dict:
        """Execute a tool for a player. Returns {"ok": bool, "result"|"error"}."""
        if self._stop:
            return {"ok": False, "error": "This game has been closed."}
        if self.crashed:
            return {"ok": False, "error": CRASHED}
        if name == "end_turn":
            self._hold_for_autosave(pid)     # only when it is pid's turn, and pid's ends the round
        with ledger.cpu_unit(self.cpu_activity), self.lock:
            before_turn, before_current = self.game.turn, self.game.current
            kind = engine_api.tool_kind(name) or "unknown"
            t0 = time.perf_counter()
            try:
                result = self.game.execute(pid, name, args or {})
                ok = True
            except ActionError as e:
                self.metrics.tool_call(pid, name, args or {}, kind, False, time.perf_counter() - t0, str(e))
                return {"ok": False, "error": str(e)}
            except EngineCrash as e:  # the engine stopped: never a refusal the caller could fix
                self.metrics.tool_call(pid, name, args or {}, kind, False, time.perf_counter() - t0, "engine crash")
                self._crashed(str(e))
                return {"ok": False, "error": CRASHED}
            except Exception as e:  # engine bug: report, don't crash the server
                self.errors.append({"t": time.time(), "player": pid, "tool": name, "args": args,
                                    "trace": traceback.format_exc()})
                self.metrics.tool_call(pid, name, args or {}, kind, False, time.perf_counter() - t0, f"Internal error: {e}")
                return {"ok": False, "error": f"Internal error: {e}"}
            self.metrics.tool_call(pid, name, args or {}, kind, True, time.perf_counter() - t0)
            if kind == "action":
                self._after_action(before_turn, before_current)
            if name in ("open_negotiation", "respond_negotiation") and isinstance(result, dict):
                nid = result.get("negotiation_id") or (args or {}).get("negotiation_id")
                if nid and wait_negotiation > 0:
                    result = self._wait_negotiation_reply(pid, int(nid), wait_negotiation, result)
            return {"ok": ok, "result": result}

    def _track_turn(self):
        """Close the metrics record of the player whose turn ended and open one for the player now to move."""
        g = self.game
        key = (g.turn, g.current, g.phase)
        if key == self._metrics_current:
            return
        if self._metrics_current is not None:
            _, prev, _ = self._metrics_current
            self.metrics.end_turn(prev)
        self._metrics_current = key
        turn, current, phase = key
        if phase == "playing" and current < len(self.seats) and g.is_alive(current):
            seat = self.seats[current]
            self.metrics.begin_turn(current, turn, seat.type, seat.llm.get("model") if seat.model_seat else None)

    def metrics_report(self) -> dict:
        """Per-seat metrics for this game, as the stats screen and reports use them."""
        players = {}
        for s in self.seats:
            players[s.player] = {"name": self.game.player_name(s.player), "controller": s.type,
                                 "model": s.llm.get("model") if s.model_seat else None,
                                 "settings": {k: s.llm.get(k) for k in ("provider", "base_url", "tool_mode", "reasoning_effort", "effort")}
                                 if s.model_seat else None}
            if s.type == "hybrid":
                # marked (P3.6.7): its turns are its bot's, its model's work the diplomat rows and chat answers;
                # `bot_best` when its bot was "Best bot" (the stats screen labels it as the lobby does)
                players[s.player].update({"hybrid": True, "bot": (s.bot or {}).get("profile") or "standard",
                                          "bot_best": (s.bot or {}).get("chosen_as") == "best",
                                          "owners": (s.hybrid or {}).get("owners")})
        return {"summary": self.metrics.summary(players), "turns": self.metrics.turn_rows(),
                "negotiations": self.metrics.data["negotiations"],
                "diplomat": self.metrics.data.get("diplomat") or [], "game_turn": self.game.turn}

    def _after_action(self, before_turn: int, before_current: int):
        """Bump the version, notify watchers and autosave after something changed. The hybrid seats' diplomacy records
        read the negotiations first (``_record_diplomacy``), so each proposal is valued against the game as the drive,
        answer or tool call left it, and the autosave below keeps what they wrote."""
        self.version += 1
        g = self.game
        self._record_diplomacy()
        self._track_turn()
        self._dispatch_negotiation_interrupts()
        if g.current != before_current or g.turn != before_turn:
            self.cond.notify_all()
            self._broadcast({"type": "turn", "turn": g.turn, "current_player": g.current, "phase": g.phase})
            if g.turn != self._last_round or g.phase != "playing":
                self._last_round = g.turn
                self.autosave()
        else:
            self.cond.notify_all()
            if g.phase != "playing":
                # a game that ended inside a turn (a capital taken, a vote won): its last autosave, which also tells the
                # games index it is finished; autosave() takes it once (it skips an unchanged turn and phase)
                self.autosave()
        self._broadcast({"type": "update", "version": self.version, "turn": g.turn, "current_player": g.current,
                         "phase": g.phase})

    def _recorder(self, pid: int):
        """Seat ``pid``'s diplomacy record (``citar.diplomacy_stats.DiplomacyRecorder``), made the first time it is
        needed: its rows go to the metrics' ``diplomacy`` list (rows a loaded game's save holds are where it carries
        on from), and its proposals are valued by the seat's own bot profile playing every category (the hybrid seat's
        fallback bot, ``hybrid_agent.valuation_bot``). Lock held."""
        rec = self.recorders.get(pid)
        if rec is None:
            from ..agents.hybrid_agent import valuation_bot
            from ..diplomacy_stats import DiplomacyRecorder
            rows = self.metrics.data.setdefault("diplomacy", [])
            try:
                bot = valuation_bot(self.seats[pid].bot)
            except Exception as e:
                # no bot to value with: the record stops before it starts, said once (``_record_failed``)
                rec = self.recorders[pid] = DiplomacyRecorder(pid, None, rows)
                self._record_failed(rec, e)
                return rec
            rec = self.recorders[pid] = DiplomacyRecorder(pid, bot, rows)
        return rec

    def _record_failed(self, rec, exc: BaseException) -> None:
        """Seat ``rec.pid``'s diplomacy record failed unexpectedly: it stops for the rest of the game, its rows saying
        so (``DiplomacyRecorder.fail``: a save keeps it, and its benchmark job pairs nothing, since rows missed or
        valued late are not its control's measure), and the failure is the server's news once (``errors``, with its
        traceback; call it from the ``except``), never the action's nor the event's. Lock held."""
        if rec.broken is not None:
            return
        rec.fail(self.game.turn, type(exc).__name__)
        self.errors.append({"t": time.time(), "player": rec.pid, "where": "diplomacy record",
                            "trace": traceback.format_exc()})
        log.warning("game %s: seat %s's diplomacy record failed (%s: %s); it stops here", self.id, rec.pid,
                    type(exc).__name__, exc)

    def _record_diplomacy(self) -> None:
        """Every hybrid seat's record reads what moved (``DiplomacyRecorder.observe``). Lock held; a failure stops that
        seat's record and is the server's news (``_record_failed``), never the action's."""
        for seat in self.seats:
            if seat.type != "hybrid":
                continue
            rec = self._recorder(seat.player)
            if rec.broken is not None:
                continue
            try:
                rec.observe(self.game)
            except EngineCrash:
                raise
            except Exception as e:
                self._record_failed(rec, e)

    def drive_bots(self, pid: int) -> dict:
        """One drive of the game's bot seats on ``pid``'s turn (``EngineGame.drive``, one seat's turn at most), with
        every side effect an action has. Called with the lock held, by ``pid``'s BotAgent.

        Every seat with a bot (bot and hybrid seats) is passed, so a negotiation one bot opens with another is answered
        inside the drive, and a hybrid seat's drive stops at ``hybrid_diplomat`` for its model's diplomacy. The drive's
        action counts for ``pid`` go on its turn's metrics record (``bot_actions``), whose ``end_reason`` is
        ``end_turn`` when the drive ended the turn; then ``_after_action`` runs: the version, the metrics turns, the
        responders of the seats a chat now waits on, the broadcasts and the autosave. Returns the drive's stop
        (``{"stop", "player", "negotiations", "actions"}``); ``{"stop": "crashed"}`` when the engine stopped, which
        crashes the session (``_crashed``).
        """
        g = self.game
        before = (g.turn, g.current)
        self._track_turn()          # pid's turn has its metrics record, however the turn came to it
        bots = {}
        for seat in self.seats:
            if seat.has_bot:
                # a hybrid seat's bot leaves its model's categories alone, and defers the chats they touch (P3.6.3)
                agent = self.get_agent(seat.player)
                if agent is not None:
                    bots[seat.player] = agent.bot
        try:
            stop = g.drive(bots, seat_limit=1)
        except EngineCrash as e:
            self._crashed(str(e))
            return {"stop": "crashed", "player": pid, "negotiations": [], "actions": {}}
        self.metrics.bot_actions(pid, stop["actions"].get(pid))
        if (g.turn, g.current) != before or g.phase != "playing":
            rec = self.metrics.current(pid)
            if rec is not None and not rec["end_reason"]:
                rec["end_reason"] = "end_turn"
        self._after_action(*before)
        return stop

    # ------------------------------------------------------------------
    # Crashes
    # ------------------------------------------------------------------
    def _crashed(self, message: str):
        """The engine stopped after an internal error (``EngineCrash``): stop the session where it stands.

        A crashed game refuses every command, so nothing may play on: the game is paused (``pause_reason`` kind
        ``crashed``), the driver stops, every agent is cancelled and nothing is emitted on the game. Watchers get a
        ``crashed`` broadcast, and ``info()`` (the lobby, the game screen) shows it. The state as the crash left it is
        written once as ``crash-<turn>.citar`` for whoever debugs it; the autosave is never written again, so the last
        good one is what a restart or a load comes back to. Reads (the summary, the views, the replay) still answer.
        Idempotent; safe with or without the lock held.
        """
        with self.lock:
            if self.crashed is not None:
                return
            turn = self.game.turn
            self.crashed = {"message": str(message)[:4000], "turn": turn, "at": time.time()}
            self.paused = True
            self.pause_reason = {"kind": "crashed", "message": CRASHED, "since": time.time()}
            self.metrics.pause()
            for pid in list(self.agents):
                self.cancel_agent(pid)
            self._halt_answers()
            self.chat.stop()
            self.release_leases()
            self.metrics.interrupt_open()
            self._metrics_current = None
            self.errors.append({"t": time.time(), "where": "engine", "trace": str(message)})
            self.cond.notify_all()
            try:
                # through the writer like any save; not waited for, so the lock is let go at once
                self.save(f"crash-{turn:03d}", wait=False)
            except Exception:
                self.errors.append({"t": time.time(), "where": "crash save", "trace": traceback.format_exc()})
        self.mark_live()
        self._broadcast({"type": "crashed", "turn": turn, "message": CRASHED})

    def _await(self, done, timeout: float, halted=None, hold_paused: bool = False) -> float:
        """Wait, with the lock held, until done() holds, the timeout passes, the session stops or halted() is true.

        Returns the seconds of waiting that counted against the timeout. ``halted`` is how an agent that has been
        cancelled (a seat change, quiet hours) stops waiting at once, rather than keeping the driver thread in a turn
        that is no longer its own. With ``hold_paused``, time the game spends paused does not count: an agent's wait
        is part of its turn, whose clock stops in a pause, and a person who pauses to think over an offer should not
        lose the chat to the clock. Time is the session's ``clock`` (a test's fake clock passes it at once).
        """
        counted = 0.0
        clock = self.clock
        while not done() and not self._stop and not (halted is not None and halted()):
            left = timeout - counted
            if left <= 0:
                break
            t = clock.now()
            clock.wait(self.cond, min(left, 1.0))
            if not (hold_paused and self.paused):
                counted += clock.now() - t
        return counted

    def _await_reply(self, pid: int, nid: int, timeout: float, halted=None, hold_paused: bool = False) -> float:
        """Wait, with the lock held, for the other side's answer in a negotiation (see _await, which it returns)."""
        def answered():
            """Whether the negotiation is settled or back with pid."""
            n = self.game.negotiation_head(nid)
            return n["status"] != "open" or n["awaiting"] == pid
        return self._await(answered, timeout, halted, hold_paused)

    def _wait_negotiation_reply(self, pid: int, nid: int, timeout: float, result: dict) -> dict:
        """Block until the other side answers a negotiation, or the wait runs out."""
        self._await_reply(pid, nid, timeout)
        return self._reply_result(pid, nid, result)

    def _reply_result(self, pid: int, nid: int, result: dict) -> dict:
        """A negotiation tool's result with where the negotiation now stands: the other side's answer if one came,
        else a note that none has yet."""
        n = self.game.negotiation_head(nid)
        if n["status"] == "open" and n["awaiting"] != pid:
            result = dict(result)
            result["note"] = "No reply yet. Continue your turn; the reply will show up in get_diplomacy/get_briefing."
            return result
        view = self.game.negotiation_view(nid, pid)
        last = view["history"][-1] if view["history"] else None
        result = dict(result)
        result["negotiation"] = {"id": nid, "status": n["status"], "your_move": n["awaiting"] == pid,
                                 "their_last": last if last and not last["you"] else None,
                                 "current_proposal": view["current_proposal"]}
        return result

    # ------------------------------------------------------------------
    # Chats: the seats' responders and the chat clock's actions (design/phase3-features.md P3.5.2, P3.5.3)
    # ------------------------------------------------------------------
    def _dispatch_negotiation_interrupts(self, pids=None, again: bool = False):
        """Hand the seats a program plays the chats that wait on them (``citar.agents.responders``: a line per seat,
        one chat at a time, on the model or the bot pool). Called with the lock held, after every action (it reads the
        negotiations' heads, not copies of their histories), and with ``again`` when the seats come back (a resume, a
        load, a seat change: ``dispatch_waiting``).

        This is why an agent should wait on ``wait_for_turn`` rather than polling for its own turn: a negotiation
        opened on somebody else's turn blocks that turn until it is answered. The current player's own agent handles
        the replies to the chats it opened during its turn (its loop's notes and end_turn's wait), so those are not
        handed out, except a hybrid seat's: its turn is its bot's, and what comes back to it goes to its line (its bot
        first, then its model, P3.6.5) like any other chat. A model seat whose machine the game paused for, or of a
        loaded game not yet resumed (``answers_held``), answers when it resumes.
        ``again`` hands out once more a chat whose key is still held but which no line of its seat holds now (an answer
        that stopped for such a pause).
        """
        from ..agents import responders
        current = self.game.current
        for n in self.game.open_negotiation_heads():
            pid = n["awaiting"]
            if pid is None or (pids is not None and pid not in pids):
                continue
            seat = self.seats[pid] if pid < len(self.seats) else None
            if seat is None or seat.type not in ("bot", "llm", "hybrid"):
                continue
            if current == pid and n["initiator"] == pid and seat.type != "hybrid":
                continue
            key = (n["id"], n["entries"])
            if key in self._responding and (not again or responders.busy_with(self, pid, n["id"])):
                continue
            if seat.type == "llm" and self.answers_held():
                continue
            self._responding[key] = pid
            # a hybrid seat's bot answers first, on the bot pool, and leaves what it does not own to its model
            responders.submit(self, pid, n["id"], n["entries"], "model" if seat.type == "llm" else "bot")

    #: The pauses during which a model seat's chat answers wait for the resume: the game paused because its machine is
    #: out of reach (a disconnect), in its quiet hours, or its budget is spent, so an answer asked then could only
    #: fail, and be declined. A person's pause and a make-way pause hold no answer (a game that made way answers on a
    #: lease of its own for the answer, ``lease_for(answering=True)``).
    HOLD_ANSWERS = ("disconnect", "restricted", "budget")

    def answers_held(self) -> bool:
        """Whether the game's model seats' chat answers wait for its resume: a pause of ``HOLD_ANSWERS``, or the pause a
        load leaves until the game is first resumed (``held_from_load``: a loaded game comes back paused so that its
        model configuration can be checked before anything spends tokens, whatever acts in it meanwhile)."""
        if not self.paused:
            return False
        return self.held_from_load or (self.pause_reason or {}).get("kind") in self.HOLD_ANSWERS

    def dispatch_waiting(self, pids=None) -> None:
        """Hand out again every open chat that waits on a bot or model seat (of ``pids``, when given) and no responder
        holds (P3.5.3: on resume, load, ``restore_live`` and a seat change), so a chat that was waiting when the game
        stopped or the seat changed is answered without anyone acting first."""
        if self._stop or self.crashed:
            return
        with self.lock:
            self._dispatch_negotiation_interrupts(pids, again=True)

    def _run_responder(self, pid: int, nid: int, key, epoch: Optional[int] = None, kind: str = "model"):
        """Run an AI seat's answer to a negotiation, on its line's pool thread (``citar.agents.responders``). A bot seat
        answers through the engine (``EngineGame.answer``, BotAgent.respond_negotiation), a model seat through its
        chat's conversation (``ChatThread``), a hybrid seat as its line's ``kind`` says (its bot's pass, then its
        model's answer: ``HybridAgent.respond_negotiation``, which never leaves a chat it could not answer to be
        declined: its fallback bot answers it). A model seat of a game that made way answers on a lease of its own for
        the answer (``lease_for(answering=True)``), which goes when its last answer ends (``_answered``).

        A halt never declines (P3.5.3): an answer whose seat was halted since it was taken (``epoch``: a suspension, a
        seat change, a crash) returns without a word, and the chat waits for the seat's next responder or the chat
        clock. Only an answer that ran and failed outright (an error, a model that used its steps without answering,
        a seat with no machine) is declined, "(no response)"; a slow one is the chat clock's."""
        from ..agents import responders
        if self._stop or self.crashed:
            return
        with self.lock:
            self._answering[pid] = self._answering.get(pid, 0) + 1
        outcome, raised = None, False
        try:
            agent = self.get_agent(pid, answering=True)
            if agent is not None and not responders.halted(self, pid, epoch):
                # the answer's CPU is this game's, whichever thread runs it (P3.4.3)
                with ledger.cpu_unit(self.cpu_activity):
                    if self.seats[pid].type == "hybrid":
                        outcome = agent.respond_negotiation(self, pid, nid, kind=kind, epoch=epoch)
                    else:
                        outcome = agent.respond_negotiation(self, pid, nid)
        except EngineCrash as e:
            self._crashed(str(e))
        except Exception:
            outcome, raised = "failed", True
            self.errors.append({"t": time.time(), "player": pid, "where": "negotiation responder",
                                "trace": traceback.format_exc()})
        finally:
            if (not self._stop and not self.crashed and not responders.halted(self, pid, epoch)
                    and outcome in ("unanswered", "failed")):
                self._reject_if_unanswered(pid, nid, key, record=raised)
            self._answered(pid)

    def _answered(self, pid: int) -> None:
        """A chat answer of seat ``pid`` ended. When it was the seat's last one, running or queued on its line, and the
        game has made way (P3.3.3: a make-way pause holds no lease until the resume), the lease the answer was made on
        goes, with the agent bound to it: otherwise ``occupied()`` would list the game again, and the work it made way
        for would wait for the game while the game waits for that work. The resume makes the seat's lease again
        (``_want_leases``). The seat's line answers one chat at a time, so a chat still queued on it keeps the lease
        for its own answer, and the agent goes without a halt: a chat handed to the line meanwhile is answered on a
        lease made for it, not dropped. Non-blocking (the lock is held only to look and release)."""
        with self.lock:
            left = self._answering.get(pid, 0) - 1
            if left > 0:
                self._answering[pid] = left
                return
            self._answering.pop(pid, None)
            if self._made_way() and self.leases.get(pid) is not None:
                ln = self._lines.get(pid)
                if ln is not None and ln.pending():
                    return                  # the line's next answer runs on it; the last one lets it go
                # a model seat's (a bot's answer holds no lease, and its agent keeps the bot's memory)
                self.cancel_agent(pid, halt=False)
                self.release_leases([pid], for_good=False)

    def _reject_if_unanswered(self, pid: int, nid: int, key, record: bool = False):
        """Reject a negotiation a seat failed to answer, so the other side is not left waiting on it for the chat
        clock's whole timeout. ``record``: the failure was not the model's conversation's (which wrote its own metrics
        row), so the row is written here."""
        with self.lock:
            try:
                n = self.game.negotiation_head(nid)
                if n["status"] == "open" and n["awaiting"] == pid and n["entries"] == key[1]:
                    before = (self.game.turn, self.game.current)
                    self.game.execute(pid, "respond_negotiation",
                                      {"negotiation_id": nid, "action": "reject", "message": "(no response)"})
                    if record and self.seats[pid].type in ("llm", "mcp"):
                        self.metrics.negotiation(pid, 0.0, 0, 0, 0, nid=nid, outcome="fallback", cause="failed")
                    self._after_action(*before)
            except ActionError:
                pass
            except EngineCrash as e:
                self._crashed(str(e))

    def cancel_chat_answer(self, pid: int, nid: int) -> None:
        """Stop the answer still in progress for chat ``nid`` (the chat clock, at its deadline): the seat's
        conversation for it (``LLMAgent.cancel_chat``). Non-blocking; allowed with the lock held."""
        fn = getattr(self.agents.get(pid), "cancel_chat", None)
        if fn is not None:
            try:
                fn(nid)
            except Exception:
                log.debug("could not cancel seat %s's answer to negotiation %s", pid, nid, exc_info=True)

    def _chat_still(self, nid: int, pid: Optional[int], entries: int) -> bool:
        """Whether chat ``nid`` stands where the clock saw it: open, waiting on ``pid``, with ``entries``. Lock held."""
        try:
            n = self.game.negotiation_head(nid)
        except ActionError:
            return False
        return n["status"] == "open" and n["awaiting"] == pid and n["entries"] == entries

    def decline_late(self, pid: int, nid: int, entries: int, cause: str = "timeout") -> bool:
        """The chat clock's action for a model seat (an LLM or MCP seat) whose time ran out: the seat declines,
        "(no reply in time)", on its behalf, unless the chat moved meanwhile. Returns whether it declined."""
        with self.lock:
            if self._stop or self.crashed or not self._chat_still(nid, pid, entries):
                return False
            before = (self.game.turn, self.game.current)
            try:
                self.game.execute(pid, "respond_negotiation",
                                  {"negotiation_id": nid, "action": "reject", "message": NO_REPLY})
            except ActionError:
                return False
            except EngineCrash as e:
                self._crashed(str(e))
                return False
            self.metrics.negotiation(pid, self.chat.timeout("model") or 0.0, 0, 0, 0, nid=nid,
                                     outcome="declined_late", cause=cause)
            self._after_action(*before)
            return True

    def chat_fallback(self, pid: int, nid: int, entries: int, cause: str = "timeout") -> bool:
        """The chat clock's action for a hybrid seat whose model's time ran out: its fallback bot answers
        (``EngineGame.answer``, design/phase3-features.md P3.6.5), or, with no bot to answer, the seat declines as a
        model seat does. Returns whether the chat moved."""
        agent = self.agents.get(pid)
        if agent is None and 0 <= pid < len(self.seats) and self.seats[pid].type == "hybrid" and not self._stop:
            agent = self.get_agent(pid)
        bot = getattr(agent, "fallback_bot", None)
        if bot is None:
            return self.decline_late(pid, nid, entries, cause)
        return self.fallback_answer(pid, nid, bot, cause, entries=entries,
                                    seconds=self.chat.timeout("hybrid") or 0.0)

    def fallback_answer(self, pid: int, nid: int, bot, cause: str, *, entries: Optional[int] = None,
                        record: bool = True, seconds: float = 0.0) -> bool:
        """A hybrid seat's fallback bot (``bot``: every category the bot's) answers chat ``nid`` for it, unless the chat
        no longer waits on it (or, with ``entries``, moved since the clock saw it): the chat clock at its deadline, the
        seat's responder when its model's prompt was refused, expired or failed, or its turn past its safety cap
        (P3.6.5). The metrics record a ``fallback`` with its ``cause`` (``chat.CAUSES``) unless ``record`` is False (the
        model's conversation wrote it). Returns whether the chat moved. Non-blocking beyond the engine's answer."""
        with self.lock:
            if self._stop or self.crashed:
                return False
            try:
                n = self.game.negotiation_head(nid)
            except ActionError:
                return False
            if n["status"] != "open" or n["awaiting"] != pid or (entries is not None and n["entries"] != entries):
                return False
            before = (self.game.turn, self.game.current)
            try:
                r = self.game.answer(pid, nid, bot)
            except ActionError:
                return False
            except EngineCrash as e:
                self._crashed(str(e))
                return False
            self.metrics.bot_actions(pid, r.get("actions"))
            if record:
                self.metrics.negotiation(pid, seconds, 0, 0, 0, nid=nid, outcome="fallback", cause=cause)
            if r.get("outcome") == "done":
                self._after_action(*before)
            return r.get("outcome") == "done"

    def diplomat_call(self, pid: int, name: str, args: Optional[dict] = None, *, chat: bool = False) -> dict:
        """A tool call of a hybrid seat's model (design/phase3-features.md P3.6.6): its diplomat phase's, or its chat
        answer's (``chat``). The session refuses a tool none of its categories gives it, "That is the bot's to decide.",
        and the bot's veto refuses an offer, a counter or an acceptance of a deal that includes something the bot
        decides and that the bot values below zero (``HybridAgent.veto``); each refusal is counted
        (``Metrics.diplomat_refusal``). Otherwise the call goes through ``call_tool``, in the same hold of the lock as
        the veto, so the deal it accepts is the one the bot valued. Returns {"ok", "result"|"error"}."""
        from ..agents import hybrid
        args = args if isinstance(args, dict) else {}
        agent = self.agents.get(pid)
        owners = getattr(agent, "owners", None)
        if owners is None or self.seats[pid].type != "hybrid":
            return {"ok": False, "error": "This seat has no diplomat."}
        allowed = hybrid.chat_tools(owners) if chat else hybrid.diplomat_tools(owners)
        if name not in allowed:
            with self.lock:
                self.metrics.diplomat_refusal(pid, name, "category", chat=chat)
            return {"ok": False, "error": hybrid.NOT_YOURS}
        with self.lock:
            veto = agent.veto(self, pid, name, args)
            if veto:
                self.metrics.diplomat_refusal(pid, name, "veto", chat=chat)
                return {"ok": False, "error": veto}
            return self.call_tool(pid, name, args)

    def expire_chat(self, nid: int, entries: int) -> bool:
        """The chat clock's action for a chat whose person's time ran out: it closes as expired, "(no reply in time)",
        whoever opened it, unless it moved meanwhile. Returns whether it closed."""
        with self.lock:
            try:
                n = self.game.negotiation_head(nid)
            except ActionError:
                return False
            if n["status"] != "open" or n["entries"] != entries:
                return False
            return self.close_negotiation(nid, "expired", NO_REPLY)

    def chat_view(self, nid: int, pid: Optional[int]) -> dict:
        """Chat ``nid`` as seat ``pid`` sees it (``negotiation_view``; the initiator's side for a spectator, ``pid``
        None), with where its clock stands (``clock``): the chat window's ``GET /api/games/{gid}/negotiations/{nid}``.
        Raises ActionError for a chat the game does not have, or one ``pid`` is not in. Lock held."""
        n = self.game.negotiation_head(nid)
        side = pid if pid is not None else n["initiator"]
        if side not in (n["initiator"], n["responder"]):
            raise ActionError(f"No such negotiation: {nid}.")
        view = self.game.negotiation_view(nid, side)
        timing = self.chat.deadline_of(nid) if n["status"] == "open" else {}
        view["clock"] = {"waiting_on": n["awaiting"], "kind": timing.get("kind"), "deadline": timing.get("deadline"),
                         "left_s": timing.get("left_s"), "paused": timing.get("paused", bool(self.paused))}
        return view

    def close_negotiation(self, nid: int, status: str, note: str, by: Optional[int] = None) -> bool:
        """Time a chat out or force it closed (diplomacy.close_negotiation), and tell everyone watching.

        Returns False when there was nothing to close: the negotiation had been settled in the meantime, or the
        engine has stopped (the session is then crashed).
        """
        with self.lock:
            try:
                self.game.close_negotiation(nid, status, note, by)
            except ActionError:
                return False
            except EngineCrash as e:
                self._crashed(str(e))
                return False
            self._after_action(self.game.turn, self.game.current)
            return True

    def _close_open_chats(self, pid: int, note: str):
        """Close, as expired, every open negotiation a seat is in. Called with the lock held."""
        for n in self.game.open_negotiation_heads(pid):
            self.close_negotiation(n["id"], "expired", note)

    # ------------------------------------------------------------------
    # Agents
    # ------------------------------------------------------------------
    def get_agent(self, pid: int, *, answering: bool = False):
        """The agent driving a seat, created on first use. None for a model seat of a game that has stopped or crashed:
        a thread that looked at the game just before (the driver, a chat's responder) plays nothing more, and makes
        no lease that nothing would release. None too for a model seat of a game that made way, unless the agent is
        for a chat's answer (``answering``): the driver never plays a turn on a machine its game gave up.

        A model seat's agent is bound to the seat's live lease. One whose lease was released after the agent was made
        (a make-way pause or a seat change landing while it was made) is never kept: it is dropped and made again on
        the seat's lease, so a seat never plays turn after turn on a lease that refuses every request."""
        seat = self.seats[pid]
        if seat.type == "llm" and not answering and self._made_way():
            return None
        agent = self.agents.get(pid)
        if agent is not None and not self._stale(agent):
            return agent
        if agent is not None:
            with self.lock:
                if self.agents.get(pid) is agent:
                    self.cancel_agent(pid, halt=False)     # its answer in progress, if any, goes on with the new one
        if seat.type == "llm":
            return None if self._leases_ended() else self._llm_agent(pid, seat, answering)
        if seat.type not in ("bot", "hybrid"):
            return None
        from ..agents.bot_agent import BotAgent
        from ..agents.hybrid_agent import HybridAgent
        with self.lock:
            # made and kept in one hold of the lock (no database read: its bot handles only, its model's configuration
            # is read when first needed, off the lock), so the threads that ask at once (the driver, the responders,
            # the chat clock) get one agent: a hybrid seat's diplomat state and its one voice live on it (P3.6.5)
            kept = self.agents.get(pid)
            if kept is not None and not self._stale(kept):
                return kept
            if seat.type == "bot":
                # the bot draws from the game's seed, so the same map seed plays the same game
                agent = BotAgent(**{k: v for k, v in seat.bot.items() if k in ("aggression", "profile")})
            elif seat.type == "hybrid":
                # the diplomat reads the events from here on (P3.6.4)
                last = self.game.events(last=1)
                agent = HybridAgent(seat, started_turn=self.game.turn, event_mark=last[-1]["id"] if last else None)
            else:
                return None                 # the seat changed meanwhile
            self.agents[pid] = agent
            return agent

    @staticmethod
    def _stale(agent) -> bool:
        """Whether a cached agent is bound to a lease that has been released."""
        return bool(getattr(getattr(agent, "binding", None), "released", False))

    def _llm_agent(self, pid: int, seat, answering: bool):
        """A model seat's agent, bound to the seat's lease (``get_agent``). It is kept only if, under the game's lock,
        the seat still plays the block it was made from and that lease is still the seat's and live: ``suspend``,
        ``update_seat`` and ``_crashed`` change the seat, cancel its agent and release its lease in one hold of that
        lock, so an agent made across one of them is either cancelled by it or never kept. Made again, a few times,
        when the seat or its lease changed meanwhile (from the seat's block as it is then); None when no lease can be
        made."""
        from ..agents.llm_agent import LLMAgent, NoMachineAgent
        from .. import servers
        for _ in range(3):
            block = seat.llm
            if seat.type != "llm":
                return None
            try:
                cfg = servers.resolve_llm(block, inline_ok=self._inline_llm_allowed(block))
            except servers.ServerError as e:
                # Never the seat's block as it is: without a machine to build it from, that block is an address and
                # a key variable somebody typed in, and only an administrator's game may dial one (P3.2.9).
                agent, lease = NoMachineAgent(str(e)), None
            else:
                lease = self.lease_for(pid, cfg.get("model") or "", answering=answering)
                if lease is None:
                    return None         # ended, stopped, crashed, eliminated or made way meanwhile
                agent = LLMAgent(cfg, binding=lease)
            with self.lock:
                if seat.type != "llm" or seat.llm is not block:
                    continue            # the seat changed while its agent was made: made again from its new block
                if lease is not None and (self.leases.get(pid) is not lease or lease.released
                                          or self._leases_ended()):
                    continue            # its lease went meanwhile (a make-way pause, the game's end): look again
                kept = self.agents.get(pid)
                if kept is not None and not self._stale(kept):
                    return kept         # another thread made the seat's agent meanwhile
                self.agents[pid] = agent
                return agent
        return None

    def prompt_agent(self, pid: int):
        """A model seat's agent for pooled prompts (design/phase3-features.md P3.3.3: a probe's offer and message
        cases), made fresh: it is bound to no lease, so the seat holds none while it answers, and its caller gives it
        each prompt's ticket (``agent.binding``) before it asks. A seat whose block names no machine its game may use
        gets a ``NoMachineAgent``, as ``get_agent`` gives it. Reads the database: never call it with the game's lock
        held."""
        from ..agents.llm_agent import LLMAgent, NoMachineAgent
        from .. import servers
        seat = self.seats[pid]
        block = seat.llm
        try:
            agent = LLMAgent(servers.resolve_llm(block, inline_ok=self._inline_llm_allowed(block)))
        except servers.ServerError as e:
            agent = NoMachineAgent(str(e))
        with self.lock:
            self.cancel_agent(pid, halt=False)
            self.agents[pid] = agent        # the game's stop cancels it
        return agent

    def lease_for(self, pid: int, model: str = "", *, answering: bool = False):
        """The seat's lease on its machine (design/phase3-features.md P3.3.3): made when the session is registered
        (``open_leases``), else on first use (a game opened only to be looked at, a probe case played outside a run).
        It belongs to the seat, not to its agent: an agent made again (a seat change, a resume) binds to the same lease
        while its seat names the same machine. Its kind is ``benchmark`` in a benchmark game, ``lease_kind`` when set (a
        probe's turn case), else ``game`` (a lobby game: it lends its slot while the seat is idle). Its consumer is the
        game's owner, capped by the concurrency of the grant that lets the owner use the machine for the lease's work
        (``_cap_purpose``). A seat with no machine gets an unscheduled lease (its calls pass through, metered). None for
        a seat that plays no model or was eliminated, once the game has ended, stopped or crashed (its leases are
        released for good then), and while the game has made way, except for a chat's answer (``answering``), whose
        lease goes when the seat's last answer ends (``_answered``). Reads the database for a lease it makes, before it
        takes the leases' lock (which ``release_leases`` takes under the session's): never call it with the session's
        lock held."""
        seat = self.seats[pid] if pid < len(self.seats) else None
        if seat is None or seat.type != "llm" or pid in self._seats_out:
            return None
        if not answering and self._made_way():
            return None
        sid = (seat.llm or {}).get("server_id") or None
        model = model or (seat.llm or {}).get("model") or ""
        kind = self._lease_kind()
        from ..pool import scheduler as prompt_scheduler, seats as pool_seats
        lease, new_owner = self._live_lease(pid, sid, model, kind)
        if lease is not None:
            if new_owner:
                # made when the game was registered, before its owner was known (a save loaded by a member whose game
                # had no owner yet): the owner's grant caps it, and its budget counts it, from now on (a database
                # read, off every lock)
                lease.cap, lease.access_grant = pool_seats.consumer_terms(self.owner_id, sid, self._cap_purpose())
            return lease
        if self._leases_ended():
            return None
        # off every lock: the grant's concurrency and budget and the machine (its row read on first use) are database
        # reads
        self._note_standing(sid)
        cap, access_grant = pool_seats.consumer_terms(self.owner_id, sid, self._cap_purpose())
        sched = prompt_scheduler.get()
        sched.machine(sid)
        with self._lease_lock:
            if self._leases_ended() or pid in self._seats_out:
                return None
            if not answering and self._made_way():
                return None         # made way meanwhile: suspend set the pause before it released the leases
            lease = self.leases.get(pid)
            if self._still_lease(pid, lease, sid, kind):
                return self._renewed(pid, lease, model)         # another thread made it meanwhile
            if lease is not None:
                self._let_go(pid, lease)
            lease = sched.lease(sid, model, kind=kind, activity=self.usage_act, consumer=self.owner_id,
                                label=self.lease_label or f"game “{self.name}”", cap=cap, game=self.id,
                                ref=self._lease_ref(), on_status=self._lease_status if kind == "benchmark" else None,
                                access_grant=access_grant,
                                on_budget=lambda payload, pid=pid: self._budget_warning(pid, payload))
            self.leases[pid] = lease
            return lease

    def _note_standing(self, sid: Optional[str]) -> None:
        """The machine ``sid``'s standing as this game's seat runs on it (P4.5.8 rule 11), into ``machines``: the first
        one seen, or a worse one (its model results no longer counting) over it. A database read: never under a
        lock."""
        if not sid:
            return
        from ..jobs import trust
        st = trust.standing(sid)
        if st is None:
            return
        prev = self.machines.get(sid)
        if prev is None or (prev.get("vouched", True) and not st["vouched"]):
            self.machines[sid] = st

    def _lease_kind(self) -> str:
        """The kind this game's seats' leases take."""
        return "benchmark" if self.benchmark else (self.lease_kind or "game")

    #: The admission purpose each kind of seat lease is admitted for (``pool.seats.authorize``): a benchmark game's
    #: seats are a benchmark run's work, a probe's turn case a probe run's, every other seat a game's.
    CAP_PURPOSES = {"benchmark": "benchmark", "probe_case": "probe"}

    def _cap_purpose(self) -> str:
        """The purpose whose grant caps this game's leases (``pool.seats.consumer_cap``): the one its seats were
        admitted for, so a member whose grant on a machine covers only benchmarks is held to that grant's
        concurrency in her benchmark games."""
        return self.CAP_PURPOSES.get(self._lease_kind(), "game")

    def _lease_ref(self) -> tuple:
        """The work this game's leases serve, as the shared queue names it: its benchmark run, the probe run a case is
        part of (``lease_ref``), or the game."""
        if self.lease_ref:
            return tuple(self.lease_ref)
        run = (self.benchmark or {}).get("run_id") if isinstance(self.benchmark, dict) else None
        return ("benchmark", run) if run else ("game", self.id)

    def _leases_ended(self) -> bool:
        """Whether the game's leases are released for good (it ended, stopped or crashed)."""
        return self._leases_closed or self._stop or self.crashed is not None

    def _still_lease(self, pid: int, lease, sid: Optional[str], kind: str) -> bool:
        """Whether ``lease`` is still the seat's for machine ``sid``: live, on that machine, of the kind the game's
        leases take now. Leases' lock held."""
        return lease is not None and not lease.released and lease.server_id == sid and lease.kind == kind

    def _live_lease(self, pid: int, sid: Optional[str], model: str, kind: str) -> tuple:
        """The seat's lease when it still plays ``sid``, brought up to date (its activity, consumer and model), and
        whether its consumer changed (the game's owner was set after the lease was made); (None, False) when it has
        none. Non-blocking."""
        with self._lease_lock:
            if self._leases_ended():
                return None, False
            lease = self.leases.get(pid)
            if not self._still_lease(pid, lease, sid, kind):
                return None, False
            new_owner = lease.consumer != self.owner_id
            return self._renewed(pid, lease, model), new_owner

    def _renewed(self, pid: int, lease, model: str):
        """``lease`` with the game's activity and owner, and the model its seat plays now (a model changed on the same
        machine keeps the lease: the loaded-model rule and the ETA read the new one). Leases' lock held."""
        lease.activity, lease.consumer = self.usage_act, self.owner_id
        if model and lease.model != model:
            lease.set_model(model)
        return lease

    def _let_go(self, pid: int, lease) -> None:
        """The seat no longer plays under ``lease``: released. Leases' lock held; non-blocking."""
        self.leases.pop(pid, None)
        lease.release()

    def release_leases(self, pids=None, *, for_good: Optional[bool] = None) -> None:
        """Release the leases of ``pids`` (every seat's when None): the seat no longer plays its machine (a seat change,
        an elimination, a make-way pause), or the game ended, stopped or crashed. For good (``for_good``, the default
        when ``pids`` is None): ``lease_for`` makes no lease after it. Non-blocking: allowed with the session's lock
        held (P3.3.9)."""
        with self._lease_lock:
            if for_good if for_good is not None else pids is None:
                self._leases_closed = True
            for pid in list(self.leases) if pids is None else list(pids):
                lease = self.leases.get(pid)
                if lease is not None:
                    self._let_go(pid, lease)

    def _seat_out(self, pid: int) -> None:
        """Seat ``pid`` was eliminated: its lease goes, and none is made for it again. Non-blocking."""
        with self._lease_lock:
            self._seats_out.add(pid)
            lease = self.leases.get(pid)
            if lease is not None:
                self._let_go(pid, lease)

    def _made_way(self) -> bool:
        """Whether the game paused to make way for higher-priority work on its machine (``pause_reason`` kind
        ``queue``): it holds no lease until it resumes."""
        return self.paused and (self.pause_reason or {}).get("kind") == "queue"

    def open_leases(self, pids=None) -> None:
        """Make the lease of every living model seat with a machine (of ``pids``, when given) that has none (P3.3.3):
        when the session is registered (``start``: created, loaded paused, brought back by ``restore_live``, reloaded
        by a benchmark, launched from a scenario), when a seat moves to another machine (``update_seat``), and when a
        game that made way resumes. So a game whose next turn will want a machine is listed by ``occupied()`` before
        any of its seats is asked anything, as when its bots move first. Nothing for a game that is over, made way,
        ended, stopped or crashed, or was opened only to be looked at (``holds_machines``). Reads the database for a
        lease it makes: never call it with the session's lock held (``_want_leases`` defers to the driver then)."""
        if not self.holds_machines or self._leases_ended():
            return
        with self.lock:
            if self.game.phase != "playing" or self._made_way():
                return
            wanted = [seat.player for seat in self.seats
                      if seat.type == "llm" and (seat.llm or {}).get("server_id")
                      and (pids is None or seat.player in pids) and self.game.is_alive(seat.player)]
        for pid in wanted:
            self.lease_for(pid)

    def _want_leases(self, pids=None) -> None:
        """``open_leases`` now; or, when the calling thread holds the game's lock (``open_leases`` reads the
        database), by the driver at its next look."""
        is_owned = getattr(self.lock, "_is_owned", None)
        if is_owned is not None and is_owned():
            self._leases_due = True
            self.cond.notify_all()
        else:
            self.open_leases(pids)

    def leased_machines(self) -> list:
        """The machines this game's live leases are on (``usage.session_servers``)."""
        with self._lease_lock:
            return [lease.server_id for lease in self.leases.values() if not lease.released and lease.server_id]

    def _lease_status(self, payload: dict) -> None:
        """A benchmark lease's news, on the scheduler's notifier thread with no machine lock held: when its first
        request went ahead after the machine's pooled requests drained (P3.3.3), the wait is added to the job's
        ``drain_s`` and a drain cut short by its bound marks it ``contaminated`` (its timing is left out of the speed
        score). Both are kept in ``benchmark``, which the job's result (``benchmarks.game_progress``) and the game's
        saves carry."""
        if payload.get("state") != "drained":
            return
        with self.lock:
            bench = self.benchmark
            if not isinstance(bench, dict):
                return
            bench["drain_s"] = round(float(bench.get("drain_s") or 0.0) + float(payload.get("drain_s") or 0.0), 3)
            bench["drains"] = int(bench.get("drains") or 0) + 1
            if payload.get("contaminated"):
                bench["contaminated"] = True

    @property
    def cpu_activity(self) -> str:
        """The activity this game's CPU is charged to in the compute ledger (P3.4.3): its usage ledger's, else its
        own (a game with no model seat has no usage record, but its drives use the host's CPU all the same)."""
        return self.usage_act or f"game:{self.id}"

    def _budget_warning(self, pid: int, payload: dict) -> None:
        """A request of seat ``pid`` took a soft budget past its amount (once a period; on the scheduler's notifier
        thread, no machine lock held): the game's log says so to whoever plays and watches it. The work goes on."""
        text = (payload or {}).get("warning")
        if not text or self._stop or self.crashed:
            return
        try:
            with self.lock:
                self.game.emit("agent_error", f"{self.game.player_name(pid)}'s machine: {text}", None, player=pid)
        except Exception:
            log.debug("could not tell game %s of a soft budget", self.id, exc_info=True)

    BUDGET_POLL_SECONDS = 30.0        # how often a game paused by a spent budget checks whether its period has turned

    def pause_for_budget(self, pid: int, message: str, budget: Optional[dict], retry_at: Optional[datetime] = None):
        """A hard budget refused a request of seat ``pid`` (P3.4.6): pause the whole game with the budget's reason, as
        quiet hours do, and resume it by itself when the budget's period turns (``budget``: its ``id`` and the period
        ``key`` it was spent in). A budget counted in total never turns: the game waits for a person (or the machine's
        owner raising it, then Resume). Called from the seat's own turn, on the driver thread."""
        from ..pool import budgets as pool_budgets
        until = retry_at.timestamp() if retry_at is not None else None
        reason = {"kind": "budget", "player": pid, "message": message, "since": time.time(), "until": until,
                  "budget": (budget or {}).get("id"), "period": (budget or {}).get("key")}
        self.suspend(reason)
        with self.lock:
            self.game.emit("game_paused", f"Game paused: {message}"
                           + (" It resumes by itself when the budget's period ends." if until else ""), None,
                           player=pid)
        if not reason["budget"] or until is None:
            return

        def watch():
            """Resume once the budget's period has turned, unless the pause has since become someone else's."""
            while not self._stop:
                time.sleep(self.BUDGET_POLL_SECONDS)
                if self._stop or not self.paused or self.pause_reason is not reason:
                    return
                key = pool_budgets.keeper().current_key(reason["budget"])
                if key is None or key != reason["period"]:
                    with self.lock:
                        if not self.paused or self.pause_reason is not reason:
                            return
                        self.game.emit("game_resumed", "A new budget period has begun; the game has resumed.", None,
                                       player=pid)
                    self.resume()
                    return
        threading.Thread(target=watch, name=f"budget-{self.id}-{pid}", daemon=True).start()

    def _inline_llm_allowed(self, llm: dict) -> bool:
        """Whether a seat's model block may describe its own provider, address and key rather than name a machine
        (``servers.resolve_llm``'s ``inline_ok``): when the game has no owner (tests, scripts), or its owner is an
        administrator. Read from the database when an agent is made, off the game's lock, and only for a block
        without a machine: a member's game never dials what a seat's block says (P3.2.9)."""
        if not llm or llm.get("server_id") or self.owner_id is None:
            return True
        try:
            from .. import db
            from ..db.models import User
            with db.session() as s:
                owner = s.get(User, self.owner_id)
                return owner is not None and owner.role == "admin" and owner.status in ("probation", "active")
        except Exception:
            traceback.print_exc()
            return False

    def start(self):
        """Start the turn driver, once the model seats' leases are made (``open_leases``): every registered session
        starts here, so its machines are held from then on, whoever moves first."""
        if self._driver is None:
            self.open_leases()
            self.chat.start()
            self._driver = threading.Thread(target=self._drive, daemon=True, name=f"driver-{self.id}")
            self._driver.start()

    @property
    def stopped(self) -> bool:
        """Whether this session has been shut down."""
        return self._stop

    def cancel_agent(self, pid: int, halt: bool = True):
        """Stop a seat's agent, so its configuration can be changed, and halt its chat answers (``halt``): what they
        were answering waits for the seat's next responder or the chat clock, never declined (P3.5.3). Non-blocking."""
        agent = self.agents.pop(pid, None)
        if agent is not None and hasattr(agent, "cancel"):
            agent.cancel()
        if halt:
            from ..agents import responders
            responders.halt(self, pid)

    def _halt_answers(self) -> None:
        """Halt every seat's chat answers (a suspension, a crash, the game's end). Non-blocking."""
        from ..agents import responders
        for pid in list(self._lines):
            responders.halt(self, pid)

    def update_seat(self, pid: int, type: Optional[str] = None, llm: Optional[dict] = None,
                    bot: Optional[dict] = None, name: Optional[str] = None, hybrid: Optional[dict] = None,
                    handicap: Optional[str] = None):
        """Change who controls a seat, or how, and abort a turn in progress so the new controller takes over.

        A new seat type is also the civilization's new engine controller, so its difficulty numbers and the
        decisions the engine takes for it follow the change: those its seat chose itself (``Seat.engine``: its lobby or
        scenario entry's ``handicap`` and ``auto``, or a ``handicap`` given here), and the new controller's defaults for
        the rest. Every one is set explicitly, at every change of controller: the engine keeps a setting once it was
        set explicitly, and a hybrid seat's are, so a seat that was ever hybrid would otherwise carry them through
        every later change. A hybrid seat (design/phase3-features.md P3.6.2) is the engine's ``hybrid`` controller with
        its seat's handicap (``ai`` unless ``handicap`` says otherwise) and the automatic decisions its owners leave to
        the bot (``hybrid.engine_auto``). ``hybrid`` is checked (``normalize_hybrid``) before anything changes. ``bot``
        is the server's (``_seat_bot``): "Best bot" is pinned, as at creation, and the seat keeps its mark of a bot
        chosen as best only while the bot sent names the profile it was pinned to (the Join/Seats dialog sends the
        seat's bot back as it was); a ``chosen_as`` sent is never taken. Raises ValueError for an unknown seat type, a
        handicap that is none, a bot or hybrid settings that are not allowed, and EngineCrash on a game whose engine
        has stopped (the session is then crashed, as everywhere).
        """
        from ..agents import hybrid as hybrid_mod
        moved = False
        if bot is not None:
            # the bot as the server keeps it, "Best bot" pinned to the profile it stands for now (it reads the
            # ratings: off the lock)
            bot = _seat_bot(bot)
        with self.lock:
            seat = self.seats[pid]
            before = (seat.type, (seat.llm or {}).get("server_id"), (seat.llm or {}).get("model"))
            new_type = type or seat.type
            if type and type not in SEAT_TYPES:
                raise ValueError(f"Unknown seat type '{type}' (one of {', '.join(SEAT_TYPES)}).")
            block = None
            if new_type == "hybrid" and (hybrid is not None or seat.type != "hybrid" or handicap is not None):
                block = hybrid_mod.normalize_hybrid(hybrid if hybrid is not None else seat.hybrid,
                                                    benchmark=bool(self.benchmark))
                if handicap is not None or seat.type != "hybrid":
                    handicap = hybrid_mod.check_handicap(handicap)
            own = dict(seat.engine or {})
            if new_type != "hybrid" and handicap is not None:
                own["handicap"] = handicap          # the engine refuses one that is none, below
            try:
                if block is not None:
                    self.game.set_controller(pid, "hybrid", handicap, hybrid_mod.engine_auto(block["owners"]))
                elif new_type != "hybrid" and (type or handicap is not None):
                    self.game.set_controller(
                        pid, new_type, own.get("handicap") or hybrid_mod.default_handicap(new_type),
                        {**hybrid_mod.default_auto(new_type), **(own.get("auto") or {})})
            except EngineCrash as e:
                self._crashed(str(e))
                raise
            seat.engine = own
            if type:
                seat.type = type
            if block is not None:
                seat.hybrid = block
            elif seat.type != "hybrid":
                seat.hybrid = {}
            if llm is not None:
                seat.llm = llm
            if bot is not None:
                old = seat.bot or {}
                if old.get("chosen_as") == "best" and bot.get("chosen_as") != "best" \
                        and bot.get("profile") == old.get("profile"):
                    bot = {**bot, "chosen_as": "best"}      # still the bot that was chosen as the best one
                seat.bot = bot
            if name is not None:
                seat.name = name
            # its diplomacy record values with the seat's bot as it is now, carrying on from the rows it wrote
            self.recorders.pop(pid, None)
            self.cancel_agent(pid)
            if (seat.type, (seat.llm or {}).get("server_id"), (seat.llm or {}).get("model")) != before:
                # another machine, another model, or no model: the old lease goes and the seat's new one is made
                self.release_leases([pid])
                moved = True
            self.cond.notify_all()
            self.chat.note()                # a chat waiting on the seat is timed for its new kind of player
        if moved:
            self._want_leases([pid])
        # a chat that waited on the seat is answered by its new controller (P3.5.3)
        self.dispatch_waiting([pid])

    def set_paused(self, paused: bool):
        """Pause or resume the AI players from the game screen.

        A pause stops the game, clocks included: the turn in progress stops counting, and an AI that is
        mid-turn holds before its next model call until play resumes (see LLMAgent.play_turn). A crashed game stays
        paused (``_crashed``).
        """
        with self.lock:
            if paused == self.paused or self.crashed:
                return
            self.chat.settle()              # the chats' clocks stand still from here, or go on from here
            self.paused = paused
            self.pause_reason = None
            if not paused:
                self.held_from_load = False
            if paused:
                self.metrics.pause()
            else:
                self.metrics.unpause()
            self.cond.notify_all()
            self.chat.republish()
            if not paused:
                self._want_leases()         # a game that had made way holds its machines again
                # its models answer what waited on them meanwhile (P3.5.3)
                self._dispatch_negotiation_interrupts(again=True)

    def suspend(self, reason: Optional[dict] = None):
        """Pause the game immediately, aborting any AI turn in progress (used for quiet hours and benchmark pauses).
        The interrupted turn is excluded from metrics and replayed from its current state on resume().

        ``reason`` is shown on the game screen; any pause replaces the one before it, which is also how a
        disconnect watcher knows that someone else has since paused the game for their own reasons. A crashed game
        keeps its own pause."""
        with self.lock:
            if self.crashed:
                return
            self.chat.settle()
            self.paused = True
            self.pause_reason = reason
            self.metrics.pause()
            for pid in list(self.agents):
                self.cancel_agent(pid)
            self._halt_answers()            # never declined: answered again on resume (P3.5.3)
            self.chat.republish(answers_halted=True)
            if (reason or {}).get("kind") == "queue":
                # it makes way for higher-priority work: its machines are free until it resumes (P3.3.3); every other
                # pause keeps them, since the game resumes where it left off
                self.release_leases(for_good=False)
            self.metrics.interrupt_open()
            self._metrics_current = None
            self.cond.notify_all()
        self.mark_live()
        self._broadcast({"type": "control", "paused": True, "ai_delay": self.ai_delay, "pause_reason": reason})

    def resume(self):
        """Resume a paused game; a crashed one stays paused (``_crashed``)."""
        with self.lock:
            if self.crashed:
                return
            self.chat.settle()
            self.paused = False
            self.pause_reason = None
            self.held_from_load = False
            self.metrics.unpause()
            self._track_turn()
            self.cond.notify_all()
            self.chat.republish()
        self.mark_live()
        self._broadcast({"type": "control", "paused": False, "ai_delay": self.ai_delay, "pause_reason": None})
        self._want_leases()                 # a game that made way holds its machines again
        self.dispatch_waiting()             # the chats waiting on its bots and models are answered (P3.5.3)

    RECONNECT_POLL_SECONDS = 10.0     # how often a game paused by a disconnect checks whether the server is back

    def pause_for_disconnect(self, pid: int, message: str, cfg: dict):
        """Pause the whole game because a seat's model server is unreachable, and resume by itself when it answers.

        Called from the seat's own turn (on the driver thread). A watcher thread then checks the server every
        few seconds; when it is reachable again the game resumes and the interrupted turn is replayed. Pressing
        Resume works too, and pausing for any other reason (quiet hours, a person) retires the watcher.
        """
        from ..agents.llm_agent import reachable
        reason = {"kind": "disconnect", "player": pid, "message": message, "since": time.time()}
        self.suspend(reason)
        with self.lock:
            self.game.emit("game_paused", f"Game paused: {message}. It resumes by itself when the server answers "
                                          f"again, or press Resume.", None, player=pid)

        def watch():
            """Resume once the server is back, unless the pause has since become someone else's."""
            while not self._stop:
                time.sleep(self.RECONNECT_POLL_SECONDS)
                if self._stop or not self.paused or self.pause_reason is not reason:
                    return
                if reachable(cfg):
                    with self.lock:
                        if not self.paused or self.pause_reason is not reason:
                            return
                        self.game.emit("game_resumed", f"{self.game.player_name(pid)}'s model server is reachable "
                                                       f"again; the game has resumed.", None, player=pid)
                    self.resume()
                    return
        threading.Thread(target=watch, name=f"reconnect-{self.id}-{pid}", daemon=True).start()

    QUIET_POLL_SECONDS = 30.0         # how often a game paused for quiet hours checks whether they are over

    def _quiet_hours(self, pid: int, seat) -> bool:
        """Before an AI seat's turn: if its model server is in its restricted (quiet) hours, pause the whole game
        and resume it by itself when they end. Returns True if the game was paused.

        Checked between turns, so a turn in progress always finishes. Lobby games only: benchmark games are
        paused by their scheduler (with its grace period), and probe cases wait in the probe runner.
        """
        if seat.type != "llm" or self.benchmark or not getattr(self, "registered", False):
            return False
        from .. import servers
        server_id = (seat.llm or {}).get("server_id")
        try:
            end = servers.restricted_now(server_id)
        except Exception:
            return False
        if not end:
            return False
        name = servers.server_name(server_id)
        message = f"{name} is in its quiet hours until {servers.restricted_text(server_id, end)}"
        reason = {"kind": "restricted", "player": pid, "message": message, "since": time.time(),
                  "until": end.timestamp()}
        self.suspend(reason)
        with self.lock:
            self.game.emit("game_paused", f"Game paused: {message}. It resumes by itself when they end.", None, player=pid)

        def watch():
            """Resume once the quiet hours are over, unless the pause has since become someone else's."""
            while not self._stop:
                time.sleep(self.QUIET_POLL_SECONDS)
                if self._stop or not self.paused or self.pause_reason is not reason:
                    return
                try:
                    still = servers.restricted_now(server_id)
                except Exception:
                    still = None
                if not still:
                    with self.lock:
                        if not self.paused or self.pause_reason is not reason:
                            return
                        self.game.emit("game_resumed", f"{name}'s quiet hours are over; the game has resumed.", None, player=pid)
                    self.resume()
                    return
        threading.Thread(target=watch, name=f"quiet-{self.id}-{pid}", daemon=True).start()
        return True

    QUEUE_POLL_SECONDS = 10.0         # how often a game that made way checks whether its turn has come back

    def queue_machine(self) -> Optional[str]:
        """The model machine this lobby game's AI plays on (the first live LLM seat's), for the shared queue."""
        if self.benchmark or not getattr(self, "registered", False) or self.game.phase != "playing":
            return None
        for seat in self.seats:
            sid = (seat.llm or {}).get("server_id") if seat.type == "llm" else None
            if sid:
                try:
                    if not self.game.is_alive(seat.player):
                        continue
                except Exception:
                    pass
                return sid
        return None

    def _make_way(self, pid: int, seat) -> bool:
        """Before an AI seat's turn: if higher-priority work is waiting for its machine, pause the game until that
        work is done and nothing else ranks ahead of it. Returns True if the game paused."""
        if seat.type != "llm" or self.benchmark or not getattr(self, "registered", False):
            return False
        from ..pool import queue as work_queue, seats as pool_seats
        from .. import servers
        server_id = (seat.llm or {}).get("server_id")
        prio = work_queue.priority("game", self.id)
        try:
            first = work_queue.preempting(server_id, "game", self.id, prio)
        except Exception:
            return False
        if first is None:
            return False
        name = servers.server_name(server_id)
        # the pause is shown to whoever may watch the game: the work it makes way for is named by its kind only
        # (it may be somebody else's private run, P3.9.1)
        what = pool_seats.kind_text(pool_seats.queue_holder(first))
        reason = {"kind": "queue", "player": pid, "since": time.time(),
                  "message": f"making way on {name} for {what} (priority {first['priority']})"}
        self.suspend(reason)
        with self.lock:
            self.game.emit("game_paused", f"Game paused: {reason['message']}. It carries on by itself when that is done.",
                           None, player=pid)

        def watch():
            """Take the machine back when nothing waiting ranks ahead of this game and the machine is free."""
            while not self._stop:
                time.sleep(self.QUEUE_POLL_SECONDS)
                if self._stop or not self.paused or self.pause_reason is not reason:
                    return
                try:
                    ahead = work_queue.first_ahead(server_id, (-work_queue.priority("game", self.id), self.created),
                                                   exclude=("game", self.id))
                    busy = pool_seats.occupied(server_id, exclude=frozenset({self.id}))
                except Exception:
                    continue
                if ahead is None and not busy:
                    with self.lock:
                        if not self.paused or self.pause_reason is not reason:
                            return
                        self.game.emit("game_resumed", f"{name} is free again; the game has resumed.", None, player=pid)
                    self.resume()
                    return
        threading.Thread(target=watch, name=f"queue-{self.id}-{pid}", daemon=True).start()
        return True

    def stop(self, save_as: Optional[str] = None) -> Optional[Path]:
        """Close the game: halt the driver and abort any AI turn (including in-flight model requests), then let the
        writer finish the saves already taken and close the journal (DESIGN.md P2.5.3). Returns only once the write in
        flight is on the disk, so a session loaded next never shares the journal with this one (and the journal's OS
        lock would refuse it). Nothing is saved after it.

        ``save_as`` takes a last named save in the same hold of the lock that stops the game, so it is the timeline's
        last save: no autosave of a round played after it names more of the journal, which would make every later load of
        it fork a copy of the history (the benchmark scheduler's final save of a job whose game would play on). Returns
        its path once written; what stopped it is raised once the game is closed."""
        if self.usage_act and not self._stop:
            from .. import usage
            try:
                usage.finish_session(self)
            except Exception:
                pass
        final, failed = None, None
        was_stopped = self._stop
        with self.lock:                 # no autosave is between its check and its snapshot as the writer closes
            index = (self.game.turn, self.game.phase)
            if save_as is not None:
                try:
                    if self.read_only:
                        raise RuntimeError("This game was opened to be read, not saved.")
                    final = self._snapshot(_save_name(save_as), autosave=False)   # refused once it was stopped
                except Exception as e:
                    failed = e
            self._stop = True
            self.paused = True
        for pid in list(self.agents):
            self.cancel_agent(pid)
        self._halt_answers()
        self.chat.stop()
        self.release_leases()
        with self.lock:
            self.cond.notify_all()
        self._writer.close()
        if self.journal is not None:
            self.journal.close()
        if not was_stopped and not self.read_only:
            # the games index: closed before its end, or finished (P3.2.5); off the lock, after the last save
            from . import ownership
            ownership.sync_quietly(self.id, turn=index[0], phase=index[1], stopped=True)
        if failed is not None:
            raise failed
        return final.result() if final is not None else None

    def _drive(self):
        """The turn driver: run each AI seat's turn to completion, then move on.

        The loop that makes a game with no humans in it play itself, and the one that must never die - a
        failure in one seat's turn is recorded and skipped rather than ending the game. Only a crash of the engine
        itself (``EngineCrash``) ends it (``_crashed``): the game then takes no more commands.

        A bot seat's turn is driven by the engine and ends inside the drive (BotAgent.play_turn), so the check below
        finds the turn already over and leaves it; any other turn an agent leaves open is closed here.
        """
        while not self._stop and not self.crashed:
            # the chat clock is a thread of its own: one that died is started again, with a log line (P3.9.2)
            self.chat.ensure()
            if self._leases_due:
                # asked for by a thread that held the lock (a resume or a seat change under it): made here, off it
                self._leases_due = False
                self.open_leases()
            with self.lock:
                g = self.game
                if g.phase != "playing":
                    self.cond.wait(timeout=2)
                    continue
                pid = g.current
                seat = self.seats[pid] if pid < len(self.seats) else None
                if self.paused or seat is None or seat.type in ("human", "mcp"):
                    self.cond.wait(timeout=1)
                    continue
                turn_marker = (g.turn, pid)
            if self._quiet_hours(pid, seat) or self._make_way(pid, seat):
                continue
            self._hold_for_autosave(pid)
            agent = self.get_agent(pid)
            if agent is None:
                # the game stopped or crashed since the look above, or the seat is no AI's any more: look again (after a
                # pause for the second, so a seat no agent plays never spins the driver)
                if not (self._stop or self.crashed):
                    with self.lock:
                        self.cond.wait(timeout=1)
                continue
            self.agent_status[pid] = "thinking"
            self._broadcast({"type": "agent", "player": pid, "status": "thinking"})
            # the seat is mid-turn: its lease's slot is kept from pooled prompts until the turn is over (P3.3.3)
            lease = self.leases.get(pid)
            if lease is not None:
                lease.activate()
            try:
                # the turn's CPU on this thread is this game's (P3.4.3); its time waiting for models costs none
                with ledger.cpu_unit(self.cpu_activity):
                    agent.play_turn(self, pid)
            except EngineCrash as e:
                self._crashed(str(e))
            except Exception as e:
                # Recorded and shown to watchers, never emitted on the game: an agent's failure is the server's news,
                # and the game is the engine's to change.
                if not self._stop:
                    self.errors.append({"t": time.time(), "player": pid, "where": "play_turn", "trace": traceback.format_exc()})
                    self._broadcast({"type": "agent_error", "player": pid,
                                     "text": f"{g.player_name(pid)}'s AI failed: {e}"})
            finally:
                if lease is not None:
                    lease.idle()
            if self._stop or self.crashed:
                break
            if getattr(agent, "cancelled", False):
                continue  # controller changed mid-turn: let the new controller play this same turn
            self.agent_status[pid] = "idle"
            self._broadcast({"type": "agent", "player": pid, "status": "idle"})
            with self.lock:
                if (g.turn, g.current) == turn_marker and g.phase == "playing":
                    rec = self.metrics.current(pid)
                    if rec is not None and not rec["end_reason"]:
                        rec["end_reason"] = "end_turn" if seat.has_bot else "ended_by_server"
                    try:
                        before = (g.turn, g.current)
                        # the end_turn tool refuses while a chat the seat is in is open, which would stall the
                        # driver on this turn for good
                        self._close_open_chats(pid, "(no reply in time)")
                        g.execute(pid, "end_turn", {})
                        self._after_action(*before)
                    except ActionError:
                        pass
                    except EngineCrash as e:
                        self._crashed(str(e))
                        break
            if self.ai_delay > 0:
                time.sleep(self.ai_delay)
        if self.crashed:
            # The driver makes a seat's agent after it looks at the game, without the lock: a crash in between has
            # already cancelled the others, and this one goes the same way.
            with self.lock:
                for pid in list(self.agents):
                    self.cancel_agent(pid)

    def wait_for_turn(self, pid: int, timeout: float) -> dict:
        """Long-poll: returns when it's pid's turn, a negotiation awaits pid, or the game ends ("game_over"), or at
        once with "crashed" when the engine has stopped the game (``_crashed``): no turn comes again.

        On pid's own turn with negotiations it is in still waiting on the other side, it waits for their answers
        instead ("negotiation_update" when one comes, "waiting_for_reply" when none has by the timeout): the end_turn
        tool refuses until they are settled, and this is how an agent playing over HTTP or MCP waits for a reply
        without polling. While the game plays, every answer lists the chats pid is in with their deadlines
        (``chats``: ``negotiation_id``, ``with``, ``waiting_on`` you or them, ``deadline`` in epoch seconds,
        ``seconds_left``; the chat clock, P3.5.4): a chat waiting on pid closes, or pid is taken to have declined,
        when its time runs out.
        """
        deadline = time.time() + timeout
        chats = None            # on pid's turn: the open negotiations it is in, as they stood when the wait began
        with self.lock:
            while True:
                res, chats = self._wait_look(pid, deadline, chats)
                if res is not None:
                    if res["status"] not in ("crashed", "game_over", "eliminated"):
                        res["chats"] = self.chat.chats_of(pid)
                    return res
                self.cond.wait(timeout=min(max(0.0, deadline - time.time()), 1.0))

    def _wait_look(self, pid: int, deadline: float, chats) -> tuple:
        """One look of ``wait_for_turn``: (its answer, or None when there is none yet; the chats it watches on pid's
        turn, as they stood when the wait began). Lock held."""
        if self.crashed:
            # the game takes no more moves (_crashed): an agent that kept waiting or acting would loop for good
            return {"status": "crashed", "turn": self.crashed["turn"], "message": CRASHED}, chats
        g = self.game
        pending = [n["id"] for n in g.open_negotiation_heads(pid) if n["awaiting"] == pid]
        if g.phase != "playing":
            return {"status": "game_over", "winner": g.winner, "victory": g.victory}, chats
        if not g.is_alive(pid):
            return {"status": "eliminated"}, chats
        if pending:
            return {"status": "negotiation", "negotiation_ids": pending, "your_turn": g.current == pid}, chats
        if g.current == pid:
            now = self._chat_marks(pid)
            if chats is None:
                chats = now
            changed = sorted(nid for nid, mark in chats.items() if now.get(nid) != mark)
            if changed:
                return {"status": "negotiation_update", "negotiation_ids": changed, "your_turn": True,
                        "turn": g.turn}, chats
            if not chats:
                return {"status": "your_turn", "turn": g.turn}, chats
        if deadline - time.time() <= 0:
            if chats and g.current == pid:
                return {"status": "waiting_for_reply", "negotiation_ids": sorted(chats), "your_turn": True,
                        "turn": g.turn,
                        "note": "No answer yet. Call wait_for_turn again to keep waiting, or withdraw with "
                                "respond_negotiation(action='reject', message=...) and end your turn. A chat closes "
                                "by itself when its time runs out (see chats: seconds_left)."}, chats
            return {"status": "waiting", "turn": g.turn, "current_player": g.current,
                    "current_player_name": g.player_name(g.current)}, chats
        return None, chats

    def _chat_marks(self, pid: int) -> dict:
        """The open negotiations pid is in, each as (awaiting, entries): what an answer changes."""
        return {n["id"]: (n["awaiting"], n["entries"]) for n in self.game.open_negotiation_heads(pid)}

    # ------------------------------------------------------------------
    # Push
    # ------------------------------------------------------------------
    def _on_event(self, ev: dict):
        """Forward a game event to everyone watching. An eliminated seat lets its lease go, and a game that is over
        lets every lease go for good (P3.3.3): nothing will be asked of their machines again. A chat that moved or
        closed leaves the responders' keys, and the chat clock times it as it now stands (P3.5.2, P3.5.3); a closed one
        is told to its two sides (``negotiation_status`` ``closed``) and its conversations let go. Called after the
        engine call that caused it, usually with the lock held: none of this blocks."""
        kind = ev.get("type")
        if kind in DIPLOMACY_EVENTS:
            for seat in self.seats:
                if seat.type != "hybrid":
                    continue
                rec = None
                try:
                    rec = self._recorder(seat.player)
                    rec.on_event(ev)
                except Exception as e:
                    # the record's failure stops it and is the server's news; the event's own effects below still happen
                    if rec is not None:
                        self._record_failed(rec, e)
        if kind == "negotiation":
            self._chat_moved(ev)
        elif kind == "eliminated":
            pid = (ev.get("data") or {}).get("player")
            if isinstance(pid, int) and 0 <= pid < len(self.seats):
                self._seat_out(pid)
        elif kind in ("victory", "game_over"):
            self.release_leases()
        self._broadcast({"type": "event", "event": ev})

    def _chat_moved(self, ev: dict) -> None:
        """A negotiation event (``_on_event``): the chat it names moved (``data.awaiting``: it waits on that seat
        now) or closed (``data.status``)."""
        data = ev.get("data") or {}
        nid = data.get("negotiation")
        if nid is None:
            return
        for key in [k for k in list(self._responding) if k[0] == nid]:
            self._responding.pop(key, None)
        status = data.get("status")
        if status:
            players = [p for p in (ev.get("players") or []) if isinstance(p, int)]
            for p in players:
                fn = getattr(self.agents.get(p), "chat_closed", None)
                if fn is not None:
                    fn(nid)
            self._broadcast({"type": "negotiation_status", "negotiation": nid, "players": players,
                             "waiting_on": None, "kind": None, "state": "closed", "status": status,
                             "position": None, "eta_s": None, "deadline": None})
        try:
            self.chat.note()
        except Exception:
            self.errors.append({"t": time.time(), "where": "chat clock", "trace": traceback.format_exc()})

    def _broadcast(self, msg: dict):
        """Send a message to every subscriber, dropping ones that have gone away."""
        for fn in list(self.subscribers):
            try:
                fn(msg)
            except Exception:
                pass

    # ------------------------------------------------------------------
    # Saves (DESIGN.md P2.5.3)
    # ------------------------------------------------------------------
    @property
    def folder(self) -> Path:
        """The game's folder: its saves (``autosave.citar``, ``turnNNN.citar``, ...) and its journals."""
        return SAVE_DIR / self.id

    def _session_record(self) -> dict:
        """The session's own record, as a save keeps it: what ``from_save`` rebuilds the session from."""
        return {"id": self.id, "name": self.name, "created": self.created,
                "seats": [{**asdict(s), "connected": False} for s in self.seats],
                "spectator_token": self.spectator_token, "benchmark": self.benchmark, "usage_act": self.usage_act,
                "crashed": self.crashed, "machines": dict(self.machines)}

    def _timeline(self):
        """The session's journal, its timeline: opened for a new game before its first save (``open_timeline``:
        ``journal.cjnl``, or the first free ``journal-N.cjnl``), or by ``from_save``, which continues or forks the
        save's; a session that skipped both opens it at its first save. Lock held."""
        if self.journal is None:
            self.journal = _new_journal(self.folder)
        return self.journal

    def open_timeline(self):
        """Open a new game's journal before its first save, without the lock: creating the file and its folder takes
        milliseconds on Windows (a virus scanner looks at every new file), which the first save would otherwise spend
        holding the game's lock (P2.5.3's budget for a save is 10 ms). Only for a session nothing else can reach yet: a
        new one, before it is registered or started, so nothing can save it meanwhile. Never raises: a journal that
        will not open (a folder it cannot make, every name taken) is recorded in ``errors``, and the first save tries
        again under the lock (``_timeline``), where the autosave records what stops it."""
        if self.journal is not None or self.read_only:
            return
        try:
            self.journal = _new_journal(self.folder)
        except Exception:
            self.errors.append({"t": time.time(), "where": "open_timeline", "trace": traceback.format_exc()})

    def _snapshot(self, name: str, autosave: bool) -> "SaveJob":
        """Take a save under the lock and hand it to the writer: the game's snapshot (a copy of its state, and its
        history since the last save, pending in the journal), copies of the session's record and its metrics (the
        metrics as JSON pieces, mostly encoded by earlier saves: ``Metrics.json_parts``), and where it goes. Well under
        a millisecond on a small map, about two at the end of a 24-player gargantuan game (P2.5.3's budget is 10 ms);
        the write itself happens off the lock. What it took is added to ``save_lock``."""
        with self.lock:
            t0 = time.perf_counter()
            if self._stop:
                raise RuntimeError("This game has been closed.")
            journal = self._timeline()
            snap = self.game.save_snapshot(journal)
            job = SaveJob(snap, journal, self.folder / f"{name}.citar", autosave,
                          json.dumps(self._session_record()).encode(), self.metrics.json_parts(), self.game.turn,
                          self.game.phase)
            self._writer.submit(job)
            took = time.perf_counter() - t0
            st = self.save_lock
            st["saves"] += 1
            st["total_s"] += took
            st["last_s"] = took
            st["max_s"] = max(st["max_s"], took)
            return job

    def save(self, filename: Optional[str] = None, wait: bool = True) -> Path:
        """Write a named save (``turnNNN`` when none is given), ``crash-NNN`` included: the snapshot is taken under the
        lock and written by the session's writer, after the saves already taken, while the game plays on. With
        ``wait`` (the save route), returns once it is on the disk and raises what stopped it (an ``OSError``: the old
        file stays, and the history it would have named goes into the next save); call it without the lock held, or the
        game waits for the disk too."""
        if self.read_only:
            raise RuntimeError("This game was opened to be read, not saved.")
        job = self._snapshot(_save_name(filename or f"turn{self.game.turn:03d}"), autosave=False)
        if wait:
            job.result()
        return job.path

    def save_copy(self, path: Path) -> Path:
        """Write the game as it stands to ``path`` with a journal of its own beside it (``<name>.cjnl``, the whole
        history as one record): a save that stands apart from the game's folder and its timeline, as a probe case's.
        Built under the lock (the whole history, once), written off it. Loading it later forks its journal into the
        loaded game's folder. Replaces any copy already at ``path``."""
        path = Path(path)
        journal_path = path.with_suffix(".cjnl")
        with self.lock:
            value = self.game.to_save()
            record = json.dumps(self._session_record()).encode()
            metrics = self.metrics.json_parts()
        metrics = b"".join(metrics)
        copy = EngineGame.from_save(value)
        path.parent.mkdir(parents=True, exist_ok=True)
        journal_path.unlink(missing_ok=True)
        journal, _ = engine_api.open_journal(journal_path)
        try:
            copy.save_snapshot(journal).write(path, journal, record, metrics)
        finally:
            journal.close()
        return path

    LIVE_MARK = "live.json"

    def mark_live(self):
        """Record that this lobby game is open, and whether it is paused, so a restart can bring it back.

        Only games the lobby lists: benchmark games are reloaded by their scheduler, and probe cases are
        not registered at all. A finished game has nothing to come back to, so its mark is removed. A
        game the server paused because a model server was unreachable comes back running: whatever was
        wrong is retried, and the disconnect rule applies again if it is still wrong. So does one paused
        for its machine's budget: its watcher is gone with the process, and its seat's next request is
        refused again (and the game paused, with a watcher) while the budget is still spent.
        """
        if not getattr(self, "registered", False):
            return
        path = SAVE_DIR / self.id / self.LIVE_MARK
        with self._mark_lock:
            if self._stop:
                return
            try:
                if self.benchmark or self.game.phase != "playing":
                    path.unlink(missing_ok=True)
                    return
                # paused by the server (a disconnect, quiet hours, a spent budget) comes back running: the check that
                # paused it runs again
                paused = self.paused and (self.pause_reason or {}).get("kind") not in SERVER_PAUSES
                path.parent.mkdir(parents=True, exist_ok=True)
                # ``leases``: a game opened only to be looked at comes back holding no machine (``holds_machines``)
                path.write_text(json.dumps({"paused": paused, "name": self.name, "at": time.time(),
                                            "leases": bool(self.holds_machines)}), encoding="utf-8")
            except OSError:
                pass

    def unmark_live(self):
        """The game was closed on purpose: do not bring it back on the next start. Call after stop(), so an
        autosave already under way cannot put the mark back."""
        with self._mark_lock:
            try:
                (SAVE_DIR / self.id / self.LIVE_MARK).unlink(missing_ok=True)
            except OSError:
                pass

    def autosave(self, force: bool = False):
        """Write the autosave: once per round (and once when the game ends) unless forced.

        This is what makes a game survive a restart: the scheduler reloads a benchmark from here, restore_live a
        lobby game, and a game interrupted mid-run continues from where its autosave stands. Every round's is taken:
        under the lock it costs a snapshot of the state and the round's history (about a millisecond on a gargantuan
        map), and the writer writes it off the lock. Autosaves the writer has not begun when a newer one comes are
        not written (their history goes into the newer one), so a fast game never queues up a backlog. A crashed game
        is never autosaved again, so its autosave stays the last good state (``_crashed``); a game opened only to be
        read is never saved.
        """
        if self._stop or self.crashed or self.read_only:
            return
        with self.lock:
            g = self.game
            mark = (g.turn, g.phase)
            if not force and mark == self._autosaved:
                return
            self._autosaved = mark
            try:
                self._snapshot("autosave", autosave=True)
            except Exception:
                if not self._stop:
                    self.errors.append({"t": time.time(), "where": "autosave", "trace": traceback.format_exc()})
        self.mark_live()

    #: How long the turn that ends a round waits, at most, for the round's autosave to be written
    #: (``_hold_for_autosave``): a disk that stalls delays a round by this much, never stops the game.
    AUTOSAVE_WAIT_SECONDS = 10.0

    def _ends_round(self, pid: int) -> bool:
        """Whether ``pid``'s turn is the last of its round: no living major civilization (a seat) comes after it.
        City-states and barbarians play inside its end of turn, which starts the next round."""
        g = self.game
        return not any(g.is_alive(q) for q in range(pid + 1, len(self.seats)))

    def _hold_for_autosave(self, pid: int):
        """Before the turn that ends a round, wait until the round's autosave is on the disk. Without the lock.

        The autosave is taken as the round begins and written while its turns are played, so this waits only when a
        write is slower than the rest of the round (a busy disk, a scanner, a fast all-bot round). Without it the
        writer, which passes over autosaves it has not begun, could fall several rounds behind a fast game, and a
        restart (``restore_live``) or a crash would cost all of them. With it the autosave on the disk is never more
        than a round behind the game: the round in progress, and, while that round's own autosave is being written,
        the one before it. A disk that stalls holds the round for ``AUTOSAVE_WAIT_SECONDS`` at most.

        Only ``pid``'s own turn waits (``current`` is read without the lock): an ``end_turn`` sent out of turn is
        refused, and never waits for the writer first."""
        if self.read_only or self.crashed or self.game.current != pid or not self._ends_round(pid):
            return
        self.flush_saves(self.AUTOSAVE_WAIT_SECONDS)

    def _autosave_landed(self, turn: int, phase: str):
        """A round's autosave is on the disk: put the game's turn and status on its row in the games index (P3.2.5).
        Called on the save writer's thread, after the save counts as written, so neither the game's lock nor a wait
        for the saves (``flush_saves``) ever waits on the database. Nothing for a game opened only to be read."""
        if self.read_only or (turn, phase) == self._indexed:
            return
        self._indexed = (turn, phase)
        from . import ownership
        ownership.sync_quietly(self.id, turn=turn, phase=phase)

    def save_writer_stats(self) -> dict:
        """What the session's saves cost off the lock, and the turn of the autosave on the disk (``SaveWriter.stats``)."""
        return self._writer.stats()

    def flush_saves(self, timeout: Optional[float] = None) -> bool:
        """Wait, at most ``timeout`` seconds, until the writer has written every save taken so far; whether it has.
        Never call it with the lock held by a thread the writer waits on (it waits on none)."""
        return self._writer.drain(timeout)

    @classmethod
    def from_save(cls, source, read_only: bool = False, game: Optional[EngineGame] = None) -> "GameSession":
        """Rebuild a session from a save (its path, or ``engine_api.read_save``'s), its seats and their tokens included.

        The game is loaded with the history the save's container names (``EngineGame.from_save``), unless ``game`` is
        that game already loaded (``SessionManager.load`` loads it before it stops a running session). A session to play
        on then takes that journal as its timeline (``_open_timeline``): it continues it when the save is in the game's
        own folder, the journal opens clean and no other save in the folder names more of it, cutting off the records a
        save that never completed left; otherwise (an older save, corruption, a save from elsewhere) it forks the
        save's prefix into a new journal, so every other save that names the old one stays valid. Raises ``ValueError``
        (``LoadError``) for a save that does not load, and when another session holds its journal: stop that session
        first (``SessionManager.load`` does). ``read_only`` reads the history and opens no journal: such a session is
        never saved.
        """
        doc = engine_api.read_save(Path(source)) if isinstance(source, (str, Path)) else source
        sess = doc.session
        g = game if game is not None else EngineGame.from_save(doc)
        seats = [Seat(**{k: v for k, v in s.items() if k in Seat.__dataclass_fields__}) for s in sess.get("seats", [])]
        s = cls(g, seats, sess.get("name", ""), sess.get("id") or doc.path.parent.name)
        if doc.metrics:
            s.metrics = Metrics(doc.metrics)
            s._metrics_current = None
            s._track_turn()
            s.recorders = {}                     # the hybrid seats' records carry on from the rows the save holds
        s.created = sess.get("created", time.time())
        if sess.get("spectator_token"):
            s.spectator_token = sess["spectator_token"]
        s.benchmark = sess.get("benchmark")
        s.usage_act = sess.get("usage_act")
        if isinstance(sess.get("machines"), dict):
            s.machines = {k: dict(v) for k, v in sess["machines"].items() if isinstance(v, dict)}
        if sess.get("crashed"):
            # a crash save: the state as the crash left it, for whoever debugs it, never to play on
            s.crashed = dict(sess["crashed"])
            s.paused = True
            s.pause_reason = {"kind": "crashed", "message": CRASHED, "since": s.crashed.get("at")}
            s.metrics.interrupt_open()           # no turn of it is played again: none is left in progress
            s._metrics_current = None
        if read_only:
            s.read_only = True
        else:
            s.journal = _open_timeline(s.folder, doc)
        return s


class SaveJob:
    """One save the writer writes: the snapshot taken under the lock, its journal, where it goes, and copies of the
    session's record and metrics as JSON (the metrics in pieces, joined off the lock). ``result`` waits for it."""

    __slots__ = ("snap", "journal", "path", "autosave", "session", "metrics", "turn", "phase", "done", "error",
                 "superseded", "ticket")

    def __init__(self, snap, journal, path: Path, autosave: bool, session: bytes, metrics, turn: int = 0,
                 phase: str = "playing"):
        self.snap, self.journal, self.path, self.autosave = snap, journal, path, autosave
        self.session, self.metrics = session, metrics
        self.turn = turn                 # the game's turn when it was taken
        self.phase = phase               # and its phase
        self.ticket = 0                  # its place in the writer's order (SaveWriter.submit)
        self.done = threading.Event()
        self.error: Optional[BaseException] = None
        self.superseded = False          # an autosave a newer one replaced before it was written

    def write(self):
        """Write it (the writer thread): the journal's pending history up to the snapshot, then the container."""
        metrics = self.metrics if isinstance(self.metrics, bytes) else b"".join(self.metrics)
        self.snap.write(self.path, self.journal, self.session, metrics)

    def release(self):
        """Let go of what the write needed, once it is written or passed over: the snapshot is a copy of the whole
        state (tens of megabytes on a late gargantuan map), and the journal must close with its session. Waiting on it
        (``result``) needs only ``done``, ``error`` and ``path``."""
        self.snap = self.journal = self.session = self.metrics = None

    def result(self, timeout: Optional[float] = None) -> Path:
        """Wait until it is written, and raise what stopped it."""
        if not self.done.wait(timeout):
            raise TimeoutError(f"{self.path.name} is not written yet")
        if self.error is not None:
            raise self.error
        return self.path


class SaveWriter:
    """A session's one writer thread (DESIGN.md P2.5.3): it writes the saves the session takes under its lock, in the
    order they were taken, off the lock.

    Autosaves coalesce: one the writer has not begun is passed over when a newer autosave is queued, since the newer
    one's journal records carry its history too (every chunk is appended, in order; only the newest autosave's
    container is written). A named save is always written, and its caller may wait for it (``SaveJob.result``). A save
    that fails is recorded in the session's errors (an autosave's) or raised to whoever waits (a named one's); its
    history stays pending in the journal for the next. The thread starts with the first save and ends when the
    session closes it (``close``, from ``GameSession.stop``), after the saves already taken.
    """

    def __init__(self, session: "GameSession"):
        self._session = weakref.ref(session)
        self._name = f"saves-{session.id}"
        self._cv = threading.Condition()
        self._jobs: deque = deque()
        self._submitted = 0                  # the saves taken so far
        # every save before this ticket is written (or failed), or was passed over for one that is
        self._finished = 0
        self._closed = False
        self._thread: Optional[threading.Thread] = None
        # what the writes cost and where they stand (``stats``): the saves written, the autosaves passed over for a
        # newer one, the writes that failed, the time the writes took, and the turn of the last autosave written
        self._stats = {"written": 0, "passed_over": 0, "failed": 0, "total_s": 0.0, "max_s": 0.0, "last_s": 0.0,
                       "autosave_turn": None}
        # A session dropped without stop() (a test's, an error path's) ends its thread too, once the saves it took are
        # written; the thread keeps nothing of the session's (``SaveJob.release``), so its journal closes with it.
        weakref.finalize(session, self._abandon)

    def submit(self, job: SaveJob):
        """Queue a save; starts the thread with the first."""
        with self._cv:
            if self._closed:
                raise RuntimeError("This game has been closed.")
            job.ticket = self._submitted
            self._submitted += 1
            self._jobs.append(job)
            if self._thread is None:
                self._thread = threading.Thread(target=self._run, name=self._name, daemon=True)
                self._thread.start()
            self._cv.notify_all()

    def _take(self) -> tuple[Optional[SaveJob], list]:
        """The next save to write, and the autosaves passed over for it (a newer queued autosave replaces them; the
        last one queued is never passed over, so there are none without a save to write). Condition held."""
        passed = []
        while self._jobs:
            job = self._jobs.popleft()
            if job.autosave and any(j.autosave for j in self._jobs):
                job.superseded = True
                job.release()                # its history is in the journal's queue, which the newer one appends
                passed.append(job)
                continue
            return job, passed
        return None, passed

    def _run(self):
        while True:
            with self._cv:
                while not self._jobs and not self._closed:
                    self._cv.wait()
                job, passed = self._take()
                if job is None:              # closed, and every save taken is written
                    self._cv.notify_all()
                    return
            t0 = time.perf_counter()
            try:
                job.write()
            except BaseException as e:       # recorded, and the writer goes on: the next save carries the history
                job.error = e
                s = self._session()
                if s is not None and job.autosave:
                    s.errors.append({"t": time.time(), "where": "autosave", "trace": traceback.format_exc()})
                s = None
            finally:
                # nothing of a written save is kept while the thread waits for the next (an idle game's may never come)
                job.release()
                # a passed-over autosave's history reaches the disk with this save's, so it is done only now, and
                # counts as finished (``drain``) with it: its ticket is below this one's
                for p in passed:
                    p.error = job.error
                    p.done.set()
                job.done.set()
                took = time.perf_counter() - t0
                with self._cv:
                    self._finished = job.ticket + 1
                    st = self._stats
                    st["written" if job.error is None else "failed"] += 1
                    st["passed_over"] += len(passed)
                    st["total_s"] += took
                    st["last_s"] = took
                    st["max_s"] = max(st["max_s"], took)
                    if job.autosave and job.error is None:
                        st["autosave_turn"] = job.turn
                    self._cv.notify_all()
                if job.autosave and job.error is None:
                    # only now, once the save counts as written: a wait for the saves never waits on the database
                    s = self._session()
                    if s is not None:
                        try:
                            s._autosave_landed(job.turn, job.phase)
                        except Exception:
                            traceback.print_exc()
                    s = None
                job = passed = None

    def stats(self) -> dict:
        """What the writes cost and where they stand: ``written``, ``passed_over`` (autosaves a newer one replaced),
        ``failed``, ``total_s``, ``max_s`` and ``last_s`` (the writes' time, off the lock), ``autosave_turn`` (the
        game's turn when the last autosave written was taken; None before the first) and ``queued`` (saves taken and
        not yet written)."""
        with self._cv:
            return {**self._stats, "queued": self._submitted - self._finished}

    def _abandon(self):
        """The session is gone without being stopped: write what it took, then end. Never waits (a finalizer)."""
        with self._cv:
            self._closed = True
            self._cv.notify_all()

    def drain(self, timeout: Optional[float] = None) -> bool:
        """Wait until every save taken before the call is written, or passed over for a newer autosave that is written
        (whose history is on the disk with it); whether they were within ``timeout``. Saves taken meanwhile are waited
        for only as the carrier of one passed over: at most the write in flight and one more, so a game that plays on
        cannot keep it waiting."""
        deadline = None if timeout is None else time.monotonic() + timeout
        with self._cv:
            target = self._submitted
            while self._finished < target:
                left = None if deadline is None else deadline - time.monotonic()
                if left is not None and left <= 0:
                    return False
                self._cv.wait(left)
        return True

    def close(self):
        """Take no more saves, write the ones taken, and end the thread. Waits for the write in flight."""
        with self._cv:
            self._closed = True
            self._cv.notify_all()
            thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join()


#: A game folder's journals: ``journal.cjnl``, then ``journal-2.cjnl``, ``journal-3.cjnl``, ...
JOURNAL = "journal"
JOURNAL_SUFFIX = ".cjnl"


#: How many journal names a folder is tried for before a new timeline gives up: far more than a game makes.
MAX_JOURNALS = 10_000


def _journal_names(folder: Path):
    """The journal names of a folder in order, ``journal.cjnl`` first."""
    yield folder / f"{JOURNAL}{JOURNAL_SUFFIX}"
    for n in range(2, MAX_JOURNALS + 1):
        yield folder / f"{JOURNAL}-{n}{JOURNAL_SUFFIX}"


def _no_journal_name(folder: Path) -> OSError:
    """The error when every journal name of a folder is taken."""
    return OSError(f"{folder} has no free journal name left (journal-{MAX_JOURNALS}.cjnl is taken).")


def _new_journal(folder: Path):
    """A new timeline's journal: the first of the folder's journal names that is not taken, opened (DESIGN.md
    P2.5.3). A file that is there already belongs to another timeline, whatever it holds, and is never reused."""
    folder.mkdir(parents=True, exist_ok=True)
    for path in _journal_names(folder):
        if path.exists():
            continue
        try:
            journal, found = engine_api.open_journal(path)
        except ValueError:
            if engine_api.journal_in_use(path):  # another session took it between the look and the open
                continue
            raise
        if found["records"]:                 # written between the look and the open: another timeline's
            journal.close()
            continue
        return journal
    raise _no_journal_name(folder)


def _names_more_of(folder: Path, ref: dict) -> bool:
    """Whether a save in ``folder`` names more of the journal ``ref`` names than ``ref`` does: a later save of the same
    timeline, whose records a continued timeline would overwrite. A save whose header cannot be read might, so it
    counts (forking is always safe)."""
    for p in folder.glob("*.citar"):
        try:
            other = engine_api.save_header(p)["journal"]
        except Exception:
            if _is_v1(p):
                continue                     # a Python engine's save names no journal
            return True
        if other and other["file"] == ref["file"] and other["records"] > ref["records"]:
            return True
    return False


def _save_name(name: str) -> str:
    """A named save's file name, without its suffix: letters, digits, '-', '_' and spaces, at most 60."""
    return "".join(ch for ch in name if ch.isalnum() or ch in "-_ ")[:60] or "save"


def _load_ahead(doc) -> Optional[EngineGame]:
    """What of a save (``engine_api.read_save``'s) can be loaded while a running session of its game plays on
    (``SessionManager.load``): the whole game, history included, when no session holds the journal it names; else (the
    running game's own timeline) its state alone, as a check whose game is not kept, and None. Raises ``ValueError``
    (``LoadError``) for a save that does not load."""
    ref = doc.journal
    if ref is not None:
        try:
            held = engine_api.journal_in_use(doc.path.parent / ref["file"])
        except OSError:
            held = True                      # cannot tell: read it once the running session has let go, as a held one
        if held:
            EngineGame.from_save(doc, history=False)
            return None
    return EngineGame.from_save(doc)


def _is_v1(path: Path) -> bool:
    """Whether a file is a version 1 save (gzip JSON, the Python engine's), which names no journal."""
    try:
        with open(path, "rb") as f:
            return f.read(2) == b"\x1f\x8b"
    except OSError:
        return False


def _open_timeline(folder: Path, doc):
    """The journal a session loaded from ``doc`` (``engine_api.read_save``'s) goes on writing (DESIGN.md P2.5.3).

    It continues the save's own journal when the save is in the game's folder, no save in the folder names more of
    that journal, and the journal opens clean: the records past the save's (a save that never completed) are cut
    off. Otherwise (an older save of a timeline that went on, corruption anywhere in the journal, a save from another
    folder) the save's prefix is forked into a new journal in the game's folder, and the old journal is left as it
    is, so every save that names it still loads. A save that names no journal starts a new one. Call it once the
    game is loaded: reading the history takes the journal's shared lock, which a writer's excludes.
    """
    ref = doc.journal
    if ref is None:
        return _new_journal(folder)
    source = doc.path.parent / ref["file"]
    folder.mkdir(parents=True, exist_ok=True)
    if _same_folder(doc.path.parent, folder) and not _names_more_of(folder, ref):
        # by the game's own folder's path, which its saves are written under (the save's may be another spelling of it)
        journal, found = engine_api.open_journal(folder / ref["file"])
        if found["corrupt_at"] is None:
            try:
                journal.truncate_to(ref)
            except OSError:
                pass                         # the journal stands at ref all the same, and cuts before it appends
            except BaseException:
                journal.close()
                raise
            return journal
        journal.close()                      # corrupt: left as it is, and the save's prefix forked
    for path in _journal_names(folder):
        if path.exists():
            continue
        try:
            engine_api.fork_journal(source, ref, path)
        except OSError:
            if path.exists():                # made between the look and the fork: try the next name
                continue
            raise
        journal, _ = engine_api.open_journal(path)
        return journal
    raise _no_journal_name(folder)


def _same_folder(a: Path, b: Path) -> bool:
    """Whether two paths name one folder (a symlink, a junction or an 8.3 name included)."""
    try:
        return a.resolve() == b.resolve()
    except OSError:
        return False


_MANAGERS: Optional[weakref.WeakSet] = None


def _game_items(want_waiting: bool) -> list[dict]:
    """Lobby games on model machines, for the shared queue: those waiting (they made way), or those playing."""
    out = []
    for s in all_sessions():
        sid = s.queue_machine() if not s.stopped else None
        if not sid:
            continue
        made_way = s.paused and (s.pause_reason or {}).get("kind") == "queue"
        if made_way != want_waiting:
            continue
        out.append({"kind": "game", "id": s.id, "group": s.id, "run": s.name, "label": f"game “{s.name}”",
                    "server_id": sid, "created": s.created,
                    "waiting": (s.pause_reason or {}).get("message") if made_way else None})
    return out


def register_games_with_queue():
    """Let the shared work queue list lobby games (see citar.pool.queue)."""
    from ..pool import queue as work_queue
    work_queue.register("game", lambda: _game_items(True), lambda: _game_items(False))


def all_sessions() -> list:
    """Every live session in this process, across session managers (for "is this machine in use?")."""
    return [s for m in list(_MANAGERS or ()) for s in list(m.sessions.values())]



def _own_engine(sc: dict, stype: str) -> dict:
    """What a seat's entry chose of its engine settings (``Seat.engine``): its ``handicap`` and ``auto``, except a
    hybrid seat's, which follow its owners."""
    if stype == "hybrid":
        return {}
    return {k: sc[k] for k in ("handicap", "auto") if sc.get(k) is not None}


def _seat_settings(sc: dict, stype: str, pid: int) -> tuple:
    """A new seat's settings from its lobby or scenario entry: (handicap, automatic decisions, its ``hybrid`` block,
    its bot). A hybrid seat's block is checked (``normalize_hybrid``), its handicap is ``ai`` unless it says ``human``,
    and its automatic decisions are those its owners leave to the bot (P3.6.2); any other seat's are its entry's own.
    The bot is the server's (``_seat_bot``: "Best bot" pinned). Raises ValueError, naming the seat as the engine's
    ``normalize_seats`` names it ("Seat 1" for P1), for a bot or hybrid settings that are not allowed. Reads the
    ratings, so never under a session's lock."""
    try:
        bot = _seat_bot(sc.get("bot"))
        if stype != "hybrid":
            return sc.get("handicap"), sc.get("auto"), {}, bot
        from ..agents import hybrid
        block = hybrid.normalize_hybrid(sc.get("hybrid"))
        handicap = hybrid.check_handicap(sc.get("handicap"))
    except ValueError as e:
        raise ValueError(f"Seat {pid}: {e}") from None
    return handicap, hybrid.engine_auto(block["owners"]), block, bot


def _clean_bot(bot) -> dict:
    """A seat's bot as a person sends it (a lobby or scenario entry, the seat route), as the server keeps it: an object
    (ValueError for anything else, as for a model block), its aggression a number held to 0..1 or none
    (``profiles.clean_aggression``: a ProfileError, a ValueError, for one that is no number, which the bot would
    only have refused when it came to play), and without ``chosen_as``, which is the server's mark alone (``_seat_bot``
    sets it): a client that sent it with any profile would have a bot labelled "(best)" that was never chosen as
    best."""
    if bot is None:
        return {}
    if not isinstance(bot, dict):
        raise ValueError('A seat\'s bot is an object: {"profile": ..., "aggression": ...}.')
    out = {k: v for k, v in bot.items() if k != "chosen_as"}
    if "aggression" in out:
        from ..bots.profiles import clean_aggression
        out["aggression"] = clean_aggression(out["aggression"])
    return out


def _seat_bot(bot) -> dict:
    """A seat's bot (``_clean_bot``), with "Best bot" pinned: a seat asking for "best" gets the profile that is
    best-ranked right now, recorded in the seat with ``chosen_as: "best"``, so the game keeps playing (and reporting)
    that bot even after the rankings change. Reads the ratings, so never under a session's lock."""
    out = _clean_bot(bot)
    if out.get("profile") != "best":
        return out
    from ..bots.ratings import best_profile
    return {**out, "profile": best_profile(), "chosen_as": "best"}

class SessionManager:
    """Every live game, and the operations that create or load one."""
    def __init__(self):
        global _MANAGERS
        self.sessions: dict[str, GameSession] = {}
        self.lock = threading.Lock()
        self._loading = threading.Lock()     # one load at a time: a load stops, reads and replaces a session
        if _MANAGERS is None:
            _MANAGERS = weakref.WeakSet()
        _MANAGERS.add(self)

    def create(self, config: dict, seats_cfg: list[dict], name: str = "", track: bool = True,
               start: bool = True, owner_id: Optional[str] = None) -> GameSession:
        """Create a game from a lobby configuration and start its session.

        ``start=False`` leaves the game stopped so the caller can finish setting it up first: the driver plays
        the first AI turn at once, so anything that turn reads (``benchmark``, which exempts the game from the
        lobby's quiet hours and queue, ``owner_id``, which decides what its model seats may dial) must be in place
        before ``start()``.
        """
        players = []
        seats = []
        for i, sc in enumerate(seats_cfg):
            stype = sc.get("type", "bot")
            if stype not in SEAT_TYPES:
                raise ValueError(f"Unknown seat type '{stype}'")
            handicap, auto, block, bot = _seat_settings(sc, stype, i)
            players.append({"name": sc.get("civ_name") or None, "color": sc.get("color"),
                            "leader": sc.get("leader"), "nation": sc.get("nation"), "controller": stype,
                            "handicap": handicap, "auto": auto, "difficulty": sc.get("difficulty") or None})
            seats.append(Seat(player=i, type=stype, name=sc.get("name") or "", llm=sc.get("llm") or {},
                              bot=bot, hybrid=block, engine=_own_engine(sc, stype)))
        cfg = dict(config)
        cfg["players"] = players
        game = EngineGame.new(cfg)
        s = GameSession(game, seats, name)
        # what the engine was given, so a hybrid benchmark job's control game is made from the very same settings
        # (citar.server.controls, design/phase3-features.md P3.6.9)
        s.engine_config = json.loads(json.dumps(cfg))
        s.owner_id = owner_id
        s.open_timeline()                    # before anything can reach the session, so before anything can save it
        s.registered = True
        with self.lock:
            self.sessions[s.id] = s
        if track:
            self.track(s)
        s.autosave(force=True)
        if start:
            s.start()
        return s

    @staticmethod
    def track(s: "GameSession"):
        """Record the game in the usage ledger (games with a seat a model plays, hybrid seats included; others cost
        next to nothing)."""
        if not any(seat.model_seat for seat in s.seats) and not s.usage_act:
            return
        from .. import usage
        try:
            if s.benchmark:
                b = s.benchmark
                usage.track_session(s, "benchmark", parent={"kind": "benchmark_run", "id": b.get("run_id"), "name": b.get("run_name")},
                                    ref={"job_id": b.get("job_id"), "scenario": b.get("scenario"), "suite_id": b.get("suite_id")})
            else:
                usage.track_session(s, "game")
        except Exception:
            traceback.print_exc()

    def create_from_scenario(self, scn: dict, seats_cfg: Optional[list] = None, name: str = "",
                             register: bool = True, start: bool = True,
                             check_seat: Optional[Callable[[dict], dict]] = None) -> GameSession:
        """A new game from a scenario's saved state. Seats default to the scenario's; a "script" seat (probes) is
        driven from outside, so in a normal game it behaves like a human seat. ``check_seat`` sees each seat as the
        scenario's defaults and ``seats_cfg`` make it, before anything is set up, and returns it (cleaned) or raises:
        the launch route's gate on members' model blocks (P3.2.9)."""
        g = EngineGame.from_state(scn["state"])
        defaults = scn.get("seats") or []
        seats = []
        for p in g.majors():
            pid = p["id"]
            sc = dict(defaults[pid]) if pid < len(defaults) else {}
            if seats_cfg and pid < len(seats_cfg) and seats_cfg[pid]:
                given = seats_cfg[pid]
                if sc.get("type") == "hybrid" and given.get("type") not in (None, "hybrid"):
                    # a hybrid seat's handicap is one of its hybrid settings, not a choice for whatever the launch seats
                    # there instead (a bot with a person's numbers): it goes with the settings, unless the launch names one
                    sc.pop("hybrid", None)
                    sc.pop("handicap", None)
                sc.update({k: v for k, v in given.items() if v is not None})
            if check_seat is not None:
                sc = check_seat(sc)
            stype = sc.get("type") or "bot"
            if stype == "script":
                stype = "human"
            if stype not in SEAT_TYPES:
                raise ValueError(f"Unknown seat type '{stype}'")
            handicap, auto, block, bot = _seat_settings(sc, stype, pid)
            g.set_controller(pid, stype, handicap, auto)
            if sc.get("difficulty"):
                g.set_difficulty(pid, sc["difficulty"])
            seats.append(Seat(player=pid, type=stype, name=sc.get("name") or sc.get("label") or "",
                              llm=sc.get("llm") or {}, bot=bot, hybrid=block, engine=_own_engine(sc, stype)))
        s = GameSession(g, seats, name or scn.get("name") or "Scenario")
        if register:
            self.register_new(s)
        if start:
            s.start()
        return s

    def register_new(self, s: GameSession):
        """A new game made with ``register=False`` joins the live ones: its timeline, the list, the usage ledger and
        its first autosave, as ``create_from_scenario`` does with ``register``. For a caller that checks the game
        before anything of it reaches the disk (the scenario launch's admission). Not started."""
        s.open_timeline()                    # before anything can reach the session, so before anything can save it
        s.registered = True
        with self.lock:
            self.sessions[s.id] = s
        self.track(s)
        s.autosave(force=True)

    def load(self, path: Path, kind: Optional[str] = None, leases: bool = True) -> GameSession:
        """Load a save into a live session, paused.

        The save is read first, while a running session of its game plays on, and as much of it is loaded as can be
        without that session's journal (``_load_ahead``): a save that is damaged, of another version, or whose state
        does not load is refused with the running game untouched, and so is one whose history (in a journal no session
        holds) cannot be read. A save of a game running here is usually on that game's timeline, whose journal the
        running session holds (the OS lock keeps every other reader and writer out): its history is read once that
        session is stopped, its writer drained and its journal closed. If it then does not load, the running game comes
        back from its own autosave, so a bad save never closes a game. Raises what stopped the load (``ValueError`` for
        a save that does not load). ``kind`` is the games index's kind for a game that has no row yet
        (``ownership.index_loaded``): `probe` for a probe case opened to be looked at. ``leases`` False: the game is
        opened only to be looked at (a finished benchmark job's, a probe case's end state), so it holds no machine
        until one of its seats is asked something (``GameSession.holds_machines``).
        """
        path = Path(path)
        with self._loading:
            doc = engine_api.read_save(path)
            sid = doc.header["session"]["id"] or path.parent.name
            with self.lock:
                live = self.sessions.get(sid)
            running = live is not None and not live.stopped
            paused = live is not None and live.paused       # stop() pauses it
            game = _load_ahead(doc) if running else None
            if live is not None:
                live.stop()
            try:
                if live is not None and engine_api.save_header(path) != doc.header:
                    doc, game = engine_api.read_save(path), None    # the stopped session's writer has written it since
                s = GameSession.from_save(doc, game=game)
            except BaseException:
                if running:                      # a game closed before the load stays closed
                    self._reopen(live, paused)
                raise
            self._register_loaded(s, kind=kind, leases=leases)
            return s

    def _register_loaded(self, s: GameSession, kind: Optional[str] = None, leases: bool = True):
        """A loaded session joins the live ones, paused, its driver started and its model seats' leases made (unless
        ``leases`` is False: ``load``). Its row in the games index says it is played again (one is made for a game
        without: ``ownership.index_loaded``), and gives the session its owner, before the leases and the driver."""
        from . import ownership
        s.registered = True
        s.holds_machines = leases
        s.owner_id = ownership.index_loaded(s, kind=kind)
        s._indexed = (s.game.turn, s.game.phase)
        with self.lock:
            self.sessions[s.id] = s
        s.paused = True
        # its models answer no chat until it is resumed (P3.5.3: resume, set_paused(False)), whatever acts in it first
        # (a bot's answer, a person's move), since a loaded game is paused so that its model configuration can be
        # checked before anything spends tokens
        s.held_from_load = True
        s.start()               # its chat clock is new: every open chat is timed afresh from here (ChatClock.start)
        self.track(s)
        s.mark_live()
        # its bots answer the chats waiting on them now (a hybrid seat's bot its own part: its model's waits for the
        # resume); a game opened only to be looked at answers nothing (its end state is what is looked at)
        if leases:
            s.dispatch_waiting([p for p, seat in enumerate(s.seats) if seat.has_bot])

    def _reopen(self, live: GameSession, paused: bool):
        """Bring back a game ``load`` stopped for a save that then did not load (its history, in the journal the game
        held, damaged or of another timeline; its timeline not opened): from its own autosave, paused or not as it was.
        If that fails too the game stays closed."""
        with self.lock:
            if self.sessions.get(live.id) is live:
                del self.sessions[live.id]
        try:
            s = GameSession.from_save(live.folder / "autosave.citar")
        except Exception:
            traceback.print_exc()
            return
        self._register_loaded(s, leases=live.holds_machines)
        if not paused:
            s.resume()

    def restore_live(self) -> list[str]:
        """Bring back the lobby games that were open when the server last stopped.

        Every open lobby game keeps a mark beside its autosave (see ``GameSession.mark_live``). Each is
        reloaded from that autosave, at most a round behind the game (a round's last turn waits for the round's
        autosave, ``GameSession._hold_for_autosave``): a restart costs the round in progress and, while that round's own
        autosave was still being written, the one before it. It is then resumed
        unless it was paused. Benchmark games are the scheduler's to reload, and are not marked. A game whose autosave
        the Python engine wrote (a server upgraded from 0.1.5) never loads: it is named once and its mark removed, so it
        is not tried again at every start.
        """
        restored = []
        for mark in sorted(SAVE_DIR.glob(f"*/{GameSession.LIVE_MARK}")):
            save = mark.parent / "autosave.citar"
            try:
                state = json.loads(mark.read_text(encoding="utf-8"))
                if not save.exists() or mark.parent.name in self.sessions:
                    continue
                # a game opened only to be looked at (a probe case's end state) still holds no machine
                s = self.load(save, leases=state.get("leases", True) is not False)
                if s.benchmark or s.game.phase != "playing":
                    s.unmark_live()
                    continue
                if not state.get("paused"):
                    s.resume()
                restored.append(f"{s.name} (turn {s.game.turn}{', paused' if state.get('paused') else ''})")
            except ValueError as e:
                if not _is_v1(save):
                    traceback.print_exc()
                    continue
                print(f"Game {mark.parent.name} is not restored: {e}", flush=True)
                try:
                    mark.unlink(missing_ok=True)
                except OSError:
                    pass
            except Exception:
                traceback.print_exc()
        return restored

    FINISHED_GRACE_SECONDS = 600.0    # how long a finished lobby game stays in the list for its players

    def close_finished(self, now: Optional[float] = None) -> list[str]:
        """Close lobby games that have been over for a while and that nobody is watching.

        A finished game used to stay under the lobby's current games until someone pressed Close. It stays for a
        few minutes after the end so its players see the result, then leaves the list; its saves (and replay)
        are kept, as with Close. Benchmark games are released by their scheduler in the same way.
        """
        now = now or time.time()
        closed = []
        for s in list(self.sessions.values()):
            if s.benchmark or s.stopped or s.game.phase == "playing":
                s.__dict__.pop("_over_since", None)
                continue
            since = s.__dict__.setdefault("_over_since", now)
            if now - since >= self.FINISHED_GRACE_SECONDS and not s.subscribers:
                try:
                    self.delete(s.id, save_as="final")
                except Exception:
                    pass
                closed.append(s.name)
        return closed

    def get(self, sid: str) -> Optional[GameSession]:
        """A session by id, or None."""
        return self.sessions.get(sid)

    def delete(self, sid: str, save_as: Optional[str] = None):
        """Close a game: its session stops and leaves the list; its saves stay. ``save_as`` takes a last named save as it
        stops (``GameSession.stop``), whose error is raised once the game is closed."""
        with self.lock:
            s = self.sessions.pop(sid, None)
        if s:
            try:
                s.stop(save_as)
            finally:
                s.unmark_live()

    _save_meta_cache: dict = {}

    @classmethod
    def save_meta(cls, p: Path) -> dict:
        """Game name, turn and players of a save file (cached by modification time), from its header alone: never the
        state (DESIGN.md P2.5.1). A save this version cannot read says why."""
        st = p.stat()
        key = str(p)
        hit = cls._save_meta_cache.get(key)
        if hit and hit[0] == st.st_mtime:
            return hit[1]
        meta: dict = {}
        try:
            header = engine_api.save_header(p)
            summ, sess = header["summary"], header["session"]
            seats = summ.get("seats") or []
            meta = {"game_name": sess.get("name"), "turn": summ["turn"], "phase": summ["phase"],
                    "turn_limit": summ["turn_limit"], "benchmark": bool(sess.get("benchmark")),
                    "players": [{"name": pl["name"], "alive": pl["alive"],
                                 "seat": seats[i] if i < len(seats) else None}
                                for i, pl in enumerate(summ["majors"])]}
            if summ["winner"] is not None:
                meta["winner"] = summ["winner"]
        except Exception as e:
            meta = {"unreadable": True, "error": str(e)[:300]}
        cls._save_meta_cache[key] = (st.st_mtime, meta)
        return meta

    @classmethod
    def list_saves(cls) -> list[dict]:
        """Every save on disk, with what is in it."""
        out = []
        if not SAVE_DIR.exists():
            return out
        for p, st in fsutil.newest_first(SAVE_DIR.glob("*/*.citar")):
            try:
                meta = cls.save_meta(p)
            except FileNotFoundError:
                continue                    # deleted since the listing
            out.append({"path": str(p.relative_to(SAVE_DIR)).replace("\\", "/"), "game_id": p.parent.name,
                        "name": p.stem, "modified": st.st_mtime, "size": st.st_size, **meta})
        return out

    #: How old a temporary file beside a save must be before a deletion removes it: a fresher one may be a write in
    #: progress, of this process or of another sharing the folder.
    STALE_TEMP_SECONDS = 300.0

    @classmethod
    def delete_save(cls, rel_path: str, whole_game: bool = False) -> list[str]:
        """Delete one save, or every save of its game, and the journals no save left in the folder names (DESIGN.md
        P2.5.3), except one a session is writing (a new timeline's, which no save names yet). Returns the saves
        deleted."""
        # Two paths to the same file, on purpose. `p` stays rooted at SAVE_DIR as written, so the
        # names reported back can be made relative to it; `p.resolve()` is what the containment
        # check has to use, because that is what stops a `..` in rel_path escaping the directory.
        #
        # Mixing them was a bug: resolving the candidate and comparing it against an unresolved
        # SAVE_DIR raised "is not in the subpath of" wherever the two differ - a macOS temporary
        # directory (/var -> /private/var), a Windows 8.3 short path, or any save directory reached
        # through a symlink or a junction. Deleting a save failed there and nowhere else.
        p = SAVE_DIR / rel_path
        if SAVE_DIR.resolve() not in p.resolve().parents or not p.exists() or p.suffix != ".citar":
            raise KeyError(rel_path)
        targets = sorted(p.parent.glob("*.citar")) if whole_game else [p]
        removed = []
        for q in targets:
            q.unlink()
            removed.append(str(q.relative_to(SAVE_DIR)).replace("\\", "/"))
        cls._delete_unnamed_journals(p.parent)
        try:
            now = time.time()
            for rest in p.parent.iterdir():
                if rest.suffix == ".tmp" and now - rest.stat().st_mtime > cls.STALE_TEMP_SECONDS:
                    rest.unlink()
            if not any(p.parent.iterdir()):
                p.parent.rmdir()
        except OSError:
            pass
        return removed

    @staticmethod
    def _delete_unnamed_journals(folder: Path):
        """Remove the journals of ``folder`` that no save there names and no session holds. When a save's header cannot
        be read, what it names cannot be told, and every journal stays."""
        named = set()
        for q in folder.glob("*.citar"):
            try:
                ref = engine_api.save_header(q)["journal"]
            except Exception:
                if _is_v1(q):
                    continue
                return
            if ref:
                named.add(ref["file"])
        for j in folder.glob(f"*{JOURNAL_SUFFIX}"):
            try:
                if j.name not in named and not engine_api.journal_in_use(j):
                    j.unlink()
            except OSError:
                pass
