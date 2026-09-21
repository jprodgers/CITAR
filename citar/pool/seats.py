"""Game seats on pooled machines.

A seat's ``llm`` block names a server by id. Servers in the admin registry (``config/servers.json``)
are dialled by CITAR itself; servers registered on the Servers page live in the database and are
reached through the helper (worker) connected from that machine. This module turns a seat naming a
pooled server into an agent configuration that plays through its helper, and checks that whoever
creates the game may use the machine.
"""
from __future__ import annotations

from typing import Optional

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
        if not decision.allowed:
            raise ValueError(f"{server.name}: {decision.reason}")
