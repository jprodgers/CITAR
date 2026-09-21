"""hCaptcha verification for the open signup path.

The widget in the browser produces a response token; this module asks hCaptcha whether that token is
real, unused, and issued for our site. Skipping the server-side call and trusting the widget is the
usual mistake and is worth nothing — a script never runs the widget at all, it just posts the form.

Failure policy is fail-closed: if hCaptcha cannot be reached, signups are refused rather than waved
through, because "the captcha provider is down" is indistinguishable from "the attacker is blocking
the captcha provider". Signup is not an operation that has to stay available for the sixty seconds
an outage lasts, and invite links keep working regardless.

When no keys are configured the check is skipped entirely and the server warns at boot — that is the
documented state for a private or invite-only deployment, not a silent hole.
"""
from __future__ import annotations

import logging
from typing import Optional

import httpx

from .. import settings

log = logging.getLogger("citar.captcha")

VERIFY_URL = "https://api.hcaptcha.com/siteverify"
TIMEOUT = 10.0

#: hCaptcha's codes are aimed at developers; these are what the person filling in the form sees.
_MESSAGES = {
    "missing-input-response": "Complete the anti-bot check to continue.",
    "invalid-input-response": "The anti-bot check did not come back valid. Try it again.",
    "expired-input-response": "The anti-bot check expired. Complete it again.",
    "already-seen-response": "That anti-bot check was already used. Complete it again.",
    "bad-request": "The anti-bot check was rejected. Reload the page and try again.",
    "sitekey-secret-mismatch": "The anti-bot check is misconfigured on this server.",
    "missing-input-secret": "The anti-bot check is misconfigured on this server.",
    "invalid-input-secret": "The anti-bot check is misconfigured on this server.",
}


class CaptchaError(ValueError):
    """The check did not pass. The message is safe to show."""


def enabled() -> bool:
    """Whether a captcha is configured and will actually be verified.

    Both halves matter: a site key with no secret produces a widget that is never checked, which is
    worse than no captcha because it looks like protection.
    """
    return settings.get().captcha_enabled


def site_key() -> str:
    """What the login page needs to render the widget. Public by design."""
    return settings.get().hcaptcha_site_key


async def verify(token: Optional[str], *, remote_ip: Optional[str] = None) -> None:
    """Raise CaptchaError unless this is a genuine, unused response for our site."""
    cfg = settings.get()
    if not cfg.captcha_enabled:
        return
    if not token:
        raise CaptchaError(_MESSAGES["missing-input-response"])

    data = {"secret": cfg.hcaptcha_secret, "response": token}
    if remote_ip and remote_ip != "unknown":
        data["remoteip"] = remote_ip
    if cfg.hcaptcha_site_key:
        # Lets hCaptcha reject a token that was solved against somebody else's site key.
        data["sitekey"] = cfg.hcaptcha_site_key

    try:
        async with httpx.AsyncClient(timeout=TIMEOUT) as client:
            response = await client.post(VERIFY_URL, data=data)
            response.raise_for_status()
            result = response.json()
    except Exception as exc:
        log.warning("hCaptcha verification could not be completed: %s", exc)
        raise CaptchaError("The anti-bot check could not be completed right now. "
                           "Please try again in a moment.") from exc

    if result.get("success"):
        return

    codes = result.get("error-codes") or []
    log.info("hCaptcha rejected a response: %s", codes)
    for code in codes:
        if code in _MESSAGES:
            raise CaptchaError(_MESSAGES[code])
    raise CaptchaError("The anti-bot check did not pass. Try it again.")


def client_config() -> dict:
    """What /api/auth/config tells the browser."""
    return {"provider": "hcaptcha" if enabled() else None, "site_key": site_key() if enabled() else ""}
