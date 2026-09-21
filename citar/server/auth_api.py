"""HTTP API for signing in, signing up and managing an account.

Emailed links land on server-side routes (`/auth/verify`, `/auth/reset`) rather than on a page the
frontend has to interpret. A verification link then works even if scripts fail to load, and the
one-shot token is consumed by the server before any redirect happens, so it never sits in a URL the
browser hands on to a third party through the Referer header.
"""
from __future__ import annotations

import logging
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request, Response
from fastapi.responses import RedirectResponse
from pydantic import BaseModel, Field
from sqlalchemy.orm import Session

from .. import settings
from ..auth import (accounts, audit, captcha, invites, mailer, oauth, passwords, policy,
                    ratelimit, sessions)
from ..auth.deps import Principal, get_db, principal, require_user
from ..db.models import Invite, User

log = logging.getLogger("citar.auth_api")
router = APIRouter()


def _fail(exc: Exception, status: int = 400) -> HTTPException:
    """Turn an internal error into an HTTP error, without leaking what happened internally."""
    return HTTPException(status, str(exc))


def _limit(rule: str, subject: str, s: Optional[Session] = None) -> None:
    """Apply a rate limit, raising 429 when it is exceeded.

    Every endpoint that can be used to guess something - sign-in, password reset, invitation checks -
    goes through here. The limits are per subject rather than global, so one attacker cannot lock
    everybody else out by exhausting a shared counter.
    """
    try:
        ratelimit.check(rule, subject, session=s)
    except ratelimit.RateLimited as exc:
        raise HTTPException(429, str(exc), headers={"Retry-After": str(exc.retry_after)})


def _ua(request: Request) -> str:
    """The user agent, truncated, for the session list."""
    return request.headers.get("user-agent", "")[:300]


# ============================================================================ configuration

@router.get("/api/auth/config")
def auth_config():
    """What the login page needs before anybody has signed in."""
    cfg = settings.get()
    return {
        **policy.client_policy(),
        "providers": oauth.available(),
        "captcha": captcha.client_config(),
        "local_mode": cfg.local,
        "password_min_length": passwords.MIN_LENGTH,
    }


@router.get("/api/auth/me")
def whoami(p: Principal = Depends(principal)):
    """Who the caller is, or ``authenticated: false``.

    Deliberately public: it is the endpoint a client asks before it knows whether it is signed in.
    """
    return p.client()


# ============================================================================ sign up / in

class SignupBody(BaseModel):
    """A registration: handle, e-mail, password, and an invitation code when one is required."""
    handle: str = Field(min_length=1, max_length=64)
    email: str = Field(min_length=3, max_length=320)
    password: str = Field(min_length=1, max_length=1024)
    display_name: str = ""
    invite: Optional[str] = None
    captcha_token: Optional[str] = None
    tz: Optional[str] = None


@router.post("/api/auth/signup")
async def signup(body: SignupBody, request: Request, response: Response, s: Session = Depends(get_db)):
    """Create an account, subject to the registration mode, the captcha and the invitation."""
    ip = ratelimit.client_ip(request)
    _limit("signup_ip", ip, s)
    _limit("signup_subnet", ratelimit.subnet(ip), s)

    mode = policy.registration_mode()
    invite = None
    if body.invite:
        try:
            invite = invites.check(s, body.invite, email=body.email)
        except invites.InviteError as exc:
            _limit("invite_redeem", ip, s)
            raise _fail(exc)
    elif mode != "open":
        raise HTTPException(403, policy.get("closed_message") or
                            "This server is invite-only. Ask an administrator for an invitation.")

    if not settings.get().email_enabled:
        raise HTTPException(503, "Email sign-up is unavailable on this server because it has no mail "
                                 "server configured. Use one of the sign-in providers instead.")

    # An invite is a human vouching for a human; the captcha is for the open door.
    if invite is None:
        try:
            await captcha.verify(body.captcha_token, remote_ip=ip)
        except captcha.CaptchaError as exc:
            raise _fail(exc)

    try:
        user = accounts.create_user(
            s, handle=body.handle, email=body.email, password=body.password,
            display_name=body.display_name or body.handle, tz=_clean_tz(body.tz),
            skip_probation=bool(invite and invite.skip_probation))
        if invite is not None:
            invites.redeem(s, invite, user, ip=ip)
        raw_token, url = accounts.issue_email_token(s, user, "verify")
        audit.record(s, "user.signup", actor=user, object_type="user", object_id=user.id, ip=ip,
                     method="password", invited=bool(invite))
    except (accounts.AccountError, passwords.PasswordError) as exc:
        raise _fail(exc)

    s.commit()
    # The account exists from here on, so a mail failure is reported without discarding it —
    # answering 502 would leave somebody with an account they have been told was not created.
    sent = False
    try:
        sent = await mailer.send_verification(user.email, user.name, url)
    except mailer.MailError as exc:
        log.error("verification mail failed for %s: %s", user.email, exc)
    return {
        "ok": True,
        "status": user.status,
        "email_sent": sent,
        "message": (f"Account created. Confirm {user.email} to sign in."
                    if sent else
                    "Account created, but the confirmation email could not be sent just now. "
                    "Use 'resend confirmation' on the sign-in page in a moment."),
    }


class LoginBody(BaseModel):
    """Credentials for signing in."""
    identifier: str = Field(min_length=1, max_length=320)
    password: str = Field(min_length=1, max_length=1024)
    tz: Optional[str] = None


@router.post("/api/auth/login")
def login(body: LoginBody, request: Request, response: Response, s: Session = Depends(get_db)):
    """Sign in and start a session."""
    ip = ratelimit.client_ip(request)
    _limit("login_ip", ip)
    # Limit the account too: per-IP alone lets a botnet spray one password at every account.
    _limit("login_account", accounts.normalize_handle(body.identifier)[:64], s)

    try:
        user = accounts.authenticate(s, body.identifier, body.password)
    except accounts.AccountError as exc:
        audit.record(s, "auth.login_failed", actor_label=body.identifier[:60], ip=ip)
        raise HTTPException(401, str(exc))

    ratelimit.clear("login_account", accounts.normalize_handle(body.identifier)[:64], session=s)
    if body.tz:
        user.tz = _clean_tz(body.tz)
    row, raw = sessions.start(s, user, ip=ip, user_agent=_ua(request))
    sessions.set_cookie(response, raw)
    audit.record(s, "auth.login", actor=user, ip=ip, method="password")
    return {"ok": True, "user": user.public(full=True), "csrf_token": row.csrf_token,
            "capabilities": policy.capabilities(user)}


@router.post("/api/auth/logout")
def logout(request: Request, response: Response, p: Principal = Depends(principal),
           s: Session = Depends(get_db)):
    """End the current session."""
    if p.auth_session is not None:
        sessions.revoke(s, p.auth_session)
        if p.user is not None:
            audit.record(s, "auth.logout", actor=p.user, ip=p.ip)
    sessions.clear_cookie(response)
    # Local mode would sign the owner straight back in on the next request; say so rather than
    # leaving them staring at a login page that will not appear.
    return {"ok": True, "local_mode": settings.get().local}


# ============================================================================ email verification

@router.get("/auth/verify")
def verify_email(token: str = "", s: Session = Depends(get_db)):
    """Where the link in the confirmation email lands."""
    try:
        user, _row = accounts.consume_email_token(s, token, "verify")
    except accounts.AccountError as exc:
        return RedirectResponse(settings.get().url(f"/#/login?error={_q(str(exc))}"), status_code=303)
    accounts.mark_verified(s, user)
    audit.record(s, "user.email_verified", actor=user, object_type="user", object_id=user.id)
    return RedirectResponse(settings.get().url("/#/login?verified=1"), status_code=303)


class ResendBody(BaseModel):
    """An address to resend a verification e-mail to."""
    email: str


@router.post("/api/auth/verify/resend")
async def resend_verification(body: ResendBody, request: Request, s: Session = Depends(get_db)):
    """Send another verification e-mail.

    Answers identically whether or not the address exists, because a different answer is a way to test
    whether somebody has an account here.
    """
    ip = ratelimit.client_ip(request)
    _limit("verify_resend", ip, s)
    user = accounts.by_email(s, body.email)
    # Same answer either way: this endpoint must not reveal which addresses are registered.
    generic = {"ok": True, "message": "If that address needs confirming, a new link is on its way."}
    if user is None or user.email_verified or user.status in ("suspended", "deleted"):
        return generic
    raw, url = accounts.issue_email_token(s, user, "verify")
    s.commit()
    try:
        await mailer.send_verification(user.email, user.name, url)
    except mailer.MailError as exc:
        log.error("resend failed: %s", exc)
    return generic


# ============================================================================ passwords

class ForgotBody(BaseModel):
    """An address to send a password reset to."""
    email: str


@router.post("/api/auth/password/forgot")
async def forgot_password(body: ForgotBody, request: Request, s: Session = Depends(get_db)):
    """Send a password reset link. Answers identically for unknown addresses."""
    ip = ratelimit.client_ip(request)
    _limit("reset_request", ip, s)
    user = accounts.by_email(s, body.email)
    generic = {"ok": True, "message": "If that address has an account, a reset link is on its way."}
    if user is None or user.status in ("suspended", "deleted") or not user.email:
        return generic
    _limit("reset_request", user.id, s)
    raw, url = accounts.issue_email_token(s, user, "reset")
    audit.record(s, "auth.reset_requested", actor=user, ip=ip)
    s.commit()
    try:
        await mailer.send_password_reset(user.email, user.name, url)
    except mailer.MailError as exc:
        log.error("reset mail failed: %s", exc)
    return generic


@router.get("/auth/reset")
def reset_landing(token: str = ""):
    """The link in the reset email. The token is not consumed here — the person still has to choose
    a password — so it is handed to the form that will spend it."""
    return RedirectResponse(settings.get().url(f"/#/reset?token={_q(token)}"), status_code=303)


class ResetBody(BaseModel):
    """A reset token and the new password."""
    token: str
    password: str


@router.post("/api/auth/password/reset")
async def reset_password(body: ResetBody, request: Request, s: Session = Depends(get_db)):
    """Set a new password from a reset link. The token in the link is the credential."""
    ip = ratelimit.client_ip(request)
    _limit("reset_attempt", ip, s)
    try:
        user, _row = accounts.consume_email_token(s, body.token, "reset")
        accounts.set_password(s, user, body.password, ip=ip)
    except (accounts.AccountError, passwords.PasswordError) as exc:
        raise _fail(exc)
    # A reset is also how an account recovers from never confirming its address.
    if not user.email_verified:
        accounts.mark_verified(s, user)
    email, name = user.email, user.name
    s.commit()
    if email:
        try:
            await mailer.send_security_notice(email, name, "Your password was changed",
                                              "It was changed using a reset link sent to this address.")
        except mailer.MailError:
            pass
    return {"ok": True, "message": "Password changed. You can sign in now."}


class ChangePasswordBody(BaseModel):
    """The current and new passwords."""
    current_password: str = ""
    new_password: str


@router.post("/api/auth/password/change")
async def change_password(body: ChangePasswordBody, request: Request,
                          p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Change a password, which requires the current one."""
    user = require_user(request, p)
    # An account that has only ever used SSO has no password to confirm; it is setting a first one,
    # and the live session is what authorizes that.
    if user.password_hash and not passwords.verify(user.password_hash, body.current_password):
        _limit("login_account", user.id, s)
        raise HTTPException(400, "That is not your current password.")
    try:
        accounts.set_password(s, user, body.new_password, ip=p.ip,
                              revoke_sessions=False if p.auth_session is None else True)
    except passwords.PasswordError as exc:
        raise _fail(exc)
    # Keep the session doing the changing; drop every other one.
    if p.auth_session is not None:
        accounts.revoke_all_sessions(s, user, keep=p.auth_session.id)
    email, name = user.email, user.name
    s.commit()
    if email:
        try:
            await mailer.send_security_notice(email, name, "Your password was changed",
                                              "Every other signed-in device has been logged out.")
        except mailer.MailError:
            pass
    return {"ok": True, "message": "Password changed. Other devices have been signed out."}


# ============================================================================ single sign-on

@router.get("/api/auth/oauth/{provider}/start")
def oauth_start(provider: str, request: Request, next: str = "/", invite: str = "",
                link: bool = False, tz: str = "", p: Principal = Depends(principal)):
    """Begin sign-in with a provider."""
    ip = ratelimit.client_ip(request)
    _limit("oauth_start", ip)
    try:
        link_to = p.user.id if (link and p.user is not None) else None
        url, state = oauth.begin(provider, next_url=next, invite=invite or None,
                                 link_to_user=link_to, tz=tz or None)
    except oauth.OAuthError as exc:
        raise _fail(exc)
    response = RedirectResponse(url, status_code=303)
    oauth.set_state_cookie(response, state)
    return response


@router.get("/api/auth/oauth/{provider}/callback")
async def oauth_callback(provider: str, request: Request, code: str = "", state: str = "",
                         error: str = "", error_description: str = "", s: Session = Depends(get_db)):
    """Return from a provider, sign the person in, and send them where they were going."""
    ip = ratelimit.client_ip(request)

    def bounce(message: str, path: str = "/#/login"):
        """Send the browser back to the client with a message, rather than showing a bare error."""
        response = RedirectResponse(settings.get().url(f"{path}?error={_q(message)}"), status_code=303)
        oauth.clear_state_cookie(response)
        return response

    if error:
        log.info("oauth %s returned %s: %s", provider, error, error_description)
        return bounce("Sign-in was cancelled." if error == "access_denied"
                      else f"{provider.title()} refused the sign-in.")
    try:
        carried = oauth.read_state(request.cookies.get(oauth.STATE_COOKIE), state)
        if carried.get("p") != provider:
            raise oauth.OAuthError("That sign-in attempt could not be verified. Start again.")
        access_token = await oauth.exchange(provider, code, verifier=carried.get("v"))
        profile = await oauth.fetch_profile(provider, access_token)
    except oauth.OAuthError as exc:
        return bounce(str(exc))
    except Exception:
        log.exception("oauth callback failed for %s", provider)
        return bounce("That sign-in could not be completed. Please try again.")

    next_url = carried.get("n", "/")
    tz = _clean_tz(carried.get("z"))

    try:
        user, created = _resolve_oauth_user(s, provider, profile, carried, ip=ip, tz=tz)
    except accounts.AccountError as exc:
        return bounce(str(exc))
    except invites.InviteError as exc:
        return bounce(str(exc))

    if carried.get("u"):
        # Linking a second provider to an account that is already signed in.
        response = RedirectResponse(settings.get().url("/#/account?linked=" + provider), status_code=303)
        oauth.clear_state_cookie(response)
        return response

    row, raw = sessions.start(s, user, ip=ip, user_agent=_ua(request))
    audit.record(s, "auth.login", actor=user, ip=ip, method=provider, new_account=created)
    response = RedirectResponse(settings.get().url(next_url if next_url != "/" else "/#/"), status_code=303)
    sessions.set_cookie(response, raw)
    oauth.clear_state_cookie(response)
    return response


def _resolve_oauth_user(s: Session, provider: str, profile: dict, carried: dict, *,
                        ip: str, tz: str):
    """Find, link or create the account behind an SSO login. Returns (user, created)."""
    provider_user_id = profile.get("provider_user_id")
    if not provider_user_id:
        raise accounts.AccountError(f"{provider.title()} did not identify the account.")

    email = profile.get("email")
    email_verified = bool(profile.get("email_verified"))
    normalized_email = accounts.normalize_email(email) if email else None

    # 1. This provider account is already linked.
    identity = accounts.identity_for(s, provider, provider_user_id)
    if identity is not None:
        user = s.get(User, identity.user_id)
        if user is None or user.status == "deleted":
            raise accounts.AccountError("That account no longer exists.")
        if user.status == "suspended":
            raise accounts.AccountError("This account has been suspended.")
        accounts.link_identity(s, user, provider=provider, provider_user_id=provider_user_id,
                               email=normalized_email, email_verified=email_verified,
                               display_name=profile.get("display_name"),
                               avatar_url=profile.get("avatar_url"))
        return user, False

    # 2. Somebody signed in is adding this provider to their account.
    if carried.get("u"):
        user = s.get(User, carried["u"])
        if user is None:
            raise accounts.AccountError("That account no longer exists.")
        accounts.link_identity(s, user, provider=provider, provider_user_id=provider_user_id,
                               email=normalized_email, email_verified=email_verified,
                               display_name=profile.get("display_name"),
                               avatar_url=profile.get("avatar_url"))
        audit.record(s, "user.identity_linked", actor=user, object_type="user", object_id=user.id,
                     ip=ip, provider=provider)
        return user, False

    # 3. An existing account with the same address — link only if the provider vouched for it.
    #    Without that check, registering an address at a sloppy provider would hand over the
    #    matching CITAR account.
    if normalized_email and email_verified:
        existing = accounts.by_email(s, normalized_email)
        if existing is not None and existing.status != "deleted":
            if existing.status == "suspended":
                raise accounts.AccountError("This account has been suspended.")
            accounts.link_identity(s, existing, provider=provider, provider_user_id=provider_user_id,
                                   email=normalized_email, email_verified=True,
                                   display_name=profile.get("display_name"),
                                   avatar_url=profile.get("avatar_url"))
            audit.record(s, "user.identity_linked", actor=existing, object_type="user",
                         object_id=existing.id, ip=ip, provider=provider, matched_by="verified_email")
            return existing, False
    elif normalized_email and accounts.by_email(s, normalized_email) is not None:
        raise accounts.AccountError(
            f"An account already uses {normalized_email}, and {provider.title()} did not confirm that "
            "address belongs to you. Sign in with your password and link the account from your "
            "account page instead.")

    # 4. A new account.
    mode = policy.registration_mode()
    invite = None
    if carried.get("i"):
        invite = invites.check(s, carried["i"], email=normalized_email)
    elif mode != "open":
        raise accounts.AccountError(policy.get("closed_message") or
                                    "This server is invite-only. Ask an administrator for an invitation.")

    handle = accounts.suggest_handle(s, profile.get("handle_hint") or
                                     (normalized_email or "player").split("@")[0])
    skip_probation = bool(invite and invite.skip_probation) or \
        (policy.get("probation_skips_sso") and email_verified)
    user = accounts.create_user(
        s, handle=handle, email=normalized_email, display_name=profile.get("display_name") or handle,
        tz=tz, email_verified=email_verified, avatar_url=profile.get("avatar_url"),
        skip_probation=skip_probation,
        # An SSO account with a provider-verified address does not need to confirm it again.
        status=None if email_verified or not normalized_email else "probation")
    accounts.link_identity(s, user, provider=provider, provider_user_id=provider_user_id,
                           email=normalized_email, email_verified=email_verified,
                           display_name=profile.get("display_name"),
                           avatar_url=profile.get("avatar_url"))
    if invite is not None:
        invites.redeem(s, invite, user, ip=ip)
    audit.record(s, "user.signup", actor=user, object_type="user", object_id=user.id, ip=ip,
                 method=provider, invited=bool(invite))
    return user, True


@router.post("/api/auth/oauth/{provider}/unlink")
def oauth_unlink(provider: str, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Disconnect a sign-in provider from this account."""
    user = require_user(request, p)
    try:
        accounts.unlink_identity(s, user, provider)
    except accounts.AccountError as exc:
        raise _fail(exc)
    audit.record(s, "user.identity_unlinked", actor=user, object_type="user", object_id=user.id,
                 ip=p.ip, provider=provider)
    return {"ok": True, "providers": [i.provider for i in user.identities]}


# ============================================================================ the account itself

class ProfileBody(BaseModel):
    """The editable parts of a profile."""
    display_name: Optional[str] = None
    tz: Optional[str] = None
    data_sharing: Optional[str] = None
    locale: Optional[str] = None


@router.put("/api/auth/me")
def update_profile(body: ProfileBody, request: Request, p: Principal = Depends(principal),
                   s: Session = Depends(get_db)):
    """Change the caller's own profile."""
    user = require_user(request, p)
    if body.display_name is not None:
        user.display_name = body.display_name.strip()[:80]
    if body.tz is not None:
        user.tz = _clean_tz(body.tz)
    if body.locale is not None:
        user.locale = body.locale.strip()[:16] or "en"
    if body.data_sharing is not None:
        if body.data_sharing not in ("pool", "private"):
            raise HTTPException(400, "data_sharing is 'pool' or 'private'.")
        if body.data_sharing != user.data_sharing:
            audit.record(s, "user.data_sharing", actor=user, object_type="user", object_id=user.id,
                         ip=p.ip, to=body.data_sharing)
        user.data_sharing = body.data_sharing
    return {"ok": True, "user": user.public(full=True)}


@router.get("/api/auth/sessions")
def list_sessions(p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Every session this account has open, with where and when each began."""
    user = p.user
    if user is None:
        raise HTTPException(401, "Sign in to do that.")
    return {"sessions": sessions.list_for(s, user,
                                          current_id=p.auth_session.id if p.auth_session else None)}


@router.delete("/api/auth/sessions/{session_id}")
def revoke_session(session_id: str, request: Request, p: Principal = Depends(principal),
                   s: Session = Depends(get_db)):
    """End one session - how somebody signs out a device they no longer have."""
    user = require_user(request, p)
    if not sessions.revoke_by_id(s, user, session_id):
        raise HTTPException(404, "No such session.")
    audit.record(s, "auth.session_revoked", actor=user, ip=p.ip, object_type="session",
                 object_id=session_id)
    return {"ok": True}


@router.post("/api/auth/sessions/revoke-others")
def revoke_other_sessions(request: Request, p: Principal = Depends(principal),
                          s: Session = Depends(get_db)):
    """End every session except this one."""
    user = require_user(request, p)
    count = accounts.revoke_all_sessions(s, user, keep=p.auth_session.id if p.auth_session else None)
    audit.record(s, "auth.sessions_revoked", actor=user, ip=p.ip, count=count)
    return {"ok": True, "revoked": count}


# ============================================================================ invitations

@router.get("/api/invites/check")
def check_invite(code: str, s: Session = Depends(get_db)):
    """Lets the signup form say 'you have been invited as a moderator' before anything is created."""
    try:
        invite = invites.check(s, code)
    except invites.InviteError as exc:
        raise _fail(exc)
    creator = accounts.by_id(s, invite.created_by)
    return {"ok": True, "role": invite.role, "email": invite.email, "note": invite.note,
            "invited_by": creator.name if creator else None}


@router.get("/api/invites")
def list_invites(include_spent: bool = False, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """The caller's invitations, and how many they may still send."""
    if p.user is None:
        raise HTTPException(401, "Sign in to do that.")
    return {"invites": invites.list_for(s, p.user, include_spent=include_spent),
            "quota": p.user.invite_quota, "can_invite": policy.can(p.user, "invite")}


class InviteBody(BaseModel):
    """An invitation: the role, how many uses, when it expires, and who to send it to."""
    role: str = "user"
    email: Optional[str] = None
    max_uses: int = 1
    expires_days: Optional[int] = invites.DEFAULT_EXPIRY_DAYS
    note: str = ""
    send_email: bool = False


@router.post("/api/invites")
async def create_invite(body: InviteBody, request: Request, p: Principal = Depends(principal),
                        s: Session = Depends(get_db)):
    """Mint an invitation code."""
    user = require_user(request, p)
    try:
        invite = invites.create(s, user, role=body.role, email=body.email, max_uses=body.max_uses,
                                expires_days=body.expires_days, note=body.note, ip=p.ip)
    except (invites.InviteError, accounts.AccountError) as exc:
        raise _fail(exc)
    payload = invites.to_client(s, invite)
    s.commit()

    if body.send_email and body.email:
        if not settings.get().email_enabled:
            payload["warning"] = "This server cannot send email — copy the link and send it yourself."
        else:
            try:
                await mailer.send_invite(invite.email, user.name, payload["url"], invite.note or "")
                payload["emailed"] = True
            except mailer.MailError as exc:
                payload["warning"] = f"The invitation was created but could not be emailed: {exc}"
    return payload


@router.delete("/api/invites/{invite_id}")
def revoke_invite(invite_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db)):
    """Revoke an unused invitation."""
    user = require_user(request, p)
    invite = s.get(Invite, invite_id)
    if invite is None:
        raise HTTPException(404, "No such invitation.")
    try:
        invites.revoke(s, invite, user, ip=p.ip)
    except invites.InviteError as exc:
        raise _fail(exc)
    return {"ok": True}


# ============================================================================ helpers

def _clean_tz(tz: Optional[str]) -> str:
    """Accept only a real IANA zone. The browser supplies this, so it is untrusted input, and it
    ends up in scheduling arithmetic where a bad value would throw on every evaluation."""
    candidate = (tz or "").strip()
    if not candidate:
        return "UTC"
    try:
        from zoneinfo import ZoneInfo
        ZoneInfo(candidate)
        return candidate
    except Exception:
        return "UTC"


def _q(text: str) -> str:
    """Escape text for inclusion in an HTML response."""
    from urllib.parse import quote
    return quote(text or "", safe="")
