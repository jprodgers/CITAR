"""Game seats on pooled machines.

A seat's ``llm`` block names a machine by id. Every machine is a row of the ``servers`` table (design/phase3-features.md
P3.2.8): CITAR dials one whose ``reach`` is ``direct`` itself (``citar.servers.resolve_llm``), and plays one whose
``reach`` is ``worker`` through the helper connected from that machine. This module turns a seat naming a helper
machine into an agent configuration that plays through its helper, and checks that whoever creates the game may use
the machine, whichever its reach.
"""
from __future__ import annotations

import threading
from datetime import datetime
from typing import Optional

# ---------------------------------------------------------------------------- who is using a machine
# Game sessions are found through every SessionManager; work that runs games outside one (a probe
# case plays in an unregistered session) holds a claim instead, for as long as it runs.
#: server id -> {holder label: (queue kind, id) of the work, or None}
_claims: dict[str, dict] = {}
_claims_lock = threading.Lock()


def claim(server_id: Optional[str], holder: str, ref: Optional[tuple] = None) -> None:
    """Mark a machine as in use by something that is not a registered game (a probe run, say). ``ref`` is the work's
    (queue kind, id), so the Queue page can name it only to those who may see it (``occupants``)."""
    if server_id:
        with _claims_lock:
            _claims.setdefault(server_id, {})[holder] = ref


def release(server_id: Optional[str], holder: str) -> None:
    """Undo :func:`claim`."""
    if server_id:
        with _claims_lock:
            _claims.get(server_id, {}).pop(holder, None)


def occupied(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[str]:
    """What is using a model server right now: the names of live games with an AI seat on it, and claims.

    A game counts while it is being played - not finished, not closed - even between its AI's turns,
    because its next turn will want the machine. A paused game counts too: it resumes where it left
    off. ``exclude`` holds session ids the caller already accounts for (its own games).

    This is what lets queued benchmarks and probes wait for a machine instead of fighting a running
    game for its single slot.
    """
    return [occ["label"] for occ in occupants(server_id, exclude)]


def occupants(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[dict]:
    """``occupied`` with what each holder is: {"label", "kind" (the queue's kind: game, benchmark, probe, report;
    None when a claim did not say), "id" (a game's id, a run's)}. A benchmark job's game is a game of kind
    benchmark, named by its run."""
    if not server_id:
        return []
    from ..server.session import all_sessions
    out = []
    for s in all_sessions():
        if s.id in exclude or s.stopped or s.game.phase != "playing":
            continue
        # a game that paused to make way for higher-priority work gives the machine up until its turn comes
        if s.paused and (s.pause_reason or {}).get("kind") == "queue":
            continue
        # only a seat still in the game: an eliminated model will never be asked anything again, even if
        # the bots play on to the end
        if any(seat.type == "llm" and (seat.llm or {}).get("server_id") == server_id and _alive(s, seat.player)
               for seat in s.seats):
            bench = getattr(s, "benchmark", None) or {}
            if bench.get("run_id"):
                out.append({"label": f"game “{s.name}”", "kind": "benchmark", "id": bench["run_id"]})
            else:
                out.append({"label": f"game “{s.name}”", "kind": "game", "id": s.id})
    with _claims_lock:
        held = dict(_claims.get(server_id, {}))
    for holder in sorted(held):
        kind, ident = held[holder] or (None, None)
        out.append({"label": holder, "kind": kind, "id": ident})
    return out


def _alive(session, pid: int) -> bool:
    """Whether a seat's civilization is still in the game."""
    try:
        return session.game.is_alive(pid)
    except Exception:
        return True


def max_parallel(pooled: dict) -> int:
    """How many requests a machine takes at once: its row's slots (``Server.max_concurrent``, the one number of
    P3.2.8), or 1."""
    try:
        return max(1, int(pooled.get("max_concurrent") or 1))
    except (TypeError, ValueError):
        return 1

#: Settings a model entry may carry for its seats, copied into the agent configuration.
_INFERENCE = ("reasoning_effort", "max_tokens", "temperature", "max_tool_calls_per_turn", "tool_mode")


def lookup(server_id: Optional[str]) -> Optional[dict]:
    """A machine as a plain dict (id, name, provider, reach, enabled, max_concurrent, config as stored, owner_tz),
    or None if there is none. Read from ``citar.servers``' cache of the rows, so asking costs no database query."""
    if not server_id or not isinstance(server_id, str):
        return None
    from .. import servers
    m = servers.machine(server_id)
    if m is None:
        return None
    return {k: m[k] for k in ("id", "name", "provider", "reach", "enabled", "max_concurrent", "config", "owner_tz")}


def restricted(pooled: Optional[dict], when: Optional[datetime] = None) -> Optional[datetime]:
    """When a pooled machine is inside its quiet hours: the moment they end, else None.

    The hours are the owner's wall clock ("21:00-06:00" means 9pm where the machine is), so they are read in
    the owner's zone. Like ``servers.restricted``, ``when`` and the result are naive local times of the
    computer CITAR runs on, which is what every caller compares them with.
    """
    if not pooled:
        return None
    from .. import servers
    from .windows import zone
    z = zone(pooled.get("owner_tz"))
    local = (when or datetime.now()).astimezone(z).replace(tzinfo=None)
    end = servers.restricted({"restricted_hours": (pooled.get("config") or {}).get("restricted_hours")}, local)
    if end is None:
        return None
    return end.replace(tzinfo=z).astimezone().replace(tzinfo=None)


def clock_text(pooled: Optional[dict], end: Optional[datetime]) -> Optional[str]:
    """A time from :func:`restricted` as the machine's owner reads it ("06:00" where the machine is)."""
    if end is None:
        return None
    from .windows import zone
    return end.astimezone(zone((pooled or {}).get("owner_tz"))).strftime("%H:%M")


def live_models(server_id: str) -> list[dict]:
    """What the machine's helper reports right now: key, whether it is loaded, context length."""
    try:
        from ..server.workers import hub
        connection = hub().get(server_id)
        return list(connection.models) if connection else []
    except Exception:
        return []


def resolve(llm: dict, pooled: dict) -> dict:
    """The agent configuration for a seat on a helper machine: the ``worker`` provider, which sends every
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
    """Display info for a seat on a helper machine, in the shape ``servers.describe_seat`` returns."""
    key = llm.get("model") or llm.get("model_id")
    end = restricted(pooled)
    return {"server_id": pooled["id"], "server": pooled["name"], "model": key, "label": key, "profile": None,
            "missing_server": False, "restricted_until": clock_text(pooled, end), "pooled": True}


def authorize(session, user, seats: list[dict], purpose: str = "game", *, queued: bool = False) -> None:
    """Refuse a game whose LLM seats use pooled machines the creator may not use for games right now.

    Raises ValueError with the admission system's own explanation (no grant, outside a window, budget
    spent, machine offline...), except to somebody who may not even see the machine: they are told only
    that they may not use it, since the explanation names the machine and its group. Every machine is a row
    (the host and the site's API rows included, since the registry merge), so every seat with a machine is
    admitted here; a helper machine is online while its helper is connected, a machine CITAR dials has no
    such state (``liveness``). ``purpose`` is the
    work the seats do (``admission.PURPOSES``): a benchmark run's machines are admitted for `benchmark`, a
    probe run's subject for `probe` (design/phase3-features.md P3.2.4: "create" is the capability plus
    admission where the work runs on a machine). ``queued`` is for work that waits its turn (a run): who may is
    decided now, when is the runner's, so a refusal that only says "not yet" (outside a window, a spent period,
    the helper offline, every slot busy) lets it through.
    """
    from ..auth import access
    from ..db.models import Server
    from . import admission
    for seat in seats:
        llm = seat.get("llm") or {}
        if seat.get("type") != "llm" or not llm.get("server_id"):
            continue
        server = session.get(Server, llm["server_id"]) if isinstance(llm["server_id"], str) else None
        if server is None:
            continue
        online, in_flight = liveness(server)
        decision = admission.check(session, user, server, purpose, online=online, in_flight=in_flight)
        # "busy right now" is fine for a game: its turns take their place in the machine's queue
        if queued and (decision.temporary or decision.code == "offline"):
            continue
        if not decision.allowed and decision.code != "concurrency":
            if access.VIEW not in access.on(session, user, server):
                raise ValueError("You may not use that machine. Pick one of the machines you may use.")
            raise ValueError(f"{server.name}: {decision.reason}")


def liveness(server) -> tuple:
    """``(online, in_flight)`` for admission: a helper machine is online while its helper is connected, with the
    requests it has in flight; a machine CITAR dials itself has no connection to be absent, so both are unknown
    (None), as ``citar.server.pool_api`` reports it, rather than "offline" for ever."""
    if (getattr(server, "reach", None) or "worker") != "worker":
        return None, None
    from ..server.workers import hub
    return hub().is_online(server.id), hub().in_flight(server.id)


def consumer_cap(user_id: Optional[str], server_id: Optional[str], purpose: str = "game") -> int:
    """The concurrency of the grant that lets ``user_id`` use ``server_id`` for ``purpose`` (``AccessGrant.concurrency``;
    design/phase3-features.md P3.3.7): the prompt scheduler's cap on that consumer's running requests on the machine,
    leases and tickets together. 0 for no cap: the machine's owner, an administrator, no account (the server's own
    work), no machine, a grant with none, or no grant at all (admission has refused that already). Reads the
    database."""
    if not user_id or not server_id:
        return 0
    from .. import db
    from ..db.models import Server, User
    from . import admission
    if not db.configured():
        return 0
    try:
        with db.session() as s:
            user, server = s.get(User, user_id), s.get(Server, server_id)
            if user is None or server is None:
                return 0
            grant = admission.check(s, user, server, purpose, in_flight=None).grant
            return max(0, int(grant.concurrency or 0)) if grant is not None else 0
    except Exception:
        import logging
        logging.getLogger("citar.pool").warning("could not read the grant for %s on %s", user_id, server_id,
                                                exc_info=True)
        return 0


def admit(session, user, server_ids, purpose: str) -> None:
    """``authorize`` for work that names machines rather than seats and waits its turn for them (a benchmark run's
    machines, a probe run's subject, a report's narrative): may ``user`` use each pooled one for ``purpose`` at all.
    Raises ValueError as ``authorize`` does."""
    authorize(session, user, [{"type": "llm", "llm": {"server_id": sid}} for sid in server_ids if sid], purpose,
              queued=True)
