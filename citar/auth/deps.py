"""FastAPI dependencies: who is calling, and may they do this.

Every protected route declares its requirement instead of checking by hand:

    @router.post("/api/servers")
    def create(body: dict, me: User = Depends(require_cap("register_servers"))):

The dependency and the route share one database session per request (FastAPI caches a dependency's
result for the request), so reading the caller and doing the work happen in one transaction.

Local mode signs the owner in here. It is worth being precise about what that does and does not
mean: it mints a genuine session for a genuine admin account, and every check below then runs
exactly as it would in server mode. There is no branch anywhere that skips a permission test because
the server is local — which is the property that stops local mode from becoming a place where
authorization bugs hide.
"""
from __future__ import annotations

import logging
from typing import Callable, Iterator, Optional

from fastapi import Depends, HTTPException, Request, Response
from sqlalchemy.orm import Session

from .. import db, settings
from ..db.models import AuthSession, User
from . import accounts, policy, ratelimit, sessions

log = logging.getLogger("citar.deps")


def get_db() -> Iterator[Session]:
    """A database session for the request, shared by every dependency in it."""
    with db.session() as s:
        yield s


class Principal:
    """The caller: an account and the session it is using, either possibly absent."""

    __slots__ = ("user", "auth_session", "ip", "local_auto")

    def __init__(self, user: Optional[User], auth_session: Optional[AuthSession],
                 ip: str = "", local_auto: bool = False):
        self.user = user
        self.auth_session = auth_session
        self.ip = ip
        #: True when local mode just signed this caller in without a login form.
        self.local_auto = local_auto

    @property
    def authenticated(self) -> bool:
        """Whether there is an account behind this request."""
        return self.user is not None

    def client(self) -> dict:
        """The principal as the client shows it."""
        if self.user is None:
            return {"authenticated": False, "user": None, "capabilities": {}}
        return {
            "authenticated": True,
            "user": self.user.public(full=True),
            "capabilities": policy.capabilities(self.user),
            "csrf_token": self.auth_session.csrf_token if self.auth_session else "",
            "session_id": self.auth_session.id if self.auth_session else "",
            "local_mode": settings.get().local,
        }


def principal(request: Request, response: Response, s: Session = Depends(get_db)) -> Principal:
    """Resolve the caller. Never raises — routes that need an account ask for `require_user`."""
    ip = ratelimit.client_ip(request)
    row = sessions.resolve(s, sessions.cookie_from(request))
    user = sessions.user_for(s, row)

    if user is None and settings.get().local:
        # Loopback-only process, single operator: skip the login form, but still make a real session.
        user = accounts.ensure_local_owner(s)
        row, raw = _local_session(s, user, request)
        if raw is not None:
            sessions.set_cookie(response, raw)
        return Principal(user, row, ip, local_auto=True)

    if user is not None:
        request.state.citar_user_id = user.id
    return Principal(user, row, ip)


#: The local owner's session token, kept in memory so that requests which cannot carry a cookie back
#: (anything returning a raw Response, the websocket handshake, curl) do not mint a fresh session
#: row every time. Without this, a local instance accumulates thousands of sessions in an afternoon.
_local_token: Optional[str] = None


def _local_session(s: Session, user: User, request: Request):
    """Reuse the local owner's live session if there is one; otherwise open one.

    Returns (session row, raw token to set as a cookie) — the token is None when an existing session
    was reused and the caller already holds the cookie.
    """
    global _local_token
    if _local_token:
        row = sessions.resolve(s, _local_token)
        if row is not None and row.user_id == user.id:
            return row, _local_token
    row, raw = sessions.start(s, user, ip="127.0.0.1",
                              user_agent=request.headers.get("user-agent"))
    _local_token = raw
    return row, raw


def _enforce_csrf(request: Request, p: Principal) -> None:
    """Cookie-authenticated writes must carry the session's CSRF token."""
    if request.method not in sessions.UNSAFE_METHODS:
        return
    if not sessions.origin_ok(request):
        expected = settings.get().public_origin
        got = request.headers.get("origin") or "none"
        # Naming both sides turns the commonest cause — reaching the server as 127.0.0.1 when
        # CITAR_PUBLIC_ORIGIN says localhost, or over http when it says https — from a mystery
        # into a one-line fix.
        raise HTTPException(403,
                            f"This request came from {got}, but this server expects {expected}. "
                            "Reach it at that address, or correct CITAR_PUBLIC_ORIGIN.")
    if not sessions.csrf_ok(request, p.auth_session):
        raise HTTPException(403, "Your session token is stale. Reload the page and try again.")


def require_user(request: Request, p: Principal = Depends(principal)) -> User:
    """An authenticated, usable account."""
    if p.user is None:
        raise HTTPException(401, "Sign in to do that.")
    if p.user.status == "pending":
        raise HTTPException(403, "Confirm your email address first.")
    if p.user.status in ("suspended", "deleted"):
        raise HTTPException(403, "This account is not active.")
    _enforce_csrf(request, p)
    return p.user


def optional_user(p: Principal = Depends(principal)) -> Optional[User]:
    """For routes that serve both signed-in and anonymous callers — a public game, say."""
    return p.user


def require_role(role: str) -> Callable:
    """`moderator` or `admin`. Ranked, so an admin satisfies a moderator requirement."""

    def dependency(request: Request, p: Principal = Depends(principal)) -> User:
        """Resolve the caller and refuse anyone below the required role."""
        user = require_user(request, p)
        if not user.at_least(role):
            raise HTTPException(403, f"That needs {role} access.")
        return user

    return dependency


def require_cap(capability: str) -> Callable:
    """A specific capability, resolved through role defaults, probation and per-account overrides."""

    def dependency(request: Request, p: Principal = Depends(principal)) -> User:
        """Resolve the caller and refuse anyone without the required capability."""
        user = require_user(request, p)
        if not policy.can(user, capability):
            raise HTTPException(403, _denial_message(user, capability))
        return user

    return dependency


def _denial_message(user: User, capability: str) -> str:
    """Say *why*, so the person can act on it rather than guessing."""
    if user.status == "probation" and capability in policy.PROBATION_DENIES:
        return ("New accounts cannot do that yet. An administrator can lift the restriction — "
                "it is there to keep automated signups from registering servers or publishing.")
    friendly = {
        "register_servers": "Your account is not allowed to register servers.",
        "run_reports": "Your account is not allowed to build reports.",
        "publish_public": "Your account is not allowed to publish to the public internet.",
        "create_games": "Your account is not allowed to start games.",
        "invite": "Your account cannot create invitations.",
    }
    return friendly.get(capability, f"Your account lacks the {capability} permission.")


# Convenience aliases the routers read more clearly with.
require_admin = require_role("admin")
require_moderator = require_role("moderator")
