"""The shared server pool: who owns which machine, who may use it, and on what terms.

    windows     when a grant may be used, in the owner's wall clock
    budgets     spend caps, reservations and settlement
    admission   may this person run this work on that server, right now
    (here)      servers, groups and grants — ownership and the CRUD around it

`config/servers.json` becomes a per-owner table. The file stays supported as an import and export
format, and the rich per-server configuration — hardware, power draw, component depreciation, cost
periods, model catalog, load profiles — is kept whole in `Server.config` rather than normalized,
because `citar/servers.py` already validates, prices and renders exactly that shape. Splitting it
into columns would mean rewriting 1,200 lines of working code to enable queries nobody needs.
"""
from __future__ import annotations

import logging
from datetime import datetime
from typing import Optional

from sqlalchemy import select

from ..auth import access, audit
from ..db.models import AccessGrant, AvailabilityWindow, Server, ServerGroup, User
from . import admission, budgets, windows

log = logging.getLogger("citar.pool")

__all__ = ["admission", "budgets", "windows", "PoolError"]


class PoolError(ValueError):
    """A problem the person can fix. Safe to show."""


# ---------------------------------------------------------------------------- groups

def create_group(session, owner: User, *, name: str, description: str = "",
                 visibility: str = "private") -> ServerGroup:
    """Create a server group."""
    name = (name or "").strip()[:120]
    if not name:
        raise PoolError("Give the group a name.")
    if visibility not in ("private", "allowlist", "link", "public"):
        raise PoolError("Visibility is private, allowlist, link or public.")
    group = ServerGroup(owner_id=owner.id, name=name, description=(description or "").strip(),
                        visibility=visibility)
    session.add(group)
    session.flush()
    audit.record(session, "server_group.created", actor=owner, object_type="server_group",
                 object_id=group.id, name=name)
    return group


def groups_for(session, user: Optional[User], *, owned_only: bool = False) -> list:
    """Groups this user owns, plus any they have been granted access to."""
    if user is None:
        return []
    owned = list(session.scalars(select(ServerGroup).where(
        ServerGroup.owner_id == user.id, ServerGroup.archived_at.is_(None))))
    if owned_only:
        return owned
    granted_ids = {g.group_id for g in session.scalars(select(AccessGrant).where(
        AccessGrant.enabled.is_(True),
        ((AccessGrant.subject_type == "everyone")
         | ((AccessGrant.subject_type == "user") & (AccessGrant.subject_user_id == user.id)))))}
    others = [g for g in session.scalars(select(ServerGroup).where(
        ServerGroup.id.in_(granted_ids or {""}), ServerGroup.archived_at.is_(None)))
        if g.owner_id != user.id]
    return owned + others


# ---------------------------------------------------------------------------- servers

def create_server(session, owner: User, config: dict, *, group_id: Optional[str] = None,
                  reach: str = "worker") -> Server:
    """Register a machine. `config` is the registry dict, validated by citar/servers.py."""
    from .. import servers as registry

    try:
        normalized = registry.normalize_server(dict(config or {}))
    except (registry.ServerError, ValueError) as exc:
        raise PoolError(str(exc)) from exc

    if reach not in ("worker", "direct"):
        raise PoolError("reach is 'worker' or 'direct'.")
    if group_id:
        group = session.get(ServerGroup, group_id)
        if group is None or group.owner_id != owner.id:
            raise PoolError("That server group is not yours.")

    server = Server(
        owner_id=owner.id, group_id=group_id,
        name=(normalized.get("name") or "Unnamed server")[:120],
        kind=normalized.get("kind") or "owned",
        provider=(normalized.get("connection") or {}).get("provider") or "none",
        config=normalized, reach=reach,
        max_concurrent=max(1, int((normalized.get("connection") or {}).get("max_parallel") or 1)))
    session.add(server)
    session.flush()
    audit.record(session, "server.created", actor=owner, object_type="server", object_id=server.id,
                 name=server.name, kind=server.kind, provider=server.provider, reach=reach)
    return server


def update_server(session, actor: User, server: Server, config: dict, *,
                  group_id: Optional[str] = "__keep__", reach: Optional[str] = None) -> Server:
    """Change a registered machine's details."""
    from .. import servers as registry

    access.on(session, actor, server).require(access.MANAGE)
    try:
        normalized = registry.normalize_server(dict(config or {}))
    except (registry.ServerError, ValueError) as exc:
        raise PoolError(str(exc)) from exc

    # The config dict carries an id of its own; the row's primary key is authoritative.
    normalized["id"] = server.id
    server.config = normalized
    server.name = (normalized.get("name") or server.name)[:120]
    server.kind = normalized.get("kind") or server.kind
    server.provider = (normalized.get("connection") or {}).get("provider") or server.provider
    server.max_concurrent = max(1, int((normalized.get("connection") or {}).get("max_parallel") or 1))
    if reach in ("worker", "direct"):
        server.reach = reach
    if group_id != "__keep__":
        if group_id:
            group = session.get(ServerGroup, group_id)
            if group is None or group.owner_id != server.owner_id:
                raise PoolError("That server group is not yours.")
        server.group_id = group_id or None
    audit.record(session, "server.updated", actor=actor, object_type="server", object_id=server.id)
    return server


def delete_server(session, actor: User, server: Server) -> None:
    """Remove a machine from the pool, revoking its tokens."""
    access.on(session, actor, server).require(access.MANAGE)
    audit.record(session, "server.deleted", actor=actor, object_type="server", object_id=server.id,
                 name=server.name)
    session.delete(server)


def servers_for(session, user: Optional[User], *, owned_only: bool = False) -> list:
    """The machines a viewer may use, through ownership or a grant."""
    if user is None:
        return []
    if owned_only:
        return list(session.scalars(select(Server).where(Server.owner_id == user.id)
                                    .order_by(Server.name)))
    if user.role == "admin":
        return list(session.scalars(select(Server).order_by(Server.name)))
    group_ids = {g.id for g in groups_for(session, user)}
    return [s for s in session.scalars(select(Server).order_by(Server.name))
            if s.owner_id == user.id or (s.group_id and s.group_id in group_ids)]


def public_server(session, server: Server, viewer: Optional[User]) -> dict:
    """What a viewer is allowed to know about a server.

    Being permitted to run a model on somebody's machine is not being permitted to read its API key
    reference, its electricity tariff or what it cost them. Non-owners get identity, capability and
    availability; owners and administrators get the whole configuration.
    """
    from .. import servers as registry

    perms = access.on(session, viewer, server)
    owner = session.get(User, server.owner_id)
    base = {
        "id": server.id,
        "name": server.name,
        "kind": server.kind,
        "provider": server.provider,
        "reach": server.reach,
        "enabled": server.enabled,
        "max_concurrent": server.max_concurrent,
        "group_id": server.group_id,
        "owner": owner.public() if owner else None,
        "is_mine": bool(viewer and server.owner_id == viewer.id),
        "can_manage": access.MANAGE in perms,
    }
    config = server.config or {}
    # Safe for anyone who can see the server at all: what it can run, and roughly what it is.
    base["models"] = [{"key": m.get("key"), "label": m.get("label") or m.get("key")}
                      for m in (config.get("models") or [])]
    hardware = config.get("hardware") or {}
    base["hardware_summary"] = {
        "cpu": (hardware.get("cpu") or {}).get("model"),
        "gpus": [g.get("name") for g in (hardware.get("gpus") or [])],
        "ram_gb": (hardware.get("memory") or {}).get("ram_gb"),
    }
    # quiet hours are public: anyone who may use the machine needs to know when it is off limits
    from . import seats
    base["owner_tz"] = (owner.tz if owner else None) or "UTC"
    base["restricted_hours"] = config.get("restricted_hours") or registry.default_restricted()
    view = {"config": config, "owner_tz": base["owner_tz"]}
    base["restricted_until"] = seats.clock_text(view, seats.restricted(view))
    if access.MANAGE in perms:
        try:
            base["config"] = registry.public(config)
            base["config"]["restricted_until"] = base["restricted_until"]
        except Exception:
            # Stored configs can predate a registry change; showing the raw dict to its owner beats
            # failing the whole request over a missing optional field.
            log.debug("registry.public() failed for server %s", server.id, exc_info=True)
            base["config"] = config
    return base


def set_costing(session, actor: User, server: Server, body: dict) -> dict:
    """Change a machine's power figures, components (for depreciation) and cost periods, leaving the rest alone.

    Cost periods name electricity plans from the registry (Models page → Electricity plans)."""
    from .. import servers as registry

    access.on(session, actor, server).require(access.MANAGE)
    config = dict(server.config or {})
    for key in ("power", "components", "costs"):
        if body.get(key) is not None:
            config[key] = body[key]
    try:
        normalized = registry.normalize_server({**config, "id": server.id, "name": server.name, "kind": server.kind})
    except (registry.ServerError, ValueError, TypeError) as exc:
        raise PoolError(str(exc)) from exc
    plans = {p["id"] for p in registry.load()["electricity_plans"]}
    for period in normalized["costs"]:
        if period.get("electricity_plan_id") and period["electricity_plan_id"] not in plans:
            raise PoolError(f"Unknown electricity plan '{period['electricity_plan_id']}'.")
    for key in ("power", "components", "costs"):
        config[key] = normalized[key]
    if body.get("former_ids") is not None:
        config["former_ids"] = [str(x)[:40] for x in body["former_ids"] if x and str(x) != server.id][:10]
    server.config = config
    audit.record(session, "server.costing", actor=actor, object_type="server", object_id=server.id)
    return {k: config.get(k) for k in ("power", "components", "costs", "former_ids")}


def set_quiet_hours(session, actor: User, server: Server, restricted_hours: dict) -> dict:
    """Change only a machine's quiet hours (restricted hours), leaving the rest of its configuration alone."""
    from .. import servers as registry

    access.on(session, actor, server).require(access.MANAGE)
    config = dict(server.config or {})
    try:
        rh = registry.normalize_server({"id": server.id, "name": server.name, "kind": server.kind,
                                        "restricted_hours": restricted_hours or {}})["restricted_hours"]
    except (registry.ServerError, ValueError) as exc:
        raise PoolError(str(exc)) from exc
    config["restricted_hours"] = rh
    server.config = config
    audit.record(session, "server.quiet_hours", actor=actor, object_type="server", object_id=server.id,
                 enabled=rh["enabled"], windows=len(rh["windows"]))
    return rh


# ---------------------------------------------------------------------------- grants

def create_grant(session, actor: User, group: ServerGroup, *, subject_type: str = "user",
                 subject_handle: Optional[str] = None, purposes: Optional[list] = None,
                 concurrency: int = 0, priority: int = 0, expires_at: Optional[datetime] = None,
                 note: str = "", window_rows: Optional[list] = None,
                 budget: Optional[dict] = None) -> AccessGrant:
    """Give somebody — or everybody — permission to use a group, with conditions."""
    from ..auth import accounts

    if group.owner_id != actor.id and actor.role != "admin":
        raise PoolError("Only the owner of a server group can grant access to it.")
    if subject_type not in ("user", "everyone"):
        raise PoolError("subject_type is 'user' or 'everyone'.")

    subject_user_id = None
    if subject_type == "user":
        if not subject_handle:
            raise PoolError("Name the account to grant access to.")
        target = accounts.by_handle(session, subject_handle)
        if target is None or target.status in ("deleted", "suspended"):
            raise PoolError(f"No account called '{subject_handle}'.")
        subject_user_id = target.id

    for purpose in purposes or []:
        if purpose not in admission.PURPOSES:
            raise PoolError(f"Unknown purpose {purpose!r}.")

    grant = AccessGrant(
        group_id=group.id, subject_type=subject_type, subject_user_id=subject_user_id,
        purposes=list(purposes or []), concurrency=max(0, int(concurrency or 0)),
        priority=int(priority or 0), expires_at=expires_at,
        note=(note or "").strip()[:500] or None, created_by=actor.id)
    session.add(grant)
    session.flush()

    set_windows(session, grant, window_rows or [])
    if budget:
        budgets.set_budget(session, grant, period=budget.get("period", "month"),
                           amount=float(budget.get("amount") or 0),
                           currency=budget.get("currency", "USD"),
                           behavior=budget.get("behavior", "hard"))
    audit.record(session, "grant.created", actor=actor, object_type="server_group",
                 object_id=group.id, subject=subject_handle or "everyone",
                 purposes=purposes or ["all"], has_budget=bool(budget),
                 windows=len(window_rows or []))
    return grant


def set_windows(session, grant: AccessGrant, entries) -> list:
    """Replace a grant's availability windows, normalizing wrapping ones into storable rows."""
    for old in session.scalars(select(AvailabilityWindow)
                               .where(AvailabilityWindow.grant_id == grant.id)):
        session.delete(old)
    session.flush()
    rows = windows.normalize_many(entries or [])
    for weekday, start_min, end_min in rows:
        session.add(AvailabilityWindow(grant_id=grant.id, weekday=weekday,
                                       start_min=start_min, end_min=end_min))
    session.flush()
    return rows


def revoke_grant(session, actor: User, grant: AccessGrant) -> None:
    """Revoke a grant, ending the access it gave."""
    group = session.get(ServerGroup, grant.group_id)
    if group is not None and group.owner_id != actor.id and actor.role != "admin":
        raise PoolError("Only the owner of a server group can revoke access to it.")
    audit.record(session, "grant.revoked", actor=actor, object_type="server_group",
                 object_id=grant.group_id, subject=grant.subject_user_id or "everyone")
    session.delete(grant)


def grant_to_client(session, grant: AccessGrant, viewer: Optional[User]) -> dict:
    """A grant as the UI shows it, with both time-zone readings and the budget meter."""
    subject = session.get(User, grant.subject_user_id) if grant.subject_user_id else None
    tz = budgets.owner_tz(session, grant)
    window_rows = list(session.scalars(select(AvailabilityWindow)
                                       .where(AvailabilityWindow.grant_id == grant.id)))
    viewer_tz = (viewer.tz if viewer else None) or "UTC"
    state = windows.evaluate(window_rows, tz)
    return {
        "id": grant.id,
        "group_id": grant.group_id,
        "subject_type": grant.subject_type,
        "subject": subject.public() if subject else None,
        "purposes": grant.purposes or [],
        "purpose_label": (", ".join(admission.PURPOSE_LABELS.get(p, p) for p in grant.purposes)
                          if grant.purposes else "any kind of work"),
        "concurrency": grant.concurrency,
        "priority": grant.priority,
        "enabled": grant.enabled,
        "note": grant.note,
        "expires_at": grant.expires_at.isoformat() if grant.expires_at else None,
        "windows": windows.to_client(window_rows, tz),
        "availability": windows.describe_for_viewer(window_rows, tz, viewer_tz),
        "open_now": state.open,
        "budget": budgets.state(session, grant).client(),
    }


# ---------------------------------------------------------------------------- migration

def import_registry(session, owner: User, *, registry_data: Optional[dict] = None) -> dict:
    """Bring `config/servers.json` into the database as `owner`'s servers.

    Idempotent: a server already imported (matched on its registry id) is left alone, so running
    this twice does not duplicate anybody's hardware.
    """
    from .. import servers as registry

    data = registry_data or registry.load()
    existing = {s.config.get("id") for s in session.scalars(select(Server)) if s.config}
    group = session.scalar(select(ServerGroup).where(
        ServerGroup.owner_id == owner.id, ServerGroup.name == "Imported"))
    created, skipped = [], []

    for entry in data.get("servers") or []:
        if entry.get("id") in existing:
            skipped.append(entry.get("name"))
            continue
        if group is None:
            group = create_group(session, owner, name="Imported",
                                 description="Servers migrated from config/servers.json.")
        server = Server(
            owner_id=owner.id, group_id=group.id,
            name=(entry.get("name") or "Unnamed")[:120],
            kind=entry.get("kind") or "owned",
            provider=(entry.get("connection") or {}).get("provider") or "none",
            config=entry,
            # Everything in the old single-user registry was reached directly over the LAN. That is
            # still right for the CITAR host itself; anything else should move to a worker.
            reach="direct",
            max_concurrent=max(1, int((entry.get("connection") or {}).get("max_parallel") or 1)))
        session.add(server)
        created.append(entry.get("name"))

    session.flush()
    if created:
        audit.record(session, "pool.imported_registry", actor=owner, count=len(created),
                     servers=created)
    return {"imported": created, "already_present": skipped,
            "group_id": group.id if group else None}
