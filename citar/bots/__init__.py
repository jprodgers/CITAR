"""Bookkeeping about the scripted opponent: its profiles (:mod:`citar.bots.profiles`) and its ratings
(:mod:`citar.bots.ratings`).

The bot itself is compiled into the engine (``crates/citar-bot``): versions such as ``basic-1``, each a deliberate
copy with its own parameter schema, which games reach through ``citar.engine_api`` (``bot_instance``). It is
deliberately not a learned model: a few thousand lines of heuristics that can be read, argued with and changed on
purpose. That matters because the bot is the **yardstick** - every model score in CITAR is a comparison against it,
so what it does is the unit the whole benchmark is denominated in, and a unit nobody can inspect is not much of a
unit.

0.1.5's Python bot and its frozen snapshots were archived with 0.1.5 (the tag ``python-engine-0.1.6`` keeps them): a
profile or lab seat that names a snapshot is refused as archived.

See ``docs/BOTS.md`` for what the bot does and how to change it without fooling yourself about the result.
"""
