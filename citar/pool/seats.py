"""Game seats on pooled machines.

A seat's ``llm`` block names a server by id. Servers in the admin registry (``config/servers.json``)
are dialled by CITAR itself; servers registered on the Servers page live in the database and are
reached through the helper (worker) connected from that machine. This module turns a seat naming a
pooled server into an agent configuration that plays through its helper, and checks that whoever
creates the game may use the machine.
"""
from __future__ import annotations

import threading
from typing import Optional

# ---------------------------------------------------------------------------- who is using a machine
# Game sessions are found through every SessionManager; work that runs games outside one (a probe
# case plays in an unregistered session) holds a claim instead, for as long as it runs.
_claims: dict[str, set] = {}
_claims_lock = threading.Lock()


def claim(server_id: Optional[str], holder: str) -> None:
    """Mark a machine as in use by something that is not a registered game (a probe run, say)."""
    if server_id:
        with _claims_lock:
            _claims.setdefault(server_id, set()).add(holder)


def release(server_id: Optional[str], holder: str) -> None:
    """Undo :func:`claim`."""
    if server_id:
        with _claims_lock:
            _claims.get(server_id, set()).discard(holder)


def occupied(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[str]:
    """What is using a model server right now: the names of live games with an AI seat on it, and claims.

    A game counts while it is being played - not finished, not closed - even between its AI's turns,
    because its next turn will want the machine. A paused game counts too: it resumes where it left
    off. ``exclude`` holds session ids the caller already accounts for (its own games).

    This is what lets queued benchmarks and probes wait for a machine instead of fighting a running
    game for its single slot.
    """
    if not server_id:
        return []
    from ..server.session import all_sessions
    out = []
    for s in all_sessions():
        if s.id in exclude or s.stopped or s.game.s.phase != "playing":
            continue
        # only a seat still in the game: an eliminated model will never be asked anything again, even if
        # the bots play on to the end
        if any(seat.type == "llm" and (seat.llm or {}).get("server_id") == server_id and _alive(s, seat.player)
               for seat in s.seats):
            out.append(f"game “{s.name}”")
    with _claims_lock:
        out.extend(sorted(_claims.get(server_id, ())))
    return out


def _alive(session, pid: int) -> bool:
    """Whether a seat's civilization is still in the game."""
    try:
        return bool(session.game.player(pid).alive)
    except Exception:
        return True


def max_parallel(pooled: dict) -> int:
    """How many requests a pooled machine takes at once (its registration, or 1)."""
    try:
        return max(1, int(((pooled.get("config") or {}).get("connection") or {}).get("max_parallel") or 1))
    except (TypeError, ValueError):
        return 1

#: Settings a model entry may carry for its seats, copied into the agent configuration.
_INFERENCE = ("reasoning_effort", "max_tokens", "temperature", "max_tool_calls_per_turn", "tool_mode")


def lookup(server_id: Optional[str]) -> Optional[dict]:
    """A pooled server as a plain dict (id, name, provider, config, enabled), or None if there is none."""
    if not server_id:
        return None
    try:
        from .. import db
        from ..db.models import Server
        with db.session() as s:
            sv = s.get(Server, server_id)
            if sv is None:
                return None
            return {"id": sv.id, "name": sv.name, "provider": sv.provider, "enabled": sv.enabled,
                    "config": dict(sv.config or {})}
    except Exception:
        return None


def live_models(server_id: str) -> list[dict]:
    """What the machine's helper reports right now: key, whether it is loaded, context length."""
    try:
        from ..server.workers import hub
        connection = hub().get(server_id)
        return list(connection.models) if connection else []
    except Exception:
        return []


def resolve(llm: dict, pooled: dict) -> dict:
    """The agent configuration for a seat on a pooled server: the ``worker`` provider, which sends every
    request down the helper's connection."""
    from ..servers import SEAT_OVERRIDES
    key = llm.get("model") or llm.get("model_id") or ""
    if not key:
        loaded = [m for m in live_models(pooled["id"]) if m.get("loaded")]
        key = loaded[0]["key"] if loaded else ""
    entry = next((m for m in pooled["config"].get("models") or [] if m.get("key") == key), {})
    cfg = {"server_id": pooled["id"], "server": pooled["name"], "provider": "worker", "model": key,
           "model_id": key, "tool_mode": "native"}
    inference = entry.get("inference") or {}
    for k in _INFERENCE:
        if inference.get(k) not in (None, "", "auto"):
            cfg[k] = inference[k]
    for k in SEAT_OVERRIDES:
        if llm.get(k) not in (None, "", "auto"):
            cfg[k] = llm[k]
    return cfg


def describe(llm: dict, pooled: dict) -> dict:
    """Display info for a seat on a pooled server, in the shape ``servers.describe_seat`` returns."""
    key = llm.get("model") or llm.get("model_id")
    return {"server_id": pooled["id"], "server": pooled["name"], "model": key, "label": key, "profile": None,
            "missing_server": False, "restricted_until": None, "pooled": True}


def authorize(session, user, seats: list[dict]) -> None:
    """Refuse a game whose LLM seats use pooled machines the creator may not use for games right now.

    Raises ValueError with the admission system's own explanation (no grant, outside a window, budget
    spent, machine offline...).
    """
    from ..db.models import Server
    from ..server.workers import hub
    from . import admission
    for seat in seats:
        llm = seat.get("llm") or {}
        if seat.get("type") != "llm" or not llm.get("server_id") or lookup(llm["server_id"]) is None:
            continue
        server = session.get(Server, llm["server_id"])
        decision = admission.check(session, user, server, "game", online=hub().is_online(server.id),
                                   in_flight=hub().in_flight(server.id))
        # "busy right now" is fine for a game: its turns take their place in the machine's queue
        if not decision.allowed and decision.code != "concurrency":
            raise ValueError(f"{server.name}: {decision.reason}")
