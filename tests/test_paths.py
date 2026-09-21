"""Where CITAR puts its files.

These are release tests. Before packaging, every module resolved its own directory from
``__file__``, which works in a checkout and puts saved games inside ``site-packages`` once CITAR is
installed from a wheel — a directory that is the wrong place for them and is often not writable at
all. The rules in :mod:`citar.paths` are what stop that, so they are pinned here:

* a source checkout keeps state beside the code, because that is the development workflow;
* an installed copy uses the per-user directory for the platform;
* the environment overrides both, which is how the systemd unit points at ``/var/lib/citar``;
* the private data directory never lands in a checkout, even when everything else does, because a
  synced project folder corrupts a live SQLite database.
"""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401

from citar import paths

ENV_KEYS = ("CITAR_STATE_DIR", "CITAR_SAVE_DIR", "CITAR_CONFIG_DIR",
            "CITAR_BENCH_DIR", "CITAR_DATA_DIR")


class _Env:
    """Set exactly the CITAR path variables named, and restore everything afterwards.

    The test package sets CITAR_SAVE_DIR and CITAR_CONFIG_DIR for every test module, so a test of
    the unset behaviour has to clear them rather than merely not set them.
    """

    def __init__(self, **values):
        self.values = values
        self.saved: dict = {}

    def __enter__(self):
        for key in ENV_KEYS:
            self.saved[key] = os.environ.get(key)
            os.environ.pop(key, None)
        for key, value in self.values.items():
            os.environ[key] = str(value)
        return self

    def __exit__(self, *exc):
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


class PackageData(unittest.TestCase):
    def test_data_travels_with_the_package(self):
        # Resolved from the module's own location, never from the working directory: an installed
        # copy has no checkout to look in.
        self.assertTrue((paths.package_data() / "ruleset").is_dir())
        self.assertTrue((paths.web_dir() / "index.html").is_file())
        self.assertTrue((paths.migrations_dir() / "env.py").is_file())
        self.assertTrue(paths.collectors_dir().is_dir())

    def test_collectors_are_present(self):
        # These are downloaded from the Servers page. A packaging change that dropped them would
        # only show up as a 404 in the browser, which is a slow way to find out.
        names = {p.name for p in paths.collectors_dir().iterdir()}
        self.assertIn("collect_hardware.sh", names)
        self.assertIn("collect_hardware.ps1", names)


class StateDirectory(unittest.TestCase):
    def test_state_dir_wins_over_the_checkout(self):
        with tempfile.TemporaryDirectory() as tmp:
            with _Env(CITAR_STATE_DIR=tmp):
                self.assertEqual(paths.state_dir(), Path(tmp))
                self.assertEqual(paths.saves_path(), Path(tmp) / "saves")
                self.assertEqual(paths.config_path(), Path(tmp) / "config")
                self.assertEqual(paths.bench_path(), Path(tmp) / "benchmarks")

    def test_specific_overrides_beat_the_general_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            state, saves = Path(tmp) / "state", Path(tmp) / "elsewhere"
            with _Env(CITAR_STATE_DIR=state, CITAR_SAVE_DIR=saves):
                self.assertEqual(paths.saves_path(), saves)
                self.assertEqual(paths.config_path(), state / "config")

    def test_benchmarks_follow_a_redirected_save_dir(self):
        # Long-standing behaviour the test suite depends on: redirecting saves must not leave
        # benchmark runs behind in a different tree.
        with tempfile.TemporaryDirectory() as tmp:
            saves = Path(tmp) / "saves"
            with _Env(CITAR_SAVE_DIR=saves):
                self.assertEqual(paths.bench_path(), Path(tmp) / "benchmarks")

    def test_running_from_this_checkout_is_detected(self):
        # The tests themselves run from a checkout, so this is the real thing rather than a mock.
        with _Env():
            self.assertTrue(paths.in_source_checkout())
            self.assertEqual(paths.state_dir(), paths.ROOT)

    def test_an_installed_copy_uses_the_per_user_directory(self):
        # Simulated by pointing ROOT at a directory with no checkout markers in it, which is what
        # site-packages looks like.
        with tempfile.TemporaryDirectory() as tmp:
            original = paths.ROOT
            try:
                paths.ROOT = Path(tmp)
                with _Env():
                    self.assertFalse(paths.in_source_checkout())
                    self.assertEqual(paths.state_dir(), paths._user_base())
                    self.assertNotIn(str(Path(tmp)), str(paths.state_dir()))
            finally:
                paths.ROOT = original


class PrivateData(unittest.TestCase):
    def test_never_the_checkout(self):
        """The database and secret key stay out of a synced project folder.

        A sync client copying a SQLite file mid-write corrupts it, and password hashes do not
        belong in a folder that syncs to somebody's cloud drive. So this one directory ignores the
        checkout even though every other one honours it.
        """
        with _Env():
            self.assertTrue(paths.in_source_checkout())
            self.assertNotEqual(paths.data_dir(), paths.ROOT)

    def test_state_dir_is_honoured_for_private_data_too(self):
        # A deployment that names one directory for all of CITAR's state means this as well.
        with tempfile.TemporaryDirectory() as tmp:
            with _Env(CITAR_STATE_DIR=tmp):
                self.assertEqual(paths.data_dir(), Path(tmp))

    def test_explicit_data_dir_wins(self):
        with tempfile.TemporaryDirectory() as tmp:
            state, data = Path(tmp) / "state", Path(tmp) / "private"
            with _Env(CITAR_STATE_DIR=state, CITAR_DATA_DIR=data):
                self.assertEqual(paths.data_dir(), data)


class NoImportSideEffects(unittest.TestCase):
    def test_path_helpers_do_not_create_directories(self):
        """``*_path`` resolves; ``*_dir`` creates.

        Modules resolve their directory once at import into a module-level constant. If that
        created the directory, importing the engine to read a rule would leave an empty ``saves/``
        behind, and a read-only environment would fail at import time rather than at the first
        write.
        """
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "untouched"
            with _Env(CITAR_STATE_DIR=target):
                paths.saves_path("maps")
                paths.config_path("servers.json")
                paths.bench_path("runs")
                self.assertFalse(target.exists())
                self.assertTrue(paths.save_dir().exists())


class Describe(unittest.TestCase):
    def test_describe_covers_every_location(self):
        # `citar where` and `citar doctor` print this, and it is what a bug report should contain.
        described = paths.describe()
        for key in ("package", "source checkout", "state", "saves", "config", "benchmarks",
                    "private data"):
            self.assertIn(key, described)
            self.assertTrue(described[key])


if __name__ == "__main__":
    unittest.main()
