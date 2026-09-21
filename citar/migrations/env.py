"""Alembic environment for CITAR.

The URL comes from citar.settings rather than alembic.ini, so migrations follow whatever database
the server itself would use — the local SQLite file, or the VPS's Postgres — with no checked-in
credentials.

`render_as_batch` is on because SQLite cannot ALTER a column in place: batch mode rewrites the table
instead. Without it, any future migration that changes a column type or a constraint would work on
Postgres and fail on SQLite, which is exactly the divergence we are trying to avoid.
"""
from __future__ import annotations

import sys
from logging.config import fileConfig
from pathlib import Path

from alembic import context
from sqlalchemy import engine_from_config, pool

# The directory that contains the ``citar`` package: a checkout root, or site-packages. Needed
# for the ``alembic`` CLI, which starts with only the migration directory on the path; when the
# server applies migrations in-process the package is already imported.
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from citar import settings
from citar.db.models import Base

config = context.config
# disable_existing_loggers defaults to True, which silences every logger already configured in the
# process. When the server applies migrations at startup that includes uvicorn's — so an
# application-startup failure right afterwards produced a crash loop with no error message
# anywhere, in the logs or on the console. `configure_logger` lets the in-process caller opt out
# entirely; the CLI still gets alembic's own logging.
if config.config_file_name is not None and config.attributes.get("configure_logger", True):
    fileConfig(config.config_file_name, disable_existing_loggers=False)

target_metadata = Base.metadata


def _url() -> str:
    """The database URL, taken from CITAR's settings rather than from alembic.ini.

    So the same migration commands work against the local SQLite file and a deployment's PostgreSQL,
    with no credentials in a checked-in file.
    """
    return config.get_main_option("sqlalchemy.url") or settings.get().db_url


def run_migrations_offline() -> None:
    """Generate SQL without connecting, for reviewing a migration before it runs."""
    context.configure(url=_url(), target_metadata=target_metadata, literal_binds=True,
                      dialect_opts={"paramstyle": "named"}, render_as_batch=True,
                      compare_type=True, compare_server_default=True)
    with context.begin_transaction():
        context.run_migrations()


def run_migrations_online() -> None:
    """Apply migrations against the database."""
    section = config.get_section(config.config_ini_section) or {}
    section["sqlalchemy.url"] = _url()
    connectable = engine_from_config(section, prefix="sqlalchemy.", poolclass=pool.NullPool)
    with connectable.connect() as connection:
        context.configure(connection=connection, target_metadata=target_metadata,
                          render_as_batch=True, compare_type=True, compare_server_default=True)
        with context.begin_transaction():
            context.run_migrations()
    connectable.dispose()


if context.is_offline_mode():
    run_migrations_offline()
else:
    run_migrations_online()
