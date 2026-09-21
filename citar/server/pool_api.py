"""HTTP API for the shared server pool: servers, groups, grants, windows and budgets.

Separate from `admin_api.py`, which is the old single-tenant registry and stays administrator-only.
These routes are per-owner: an ordinary account with the `register_servers` capability can add its
own hardware, put it in a group and decide who may use it on what terms.
"""
from __future__ import annotations

import logging
from datetime import datetime, timezone
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
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
    """Turn an internal error into an HTTP error."""
    return HTTPException(status, str(exc))


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


def _load(server_id: str) -> Optional[int]:
    """How many activities currently hold this server."""
    try:
        from .workers import hub
        return hub().in_flight(server_id)
    except Exception:
        return None


# ---------------------------------------------------------------------------- servers

@router.get("/servers")
def list_servers(request: Request, purpose: str = "game", p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Every server this account can see, each with whether it may be used right now and why not.

    Refused servers are included with their reason rather than hidden: somebody should be able to
    see that the overnight box exists and that it opens in four hours, instead of wondering why a
    server they were told about is missing.
    """
    me = require_user(request, p)
    out = []
    for server in pool.servers_for(s, me):
        decision = admission.check(s, me, server, purpose,
                                   online=_liveness(server.id), in_flight=_load(server.id))
        entry = pool.public_server(s, server, me)
        entry["admission"] = decision.client()
        entry["online"] = _liveness(server.id)
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
                  s: Session = Depends(get_db)):
    """Register a machine, and issue its first worker token."""
    try:
        server = pool.create_server(s, me, body.config, group_id=body.group_id, reach=body.reach)
    except pool.PoolError as exc:
        raise _fail(exc)
    return pool.public_server(s, server, me)


@router.get("/servers/{server_id}")
def get_server(server_id: str, request: Request, p: Principal = Depends(principal),
               s: Session = Depends(get_db)):
    """One machine's details. Cost configuration is included only for its owner."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.VIEW)
    out = pool.public_server(s, server, me)
    out["admission"] = admission.check(s, me, server, online=_liveness(server_id),
                                       in_flight=_load(server_id)).client()
    return out


class ServerUpdate(BaseModel):
    """The editable fields of a registered machine."""
    config: Optional[dict] = None
    group_id: Optional[str] = "__keep__"
    reach: Optional[str] = None
    enabled: Optional[bool] = None


@router.put("/servers/{server_id}")
def update_server(server_id: str, body: ServerUpdate, request: Request,
                  p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Change a machine's details."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
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
    return pool.public_server(s, server, me)


@router.delete("/servers/{server_id}")
def delete_server(server_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db)):
    """Remove a machine from the pool."""
    me = require_user(request, p)
    server = _server(s, server_id, me, access.MANAGE)
    pool.delete_server(s, me, server)
    return {"deleted": server_id}


# ---------------------------------------------------------------------------- groups

@router.get("/groups")
def list_groups(request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The server groups the viewer can see."""
    me = require_user(request, p)
    out = []
    for group in pool.groups_for(s, me):
        perms = access.on(s, me, group)
        owner = s.get(User, group.owner_id)
        entry = {
            "id": group.id, "name": group.name, "description": group.description,
            "visibility": group.visibility, "owner": owner.public() if owner else None,
            "is_mine": group.owner_id == me.id, "can_manage": access.MANAGE in perms,
            "servers": [pool.public_server(s, sv, me)
                        for sv in s.query(Server).filter(Server.group_id == group.id)],
        }
        if access.MANAGE in perms:
            entry["grants"] = [pool.grant_to_client(s, g, me)
                               for g in s.query(AccessGrant).filter(AccessGrant.group_id == group.id)]
            entry["share_url"] = (f"/#/servers/groups/{group.id}?k={group.slug}"
                                  if group.visibility == "link" else None)
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
                 s: Session = Depends(get_db)):
    """Create a server group."""
    try:
        group = pool.create_group(s, me, name=body.name, description=body.description,
                                  visibility=body.visibility)
    except pool.PoolError as exc:
        raise _fail(exc)
    return {"id": group.id, "name": group.name, "visibility": group.visibility}


@router.put("/groups/{group_id}")
def update_group(group_id: str, body: GroupBody, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Change a group."""
    me = require_user(request, p)
    group = _group(s, group_id, me, access.MANAGE)
    group.name = (body.name or group.name).strip()[:120]
    group.description = (body.description or "").strip()
    if body.visibility != group.visibility:
        access.set_visibility(s, group, body.visibility, actor=me)
    return {"id": group.id, "name": group.name, "visibility": group.visibility}


@router.delete("/groups/{group_id}")
def delete_group(group_id: str, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Delete a group."""
    me = require_user(request, p)
    group = _group(s, group_id, me, access.MANAGE)
    # Servers outlive their group; losing a group should not silently delete the hardware
    # registered in it, only un-share it.
    for server in s.query(Server).filter(Server.group_id == group.id):
        server.group_id = None
    audit.record(s, "server_group.deleted", actor=me, object_type="server_group", object_id=group.id)
    s.delete(group)
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
    """A spending budget: an amount, a period, and what happens when it is reached."""
    period: str = "month"
    amount: float = 0.0
    currency: str = "USD"
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
                 p: Principal = Depends(principal), s: Session = Depends(get_db)):
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
    return pool.grant_to_client(s, grant, me)


@router.put("/grants/{grant_id}")
def update_grant(grant_id: str, body: GrantBody, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db)):
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
    return pool.grant_to_client(s, grant, me)


@router.delete("/grants/{grant_id}")
def revoke_grant(grant_id: str, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Revoke a grant."""
    me = require_user(request, p)
    grant = s.get(AccessGrant, grant_id)
    if grant is None:
        raise HTTPException(404, "No such grant.")
    try:
        pool.revoke_grant(s, me, grant)
    except pool.PoolError as exc:
        raise _fail(exc)
    return {"revoked": grant_id}


# ---------------------------------------------------------------------------- worker tokens

@router.get("/servers/{server_id}/workers")
def worker_status(server_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db)):
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
                        p: Principal = Depends(principal), s: Session = Depends(get_db)):
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
    return {
        "id": row.id, "token": raw, "prefix": row.prefix,
        "command": _worker_command(server).replace("YOUR_TOKEN", raw),
        "args": _worker_args(server).replace("YOUR_TOKEN", raw),
        "warning": "This is the only time the token is shown. Store it now.",
    }


@router.delete("/servers/{server_id}/workers/tokens/{token_id}")
def revoke_worker_token(server_id: str, token_id: str, request: Request,
                        p: Principal = Depends(principal), s: Session = Depends(get_db)):
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
    return {"revoked": token_id}


@router.post("/servers/{server_id}/test")
def test_server(server_id: str, request: Request, model: str = "", p: Principal = Depends(principal),
                s: Session = Depends(get_db)):
    """Send a trivial completion to a server and report what came back.

    The "is this thing actually working" button. Checking that a worker is *connected* is not the
    same as checking that the model behind it will answer — a worker can be online with LM Studio
    closed, or with the named model not loaded — and the difference is worth one round trip before
    somebody schedules a six-hour benchmark against it.
    """
    import time as _time

    from ..worker import protocol as P
    from .workers import WorkerError, hub

    me = require_user(request, p)
    server = _server(s, server_id, me, access.VIEW)
    decision = admission.check(s, me, server, "game",
                               online=_liveness(server.id), in_flight=_load(server.id))
    if not decision.allowed:
        return {"ok": False, "stage": "admission", "reason": decision.reason}

    connection = hub().get(server.id)
    if connection is None:
        return {"ok": False, "stage": "connection",
                "reason": "No worker is connected for this server."}

    chosen = model or next((m.get("key") for m in connection.models
                            # Embedding models cannot answer a chat completion; skip them so the
                            # default pick is something that can actually reply.
                            if m.get("key") and "embed" not in m["key"].lower()), None)
    if not chosen:
        return {"ok": False, "stage": "model",
                "reason": "The worker reported no chat-capable models."}

    started = _time.time()
    try:
        answer = hub().submit(server.id, P.Request(
            id=f"test-{me.id[:6]}-{int(started)}", model=chosen,
            messages=[{"role": "user", "content": "Reply with exactly: CITAR OK"}],
            # Enough headroom that a reasoning model still has budget left for a visible answer
            # after its thinking: a 24-token cap produced 21 reasoning tokens and an empty reply,
            # which looks like a broken server rather than a too-small budget.
            tools=[], tool_mode="native", params={"max_tokens": 256, "temperature": 0},
            timeout=120, activity="server-test"), timeout=120)
    except WorkerError as exc:
        return {"ok": False, "stage": "completion", "model": chosen, "reason": str(exc),
                "refusal": exc.refusal, "retryable": exc.retryable}

    elapsed = round(_time.time() - started, 2)
    audit.record(s, "server.tested", actor=me, object_type="server", object_id=server.id,
                 model=chosen, seconds=elapsed)
    return {"ok": True, "model": chosen, "seconds": elapsed,
            "reply": (answer.get("text") or "").strip()[:400],
            "usage": answer.get("usage") or {},
            "worker": {"hostname": connection.hostname, "provider": connection.provider}}


# ---------------------------------------------------------------------------- availability

@router.get("/availability")
def availability(request: Request, purpose: str = "game", p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """What this account could run right now, and when the rest becomes available.

    This is what the lobby asks before offering model seats.
    """
    me = require_user(request, p)
    now = datetime.now(timezone.utc)
    usable, blocked = [], []
    for server, decision in admission.usable_servers(s, me, purpose, now=now,
                                                     liveness=_liveness, load=_load):
        entry = {"id": server.id, "name": server.name, "provider": server.provider,
                 "models": [m.get("key") for m in (server.config or {}).get("models") or []],
                 **decision.client()}
        (usable if decision.allowed else blocked).append(entry)
    return {"purpose": purpose, "usable": usable, "blocked": blocked,
            "viewer_tz": me.tz, "now": now.isoformat()}


# ---------------------------------------------------------------------------- migration

@router.post("/import-registry")
def import_registry(request: Request, p: Principal = Depends(principal),
                    s: Session = Depends(get_db)):
    """Bring config/servers.json into the database as this account's servers. Idempotent."""
    me = require_user(request, p)
    if me.role != "admin":
        raise HTTPException(403, "Only an administrator can import the legacy server registry.")
    try:
        result = pool.import_registry(s, me)
    except Exception as exc:
        log.exception("registry import failed")
        raise _fail(exc)
    return result
