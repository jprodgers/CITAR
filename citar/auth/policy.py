"""Server policy: the settings an admin changes at runtime, and what each role may do by default.

citar/settings.py holds what the *process* needs to boot — ports, keys, credentials — and only
changes on a restart. This module holds what the *operator* decides and can change from the admin
console while the server is running: whether registration is open, whether new accounts start on
probation, and what each role is allowed to do.

Capability resolution, most specific first:

    1. an explicit override on the account            (User.caps, set by an admin)
    2. the role default for that account's role       (this module, editable in the console)
    3. False

Suspended and pending accounts get nothing regardless, which is checked before any of the above —
otherwise an admin override could keep a suspended account working.
"""
from __future__ import annotations

import threading
from typing import Any, Optional

from .. import db, settings
from ..db.models import CAPABILITIES, Setting, User

#: Defaults per role. `user` is deliberately allowed to register servers and run reports: sharing
#: hardware and analysing results is the point of the site, not a privilege. What `user` cannot do
#: by default is publish to the whole internet, which is the one action with consequences outside
#: this server.
ROLE_DEFAULTS: dict = {
    "user": {
        "create_games": True,
        "register_servers": True,
        "run_reports": True,
        "publish_public": False,
        "invite": False,
    },
    "moderator": {
        "create_games": True,
        "register_servers": True,
        "run_reports": True,
        "publish_public": True,
        "invite": True,
    },
    "admin": dict.fromkeys(CAPABILITIES, True),
}

#: Probation is the holding state for a brand-new account: it can play and watch but cannot add
#: hardware, invite anyone or publish. These are the capabilities it loses until it is cleared.
PROBATION_DENIES = ("register_servers", "invite", "publish_public")

DEFAULTS: dict = {
    # open | invite | closed. Boots from CITAR_REGISTRATION, then lives here.
    "registration": None,
    # Whether verified email/password signups start on probation. SSO signups from a provider that
    # asserts a verified email skip it when `probation_skips_sso` is on.
    "probation_enabled": True,
    "probation_skips_sso": True,
    # New accounts may be handed an invite quota so the community can grow without an admin.
    "default_invite_quota": 0,
    "default_max_concurrent_games": 2,
    # Minimum distinct contributors before a pooled average is shown, so a public number can never
    # be read back as one private account's results.
    "aggregate_min_contributors": 3,
    "role_defaults": ROLE_DEFAULTS,
    "welcome_message": "",
    # Set once the first-run wizard has been finished or dismissed. Without it, somebody who chose
    # to play against the scripted bots would be offered setup on every page load forever, because
    # the test it would otherwise use ("is a model configured?") still answers no.
    "setup_dismissed": False,
    # Shown on the login page when registration is closed or invite-only.
    "closed_message": "CITAR is currently invite-only.",
}

_lock = threading.RLock()
_cache: dict = {}
_loaded = False


def _load() -> dict:
    """Read policy from the database, falling back to defaults before migrations have run."""
    global _loaded
    with _lock:
        if _loaded:
            return _cache
        values = dict(DEFAULTS)
        try:
            with db.session() as s:
                for row in s.query(Setting).all():
                    values[row.key] = row.value.get("v") if isinstance(row.value, dict) else row.value
        except Exception:
            # First boot, before migrations have run. Defaults are correct until the table exists.
            pass
        if values.get("registration") is None:
            values["registration"] = settings.get().registration
        _cache.clear()
        _cache.update(values)
        _loaded = True
        return _cache


def get(key: str, default: Any = None) -> Any:
    """One policy value."""
    return _load().get(key, DEFAULTS.get(key, default))


def all_settings() -> dict:
    """Every policy value."""
    return dict(_load())


def set(key: str, value: Any, *, session=None, actor: Optional[User] = None) -> Any:
    """Persist one policy value. Pass a session to change it in the same unit of work as an audit
    entry."""
    if key not in DEFAULTS:
        raise KeyError(f"Unknown policy setting {key!r}.")

    def _write(s):
        """Write the value, with the actor recorded."""
        row = s.get(Setting, key)
        if row is None:
            row = Setting(key=key, value={"v": value})
            s.add(row)
        else:
            row.value = {"v": value}
        if actor is not None:
            row.updated_by = actor.id

    if session is not None:
        _write(session)
    else:
        with db.session() as s:
            _write(s)
    with _lock:
        _cache[key] = value
    return value


def invalidate():
    """Drop the cache; the next read reloads from the database."""
    global _loaded
    with _lock:
        _loaded = False
        _cache.clear()


# ---------------------------------------------------------------------------- capabilities

def registration_mode() -> str:
    """Who may sign up: open, invite or closed."""
    return get("registration") or settings.get().registration


def can(user: Optional[User], capability: str) -> bool:
    """Whether this account may do `capability` right now."""
    if user is None:
        return False
    if capability not in CAPABILITIES:
        raise KeyError(f"Unknown capability {capability!r}.")
    # Checked before overrides on purpose: a suspended account stays stopped whatever else is set.
    if user.status in ("suspended", "deleted", "pending"):
        return False
    if user.role == "admin":
        return True
    if user.status == "probation" and capability in PROBATION_DENIES:
        return False
    overrides = user.caps or {}
    if capability in overrides:
        return bool(overrides[capability])
    role_defaults = get("role_defaults") or ROLE_DEFAULTS
    return bool((role_defaults.get(user.role) or {}).get(capability, False))


def capabilities(user: Optional[User]) -> dict:
    """Every capability resolved, for the browser to enable and disable controls with.

    The server still enforces each one — this is so the UI does not offer a button that will fail.
    """
    return {cap: can(user, cap) for cap in CAPABILITIES}


def client_policy() -> dict:
    """The parts of policy the login page needs before anyone has signed in."""
    cfg = settings.get()
    mode = registration_mode()
    return {
        "registration": mode,
        "closed_message": get("closed_message") if mode != "open" else "",
        "welcome_message": get("welcome_message"),
        "email_enabled": cfg.email_enabled,
        "providers": cfg.enabled_oauth_providers,
    }
