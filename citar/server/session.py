"""Game sessions: seats, turn driver thread, AI agents, negotiation interrupts, push updates and saves."""
from __future__ import annotations

import gzip
import json
import secrets
import threading
import time
import traceback
import weakref
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any, Callable, Optional

from ..engine import tools
from ..engine.game import Game, ActionError
from ..engine.state import GameState
from ..engine.rules import RULES_VERSION
from .metrics import Metrics
from .. import paths

# CITAR_SAVE_DIR lets the test suite keep its throwaway games out of the real save folder;
# citar.paths decides the rest (a checkout writes beside the code, an install writes per-user).
SAVE_DIR = paths.saves_path()
SEAT_TYPES = ("human", "mcp", "llm", "bot")


@dataclass
class Seat:
    """One seat in a game: who plays it, and the token that proves it.

    A seat token grants play on exactly this seat and nothing else, which is what lets an external
    agent join a game with no account at all.
    """
    player: int
    type: str = "bot"                 # human | mcp | llm | bot
    token: str = field(default_factory=lambda: secrets.token_urlsafe(12))
    name: str = ""
    llm: dict = field(default_factory=dict)   # provider, model, base_url, api_key_env, tool_mode, persona, ...
    bot: dict = field(default_factory=dict)   # aggression
    connected: bool = False

    def public(self, include_token: bool = False) -> dict:
        """The seat as the client sees it. The token is included only for whoever may have it."""
        d = {"player": self.player, "type": self.type, "name": self.name, "connected": self.connected,
             "llm": {k: v for k, v in self.llm.items() if k not in ("api_key",)}, "bot": self.bot}
        if self.type == "llm" and self.llm.get("server_id"):
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
    which is why the replay is assembled under the lock rather than streamed.
    """
    def __init__(self, game: Game, seats: list[Seat], name: str = "", session_id: Optional[str] = None):
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
        self._responding: set = set()
        self._stop = False
        self._driver: Optional[threading.Thread] = None
        self._last_round = game.turn
        self.version = 0
        self.metrics = Metrics()
        self._metrics_current: Optional[tuple] = None
        self.benchmark: Optional[dict] = None   # {"run_id", "job_id", "suite", "scenario", "model", ...} for benchmark games
        self.usage_act: Optional[str] = None    # usage ledger activity id (usage.py)
        game.listeners.append(self._on_event)
        self._track_turn()

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
        return self.game.s.phase != "playing" or not any(s.type == "human" for s in self.seats)

    def info(self, include_tokens: bool = False) -> dict:
        """The game's summary, as the lobby shows it."""
        g = self.game
        d = {
            "id": self.id, "name": self.name, "turn": g.turn, "phase": g.s.phase, "current_player": g.s.current,
            "winner": g.s.winner, "victory": g.s.victory, "paused": self.paused, "ai_delay": self.ai_delay,
            "pause_reason": self.pause_reason if self.paused else None,
            "created": self.created, "config": {k: g.s.config.get(k) for k in (
                "map_size", "map_type", "speed", "difficulty", "barbarian_difficulty", "barbarians", "turn_limit", "victories", "city_states", "religion",
                "espionage", "tech_trading", "ruins", "seed", "map_edges", "wrap_x", "wrap_y", "river_density",
                "resources", "on_disconnect", "reconnect_seconds")},
            "players": [{"id": p.id, "name": p.name, "color": p.color, "alive": p.alive, "kind": p.kind,
                         **({"difficulty": p.difficulty or g.s.config.get("difficulty")} if p.kind == "major" else {})}
                        for p in g.s.players],
            "seats": [s.public(include_tokens) for s in self.seats],
            "agent_status": {str(k): v for k, v in self.agent_status.items()},
            "agent_errors": {str(k): a.last_error for k, a in self.agents.items() if getattr(a, "last_error", None)},
            "god_view_allowed": self.god_view_allowed(),
            "benchmark": self.benchmark,
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
        with self.lock:
            before_turn, before_current = self.game.turn, self.game.s.current
            kind = tools.REGISTRY[name].kind if name in tools.REGISTRY else "unknown"
            t0 = time.perf_counter()
            try:
                result = tools.execute(self.game, pid, name, args or {})
                ok = True
            except ActionError as e:
                self.metrics.tool_call(pid, name, args or {}, kind, False, time.perf_counter() - t0, str(e))
                return {"ok": False, "error": str(e)}
            except Exception as e:  # engine bug: report, don't crash the server
                self.errors.append({"t": time.time(), "player": pid, "tool": name, "args": args,
                                    "trace": traceback.format_exc()})
                self.metrics.tool_call(pid, name, args or {}, kind, False, time.perf_counter() - t0, f"Internal error: {e}")
                return {"ok": False, "error": f"Internal error: {e}"}
            self.metrics.tool_call(pid, name, args or {}, kind, True, time.perf_counter() - t0)
            if tools.REGISTRY[name].kind == "action":
                self._after_action(before_turn, before_current)
            if name in ("open_negotiation", "respond_negotiation") and isinstance(result, dict):
                nid = result.get("negotiation_id") or (args or {}).get("negotiation_id")
                if nid and wait_negotiation > 0:
                    result = self._wait_negotiation_reply(pid, int(nid), wait_negotiation, result)
            return {"ok": ok, "result": result}

    def _track_turn(self):
        """Close the metrics record of the player whose turn ended and open one for the player now to move."""
        g = self.game
        key = (g.turn, g.s.current, g.s.phase)
        if key == self._metrics_current:
            return
        if self._metrics_current is not None:
            _, prev, _ = self._metrics_current
            self.metrics.end_turn(prev)
        self._metrics_current = key
        if g.s.phase == "playing" and g.s.current < len(self.seats) and g.player(g.s.current).alive:
            seat = self.seats[g.s.current]
            self.metrics.begin_turn(g.s.current, g.turn, seat.type, seat.llm.get("model") if seat.type == "llm" else None)

    def metrics_report(self) -> dict:
        """Per-seat metrics for this game, as the stats screen and reports use them."""
        players = {}
        for s in self.seats:
            p = self.game.player(s.player)
            players[s.player] = {"name": p.name, "controller": s.type,
                                 "model": s.llm.get("model") if s.type == "llm" else None,
                                 "settings": {k: s.llm.get(k) for k in ("provider", "base_url", "tool_mode", "reasoning_effort", "effort")}
                                 if s.type == "llm" else None}
        return {"summary": self.metrics.summary(players), "turns": self.metrics.turn_rows(),
                "negotiations": self.metrics.data["negotiations"], "game_turn": self.game.turn}

    def _after_action(self, before_turn: int, before_current: int):
        """Bump the version, notify watchers and autosave after something changed."""
        self.version += 1
        g = self.game
        self._track_turn()
        self._dispatch_negotiation_interrupts()
        if g.s.current != before_current or g.turn != before_turn:
            self.cond.notify_all()
            self._broadcast({"type": "turn", "turn": g.turn, "current_player": g.s.current, "phase": g.s.phase})
            if g.turn != self._last_round or g.s.phase != "playing":
                self._last_round = g.turn
                self.autosave()
        else:
            self.cond.notify_all()
        self._broadcast({"type": "update", "version": self.version, "turn": g.turn, "current_player": g.s.current,
                         "phase": g.s.phase})

    def _wait_negotiation_reply(self, pid: int, nid: int, timeout: float, result: dict) -> dict:
        """Block until the other side answers a negotiation, or the wait runs out."""
        from ..engine.diplomacy import get_negotiation, negotiation_view
        deadline = time.time() + timeout
        while True:
            n = get_negotiation(self.game, nid)
            if n["status"] != "open" or n["awaiting"] == pid:
                break
            left = deadline - time.time()
            if left <= 0:
                result = dict(result)
                result["note"] = "No reply yet. Continue your turn; the reply will show up in get_diplomacy/get_briefing."
                return result
            self.cond.wait(timeout=min(left, 1.0))
        n = get_negotiation(self.game, nid)
        view = negotiation_view(self.game, n, pid)
        last = view["history"][-1] if view["history"] else None
        result = dict(result)
        result["negotiation"] = {"id": nid, "status": n["status"], "your_move": n["awaiting"] == pid,
                                 "their_last": last if last and not last["you"] else None,
                                 "current_proposal": view["current_proposal"]}
        return result

    # ------------------------------------------------------------------
    # Negotiation interrupts for AI seats
    # ------------------------------------------------------------------
    def _dispatch_negotiation_interrupts(self):
        """Wake the seats that owe an answer to an open negotiation.

        This is why an agent should wait on ``wait_for_turn`` rather than polling for its own turn: a
        negotiation opened on somebody else's turn blocks that turn until it is answered.
        """
        for n in self.game.s.negotiations:
            if n["status"] != "open" or n["awaiting"] is None:
                continue
            pid = n["awaiting"]
            seat = self.seats[pid] if pid < len(self.seats) else None
            key = (n["id"], len(n["history"]))
            if seat is None or key in self._responding:
                continue
            if seat.type in ("bot", "llm") and self.game.s.current != pid:
                # the current player's own agent handles replies during its turn loop
                self._responding.add(key)
                threading.Thread(target=self._run_responder, args=(pid, n["id"], key), daemon=True).start()
            elif seat.type in ("bot", "llm") and self.game.s.current == pid and n["initiator"] != pid:
                self._responding.add(key)
                threading.Thread(target=self._run_responder, args=(pid, n["id"], key), daemon=True).start()

    def _run_responder(self, pid: int, nid: int, key):
        """Run an AI seat's answer to a negotiation, off the turn driver."""
        if self._stop:
            return
        agent = self.get_agent(pid)
        try:
            if agent is not None:
                agent.respond_negotiation(self, pid, nid)
        except Exception:
            self.errors.append({"t": time.time(), "player": pid, "where": "negotiation responder",
                                "trace": traceback.format_exc()})
        finally:
            if not self._stop:
                self._reject_if_unanswered(pid, nid, key)

    def _reject_if_unanswered(self, pid: int, nid: int, key):
        """Reject a negotiation nobody answered, so the game cannot stall on it."""
        from ..engine.diplomacy import get_negotiation
        with self.lock:
            try:
                n = get_negotiation(self.game, nid)
                if n["status"] == "open" and n["awaiting"] == pid and len(n["history"]) == key[1]:
                    # agent failed to respond: reject so the other side isn't stuck
                    tools.execute(self.game, pid, "respond_negotiation",
                                  {"negotiation_id": nid, "action": "reject", "message": "(no response)"})
                    self._after_action(self.game.turn, self.game.s.current)
            except ActionError:
                pass

    # ------------------------------------------------------------------
    # Agents
    # ------------------------------------------------------------------
    def get_agent(self, pid: int):
        """The agent driving a seat, created on first use."""
        if pid in self.agents:
            return self.agents[pid]
        seat = self.seats[pid]
        agent = None
        if seat.type == "bot":
            from ..agents.bot_agent import BotAgent
            agent = BotAgent(**{k: v for k, v in seat.bot.items() if k in ("aggression",)})
        elif seat.type == "llm":
            from ..agents.llm_agent import LLMAgent
            from .. import servers
            try:
                cfg = servers.resolve_llm(seat.llm)
            except servers.ServerError as e:
                self.game.emit("agent_error", f"{self.game.player(pid).name}'s AI: {e} Pick a server for this seat.", None, player=pid)
                cfg = dict(seat.llm)
            agent = LLMAgent(cfg)
        if agent is not None:
            self.agents[pid] = agent
        return agent

    def start(self):
        """Start the turn driver."""
        if self._driver is None:
            self._driver = threading.Thread(target=self._drive, daemon=True, name=f"driver-{self.id}")
            self._driver.start()

    @property
    def stopped(self) -> bool:
        """Whether this session has been shut down."""
        return self._stop

    def cancel_agent(self, pid: int):
        """Stop a seat's agent, so its configuration can be changed."""
        agent = self.agents.pop(pid, None)
        if agent is not None and hasattr(agent, "cancel"):
            agent.cancel()

    def suspend(self, reason: Optional[dict] = None):
        """Pause the game immediately, aborting any AI turn in progress (used for quiet hours and benchmark pauses).
        The interrupted turn is excluded from metrics and replayed from its current state on resume().

        ``reason`` is shown on the game screen; any pause replaces the one before it, which is also how a
        disconnect watcher knows that someone else has since paused the game for their own reasons."""
        with self.lock:
            self.paused = True
            self.pause_reason = reason
            for pid in list(self.agents):
                self.cancel_agent(pid)
            self.metrics.interrupt_open()
            self._metrics_current = None
            self.cond.notify_all()
        self.mark_live()
        self._broadcast({"type": "control", "paused": True, "ai_delay": self.ai_delay, "pause_reason": reason})

    def resume(self):
        """Resume a paused game."""
        with self.lock:
            self.paused = False
            self.pause_reason = None
            self._track_turn()
            self.cond.notify_all()
        self.mark_live()
        self._broadcast({"type": "control", "paused": False, "ai_delay": self.ai_delay, "pause_reason": None})

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
                        self.game.emit("game_resumed", f"{self.game.player(pid).name}'s model server is reachable "
                                                       f"again; the game has resumed.", None, player=pid)
                    self.resume()
                    return
        threading.Thread(target=watch, name=f"reconnect-{self.id}-{pid}", daemon=True).start()

    def stop(self):
        """Close the game: halt the driver and abort any AI turn (including in-flight model requests)."""
        if self.usage_act and not self._stop:
            from .. import usage
            try:
                usage.finish_session(self)
            except Exception:
                pass
        self._stop = True
        self.paused = True
        for pid in list(self.agents):
            self.cancel_agent(pid)
        with self.lock:
            self.cond.notify_all()

    def _drive(self):
        """The turn driver: run each AI seat's turn to completion, then move on.

        The loop that makes a game with no humans in it play itself, and the one that must never die - a
        failure in one seat's turn is recorded and skipped rather than ending the game.
        """
        while not self._stop:
            with self.lock:
                g = self.game
                if g.s.phase != "playing":
                    self.cond.wait(timeout=2)
                    continue
                pid = g.s.current
                seat = self.seats[pid] if pid < len(self.seats) else None
                if self.paused or seat is None or seat.type in ("human", "mcp"):
                    self.cond.wait(timeout=1)
                    continue
                turn_marker = (g.turn, pid)
            agent = self.get_agent(pid)
            self.agent_status[pid] = "thinking"
            self._broadcast({"type": "agent", "player": pid, "status": "thinking"})
            try:
                agent.play_turn(self, pid)
            except Exception as e:
                if not self._stop:
                    self.errors.append({"t": time.time(), "player": pid, "where": "play_turn", "trace": traceback.format_exc()})
                    self.game.emit("agent_error", f"{self.game.player(pid).name}'s AI failed: {e}", [pid])
            if self._stop:
                break
            if getattr(agent, "cancelled", False):
                continue  # controller changed mid-turn: let the new controller play this same turn
            self.agent_status[pid] = "idle"
            self._broadcast({"type": "agent", "player": pid, "status": "idle"})
            with self.lock:
                if (g.turn, g.s.current) == turn_marker and g.s.phase == "playing":
                    rec = self.metrics.current(pid)
                    if rec is not None and not rec["end_reason"]:
                        rec["end_reason"] = "end_turn" if seat.type == "bot" else "ended_by_server"
                    try:
                        before = (g.turn, g.s.current)
                        tools.execute(g, pid, "end_turn", {})
                        self._after_action(*before)
                    except ActionError:
                        pass
            if self.ai_delay > 0:
                time.sleep(self.ai_delay)

    def wait_for_turn(self, pid: int, timeout: float) -> dict:
        """Long-poll: returns when it's pid's turn, a negotiation awaits pid, or the game ends."""
        deadline = time.time() + timeout
        with self.lock:
            while True:
                g = self.game
                pending = [n["id"] for n in g.s.negotiations if n["status"] == "open" and n["awaiting"] == pid]
                if g.s.phase != "playing":
                    return {"status": "game_over", "winner": g.s.winner, "victory": g.s.victory}
                if not g.player(pid).alive:
                    return {"status": "eliminated"}
                if pending:
                    return {"status": "negotiation", "negotiation_ids": pending,
                            "your_turn": g.s.current == pid}
                if g.s.current == pid:
                    return {"status": "your_turn", "turn": g.turn}
                left = deadline - time.time()
                if left <= 0:
                    return {"status": "waiting", "turn": g.turn, "current_player": g.s.current,
                            "current_player_name": g.player(g.s.current).name}
                self.cond.wait(timeout=min(left, 1.0))

    # ------------------------------------------------------------------
    # Push
    # ------------------------------------------------------------------
    def _on_event(self, ev: dict):
        """Forward a game event to everyone watching."""
        self._broadcast({"type": "event", "event": ev})

    def _broadcast(self, msg: dict):
        """Send a message to every subscriber, dropping ones that have gone away."""
        for fn in list(self.subscribers):
            try:
                fn(msg)
            except Exception:
                pass

    # ------------------------------------------------------------------
    # Saves
    # ------------------------------------------------------------------
    def to_save(self) -> dict:
        """The whole session as a saveable document."""
        g = self.game
        g.save_rng()
        return {
            "format": "citar-save", "version": 1, "rules_version": RULES_VERSION, "saved_at": time.time(),
            "session": {"id": self.id, "name": self.name, "created": self.created,
                        "seats": [{**asdict(s), "connected": False} for s in self.seats],
                        "spectator_token": self.spectator_token, "benchmark": self.benchmark, "usage_act": self.usage_act},
            "state": g.s.to_dict(), "frames": g.frames, "action_log": g.action_log,
            "metrics": self.metrics.data,
        }

    def save(self, filename: Optional[str] = None) -> Path:
        """Write a named save."""
        with self.lock:
            data = self.to_save()
        folder = SAVE_DIR / self.id
        folder.mkdir(parents=True, exist_ok=True)
        name = filename or f"turn{self.game.turn:03d}"
        name = "".join(ch for ch in name if ch.isalnum() or ch in "-_ ")[:60] or "save"
        path = folder / f"{name}.citar"
        tmp = path.with_suffix(".tmp")
        with gzip.open(tmp, "wt", encoding="utf-8") as f:
            json.dump(data, f)
        for attempt in range(8):
            try:
                tmp.replace(path)
                break
            except PermissionError:
                # a sync client (OneDrive) or virus scanner briefly holds the previous save open
                if attempt == 7:
                    raise
                time.sleep(0.25)
        return path

    AUTOSAVE_MIN_SECONDS = 3.0
    LIVE_MARK = "live.json"

    def mark_live(self):
        """Record that this lobby game is open, and whether it is paused, so a restart can bring it back.

        Only games the lobby lists: benchmark games are reloaded by their scheduler, and probe cases are
        not registered at all. A finished game has nothing to come back to, so its mark is removed. A
        game the server paused because a model server was unreachable comes back running: whatever was
        wrong is retried, and the disconnect rule applies again if it is still wrong.
        """
        if not getattr(self, "registered", False) or self._stop:
            return
        path = SAVE_DIR / self.id / self.LIVE_MARK
        try:
            if self.benchmark or self.game.s.phase != "playing":
                path.unlink(missing_ok=True)
                return
            paused = self.paused and (self.pause_reason or {}).get("kind") != "disconnect"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps({"paused": paused, "name": self.name, "at": time.time()}), encoding="utf-8")
        except OSError:
            pass

    def unmark_live(self):
        """The game was closed on purpose: do not bring it back on the next start."""
        try:
            (SAVE_DIR / self.id / self.LIVE_MARK).unlink(missing_ok=True)
        except OSError:
            pass

    def autosave(self, force: bool = False):
        """Write the autosave, at most once per turn unless forced.

        This is what makes a benchmark survive a restart: the scheduler reloads from here, and a game
        interrupted mid-run continues from the last completed turn.
        """
        if self._stop:
            return
        # all-AI games can finish a round many times a second; don't rewrite the save file that often
        if not force and self.game.s.phase == "playing" and time.time() - getattr(self, "_last_autosave", 0) < self.AUTOSAVE_MIN_SECONDS:
            return
        self._last_autosave = time.time()
        try:
            self.save("autosave")
        except Exception:
            self.errors.append({"t": time.time(), "where": "autosave", "trace": traceback.format_exc()})
        self.mark_live()

    @classmethod
    def from_save(cls, data: dict) -> "GameSession":
        """Rebuild a session from a save, including its seats and their tokens."""
        state = GameState.from_dict(data["state"])
        g = Game(state)
        g.frames = data.get("frames", [])
        g.action_log = data.get("action_log", [])
        sess = data.get("session", {})
        seats = [Seat(**{k: v for k, v in s.items() if k in Seat.__dataclass_fields__}) for s in sess.get("seats", [])]
        s = cls(g, seats, sess.get("name", ""), sess.get("id"))
        if data.get("metrics"):
            s.metrics = Metrics(data["metrics"])
            s._metrics_current = None
            s._track_turn()
        s.created = sess.get("created", time.time())
        if sess.get("spectator_token"):
            s.spectator_token = sess["spectator_token"]
        s.benchmark = sess.get("benchmark")
        s.usage_act = sess.get("usage_act")
        from ..engine.visibility import refresh
        refresh(g, force=True)
        return s


def load_save_file(path: Path) -> dict:
    """Read a gzipped save file."""
    with gzip.open(path, "rt", encoding="utf-8") as f:
        return json.load(f)


_MANAGERS: Optional[weakref.WeakSet] = None


def all_sessions() -> list:
    """Every live session in this process, across session managers (for "is this machine in use?")."""
    return [s for m in list(_MANAGERS or ()) for s in list(m.sessions.values())]


class SessionManager:
    """Every live game, and the operations that create or load one."""
    def __init__(self):
        global _MANAGERS
        self.sessions: dict[str, GameSession] = {}
        self.lock = threading.Lock()
        if _MANAGERS is None:
            _MANAGERS = weakref.WeakSet()
        _MANAGERS.add(self)

    def create(self, config: dict, seats_cfg: list[dict], name: str = "", track: bool = True) -> GameSession:
        """Create a game from a lobby configuration and start its session."""
        players = []
        seats = []
        for i, sc in enumerate(seats_cfg):
            stype = sc.get("type", "bot")
            if stype not in SEAT_TYPES:
                raise ValueError(f"Unknown seat type '{stype}'")
            players.append({"name": sc.get("civ_name") or None, "color": sc.get("color"),
                            "leader": sc.get("leader"), "nation": sc.get("nation"), "controller": stype,
                            "difficulty": sc.get("difficulty") or None})
            seats.append(Seat(player=i, type=stype, name=sc.get("name") or "", llm=sc.get("llm") or {},
                              bot=sc.get("bot") or {}))
        cfg = dict(config)
        cfg["players"] = players
        game = Game.new(cfg)
        s = GameSession(game, seats, name)
        s.registered = True
        with self.lock:
            self.sessions[s.id] = s
        if track:
            self.track(s)
        s.autosave(force=True)
        s.start()
        return s

    @staticmethod
    def track(s: "GameSession"):
        """Record the game in the usage ledger (games with an AI model seat; others cost next to nothing)."""
        if not any(seat.type == "llm" for seat in s.seats) and not s.usage_act:
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
                             register: bool = True, start: bool = True) -> GameSession:
        """A new game from a scenario's saved state. Seats default to the scenario's; a "script" seat (probes) is
        driven from outside, so in a normal game it behaves like a human seat."""
        from ..engine.scenario import game_from_state
        g = game_from_state(scn["state"])
        defaults = scn.get("seats") or []
        seats = []
        for p in g.majors():
            sc = dict(defaults[p.id]) if p.id < len(defaults) else {}
            if seats_cfg and p.id < len(seats_cfg) and seats_cfg[p.id]:
                sc.update({k: v for k, v in seats_cfg[p.id].items() if v is not None})
            stype = sc.get("type") or "bot"
            if stype == "script":
                stype = "human"
            if stype not in SEAT_TYPES:
                raise ValueError(f"Unknown seat type '{stype}'")
            p.controller = stype
            if sc.get("difficulty"):
                p.difficulty = g.rules.resolve("difficulty", sc["difficulty"]) or p.difficulty
            seats.append(Seat(player=p.id, type=stype, name=sc.get("name") or sc.get("label") or "",
                              llm=sc.get("llm") or {}, bot=sc.get("bot") or {}))
        g.invalidate()
        s = GameSession(g, seats, name or scn.get("name") or "Scenario")
        if register:
            s.registered = True
            with self.lock:
                self.sessions[s.id] = s
            self.track(s)
            s.autosave(force=True)
        if start:
            s.start()
        return s

    def load(self, path: Path) -> GameSession:
        """Load a save into a live session."""
        data = load_save_file(path)
        s = GameSession.from_save(data)
        s.registered = True
        with self.lock:
            if s.id in self.sessions:
                self.sessions[s.id].stop()
            self.sessions[s.id] = s
        s.paused = True
        s.start()
        self.track(s)
        s.mark_live()
        return s

    def restore_live(self) -> list[str]:
        """Bring back the lobby games that were open when the server last stopped.

        Every open lobby game keeps a mark beside its autosave (see ``GameSession.mark_live``). Each is
        reloaded from that autosave - so a restart costs at most the turn in progress - and resumed
        unless it was paused. Benchmark games are the scheduler's to reload, and are not marked.
        """
        restored = []
        for mark in sorted(SAVE_DIR.glob(f"*/{GameSession.LIVE_MARK}")):
            try:
                state = json.loads(mark.read_text(encoding="utf-8"))
                save = mark.parent / "autosave.citar"
                if not save.exists() or mark.parent.name in self.sessions:
                    continue
                s = self.load(save)
                if s.benchmark or s.game.s.phase != "playing":
                    s.unmark_live()
                    continue
                if not state.get("paused"):
                    s.resume()
                restored.append(f"{s.name} (turn {s.game.turn}{', paused' if state.get('paused') else ''})")
            except Exception:
                traceback.print_exc()
        return restored

    def get(self, sid: str) -> Optional[GameSession]:
        """A session by id, or None."""
        return self.sessions.get(sid)

    def delete(self, sid: str):
        """Delete a game and every save of it."""
        with self.lock:
            s = self.sessions.pop(sid, None)
        if s:
            s.unmark_live()
            s.stop()

    _save_meta_cache: dict = {}

    @classmethod
    def save_meta(cls, p: Path) -> dict:
        """Game name, turn and players of a save file (cached by modification time)."""
        st = p.stat()
        key = str(p)
        hit = cls._save_meta_cache.get(key)
        if hit and hit[0] == st.st_mtime:
            return hit[1]
        meta: dict = {}
        try:
            with gzip.open(p, "rt", encoding="utf-8") as fh:
                data = json.load(fh)
            s, sess = data.get("state", {}), data.get("session", {})
            seats = sess.get("seats", [])
            majors = [pl for pl in s.get("players", []) if pl.get("kind") == "major"]
            meta = {"game_name": sess.get("name"), "turn": s.get("turn"), "phase": s.get("phase"),
                    "turn_limit": (s.get("config") or {}).get("turn_limit"),
                    "benchmark": bool(sess.get("benchmark")),
                    "players": [{"name": pl.get("name"), "alive": pl.get("alive", True),
                                 "seat": seats[i].get("type") if i < len(seats) else None} for i, pl in enumerate(majors)]}
            winner = s.get("winner")
            if winner is not None and 0 <= winner < len(s.get("players", [])):
                meta["winner"] = s["players"][winner].get("name")
        except Exception:
            meta = {"unreadable": True}
        cls._save_meta_cache[key] = (st.st_mtime, meta)
        return meta

    @classmethod
    def list_saves(cls) -> list[dict]:
        """Every save on disk, with what is in it."""
        out = []
        if not SAVE_DIR.exists():
            return out
        for p in sorted(SAVE_DIR.glob("*/*.citar"), key=lambda p: -p.stat().st_mtime):
            out.append({"path": str(p.relative_to(SAVE_DIR)).replace("\\", "/"), "game_id": p.parent.name,
                        "name": p.stem, "modified": p.stat().st_mtime, "size": p.stat().st_size, **cls.save_meta(p)})
        return out

    @staticmethod
    def delete_save(rel_path: str, whole_game: bool = False) -> list[str]:
        """Delete one save."""
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
        if whole_game or not any(p.parent.iterdir()):
            try:
                for rest in p.parent.iterdir():
                    if rest.suffix == ".tmp":
                        rest.unlink()
                p.parent.rmdir()
            except OSError:
                pass
        return removed
