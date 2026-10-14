"""HTTP API for the shared server pool: servers, groups, grants, windows and budgets.

Every machine is a row (design/phase3-features.md P3.2.8); ``admin_api.py`` keeps the registry's routes (a machine's
configuration, model loading on the host, keys, electricity plans) with a gate per route. These routes are
per-owner: an ordinary account with the `register_servers` capability can add its own hardware, put it in a group
and decide who may use it on what terms. A machine the server dials itself (``reach = direct``) is an
administrator's to add, unless it is the dry run (``pool.check_reach``: 403). Revoking a worker token, disabling a
machine and deleting one drop its helper's connection once the change is committed (``WorkerHub.drop``).
"""
from __future__ import annotations

import logging
import secrets
import threading
from datetime import datetime, timezone
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field
from sqlalchemy.orm import Session

from .. import pool
from ..auth import access, audit
from ..auth.deps import Principal, get_db, principal, require_cap, require_user
from ..db.models import AccessGrant, Server, ServerGroup, User
from ..pool import admission, budgets

log = logging.getLogger("citar.pool_api")
router = APIRouter(prefix="/api/pool")


def _fail(exc: Exception, status: int = 400) -> HTTPException:
    """Turn an internal error into an HTTP error: 403 for a machine the caller may not make one CITAR dials."""
    return HTTPException(403 if isinstance(exc, pool.ReachRefused) else status, str(exc))


def _saved(s: Session) -> None:
    """Commit the request's change before it answers. The request's session would otherwise commit only after the
    answer is sent (FastAPI ends a request-scoped dependency after the response), and the Servers page reads a change
    back at once: a card redrawn after its dialog saves would show what was there before."""
    s.commit()


def _drop(s: Session, server_id: str, reason: str, *, token_id: Optional[str] = None) -> None:
    """Commit the request's change, then disconnect the machine's helper (P3.2.8): its next hello finds the change
    and is refused, and nothing it was doing is waited for."""
    from .workers import hub
    s.commit()
    hub().drop(server_id, reason, token_id=token_id)


def _server(s: Session, server_id: str, viewer: Optional[User], permission: str) -> Server:
    """A pooled server the viewer has a permission on, or 404.

    404 rather than 403 when the viewer cannot see it: a 403 confirms the server exists, which turns
    guessing ids into enumeration.
    """
    server = s.get(Server, server_id)
    if server is None:
        raise HTTPException(404, "No such server.")
    access.on(s, viewer, server).require(permission)
    return server


def _group(s: Session, group_id: str, viewer: Optional[User], permission: str) -> ServerGroup:
    """A server group the viewer has a permission on, or 404."""
    group = s.get(ServerGroup, group_id)
    if group is None:
        raise HTTPException(404, "No such server group.")
    access.on(s, viewer, group).require(permission)
    return group


# ---------------------------------------------------------------------------- liveness

def _liveness(server_id: str) -> Optional[bool]:
    """Whether a worker is currently connected for this server.

    Liveness is a held connection plus a recent pong, not a configuration flag — a server is online
    only while an agent is actually there to answer. A `direct` server (one CITAR dials itself, only
    sane on a trusted network) has no worker, so it reports unknown rather than offline.
    """
    from ..db.models import Server as _Server
    try:
        from .. import db
        from .workers import hub
        if hub().is_online(server_id):
            return True
        with db.session() as s:
            server = s.get(_Server, server_id)
            # Nothing to be offline: a direct server is reached on demand.
            return None if (server is None or server.reach != "worker") else False
    except Exception:
        return None


# ---------------------------------------------------------------------------- servers

@router.get("/servers")
def list_servers(request: Request, purpose: str = "game", p: Principal = Depends(principal),
                 s: Session = Depends(get_db, scope="function")):
    """Every server this account can see, each with whether it may be used right now and why not.

    Refused servers are included with their reason rather than hidden: somebody should be able to
    see that the overnight box exists and that it opens in four hours, instead of wondering why a
    server they were told about is missing.
    """
    me = require_user(request, p)
    out = []
    for server in pool.servers_for(s, me):
        # no capacity step (P3.3.7): a machine busy right now queues the work, it does not refuse it
        decision = admission.check(s, me, server, purpose, online=_liveness(server.id), in_flight=None)
        entry = pool.public_server(s, server, me)
        entry["admission"] = decision.client()
        entry["online"] = _liveness(server.id)
        # what the helper reports right now, with which model is loaded - what a game seat can pick
        from ..pool.seats import live_models
        entry["live_models"] = live_models(server.id)
        out.append(entry)
    return {"servers": out, "purposes": list(admission.PURPOSES), "viewer_tz": me.tz}


class ServerBody(BaseModel):
    """A machine being registered with the pool."""
    config: dict = Field(default_factory=dict)
    group_id: Optional[str] = None
    reach: str = "worker"


@router.post("/servers")
def create_server(body: ServerBody, request: Request,
                  me: User = Depends(require_cap("register_servers")),
                  s: Session = Depends(get_db, scope="function")):
    """Register a machine, and issue its first worker token."""
    try:
        server = pool.create_server(s, me, body.config, group_id=body.group_id, reach=body.reach)
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    return pool.public_server(s, server, me)


@router.get("/servers/{server_id}")
def get_server(server_id: str, request: Request, p: Principal = Depends(principal),
               s: Session = Depends(get_db, scope="function")):
    """One machine's details. Cost configuration is included only for its owner."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.VIEW)
    out = pool.public_server(s, server, me)
    out["admission"] = admission.check(s, me, server, online=_liveness(server_id), in_flight=None).client()
    return out


class ServerUpdate(BaseModel):
    """The editable fields of a registered machine."""
    config: Optional[dict] = None
    group_id: Optional[str] = "__keep__"
    reach: Optional[str] = None
    enabled: Optional[bool] = None


@router.put("/servers/{server_id}")
def update_server(server_id: str, body: ServerUpdate, request: Request,
                  p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Change a machine's details."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    disabled = body.enabled is False and server.enabled
    if body.enabled is not None:
        server.enabled = body.enabled
        audit.record(s, "server.enabled" if body.enabled else "server.disabled", actor=me,
                     object_type="server", object_id=server.id)
    if body.config is not None:
        try:
            pool.update_server(s, me, server, body.config, group_id=body.group_id, reach=body.reach)
        except pool.PoolError as exc:
            raise _fail(exc)
    elif body.group_id != "__keep__":
        if body.group_id:
            _group(s, body.group_id, me, access.MANAGE)
        server.group_id = body.group_id or None
    if disabled:
        _drop(s, server.id, "its machine was disabled")
    else:
        _saved(s)
    return pool.public_server(s, server, me)


class Costing(BaseModel):
    """A machine's power figures, components and cost periods (any may be left out)."""
    power: Optional[dict] = None
    components: Optional[list] = None
    costs: Optional[list] = None
    former_ids: Optional[list] = None


@router.put("/servers/{server_id}/costing")
def set_costing(server_id: str, body: Costing, request: Request,
                p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Change what a machine costs to run: watts, hardware prices and the electricity plan."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    try:
        pool.set_costing(s, me, server, body.model_dump())
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    return pool.public_server(s, server, me)


@router.get("/servers/{server_id}/rates")
def machine_rates(server_id: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """What an hour on this machine costs, idle and with the model busy."""
    from .. import servers as registry
    me = require_user(request, p)
    _server(s, server_id, me, access.MANAGE)
    reg = registry.with_pooled()
    sv = next((x for x in reg["servers"] if x["id"] == server_id), None)
    if sv is None:
        raise HTTPException(404, "No such server.")
    return pool.machine_rates(sv, reg, me)


class QuietHours(BaseModel):
    """A machine's quiet hours: when every kind of work on it pauses."""
    restricted_hours: dict


@router.put("/servers/{server_id}/quiet-hours")
def set_quiet_hours(server_id: str, body: QuietHours, request: Request,
                    p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Change a machine's quiet hours, in its owner's time zone."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    try:
        pool.set_quiet_hours(s, me, server, body.restricted_hours)
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    return pool.public_server(s, server, me)


@router.delete("/servers/{server_id}")
def delete_server(server_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db, scope="function")):
    """Remove a machine from the pool: its row, then (once that is committed) its helper's connection and the key its
    stored configuration keeps."""
    from .. import servers as registry
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    config = pool.delete_server(s, me, server)
    _drop(s, server_id, "its machine was deleted")
    registry.forget_key(server_id, config)
    return {"deleted": server_id}


# ---------------------------------------------------------------------------- groups

@router.get("/groups")
def list_groups(request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """The server groups the viewer can see."""
    me = require_user(request, p)
    out = []
    for group in pool.groups_for(s, me):
        perms = access.on(s, me, group)
        owner = s.get(User, group.owner_id) if group.owner_id else None
        entry = {
            "id": group.id, "name": group.name, "description": group.description,
            "visibility": group.visibility, "owner": owner.public(me) if owner else None,
            "is_mine": group.owner_id == me.id, "can_manage": access.MANAGE in perms,
            "servers": [pool.public_server(s, sv, me)
                        for sv in s.query(Server).filter(Server.group_id == group.id)],
        }
        if access.MANAGE in perms:
            # No share link: a group's machines are shared by its grants alone, and no route takes a group's key
            # (a `link` group admits nobody by it yet), so a link would open nothing for whoever it was given to.
            entry["grants"] = [pool.grant_to_client(s, g, me)
                               for g in s.query(AccessGrant).filter(AccessGrant.group_id == group.id)]
        else:
            # A guest sees the terms that apply to *them*, not everybody else's grants.
            mine = admission.grants_for(s, me, group.id)
            entry["grants"] = [pool.grant_to_client(s, g, me) for g in mine[:1]]
        out.append(entry)
    return {"groups": out, "viewer_tz": me.tz}


class GroupBody(BaseModel):
    """A group of machines shared on the same terms."""
    name: str
    description: str = ""
    visibility: str = "private"


@router.post("/groups")
def create_group(body: GroupBody, request: Request,
                 me: User = Depends(require_cap("register_servers")),
                 s: Session = Depends(get_db, scope="function")):
    """Create a server group."""
    try:
        group = pool.create_group(s, me, name=body.name, description=body.description,
                                  visibility=body.visibility)
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    return {"id": group.id, "name": group.name, "visibility": group.visibility}


@router.put("/groups/{group_id}")
def update_group(group_id: str, body: GroupBody, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Change a group."""
    me = require_user(request, p)
    group = _group(s, group_id, me, access.MANAGE)
    group.name = (body.name or group.name).strip()[:120]
    group.description = (body.description or "").strip()
    if body.visibility != group.visibility:
        access.set_visibility(s, group, body.visibility, actor=me)
    _saved(s)
    return {"id": group.id, "name": group.name, "visibility": group.visibility}


@router.delete("/groups/{group_id}")
def delete_group(group_id: str, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db, scope="function")):
    """Delete a group."""
    me = require_user(request, p)
    group = _group(s, group_id, me, access.MANAGE)
    # Servers outlive their group; losing a group should not silently delete the hardware
    # registered in it, only un-share it.
    for server in s.query(Server).filter(Server.group_id == group.id):
        server.group_id = None
    audit.record(s, "server_group.deleted", actor=me, object_type="server_group", object_id=group.id)
    s.delete(group)
    _saved(s)
    return {"deleted": group_id}


# ---------------------------------------------------------------------------- grants

class WindowBody(BaseModel):
    """An availability window: weekday and minutes, in the owner's own time zone.

    Stored as weekday plus minutes rather than as UTC instants, and resolved late. The alternative
    breaks twice a year, when the offset changes and every window silently moves.
    """
    weekday: int
    start_min: int
    end_min: int


class BudgetBody(BaseModel):
    """A spending budget, in credits: an amount, a period, and what happens when it is reached."""
    period: str = "month"
    amount: float = 0.0
    unit: str = "credits"
    behavior: str = "hard"


class GrantBody(BaseModel):
    """Access granted to a group: who, what they may do, and any budget."""
    subject_type: str = "user"
    handle: Optional[str] = None
    purposes: list = Field(default_factory=list)
    concurrency: int = 0
    priority: int = 0
    note: str = ""
    expires_at: Optional[datetime] = None
    windows: list = Field(default_factory=list)
    budget: Optional[BudgetBody] = None


@router.post("/groups/{group_id}/grants")
def create_grant(group_id: str, body: GrantBody, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Grant access to a group."""
    me = require_user(request, p)
    group = _group(s, group_id, me, access.MANAGE)
    try:
        grant = pool.create_grant(
            s, me, group, subject_type=body.subject_type, subject_handle=body.handle,
            purposes=body.purposes, concurrency=body.concurrency, priority=body.priority,
            expires_at=body.expires_at, note=body.note,
            window_rows=[(w["weekday"], w["start_min"], w["end_min"]) for w in body.windows],
            budget=body.budget.model_dump() if body.budget else None)
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    return pool.grant_to_client(s, grant, me)


@router.put("/grants/{grant_id}")
def update_grant(grant_id: str, body: GrantBody, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Change a grant's terms."""
    me = require_user(request, p)
    grant = s.get(AccessGrant, grant_id)
    if grant is None:
        raise HTTPException(404, "No such grant.")
    _group(s, grant.group_id, me, access.MANAGE)
    for purpose in body.purposes:
        if purpose not in admission.PURPOSES:
            raise HTTPException(400, f"Unknown purpose {purpose!r}.")
    grant.purposes = list(body.purposes)
    grant.concurrency = max(0, body.concurrency)
    grant.priority = body.priority
    grant.note = (body.note or "").strip()[:500] or None
    grant.expires_at = body.expires_at
    pool.set_windows(s, grant, [(w["weekday"], w["start_min"], w["end_min"]) for w in body.windows])
    try:
        budgets.set_budget(s, grant, **(body.budget.model_dump() if body.budget
                                        else {"period": "month", "amount": 0}))
    except ValueError as exc:
        raise _fail(exc)
    audit.record(s, "grant.updated", actor=me, object_type="server_group", object_id=grant.group_id)
    _saved(s)
    # the work already running under the grant is held to its budget as it is now, from its next request (P3.4.6:
    # leases and tickets are bound to the grant, not to the budget it had when they were made)
    budgets.keeper().reload_grant(s, grant.id)
    return pool.grant_to_client(s, grant, me)


@router.delete("/grants/{grant_id}")
def revoke_grant(grant_id: str, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db, scope="function")):
    """Revoke a grant."""
    me = require_user(request, p)
    grant = s.get(AccessGrant, grant_id)
    if grant is None:
        raise HTTPException(404, "No such grant.")
    # the group's gate first: a grant of a group the caller may not see is a 404, as the group is
    _group(s, grant.group_id, me, access.MANAGE)
    try:
        pool.revoke_grant(s, me, grant)
    except pool.PoolError as exc:
        raise _fail(exc)
    _saved(s)
    budgets.keeper().reload_grant(s, grant_id)          # its budget went with it
    return {"revoked": grant_id}


# ---------------------------------------------------------------------------- worker tokens

@router.get("/servers/{server_id}/workers")
def worker_status(server_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db, scope="function")):
    """Whether a worker is connected for this server, and which tokens exist."""
    from ..db.models import WorkerToken
    from .workers import hub

    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    connection = hub().get(server.id)
    tokens_out = [
        {"id": t.id, "label": t.label, "prefix": t.prefix,
         "created_at": t.created_at.isoformat() if t.created_at else None,
         "last_seen_at": t.last_seen_at.isoformat() if t.last_seen_at else None,
         "last_ip": t.last_ip, "revoked": t.revoked_at is not None}
        for t in s.query(WorkerToken).filter(WorkerToken.server_id == server.id)
        .order_by(WorkerToken.created_at.desc())]
    return {"online": connection is not None,
            "worker": connection.client() if connection else None,
            "tokens": tokens_out,
            "command": _worker_command(server), "args": _worker_args(server)}


def _worker_args(server) -> str:
    """The worker's arguments on their own, for the page to put after whichever helper file was downloaded."""
    from .. import settings as cfg
    return f"--server {cfg.get().public_origin} --token YOUR_TOKEN --name \"{server.name}\""


def _worker_command(server) -> str:
    """The exact command the machine's owner should run to connect it (with CITAR installed there)."""
    return "python -m citar.worker " + _worker_args(server)


class TokenBody(BaseModel):
    """A request for a new worker token."""
    label: str = ""


@router.post("/servers/{server_id}/workers/tokens")
def create_worker_token(server_id: str, body: TokenBody, request: Request,
                        p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Issue a worker token. The value is returned ONCE and only its hash is stored."""
    from ..auth import tokens as token_util
    from ..db.models import WorkerToken

    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    raw, hashed = token_util.new_pair()
    row = WorkerToken(server_id=server.id, token_hash=hashed, prefix=token_util.prefix(raw),
                      label=(body.label or "")[:80], created_by=me.id)
    s.add(row)
    s.flush()
    audit.record(s, "worker.token_issued", actor=me, object_type="server", object_id=server.id,
                 label=row.label)
    # the helper may connect with it the moment it is shown
    _saved(s)
    return {
        "id": row.id, "token": raw, "prefix": row.prefix,
        "command": _worker_command(server).replace("YOUR_TOKEN", raw),
        "args": _worker_args(server).replace("YOUR_TOKEN", raw),
        "warning": "This is the only time the token is shown. Store it now.",
    }


@router.delete("/servers/{server_id}/workers/tokens/{token_id}")
def revoke_worker_token(server_id: str, token_id: str, request: Request,
                        p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """Revoke a worker token, disconnecting anything using it."""
    from datetime import datetime, timezone

    from ..db.models import WorkerToken

    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    row = s.get(WorkerToken, token_id)
    if row is None or row.server_id != server.id:
        raise HTTPException(404, "No such worker token.")
    row.revoked_at = datetime.now(timezone.utc)
    audit.record(s, "worker.token_revoked", actor=me, object_type="server", object_id=server.id)
    _drop(s, server.id, "its worker token was revoked", token_id=token_id)
    return {"revoked": token_id}


#: How many tests of machines one account may have waiting for their reply at once, over all machines: each holds a
#: request (and one of the server's worker threads) until its reply or its ``wait`` (at most 120 s). On one machine an
#: account has one at a time (the scheduler's ``one_per_consumer``).
TESTS_PER_ACCOUNT = 2
_tests_lock = threading.Lock()
_tests_waiting: dict = {}         # user id -> tests of theirs waiting now


def _test_begins(user_id: str) -> bool:
    """Count a test of ``user_id``'s as waiting, unless they have ``TESTS_PER_ACCOUNT`` waiting already."""
    with _tests_lock:
        n = _tests_waiting.get(user_id, 0)
        if n >= TESTS_PER_ACCOUNT:
            return False
        _tests_waiting[user_id] = n + 1
        return True


def _test_ends(user_id: str) -> None:
    """A test of ``user_id``'s has its answer (or gave up)."""
    with _tests_lock:
        n = _tests_waiting.get(user_id, 0) - 1
        if n > 0:
            _tests_waiting[user_id] = n
        else:
            _tests_waiting.pop(user_id, None)


@router.post("/servers/{server_id}/test")
def test_server(server_id: str, request: Request, model: str = "",
                wait: float = Query(120.0, ge=1.0, le=120.0), p: Principal = Depends(principal),
                s: Session = Depends(get_db, scope="function")):
    """Send a trivial completion to a server and report what came back.

    The "is this thing actually working" button. Checking that a worker is *connected* is not the
    same as checking that the model behind it will answer — a worker can be online with LM Studio
    closed, or with the named model not loaded — and the difference is worth one round trip before
    somebody schedules a six-hour benchmark against it.

    The completion is a ``test`` ticket of the prompt scheduler (design/phase3-features.md P3.3.3): a pooled prompt,
    admitted as every one is (P3.3.7: ``partial_llm`` and a grant for ``pooled``, its concurrency the caller's cap), it
    waits for a slot like any pooled prompt, on a model the helper has loaded (unless its owner allows swaps), and goes
    through ``make_conversation`` like every model call. ``wait`` is how long the caller waits for the reply (1 to 120
    seconds): then the ticket is cancelled, and a request the helper is still generating keeps its slot draining
    until the helper's late answer. A caller has one test of a machine at a time and ``TESTS_PER_ACCOUNT`` in all
    (another answers at once, ``code`` ``duplicate`` or ``too_many``), at most ``server_test``'s rate
    (``citar.auth.ratelimit``; 429 past it), and the request's database connection is given back before the wait.
    The test is an activity of its own in the compute ledger (``test:<id>``, design/phase3-features.md P3.4.2): used
    by the caller, given by the machine's owner, counted against the grant's budget when it has one; it has no usage
    record, so no cost report lists it.
    """
    import time as _time

    from ..agents.providers import make_conversation
    from ..auth import ratelimit
    from ..pool import scheduler as prompt_scheduler
    from .workers import hub

    me = require_user(request, p)
    server = _server(s, server_id, me, access.VIEW)
    try:
        ratelimit.check("server_test", me.id)
    except ratelimit.RateLimited as exc:
        raise HTTPException(429, str(exc), headers={"Retry-After": str(exc.retry_after)}) from None
    from ..auth import policy
    if not policy.can(me, "partial_llm"):
        return {"ok": False, "stage": "admission", "reason": policy.denial(me, "partial_llm"), "code": "capability"}
    decision = admission.check(s, me, server, "pooled", online=_liveness(server.id), in_flight=None)
    if not decision.allowed:
        return {"ok": False, "stage": "admission", "reason": decision.reason, "code": decision.code}

    connection = hub().get(server.id)
    if connection is None:
        return {"ok": False, "stage": "connection",
                "reason": "No worker is connected for this server." if server.reach == "worker" else
                "The server dials this machine itself; this test is for a machine's CITAR helper."}

    # Embedding models cannot answer a chat completion; skip them so the default pick is something that can
    # actually reply, and a loaded one first: a pooled prompt never makes the helper load a model it was not asked to.
    chat = [m for m in connection.models if m.get("key") and "embed" not in m["key"].lower()]
    chosen = model or next((m["key"] for m in chat if m.get("loaded")), None) or next((m["key"] for m in chat), None)
    if not chosen:
        return {"ok": False, "stage": "model",
                "reason": "The worker reported no chat-capable models."}

    if not _test_begins(me.id):
        return {"ok": False, "stage": "queue", "model": chosen, "code": "too_many",
                "reason": f"You have {TESTS_PER_ACCOUNT} machine tests waiting already; try again once one has "
                          "answered."}
    grant = decision.grant
    ticket, timer = None, None
    try:
        ticket = prompt_scheduler.get().ticket(
            server.id, chosen, cls="test", activity=f"test:{secrets.token_hex(6)}", consumer=me.id,
            label=f"test of {server.name}", start_within=wait, steps=1,
            cap=int(grant.concurrency or 0) if grant is not None else 0, one_per_consumer=True,
            access_grant=budgets.keeper().bind(s, grant))
        if ticket.state != "queued":
            return {"ok": False, "stage": "queue", "model": chosen, "reason": ticket.reason,
                    "code": ticket.error.code if ticket.error else None,
                    "retry_at": ticket.retry_at.isoformat() if ticket.retry_at else None}
        # nothing is written before the reply (the audit row comes after it): the request's database connection goes
        # back to the pool for the wait, which may last minutes
        s.commit()
        # Enough headroom that a reasoning model still has budget left for a visible answer after its thinking: a
        # 24-token cap produced 21 reasoning tokens and an empty reply, which looks like a broken server rather than
        # a too-small budget.
        conv = make_conversation({"provider": "worker", "server_id": server.id, "model": chosen, "max_tokens": 256,
                                  "temperature": 0, "timeout": 120}, "", [], binding=ticket)
        conv.messages.clear()                   # the request is the user's line alone, no system prompt
        conv.add_user_text("Reply with exactly: CITAR OK")
        conv.request_timeout = 120
        timer = threading.Timer(wait, ticket.cancel, kwargs={"reason": f"given up after {wait:.0f} s"})
        timer.daemon = True
        started = _time.time()
        timer.start()
        try:
            step = conv.step()
        except prompt_scheduler.SchedulerError as exc:
            return {"ok": False, "stage": "queue", "model": chosen, "reason": str(exc), "code": exc.code,
                    "cancelled": ticket.state == "cancelled"}
        except Exception as exc:
            if ticket.state == "cancelled":
                code = ticket.error.code if ticket.error is not None else "cancelled"
                why = ("a benchmark took the machine" if code == "benchmark" else
                       f"no reply within {wait:.0f} s")
                return {"ok": False, "stage": "completion", "model": chosen, "cancelled": True, "code": code,
                        "reason": f"The test was cancelled: {why} (a helper that is still generating it keeps its "
                                  "slot until it finishes)."}
            cause = exc.__cause__
            return {"ok": False, "stage": "completion", "model": chosen, "reason": str(exc),
                    "refusal": getattr(cause, "refusal", "") or getattr(exc, "refusal", ""),
                    "retryable": bool(getattr(cause, "retryable", False))}
    finally:
        if timer is not None:
            timer.cancel()
        if ticket is not None:
            ticket.close()
        _test_ends(me.id)

    elapsed = round(_time.time() - started, 2)
    audit.record(s, "server.tested", actor=me, object_type="server", object_id=server.id,
                 model=chosen, seconds=elapsed)
    return {"ok": True, "model": chosen, "seconds": elapsed, "gen_s": round(conv.last_gen_s, 3),
            "queue_s": round(conv.last_queue_s, 3),
            "reply": (step.text or "").strip()[:400],
            "usage": dict(conv.usage),
            "worker": {"hostname": connection.hostname, "provider": connection.provider}}


# ---------------------------------------------------------------------------- availability

@router.get("/availability")
def availability(request: Request, purpose: str = "game", p: Principal = Depends(principal),
                 s: Session = Depends(get_db, scope="function")):
    """What this account could run right now, and when the rest becomes available.

    This is what the lobby asks before offering model seats. Only machines the account may see are named, usable or
    not: another member's private machine, or one of the site's nobody shared, is 404 to it anywhere else (P3.9.1).
    """
    me = require_user(request, p)
    now = datetime.now(timezone.utc)
    usable, blocked = [], []
    for server, decision in admission.usable_servers(s, me, purpose, now=now,
                                                     liveness=_liveness,
                                                     only=lambda row: pool.visible(s, me, row)):
        entry = {"id": server.id, "name": server.name, "provider": server.provider,
                 "models": [m.get("key") for m in (server.config or {}).get("models") or []],
                 **decision.client()}
        (usable if decision.allowed else blocked).append(entry)
    return {"purpose": purpose, "usable": usable, "blocked": blocked,
            "viewer_tz": me.tz, "now": now.isoformat()}
