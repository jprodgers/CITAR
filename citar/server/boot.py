"""Startup: bring the database up to date and refuse to run a half-configured server.

Migrations are applied by the process itself rather than by a deploy step. A deployment that starts
with a stale schema fails in scattered, confusing ways much later; applying them here means the
server is either correct or refuses to start, and a restart is the whole upgrade procedure.

The boot checks are deliberately loud. Every one of them is a configuration mistake that would
otherwise look like the site working fine right up until somebody needs the broken part — mail that
silently goes nowhere, a captcha that is not actually checked, an admin account that does not exist.
"""
from __future__ import annotations

import logging
from pathlib import Path
from typing import Optional

from sqlalchemy import func, inspect, select

from .. import db, paths, settings
from ..auth import accounts, policy
from ..db.models import User

log = logging.getLogger("citar.boot")
ROOT = Path(__file__).resolve().parent.parent.parent


class BootError(RuntimeError):
    """The server must not start."""


def migrate() -> None:
    """Bring the schema to head, creating it if this is the first run."""
    from alembic import command
    from alembic.config import Config

    # Built in code rather than read from alembic.ini: the .ini is a development convenience for
    # the command line, and a wheel should not have to ship one outside the package.
    cfg = Config()
    cfg.set_main_option("script_location", str(paths.migrations_dir()))
    cfg.set_main_option("file_template", "%%(year)d%%(month).2d%%(day).2d_%%(hour).2d%%(minute).2d_%%(slug)s")
    cfg.set_main_option("sqlalchemy.url", settings.get().db_url)
    cfg.attributes["configure_logger"] = False

    engine = db.engine()
    inspector = inspect(engine)
    tables = set(inspector.get_table_names())

    if not tables:
        log.info("Empty database — creating the schema.")
        command.upgrade(cfg, "head")
    elif "alembic_version" not in tables:
        # A database created by create_all() (tests, or an early build) already has the tables but
        # no migration history. Stamp it rather than replaying migrations that would collide.
        log.info("Existing tables with no migration history — stamping at head.")
        db.create_all()
        command.stamp(cfg, "head")
    else:
        command.upgrade(cfg, "head")
    log.info("Database schema is up to date.")


def check_configuration() -> list:
    """Warnings worth printing at boot. Returns them; raises only for the fatal ones."""
    cfg = settings.get()
    warnings: list = []

    if cfg.server_mode:
        if not cfg.public_origin.startswith("https://") and cfg.require_https:
            raise BootError("CITAR_PUBLIC_ORIGIN must be https:// in server mode.")
        if not cfg.email_enabled:
            warnings.append(
                "No SMTP configured: email sign-up, verification and password reset are unavailable. "
                "Sign-in providers still work. Set CITAR_SMTP_HOST and CITAR_MAIL_FROM to enable them.")
        if not cfg.enabled_oauth_providers and not cfg.email_enabled:
            # Not fatal: an administrator created with admin_cli has a password and a pre-verified
            # address, and can sign in with no mail server and no OAuth provider at all. That is
            # exactly how a new deployment bootstraps itself, so refusing to start here would block
            # the normal first-run path.
            if _has_password_account():
                warnings.append(
                    "No OAuth provider and no SMTP server are configured, so the only way in is an "
                    "existing password account. Nobody can register or reset a password until you "
                    "add one of them.")
            else:
                # Deliberately not fatal. Refusing to boot here means a restart loop with the
                # service down and — because the logs are the first thing to go — no clear reason
                # why. A running server with no accounts is inert but harmless, and it can say what
                # to do about it. Failing loudly beats failing obscurely.
                warnings.append(
                    "NOBODY CAN SIGN IN YET: no OAuth provider, no SMTP server, and no account "
                    "with a password. Create an administrator:\n"
                    "         sudo -u citar /opt/citar/.venv/bin/python -m citar.server.admin_cli \\\n"
                    "              create-admin --handle NAME --email you@example.com\n"
                    "         (run it from /opt/citar with the service's environment loaded)")
        if policy.registration_mode() == "open" and not cfg.captcha_enabled:
            warnings.append(
                "Registration is OPEN but hCaptcha is not configured — the only protection against "
                "automated signups is rate limiting and email verification. Set "
                "CITAR_HCAPTCHA_SITE_KEY and CITAR_HCAPTCHA_SECRET, or set CITAR_REGISTRATION=invite.")
        if not cfg.behind_proxy:
            warnings.append(
                "CITAR_BEHIND_PROXY is off: every request will look like it came from the proxy, so "
                "per-IP rate limiting will treat the whole internet as one client.")
        if cfg.host in ("0.0.0.0", "::"):
            warnings.append(
                f"Listening on {cfg.host} exposes CITAR directly. Bind to 127.0.0.1 and let the "
                "reverse proxy terminate TLS.")
    else:
        if cfg.host not in ("127.0.0.1", "localhost", "::1"):
            warnings.append(
                f"Local mode is bound to {cfg.host}, not loopback. Local mode signs in ANY caller as "
                "the owner account — anyone who can reach this port has full control. Use "
                "CITAR_MODE=server for anything reachable by other machines.")
    return warnings


def _has_password_account() -> bool:
    """Whether anybody could actually sign in with a password right now."""
    try:
        with db.session() as s:
            return bool(s.scalar(select(func.count()).select_from(User).where(
                User.password_hash.is_not(None), User.status.in_(("active", "probation")))))
    except Exception:
        return False


def ensure_accounts() -> Optional[str]:
    """Make sure somebody can get in. Returns a message for the operator, if there is one."""
    cfg = settings.get()
    with db.session() as s:
        if cfg.local:
            owner = accounts.ensure_local_owner(s)
            return f"Local mode: signed in automatically as '{owner.handle}' (admin)."
        admins = s.scalar(select(func.count()).select_from(User)
                          .where(User.role == "admin", User.status == "active"))
        if not admins:
            return ("No administrator account exists yet. Create one with:\n"
                    "    python -m citar.server.admin_cli create-admin --handle NAME --email you@example.com\n"
                    "Until then nobody can administer this server.")
    return None


def startup() -> None:
    """Called once when the app starts."""
    cfg = settings.get()
    db.configure()
    migrate()
    policy.invalidate()

    warnings = check_configuration()
    described = cfg.describe()
    log.info("CITAR starting: %s", ", ".join(f"{k}={v}" for k, v in described.items()))
    for warning in warnings:
        log.warning("%s", warning)
    message = ensure_accounts()
    if message:
        log.info("%s", message)

    # Printed as well as logged: on a first run the operator is watching the console, and these are
    # the two things they need to act on.
    if warnings or message:
        print("\n" + "=" * 78)
        for warning in warnings:
            print("WARNING: " + warning)
        if message:
            print(message)
        print("=" * 78 + "\n", flush=True)
