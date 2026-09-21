"""Where CITAR keeps its files.

Every directory CITAR reads or writes is resolved here, once, so that the same code works whether it
was started from a git checkout or installed from a wheel.

Two kinds of location
---------------------
**Package data** ships inside the wheel and is read-only: the ruleset JSON, the browser client, the
database migrations, the hardware-collector scripts. It always sits next to this file.

**State** is everything CITAR writes: saved games, the server registry, benchmark runs, lab results,
reports, the usage ledger, the accounts database. Where that goes depends on how CITAR was started:

    git checkout      <checkout>/saves, <checkout>/config, <checkout>/benchmarks
    installed         a per-user state directory (see ``state_dir``)

The checkout case matters because it is the development workflow: a contributor's games, registry
and benchmark history sit beside the code where they can be inspected, diffed and deleted. The
installed case matters because ``site-packages`` is the wrong place to write a save file and is
often not writable at all — on Windows an installer puts it under ``Program Files``, and on Linux a
system-wide install is owned by root.

Overrides
---------
Environment variables win over both, and are how the systemd unit points a deployment at
``/var/lib/citar``:

======================  ====================================================================
``CITAR_STATE_DIR``     Base for all state. Sets saves, config and benchmarks in one go.
``CITAR_SAVE_DIR``      Saved games, and everything filed under them (maps, scenarios,
                        probes, lab, reports, usage, balance).
``CITAR_CONFIG_DIR``    The server registry (``servers.json``) and its siblings.
``CITAR_BENCH_DIR``     Benchmark suites and runs.
``CITAR_DATA_DIR``      Private state: the accounts database and the generated secret key.
                        Never the checkout, even in development — see ``data_dir``.
======================  ====================================================================

Nothing here creates a directory on import. Call ``ensure`` (or the ``*_dir`` helpers, which create
on demand) so that importing :mod:`citar.paths` stays free of side effects — tests and the ``--help``
path both rely on that.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

#: The installed package directory, ``…/citar``. Package data is resolved from here.
PACKAGE = Path(__file__).resolve().parent

#: The directory containing the package: a git checkout's root, or ``site-packages``.
ROOT = PACKAGE.parent

#: Files that mark ROOT as a CITAR source checkout rather than an install location.
_CHECKOUT_MARKERS = ("pyproject.toml", "DESIGN.md")


def _env(name: str) -> str:
    """An environment variable, stripped, or an empty string."""
    return (os.environ.get(name) or "").strip()


# --------------------------------------------------------------------------- package data

def package_data() -> Path:
    """The ruleset and other JSON shipped with the package (``citar/data``)."""
    return PACKAGE / "data"


def web_dir() -> Path:
    """The browser client served as static files (``citar/web``)."""
    return PACKAGE / "web"


def migrations_dir() -> Path:
    """The Alembic migration environment (``citar/migrations``)."""
    return PACKAGE / "migrations"


def collectors_dir() -> Path:
    """Hardware-collector scripts offered for download on the Servers page."""
    return PACKAGE / "data" / "collectors"


# --------------------------------------------------------------------------- state

def in_source_checkout() -> bool:
    """True when CITAR is running from a source tree that it may write to.

    Both halves matter. A marker file alone is not enough — a checkout mounted read-only, or one
    owned by another user, would pass that test and then fail on the first save. So the directory is
    probed for writability as well, and anything unexpected counts as "not a checkout", which falls
    back to the per-user directory and always works.
    """
    if not any((ROOT / marker).exists() for marker in _CHECKOUT_MARKERS):
        return False
    return os.access(ROOT, os.W_OK)


def state_dir() -> Path:
    """The base directory for everything CITAR writes.

    ``CITAR_STATE_DIR`` wins; then a writable source checkout; then the per-user directory that
    :func:`data_dir` also uses. The result is not created here — the callers below do that for the
    specific subdirectory they need.
    """
    override = _env("CITAR_STATE_DIR")
    if override:
        return Path(override).expanduser()
    if in_source_checkout():
        return ROOT
    return _user_base()


def _user_base() -> Path:
    """The per-user directory for this platform, without creating it.

    These are the conventional locations: anything that backs up a user profile picks them up, and
    on Linux ``XDG_DATA_HOME`` is honoured for people who have moved theirs.
    """
    override = _env("CITAR_DATA_DIR")
    if override:
        return Path(override).expanduser()
    if sys.platform == "win32":
        base = Path(_env("LOCALAPPDATA") or Path.home() / "AppData" / "Local") / "CITAR"
    elif sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support" / "CITAR"
    else:
        base = Path(_env("XDG_DATA_HOME") or Path.home() / ".local" / "share") / "citar"
    return base


def ensure(path: Path) -> Path:
    """Create *path* (and its parents) if it does not exist, and return it."""
    path.mkdir(parents=True, exist_ok=True)
    return path


def save_dir() -> Path:
    """Saved games, and the folders filed beneath them (maps, scenarios, probes, lab, reports)."""
    override = _env("CITAR_SAVE_DIR")
    return ensure(Path(override).expanduser() if override else state_dir() / "saves")


def config_dir() -> Path:
    """The server registry and other operator-edited configuration."""
    override = _env("CITAR_CONFIG_DIR")
    return ensure(Path(override).expanduser() if override else state_dir() / "config")


def bench_dir() -> Path:
    """Benchmark suites, runs and scoring settings.

    ``CITAR_SAVE_DIR`` is honoured as a fallback so that a test or deployment which redirects saves
    does not leave benchmark runs behind in a different tree; this was the existing behaviour and
    tests depend on it.
    """
    override = _env("CITAR_BENCH_DIR")
    if override:
        return ensure(Path(override).expanduser())
    saves = _env("CITAR_SAVE_DIR")
    if saves:
        return ensure(Path(saves).expanduser().parent / "benchmarks")
    return ensure(state_dir() / "benchmarks")


def data_dir() -> Path:
    """Private state: the accounts database and the locally generated secret key.

    This is deliberately *not* the checkout, even in development. The project folder may sync to
    OneDrive or Dropbox, and a sync client copying a SQLite file mid-write corrupts it; password
    hashes and session tokens do not belong in a synced folder either.

    An explicit ``CITAR_STATE_DIR`` is honoured, because somebody who names one directory for all of
    CITAR's state means this too — that is how the systemd unit points a deployment at
    ``/var/lib/citar``. What never happens is falling into a source checkout by accident.
    """
    if _env("CITAR_DATA_DIR"):
        return ensure(Path(_env("CITAR_DATA_DIR")).expanduser())
    if _env("CITAR_STATE_DIR"):
        return ensure(Path(_env("CITAR_STATE_DIR")).expanduser())
    return ensure(_user_base())


def sub(name: str) -> Path:
    """A named subdirectory of :func:`save_dir`, created on demand.

    ``sub("lab")`` is how the lab, probes, reports, usage ledger and balance reports find their
    homes, so that redirecting ``CITAR_SAVE_DIR`` moves all of them together.
    """
    return ensure(save_dir() / name)


# --------------------------------------------------------------------------- non-creating variants
#
# Modules resolve their directory once, at import, into a module-level constant. Those must not
# create anything: importing the engine to read a rule, or running ``citar --help``, should not
# leave an empty ``saves/`` behind, and a read-only environment should not fail at import time.
# Each write site already creates what it needs.

def saves_path(*parts: str) -> Path:
    """A path under the save directory, without creating anything."""
    override = _env("CITAR_SAVE_DIR")
    base = Path(override).expanduser() if override else state_dir() / "saves"
    return base.joinpath(*parts)


def config_path(*parts: str) -> Path:
    """A path under the configuration directory, without creating anything."""
    override = _env("CITAR_CONFIG_DIR")
    base = Path(override).expanduser() if override else state_dir() / "config"
    return base.joinpath(*parts)


def bench_path(*parts: str) -> Path:
    """A path under the benchmark directory, without creating anything."""
    override = _env("CITAR_BENCH_DIR")
    if override:
        base = Path(override).expanduser()
    else:
        saves = _env("CITAR_SAVE_DIR")
        base = Path(saves).expanduser().parent / "benchmarks" if saves else state_dir() / "benchmarks"
    return base.joinpath(*parts)


def describe() -> dict[str, str]:
    """Every resolved location, for ``citar doctor`` and bug reports."""
    return {
        "package": str(PACKAGE),
        "source checkout": "yes" if in_source_checkout() else "no (installed)",
        "state": str(state_dir()),
        "saves": str(save_dir()),
        "config": str(config_dir()),
        "benchmarks": str(bench_dir()),
        "private data": str(data_dir()),
    }
