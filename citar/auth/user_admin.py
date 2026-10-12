"""Administering accounts: the admin users page and the CLI share this (design/phase3-features.md P3.2.7).

``apply`` is the one way an administrator changes an account: its role, status (a suspension with its reason),
capability overrides (``{cap: true | false | null}``, null removing the override), limits, trust tier and whether it
is listed. Everything asked is checked before anything changes, and the change is one audit row naming the fields,
each with its old and new value. One rule holds whoever asks, the web page or ``admin_cli``: the server never loses
its last active administrator (the last one cannot demote, suspend or put themselves on probation).

``search`` pages the accounts for the admin page, and ``audit_page`` the audit log (filters: actor, action, object
type and id, before an entry; at most ``AUDIT_PAGE_MAX`` a page).
"""
from __future__ import annotations

from datetime import datetime, timezone
from typing import Optional

from sqlalchemy import func, or_, select

from ..db.models import CAPABILITIES, ROLES, TRUST_TIERS, AuditLog, User
from . import accounts, audit, policy

#: What ``apply`` changes, in the order the audit row names them.
FIELDS = ("role", "status", "caps", "invite_quota", "max_concurrent_games", "max_seats_per_game", "trust_tier",
          "listed")
#: The statuses an administrator sets. `pending` is an address not yet confirmed and `deleted` an account gone:
#: neither is a choice.
STATUSES = ("active", "probation", "suspended")
#: Each limit's range (0 seats means the ruleset's maximum).
LIMITS = {"invite_quota": (0, 1000), "max_concurrent_games": (0, 100), "max_seats_per_game": (0, 64)}
#: Accounts per page on the admin users page, and audit entries per page (the most, and the default).
USERS_PAGE = 50
AUDIT_PAGE_MAX = 200
AUDIT_PAGE = 100


class UserAdminError(ValueError):
    """A change that is refused; the message says why. ``status`` is the HTTP answer (400 or 409)."""

    def __init__(self, message: str, status: int = 400):
        super().__init__(message)
        self.status = status


def _int(field: str, value) -> int:
    """A limit, in its range."""
    low, high = LIMITS[field]
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise UserAdminError(f"{field} is a whole number from {low} to {high}.")
    return value


def _caps(user: User, value) -> dict:
    """The account's overrides with ``value`` applied: true or false set one, null removes it."""
    if not isinstance(value, dict):
        raise UserAdminError("caps is an object of {capability: true | false | null}.")
    out = {k: bool(v) for k, v in (user.caps or {}).items() if k in CAPABILITIES}
    for cap, setting in value.items():
        if cap not in CAPABILITIES:
            raise UserAdminError(f"Unknown capability {cap!r}; they are {', '.join(CAPABILITIES)}.")
        if setting is None:
            out.pop(cap, None)
        elif isinstance(setting, bool):
            out[cap] = setting
        else:
            raise UserAdminError(f"{cap} is true, false or null (the role's default).")
    return out


def _other_active_admin(session, user: User) -> bool:
    """Whether an active administrator other than ``user`` exists."""
    return session.scalar(select(func.count()).select_from(User).where(
        User.role == "admin", User.status == "active", User.id != user.id)) > 0


def apply(session, user: User, changes: dict, *, actor: Optional[User] = None, actor_label: str = "",
          reason: Optional[str] = None, ip: Optional[str] = None) -> list:
    """Change ``user`` as an administrator (``actor``, or ``actor_label`` for the command line) asked: ``changes``
    holds any of ``FIELDS``; ``reason`` goes with a suspension (required) and into the audit row. Raises
    ``UserAdminError`` before changing anything when a part is refused. Returns the fields that changed, which the
    one audit row (``user.updated``) names with their old and new values; nothing changed, nothing recorded."""
    if not isinstance(changes, dict):
        raise UserAdminError("Send an object of the fields to change.")
    unknown = sorted(set(changes) - set(FIELDS))
    if unknown:
        raise UserAdminError(f"Unknown field {unknown[0]!r}; an administrator changes {', '.join(FIELDS)}.")
    new: dict = {}
    if "role" in changes:
        if changes["role"] not in ROLES:
            raise UserAdminError(f"role is one of {', '.join(ROLES)}.")
        new["role"] = changes["role"]
    if "status" in changes:
        if changes["status"] not in STATUSES:
            raise UserAdminError(f"status is one of {', '.join(STATUSES)}.")
        new["status"] = changes["status"]
    if "caps" in changes:
        new["caps"] = _caps(user, changes["caps"])
    for field in LIMITS:
        if field in changes:
            new[field] = _int(field, changes[field])
    if "trust_tier" in changes:
        if changes["trust_tier"] not in TRUST_TIERS:
            raise UserAdminError(f"trust_tier is one of {', '.join(TRUST_TIERS)}.")
        new["trust_tier"] = changes["trust_tier"]
    if "listed" in changes:
        if not isinstance(changes["listed"], bool):
            raise UserAdminError("listed is true or false.")
        new["listed"] = changes["listed"]
    reason = (reason or "").strip()[:1000] or None

    before = {f: (dict(user.caps or {}) if f == "caps" else getattr(user, f)) for f in new}
    changed = [f for f in FIELDS if f in new and new[f] != before[f]]
    if not changed:
        return []
    if "status" in changed and new["status"] == "suspended" and not reason:
        raise UserAdminError("Say why the account is suspended: its owner is told the reason when they sign in.")
    stays_admin = new.get("role", user.role) == "admin" and new.get("status", user.status) == "active"
    if user.role == "admin" and user.status == "active" and not stays_admin and not _other_active_admin(session, user):
        raise UserAdminError("That is the only active administrator. Make another account an administrator first.",
                             status=409)

    was_suspended = user.status == "suspended"
    for field in changed:
        setattr(user, field, new[field])
    if "status" in changed:
        if user.status == "suspended":
            user.suspended_at = datetime.now(timezone.utc)
            user.suspended_by = actor.id if actor is not None else None
            user.suspended_reason = reason
            accounts.revoke_all_sessions(session, user)
        elif was_suspended:
            user.suspended_at = user.suspended_by = user.suspended_reason = None
    audit.record(session, "user.updated", actor=actor, actor_label=actor_label, object_type="user",
                 object_id=user.id, ip=ip, fields=changed,
                 changes={f: {"from": before[f], "to": new[f]} for f in changed},
                 **({"reason": reason} if reason else {}))
    if user.role == "admin" and user.status == "active" and ("role" in changed or "status" in changed):
        accounts.on_first_admin(session, user)
    return changed


def to_client(user: User) -> dict:
    """An account as the admin page shows it: everything its owner sees (``User.account``), its standing and when it
    was last seen, and every capability resolved (``capabilities``) beside its overrides (``caps``)."""
    out = user.account()
    out.update(last_seen_at=user.last_seen_at.isoformat() if user.last_seen_at else None,
               suspended_at=user.suspended_at.isoformat() if user.suspended_at else None,
               suspended_reason=user.suspended_reason, is_local_owner=bool(user.is_local_owner),
               capabilities=policy.capabilities(user))
    return out


def _like(text: str) -> str:
    return "%" + text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"


def search(session, *, text: str = "", role: Optional[str] = None, status: Optional[str] = None,
           offset: int = 0) -> dict:
    """Every account (administrators see them all, hidden profiles and suspended accounts included), newest first,
    ``USERS_PAGE`` at a time; ``text`` matches the handle, the display name or the email address."""
    stmt = select(User)
    text = (text or "").strip().lower()[:120]
    if text:
        pattern = _like(text)
        stmt = stmt.where(or_(User.handle_lower.like(pattern, escape="\\"),
                              func.lower(User.display_name).like(pattern, escape="\\"),
                              User.email_lower.like(pattern, escape="\\")))
    if role:
        if role not in ROLES:
            raise UserAdminError(f"role is one of {', '.join(ROLES)}.")
        stmt = stmt.where(User.role == role)
    if status:
        if status not in ("pending", "probation", "active", "suspended", "deleted"):
            raise UserAdminError("status is pending, probation, active, suspended or deleted.")
        stmt = stmt.where(User.status == status)
    offset = max(0, int(offset or 0))
    rows = list(session.scalars(stmt.order_by(User.created_at.desc(), User.id).offset(offset).limit(USERS_PAGE + 1)))
    return {"users": [to_client(u) for u in rows[:USERS_PAGE]], "offset": offset,
            "next": offset + USERS_PAGE if len(rows) > USERS_PAGE else None}


def audit_page(session, *, actor: Optional[str] = None, action: Optional[str] = None,
               object_type: Optional[str] = None, object_id: Optional[str] = None, before: Optional[int] = None,
               limit: int = AUDIT_PAGE) -> dict:
    """A page of the audit log, newest first. ``actor`` is an account's id or handle; ``action`` an action, or a prefix
    ending in ``*`` (``user.*``); ``before`` an entry's id, for the next page (``next_before``)."""
    limit = max(1, min(int(limit or AUDIT_PAGE), AUDIT_PAGE_MAX))
    stmt = select(AuditLog)
    if actor:
        found = session.get(User, actor) or accounts.by_handle(session, actor)
        stmt = stmt.where(AuditLog.actor_id == (found.id if found is not None else actor))
    if action:
        if action.endswith("*"):
            stmt = stmt.where(AuditLog.action.like(_like(action[:-1])[1:], escape="\\"))
        else:
            stmt = stmt.where(AuditLog.action == action)
    if object_type:
        stmt = stmt.where(AuditLog.object_type == object_type)
    if object_id:
        stmt = stmt.where(AuditLog.object_id == object_id)
    if before is not None:
        stmt = stmt.where(AuditLog.id < int(before))
    rows = list(session.scalars(stmt.order_by(AuditLog.id.desc()).limit(limit + 1)))
    entries = _labelled(session, rows[:limit])
    return {"entries": entries, "next_before": entries[-1]["id"] if len(rows) > limit else None}


def about(session, user: User, limit: int = 50) -> list:
    """The audit rows about an account or by it, newest first: the admin page's drawer."""
    rows = session.scalars(select(AuditLog).where(or_(
        (AuditLog.object_type == "user") & (AuditLog.object_id == user.id), AuditLog.actor_id == user.id))
        .order_by(AuditLog.id.desc()).limit(limit))
    return _labelled(session, list(rows))


def _labelled(session, rows: list) -> list:
    """Audit rows as the admin page shows them, an account they are about named by its handle (object_label):
    one query for the page, not one per row."""
    ids = {e.object_id for e in rows if e.object_type == "user" and e.object_id}
    handles = dict(session.execute(select(User.id, User.handle).where(User.id.in_(ids))).all()) if ids else {}
    out = []
    for e in rows:
        entry = audit.to_client(e)
        if e.object_type == "user" and e.object_id in handles:
            entry["object_label"] = "@" + handles[e.object_id]
        out.append(entry)
    return out
