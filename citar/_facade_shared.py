"""What the facade's two backends share (crates/citar-engine/DESIGN.md P2.6.6): one error class, so that
``except engine_api.BackendError`` means the same on either side.

``citar.engine_api`` is the door; this module, ``citar/engine/facade.py`` (the Python engine) and
``citar/_facade_rust.py`` (``citar._engine``) are behind it, and nothing else in the package imports them
(tests/test_engine_boundary.py). It imports nothing, so either backend can load without the other: the Python one
without the extension built, the Rust one after package 2-12 deletes the Python engine.
"""


class BackendError(NotImplementedError):
    """A facade name this backend does not have: a name Phase 2 added, on the Python backend ("Rust backend only"),
    or a Python engine's internal, on the Rust backend ("Python backend only"). A caller that meets it is running on
    the wrong backend for what it asks; tests that use such a name are marked (tests/backends.py)."""


def rust_only(name: str) -> BackendError:
    """The error a Rust-only name raises on the Python backend."""
    return BackendError(f"{name}: Rust backend only (set CITAR_ENGINE=rust).")


def python_only(name: str) -> BackendError:
    """The error a Python engine's internal raises on the Rust backend."""
    return BackendError(f"{name}: Python backend only; the Rust engine has no such thing.")
