"""The admin users page and the audit log over HTTP (design/phase3-features.md P3.2.7).

    GET   /api/admin/users           every account, newest first; ?q= (handle, display name, email), ?role=, ?status=,
                                     ?offset= by 50
    GET   /api/admin/users/{id}      one account: everything about it, its capabilities resolved, the role defaults,
                                     and its audit rows (about it and by it)
    PATCH /api/admin/users/{id}      any of role, status (with "reason" for a suspension), caps ({cap: true | false |
                                     null}), invite_quota, max_concurrent_games, max_seats_per_game, trust_tier,
                                     listed: one audit row naming the fields (``user_admin.apply``)
    GET   /api/admin/audit           the audit log, newest first; ?actor= (id or handle), ?action= (or a prefix
                                     ending in *), ?object_type=, ?object_id=, ?before= (an entry's id), ?limit= (at
                                     most 200)

Administrators only. A signed-in account that is not one gets 404 on every route here, as if there were none
(``require_role("admin", hide=True)``); a visitor 401.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Body, Depends, HTTPException, Request
from sqlalchemy.orm import Session

from ..auth import policy, ratelimit, user_admin
from ..auth.deps import get_db, require_role
from ..db.models import User

router = APIRouter()
require_admin_hidden = require_role("admin", hide=True)


def _user(s: Session, uid: str) -> User:
    """The account, or 404."""
    user = s.get(User, uid)
    if user is None:
        raise HTTPException(404, "No such account.")
    return user


def _call(fn, *args, **kwargs):
    """Run an administration call, its refusal an HTTP answer."""
    try:
        return fn(*args, **kwargs)
    except user_admin.UserAdminError as exc:
        raise HTTPException(exc.status, str(exc))


@router.get("/api/admin/users")
def list_users(q: str = "", role: Optional[str] = None, status: Optional[str] = None, offset: int = 0,
               me: User = Depends(require_admin_hidden), s: Session = Depends(get_db)):
    """Every account, newest first, filtered and paged (``user_admin.search``)."""
    return _call(user_admin.search, s, text=q, role=role or None, status=status or None, offset=offset)


@router.get("/api/admin/users/{uid}")
def get_user(uid: str, me: User = Depends(require_admin_hidden), s: Session = Depends(get_db)):
    """One account for the edit drawer: everything about it, the role defaults its capabilities fall back to, and
    its audit rows."""
    user = _user(s, uid)
    defaults = (policy.get("role_defaults") or {}).get(user.role) or {}
    return {"user": user_admin.to_client(user),
            "role_defaults": {cap: bool(defaults.get(cap, (policy.ROLE_DEFAULTS.get(user.role) or {}).get(cap, False)))
                              for cap in policy.CAPABILITIES},
            "audit": user_admin.about(s, user)}


@router.patch("/api/admin/users/{uid}")
def patch_user(uid: str, request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
               s: Session = Depends(get_db)):
    """Change an account (``user_admin.apply``): the body's fields other than ``reason``, all checked before any
    changes; one audit row names what changed."""
    body = dict(body)
    reason = body.pop("reason", None)
    if reason is not None and not isinstance(reason, str):
        raise HTTPException(400, "reason is text.")
    user = _user(s, uid)
    changed = _call(user_admin.apply, s, user, body, actor=me, reason=reason, ip=ratelimit.client_ip(request))
    return {"changed": changed, "user": user_admin.to_client(user)}


@router.get("/api/admin/audit")
def audit_log(actor: Optional[str] = None, action: Optional[str] = None, object_type: Optional[str] = None,
              object_id: Optional[str] = None, before: Optional[int] = None, limit: int = user_admin.AUDIT_PAGE,
              me: User = Depends(require_admin_hidden), s: Session = Depends(get_db)):
    """A page of the audit log (``user_admin.audit_page``)."""
    return user_admin.audit_page(s, actor=actor or None, action=action or None, object_type=object_type or None,
                                 object_id=object_id or None, before=before, limit=limit)
