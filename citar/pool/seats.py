"""Game seats on pooled machines.

A seat's ``llm`` block names a machine by id. Every machine is a row of the ``servers`` table (design/phase3-features.md
P3.2.8): CITAR dials one whose ``reach`` is ``direct`` itself (``citar.servers.resolve_llm``), and plays one whose
``reach`` is ``worker`` through the helper connected from that machine. This module turns a seat naming a helper
machine into an agent configuration that plays through its helper, and checks that whoever creates the game may use
the machine, whichever its reach.
"""
from __future__ import annotations

from datetime import datetime
from typing import Optional

# ---------------------------------------------------------------------------- who is using a machine
# A machine is in use by the leases live on it (design/phase3-features.md P3.3.5): every living model seat of a
# registered game (made when its session is registered, released when the seat changes, is eliminated or the game ends,
# stops, crashes or makes way), a probe run's and a report's narrative's. Before package 3-08 this module walked every
# session's seats and kept a table of "claims" for the work that was not a registered game; the scheduler's leases
# replace both, so what waits for a machine and what the scheduler serves are the same list.


def occupied(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[str]:
    """What is using a model machine right now: the label of every lease live on it, oldest first, one per lease (a
    game with two model seats on the machine is there twice; ``held_text`` names each holder once). ``exclude`` holds
    game session ids the caller already accounts for (its own games).

    A game counts from its registration (created, loaded paused, restored, reloaded by its benchmark, launched from a
    scenario) to its end, even between its seats' turns and while a person, quiet hours or a lost connection pauses
    it, because its next turn will want the machine; a game that paused to make way for higher-priority work, an
    eliminated seat and a finished game do not. This is what lets queued benchmarks, probe runs and reports wait for a
    machine instead of fighting a running game for its slot.
    """
    return [occ["label"] for occ in occupants(server_id, exclude)]


def occupants(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[dict]:
    """``occupied`` with what each lease serves: {"label", "kind" (the queue's kind: game, benchmark, probe, report;
    None when the lease names none), "id" (a game's id, a run's or a report's), "game" (the session id of a game
    seat's lease)}. A benchmark job's game is a game of kind benchmark, named by its run."""
    if not server_id:
        return []
    from . import scheduler
    sched = scheduler.current()
    if sched is None:
        return []
    out = []
    for lease in sched.leases(server_id):
        if lease.game is not None and lease.game in exclude:
            continue
        kind, ident = lease.ref or (None, None)
        out.append({"label": lease.label, "kind": kind, "id": ident, "game": lease.game})
    return out


def held_text(held: list) -> str:
    """The holders of a machine as a sentence part, each named once ("game “a”, report “b”")."""
    return ", ".join(dict.fromkeys(held))


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
