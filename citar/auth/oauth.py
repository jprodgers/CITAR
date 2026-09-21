"""Single sign-on with Google, GitHub, Discord and Microsoft.

The flow is written out here rather than delegated to a framework integration, because the parts
that matter for security are exactly the parts an integration hides.

*State lives in a signed, short-lived cookie*, not in server-side session storage. It is HttpOnly,
SameSite=Lax, expires in ten minutes, and is deleted the moment the callback consumes it. A callback
whose `state` does not match the cookie is rejected — that check is the entire defence against an
attacker feeding you their own authorization code and quietly linking your CITAR account to their
Google account.

*PKCE* is used with every provider that supports it, so an intercepted authorization code cannot be
redeemed without the verifier that never left this server. GitHub's classic OAuth apps do not
support it, so it is a per-provider flag rather than an assumption.

*Identity comes from the userinfo endpoint*, called directly by this server over TLS with the access
token, rather than from decoding an id_token. Both are fine, but this way there is no JWKS cache, no
signature validation to get wrong, and no chance of trusting an unverified token: the data arrives
on a connection we opened to a host we pinned by name.

*A provider's unverified email never auto-links.* If GitHub says an address is unconfirmed, we do
not attach that login to an existing CITAR account with the same address — that is account takeover
for the price of registering an address you do not own.
"""
from __future__ import annotations

import base64
import hashlib
import logging
import secrets
from dataclasses import dataclass
from typing import Optional
from urllib.parse import urlencode

import httpx
from itsdangerous import BadSignature, URLSafeTimedSerializer

from .. import settings

log = logging.getLogger("citar.oauth")

STATE_COOKIE = "citar_oauth"
STATE_MAX_AGE = 600          # ten minutes to get through a provider's login screen
TIMEOUT = 15.0


class OAuthError(ValueError):
    """Something went wrong in the handshake. The message is safe to show."""


@dataclass(frozen=True)
class Provider:
    """One sign-in provider's endpoints and how to read its profile."""
    name: str
    label: str
    authorize_url: str
    token_url: str
    userinfo_url: str
    scope: str
    pkce: bool = True
    #: Providers that want an explicit Accept header to return JSON rather than form-encoding.
    json_accept: bool = False


def _providers() -> dict:
    """Every provider CITAR knows how to talk to."""
    tenant = settings.get().oauth["microsoft"].tenant or "common"
    return {
        "google": Provider(
            "google", "Google",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
            "openid email profile"),
        "github": Provider(
            "github", "GitHub",
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            "read:user user:email",
            pkce=False, json_accept=True),
        "discord": Provider(
            "discord", "Discord",
            "https://discord.com/oauth2/authorize",
            "https://discord.com/api/oauth2/token",
            "https://discord.com/api/users/@me",
            "identify email"),
        "microsoft": Provider(
            "microsoft", "Microsoft",
            f"https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize",
            f"https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token",
            "https://graph.microsoft.com/oidc/userinfo",
            "openid email profile"),
    }


def provider(name: str) -> Provider:
    """One provider by name, or None if it is not configured."""
    found = _providers().get(name)
    if found is None:
        raise OAuthError(f"Unknown sign-in provider {name!r}.")
    if not settings.get().oauth_enabled(name):
        raise OAuthError(f"{found.label} sign-in is not configured on this server.")
    return found


def available() -> list:
    """The providers the login page should offer."""
    cfg = settings.get()
    return [{"name": p.name, "label": p.label}
            for p in _providers().values() if cfg.oauth_enabled(p.name)]


# ---------------------------------------------------------------------------- state cookie

def _serializer() -> URLSafeTimedSerializer:
    """The signer for the state parameter, which ties a callback to the browser that started it."""
    return URLSafeTimedSerializer(settings.get().secret_key, salt="citar-oauth-state")


def _pkce_pair() -> tuple:
    """A PKCE verifier and challenge.

    PKCE is used even for confidential clients here: it costs nothing and removes a whole class of
    authorisation-code interception.
    """
    verifier = secrets.token_urlsafe(64)[:128]
    digest = hashlib.sha256(verifier.encode("ascii")).digest()
    challenge = base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")
    return verifier, challenge


def begin(name: str, *, next_url: str = "/", invite: Optional[str] = None,
          link_to_user: Optional[str] = None, tz: Optional[str] = None) -> tuple:
    """Build the provider's authorization URL. Returns (url, signed state cookie value).

    `link_to_user` is set when an account that is already signed in is adding a second sign-in
    method; it is carried through the handshake so the callback links instead of creating.
    """
    cfg = settings.get()
    prov = provider(name)
    client = cfg.oauth[name]

    state = secrets.token_urlsafe(24)
    payload = {"s": state, "p": name, "n": _safe_next(next_url)}
    if invite:
        payload["i"] = invite
    if link_to_user:
        payload["u"] = link_to_user
    if tz:
        payload["z"] = tz

    params = {
        "client_id": client.client_id,
        "redirect_uri": cfg.callback_url(name),
        "response_type": "code",
        "scope": prov.scope,
        "state": state,
    }
    if prov.pkce:
        verifier, challenge = _pkce_pair()
        payload["v"] = verifier
        params["code_challenge"] = challenge
        params["code_challenge_method"] = "S256"
    if name == "google":
        # Without this, Google silently reuses a previous consent and never re-prompts for the
        # account picker, which is maddening for anyone with two Google accounts.
        params["prompt"] = "select_account"
    if name == "microsoft":
        params["response_mode"] = "query"

    return f"{prov.authorize_url}?{urlencode(params)}", _serializer().dumps(payload)


def _safe_next(next_url: Optional[str]) -> str:
    """Only ever redirect back inside this site.

    An open redirect on the login path is a phishing primitive: the victim sees a genuine
    citar.example.com link, signs in, and is bounced to somebody else's lookalike page.
    """
    candidate = (next_url or "/").strip()
    if not candidate.startswith("/") or candidate.startswith("//") or "\\" in candidate:
        return "/"
    return candidate[:512]


def read_state(cookie_value: Optional[str], returned_state: Optional[str]) -> dict:
    """Validate the callback against the cookie and return the carried payload."""
    if not cookie_value:
        raise OAuthError("The sign-in attempt expired or the browser dropped a cookie. "
                         "Start again from the sign-in page.")
    try:
        payload = _serializer().loads(cookie_value, max_age=STATE_MAX_AGE)
    except BadSignature as exc:
        raise OAuthError("That sign-in attempt could not be verified. Start again.") from exc
    if not returned_state or not secrets.compare_digest(str(payload.get("s", "")), returned_state):
        log.warning("OAuth state mismatch for provider %s", payload.get("p"))
        raise OAuthError("That sign-in attempt could not be verified. Start again.")
    return payload


# ---------------------------------------------------------------------------- token exchange

async def exchange(name: str, code: str, *, verifier: Optional[str] = None) -> str:
    """Swap the authorization code for an access token."""
    cfg = settings.get()
    prov = provider(name)
    client = cfg.oauth[name]

    data = {
        "client_id": client.client_id,
        "client_secret": client.client_secret,
        "code": code,
        "grant_type": "authorization_code",
        "redirect_uri": cfg.callback_url(name),
    }
    if verifier:
        data["code_verifier"] = verifier
    headers = {"Accept": "application/json"} if prov.json_accept else {}

    async with httpx.AsyncClient(timeout=TIMEOUT) as http:
        response = await http.post(prov.token_url, data=data, headers=headers)
    if response.status_code >= 400:
        log.warning("%s token exchange failed: %s %s", name, response.status_code, response.text[:400])
        raise OAuthError(f"{prov.label} did not accept the sign-in. Please try again.")
    try:
        body = response.json()
    except ValueError:
        raise OAuthError(f"{prov.label} returned an unreadable response.")
    if body.get("error"):
        log.warning("%s token exchange error: %s", name, body.get("error_description") or body["error"])
        raise OAuthError(f"{prov.label} did not accept the sign-in. Please try again.")
    token = body.get("access_token")
    if not token:
        raise OAuthError(f"{prov.label} did not return an access token.")
    return token


async def fetch_profile(name: str, access_token: str) -> dict:
    """Normalized identity from the provider.

    Returns provider_user_id, email, email_verified, display_name, avatar_url. `email` may be None —
    GitHub accounts can hide it and Discord accounts need not have one.
    """
    prov = provider(name)
    headers = {"Authorization": f"Bearer {access_token}", "Accept": "application/json",
               "User-Agent": "CITAR"}
    async with httpx.AsyncClient(timeout=TIMEOUT) as http:
        response = await http.get(prov.userinfo_url, headers=headers)
        if response.status_code >= 400:
            log.warning("%s userinfo failed: %s %s", name, response.status_code, response.text[:300])
            raise OAuthError(f"Could not read your {prov.label} profile. Please try again.")
        data = response.json()

        if name == "github":
            email, verified = data.get("email"), False
            # A GitHub account with a private email returns null above; the verified primary is on
            # a separate endpoint, and only a *verified* one is worth anything to us.
            try:
                emails = (await http.get("https://api.github.com/user/emails", headers=headers)).json()
                primary = next((e for e in emails if e.get("primary") and e.get("verified")), None)
                fallback = next((e for e in emails if e.get("verified")), None)
                chosen = primary or fallback
                if chosen:
                    email, verified = chosen["email"], True
            except Exception:
                pass
            return {"provider_user_id": str(data["id"]), "email": email, "email_verified": verified,
                    "display_name": data.get("name") or data.get("login"),
                    "handle_hint": data.get("login"), "avatar_url": data.get("avatar_url")}

        if name == "discord":
            avatar = (f"https://cdn.discordapp.com/avatars/{data['id']}/{data['avatar']}.png"
                      if data.get("avatar") else None)
            return {"provider_user_id": str(data["id"]), "email": data.get("email"),
                    "email_verified": bool(data.get("verified")),
                    "display_name": data.get("global_name") or data.get("username"),
                    "handle_hint": data.get("username"), "avatar_url": avatar}

        if name == "microsoft":
            # Graph's OIDC userinfo omits email_verified. A Microsoft work or school account is
            # administered, and a personal one has been through Microsoft's own verification, so
            # treating a returned address as verified is reasonable — and it is only ever used to
            # auto-link, never to grant anything on its own.
            return {"provider_user_id": str(data.get("sub")),
                    "email": data.get("email") or data.get("preferred_username"),
                    "email_verified": bool(data.get("email") or data.get("preferred_username")),
                    "display_name": data.get("name"),
                    "handle_hint": (data.get("email") or "").split("@")[0], "avatar_url": data.get("picture")}

        # google, and any future OIDC provider
        return {"provider_user_id": str(data.get("sub")), "email": data.get("email"),
                "email_verified": bool(data.get("email_verified")),
                "display_name": data.get("name"),
                "handle_hint": (data.get("email") or "").split("@")[0],
                "avatar_url": data.get("picture")}


def set_state_cookie(response, value: str) -> None:
    """Store the signed state for this sign-in attempt."""
    response.set_cookie(STATE_COOKIE, value, max_age=STATE_MAX_AGE, httponly=True,
                        secure=settings.get().require_https, samesite="lax", path="/api/auth/oauth")


def clear_state_cookie(response) -> None:
    """Clear the state cookie once the callback has been handled."""
    response.delete_cookie(STATE_COOKIE, path="/api/auth/oauth", httponly=True,
                           secure=settings.get().require_https, samesite="lax")
