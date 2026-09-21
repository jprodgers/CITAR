"""Runtime settings for the CITAR server, read from the environment once at import.

CITAR runs in one of two modes:

    local     (default) one operator on their own machine. The server binds to loopback, mints a
              session for a single owner account automatically, and generates its own secret key.
              This is the laptop workflow: no login screen, no TLS, no OAuth registration.
    server    the public deployment. Sessions are real, TLS is required, and the process refuses to
              start without a secret key and a public origin.

Every authorization check runs the same way in both modes — local mode is a real account with a real
session, not a bypass — so a permission bug cannot hide locally and surface in production.

Where things live
-----------------
The project folder syncs to OneDrive, which is fine for code and game saves but *not* for a live
SQLite database: the sync client copies files mid-write and a WAL database does not survive that.
It is also the wrong place for password hashes and session tokens. So state that must be both
durable and private goes to a per-user data directory outside the project:

    Windows   %LOCALAPPDATA%\\CITAR
    macOS     ~/Library/Application Support/CITAR
    Linux     $XDG_DATA_HOME/citar  (default ~/.local/share/citar)

Override with CITAR_DATA_DIR. On the VPS this is a normal directory under the service account, and
it is the only thing that needs backing up besides saves/.

Secrets come from the environment, never from a file in the project folder. `citar/env.example`
lists every variable; on the VPS they are set in the systemd unit's EnvironmentFile with mode 0600.
"""
from __future__ import annotations

import os
import secrets
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

ROOT = Path(__file__).resolve().parent.parent
MODES = ("local", "server")

# Providers we can do single sign-on with. Each needs a client id and secret registered by the
# operator; a provider with no credentials configured simply does not appear on the login page.
OAUTH_PROVIDERS = ("google", "github", "discord", "microsoft")


class SettingsError(RuntimeError):
    """A misconfiguration serious enough that the server must not start."""


def _env(name: str, default: str = "") -> str:
    """An environment variable, stripped, with a default."""
    return (os.environ.get(name) or default).strip()


def _flag(name: str, default: bool = False) -> bool:
    """A boolean environment variable."""
    raw = _env(name)
    if not raw:
        return default
    return raw.lower() in ("1", "true", "yes", "on")


def _int(name: str, default: int) -> int:
    """An integer environment variable, raising a readable error if it is not one."""
    raw = _env(name)
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        raise SettingsError(f"{name} must be a whole number, not {raw!r}.")


def data_dir() -> Path:
    """The private state directory (database, generated secret key). Created on demand.

    Resolved by :func:`citar.paths.data_dir`, which every other directory also goes through, so
    there is one answer to "where does CITAR put things" rather than one per module.
    """
    from . import paths

    return paths.data_dir()


def _local_secret_key() -> str:
    """A persistent secret for local mode, so restarting does not log the operator out.

    Server mode never uses this: there the key is supplied by the environment, because a key on disk
    next to the database is not much of a secret and cannot be rotated across several machines.
    """
    path = data_dir() / "secret_key"
    if path.exists():
        key = path.read_text(encoding="utf-8").strip()
        if len(key) >= 32:
            return key
    key = secrets.token_urlsafe(48)
    path.write_text(key, encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        pass  # Windows ACLs; the directory is already per-user
    return key


@dataclass(frozen=True)
class SMTP:
    """Outbound mail settings."""
    host: str = ""
    port: int = 587
    user: str = ""
    password: str = ""
    from_address: str = ""
    from_name: str = "CITAR"
    security: str = "starttls"   # starttls | ssl | none
    #: smtp sends for real; log writes the whole message to the log instead. `log` is how the email
    #: flows — verification, reset, invitations — get exercised on a laptop with no mail server:
    #: the link appears in the terminal and the identical code path runs.
    transport: str = "smtp"

    @property
    def configured(self) -> bool:
        """Whether enough is set for mail to be sent at all."""
        if self.transport == "log":
            return True
        return bool(self.host and self.from_address)


@dataclass(frozen=True)
class OAuthClient:
    """One sign-in provider's credentials."""
    provider: str
    client_id: str = ""
    client_secret: str = ""
    # Entra tenant, for the microsoft provider only ("common" accepts any Microsoft account).
    tenant: str = "common"

    @property
    def configured(self) -> bool:
        """Whether this provider has both halves of its credentials."""
        return bool(self.client_id and self.client_secret)


@dataclass(frozen=True)
class Settings:
    """Everything the process needs to start, read once from the environment.

    Frozen, because settings that change under a running server are a source of behaviour that cannot
    be reproduced. What an operator changes while running is policy, which lives in the database -
    see :mod:`citar.auth.policy`.
    """
    mode: str
    public_origin: str
    secret_key: str
    db_url: str
    host: str
    port: int
    smtp: SMTP
    oauth: dict
    hcaptcha_site_key: str
    hcaptcha_secret: str
    behind_proxy: bool
    trusted_proxy_hops: int
    session_days: int
    session_idle_hours: int
    registration: str            # open | invite | closed (the boot default; admins change it live)
    require_https: bool
    worker_ping_seconds: int
    debug: bool

    # ---------------------------------------------------------------- derived
    @property
    def local(self) -> bool:
        """Whether this is local mode."""
        return self.mode == "local"

    @property
    def server_mode(self) -> bool:
        """Whether this is a public deployment."""
        return self.mode == "server"

    @property
    def captcha_enabled(self) -> bool:
        """Whether a captcha is configured and will actually be checked."""
        return bool(self.hcaptcha_site_key and self.hcaptcha_secret)

    @property
    def email_enabled(self) -> bool:
        """Whether mail can be sent."""
        return self.smtp.configured

    def oauth_enabled(self, provider: str) -> bool:
        """Whether a sign-in provider is configured."""
        client = self.oauth.get(provider)
        return bool(client and client.configured)

    @property
    def enabled_oauth_providers(self) -> list:
        """The providers that will appear on the sign-in page."""
        return [p for p in OAUTH_PROVIDERS if self.oauth_enabled(p)]

    def callback_url(self, provider: str) -> str:
        """The callback URL to register with a provider. Must match exactly."""
        return f"{self.public_origin.rstrip('/')}/api/auth/oauth/{provider}/callback"

    def url(self, path: str = "/") -> str:
        """An absolute URL on this server, built from the public origin."""
        return self.public_origin.rstrip("/") + "/" + path.lstrip("/")

    def describe(self) -> dict:
        """What the operator sees in the logs at boot, and admins see on the settings page.

        Deliberately free of secrets: whether something is configured, never what it is.
        """
        return {
            "mode": self.mode,
            "public_origin": self.public_origin,
            "database": _redact_db_url(self.db_url),
            "data_dir": str(data_dir()),
            "bind": f"{self.host}:{self.port}",
            "email": self.smtp.host if self.email_enabled else "not configured",
            "captcha": "hcaptcha" if self.captcha_enabled else "not configured",
            "sso": self.enabled_oauth_providers or ["none configured"],
            "registration": self.registration,
            "behind_proxy": self.behind_proxy,
        }


def _redact_db_url(url: str) -> str:
    """Postgres URLs carry a password; never print it."""
    if "://" not in url or "@" not in url:
        return url
    scheme, rest = url.split("://", 1)
    creds, host = rest.rsplit("@", 1)
    user = creds.split(":", 1)[0]
    return f"{scheme}://{user}:***@{host}"


def default_db_url() -> str:
    # check_same_thread is handled by the engine; the path is absolute so the cwd cannot move the db.
    """The SQLite database in the private data directory."""
    return "sqlite:///" + (data_dir() / "citar.db").as_posix()


def load(**overrides) -> Settings:
    """Read settings from the environment. `overrides` is for tests, which never touch the real env."""
    mode = (overrides.get("mode") or _env("CITAR_MODE", "local")).lower()
    if mode not in MODES:
        raise SettingsError(f"CITAR_MODE must be one of {', '.join(MODES)}, not {mode!r}.")

    public_origin = overrides.get("public_origin", _env("CITAR_PUBLIC_ORIGIN"))
    secret_key = overrides.get("secret_key", _env("CITAR_SECRET_KEY"))
    db_url = overrides.get("db_url") or _env("CITAR_DB_URL") or default_db_url()
    host = overrides.get("host") or _env("CITAR_HOST") or ("127.0.0.1" if mode == "local" else "0.0.0.0")
    port = overrides.get("port") or _int("CITAR_PORT", 8765)
    behind_proxy = overrides.get("behind_proxy", _flag("CITAR_BEHIND_PROXY", mode == "server"))
    require_https = overrides.get("require_https", _flag("CITAR_REQUIRE_HTTPS", mode == "server"))
    registration = (overrides.get("registration") or _env("CITAR_REGISTRATION", "invite")).lower()
    if registration not in ("open", "invite", "closed"):
        raise SettingsError("CITAR_REGISTRATION must be open, invite or closed.")

    if mode == "server":
        if not secret_key:
            raise SettingsError(
                "CITAR_SECRET_KEY is required in server mode. Generate one with:\n"
                "    python -c \"import secrets; print(secrets.token_urlsafe(48))\"\n"
                "Changing it later logs everyone out, so store it with the deployment.")
        if len(secret_key) < 32:
            raise SettingsError("CITAR_SECRET_KEY is too short; use at least 32 characters.")
        if not public_origin:
            raise SettingsError(
                "CITAR_PUBLIC_ORIGIN is required in server mode, e.g. https://citar.example.com\n"
                "It is what OAuth callbacks and emailed links are built from.")
        if require_https and not public_origin.startswith("https://"):
            raise SettingsError(
                f"CITAR_PUBLIC_ORIGIN is {public_origin!r}, but HTTPS is required in server mode: OAuth\n"
                "providers reject plain-HTTP callbacks and session cookies need the Secure flag.\n"
                "Terminate TLS in front of CITAR (the shipped Caddyfile does this) or, only for a\n"
                "private test, set CITAR_REQUIRE_HTTPS=0.")
    else:
        secret_key = secret_key or _local_secret_key()
        public_origin = public_origin or f"http://{'localhost' if host in ('127.0.0.1', '0.0.0.0') else host}:{port}"

    smtp = SMTP(
        host=_env("CITAR_SMTP_HOST"),
        port=_int("CITAR_SMTP_PORT", 587),
        user=_env("CITAR_SMTP_USER"),
        password=_env("CITAR_SMTP_PASSWORD"),
        from_address=_env("CITAR_MAIL_FROM"),
        from_name=_env("CITAR_MAIL_FROM_NAME", "CITAR"),
        security=_env("CITAR_SMTP_SECURITY", "starttls").lower(),
        transport=_env("CITAR_MAIL_TRANSPORT", "smtp").lower(),
    )
    if smtp.security not in ("starttls", "ssl", "none"):
        raise SettingsError("CITAR_SMTP_SECURITY must be starttls, ssl or none.")
    if smtp.transport not in ("smtp", "log"):
        raise SettingsError("CITAR_MAIL_TRANSPORT must be smtp or log.")
    if smtp.transport == "log" and mode == "server":
        raise SettingsError(
            "CITAR_MAIL_TRANSPORT=log writes verification and password-reset links to the log "
            "instead of sending them, which is a development-only setting. It cannot be used in "
            "server mode.")

    oauth = {}
    for provider in OAUTH_PROVIDERS:
        prefix = f"CITAR_OAUTH_{provider.upper()}"
        oauth[provider] = OAuthClient(
            provider=provider,
            client_id=_env(f"{prefix}_CLIENT_ID"),
            client_secret=_env(f"{prefix}_CLIENT_SECRET"),
            tenant=_env("CITAR_OAUTH_MICROSOFT_TENANT", "common"),
        )

    return Settings(
        mode=mode,
        public_origin=public_origin.rstrip("/"),
        secret_key=secret_key,
        db_url=db_url,
        host=host,
        port=int(port),
        smtp=smtp,
        oauth=oauth,
        hcaptcha_site_key=_env("CITAR_HCAPTCHA_SITE_KEY"),
        hcaptcha_secret=_env("CITAR_HCAPTCHA_SECRET"),
        behind_proxy=behind_proxy,
        trusted_proxy_hops=_int("CITAR_TRUSTED_PROXY_HOPS", 1),
        session_days=_int("CITAR_SESSION_DAYS", 30),
        session_idle_hours=_int("CITAR_SESSION_IDLE_HOURS", 24 * 14),
        registration=registration,
        require_https=require_https,
        worker_ping_seconds=_int("CITAR_WORKER_PING_SECONDS", 20),
        debug=_flag("CITAR_DEBUG"),
    )


_settings: Optional[Settings] = None


def get() -> Settings:
    """The process-wide settings, loaded on first use."""
    global _settings
    if _settings is None:
        _settings = load()
    return _settings


def set_for_test(**overrides) -> Settings:
    """Replace the process settings. Tests only — `reset()` puts things back."""
    global _settings
    _settings = load(**overrides)
    return _settings


def reset():
    """Forget the cached settings, so the next read picks up a changed environment.

    For tests, and for the setup wizard. Not something a running server does.
    """
    global _settings
    _settings = None
