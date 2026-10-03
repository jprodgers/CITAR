"""CITAR — Civ Inspired Tool for AI Research.

A turn-based 4X game with a Civilization V ruleset (derived from UnCiv, MPL-2.0) whose players can
be humans in a browser, scripted bots, or language models driven over MCP, the Anthropic API, or any
OpenAI-compatible endpoint. It exists to measure how models play a long, stateful game with
imperfect information: benchmarks, scenario probes, per-turn metrics and costed reports are part of
the program rather than bolted on.

Start here
----------
``citar.engine``   the rules, with no I/O — the map, cities, combat, diplomacy, victory
``citar.engine_api``  the only door to the engine for everything else
``citar.agents``   the adapters that let a model take a seat
``citar.server``   the FastAPI app, session manager and benchmark scheduler
``citar.paths``    where files live, whether run from a checkout or an installed wheel

The command line is ``citar`` (see :mod:`citar.cli`); ``citar serve`` starts the game server and
``citar setup`` walks through first-time configuration.
"""

__version__ = "0.1.5"
__all__ = ["__version__"]


def _ext_dir_first():
    """Put ``CITAR_EXT_DIR``, when set, first on the package's path.

    The laptop's dev loop (``cargo xtask develop``, CONTRIBUTING.md) builds the Rust extension outside the checkout,
    which sits in a synced folder that locks files mid-build, and a running server would hold a copy built into the
    tree. With the folder first, ``import citar._engine`` finds the library built there while every other module
    still comes from the checkout: the folder holds nothing else.
    """
    import os
    ext = os.environ.get("CITAR_EXT_DIR")
    if ext and ext not in __path__:
        __path__.insert(0, ext)


_ext_dir_first()
del _ext_dir_first
