"""HTTP API for the two setup wizards in the browser.

Two audiences, one router, because they ask the same underlying questions at different depths:

**First run** (``/api/setup/*``). Somebody has just installed CITAR and opened the browser. The
wizard finds their model servers, registers the one they pick, and gets them into a game. It is
offered when the registry holds nothing that can actually play — a fresh install ships a few
placeholder entries, so "is there a registry file" is the wrong test and "is there a model somebody
could put in a seat" is the right one.

**Operator console** (``/api/setup/console``). A running public server, seen by an administrator:
what is configured, what is missing, and what each gap costs them. Everything it reports is derived
from the same settings and policy the server actually uses, so it cannot drift from reality the way
a checklist in a document does.

Authorisation is the ordinary kind. The first-run routes require the ``register_servers``
capability, which in local mode the auto-signed-in owner has and on a public server a normal member
has for their own hardware. The console routes are administrator-only. Neither is exempt because
"setup is special": a setup endpoint that skipped authorisation would be the single most useful
thing on the server to an attacker.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel

from .. import keystore, paths, servers as registry, settings
from ..auth import policy
from ..auth.deps import require_admin, require_cap
from ..db.models import User

router = APIRouter()

#: Providers that cannot take a seat, whatever their catalogue says.
_NON_PLAYING = ("none", "dryrun")


# ----------------------------------------------------------------------------- first run

def _playable_models() -> int:
    """How many models in the registry could actually be put in a seat.

    A model on a server whose provider is ``none`` (a machine recorded for costing only) or
    ``dryrun`` (the no-model test harness) does not count, and neither does a server with an empty
    catalogue. This is the number that decides whether first-run setup is offered.
    """
    total = 0
    for server in registry.list_servers():
        provider = ((server.get("connection") or {}).get("provider") or "none")
        if provider in _NON_PLAYING:
            continue
        if provider != "anthropic" and not (server.get("connection") or {}).get("base_url"):
            continue
        total += len([m for m in server.get("models") or [] if m.get("enabled", True)])
    return total


@router.get("/api/setup/state")
def setup_state(me: User = Depends(require_cap("register_servers"))):
    """Whether to offer first-run setup, and what the machine looks like.

    Deliberately cheap: it does not probe the network. The browser calls this on every page load to
    decide whether to show the welcome banner, so the expensive detection lives behind ``/scan``,
    which the wizard calls once when it opens.
    """
    cfg = settings.get()
    return {
        "needed": _playable_models() == 0 and not policy.get("setup_dismissed", False),
        "dismissed": bool(policy.get("setup_dismissed", False)),
        "playable_models": _playable_models(),
        "mode": "server" if cfg.server_mode else "local",
        "paths": paths.describe(),
    }


class ScanResult(BaseModel):
    """What detection found. Mirrors :mod:`citar.wizard.detect` so both wizards agree."""

    endpoints: list
    hardware_lines: list
    vram_gb: float
    suggested_model: str
    suggested_reason: str


@router.post("/api/setup/scan", response_model=ScanResult)
def setup_scan(me: User = Depends(require_cap("register_servers"))):
    """Look for model servers on this machine, and read its hardware.

    Takes a second or two: several endpoints are probed, in parallel, with short timeouts.
    """
    from ..wizard import detect

    endpoints = detect.find_endpoints(include_unreachable=True)
    info = detect.hardware()
    vram = detect.usable_vram_gb(info)
    model, reason = detect.suggest_model(vram)
    return ScanResult(
        endpoints=[{
            "provider": e.provider, "label": e.label, "base_url": e.base_url,
            "install_url": e.install_url, "reachable": e.reachable,
            "models": [m for m in e.models if not _is_embedding(m)],
            "summary": e.summary, "error": e.error,
        } for e in endpoints],
        hardware_lines=detect.describe_hardware(info),
        vram_gb=vram,
        suggested_model=model,
        suggested_reason=reason,
    )


def _is_embedding(model_key: str) -> bool:
    """Embedding models show up in every catalogue and cannot play a turn."""
    lowered = model_key.lower()
    return any(marker in lowered for marker in ("embed", "-rerank", "bge-", "e5-", "gte-"))


class ApplyBody(BaseModel):
    """One endpoint or one hosted API to register."""

    provider: str
    label: str = ""
    base_url: str = ""
    models: list[str] = []
    api_key: Optional[str] = None
    kind: str = "owned"
    collect_hardware: bool = True


@router.post("/api/setup/apply")
def setup_apply(body: ApplyBody, me: User = Depends(require_cap("register_servers"))):
    """Register what the wizard chose, and return the new server."""
    if body.provider not in registry.PROVIDERS:
        raise HTTPException(400, f"Unknown provider {body.provider!r}.")
    if body.provider not in ("anthropic",) and not body.base_url:
        raise HTTPException(400, "This provider needs a base URL.")

    server = registry.default_server(kind=body.kind, provider=body.provider)
    server["name"] = body.label or body.provider
    server["description"] = "Added by first-run setup."
    server["connection"]["base_url"] = body.base_url
    server["connection"]["manage_loading"] = body.provider == "lmstudio"
    server["models"] = [registry.default_model(key, body.provider)
                        for key in body.models if not _is_embedding(key)]

    if body.collect_hardware and body.base_url.startswith(("http://localhost", "http://127.0.0.1")):
        from .. import hwinfo

        try:
            server["hardware"] = hwinfo.collect()
        except Exception:
            pass
        server["is_host"] = not registry.load().get("host_server_id")

    if body.api_key:
        server["connection"]["key"] = {"backend": "keyring", "env": ""}

    try:
        saved = registry.upsert(server)
    except registry.ServerError as exc:
        raise HTTPException(400, str(exc))

    if body.api_key:
        try:
            keystore.store(saved["id"], "keyring", body.api_key)
        except Exception as exc:
            return {"server": saved, "warning":
                    f"The server was added, but the key could not be stored ({exc}). "
                    "Set it on the Servers page."}
    return {"server": saved}


@router.post("/api/setup/dismiss")
def setup_dismiss(me: User = Depends(require_cap("register_servers"))):
    """Stop offering first-run setup.

    Recorded rather than inferred: somebody who chose to play against the scripted bots has finished
    setting up, even though the test for "is a model configured" still answers no, and being asked
    again on every page load would be nagging.
    """
    policy.set("setup_dismissed", True, actor=me)
    return {"dismissed": True}


@router.post("/api/setup/reopen")
def setup_reopen(me: User = Depends(require_cap("register_servers"))):
    """Offer first-run setup again; the Servers page links here."""
    policy.set("setup_dismissed", False, actor=me)
    return {"dismissed": False}


# ----------------------------------------------------------------------------- operator console

def _check(ok: bool, title: str, detail: str, fix: str = "", severity: str = "warn") -> dict:
    """One configuration check: what is true, and what to do if it is not."""
    return {"ok": ok, "title": title, "detail": detail, "fix": fix,
            "severity": "ok" if ok else severity}


@router.get("/api/setup/console")
def operator_console(me: User = Depends(require_admin)):
    """Everything an operator needs to see about this deployment, in one call.

    Each entry says what is true, and for anything that is not configured, what that costs and the
    exact command or page that fixes it. Nothing here is a secret: keys and passwords are reported
    as present or absent, never echoed.
    """
    cfg = settings.get()
    checks: list[dict] = []

    # Most of what follows only *is* a problem on a public server. Local mode binds to loopback and
    # signs the operator in automatically, so "no sign-in provider configured" is how local mode is
    # supposed to look, and reporting it as a fault would teach people to ignore this page — which
    # is the failure mode of every checklist that cries wolf.
    public = cfg.server_mode

    checks.append(_check(
        not public or cfg.public_origin.startswith("https://"),
        "Public address",
        cfg.public_origin or "not set",
        "CITAR_PUBLIC_ORIGIN must be the https:// URL browsers actually use. Sign-in links are "
        "built from it, and a mismatch refuses every write.",
        severity="error"))

    checks.append(_check(
        bool(cfg.email_enabled) or not public, "E-mail",
        f"via {cfg.smtp.host}:{cfg.smtp.port}" if cfg.email_enabled
        else ("not configured - not needed in local mode" if not public else "not configured"),
        "Without SMTP there is no sign-up verification and no password reset. Invitations still "
        "work. Set CITAR_SMTP_HOST and CITAR_MAIL_FROM."))

    providers = cfg.enabled_oauth_providers
    if public:
        ways = ", ".join(providers) + (" + password" if cfg.email_enabled else "") \
            if (providers or cfg.email_enabled) else "none"
        checks.append(_check(
            bool(providers) or bool(cfg.email_enabled), "Sign-in methods", ways,
            "With neither e-mail nor a sign-in provider, the only way in is an account made with "
            "`citar admin create-admin`. docs/server/OAUTH.md has the registration steps.",
            severity="error"))
    else:
        checks.append(_check(
            True, "Sign-in", "automatic - local mode signs you in as the owner",
            "Single sign-on and passwords are for a server other people use."))

    mode = policy.registration_mode()
    checks.append(_check(
        not public or mode != "open" or bool(cfg.hcaptcha_site_key), "Sign-up protection",
        f"registration is {mode}" + (", hCaptcha on" if cfg.hcaptcha_site_key else ""),
        "Open registration without a captcha collects automated sign-ups. Either set "
        "CITAR_HCAPTCHA_SITE_KEY and CITAR_HCAPTCHA_SECRET, or switch registration to invite."))

    playable = _playable_models()
    checks.append(_check(
        playable > 0, "Models",
        f"{playable} model(s) available to seats" if playable else "none registered",
        "Add a machine on the Servers page, or have somebody run `citar setup --worker` to "
        "connect theirs."))

    state = paths.state_dir()
    checks.append(_check(
        True, "State directory", str(state),
        "This is the only directory that needs backing up, besides the environment file."))

    return {
        "mode": "server" if cfg.server_mode else "local",
        "version": _version(),
        "checks": checks,
        "policy": policy.all_settings(),
        "paths": paths.describe(),
    }


def _version() -> str:
    """The running CITAR version."""
    from .. import __version__

    return __version__


class PolicyBody(BaseModel):
    """A policy value to change."""
    key: str
    value: object


@router.post("/api/setup/policy")
def set_policy(body: PolicyBody, me: User = Depends(require_admin)):
    """Change one runtime policy value, with an audit entry."""
    from .. import db
    from ..auth import audit

    try:
        with db.session() as session:
            policy.set(body.key, body.value, session=session, actor=me)
            audit.record(session, "policy.set", actor=me, object_type="setting",
                         object_id=body.key, detail={"value": body.value})
    except KeyError:
        raise HTTPException(400, f"Unknown policy setting {body.key!r}.")
    return {"key": body.key, "value": policy.get(body.key)}


class TestEmailBody(BaseModel):
    """Where to send a test message."""
    to: str


@router.post("/api/setup/test-email")
async def test_email(body: TestEmailBody, me: User = Depends(require_admin)):
    """Send a test message, and report what the mail server said.

    Mail is the setting most likely to be wrong in a way nothing notices: a bad password means every
    verification link silently vanishes, and the first report of it is somebody who cannot sign up.
    The error text is passed through because it is the only useful thing here - "authentication
    failed" and "certificate verify failed" need completely different fixes.
    """
    from ..auth import mailer

    cfg = settings.get()
    if not cfg.email_enabled:
        raise HTTPException(400, "No SMTP is configured on this server.")
    try:
        sent = await mailer.send(
            body.to, "CITAR test message",
            "This is a test from your CITAR server.\n\n"
            "If you are reading it, verification e-mails, password resets and e-mailed "
            f"invitations will work.\n\nServer: {cfg.public_origin}\n")
    except Exception as exc:
        raise HTTPException(400, f"{type(exc).__name__}: {exc}")
    return {"sent": bool(sent), "to": body.to}
