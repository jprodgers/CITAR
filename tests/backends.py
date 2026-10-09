"""Markers for the suite's two backends (crates/citar-engine/DESIGN.md P2.7.4).

The suite runs on whichever backend ``CITAR_ENGINE`` chooses (``citar.engine_api.BACKEND``): the Rust engine by
default since package 2-09, which makes CI's main run. On the Python backend CI runs only the reference subset
(``tests/python_reference.txt``). Most tests pass on both. The others say why they do not, with a marker that
tests/test_backends.py holds to account:

- ``@python_engine_only("<successor>")``: the test pokes the Python engine, whose behaviour now lives in the named
  successor: a rule script (``tests/rules/<name>.toml``; one whose ``needs`` names a bot package still to come runs on
  Rust once that package lands), a Rust test (a ``fn`` of that name in ``crates/**/*.rs`` with a test attribute) or
  a Python test (``tests.<module>.<Class>.<test>``). Skipped on Rust; deleted with the Python engine in 2-12.
- ``@rust_pending("<package>")``: a test the named package (2-11, the last) makes pass on Rust. Skipped on Rust
  until then; that package's gate is that no marker names it any more, and it leaves ``PENDING_PACKAGES``.
- ``@rust_only``: a test of a name only the Rust backend has. Skipped on Python.

Each marker works on a test method or a whole TestCase class, and records what it says on the object (``_backend``),
so the meta-test can find every one without running anything.
"""
from __future__ import annotations

import re
import unittest

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
from citar import engine_api

#: Whether the suite runs on the Rust backend.
RUST = engine_api.BACKEND == "rust"
#: The packages a rust_pending marker may name: the ones still to finish the swap's Python side. A package leaves
#: this list when it lands (2-09, the server, and 2-10, the bots, the lab and the ladder, have), so no marker can
#: name it again.
PENDING_PACKAGES = ("2-11",)
#: The bot packages still to come, whose rule scripts carry a ``needs`` header that both runners skip on Rust: a
#: python_engine_only successor may be such a script until its package lands. A package removes itself here when it
#: removes its headers. None is left: 2-05, diplomacy, the last, has, so every successor runs on Rust.
BOT_PACKAGES_TO_COME: tuple = ()
_PACKAGE = re.compile(r"[0-9]-[0-9]{2}[a-z]?")


def _mark(target, kind: str, value):
    """Record a marker on a test or a class, for the meta-test."""
    marks = list(getattr(target, "_backend", ()))
    marks.append((kind, value))
    target._backend = tuple(marks)
    return target


def python_engine_only(successor: str):
    """Skip on Rust: the test pokes the Python engine, and ``successor`` names where its behaviour is tested now."""
    if not isinstance(successor, str) or not successor.strip():
        raise ValueError("python_engine_only names its successor")

    def deco(target):
        skipped = unittest.skipIf(RUST, f"Python engine only; the successor is {successor}")(target)
        return _mark(skipped, "python_engine_only", successor)
    return deco


def rust_pending(package: str):
    """Skip on Rust until ``package`` makes the test pass there."""
    if not isinstance(package, str) or not _PACKAGE.fullmatch(package):
        raise ValueError(f"rust_pending names a package, as '2-11', not {package!r}")

    def deco(target):
        skipped = unittest.skipIf(RUST, f"passes on Rust from package {package}")(target)
        return _mark(skipped, "rust_pending", package)
    return deco


def rust_only(target):
    """Skip on Python: the test uses a name only the Rust backend has."""
    skipped = unittest.skipUnless(RUST, "a name only the Rust backend has (CITAR_ENGINE=rust)")(target)
    return _mark(skipped, "rust_only", None)


def has_test_ops() -> bool:
    """Whether the Rust backend's build has the test operations (always true on Python, whose engine has them)."""
    if not RUST:
        return True
    from citar import _engine
    return bool(_engine.HAS_TEST_OPS)
