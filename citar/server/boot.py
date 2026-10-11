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
import os
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


#: The revisions of 0.1.5 and Phases 0-2, which the 0.1.6 baseline (``0016a_baseline``) replaced
#: (design/phase3-features.md P3.2.2). A database at one of them is set aside, never migrated.
OLD_REVISIONS = ("f064316a6b34", "767c824c2fcb")

#: The files an SQLite database may have beside it, renamed with it: its write-ahead log, the log's
#: shared-memory index, and a rollback journal a crash left.
SQLITE_COMPANIONS = ("-wal", "-shm", "-journal")


def _alembic_config(url: str):
    """The migration environment for the database at ``url``, built in code rather than read from
    alembic.ini: the .ini is a development convenience for the command line, and a wheel should not
    have to ship one outside the package."""
    from alembic.config import Config
    cfg = Config()
    cfg.set_main_option("script_location", str(paths.migrations_dir()))
    cfg.set_main_option("file_template", "%%(year)d%%(month).2d%%(day).2d_%%(hour).2d%%(minute).2d_%%(slug)s")
    # set_main_option interpolates: a '%' in a password would otherwise be read as a placeholder
    cfg.set_main_option("sqlalchemy.url", url.replace("%", "%%"))
    cfg.attributes["configure_logger"] = False
    return cfg


def migrate() -> Optional[str]:
    """Bring the schema to head, creating it if this is the first run. Returns a notice for the
    operator when the database was set aside, else None; raises ``BootError`` when the server must
    not start on it.

    The cases (P3.2.2):

    * an empty database: the schema is created;
    * a database at a revision these scripts know: upgraded to head;
    * a database at one of ``OLD_REVISIONS`` (0.1.5 or Phases 0-2): it is not migrated. On SQLite its
      write-ahead log is checkpointed into the file, the engine let go, and the file renamed, never
      deleted, to ``citar-0.1.5-<YYYYmmdd-HHMMSS>.db`` beside it with its ``-wal``/``-shm`` if any are
      left; then the schema is created on a fresh database. A rename that fails (on Windows, another
      process holding the file) undoes the renames before it and refuses the boot, naming the file:
      the server never carries on with the old database, or half of it renamed. On another database
      (Postgres) the boot is refused: a server process does not rename somebody's database;
    * a database at a revision these scripts do not know (a newer build's): refused, naming it;
    * tables with no migration history (only ``db.create_all()`` makes those: tests): stamped at
      head when they are the baseline's (``users.listed`` exists), else set aside as an old one.
    """
    from alembic import command
    from alembic.script import ScriptDirectory

    engine = db.engine()
    # the engine's own URL, so the migrations run on the database the server is about to use
    cfg = _alembic_config(engine.url.render_as_string(hide_password=False))
    inspector = inspect(engine)
    tables = set(inspector.get_table_names())

    if not tables:
        log.info("Empty database — creating the schema.")
        command.upgrade(cfg, "head")
        log.info("Database schema is up to date.")
        return None

    revisions = _current_revisions(engine) if "alembic_version" in tables else []
    if not revisions:
        columns = ({c["name"] for c in inspector.get_columns("users")} if "users" in tables else set())
        if "listed" in columns:
            # Made by create_all() from the current models: the tables are the baseline's, with no
            # history. Stamp rather than replay a revision that would collide with them.
            log.info("Existing tables with no migration history — stamping at head.")
            db.create_all()
            command.stamp(cfg, "head")
            log.info("Database schema is up to date.")
            return None
        return _set_aside(cfg, engine, "tables from before 0.1.6 with no migration history")

    script = ScriptDirectory.from_config(cfg)
    for rev in revisions:
        if rev in OLD_REVISIONS:
            return _set_aside(cfg, engine, f"revision {rev}, from 0.1.5 or an earlier 0.1.6 build")
        try:
            known = script.get_revision(rev) is not None
        except Exception:
            known = False
        if not known:
            raise BootError(
                f"The database ({_describe_url(engine)}) is at schema revision {rev}, which this version "
                "of CITAR does not know: it was written by a newer version. Run that version, or point "
                "CITAR_DB_URL at another database.")
    command.upgrade(cfg, "head")
    log.info("Database schema is up to date.")
    return None


def _current_revisions(engine) -> list:
    """The revisions an ``alembic_version`` table names (none for an empty one)."""
    from sqlalchemy import text
    with engine.connect() as conn:
        return [row[0] for row in conn.execute(text("SELECT version_num FROM alembic_version"))]


def _describe_url(engine) -> str:
    """The database, for a message: its file for SQLite, the URL without its password otherwise."""
    url = engine.url
    if url.get_backend_name() == "sqlite":
        return url.database or "an in-memory SQLite database"
    return url.render_as_string(hide_password=True)


def _set_aside(cfg, engine, why: str) -> str:
    """Rename an old SQLite database aside and create the schema on a fresh one; refuse any other.

    Returns the notice for the operator. Raises ``BootError`` when it cannot be done whole."""
    from alembic import command

    if engine.url.get_backend_name() != "sqlite":
        raise BootError(
            f"The database ({_describe_url(engine)}) holds {why}. CITAR 0.1.6 starts on a fresh schema "
            "and does not migrate it, and a server does not drop or rename a database it did not "
            "create. Create a new database and point CITAR_DB_URL at it, or drop this one's schema, "
            "then start again.")
    path = engine.url.database
    if not path or path == ":memory:":
        raise BootError(f"The in-memory database holds {why}; nothing can be renamed aside.")
    old = Path(path).resolve()
    url = engine.url.render_as_string(hide_password=False)
    _quiesce_sqlite(engine)
    moved = _rename_aside(old)
    db.configure(url)              # a new file at the old path, the fresh database
    command.upgrade(cfg, "head")
    notice = (f"The database {old} held {why}. CITAR 0.1.6 starts on a fresh schema, so it was renamed "
              f"to {moved} (nothing was deleted) and a new database was created. Games on disk are "
              "indexed again as they are loaded; accounts, servers and sharing start empty.")
    log.warning("%s", notice)
    log.info("Database schema is up to date.")
    return notice


def _quiesce_sqlite(engine) -> None:
    """Write the database's log into its file and close every connection, so the file can be
    renamed and holds everything on its own."""
    try:
        with engine.connect() as conn:
            conn.exec_driver_sql("PRAGMA wal_checkpoint(TRUNCATE)")
    except Exception:
        # A database not in WAL mode has nothing to checkpoint; one that cannot be read at all is
        # renamed as it is, with whatever log it has beside it.
        log.debug("could not checkpoint the old database", exc_info=True)
    db.dispose()


def _rename_aside(path: Path) -> Path:
    """Rename ``path`` and its companion files to ``citar-0.1.5-<stamp>.db`` beside it; return the
    new name. All or nothing: a rename that fails undoes the ones before it and raises BootError
    naming the file and the error."""
    from datetime import datetime
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    target = path.with_name(f"citar-0.1.5-{stamp}.db")
    n = 2
    while any(Path(str(target) + c).exists() for c in ("",) + SQLITE_COMPANIONS):
        target = path.with_name(f"citar-0.1.5-{stamp}-{n}.db")
        n += 1
    done: list = []
    for suffix in ("",) + SQLITE_COMPANIONS:
        src, dst = Path(str(path) + suffix), Path(str(target) + suffix)
        if suffix and not src.exists():
            continue
        try:
            os.replace(src, dst)
        except OSError as e:
            undone = _undo_renames(done)
            raise BootError(
                f"The database {path} is from before 0.1.6 and has to be renamed aside, but {src} "
                f"could not be renamed: {e}. Stop whatever else has it open (another CITAR, a "
                "database browser, a backup or sync tool) and start again."
                + ("" if undone else " Some files could not be put back; see the log.")) from e
        done.append((src, dst))
    return target


def _undo_renames(done: list) -> bool:
    """Put renamed files back, newest first; whether every one went back."""
    ok = True
    for src, dst in reversed(done):
        try:
            os.replace(dst, src)
        except OSError:
            ok = False
            log.exception("could not put %s back as %s", dst, src)
    return ok


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
    notice = migrate()
    policy.invalidate()

    warnings = check_configuration()
    if notice:
        warnings.insert(0, notice)
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
