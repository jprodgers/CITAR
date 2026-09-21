"""Browser sessions: the cookie, its lifetime, and CSRF.

Opaque server-side sessions rather than a self-contained JWT. The deciding factor is revocation: a
signed token is valid until it expires no matter what happens to the account, so "log out everywhere"
and "suspend this account now" either do not work or need a server-side blocklist — at which point
the database lookup that a JWT was supposed to avoid is back, with extra steps.

The cookie carries a random token; the database stores only its hash, so a leaked backup yields no
live sessions.

Cookie flags, and why each one:

    HttpOnly    script cannot read it, so an XSS bug cannot exfiltrate the session
    Secure      never sent over plain HTTP (relaxed only in local mode, which has no HTTPS)
    SameSite    Lax — the cookie does not ride along on cross-site POSTs, which blocks the
                straightforward CSRF attack before the token check even runs
    Path=/      one session for the whole app

SameSite=Lax is not sufficient on its own: it does nothing for same-site requests and browsers
disagree at the edges, so state-changing requests also carry a CSRF token that must match the one
bound to the session. Lax must also not be tightened to Strict, because an OAuth callback arrives as
a cross-site navigation and Strict would drop the cookie on the way back from the provider.
"""
from __future__ import annotations

import logging
from datetime import datetime, timedelta, timezone
from typing import Optional, Tuple

from sqlalchemy import select

from .. import settings
from ..db.models import AuthSession, User
from . import tokens

log = logging.getLogger("citar.sessions")

COOKIE_NAME = "citar_session"
CSRF_HEADER = "x-citar-csrf"
#: Methods that change state and therefore need a CSRF token.
UNSAFE_METHODS = ("POST", "PUT", "PATCH", "DELETE")


def start(session, user: User, *, ip: Optional[str] = None, user_agent: Optional[str] = None
          ) -> Tuple[AuthSession, str]:
    """Open a session for `user`. Returns the row and the raw token for the cookie."""
    cfg = settings.get()
    raw, hashed = tokens.new_pair()
    now = datetime.now(timezone.utc)
    row = AuthSession(
        user_id=user.id,
        token_hash=hashed,
        created_at=now,
        last_seen_at=now,
        expires_at=now + timedelta(days=cfg.session_days),
        ip=(ip or "")[:64] or None,
        user_agent=(user_agent or "")[:300] or None,
    )
    session.add(row)
    user.last_seen_at = now
    session.flush()
    return row, raw


def resolve(session, raw_token: Optional[str]) -> Optional[AuthSession]:
    """The live session for this cookie, or None. Touches `last_seen_at` as a side effect.

    A session that is past its idle limit is revoked here rather than merely ignored, so it does not
    come back to life if somebody returns with the same cookie later.
    """
    if not raw_token:
        return None
    row = session.scalar(select(AuthSession).where(AuthSession.token_hash == tokens.hash_token(raw_token)))
    if row is None:
        return None

    cfg = settings.get()
    now = datetime.now(timezone.utc)
    idle_cutoff = now - timedelta(hours=cfg.session_idle_hours)
    if row.revoked_at is not None or row.expires_at <= now:
        return None
    if row.last_seen_at < idle_cutoff:
        row.revoked_at = now
        return None

    # Only write when the clock has moved enough to matter; otherwise every request writes a row.
    if (now - row.last_seen_at).total_seconds() > 60:
        row.last_seen_at = now
    return row


def user_for(session, row: Optional[AuthSession]) -> Optional[User]:
    """The account behind a session, provided it is still allowed in.

    Status is re-checked on every request on purpose: suspending an account has to take effect
    immediately, not whenever the cookie happens to expire.
    """
    if row is None:
        return None
    user = session.get(User, row.user_id)
    if user is None or user.status in ("suspended", "deleted"):
        if user is not None and row.revoked_at is None:
            row.revoked_at = datetime.now(timezone.utc)
        return None
    return user


def revoke(session, row: AuthSession) -> None:
    """End a session."""
    if row.revoked_at is None:
        row.revoked_at = datetime.now(timezone.utc)


def revoke_by_id(session, user: User, session_id: str) -> bool:
    """End a session by id, for signing out a device from elsewhere."""
    row = session.get(AuthSession, session_id)
    if row is None or row.user_id != user.id:
        return False
    revoke(session, row)
    return True


def list_for(session, user: User, *, current_id: Optional[str] = None) -> list:
    """Every live session for the account — the device list on the account page."""
    now = datetime.now(timezone.utc)
    rows = session.scalars(select(AuthSession)
                           .where(AuthSession.user_id == user.id, AuthSession.revoked_at.is_(None))
                           .order_by(AuthSession.last_seen_at.desc()))
    return [{"id": r.id, "created_at": r.created_at.isoformat(), "last_seen_at": r.last_seen_at.isoformat(),
             "expires_at": r.expires_at.isoformat(), "ip": r.ip, "user_agent": r.user_agent,
             "current": r.id == current_id}
            for r in rows if r.expires_at > now]


def purge_expired(session) -> int:
    """Delete sessions that expired more than a month ago. Called by the janitor."""
    cutoff = datetime.now(timezone.utc) - timedelta(days=30)
    rows = list(session.scalars(select(AuthSession).where(AuthSession.expires_at < cutoff)))
    for row in rows:
        session.delete(row)
    return len(rows)


# ---------------------------------------------------------------------------- cookies

def set_cookie(response, raw_token: str) -> None:
    """Set the session cookie, with the flags the current mode requires."""
    cfg = settings.get()
    response.set_cookie(
        COOKIE_NAME,
        raw_token,
        max_age=cfg.session_days * 86400,
        httponly=True,
        # Local mode is plain HTTP on loopback; a Secure cookie there would simply never be sent.
        secure=cfg.require_https,
        samesite="lax",
        path="/",
    )


def clear_cookie(response) -> None:
    """Clear the session cookie."""
    cfg = settings.get()
    response.delete_cookie(COOKIE_NAME, path="/", httponly=True,
                           secure=cfg.require_https, samesite="lax")


def cookie_from(request) -> Optional[str]:
    """Read the session cookie from a request."""
    return request.cookies.get(COOKIE_NAME)


# ---------------------------------------------------------------------------- CSRF

def csrf_ok(request, row: Optional[AuthSession]) -> bool:
    """Whether a state-changing request carries the right CSRF token.

    Requests authenticated by a seat token or a bearer token in the Authorization header are exempt:
    CSRF is an attack on *ambient* credentials, and a header a browser will not attach on its own
    cannot be forged by another site.
    """
    if request.method not in UNSAFE_METHODS:
        return True
    if row is None:
        return True          # not cookie-authenticated; nothing ambient to abuse
    if request.headers.get("authorization"):
        return True
    import hmac
    sent = request.headers.get(CSRF_HEADER) or ""
    return bool(sent) and hmac.compare_digest(sent, row.csrf_token or "")


def origin_ok(request) -> bool:
    """Check Origin on websocket upgrades and unsafe requests.

    A browser sets Origin on cross-site requests and cannot be talked out of it, which makes this a
    cheap second line behind SameSite. Requests with no Origin at all (curl, the MCP client, a
    worker) are allowed through — they are not browsers carrying somebody's ambient cookie.
    """
    origin = request.headers.get("origin")
    if not origin:
        return True
    cfg = settings.get()
    if origin == cfg.public_origin:
        return True
    if cfg.local:
        # Local mode gets reached as localhost, 127.0.0.1 or the LAN address interchangeably.
        from urllib.parse import urlparse
        host = urlparse(origin).hostname or ""
        return host in ("localhost", "127.0.0.1", "::1") or host.startswith("192.168.") \
            or host.startswith("10.") or host.endswith(".local")
    return False
