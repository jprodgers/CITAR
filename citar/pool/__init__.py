"""The shared server pool: who owns which machine, who may use it, and on what terms.

    windows     when a grant may be used, in the owner's wall clock
    budgets     spend caps, reservations and settlement
    admission   may this person run this work on that server, right now
    (here)      servers, groups and grants — ownership and the CRUD around it

Every machine is a row (design/phase3-features.md P3.2.8): the host, the site's online APIs and the machines on the
LAN an administrator registers (``reach = direct``: CITAR dials them), and the members' machines behind the CITAR
helper (``reach = worker``). ``config/servers.json``, the registry of CITAR before 0.1.6, is imported once under its
own ids (``import_registry``, run by ``citar.pool.registry_import``), and the rich per-server configuration —
hardware, power draw, component depreciation, cost periods, model catalog, load profiles — is kept whole in
`Server.config` rather than normalized, because `citar/servers.py` already validates, prices and renders exactly that
shape. Splitting it into columns would mean rewriting 1,200 lines of working code to enable queries nobody needs.

Who may make a machine CITAR dials: an administrator, or anyone for the dry-run provider, which dials nothing
(``check_reach``). A direct machine is a URL the server requests; a member naming one could make the server reach
addresses on its own network.
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


class ReachRefused(PoolError):
    """A member asked for what only an administrator may give a machine: a machine CITAR would dial (``reach =
    direct``) with a provider that dials something, the ``env`` key backend, or the site's electricity plans
    (P3.2.8). The routes answer 403 with the reason."""


def _site_admin(actor: Optional[User]) -> bool:
    """An administrator in good standing (probation included, as ``access.on`` reads the role)."""
    return actor is not None and actor.role == "admin" and actor.status in ("active", "probation")


def check_reach(actor: Optional[User], reach: str, provider: str) -> None:
    """Refuse a machine CITAR dials unless ``actor`` is an administrator in good standing or its provider is the dry
    run, which dials nothing. A helper machine (``worker``) is anybody's to register: the helper dials out to us."""
    if reach != "direct" or (provider or "").lower() == "dryrun":
        return
    if _site_admin(actor):
        return
    raise ReachRefused("Only an administrator can add a machine the CITAR server connects to itself: it would make "
                       "the server request an address on its own network. Register it as a helper machine (run the "
                       "CITAR helper on it), or use the dry-run provider.")


def check_key_backend(actor: Optional[User], normalized: dict) -> None:
    """Refuse the ``env`` key backend unless ``actor`` is an administrator: it names one of the server's own
    environment variables, whose value the server would send as the key (and whose last characters the Servers
    page would show as its hint)."""
    backend = ((normalized.get("connection") or {}).get("key") or {}).get("backend")
    if backend != "env":
        return
    if _site_admin(actor):
        return
    raise ReachRefused("Only an administrator can point a machine's key at one of the server's environment "
                       "variables. Store the key itself instead.")


def _plan_ids(costs) -> set:
    """The electricity plans a machine's cost periods name."""
    return {p.get("electricity_plan_id") for p in costs if isinstance(p, dict)
            and isinstance(p.get("electricity_plan_id"), str) and p.get("electricity_plan_id")} if isinstance(
        costs, list) else set()


def check_plans(actor: Optional[User], normalized: dict, stored: Optional[dict] = None) -> None:
    """Refuse a member naming one of the site's electricity plans in a machine's costs: the plans are the household's
    bill (an administrator's, ``/api/electricity-plans``), and a machine priced with one would tell its owner the
    tariff (its rates, its reports). A plan the row already names (``stored``, its stored config: an administrator
    put it there) stays."""
    if _site_admin(actor):
        return
    added = _plan_ids(normalized.get("costs")) - _plan_ids((stored or {}).get("costs"))
    if added:
        raise ReachRefused("Only an administrator can price a machine with the site's electricity plans. Leave the "
                           "plan out of your machine's costs.")


# ---------------------------------------------------------------------------- groups

def create_group(session, owner: User, *, name: str, description: str = "",
                 visibility: str = "private") -> ServerGroup:
    """Create a server group. Its visibility is on the ladder, and `global` needs `publish_global`, as a
    change of it does (``access.set_visibility``)."""
    name = (name or "").strip()[:120]
    if not name:
        raise PoolError("Give the group a name.")
    access.check_publish(owner, visibility)
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
    check_reach(owner, reach, (normalized.get("connection") or {}).get("provider") or "none")
    check_key_backend(owner, normalized)
    check_plans(owner, normalized)
    if group_id:
        group = session.get(ServerGroup, group_id)
        if group is None or group.owner_id != owner.id:
            raise PoolError("That server group is not yours.")

    server = Server(owner_id=owner.id, group_id=group_id, reach=reach)
    registry.apply_row(server, normalized)
    session.add(server)
    session.flush()
    server.config = {**(server.config or {}), "id": server.id}
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
    new_reach = reach if reach in ("worker", "direct") else (server.reach or "worker")
    check_reach(actor, new_reach, (normalized.get("connection") or {}).get("provider") or "none")
    check_key_backend(actor, normalized)
    check_plans(actor, normalized, server.config)
    server.reach = new_reach                    # first: a helper machine's row keeps what its view hides (apply_row)
    registry.apply_row(server, normalized)
    if group_id != "__keep__":
        if group_id:
            group = session.get(ServerGroup, group_id)
            if group is None or group.owner_id != server.owner_id:
                raise PoolError("That server group is not yours.")
        server.group_id = group_id or None
    audit.record(session, "server.updated", actor=actor, object_type="server", object_id=server.id)
    return server


def delete_server(session, actor: User, server: Server) -> dict:
    """Remove a machine from the pool, revoking its tokens. Returns the configuration it stored, for the caller to
    remove its key with (``servers.forget_key``) once this is committed."""
    access.on(session, actor, server).require(access.MANAGE)
    config = dict(server.config or {})
    audit.record(session, "server.deleted", actor=actor, object_type="server", object_id=server.id,
                 name=server.name)
    session.delete(server)
    return config


def servers_for(session, user: Optional[User], *, owned_only: bool = False) -> list:
    """The machines a viewer may use, through ownership or a grant."""
    if user is None:
        return []
    if owned_only:
        # the site's machines are administrators' whoever is named their owner (access.is_site_machine)
        return [s for s in session.scalars(select(Server).where(Server.owner_id == user.id).order_by(Server.name))
                if user.role == "admin" or not access.is_site_machine(s)]
    if user.role == "admin":
        return list(session.scalars(select(Server).order_by(Server.name)))
    group_ids = {g.id for g in groups_for(session, user)}
    return [s for s in session.scalars(select(Server).order_by(Server.name))
            if (s.owner_id == user.id and not access.is_site_machine(s)) or (s.group_id and s.group_id in group_ids)]


def visible(session, viewer: Optional[User], server: Server) -> bool:
    """Whether ``viewer`` may see ``server`` at all (VIEW: its owner, administrators, anyone its group reaches). Every
    listing that names machines asks this, so another member's private machine, or one of the site's nobody shared,
    is never named to someone who would get 404 for it (P3.9.1)."""
    return viewer is not None and access.VIEW in access.on(session, viewer, server)


def visible_restrictions(session, viewer: Optional[User]) -> list:
    """The machines in their quiet hours right now (``servers.restriction_status``) that ``viewer`` may see: the
    Servers page's header, the benchmark scheduler's status and the phone site's overview all show this list."""
    from .. import servers as registry
    out = []
    for entry in registry.restriction_status():
        row = session.get(Server, entry["id"])
        if row is not None and visible(session, viewer, row):
            out.append(entry)
    return out


def machine_rates(sv: dict, reg: dict, viewer: Optional[User]) -> dict:
    """What an hour on a machine (its registry dict ``sv``) costs, idle and with the model busy. For anyone but an
    administrator the energy goes unpriced when the machine's costs name one of the site's electricity plans (an
    administrator put it there, ``check_plans``): its price per kWh is the household's tariff, which the energy's cost
    and the total would tell."""
    from .. import costing
    if not _site_admin(viewer) and any(c.get("electricity_plan_id") for c in sv.get("costs") or []):
        sv = dict(sv, costs=[dict(c, electricity_plan_id=None) for c in sv["costs"]])
    return {"idle": costing.hourly_profile(sv, reg, busy_fraction=0.0),
            "busy": costing.hourly_profile(sv, reg, busy_fraction=1.0)}


def public_server(session, server: Server, viewer: Optional[User]) -> dict:
    """What a viewer is allowed to know about a server.

    Being permitted to run a model on somebody's machine is not being permitted to read its API key
    reference, its electricity tariff or what it cost them. Non-owners get identity, capability and
    availability; owners and administrators get the whole configuration.
    """
    from .. import servers as registry

    perms = access.on(session, viewer, server)
    owner = session.get(User, server.owner_id) if server.owner_id else None
    base = {
        "id": server.id,
        "name": server.name,
        "kind": server.kind,
        "provider": server.provider,
        "reach": server.reach,
        "enabled": server.enabled,
        "max_concurrent": server.max_concurrent,
        "group_id": server.group_id,
        "owner": owner.public(viewer) if owner else None,
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
    # quiet hours are public: anyone who may use the machine needs to know when it is off limits. They are read on
    # the clock that pauses its work (servers.restricted_at): a helper machine's owner's wall clock, this computer's
    # for a machine the server dials, so the listing agrees with the scheduler and /api/servers whatever the
    # owner's zone.
    from . import seats
    base["owner_tz"] = (owner.tz if owner else None) or "UTC"
    base["restricted_hours"] = config.get("restricted_hours") or registry.default_restricted()
    if server.reach == "worker":
        view = {"config": config, "owner_tz": base["owner_tz"]}
        base["restricted_until"] = seats.clock_text(view, seats.restricted(view))
    else:
        end = registry.restricted({"restricted_hours": base["restricted_hours"]})
        base["restricted_until"] = end.strftime("%H:%M") if end else None
    if access.MANAGE in perms:
        try:
            # the registry's view of the row as this request has it (its id, its slots, a helper machine's
            # connection left to the helper), committed or not
            config = registry.row_registry(server)
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
    check_plans(actor, normalized, server.config)
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
        "subject": subject.public(viewer) if subject else None,
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


# ---------------------------------------------------------------------------- the registry's import

def import_registry(session, owner: Optional[User], *, registry_data: dict) -> dict:
    """Bring a registry (``config/servers.json``'s dict, or ``servers.bootstrap()``'s on a first run) into the
    database: every server a row **under its own id** (so seats, suites, saved games, the usage ledger and the keystore
    entries keep resolving), in no group (sharing one is its owner's later choice), owned by ``owner`` (the first
    administrator; None leaves them to administrators), dialled by CITAR (``reach = direct``), the registry's host
    the row with ``is_host``, and the host and online APIs ``server_resource`` (the site provides them). Each row's
    slots are the entry's ``connection.max_parallel``. The electricity plans join ``settings_kv`` (by id: a plan
    already there is kept), and the currency is set when none is. Idempotent: an id that is already a row is left
    alone. Writes in ``session``; the caller commits. ``citar.pool.registry_import`` runs it once, behind its marker.

    A database where the pool's old import route was used (retired in 0.1.6) holds copies of the registry's machines
    under new ids, each with the registry's id as its config's ``id``. The machines still come in under their own
    ids, which is what seats, suites, the usage ledger and the keystore name (and which the copies never answered
    to); the copies are listed in ``earlier_copies`` ({registry id: [row ids]}) for an administrator to remove, and
    left alone, since whatever names them still needs them.
    """
    from datetime import timedelta

    from .. import servers as registry
    from ..db.models import Setting, now as _now

    rows = list(session.scalars(select(Server)))
    existing = {r.id for r in rows}
    copies: dict = {}
    for r in rows:
        cid = (r.config or {}).get("id") if isinstance(r.config, dict) else None
        if isinstance(cid, str) and cid != r.id:
            copies.setdefault(cid, []).append(r.id)
    host_taken = session.scalar(select(Server.id).where(Server.is_host.is_(True))) is not None
    host_id = registry_data.get("host_server_id")
    created, skipped = [], []
    stamp = _now()
    for i, entry in enumerate(registry_data.get("servers") or []):
        if not isinstance(entry, dict):
            continue
        try:
            sv = registry.normalize_server(entry)
        except (registry.ServerError, ValueError, TypeError) as exc:
            raise PoolError(f"Server {entry.get('id')!r} in the registry does not read: {exc}") from exc
        sv["updated"] = entry.get("updated") or sv["updated"]
        if sv["id"] in existing:
            skipped.append(sv["id"])
            continue
        is_host = sv["id"] == host_id and not host_taken
        row = Server(id=sv["id"], owner_id=owner.id if owner else None, group_id=None, reach="direct",
                     is_host=is_host, server_resource=is_host or sv["kind"] == "api",
                     created_at=stamp + timedelta(microseconds=i))
        registry.apply_row(row, sv)
        session.add(row)
        existing.add(sv["id"])
        host_taken = host_taken or is_host
        created.append(sv["id"])
    session.flush()

    plans_row = session.get(Setting, registry.PLANS_KEY)
    plans = list(((plans_row.value or {}).get("v") if plans_row else None) or [])
    known = {p.get("id") for p in plans if isinstance(p, dict)}
    added_plans = [registry.normalize_plan(p) for p in registry_data.get("electricity_plans") or []
                   if isinstance(p, dict) and p.get("id") not in known]
    if added_plans:
        registry.save_plans(plans + added_plans, session=session)
    currency_row = session.get(Setting, registry.CURRENCY_KEY)
    if currency_row is None or not (currency_row.value or {}).get("v"):
        registry.save_currency(registry_data.get("currency") or "USD", registry_data.get("currency_symbol") or "$",
                               session=session)
    session.flush()
    return {"imported": created, "already_present": skipped, "electricity_plans": [p["id"] for p in added_plans],
            "host_server_id": next((sid for sid in created if sid == host_id), None),
            "earlier_copies": {sid: sorted(copies[sid]) for sid in created if sid in copies}}
