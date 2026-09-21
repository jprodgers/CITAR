"""Accounts: creating them, naming them, proving who owns them.

Three things in here are load-bearing and easy to get subtly wrong, so they are spelled out:

*Case folding.* Handles and email addresses are compared on a stored lowercase column rather than
with SQL functions, because SQLite's `=` is case-sensitive while its `LIKE` is not, and Postgres is
case-sensitive for both. Comparing an explicit column is the only approach that behaves identically
on the database we develop on and the one we might deploy on.

*Enumeration.* Signup, login and password reset never reveal whether an address has an account.
"Check your email" is the answer to a reset request for an unknown address too, and a login failure
says the same thing whether the account is missing or the password is wrong. Otherwise the login
form becomes a free tool for working out who is registered here.

*Timing.* A login for an unknown account still runs a password verification against a dummy hash, so
"no such account" and "wrong password" take the same time. Without it, the enumeration defence above
is decorative — the difference is measurable over a few requests.
"""
from __future__ import annotations

import logging
import re
import secrets
import unicodedata
from datetime import datetime, timedelta, timezone
from typing import Optional, Tuple

from sqlalchemy import func, select

from .. import settings
from ..db.models import AuthSession, EmailToken, Identity, User
from . import audit, passwords, policy, tokens

log = logging.getLogger("citar.accounts")

HANDLE_RE = re.compile(r"^[a-z0-9](?:[a-z0-9_-]{1,30}[a-z0-9])?$")
HANDLE_MIN, HANDLE_MAX = 3, 32

#: Reserved so nobody can register a handle that collides with a route or impersonates the service.
RESERVED_HANDLES = frozenset("""
admin administrator root system citar support help api www mail smtp ftp webmaster postmaster
moderator mod staff official security abuse noreply no-reply info contact about login logout
signup register auth oauth account accounts settings server servers game games report reports
benchmark benchmarks lab probe probes scenario scenarios map maps editor model models user users
me you anonymous guest deleted null undefined static assets ws worker workers health status
""".split())

#: Throwaway-mailbox providers. Not exhaustive and not meant to be — it removes the lazy path while
#: invite-only mode and probation handle anyone who bothers to find a domain that is not listed.
DISPOSABLE_DOMAINS = frozenset("""
10minutemail.com 20minutemail.com 33mail.com guerrillamail.com guerrillamail.net guerrillamail.org
sharklasers.com grr.la mailinator.com mailinator.net notmailinator.com tempmail.com temp-mail.org
tempmailaddress.com throwawaymail.com trashmail.com trashmail.de trash-mail.com yopmail.com
yopmail.fr getnada.com nada.email dispostable.com fakeinbox.com spamgourmet.com mytrashmail.com
mailnesia.com maildrop.cc harakirimail.com mintemail.com tempinbox.com emailondeck.com
burnermail.io anonbox.net spam4.me mailcatch.com inboxbear.com tempr.email discard.email
moakt.com tmpmail.org tmpmail.net luxusmail.org mail-temp.com mohmal.com incognitomail.org
email-fake.com fakemailgenerator.com throwawaymail.net minuteinbox.com tempmailo.com
1secmail.com 1secmail.org 1secmail.net vusra.com byom.de spambog.com mailexpire.com
""".split())

#: Verification and reset links are short-lived. A reset is the shorter of the two because it is the
#: one an attacker wants: an hour is plenty to read an email, and not much of a window to steal one.
VERIFY_HOURS = 24
RESET_HOURS = 1
CHANGE_EMAIL_HOURS = 6

#: A real argon2id hash of a random string, compared against when the account does not exist so that
#: the failure path costs the same as a genuine wrong password.
_DUMMY_HASH = passwords._hasher.hash(secrets.token_urlsafe(32))


class AccountError(ValueError):
    """A problem the person can fix. The message is safe to show them."""


# ---------------------------------------------------------------------------- handles

def normalize_handle(handle: str) -> str:
    """Fold to the comparable form: NFKC, lowercase, trimmed.

    NFKC first so that visually identical handles cannot coexist — without it, a fullwidth or
    styled-unicode lookalike registers as a distinct handle and can impersonate somebody.
    """
    # Only outer whitespace is stripped. Collapsing *interior* spaces would silently turn "has
    # space" into "hasspace" and register something the person did not type; the validator rejects
    # it instead and says so.
    return unicodedata.normalize("NFKC", (handle or "")).strip().lower()


def validate_handle(handle: str) -> str:
    """Return the normalized handle, or raise AccountError explaining what is wrong with it."""
    normalized = normalize_handle(handle)
    if not normalized:
        raise AccountError("Choose a username.")
    if len(normalized) < HANDLE_MIN:
        raise AccountError(f"Usernames are at least {HANDLE_MIN} characters.")
    if len(normalized) > HANDLE_MAX:
        raise AccountError(f"Usernames are at most {HANDLE_MAX} characters.")
    if not HANDLE_RE.match(normalized):
        raise AccountError("Usernames use letters, numbers, hyphens and underscores, and must start "
                           "and end with a letter or number.")
    if normalized in RESERVED_HANDLES:
        raise AccountError("That username is reserved. Pick another.")
    return normalized


def handle_taken(session, handle: str) -> bool:
    """Whether a handle is already in use, after normalisation."""
    normalized = normalize_handle(handle)
    return session.scalar(select(func.count()).select_from(User)
                          .where(User.handle_lower == normalized)) > 0


def suggest_handle(session, base: str) -> str:
    """A free handle near `base`, for SSO signups where the provider gives us a name to start from."""
    seed = re.sub(r"[^a-z0-9_-]", "", normalize_handle(base) or "player")[:HANDLE_MAX - 4] or "player"
    if len(seed) < HANDLE_MIN:
        seed = (seed + "player")[:HANDLE_MIN]
    if seed not in RESERVED_HANDLES and not handle_taken(session, seed):
        return seed
    for _ in range(50):
        candidate = f"{seed}{secrets.randbelow(9000) + 1000}"
        if not handle_taken(session, candidate):
            return candidate
    return f"player{secrets.token_hex(6)}"


# ---------------------------------------------------------------------------- email

def normalize_email(email: str) -> str:
    """Validate and canonicalize, or raise AccountError.

    Deliverability is not checked here: email_validator's DNS check is a network call on the signup
    path, and a domain that resolves today is no promise the mailbox exists. The verification link
    is what actually proves the address works.
    """
    raw = (email or "").strip()
    if not raw:
        raise AccountError("Enter an email address.")
    if len(raw) > 320:
        raise AccountError("That email address is too long.")
    try:
        from email_validator import EmailNotValidError, validate_email
        result = validate_email(raw, check_deliverability=False)
        return result.normalized.lower()
    except ImportError:
        if not re.match(r"^[^@\s]+@[^@\s]+\.[^@\s]+$", raw):
            raise AccountError("That does not look like an email address.")
        return raw.lower()
    except EmailNotValidError as exc:
        raise AccountError(f"That does not look like an email address: {exc}") from exc


def is_disposable(email: str) -> bool:
    """Whether an e-mail address belongs to a known disposable provider."""
    domain = email.rsplit("@", 1)[-1].lower()
    if domain in DISPOSABLE_DOMAINS:
        return True
    # Catch the "anything.mailinator.com" style wildcard domains too.
    return any(domain.endswith("." + known) for known in DISPOSABLE_DOMAINS)


def email_taken(session, email: str) -> bool:
    """Whether an address already has an account."""
    lowered = (email or "").lower()
    return session.scalar(select(func.count()).select_from(User)
                          .where(User.email_lower == lowered, User.status != "deleted")) > 0


# ---------------------------------------------------------------------------- lookup

def by_id(session, user_id: str) -> Optional[User]:
    """An account by id."""
    return session.get(User, user_id) if user_id else None


def by_handle(session, handle: str) -> Optional[User]:
    """An account by handle, normalised."""
    return session.scalar(select(User).where(User.handle_lower == normalize_handle(handle)))


def by_email(session, email: str) -> Optional[User]:
    """An account by e-mail address, normalised."""
    return session.scalar(select(User).where(User.email_lower == (email or "").strip().lower()))


def find_login(session, identifier: str) -> Optional[User]:
    """Accept either a username or an email address in the one login field."""
    identifier = (identifier or "").strip()
    if not identifier:
        return None
    if "@" in identifier:
        return by_email(session, identifier)
    return by_handle(session, identifier)


# ---------------------------------------------------------------------------- creation

def create_user(session, *, handle: str, email: Optional[str] = None, password: Optional[str] = None,
                display_name: str = "", role: str = "user", status: Optional[str] = None,
                tz: str = "UTC", email_verified: bool = False, invited_by: Optional[str] = None,
                avatar_url: Optional[str] = None, is_local_owner: bool = False,
                skip_probation: bool = False) -> User:
    """Create an account. Callers have already decided *whether* to — this enforces how.

    `status` is normally left to the policy: unverified addresses land in `pending`, verified ones in
    `probation` or `active` depending on the server's probation setting.
    """
    normalized_handle = validate_handle(handle)
    if handle_taken(session, normalized_handle):
        raise AccountError("That username is taken.")

    normalized_email = None
    if email:
        normalized_email = normalize_email(email)
        if email_taken(session, normalized_email):
            raise AccountError("That email address already has an account.")
        if is_disposable(normalized_email) and not is_local_owner:
            raise AccountError("That looks like a disposable email address. "
                               "Please use an address you will still have later.")

    password_hash = None
    if password:
        passwords.check(password, handle=normalized_handle, email=normalized_email or "")
        password_hash = passwords.hash_password(password)

    if role not in ("user", "moderator", "admin"):
        raise AccountError(f"Unknown role {role!r}.")

    if status is None:
        if email and not email_verified:
            status = "pending"
        elif policy.get("probation_enabled") and not skip_probation and role == "user":
            status = "probation"
        else:
            status = "active"

    user = User(
        handle=handle.strip() if normalize_handle(handle) == normalized_handle else normalized_handle,
        handle_lower=normalized_handle,
        display_name=(display_name or "").strip()[:80],
        email=normalized_email,
        email_lower=normalized_email,
        email_verified=bool(email_verified),
        password_hash=password_hash,
        role=role,
        status=status,
        tz=tz or "UTC",
        avatar_url=avatar_url,
        invited_by=invited_by,
        is_local_owner=is_local_owner,
        caps={},
        invite_quota=int(policy.get("default_invite_quota") or 0),
        max_concurrent_games=int(policy.get("default_max_concurrent_games") or 2),
        data_sharing="pool",
    )
    session.add(user)
    session.flush()
    log.info("created account %s (%s, %s)", user.handle, user.role, user.status)
    return user


def set_password(session, user: User, password: str, *, actor: Optional[User] = None,
                 ip: Optional[str] = None, revoke_sessions: bool = True) -> None:
    """Set or replace a password, and by default log every other device out.

    Revoking other sessions is the point of a password change after a compromise — leaving them live
    means the person who stole the old password keeps their foothold.
    """
    passwords.check(password, handle=user.handle, email=user.email or "")
    user.password_hash = passwords.hash_password(password)
    if revoke_sessions:
        revoke_all_sessions(session, user)
    audit.record(session, "password.set", actor=actor or user, object_type="user", object_id=user.id, ip=ip)


def revoke_all_sessions(session, user: User, *, keep: Optional[str] = None) -> int:
    """Log the account out everywhere. `keep` spares one session id — the one doing the changing."""
    now = datetime.now(timezone.utc)
    count = 0
    for auth_session in session.scalars(select(AuthSession).where(
            AuthSession.user_id == user.id, AuthSession.revoked_at.is_(None))):
        if keep and auth_session.id == keep:
            continue
        auth_session.revoked_at = now
        count += 1
    return count


# ---------------------------------------------------------------------------- authentication

def authenticate(session, identifier: str, password: str) -> User:
    """Check a username/email and password. Raises AccountError with a deliberately vague message.

    Every failure says the same thing, so the form cannot be used to discover which addresses are
    registered — and an unknown account still pays for a hash verification so the timing matches.
    """
    generic = AccountError("That username or password is not right.")
    user = find_login(session, identifier)
    if user is None:
        passwords.verify(_DUMMY_HASH, password or "x")
        raise generic
    if not passwords.verify(user.password_hash, password):
        raise generic

    if user.status == "suspended":
        reason = f" Reason given: {user.suspended_reason}" if user.suspended_reason else ""
        raise AccountError(f"This account has been suspended.{reason}")
    if user.status == "deleted":
        raise generic
    if user.status == "pending" or (user.email and not user.email_verified):
        raise AccountError("Confirm your email address before signing in. "
                           "We can send the link again from the sign-in page.")

    # Transparent upgrade when the hashing parameters get raised later.
    if user.password_hash and passwords.needs_rehash(user.password_hash):
        user.password_hash = passwords.hash_password(password)
    return user


# ---------------------------------------------------------------------------- email tokens

def issue_email_token(session, user: User, purpose: str, *, hours: Optional[int] = None,
                      new_email: Optional[str] = None) -> Tuple[str, str]:
    """Mint a one-shot link. Returns (raw token, absolute URL).

    Any outstanding token for the same purpose is invalidated first: two live reset links means two
    chances for an attacker and no benefit to anybody.
    """
    if purpose not in ("verify", "reset", "change_email"):
        raise ValueError(f"Unknown token purpose {purpose!r}.")
    hours = hours or {"verify": VERIFY_HOURS, "reset": RESET_HOURS,
                      "change_email": CHANGE_EMAIL_HOURS}[purpose]

    now = datetime.now(timezone.utc)
    for old in session.scalars(select(EmailToken).where(
            EmailToken.user_id == user.id, EmailToken.purpose == purpose,
            EmailToken.used_at.is_(None))):
        old.used_at = now

    raw, hashed = tokens.new_pair()
    session.add(EmailToken(user_id=user.id, purpose=purpose, token_hash=hashed,
                           new_email=new_email, expires_at=now + timedelta(hours=hours)))
    path = {"verify": "/auth/verify", "reset": "/auth/reset", "change_email": "/auth/change-email"}[purpose]
    return raw, settings.get().url(f"{path}?token={raw}")


def consume_email_token(session, raw_token: str, purpose: str) -> Tuple[User, EmailToken]:
    """Redeem a link exactly once. Raises AccountError if it is wrong, used or expired."""
    expired = AccountError("That link is no longer valid. It may have expired or already been used — "
                           "request a new one.")
    if not raw_token:
        raise expired
    row = session.scalar(select(EmailToken).where(EmailToken.token_hash == tokens.hash_token(raw_token)))
    if row is None or row.used_at is not None or row.purpose != purpose:
        raise expired
    if row.expires_at <= datetime.now(timezone.utc):
        raise expired
    user = session.get(User, row.user_id)
    if user is None or user.status == "deleted":
        raise expired
    row.used_at = datetime.now(timezone.utc)
    return user, row


def mark_verified(session, user: User) -> None:
    """Promote an account once its address is confirmed."""
    user.email_verified = True
    if user.status == "pending":
        probation = policy.get("probation_enabled") and user.role == "user"
        user.status = "probation" if probation else "active"


def clear_probation(session, user: User, *, actor: Optional[User] = None) -> None:
    """Take an account off probation, restoring the capabilities it was holding back."""
    if user.status == "probation":
        user.status = "active"
        audit.record(session, "user.probation_cleared", actor=actor, object_type="user", object_id=user.id)


# ---------------------------------------------------------------------------- SSO linking

def link_identity(session, user: User, *, provider: str, provider_user_id: str,
                  email: Optional[str] = None, email_verified: bool = False,
                  display_name: Optional[str] = None, avatar_url: Optional[str] = None) -> Identity:
    """Link a sign-in provider identity to an account."""
    existing = session.scalar(select(Identity).where(
        Identity.provider == provider, Identity.provider_user_id == provider_user_id))
    if existing is not None:
        if existing.user_id != user.id:
            raise AccountError(f"That {provider} account is already linked to a different CITAR account.")
        identity = existing
    else:
        identity = Identity(user_id=user.id, provider=provider, provider_user_id=provider_user_id)
        session.add(identity)
    identity.email = email
    identity.email_verified = bool(email_verified)
    identity.display_name = display_name
    identity.avatar_url = avatar_url
    identity.last_login_at = datetime.now(timezone.utc)
    if not user.avatar_url and avatar_url:
        user.avatar_url = avatar_url
    session.flush()
    return identity


def identity_for(session, provider: str, provider_user_id: str) -> Optional[Identity]:
    """The account a provider identity belongs to, if any."""
    return session.scalar(select(Identity).where(
        Identity.provider == provider, Identity.provider_user_id == provider_user_id))


def unlink_identity(session, user: User, provider: str) -> None:
    """Remove a sign-in method, refusing to leave the account with no way in."""
    identities = list(user.identities)
    target = next((i for i in identities if i.provider == provider), None)
    if target is None:
        raise AccountError(f"No {provider} account is linked.")
    remaining = len(identities) - 1
    if remaining == 0 and not user.password_hash:
        raise AccountError("That is the only way you can sign in. Set a password first, "
                           "or link another account.")
    session.delete(target)


# ---------------------------------------------------------------------------- local mode

LOCAL_HANDLE = "local"


def ensure_local_owner(session) -> User:
    """The account local mode signs in automatically.

    It is a real admin account with a real session — local mode is not an authorization bypass, it
    just skips the login form for a process bound to loopback. If the deployment is later switched
    to server mode, this account is still there and can be given a password and an email address.
    """
    user = session.scalar(select(User).where(User.is_local_owner.is_(True)))
    if user is not None:
        return user
    existing = by_handle(session, LOCAL_HANDLE)
    handle = LOCAL_HANDLE if existing is None else suggest_handle(session, LOCAL_HANDLE)
    user = create_user(session, handle=handle, display_name="Local owner", role="admin",
                       status="active", is_local_owner=True, email_verified=True,
                       tz=_local_timezone())
    audit.record(session, "user.local_owner_created", actor=user, object_type="user", object_id=user.id)
    return user


def _local_timezone() -> str:
    """This machine's IANA zone, so availability windows read correctly out of the box."""
    try:
        from tzlocal import get_localzone_name
        return get_localzone_name() or "UTC"
    except Exception:
        pass
    try:
        key = datetime.now().astimezone().tzinfo
        name = getattr(key, "key", None) or str(key)
        # Windows returns a display name like "Eastern Daylight Time", which zoneinfo cannot load.
        from zoneinfo import ZoneInfo
        ZoneInfo(name)
        return name
    except Exception:
        return "UTC"


def bootstrap_admin(session, *, handle: str, email: str, password: str) -> User:
    """Create the first administrator of a server deployment. Refuses once one exists."""
    existing = session.scalar(select(func.count()).select_from(User).where(User.role == "admin"))
    if existing:
        raise AccountError("This server already has an administrator.")
    user = create_user(session, handle=handle, email=email, password=password, role="admin",
                       status="active", email_verified=True, display_name=handle)
    audit.record(session, "user.bootstrap_admin", actor=user, object_type="user", object_id=user.id)
    return user
