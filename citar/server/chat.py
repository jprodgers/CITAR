"""The chat clock: how long a chat waits on each side, enforced by the session alone (design/phase3-features.md P3.5.2).

Before it, every agent kept its own timer for the chats it was in: a bot's turn waited 90 seconds in all for the seats
it was in negotiations with (``BotAgent.REPLY_WAIT_SECONDS``), a model gave each message it sent 240 seconds
(``LLMAgent.negotiation_wait_seconds``), and nothing timed a chat that waited on a person outside a turn. Here one clock
per session times every open chat by the side it waits on, from the lobby's two settings:

- ``chat_model_reply_seconds`` (default 600): a chat waiting on a model seat (an LLM or MCP seat, a hybrid seat's
  model), queue time included, except behind a benchmark in a benchmark game (below). At its end the seat declines,
  "(no reply in time)", on its behalf; a hybrid seat's fallback bot answers instead (P3.6.5, package 3-14).
- ``chat_person_reply_seconds`` (default 300): a chat waiting on a person. At its end the chat closes as expired,
  "(no reply in time)", whoever opened it.
- a chat waiting on a bot is never timed: a bot answers inside its drive or through its responder at once.

``diplomacy.max_chat_messages`` (the ruleset's cap, 30 in the shipped one, when the game sets none) is the third chat
setting; the engine enforces it, and reports it (``EngineGame.max_chat_messages``).

The clock stands still while the game is paused, and for a chat whose answer is a pooled prompt queued behind a
benchmark in a benchmark game (``answer_status``: the ticket's status says so), which then counts from the prompt's
start. A change of a chat's entries or of the seat it waits on starts that chat's clock again; a session loaded or
brought back by ``restore_live`` starts every open chat's clock afresh (a new session has a new clock); a resume carries
on where the pause left it.

At a deadline the clock first cancels any answer still in progress for that chat (``GameSession.cancel_chat_answer``:
the seat's conversation for it, or its ticket), then acts once through the session (``decline_late``, ``expire_chat``,
``chat_fallback``), each an engine call a lost race turns into an ``ActionError``, which it ignores. An action that
failed (an error, or the engine's refusal) on a chat that has not moved is tried again a little later
(``RETRY_SECONDS``, doubling up to ``MAX_RETRY_SECONDS``): a chat between two people has no agent's safety cap to
close it otherwise.

It is the chat timer, with safety nets: ``BotAgent.play_turn`` and ``LLMAgent``'s waits keep a cap of twice the
longest timeout (``safety_cap``) after which they close the chats themselves, and the session's driver still forces
the end of a turn an agent left open. The clock's loop records every exception (``session.errors``, ``where: chat
clock``) and carries on, and the driver starts it again when it finds the thread dead (``ensure``).

Status events (P3.5.5): ``{"type": "negotiation_status", "negotiation", "players", "waiting_on", "kind", "state",
"position", "eta_s", "deadline", ...}`` broadcast on the session. The clock sends ``waiting`` (with the deadline, in epoch
seconds, and ``left_s``); the answering seat's conversation or ticket, through ``answer_status``, sends ``queued`` (with
its place and ETA) and ``thinking``; the session sends ``closed``. The game socket gives them to the two parties and
to spectators of a game whose god view is allowed, and to nobody else.

Time is the session's ``clock`` (``RealClock``: ``time.monotonic``, ``time.time`` and the condition's own wait), so
tests run every timeout in fake time.

Replaces: ``BotAgent.REPLY_WAIT_SECONDS`` (``citar/agents/bot_agent.py``), ``LLMAgent._chat_left`` and its per-message
wait budget (``citar/agents/llm_agent.py``), all before 3-11.
"""
from __future__ import annotations

import logging
import threading
import time
import traceback
from dataclasses import dataclass
from typing import Optional

log = logging.getLogger("citar.chat")

#: The lobby's defaults (P3.1.2: a model's reply 10 minutes including queue time, an AI waiting on a person 5).
DEFAULT_MODEL_REPLY_SECONDS = 600.0
DEFAULT_PERSON_REPLY_SECONDS = 300.0
#: The ruleset's own cap on a chat's messages (``diplomacy.max_chat_messages``), for a game that sets none.
DEFAULT_MAX_MESSAGES = 30
#: The bounds a game's chat timeouts are held to.
MIN_REPLY_SECONDS, MAX_REPLY_SECONDS = 1.0, 7 * 86400.0
#: The note a chat closes with, or a seat declines with, when its time runs out.
NO_REPLY = "(no reply in time)"
#: Why a seat's answer did not come from its model (P3.5.2), recorded with every fallback and decline.
CAUSES = ("timeout", "queued_behind_benchmark", "refused", "failed")
#: The kind of side a chat waits on, by seat type.
KINDS = {"llm": "model", "mcp": "model", "hybrid": "hybrid", "human": "person", "bot": "bot"}
#: The longest the clock's thread sleeps between looks (it also wakes on the session's condition).
WAKE_SECONDS = 1.0
#: How long after a failed action at a deadline the clock tries again, doubling each time up to the longest.
RETRY_SECONDS, MAX_RETRY_SECONDS = 1.0, 60.0


def settings(config: Optional[dict], max_messages: Optional[int] = None) -> dict:
    """A game's chat settings from its configuration (``EngineGame.config``, which keeps the lobby's keys verbatim):
    ``chat_model_reply_seconds``, ``chat_person_reply_seconds`` and ``max_chat_messages``, each the default when the
    game has none or something that is not a number, and the timeouts held within ``MIN_REPLY_SECONDS`` and
    ``MAX_REPLY_SECONDS``. ``max_messages``, when given, is the cap the engine plays (``EngineGame.max_chat_messages``:
    the game's own, else its ruleset's), which is what the game reports."""
    config = config or {}

    def seconds(key: str, default: float) -> float:
        value = config.get(key)
        try:
            v = float(value)
        except (TypeError, ValueError):
            return default
        if v != v or isinstance(value, bool):          # NaN, or a flag where a number belongs
            return default
        return max(MIN_REPLY_SECONDS, min(MAX_REPLY_SECONDS, v))

    if max_messages is not None and not isinstance(max_messages, bool):
        cap = int(max_messages)
    else:
        cap = (config.get("diplomacy") or {}).get("max_chat_messages") \
            if isinstance(config.get("diplomacy"), dict) else None
        try:
            cap = int(cap) if cap not in (None, 0) and not isinstance(cap, bool) else DEFAULT_MAX_MESSAGES
        except (TypeError, ValueError):
            cap = DEFAULT_MAX_MESSAGES
    return {"chat_model_reply_seconds": seconds("chat_model_reply_seconds", DEFAULT_MODEL_REPLY_SECONDS),
            "chat_person_reply_seconds": seconds("chat_person_reply_seconds", DEFAULT_PERSON_REPLY_SECONDS),
            "max_chat_messages": max(2, cap)}


class RealClock:
    """The session's time (``GameSession.clock``): ``now`` for timeouts, ``wall`` for the deadlines told to clients,
    and ``wait`` on the session's condition. A test's fake clock replaces it, so timeouts pass in fake time."""

    @staticmethod
    def now() -> float:
        return time.monotonic()

    @staticmethod
    def wall() -> float:
        return time.time()

    @staticmethod
    def wait(cond: threading.Condition, timeout: float) -> None:
        """Wait on ``cond`` (held by the caller) for at most ``timeout`` seconds, or until it is notified."""
        cond.wait(timeout)


@dataclass
class _Chat:
    """One open chat as the clock times it."""
    nid: int
    players: tuple
    awaiting: Optional[int]
    entries: int
    kind: str
    timeout: Optional[float]
    counted: float = 0.0            # seconds counted against the timeout since the chat began waiting on this side
    held: bool = False              # the answer is a prompt queued behind a benchmark (a benchmark game): not counted
    acted: bool = False             # the clock is acting, or has acted, at its deadline (once)
    failures: int = 0               # actions at its deadline that failed with the chat where it stood
    retry_at: float = 0.0           # the clock's time before which a failed action is not tried again
    published: Optional[tuple] = None            # the last status sent, so an unchanged one is not sent again


class ChatClock:
    """One session's chat timer (module docstring). Its own lock (``_lock``) guards what it times, and is never held
    while it takes the session's lock or broadcasts: a thread holding the session's lock may call into it (the
    session's event hook, a wait in fake time), never the other way round."""

    def __init__(self, session, clock=None):
        self.session = session
        self.clock = clock if clock is not None else session.clock
        self._lock = threading.Lock()
        self.chats: dict[int, _Chat] = {}
        self._last = self.clock.now()
        self._paused = bool(session.paused)
        self._settings: Optional[dict] = None
        self._thread: Optional[threading.Thread] = None
        self._stopped = False

    # ------------------------------------------------------------------ settings
    @property
    def settings(self) -> dict:
        """The game's chat settings (``settings``), read from its configuration once, with the message cap the engine
        plays (the game's, else its ruleset's)."""
        if self._settings is None:
            game = self.session.game
            try:
                cap = game.max_chat_messages()
            except Exception:
                cap = None
            try:
                self._settings = settings(game.config, cap)
            except Exception:
                self._settings = settings(None, cap)
        return self._settings

    def timeout(self, kind: str) -> Optional[float]:
        """How long a chat waits on a side of ``kind`` (None: a bot, never timed)."""
        if kind in ("model", "hybrid"):
            return self.settings["chat_model_reply_seconds"]
        if kind == "person":
            return self.settings["chat_person_reply_seconds"]
        return None

    def safety_cap(self) -> float:
        """The agents' safety cap on a wait for chats (P3.5.2): twice the longest chat timeout of the game."""
        st = self.settings
        return 2.0 * max(st["chat_model_reply_seconds"], st["chat_person_reply_seconds"])

    def kind_of(self, pid: Optional[int]) -> str:
        """The kind of side seat ``pid`` is: ``model``, ``hybrid``, ``person`` or ``bot`` (``KINDS``)."""
        seats = self.session.seats
        if pid is None or not 0 <= pid < len(seats):
            return "person"
        return KINDS.get(seats[pid].type, "person")

    # ------------------------------------------------------------------ the thread
    def start(self) -> None:
        """Time the chats open now, from here (a loaded game's included), and start the clock's thread
        (``GameSession.start``)."""
        self._stopped = False
        self.note()
        self.ensure(quiet=True)

    def ensure(self, quiet: bool = False) -> bool:
        """Start the thread again if it died (P3.9.2: the driver looks at each turn); returns whether it did."""
        if self._stopped or self.session.stopped or self.session.crashed:
            return False
        t = self._thread
        if t is not None and t.is_alive():
            return False
        if t is not None and not quiet:
            log.warning("the chat clock of game %s had stopped; starting it again", self.session.id)
        self._thread = threading.Thread(target=self._run, daemon=True, name=f"chat-clock-{self.session.id}")
        self._thread.start()
        return t is not None

    def alive(self) -> bool:
        """Whether the clock's thread runs."""
        return self._thread is not None and self._thread.is_alive()

    def stop(self) -> None:
        """Stop the thread for good (the game stopped or crashed)."""
        self._stopped = True
        s = self.session
        try:
            with s.lock:
                s.cond.notify_all()
        except RuntimeError:
            pass

    def _run(self) -> None:
        s = self.session
        while not self._stopped and not s.stopped and not s.crashed:
            try:
                with s.lock:
                    if self._stopped or s.stopped:
                        break
                    self.clock.wait(s.cond, WAKE_SECONDS)
                self.tick()
            except Exception:
                # recorded, and the clock carries on: a chat that cannot be timed must not stop the others
                s.errors.append({"t": time.time(), "where": "chat clock", "trace": traceback.format_exc()})
                log.exception("the chat clock of game %s failed; it carries on", s.id)
                time.sleep(WAKE_SECONDS)

    # ------------------------------------------------------------------ timing
    def _settle(self, now: float) -> None:
        """Count the time since the last look against every timed chat, unless the game was paused all along (its
        pause is followed: ``settle`` is called before a pause or a resume changes it). Lock held."""
        dt = now - self._last
        self._last = now
        paused = bool(self.session.paused)
        was = self._paused
        self._paused = paused
        if dt <= 0 or was:
            return
        for c in self.chats.values():
            if c.timeout is not None and not c.held:
                c.counted += dt

    def settle(self) -> None:
        """Count the time up to now with the game's pause as it has stood: call it before the pause changes
        (``GameSession.set_paused``, ``suspend``, ``resume``), then ``republish`` after. Non-blocking."""
        with self._lock:
            self._settle(self.clock.now())

    def _reconcile(self, heads: list) -> list:
        """Bring the timed chats in line with the open negotiations (``heads``): a new chat, or one whose entries,
        awaited seat or that seat's kind changed, starts its clock afresh; a closed one is dropped. Returns the
        statuses to send. Lock held."""
        out = []
        seen = set()
        for n in heads:
            nid = n["id"]
            seen.add(nid)
            awaiting = n.get("awaiting")
            kind = self.kind_of(awaiting) if awaiting is not None else "person"
            c = self.chats.get(nid)
            if c is None or (c.awaiting, c.entries, c.kind) != (awaiting, n["entries"], kind):
                c = _Chat(nid, (n["initiator"], n["responder"]), awaiting, n["entries"], kind,
                          self.timeout(kind) if awaiting is not None else None)
                self.chats[nid] = c
            if c.published is None:
                # only a chat nobody has been told of yet (new, moved, or to be told again after the pause changed):
                # one already told keeps what was said last, so an answer's queued or thinking stands until the
                # answer ends (``answer_status``) rather than being turned back to waiting by the next look
                msg = self._waiting(c)
                if msg is not None:
                    out.append(msg)
        for nid in [nid for nid in self.chats if nid not in seen]:
            del self.chats[nid]
        return out

    def note(self) -> None:
        """A negotiation moved or closed, or a seat changed (the session's event hook, usually with the session's lock
        held): bring the timed chats up to date at once, so a chat's clock starts when it begins to wait. Never acts:
        acting is ``tick``'s."""
        msgs, _due = self._look(act=False)
        self._send(msgs)

    def tick(self) -> int:
        """One look (the thread's, every second and on the session's condition; a test's in fake time): count the time
        since the last, bring the chats up to date, and act on those past their deadline. Returns how many it acted
        on. Safe with or without the session's lock held."""
        s = self.session
        if s.crashed or s.stopped:
            return 0
        msgs, due = self._look(act=True)
        self._send(msgs)
        acted = 0
        for c in due:
            done = False
            try:
                done = self._act(c)
            except Exception:
                s.errors.append({"t": time.time(), "where": "chat clock", "player": c.awaiting,
                                 "trace": traceback.format_exc()})
                log.exception("the chat clock could not act on negotiation %s", c.nid)
            if done:
                acted += 1
            else:
                self._again(c)
        return acted

    def _again(self, c: _Chat) -> None:
        """The action at ``c``'s deadline failed (an error, or the engine refused it, the chat where the clock saw it or
        not): it is tried again after a pause that doubles with each failure, so a chat no agent's safety cap covers
        (between two people) still closes. A chat that moved meanwhile is not: every look brings the timed chats up to
        date before it acts (``_reconcile``), and a chat that moved or closed is timed afresh as a new record, or
        dropped, so the failed record is never acted on again."""
        with self._lock:
            if self.chats.get(c.nid) is c:
                c.failures += 1
                c.retry_at = self.clock.now() + min(MAX_RETRY_SECONDS, RETRY_SECONDS * 2 ** (c.failures - 1))
                c.acted = False

    def _look(self, act: bool) -> tuple:
        """(the statuses to send, the chats past their deadline to act on: none unless ``act``)."""
        s = self.session
        heads = s.game.open_negotiation_heads()
        with self._lock:
            now = self.clock.now()
            self._settle(now)
            msgs = self._reconcile(heads)
            due = []
            if act and not s.paused:
                for c in self.chats.values():
                    if c.timeout is not None and not c.acted and not c.held and c.counted >= c.timeout \
                            and now >= c.retry_at:
                        c.acted = True
                        due.append(c)
            return msgs, due

    def _act(self, c: _Chat) -> bool:
        """At a chat's deadline: cancel the answer still in progress, then act once by the kind of side it waits on.
        Returns whether the chat moved."""
        s = self.session
        pid = c.awaiting
        if pid is None:
            return False
        s.cancel_chat_answer(pid, c.nid)
        if c.kind == "model":
            return s.decline_late(pid, c.nid, c.entries, "timeout")
        if c.kind == "hybrid":
            return s.chat_fallback(pid, c.nid, c.entries, "timeout")
        if c.kind == "person":
            return s.expire_chat(c.nid, c.entries)
        return False

    # ------------------------------------------------------------------ the answering side
    def answer_status(self, nid: int, pid: int, payload: dict) -> None:
        """The answering seat's request for chat ``nid`` moved in its machine's queue (a ``ScheduledConversation``'s
        or a ticket's ``on_status``, on the scheduler's notifier thread; or the seat's own ``thinking``/``done``).
        ``queued`` (with ``position``, ``eta_s`` and ``benchmark``: it waits behind a benchmark) holds the chat's clock
        when the game is a benchmark game and the wait is a benchmark's; ``thinking`` (``running`` for a ticket) starts
        the clock again from zero if it was held, so it counts from the prompt's start; ``done`` (and an end) sends the
        chat's ``waiting`` again. Each change is broadcast as a ``negotiation_status``."""
        state = (payload or {}).get("state")
        s = self.session
        with self._lock:
            self._settle(self.clock.now())
            c = self.chats.get(nid)
            if c is None or c.awaiting != pid:
                return
            if state == "queued":
                if bool(payload.get("benchmark")) and bool(s.benchmark):
                    c.held = True
                msg = self._status(c, "queued", position=payload.get("position"), eta_s=payload.get("eta_s"),
                                   machine=payload.get("machine_name"),
                                   **({"held": "benchmark"} if c.held else {}))
            elif state in ("thinking", "running", "granted"):
                if c.held:
                    c.held = False
                    c.counted = 0.0                 # counted from the prompt's start (a benchmark game)
                msg = self._status(c, "thinking")
            else:
                if c.held:
                    c.held = False
                    c.counted = 0.0
                msg = self._waiting(c)
        self._send([msg] if msg is not None else [])

    # ------------------------------------------------------------------ what clients are told
    def left(self, c: _Chat) -> Optional[float]:
        """Seconds left before ``c``'s deadline, or None when it is not timed now."""
        if c.timeout is None or c.held:
            return None
        return max(0.0, c.timeout - c.counted)

    def deadline_of(self, nid: int) -> dict:
        """Where chat ``nid``'s clock stands: ``waiting_on``, ``kind``, ``deadline`` (epoch seconds, None when it is
        not timed or the game is paused), ``left_s`` and ``paused`` (the clock stands still). Empty for a chat the
        clock does not time (closed, unknown). A chat it has not looked at yet is looked at first (``note``), so an
        answer's first message never says its time is unlimited for want of a look."""
        for first in (True, False):
            with self._lock:
                self._settle(self.clock.now())
                c = self.chats.get(nid)
                if c is not None:
                    return self._timing(c)
            if not first:
                return {}
            self.note()
        return {}

    def _timing(self, c: _Chat) -> dict:
        left = self.left(c)
        paused = bool(self.session.paused)
        deadline = None if left is None or paused else round(self.clock.wall() + left, 3)
        return {"waiting_on": c.awaiting, "kind": c.kind, "deadline": deadline,
                "left_s": None if left is None else round(left, 1), "paused": paused,
                **({"held": "benchmark"} if c.held else {})}

    def _status(self, c: _Chat, state: str, *, position=None, eta_s=None, **extra) -> Optional[dict]:
        """A ``negotiation_status`` for ``c``, unless it says what the last one sent said (a deadline that only
        counted down is not news: clients count it down themselves). ``entries`` is how many entries the chat had
        when it was said: a chat only grows, so a client tells a status that arrived late (fewer entries than it has
        seen) from one ahead of the entries it has read (the chat window, P3.5.6). Lock held."""
        timing = self._timing(c)
        key = (state, position, eta_s, timing["deadline"] is None, timing["paused"], c.held, c.awaiting, c.entries)
        if key == c.published:
            return None
        c.published = key
        return {"type": "negotiation_status", "negotiation": c.nid, "players": list(c.players),
                "waiting_on": c.awaiting, "kind": c.kind, "state": state, "position": position, "eta_s": eta_s,
                "deadline": timing["deadline"], "left_s": timing["left_s"], "paused": timing["paused"],
                "entries": c.entries, **extra}

    def _waiting(self, c: _Chat) -> Optional[dict]:
        """A chat's ``waiting`` status (with its deadline), unless it is unchanged since the last one sent. Lock
        held."""
        return self._status(c, "waiting", **({"held": "benchmark"} if c.held else {}))

    def republish(self) -> None:
        """Send every timed chat's ``waiting`` again: the game paused or resumed, so the deadlines changed."""
        with self._lock:
            self._settle(self.clock.now())
            msgs = []
            for c in self.chats.values():
                c.published = None
                msg = self._waiting(c)
                if msg is not None:
                    msgs.append(msg)
        self._send(msgs)

    def _send(self, msgs: list) -> None:
        for msg in msgs:
            if msg is not None:
                self.session._broadcast(msg)

    def chats_of(self, pid: int) -> list[dict]:
        """The open chats seat ``pid`` is in, each with where its clock stands: ``negotiation_id``, ``with``,
        ``waiting_on`` (``you`` or ``them``), ``deadline``, ``seconds_left`` (MCP's ``wait_for_turn``)."""
        with self._lock:
            self._settle(self.clock.now())
            out = []
            for c in self.chats.values():
                if pid not in c.players:
                    continue
                t = self._timing(c)
                out.append({"negotiation_id": c.nid, "with": c.players[1] if c.players[0] == pid else c.players[0],
                            "waiting_on": "you" if c.awaiting == pid else "them", "deadline": t["deadline"],
                            "seconds_left": t["left_s"], "paused": t["paused"]})
            return sorted(out, key=lambda d: d["negotiation_id"])
