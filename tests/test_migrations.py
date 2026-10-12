"""The 0.1.6 schema baseline and what the boot does with a database (design/phase3-features.md P3.2.2; package
3-01's gate 1).

* `0016a_baseline` and the revisions on top of it (one line, each with an upgrade test from the one before: package
  3-10's `0016b_compute_cache_tokens`) build exactly the tables of `citar/db/models.py`: an upgrade to head on an empty
  database, then alembic's `compare_metadata`, finds nothing; the three default hardware classes are seeded.
* A database at one of the two old revisions (`tests/data/citar-0.1.5.db`, built at d93345a before they were deleted)
  is renamed aside with its `-wal` and `-shm`, never migrated, and the boot comes up on a fresh one; a rename that
  fails refuses the boot and leaves the old database whole and in place.
* A database at a revision this build does not know refuses the boot, naming it; a version-less database made by
  `create_all` is stamped; anything else on Postgres is refused, since a server does not rename a database.

Every database here is a file of the test's own temporary directory.
"""
from __future__ import annotations

import os
import shutil
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401

from alembic.autogenerate import compare_metadata
from alembic.migration import MigrationContext
from sqlalchemy import create_engine, inspect
from sqlalchemy.engine import make_url

from citar import db
from citar.db import models
from citar.server import boot

ROOT = Path(__file__).resolve().parent.parent
OLD_DB = ROOT / "tests" / "data" / "citar-0.1.5.db"
#: The baseline (package 3-01) and the revisions on top of it, oldest first; the last is head.
REVISIONS = ("0016a_baseline", "0016b_compute_cache_tokens")
BASELINE, HEAD = REVISIONS[0], REVISIONS[-1]


def _sqlite(path: Path) -> str:
    return "sqlite:///" + path.as_posix()


def _revision(path: Path):
    con = sqlite3.connect(path)
    try:
        return [r[0] for r in con.execute("SELECT version_num FROM alembic_version")]
    finally:
        con.close()


def _tables(path: Path) -> set:
    con = sqlite3.connect(path)
    try:
        return {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    finally:
        con.close()


class BootCase(unittest.TestCase):
    """Each test boots against a database file of its own, and puts the process's engine back after."""

    def setUp(self):
        # resolved, as the boot resolves the path it names: Windows runners keep TEMP under an 8.3 short name
        # (C:\Users\RUNNER~1\...), which the boot's messages spell out in full
        self.dir = Path(tempfile.mkdtemp(prefix="citar_migrate_")).resolve()
        self.path = self.dir / "citar.db"
        self._previous = db._url

    def tearDown(self):
        db.dispose()
        if self._previous:
            db.configure(self._previous)
        shutil.rmtree(self.dir, ignore_errors=True)

    def boot(self):
        db.configure(_sqlite(self.path))
        return boot.migrate()

    def plant_old(self, path: Path = None):
        shutil.copyfile(OLD_DB, path or self.path)


class Baseline(BootCase):
    def test_an_empty_database_is_built_to_head_and_matches_the_models(self):
        self.assertIsNone(self.boot())
        self.assertEqual(_revision(self.path), [HEAD])
        engine = create_engine(_sqlite(self.path))
        try:
            with engine.connect() as conn:
                ctx = MigrationContext.configure(conn, opts={"compare_type": True, "compare_server_default": True})
                self.assertEqual(compare_metadata(ctx, models.Base.metadata), [])
        finally:
            engine.dispose()

    def test_the_default_hardware_classes_are_seeded(self):
        self.boot()
        con = sqlite3.connect(self.path)
        try:
            rows = con.execute("SELECT id, kind, name, rate, is_default FROM hardware_classes ORDER BY id").fetchall()
        finally:
            con.close()
        want = sorted((c["id"], c["kind"], c["name"], c["rate"], 1) for c in models.HARDWARE_CLASS_SEEDS)
        self.assertEqual(rows, want)
        self.assertEqual({c["kind"]: c["rate"] for c in models.HARDWARE_CLASS_SEEDS},
                         {"gpu": 1.0, "cpu": 0.05, "api": 1000.0})

    def test_the_revision_writes_out_the_seeds_the_models_name(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location(
            "baseline", ROOT / "citar" / "migrations" / "versions" / "0016a_baseline.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(tuple(module.HARDWARE_CLASSES), models.HARDWARE_CLASS_SEEDS)
        self.assertEqual((module.revision, module.down_revision), (BASELINE, None))

    def test_the_baseline_renders_for_postgres(self):
        """No Postgres here: alembic's offline mode writes the revision's SQL for the dialect without connecting,
        which is what an operator reviews before running it, and what shows every type has a Postgres form."""
        import io
        from alembic import command
        from alembic.config import Config
        from citar import paths
        out = io.StringIO()
        cfg = Config(output_buffer=out)
        cfg.set_main_option("script_location", str(paths.migrations_dir()))
        cfg.set_main_option("sqlalchemy.url", "postgresql://citar@localhost/citar")
        cfg.attributes["configure_logger"] = False
        command.upgrade(cfg, "head", sql=True)
        sql = out.getvalue()
        for table in models.Base.metadata.tables:
            self.assertIn(f"CREATE TABLE {table} (", sql)
        self.assertEqual(sql.count("INSERT INTO hardware_classes"), 3)
        self.assertIn("JSON", sql)
        self.assertIn("TIMESTAMP WITH TIME ZONE", sql)

    def test_the_revisions_are_one_line_from_the_baseline(self):
        """Nothing re-squashes (P3.2.2): a later change is a revision on top, each naming the one before."""
        import importlib.util
        folder = ROOT / "citar" / "migrations" / "versions"
        names = sorted(p.name for p in folder.glob("*.py"))
        self.assertEqual(names, [f"{r}.py" for r in REVISIONS])
        chain = []
        for name in names:
            spec = importlib.util.spec_from_file_location(name[:-3], folder / name)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            chain.append((module.revision, module.down_revision))
        self.assertEqual(chain, [(r, REVISIONS[i - 1] if i else None) for i, r in enumerate(REVISIONS)])

    def test_the_cache_tokens_revision_upgrades_a_baseline_database_keeping_its_events(self):
        """3-10's revision on a database at the baseline holding a compute event: the event is kept, with no cached
        tokens; the result matches the models."""
        from alembic import command
        from citar.db.models import ComputeEvent
        db.configure(_sqlite(self.path))
        cfg = boot._alembic_config(_sqlite(self.path))
        command.upgrade(cfg, BASELINE)
        con = sqlite3.connect(self.path)
        try:
            con.execute("INSERT INTO compute_events (at, activity, activity_kind, task_class, machine_id, kind, gen_s, "
                        "cpu_core_s, tokens_in, tokens_out, tokens_reasoning, credits, flags, self_use, verification) "
                        "VALUES ('2026-10-01 10:00:00', 'game:g1', 'game', 'full', 'sv_gpu', 'gpu', 2.5, 0, 100, 20, "
                        "0, 2.5, '{}', 0, 'unverified')")
            con.commit()
        finally:
            con.close()
        self.assertEqual(_revision(self.path), [BASELINE])
        self.assertIsNone(boot.migrate())
        self.assertEqual(_revision(self.path), [HEAD])
        with db.session() as s:
            ev = s.query(ComputeEvent).one()
            self.assertEqual((ev.activity, ev.gen_s, ev.tokens_in, ev.tokens_cache_read, ev.tokens_cache_write),
                             ("game:g1", 2.5, 100, 0, 0))
        db.dispose()
        engine = create_engine(_sqlite(self.path))
        try:
            with engine.connect() as conn:
                ctx = MigrationContext.configure(conn, opts={"compare_type": True, "compare_server_default": True})
                self.assertEqual(compare_metadata(ctx, models.Base.metadata), [])
        finally:
            engine.dispose()

    def test_a_database_at_head_boots_again_unchanged(self):
        self.boot()
        db.dispose()
        self.assertIsNone(self.boot())
        self.assertEqual(_revision(self.path), [HEAD])

    def test_create_all_seeds_the_classes_too(self):
        db.configure(_sqlite(self.path))
        db.create_all()
        db.create_all()         # twice: the seeds are added once
        con = sqlite3.connect(self.path)
        try:
            self.assertEqual(con.execute("SELECT count(*) FROM hardware_classes").fetchone()[0], 3)
        finally:
            con.close()


class OldDatabase(BootCase):
    def test_the_fixture_is_a_database_of_the_old_revisions(self):
        self.assertEqual(_revision(OLD_DB), ["767c824c2fcb"])
        self.assertIn("reports", _tables(OLD_DB))
        self.assertTrue(set(boot.OLD_REVISIONS) == {"f064316a6b34", "767c824c2fcb"})

    def _leave_logs(self):
        """What a process that crashed, or another that has the file open, leaves beside a database: SQLite
        removes its own log when the last connection closes, so they are made after the boot lets go of it."""
        real = boot._quiesce_sqlite

        def quiesce(engine):
            real(engine)
            for suffix in ("-wal", "-shm"):
                Path(str(self.path) + suffix).write_bytes(b"left by another process " + suffix.encode())
        return mock.patch.object(boot, "_quiesce_sqlite", quiesce)

    def test_an_old_database_is_renamed_aside_with_its_logs_and_a_fresh_one_made(self):
        self.plant_old()
        with self._leave_logs():
            notice = self.boot()
        aside = sorted(self.dir.glob("citar-0.1.5-*.db"))
        self.assertEqual(len(aside), 1, sorted(p.name for p in self.dir.iterdir()))
        old = aside[0]
        self.assertRegex(old.name, r"^citar-0\.1\.5-\d{8}-\d{6}\.db$")
        # its logs beside it, under the same name (read before anything opens the database: SQLite drops a log it
        # does not want when its last connection closes)
        for suffix in ("-wal", "-shm"):
            self.assertEqual(Path(str(old) + suffix).read_bytes(), b"left by another process " + suffix.encode())
            self.assertFalse(Path(str(self.path) + suffix).exists() and
                             Path(str(self.path) + suffix).read_bytes().startswith(b"left by"))
        # the old database, whole, under its new name
        self.assertEqual(_revision(old), ["767c824c2fcb"])
        con = sqlite3.connect(old)
        try:
            self.assertEqual(con.execute("SELECT handle FROM users").fetchall(), [("oldtimer",)])
        finally:
            con.close()
        # and a fresh one at the old path
        self.assertEqual(_revision(self.path), [HEAD])
        self.assertNotIn("reports", _tables(self.path))
        self.assertIn(str(old), notice)
        self.assertIn("767c824c2fcb", notice)

    def test_the_engine_serves_the_fresh_database_after(self):
        self.plant_old()
        self.boot()
        with db.session() as s:
            self.assertEqual(s.query(models.User).count(), 0)
            self.assertEqual(s.query(models.HardwareClass).count(), 3)

    def test_a_failed_rename_refuses_the_boot_and_leaves_the_old_database_whole(self):
        self.plant_old()
        before = OLD_DB.read_bytes()
        real = os.replace
        calls = []

        def replace(src, dst):
            calls.append((Path(src).name, Path(dst).name))
            if len(calls) == 2:            # the second file: the write-ahead log
                raise PermissionError(13, "The process cannot access the file because it is being used", str(src))
            return real(src, dst)

        with self._leave_logs(), mock.patch.object(boot.os, "replace", replace):
            with self.assertRaises(boot.BootError) as caught:
                self.boot()
        message = str(caught.exception)
        self.assertIn(str(self.path) + "-wal", message)
        self.assertIn("being used", message)
        # the first rename was undone: everything is where it was, nothing is called 0.1.5 (listed before anything
        # opens the database, which would drop the logs)
        self.assertEqual(calls[0][0], "citar.db")
        self.assertEqual(calls[1][0], "citar.db-wal")
        self.assertEqual(sorted(p.name for p in self.dir.iterdir()), ["citar.db", "citar.db-shm", "citar.db-wal"])
        self.assertEqual(Path(str(self.path) + "-wal").read_bytes(), b"left by another process -wal")
        self.assertEqual(_revision(self.path), ["767c824c2fcb"])
        con = sqlite3.connect(self.path)
        try:
            self.assertEqual(con.execute("SELECT handle FROM users").fetchall(), [("oldtimer",)])
        finally:
            con.close()
        self.assertGreater(len(before), 0)

    def test_two_boots_in_one_second_never_overwrite_an_archive(self):
        self.plant_old()
        self.boot()
        db.dispose()
        self.plant_old()
        self.boot()
        self.assertEqual(len(list(self.dir.glob("citar-0.1.5-*.db"))), 2)

    def test_old_tables_without_a_migration_history_are_set_aside(self):
        con = sqlite3.connect(self.path)
        con.execute("CREATE TABLE users (id TEXT PRIMARY KEY, handle TEXT)")
        con.commit()
        con.close()
        notice = self.boot()
        self.assertIn("no migration history", notice)
        self.assertEqual(_revision(self.path), [HEAD])
        self.assertEqual(len(list(self.dir.glob("citar-0.1.5-*.db"))), 1)


class OtherDatabases(BootCase):
    def test_a_newer_revision_refuses_the_boot_naming_it(self):
        con = sqlite3.connect(self.path)
        con.execute("CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL PRIMARY KEY)")
        con.execute("INSERT INTO alembic_version VALUES ('0017z_from_the_future')")
        con.execute("CREATE TABLE users (id TEXT PRIMARY KEY)")
        con.commit()
        con.close()
        with self.assertRaises(boot.BootError) as caught:
            self.boot()
        self.assertIn("0017z_from_the_future", str(caught.exception))
        self.assertIn("newer version", str(caught.exception))
        self.assertEqual(_revision(self.path), ["0017z_from_the_future"])    # untouched
        self.assertEqual(list(self.dir.glob("citar-0.1.5-*")), [])

    def test_a_version_less_database_made_by_create_all_is_stamped(self):
        db.configure(_sqlite(self.path))
        db.create_all()
        self.assertNotIn("alembic_version", _tables(self.path))
        self.assertIsNone(boot.migrate())
        self.assertEqual(_revision(self.path), [HEAD])
        self.assertEqual(list(self.dir.glob("citar-0.1.5-*")), [])

    def test_postgres_is_refused_not_renamed(self):
        class _Engine:
            url = make_url("postgresql+psycopg://citar:s3cret@db.example/citar")
        with mock.patch.object(boot, "_rename_aside") as rename, mock.patch.object(boot, "_quiesce_sqlite") as quiesce:
            with self.assertRaises(boot.BootError) as caught:
                boot._set_aside(None, _Engine(), "revision 767c824c2fcb")
        message = str(caught.exception)
        self.assertIn("CITAR_DB_URL", message)
        self.assertIn("767c824c2fcb", message)
        self.assertNotIn("s3cret", message)
        rename.assert_not_called()
        quiesce.assert_not_called()

    def test_postgres_old_revision_reaches_the_refusal(self):
        """The dialect branch is taken from the engine: an old revision on a database that is not SQLite goes to
        the refusal before anything is touched."""
        refused = boot.BootError("refused")
        with mock.patch.object(boot, "_current_revisions", return_value=["f064316a6b34"]), \
                mock.patch.object(boot, "_set_aside", side_effect=refused) as aside:
            self.plant_old()
            with self.assertRaises(boot.BootError):
                self.boot()
        self.assertIn("f064316a6b34", aside.call_args.args[2])


class Startup(BootCase):
    def test_the_notice_is_a_boot_warning(self):
        self.plant_old()
        with mock.patch.object(boot.settings, "get") as get, mock.patch.object(boot, "ensure_accounts", return_value=None), \
                mock.patch.object(boot, "check_configuration", return_value=[]), mock.patch("builtins.print") as printed:
            get.return_value = mock.Mock(db_url=_sqlite(self.path), describe=lambda: {})
            boot.startup()
        shown = " ".join(str(c.args[0]) for c in printed.call_args_list if c.args)
        self.assertIn("citar-0.1.5-", shown)
        self.assertTrue(inspect(db.engine()).has_table("owned_items"))


if __name__ == "__main__":
    unittest.main()
