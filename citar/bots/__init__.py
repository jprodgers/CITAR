"""The scripted opponent, and the frozen copies of it that old experiments still play against.

:mod:`citar.bots.basic` is the bot. It is deliberately not a learned model: a few thousand lines of
heuristics that can be read, argued with and changed on purpose. That matters because the bot is
the **yardstick** — every model score in CITAR is a comparison against it, so what it does is the
unit the whole benchmark is denominated in, and a unit nobody can inspect is not much of a unit.

``frozen_<hash>.py`` modules are snapshots taken when a lab experiment was submitted. They exist so
that editing ``basic.py`` cannot change what a running experiment is measuring against half way
through, and so that a result from last month can be reproduced next year. They are never edited,
and the linter is told to leave them alone.

See ``docs/BOTS.md`` for what the bot does and how to change it without fooling yourself about the
result.
"""
