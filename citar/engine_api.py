"""The only door to the engine.

Everything in CITAR - the server, the agents, probes, benchmarks, the lab, balance runs, ``citar sim`` - reaches the
game through this module and nothing else (``tests/test_engine_boundary.py`` enforces it). Behind it is the Rust engine:
``citar/_facade_rust.py`` over the extension ``citar._engine`` (crates/citar-py; crates/citar-engine/DESIGN.md P2.6).
The Python engine it replaced, the Python bots and the switch that chose between the two engines were removed in 0.1.6
(package 2-12); the tag ``python-engine-0.1.6`` keeps them.

Rules for callers:

- What comes back is plain data - dicts, lists, strings, numbers - never a live engine object, so changing it changes
  nothing in the game.
- A refusal the caller can fix raises :class:`ActionError`. An internal error of the engine is :class:`EngineCrash` (a
  RuntimeError), never an ActionError: the game then refuses every command and still answers reads and saves.
- An ``EngineGame`` is no more thread-safe than the game it wraps: the session's lock is the caller's business. (The
  game also has a lock of its own, as insurance, never as the protocol.)

Two behaviours differ from the Python engine 0.1.5 shipped, and are documented where they apply: ``apply_ops`` is all or
nothing (a failing operation leaves the game as it was), and a subscriber hears a call's events after the call returns
rather than during it. Saves (``to_save``, ``state_dict``, and the ``.citar`` container with its journal:
``save_snapshot``, ``read_save``, ``save_header``, ``open_journal``) are the Rust engine's own format.
"""
# The names come from the star import below, which the import-time check holds to __all__.
# ruff: noqa: F405
from __future__ import annotations

# The whole public surface. A name that is not here is the facade's own business, and tests/test_engine_boundary.py
# fails a caller that reaches for it.
__all__ = [
    # errors, and text the prompts quote
    "ActionError", "MapError", "EngineCrash", "RULES_OVERVIEW", "MAP_LEGEND",
    # the ruleset and the tools
    "rules_version", "rules_client", "max_players", "map_sizes", "map_types", "speeds", "difficulties",
    "resolve_name", "ruleset_counts", "tool_list", "tool_kind", "state_summary",
    # maps and scenarios on disk
    "list_maps", "load_map", "save_map", "delete_map", "validate_map", "map_summary", "blank_map", "generate_map",
    "scenario_ops_help", "list_scenarios", "load_scenario", "scenario_summary", "delete_scenario",
    # bots and headless games
    "bot_instance", "bot_versions", "bot_schema", "bot_clean_params", "bot_fingerprint", "DIPLOMACY_CATEGORIES",
    "item_category", "proposal_categories", "bot_set_diplomacy", "bot_owns_negotiation", "run_game",
    # this build
    "build_info",
    # one game
    "DEBUG_ACTIONS", "EngineGame",
    # saves: the .citar container and its journal
    "open_journal", "fork_journal", "journal_in_use", "read_save", "save_header",
]

from ._facade_rust import *  # noqa: F403

_missing = [n for n in __all__ if n not in globals()]
if _missing:
    raise ImportError(f"citar._facade_rust lacks {', '.join(_missing)}")
del _missing
