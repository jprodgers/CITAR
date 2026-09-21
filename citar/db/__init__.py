"""Database access: engine, sessions and schema creation.

SQLite in WAL mode by default, PostgreSQL when CITAR_DB_URL points at one. The model layer is
written to the intersection of the two — no SQLite-only types, no Postgres-only types — so moving a
deployment from one to the other is a connection-string change plus a data copy, and the test suite
runs against both.

Everything that touches the database goes through `session()`:

    with db.session() as s:
        user = s.get(User, uid)
        user.display_name = "..."
        # committed on a clean exit, rolled back on an exception

The engine is created lazily so that importing citar does not touch the disk; tests call
`configure(url)` to point at a throwaway database before anything else runs.
"""
from __future__ import annotations

import contextlib
import threading
from typing import Iterator, Optional

from sqlalchemy import create_engine, event, text
from sqlalchemy.engine import Engine
from sqlalchemy.orm import Session, sessionmaker

from .. import settings
from .models import Base

_engine: Optional[Engine] = None
_factory: Optional[sessionmaker] = None
_lock = threading.RLock()
_url: Optional[str] = None


def _tune_sqlite(dbapi_connection, _record):
    """WAL and sane durability, applied to every pooled connection.

    WAL lets the turn driver read while a request writes, which a game server does constantly.
    busy_timeout replaces SQLite's instant "database is locked" with a wait, which matters because
    the engine holds its own locks for the length of a turn.
    """
    cur = dbapi_connection.cursor()
    cur.execute("PRAGMA journal_mode=WAL")
    cur.execute("PRAGMA synchronous=NORMAL")
    cur.execute("PRAGMA busy_timeout=10000")
    cur.execute("PRAGMA foreign_keys=ON")
    cur.close()


def configure(url: Optional[str] = None, *, echo: bool = False) -> Engine:
    """Build (or rebuild) the engine. Safe to call again; the previous engine is disposed."""
    global _engine, _factory, _url
    with _lock:
        url = url or settings.get().db_url
        if _engine is not None:
            if url == _url:
                return _engine
            _engine.dispose()
        kwargs: dict = {"echo": echo, "future": True, "pool_pre_ping": True}
        if url.startswith("sqlite"):
            # check_same_thread=False: the engine's turn-driver threads share the pool, and every
            # session is scoped to a single `with` block so a connection is never used concurrently.
            kwargs["connect_args"] = {"check_same_thread": False, "timeout": 10}
        else:
            kwargs["pool_size"] = 10
            kwargs["max_overflow"] = 20
        engine = create_engine(url, **kwargs)
        if url.startswith("sqlite"):
            event.listen(engine, "connect", _tune_sqlite)
        _engine, _factory, _url = engine, sessionmaker(bind=engine, expire_on_commit=False, future=True), url
        return engine


def engine() -> Engine:
    """The database engine, created on first use."""
    return _engine or configure()


def create_all():
    """Create any missing tables. Alembic owns migrations; this is for tests and first boot."""
    Base.metadata.create_all(engine())


@contextlib.contextmanager
def session() -> Iterator[Session]:
    """A unit of work: commits on success, rolls back on any exception, always closes."""
    if _factory is None:
        configure()
    s = _factory()          # type: ignore[misc]
    try:
        yield s
        s.commit()
    except Exception:
        s.rollback()
        raise
    finally:
        s.close()


def healthy() -> bool:
    """Whether the database answers. Used by /api/health and by the boot check."""
    try:
        with session() as s:
            s.execute(text("SELECT 1"))
        return True
    except Exception:
        return False


def dispose():
    """Drop all pooled connections. Tests use this between throwaway databases."""
    global _engine, _factory, _url
    with _lock:
        if _engine is not None:
            _engine.dispose()
        _engine = _factory = _url = None
