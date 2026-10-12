"""Game seats on pooled machines.

A seat's ``llm`` block names a machine by id. Every machine is a row of the ``servers`` table (design/phase3-features.md
P3.2.8): CITAR dials one whose ``reach`` is ``direct`` itself (``citar.servers.resolve_llm``), and plays one whose
``reach`` is ``worker`` through the helper connected from that machine. This module turns a seat naming a helper
machine into an agent configuration that plays through its helper, and checks that whoever creates the game may use
the machine, whichever its reach: each seat by its task class (``seat_class``: a model seat that plays every turn is
*full*, one asked a prompt at a time *partial*), and each pooled prompt when its ticket is made (``ticket``).
"""
from __future__ import annotations

from datetime import datetime
from typing import Optional

# ---------------------------------------------------------------------------- who is using a machine
# A machine is in use by the leases live on it (design/phase3-features.md P3.3.5): every living model seat of a
# registered game (made when its session is registered, released when the seat changes, is eliminated or the game ends,
# stops, crashes or makes way) and a probe's turn case's, for the case. Before package 3-08 this module walked every
# session's seats and kept a table of "claims" for the work that was not a registered game; the scheduler's leases
# replace both, so what waits for a machine and what the scheduler serves are the same list. Pooled prompts (a report's
# narrative, a probe's offer and message cases, a hybrid seat's diplomacy) are tickets and never occupy a machine.


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
    """``occupied`` with what each lease serves: {"label", "kind" (the queue's kind: game, benchmark, probe; None when
    the lease names none), "id" (a game's id, or its run's), "game" (the session id of a game
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


# ---------------------------------------------------------------------------- notes that name other work
# A queued job, run or report says what it waits for: the work holding its machine, or the work ahead of it. That work
# may be another member's private game or run (P3.2.4, P3.9.1), so a note is kept as its words and its holders
# (``note``), its stored text names no holder (``note_text`` with no namer: "someone's game"), and whoever serves it
# tells each viewer the names they may see (``citar.server.app._namer``).

#: How work a viewer may not see is named, by the shared queue's kind.
SOMEONES = {"benchmark": "someone's benchmark run", "probe": "someone's probe run", "report": "someone's report",
            "game": "someone's game", "lab": "someone's lab experiment"}
#: How work is named where no viewer is known (a game's pause, a run's log), by the shared queue's kind.
KINDS = {"benchmark": "a benchmark run", "probe": "a probe run", "report": "a report", "game": "a game",
         "lab": "a lab experiment"}


def anonymous(holder: dict) -> str:
    """A holder as somebody who may not see its work is told of it ("someone's game")."""
    return SOMEONES.get(holder.get("kind"), "someone's work")


def kind_text(holder: dict) -> str:
    """A holder by its kind alone ("a benchmark run"), for words every viewer of a game or run reads."""
    return KINDS.get(holder.get("kind"), "other work")


def holders(server_id: Optional[str], exclude: frozenset = frozenset()) -> list[dict]:
    """The work holding a machine (``occupants``), each holder once: {"kind", "id", "label"}, ``id`` being what the
    work's access is decided on (a game's id, a run's or a report's)."""
    return distinct(occupants(server_id, exclude))


def distinct(occs: list) -> list[dict]:
    """``occupants`` (one per lease) as holders, each once: {"kind", "id", "label"}."""
    out, seen = [], set()
    for occ in occs:
        key = (occ.get("kind"), occ.get("id"), occ["label"])
        if key not in seen:
            seen.add(key)
            out.append({"kind": occ.get("kind"), "id": occ.get("id"), "label": occ["label"]})
    return out


def queue_holder(item: dict) -> dict:
    """The work a shared queue item is (``pool.queue``: its kind, its ``group`` and its label), as a holder."""
    return {"kind": item.get("kind"), "id": item.get("group") or item.get("id"), "label": item.get("label") or ""}


def note(prefix: str, held: list, suffix: str = "") -> dict:
    """A note naming other work: its words before and after the names of the holders ``held``, and the holders."""
    return {"prefix": prefix, "holders": [dict(h) for h in held], "suffix": suffix}


def note_text(note: Optional[dict], name=None) -> Optional[str]:
    """A note in words, each holder named once by ``name(holder)`` (a viewer's namer), or by ``anonymous`` when no
    namer is given (what is stored, and what a reader no route renders for is told)."""
    if not note:
        return None
    name = name or anonymous
    return note["prefix"] + ", ".join(dict.fromkeys(name(h) for h in note.get("holders") or [])) + note.get("suffix", "")


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


# ---------------------------------------------------------------------------- task classes and admission
# design/phase3-features.md P3.3.7: a model seat that plays every turn is task class *full* (it holds a lease on its
# machine), one that asks a prompt at a time is *partial* (tickets: 3-14's hybrid seat). Each class needs its
# capability and a grant for its purpose; every place that asks "what does this seat play" asks ``seat_class``.

#: A seat type's task class.
SEAT_CLASSES = {"llm": "full", "hybrid": "partial"}
#: The seat types that play a model, whose seats name a machine (``citar.server.app._member_seat``).
MODEL_SEAT_TYPES = tuple(SEAT_CLASSES)
#: What each task class needs: the capability, and the grant purpose it is admitted for (``admission.PURPOSES``).
CLASS_NEEDS = {"full": ("full_llm", "game"), "partial": ("partial_llm", "pooled")}
#: The capability work of a purpose needs besides its own (a report's ``run_reports``, a probe's ``run_probes``): a
#: pooled prompt, whatever it is part of, is the account's use of pooled prompts (``partial_llm``).
PURPOSE_CAPABILITY = {"pooled": "partial_llm"}


def seat_class(seat) -> Optional[str]:
    """A seat's task class: ``full`` for a model seat that plays every turn (``llm``), ``partial`` for one whose model
    is asked a prompt at a time (``hybrid``), None for a seat that plays no model (a person, an MCP client, a bot, a
    script) or for anything that is not a seat. Takes a seat as a dict (a request's, a scenario's) or a session's
    ``Seat``."""
    kind = seat.get("type") if isinstance(seat, dict) else getattr(seat, "type", None)
    return SEAT_CLASSES.get(kind) if isinstance(kind, str) else None


def authorize(session, user, seats: list, *, queued: bool = False) -> None:
    """Refuse a game whose model seats ``user`` may not seat: each by its task class (``seat_class``), which needs its
    capability (``full_llm`` or ``partial_llm``) and, on the machine its block names, a grant for its purpose
    (``game`` or ``pooled``) that admits ``user`` now.

    Raises ValueError with the reason: the capability's (``policy.denial``), or the admission system's own (no grant,
    outside a window, budget spent, the helper offline...), except to somebody who may not even see the machine: they
    are told only that they may not use it, since the explanation names the machine and its group. Every machine is a
    row (the host and the site's API rows included), so every seat with a machine is admitted here. Admission decides
    who may, the scheduler when (P3.3.7): a busy machine is never a refusal. ``queued`` is for work that waits its turn:
    a refusal that only says "not yet" (outside a window, a spent period, the helper offline) lets it through."""
    from ..auth import policy
    for seat in seats:
        cls = seat_class(seat)
        if cls is None:
            continue
        capability, purpose = CLASS_NEEDS[cls]
        if not policy.can(user, capability):
            raise ValueError(policy.denial(user, capability))
        llm = (seat.get("llm") if isinstance(seat, dict) else getattr(seat, "llm", None)) or {}
        _admit_one(session, user, llm.get("server_id"), purpose, queued=queued)


def admit(session, user, server_ids, purpose: str) -> None:
    """Admission for work that names machines rather than seats and waits its turn for them (a benchmark run's
    machines, a probe run's subject, a report's narrative): may ``user`` use each one for ``purpose`` at all, with the
    capability the purpose needs (``PURPOSE_CAPABILITY``: pooled prompts need ``partial_llm``). The work's own
    capability (``run_benchmarks``, ``run_probes``, ``run_reports``) is its route's to check. Raises ValueError as
    ``authorize`` does."""
    from ..auth import policy
    wanted = [sid for sid in server_ids if sid]
    needs = PURPOSE_CAPABILITY.get(purpose)
    if wanted and needs and not policy.can(user, needs):
        raise ValueError(policy.denial(user, needs))
    for sid in wanted:
        _admit_one(session, user, sid, purpose, queued=True)


def _admit_one(session, user, server_id, purpose: str, *, queued: bool) -> None:
    """``authorize``'s check of one machine for ``purpose``."""
    from ..auth import access
    from ..db.models import Server
    from . import admission
    server = session.get(Server, server_id) if isinstance(server_id, str) and server_id else None
    if server is None:
        return
    decision = admission.check(session, user, server, purpose, online=helper_online(server), in_flight=None)
    if decision.allowed or (queued and (decision.temporary or decision.code == "offline")):
        return
    if access.VIEW not in access.on(session, user, server):
        raise ValueError("You may not use that machine. Pick one of the machines you may use.")
    raise ValueError(f"{server.name}: {decision.reason}")


def helper_online(server) -> Optional[bool]:
    """Whether a helper machine's helper is connected, for admission; None for a machine CITAR dials itself, which has
    no connection to be absent (as ``citar.server.pool_api`` reports it), rather than "offline" for ever."""
    if (getattr(server, "reach", None) or "worker") != "worker":
        return None
    from ..server.workers import hub
    return hub().is_online(server.id)


def ticket(server_id: Optional[str], model: str = "", *, cls: str, consumer: Optional[str] = None,
           purpose: str = "pooled", capability: Optional[str] = None, **kw):
    """A pooled prompt's ticket (``scheduler.Scheduler.ticket``, which takes ``kw``), admitted when it is made
    (design/phase3-features.md P3.3.7): may ``consumer`` use ``server_id`` for ``purpose`` now, with ``capability``
    when the work needs one (a report's narrative ``run_reports``, a probe's case ``run_probes``). Admission decides who
    may and the scheduler when: the check skips the capacity step, so a busy machine queues the prompt. A refusal is
    the ticket's ``refused``, with the admission's reason, its ``code`` and its ``retry_at`` (a helper that is offline
    refuses at once, as temporary); an admitted ticket is capped by the grant's concurrency (the scheduler's per-
    consumer cap, leases and tickets together). No consumer (the server's own work), no machine, or no database: no
    admission. The admitting grant's budget, when it has one, is the ticket's to reserve against (``budget``,
    P3.4.6). Reads the database, so never call it with a session's or a machine's lock held."""
    from . import scheduler
    refusal, cap, budget = None, 0, None
    if consumer and server_id:
        refusal, cap, budget = _ticket_admission(consumer, server_id, purpose, capability)
    return scheduler.get().ticket(server_id, model, cls=cls, consumer=consumer, cap=cap, refusal=refusal,
                                  budget=budget, **kw)


def _ticket_admission(consumer: str, server_id: str, purpose: str, capability: Optional[str]) -> tuple:
    """``ticket``'s admission: (the refusal as a ``SchedulerError``, or None; the grant's concurrency, 0 for none;
    the grant's budget id, or None)."""
    from .. import db
    from ..auth import access, policy
    from ..db.models import Server, User
    from . import admission, budgets, scheduler
    if not db.configured():
        return None, 0, None
    try:
        with db.session() as s:
            user, server = s.get(User, consumer), s.get(Server, server_id)
            if server is None:
                return None, 0, None    # no row: an unscheduled machine, which the scheduler passes through
            if user is None:
                return scheduler.MachineRefused("The account this prompt is for no longer exists.",
                                                code="account"), 0, None
            if capability and not policy.can(user, capability):
                return scheduler.MachineRefused(policy.denial(user, capability), code="capability"), 0, None
            needs = PURPOSE_CAPABILITY.get(purpose)
            if needs and not policy.can(user, needs):
                return scheduler.MachineRefused(policy.denial(user, needs), code="capability"), 0, None
            decision = admission.check(s, user, server, purpose, online=helper_online(server), in_flight=None)
            if decision.allowed:
                grant = decision.grant
                return (None, max(0, int(grant.concurrency or 0)) if grant is not None else 0,
                        budgets.keeper().handle(s, grant))
            reason = decision.reason
            if access.VIEW not in access.on(s, user, server):
                reason = "You may not use that machine."
            retry = decision.retry_at.astimezone().replace(tzinfo=None) if decision.retry_at else None
            return scheduler.MachineRefused(reason, code=decision.code or "admission", retry_at=retry), 0, None
    except Exception:
        import logging
        logging.getLogger("citar.pool").warning("could not admit a pooled prompt of %s on %s", consumer, server_id,
                                                exc_info=True)
        return scheduler.MachineRefused("The prompt could not be admitted: the server could not read its grants.",
                                        code="admission"), 0, None


def consumer_cap(user_id: Optional[str], server_id: Optional[str], purpose: str = "game") -> int:
    """The concurrency of the grant that lets ``user_id`` use ``server_id`` for ``purpose`` (``AccessGrant.concurrency``;
    design/phase3-features.md P3.3.7): the prompt scheduler's cap on that consumer's running requests on the machine,
    leases and tickets together. 0 for no cap: the machine's owner, an administrator, no account (the server's own
    work), no machine, a grant with none, or no grant at all (admission has refused that already). Reads the
    database."""
    return consumer_terms(user_id, server_id, purpose)[0]


def consumer_terms(user_id: Optional[str], server_id: Optional[str], purpose: str = "game") -> tuple:
    """(``consumer_cap``, the id of that grant's budget or None): what a lease of ``user_id`` on ``server_id`` is held
    to. The budget is the one the grant admitting the work carries (P3.4.6), loaded into the process's budget keeper,
    whether or not it has room left now (a lease made with its budget spent pauses at its first request). Reads the
    database."""
    if not user_id or not server_id:
        return 0, None
    from .. import db
    from ..db.models import Server, User
    from . import admission, budgets
    if not db.configured():
        return 0, None
    try:
        with db.session() as s:
            user, server = s.get(User, user_id), s.get(Server, server_id)
            if user is None or server is None:
                return 0, None
            grant = admission.check(s, user, server, purpose, in_flight=None).grant
            if grant is None:
                return 0, None
            return max(0, int(grant.concurrency or 0)), budgets.keeper().handle(s, grant)
    except Exception:
        import logging
        logging.getLogger("citar.pool").warning("could not read the grant for %s on %s", user_id, server_id,
                                                exc_info=True)
        return 0, None
