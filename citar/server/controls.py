"""Paired control games for hybrid benchmark jobs (design/phase3-features.md P3.6.9).

Every hybrid job has a control: the same game, from the same settings and seed, with the hybrid seat played by the same
bot profile with every category the bot's. The control seat keeps the hybrid seat's handicap and difficulty level, so
the pair differs only in who decides the model's categories; its automatic decisions are a bot seat's (the engine votes
and annexes for it), since those are the very categories the model takes over in the other arm. It stays the engine's
``hybrid`` controller, its owners all the bot's: the engine then plays it exactly as a bot seat with the same handicap
(package 3-14's gate 1 holds it to that, event for event), and its drives stop where the hybrid game's do (at its
``hybrid_diplomat`` stop, then the drive that ends its turn), so the diplomacy recorder values both arms' proposals at
the same points of the game.

A control is bot-only and needs no machine: it runs headless on the server's CPU (``run_control``: a loop over
``EngineGame.drive`` with the recorder), one at a time on its own thread (``ControlGames``), once per spec (the game's
settings and seed, the seats' bots with their fingerprints, the build) under ``<benchmark dir>/controls/<id>.json``, and
every model's job of the same scenario and seed shares it. The spec pins each seat's bot as the hybrid game set it up
(``pin_bot``: the bot version, its parameters and aggression), since a control may play long after its hybrid game
started, behind others on its queue: a profile edited or deleted meanwhile changes neither what the control plays nor
its id, and a pinned bot this build cannot make fails the control rather than play another bot.

Replaces: nothing; before 3-16 there were no hybrid benchmark jobs.
"""
from __future__ import annotations

import copy
import hashlib
import json
import logging
import re
import threading
import time
import traceback
from pathlib import Path
from typing import Callable, Optional

from .. import engine_api, fsutil
from ..engine_api import ActionError

log = logging.getLogger("citar.controls")

#: A bot seat's automatic decisions: the engine votes, annexes and picks for it (the control seat's).
BOT_AUTO = {"un_vote": True, "conquest": True, "free_picks": True}
#: The most drives a control plays before it is given up (a game that never ends is a bug, not a control).
MAX_DRIVES = 200_000
#: What a control's file says it is, so an older layout is played again rather than read.
FORMAT = 1


class ControlStopped(Exception):
    """The server is stopping: a control under way is left to be played again later."""


def pin_bot(bot_cfg: Optional[dict]) -> dict:
    """A seat's bot as its game plays it now, pinned (``profiles.resolve(..., pin=True)``): the profile (the standard
    bot when the seat names none, or names one deleted since the game was set up, as ``hybrid_agent.bot_args`` and
    ``BotAgent`` play it), its revision, the bot version it plays, its parameters, the profile's fixed aggression and
    the seat's own. ``bot_handle`` makes the bot from it alone, whatever happens to the profile afterwards."""
    from ..agents.hybrid_agent import bot_args
    from ..bots import profiles
    ref, kw = bot_args(bot_cfg)
    r = profiles.resolve(ref, pin=True)
    return {"profile": r["profile"], "profile_rev": r["profile_rev"], "engine": r["engine"], "params": r["params"],
            "fixed_aggression": r["aggression"], "aggression": kw.get("aggression")}


def bot_handle(pinned: dict):
    """The bot a pinned seat's bot (``pin_bot``) plays: a fresh handle, with every category the bot's. Raises
    ValueError when this build cannot make it (a spec of an older layout, a version no longer compiled in): the
    control then fails rather than play another bot."""
    if not isinstance(pinned, dict) or not pinned.get("engine"):
        raise ValueError("the control's spec does not pin its bots (an older layout); retry the job to play it again")
    agg = pinned.get("aggression")
    fixed = pinned.get("fixed_aggression")
    return engine_api.bot_instance(pinned["engine"], aggression=float(agg) if agg is not None else 0.4,
                                   params=pinned.get("params") or {},
                                   fixed_aggression=float(fixed) if fixed is not None else None)


def control_spec(engine_config: dict, seats, pid: int, ops: Optional[list] = None) -> dict:
    """The control of a hybrid game made from ``engine_config`` (``GameSession.engine_config``: what the engine was
    given) whose seats are ``seats`` (the session's, their bots as the server pinned them), the hybrid seat ``pid``:
    the same configuration and seed, the control seat's automatic decisions a bot seat's (``BOT_AUTO``), and each seat
    a bot plays with its bot pinned now (``pin_bot``; the hybrid seat's bot with every category the bot's). ``ops`` are
    scenario operations the hybrid game was given before its first turn, which the control is given too. Reads the
    bot profiles: off any session's lock."""
    cfg = copy.deepcopy(engine_config)
    players = cfg.get("players") or []
    players[pid] = {**players[pid], "auto": dict(BOT_AUTO)}
    bots = {}
    for seat in seats:
        if seat.type in ("bot", "hybrid"):
            bots[str(seat.player)] = pin_bot(seat.bot)
    return {"format": FORMAT, "config": cfg, "bots": bots, "pid": int(pid), "ops": list(ops or [])}


def _bots(spec: dict) -> dict:
    """The control's bot handles, by seat, from the bots its spec pinned (``bot_handle``)."""
    return {int(pid): bot_handle(pinned) for pid, pinned in spec["bots"].items()}


def control_key(spec: dict) -> str:
    """The control's id: a hash of its spec (the bots pinned in it), the build (``engine_api.build_info``'s id, over
    the engine's and the bot's code and the ruleset) and every bot's fingerprint (what it plays). Two hybrid jobs of
    the same scenario and seed, whatever their models, have the same control; a new build or bot plays it again."""
    fingerprints = {str(pid): engine_api.bot_fingerprint(b) for pid, b in sorted(_bots(spec).items())}
    raw = json.dumps({"spec": spec, "build": engine_api.build_info().get("build_id"),
                      "fingerprints": fingerprints}, sort_keys=True, separators=(",", ":"), default=str)
    return "ctl_" + hashlib.sha256(raw.encode()).hexdigest()[:20]


def run_control(spec: dict, *, stop: Optional[Callable[[], bool]] = None, events: Optional[list] = None) -> dict:
    """Play a control game to its end, headless: drive after drive (one seat's turn each, as a session drives its bot
    seats), the recorder reading what moved after each. Returns its record: the recorder's rows and summary (with how
    the game ended, ``final_of``), how many events it made and their hash (``events_sha256``), its drives and seconds.
    ``events``, when given, gets every event (a test comparing two runs). Raises ControlStopped when ``stop()`` says
    so, RuntimeError for a game that does not end."""
    from ..diplomacy_stats import DiplomacyRecorder, final_of, summarize
    pid = int(spec["pid"])
    t0 = time.perf_counter()
    bots = _bots(spec)
    # the seat's own bot with every category the bot's values its proposals, as the hybrid seat's fallback bot does in
    # the other arm (a handle of its own: the one playing the seat keeps its memory of the game)
    rec = DiplomacyRecorder(pid, bot_handle(spec["bots"][str(pid)]))
    g = engine_api.EngineGame.new(spec["config"])
    if spec.get("ops"):
        g.apply_ops(spec["ops"])
    digest = hashlib.sha256()
    count = [0]

    def heard(ev: dict) -> None:
        count[0] += 1
        digest.update(json.dumps(ev, sort_keys=True, separators=(",", ":")).encode())
        if events is not None:
            events.append(ev)
    g.subscribe(heard)
    g.subscribe(rec.on_event)
    drives = 0
    while g.phase == "playing":
        if stop is not None and stop():
            raise ControlStopped()
        if drives >= MAX_DRIVES:
            raise RuntimeError(f"the control game did not end after {MAX_DRIVES} drives (turn {g.turn})")
        st = g.drive(bots, seat_limit=1)
        drives += 1
        rec.observe(g)
        if st["stop"] == "awaiting_reply":
            # every seat is a bot, which answers inside the drive: a chat still waiting is one nobody will answer,
            # closed as a session's driver closes a turn's chats when an agent leaves them open
            for nid in st.get("negotiations") or []:
                try:
                    g.close_negotiation(nid, "expired", "(no reply in time)")
                except ActionError:
                    pass
            rec.observe(g)
        elif st["stop"] == "external":
            raise RuntimeError(f"the control game stopped at seat {st.get('player')}, which no bot plays")
    rows = rec.mine()
    return {"format": FORMAT, "pid": pid, "rows": rows, "summary": summarize(rows, pid, final_of(g, pid)),
            "events": count[0], "events_sha256": digest.hexdigest(), "drives": drives,
            "turn": g.turn, "phase": g.phase, "seconds": round(time.perf_counter() - t0, 2),
            "build_id": engine_api.build_info().get("build_id"), "made": time.time()}


class ControlGames:
    """The control games of a benchmark directory: the cache under ``controls/`` and the thread that plays the ones
    not cached yet, one at a time (each is CPU work of a few seconds to minutes, and the hybrid games beside them are
    what is being measured). ``ensure`` asks for one; ``status`` says where it stands; ``get`` reads it once done.
    A control under way when the server stops is played again from its spec (kept beside it) when it is next asked
    for. ``played`` counts the games this process actually played (the rest came from the cache)."""

    #: How many records are kept in memory.
    CACHED = 64

    def __init__(self, directory: Path):
        self.dir = Path(directory)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self._queue: list = []          # control ids waiting to be played, oldest first
        self._failed: dict = {}         # control id -> why it could not be played (until it is asked for again)
        self._running: Optional[str] = None
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self._cache: dict = {}          # control id -> its record, the last CACHED read
        self.played = 0

    @staticmethod
    def _valid(key: str) -> bool:
        return isinstance(key, str) and bool(re.fullmatch(r"ctl_[0-9a-f]{20}", key))

    def path(self, key: str) -> Path:
        """Where control ``key``'s record is kept. KeyError for an id that is not one."""
        if not self._valid(key):
            raise KeyError(key)
        return self.dir / f"{key}.json"

    def _spec_path(self, key: str) -> Path:
        return self.dir / f"{key}.spec.json"

    def get(self, key: str) -> Optional[dict]:
        """Control ``key``'s record, or None when it has not been played (or its file is of another layout). A record
        never changes once written, so the last ``CACHED`` read are kept in memory."""
        with self.lock:
            rec = self._cache.get(key)
        if rec is not None:
            return rec
        try:
            p = self.path(key)
            if not p.exists():
                return None
            rec = json.loads(fsutil.read_text(p))
        except (KeyError, OSError, ValueError):
            return None
        if rec.get("format") != FORMAT:
            return None
        with self.lock:
            self._cache[key] = rec
            while len(self._cache) > self.CACHED:
                self._cache.pop(next(iter(self._cache)))
        return rec

    def status(self, key: str) -> dict:
        """Where control ``key`` stands: ``done`` (its record reads), ``running``, ``queued``, ``failed`` (with
        ``error``), or ``missing`` (never asked for in this process, and no record that reads: a file of another layout
        or a damaged one is no record, so the job asks for the control again and it is played again)."""
        if not self._valid(key):
            return {"status": "missing"}
        if self.get(key) is not None:
            return {"status": "done"}
        with self.lock:
            if self._running == key:
                return {"status": "running"}
            if key in self._queue:
                return {"status": "queued", "position": self._queue.index(key) + 1}
            if key in self._failed:
                return {"status": "failed", "error": self._failed[key]}
        return {"status": "missing"}

    def ensure(self, spec: Optional[dict] = None, key: Optional[str] = None) -> str:
        """Ask for the control of ``spec`` (or, after a restart, of ``key``, from the spec kept beside it): its id, at
        once; played on the controls' thread unless it is cached or already waiting. A control that failed is tried
        again. Raises KeyError for a key with no spec kept."""
        if spec is not None:
            key = control_key(spec)
        if self.get(key) is not None:
            return key
        sp = self._spec_path(key) if self._valid(key) else None
        if spec is not None:
            tmp = sp.with_suffix(".tmp")
            tmp.write_text(json.dumps(spec, sort_keys=True), encoding="utf-8")
            fsutil.replace(tmp, sp)
        elif sp is None or not sp.exists():
            raise KeyError(key)
        with self.lock:
            self._failed.pop(key, None)
            if key != self._running and key not in self._queue:
                self._queue.append(key)
            if self._thread is None or not self._thread.is_alive():
                self._thread = threading.Thread(target=self._loop, daemon=True, name="control-games")
                self._thread.start()
        return key

    def wait(self, key: str, timeout: float) -> Optional[dict]:
        """Wait up to ``timeout`` seconds for control ``key``; its record, or None (a test's)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rec = self.get(key)
            if rec is not None or self.status(key)["status"] in ("failed", "missing"):
                return rec
            time.sleep(0.05)
        return self.get(key)

    def stop(self) -> None:
        """Stop the thread: a control under way is dropped and played again when next asked for."""
        self._stop.set()

    def keep_only(self, wanted: set) -> list:
        """Drop the queued controls no job wants any more (``wanted``: the ids of the controls of jobs that can still
        pair: a cancelled, failed or skipped job's control is not played for nobody). The one under way plays on (it
        is cached for whoever asks next). Returns the ids dropped."""
        with self.lock:
            dropped = [k for k in self._queue if k not in wanted]
            self._queue = [k for k in self._queue if k in wanted]
        return dropped

    def _loop(self) -> None:
        """The controls' thread: play the queue, oldest first, each control once."""
        while not self._stop.is_set():
            with self.lock:
                if not self._queue:
                    self._thread = None
                    return
                key = self._queue.pop(0)
                self._running = key
            try:
                if self.get(key) is None:
                    spec = json.loads(fsutil.read_text(self._spec_path(key)))
                    rec = run_control(spec, stop=self._stop.is_set)
                    rec["id"] = key
                    p = self.path(key)
                    tmp = p.with_suffix(".tmp")
                    tmp.write_text(json.dumps(rec, default=str), encoding="utf-8")
                    fsutil.replace(tmp, p)
                    with self.lock:
                        self.played += 1
            except ControlStopped:
                pass
            except Exception as e:
                log.warning("control game %s failed", key, exc_info=True)
                with self.lock:
                    self._failed[key] = f"{type(e).__name__}: {e}"[:500]
                    log.debug(traceback.format_exc())
            finally:
                with self.lock:
                    self._running = None
