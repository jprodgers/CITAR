"""Invite codes: the way in when registration is invite-only.

An invite carries the role the new account gets, so it is the mechanism for appointing moderators
too. Only an administrator may mint anything above `user` — otherwise a moderator could quietly
promote themselves by inviting a second account with an admin role and logging into it.

Codes are stored as issued rather than hashed. They are low-value bearer tokens that the person who
made one has to be able to copy again to send it, every redemption is rate limited and audited, and
an invite grants nothing beyond the right to create an account that still has to verify an email.
Hashing them would cost the "show me that code again" button and buy very little.
"""
from __future__ import annotations

import logging
from datetime import datetime, timedelta, timezone
from typing import Optional

from sqlalchemy import func, select

from .. import settings
from ..db.models import Invite, User
from . import accounts, audit, policy, tokens

log = logging.getLogger("citar.invites")

DEFAULT_EXPIRY_DAYS = 14


class InviteError(ValueError):
    """A problem with an invite. Safe to show."""


def create(session, creator: User, *, role: str = "user", email: Optional[str] = None,
           max_uses: int = 1, expires_days: Optional[int] = DEFAULT_EXPIRY_DAYS,
           note: str = "", skip_probation: bool = True, ip: Optional[str] = None) -> Invite:
    """Mint a code. Enforces who may invite, at what role, and how many are outstanding."""
    if not policy.can(creator, "invite"):
        raise InviteError("Your account cannot create invitations.")
    if role not in ("user", "moderator", "admin"):
        raise InviteError(f"Unknown role {role!r}.")
    if role != "user" and creator.role != "admin":
        raise InviteError("Only an administrator can invite moderators or administrators.")
    if max_uses < 1 or max_uses > 100:
        raise InviteError("An invite can be used between 1 and 100 times.")

    if creator.role != "admin":
        # The quota counts *outstanding* invites, not lifetime ones: it limits how much damage a
        # compromised account can do at once without punishing somebody who invites people slowly.
        outstanding = session.scalar(
            select(func.coalesce(func.sum(Invite.max_uses - Invite.uses), 0)).where(
                Invite.created_by == creator.id, Invite.revoked_at.is_(None)))
        if outstanding + max_uses > (creator.invite_quota or 0):
            raise InviteError(f"That would exceed your invite allowance "
                              f"({creator.invite_quota or 0} outstanding).")

    normalized_email = accounts.normalize_email(email) if email else None
    invite = Invite(
        code=tokens.invite_code(),
        created_by=creator.id,
        role=role,
        email=normalized_email,
        max_uses=max_uses,
        skip_probation=skip_probation,
        note=(note or "").strip()[:500] or None,
        expires_at=(datetime.now(timezone.utc) + timedelta(days=expires_days)) if expires_days else None,
    )
    session.add(invite)
    session.flush()
    audit.record(session, "invite.created", actor=creator, object_type="invite", object_id=invite.id,
                 ip=ip, role=role, max_uses=max_uses, pinned_email=bool(normalized_email))
    return invite


def find(session, code: str) -> Optional[Invite]:
    """An invitation by its code."""
    normalized = tokens.normalize_invite_code(code)
    if not normalized:
        return None
    return session.scalar(select(Invite).where(Invite.code == normalized))


def check(session, code: str, *, email: Optional[str] = None) -> Invite:
    """Validate a code without consuming it — used to show "you are invited as a moderator" on the
    signup form before anything is created."""
    invite = find(session, code)
    if invite is None:
        raise InviteError("That invitation code is not valid.")
    if invite.revoked_at is not None:
        raise InviteError("That invitation has been withdrawn.")
    if invite.expires_at is not None and invite.expires_at <= datetime.now(timezone.utc):
        raise InviteError("That invitation has expired. Ask for a new one.")
    if invite.uses >= invite.max_uses:
        raise InviteError("That invitation has already been used.")
    if invite.email and email and accounts.normalize_email(email) != invite.email:
        raise InviteError("That invitation was issued for a different email address.")
    return invite


def redeem(session, invite: Invite, user: User, *, ip: Optional[str] = None) -> None:
    """Consume one use, after the account it invited has been created."""
    invite.uses += 1
    user.invited_by = invite.created_by
    if invite.role != "user":
        user.role = invite.role
    if invite.skip_probation and user.status == "probation":
        user.status = "active"
    audit.record(session, "invite.redeemed", actor=user, object_type="invite", object_id=invite.id,
                 ip=ip, role=invite.role, invited_by=invite.created_by)


def revoke(session, invite: Invite, actor: User, *, ip: Optional[str] = None) -> None:
    """Revoke an unused invitation."""
    if invite.created_by != actor.id and actor.role != "admin":
        raise InviteError("You can only withdraw invitations you created.")
    if invite.revoked_at is None:
        invite.revoked_at = datetime.now(timezone.utc)
    audit.record(session, "invite.revoked", actor=actor, object_type="invite", object_id=invite.id, ip=ip)


def list_for(session, user: User, *, include_spent: bool = False) -> list:
    """The invitations one account has created."""
    stmt = select(Invite).order_by(Invite.created_at.desc())
    if user.role != "admin":
        stmt = stmt.where(Invite.created_by == user.id)
    rows = list(session.scalars(stmt))
    if not include_spent:
        rows = [r for r in rows if r.usable()]
    return [to_client(session, r) for r in rows]


def to_client(session, invite: Invite) -> dict:
    """An invitation as the client shows it, with its link."""
    creator = session.get(User, invite.created_by) if invite.created_by else None
    return {
        "id": invite.id,
        "code": invite.code,
        "url": settings.get().url(f"/#/signup?invite={invite.code}"),
        "role": invite.role,
        "email": invite.email,
        "max_uses": invite.max_uses,
        "uses": invite.uses,
        "note": invite.note,
        "skip_probation": invite.skip_probation,
        "created_by": creator.handle if creator else None,
        "created_at": invite.created_at.isoformat() if invite.created_at else None,
        "expires_at": invite.expires_at.isoformat() if invite.expires_at else None,
        "revoked_at": invite.revoked_at.isoformat() if invite.revoked_at else None,
        "usable": invite.usable(),
    }
