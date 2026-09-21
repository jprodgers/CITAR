"""``citar doctor`` - check an installation and explain anything that is wrong.

The point of this command is to turn "it doesn't work" into a specific sentence. Every check prints
one line: what was tested, what was found, and - when something is wrong - the one thing to do about
it. Nothing here changes state, so it is safe to run against a live server, and its output is what a
bug report should contain.

Checks, in the order a failure would stop you:

1. **Python and CITAR versions**, and whether this is a checkout or an installed copy.
2. **Dependencies** - every import CITAR needs, named individually rather than as one traceback.
3. **Directories** - where state lives and whether it is writable.
4. **Configuration** - the mode, and in server mode the settings that must be present.
5. **Database** - that it opens and its schema is current.
6. **Ruleset** - that the packaged data loads and how much of it there is.
7. **Model providers** - every server in the registry that CITAR can reach right now.
8. **Port** - whether the configured port is free, or already has a CITAR on it.
"""
from __future__ import annotations

import importlib
import platform
import socket
import sys
from pathlib import Path
from typing import Optional, Sequence

from . import __version__, paths

OK, WARN, FAIL = "  ok  ", " warn ", " FAIL "

#: Import name -> what breaks without it, for a dependency that is missing.
REQUIRED = {
    "fastapi": "the web server and API",
    "uvicorn": "serving HTTP",
    "httpx": "talking to model endpoints",
    "sqlalchemy": "accounts and sharing",
    "alembic": "database migrations",
    "argon2": "password hashing",
    "itsdangerous": "signed sign-in links",
    "multipart": "the sign-in forms",
    "email_validator": "checking e-mail addresses",
}

OPTIONAL = {
    "anthropic": "Claude models through the Anthropic API",
    "openai": "OpenAI-compatible endpoints (LM Studio, Ollama, vLLM, llama.cpp)",
    "mcp": "MCP clients such as Claude Code taking a seat",
    "authlib": "single sign-on with Google, GitHub, Discord or Microsoft",
    "keyring": "storing API keys in the OS credential store",
    "websockets": "the worker agent's connection",
    "tzlocal": "detecting this machine's time zone for availability windows",
}


class Report:
    """Collects check results and decides the exit status.

    Exit status is what a health check or an installer looks at: ``0`` when everything that matters
    works, ``1`` when something is actually broken. Warnings never fail the run - a missing optional
    provider is a fact about the setup, not a fault in it.
    """

    def __init__(self) -> None:
        self.failures = 0
        self.warnings = 0

    def line(self, status: str, label: str, detail: str = "", fix: str = "") -> None:
        """Print one check's result, and count it."""
        print(f"[{status}] {label}" + (f": {detail}" if detail else ""))
        if fix:
            print(f"         -> {fix}")
        if status is FAIL:
            self.failures += 1
        elif status is WARN:
            self.warnings += 1

    def section(self, title: str) -> None:
        """Print a section heading."""
        print(f"\n{title}\n{'-' * len(title)}")


def _check_versions(r: Report) -> None:
    """CITAR and Python versions, and whether this is a checkout or an install."""
    r.section("Versions")
    r.line(OK, "CITAR", __version__)
    major, minor = sys.version_info[:2]
    detail = f"{platform.python_version()} ({sys.executable})"
    if (major, minor) < (3, 11):
        r.line(FAIL, "Python", detail, "CITAR needs Python 3.11 or newer.")
    else:
        r.line(OK, "Python", detail)
    r.line(OK, "Platform", f"{platform.system()} {platform.release()} ({platform.machine()})")
    r.line(OK, "Installed from", "a source checkout" if paths.in_source_checkout() else "a package")


def _check_dependencies(r: Report) -> None:
    """Every import CITAR needs, named individually rather than as one traceback."""
    r.section("Dependencies")
    missing = []
    for name, purpose in REQUIRED.items():
        try:
            importlib.import_module(name)
        except ImportError:
            missing.append(name)
            r.line(FAIL, name, f"missing - needed for {purpose}")
    if not missing:
        r.line(OK, "required packages", f"all {len(REQUIRED)} present")
    else:
        r.line(FAIL, "install them", "", "pip install --upgrade citar")

    absent = []
    for name, purpose in OPTIONAL.items():
        try:
            importlib.import_module(name)
        except ImportError:
            absent.append(f"{name} ({purpose})")
    if absent:
        r.line(WARN, "optional packages", f"{len(absent)} not installed")
        for item in absent:
            print(f"           - {item}")
        # A bundled build has no pip, and telling somebody who double-clicked an installer to run
        # a pip command is advice they cannot follow.
        if getattr(sys, "frozen", False):
            r.line(WARN, "", "", "This build ships what it needs; download the current release if "
                                 "a feature is missing.")
        else:
            r.line(WARN, "", "", "pip install 'citar[all]' adds every optional provider.")
    else:
        r.line(OK, "optional packages", "all present")


def _check_directories(r: Report) -> None:
    """Where state lives, and whether it can actually be written."""
    r.section("Directories")
    for label, value in paths.describe().items():
        if label in ("package", "source checkout"):
            r.line(OK, label, value)
            continue
        path = Path(value)
        try:
            path.mkdir(parents=True, exist_ok=True)
            probe = path / ".citar-write-test"
            probe.write_text("", encoding="utf-8")
            probe.unlink()
            r.line(OK, label, value)
        except OSError as exc:
            r.line(FAIL, label, f"{value} - not writable ({exc.strerror or exc})",
                   "Point CITAR_STATE_DIR at a directory you own, or fix the permissions.")


def _check_configuration(r: Report) -> None:
    """The mode, and in server mode the settings that must be present."""
    r.section("Configuration")
    from . import settings

    try:
        cfg = settings.get()
    except settings.SettingsError as exc:
        r.line(FAIL, "settings", str(exc).splitlines()[0],
               "Fix the environment, then run `citar doctor` again.")
        return

    r.line(OK, "mode", "server (public deployment)" if cfg.server_mode else "local (single operator)")
    if cfg.server_mode:
        r.line(OK if cfg.public_origin.startswith("https://") else FAIL,
               "public origin", cfg.public_origin or "(unset)",
               "" if cfg.public_origin.startswith("https://")
               else "CITAR_PUBLIC_ORIGIN must be the https:// URL browsers actually use.")
        r.line(OK if cfg.email_enabled else WARN,
               "e-mail", "configured" if cfg.email_enabled else "not configured",
               "" if cfg.email_enabled
               else "Sign-up, verification and password reset need SMTP. Sign-in providers still work.")
    else:
        r.line(OK, "sign-in", "automatic - local mode logs the operator in")

    try:
        from .server import boot

        warnings = boot.check_configuration()
        for warning in warnings:
            r.line(WARN, "boot check", warning.splitlines()[0])
        if not warnings:
            r.line(OK, "boot checks", "no warnings")
    except Exception as exc:
        r.line(WARN, "boot checks", f"could not run ({type(exc).__name__}: {exc})")


def _check_database(r: Report) -> None:
    """That the database opens and its schema is current."""
    r.section("Database")
    from . import db, settings

    cfg = settings.get()
    shown = cfg.db_url
    if "@" in shown:                                            # never print a password
        shown = shown.split("://", 1)[0] + "://...@" + shown.rsplit("@", 1)[1]
    r.line(OK, "url", shown)
    try:
        from sqlalchemy import inspect

        tables = set(inspect(db.engine()).get_table_names())
    except Exception as exc:
        r.line(FAIL, "connection", f"{type(exc).__name__}: {exc}",
               "Check CITAR_DB_URL, and that the database file's directory exists and is writable.")
        return
    if not tables:
        r.line(WARN, "schema", "empty", "It is created the first time the server starts.")
    elif "alembic_version" not in tables:
        r.line(WARN, "schema", f"{len(tables)} tables, no migration history",
               "The next start stamps it; no action needed.")
    else:
        r.line(OK, "schema", f"{len(tables)} tables, migration history present")


def _check_ruleset(r: Report) -> None:
    """That the packaged ruleset loads, and how much of it there is."""
    r.section("Ruleset")
    try:
        from .engine.rules import get_rules

        rules = get_rules()
        counts = []
        for attr, label in (("techs", "techs"), ("units", "units"), ("buildings", "buildings"),
                            ("nations", "nations"), ("policies", "policies")):
            value = getattr(rules, attr, None)
            if value is not None:
                counts.append(f"{len(value)} {label}")
        r.line(OK, "loaded", ", ".join(counts) or "ok")
    except Exception as exc:
        r.line(FAIL, "ruleset", f"{type(exc).__name__}: {exc}",
               f"The packaged data should be at {paths.package_data()}. Reinstall CITAR.")


def _check_providers(r: Report) -> None:
    """Every model endpoint in the registry, and whether it answers right now."""
    r.section("Model providers")
    try:
        from . import servers as registry

        reg = registry.load()
    except Exception as exc:
        r.line(WARN, "registry", f"could not be read ({type(exc).__name__}: {exc})")
        return

    entries = reg.get("servers", []) if isinstance(reg, dict) else []
    if not entries:
        r.line(WARN, "registry", "no servers configured",
               "Run `citar setup`, or add one on the Servers page.")
        return

    import httpx

    for server in entries:
        name = server.get("name") or server.get("id")
        connection = server.get("connection") or {}
        provider = connection.get("provider", "none")
        base_url = connection.get("base_url") or ""
        if provider in ("none", "dryrun"):
            r.line(OK, name, f"{provider} - nothing to reach")
            continue
        if provider == "anthropic":
            r.line(OK, name, "Anthropic API (a key is checked when a seat uses it)")
            continue
        if not base_url:
            r.line(WARN, name, f"{provider} with no base URL")
            continue
        url = base_url.rstrip("/") + "/models"
        try:
            response = httpx.get(url, timeout=3.0)
            if response.status_code < 400:
                count = len((response.json() or {}).get("data", []))
                r.line(OK, name, f"{provider} at {base_url} - {count} models")
            else:
                r.line(WARN, name, f"{provider} at {base_url} - HTTP {response.status_code}")
        except Exception as exc:
            r.line(WARN, name, f"{provider} at {base_url} - unreachable ({type(exc).__name__})",
                   "Start the model server, or ignore this if that machine is off.")


def _check_port(r: Report) -> None:
    """Whether the configured port is free, or already has a CITAR on it."""
    r.section("Network")
    from . import settings

    cfg = settings.get()
    host = "127.0.0.1" if cfg.host in ("0.0.0.0", "::") else cfg.host
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(1.0)
        busy = probe.connect_ex((host, cfg.port)) == 0
    if not busy:
        r.line(OK, "port", f"{cfg.host}:{cfg.port} is free")
        return
    try:
        import httpx

        response = httpx.get(f"http://{host}:{cfg.port}/api/tools", timeout=2.0)
        if response.status_code < 400:
            r.line(OK, "port", f"{cfg.port} - a CITAR server is already running here")
            return
    except Exception:
        pass
    r.line(WARN, "port", f"{cfg.port} is in use by something else",
           f"Start CITAR on another port: citar serve --port {cfg.port + 1}")


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Run every check and return ``0`` when nothing is broken."""
    import argparse

    parser = argparse.ArgumentParser(prog="citar doctor", description=__doc__.splitlines()[0])
    parser.add_argument("--quiet", action="store_true", help="only print problems")
    # None means "read the command line" (``python -m citar.doctor``); an empty list is a bare
    # ``citar doctor``, which has passed its own arguments along already.
    args = parser.parse_args(sys.argv[1:] if argv is None else list(argv))

    # The Windows console is often not UTF-8, and a diagnostic that dies on its own output is
    # worse than useless. Everything printed below is ASCII; this makes the rest survive too.
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, OSError):
        pass

    report = Report()
    if args.quiet:
        original = report.line

        def quiet_line(status, label, detail="", fix=""):
            """Print only the checks that are not passing."""
            if status is not OK:
                original(status, label, detail, fix)

        report.line = quiet_line                                # type: ignore[method-assign]
        report.section = lambda title: None                     # type: ignore[assignment]

    print(f"CITAR {__version__} - checking this installation\n")
    for check in (_check_versions, _check_dependencies, _check_directories, _check_configuration,
                  _check_database, _check_ruleset, _check_providers, _check_port):
        try:
            check(report)
        except Exception as exc:
            report.line(FAIL, check.__name__.lstrip("_"), f"check crashed: {type(exc).__name__}: {exc}")

    print()
    if report.failures:
        print(f"{report.failures} problem(s) found" +
              (f", {report.warnings} warning(s)" if report.warnings else "") + ".")
        return 1
    if report.warnings:
        print(f"No problems. {report.warnings} warning(s) - see above.")
    else:
        print("Everything checks out.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
