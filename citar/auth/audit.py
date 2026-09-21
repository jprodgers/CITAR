"""The audit log: an append-only record of anything consequential.

Written on logins and failed logins, role and capability changes, invites, grants and budget edits,
suspensions, publishes, deletions and worker-token issuance. Moderators can read it; no application
route updates or deletes a row.

Two rules make it worth having when something has gone wrong:

*The actor's label is copied in, not joined to.* An account that is later deleted or renamed leaves
its history intact and readable, instead of turning every row into "user u_9f3a… (deleted)".

*Detail never contains a secret.* Passwords, tokens, API keys and session ids are never written —
what goes in `detail` is the shape of what happened ("role: user -> moderator"), so the log can be
read by a moderator without becoming a second place credentials leak from.
"""
from __future__ import annotations

import logging
from typing import Any, Optional

from sqlalchemy import desc, select

from ..db.models import AuditLog, User

log = logging.getLogger("citar.audit")

#: Keys that must never reach the log, whatever a caller passes.
_FORBIDDEN = ("password", "token", "secret", "api_key", "apikey", "csrf", "cookie", "authorization",
              "client_secret", "passphrase", "hash")


def _clean(detail: Optional[dict]) -> dict:
    """Drop anything that looks like a credential, however it was nested."""
    if not detail:
        return {}
    out: dict = {}
    for key, value in detail.items():
        if any(bad in str(key).lower() for bad in _FORBIDDEN):
            out[key] = "<redacted>"
        elif isinstance(value, dict):
            out[key] = _clean(value)
        elif isinstance(value, (str, int, float, bool, type(None))):
            out[key] = value
        elif isinstance(value, (list, tuple)):
            out[key] = [v if isinstance(v, (str, int, float, bool, type(None))) else str(v) for v in value][:50]
        else:
            out[key] = str(value)
    return out


def record(session, action: str, *, actor: Optional[User] = None, actor_id: Optional[str] = None,
           actor_label: str = "", object_type: Optional[str] = None, object_id: Optional[str] = None,
           ip: Optional[str] = None, **detail: Any) -> AuditLog:
    """Append one entry. Call inside the same unit of work as the change it describes, so the log
    and the change commit or roll back together."""
    if actor is not None:
        actor_id = actor.id
        actor_label = actor_label or f"{actor.handle} ({actor.role})"
    entry = AuditLog(actor_id=actor_id, actor_label=actor_label or "system", action=action,
                     object_type=object_type, object_id=object_id, ip=ip, detail=_clean(detail))
    session.add(entry)
    log.info("audit %s actor=%s object=%s/%s", action, actor_label or actor_id or "system",
             object_type or "-", object_id or "-")
    return entry


def recent(session, *, limit: int = 100, actor_id: Optional[str] = None, action: Optional[str] = None,
           object_type: Optional[str] = None, object_id: Optional[str] = None) -> list:
    """Recent audit entries, optionally filtered by actor or action."""
    stmt = select(AuditLog).order_by(desc(AuditLog.at)).limit(min(limit, 1000))
    if actor_id:
        stmt = stmt.where(AuditLog.actor_id == actor_id)
    if action:
        stmt = stmt.where(AuditLog.action == action)
    if object_type:
        stmt = stmt.where(AuditLog.object_type == object_type)
    if object_id:
        stmt = stmt.where(AuditLog.object_id == object_id)
    return list(session.scalars(stmt))


def to_client(entry: AuditLog) -> dict:
    """An audit entry as the console shows it."""
    return {"id": entry.id, "at": entry.at.isoformat() if entry.at else None,
            "actor_id": entry.actor_id, "actor": entry.actor_label, "action": entry.action,
            "object_type": entry.object_type, "object_id": entry.object_id, "ip": entry.ip,
            "detail": entry.detail or {}}
