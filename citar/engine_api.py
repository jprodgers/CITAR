"""The only door to the engine.

Everything outside ``citar/engine`` and ``citar/bots`` - the server, the agents, probes, benchmarks, the lab, balance
runs, ``citar sim`` - reaches the game through this module and nothing else (``tests/test_engine_boundary.py``
enforces it). Behind it are two backends with one surface (crates/citar-engine/DESIGN.md P2.6.6):

- ``python``: the Python engine (``citar/engine/facade.py``), frozen at the 38 names below and EngineGame's methods
  as they stood before Phase 2; the names Phase 2 added raise :class:`BackendError` there. It is deleted with the
  Python engine in package 2-12.
- ``rust``: the Rust engine (``citar/_facade_rust.py`` over the extension ``citar._engine``), with every name.

``CITAR_ENGINE`` chooses, read once at import: ``python`` (the default until package 2-09 flips it) or ``rust``.
Nothing outside the facade and the tests reads :data:`BACKEND`; a caller that needs a Phase 2 name uses it and lets a
:class:`BackendError` say it runs on the wrong backend.

Rules for callers, on either backend:

- What comes back is plain data - dicts, lists, strings, numbers - never a live engine object, so changing it changes
  nothing in the game. Where the Python backend hands out a live value on a hot path the docstring says "live": treat
  those values as read-only.
- A refusal the caller can fix raises :class:`ActionError`, so ``except ActionError`` works on either backend. An
  internal error of the Rust engine is :class:`EngineCrash` (a RuntimeError), never an ActionError: the game then
  refuses every command and still answers reads and saves.
- An ``EngineGame`` is no more thread-safe than the game it wraps: the session's lock is the caller's business. (The
  Rust game also has a lock of its own, as insurance, never as the protocol.)

The Rust backend differs from the Python one in two documented ways: ``apply_ops`` is all or nothing (a failing
operation leaves the game as it was), and a subscriber hears a call's events after the call returns rather than
during it. Its saves (``to_save``, ``state_dict``) are the Rust engine's own format.
"""
# The names come from the backend's star import below, which the import-time check holds to __all__.
# ruff: noqa: F405
from __future__ import annotations

import os

#: Which engine is behind the door: "python" or "rust" (``CITAR_ENGINE``). For the facade and the tests only.
BACKEND = os.environ.get("CITAR_ENGINE", "python").strip().lower() or "python"

# The whole public surface, the same on both backends. A name that is not here is backend, and
# tests/test_engine_boundary.py fails a caller that reaches for it.
__all__ = [
    # errors, and text the prompts quote
    "ActionError", "MapError", "RULES_OVERVIEW", "MAP_LEGEND",
    # the ruleset and the tools
    "rules_version", "rules_client", "max_players", "map_sizes", "map_types", "speeds", "difficulties",
    "resolve_name", "ruleset_counts", "tool_list", "tool_kind", "state_summary",
    # maps and scenarios on disk
    "list_maps", "load_map", "save_map", "delete_map", "validate_map", "map_summary", "blank_map", "generate_map",
    "scenario_ops_help", "list_scenarios", "load_scenario", "scenario_summary", "delete_scenario",
    # bots and headless games
    "bot_instance", "DIPLOMACY_CATEGORIES", "item_category", "proposal_categories", "bot_set_diplomacy",
    "bot_owns_negotiation", "run_game",
    # one game
    "DEBUG_ACTIONS", "EngineGame",
    # Phase 2 (package 2-08): the errors both backends define, and the names only the Rust backend has
    "EngineCrash", "BackendError", "build_info", "bot_versions", "bot_schema", "bot_clean_params", "bot_fingerprint",
    # which backend this is
    "BACKEND",
]

# Each backend's own __all__ is this list but BACKEND (tests/test_engine_boundary.py holds them to it).
if BACKEND == "python":
    from .engine.facade import *  # noqa: F403
elif BACKEND == "rust":
    from ._facade_rust import *  # noqa: F403
else:
    # named for this module, so that a caller (citar doctor) can tell a wrong choice from a broken install
    raise ImportError(f"CITAR_ENGINE={BACKEND!r} is no engine backend: use 'python' or 'rust'.", name=__name__)

_missing = [n for n in __all__ if n not in globals()]
if _missing:
    raise ImportError(f"the {BACKEND} backend of citar.engine_api lacks {', '.join(_missing)}")
del _missing
