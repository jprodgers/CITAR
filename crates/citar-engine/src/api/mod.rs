//! Layer 4: the host surface, shaped like `citar/engine_api.py`.
//!
//! Host methods on `Game`, the tool registry, views, the briefing, scenarios, maps and debug
//! (DESIGN.md 8, packages 1d-01 to 1d-03). It may use every other layer; nothing may use it.
//!
//! Replaces `citar/engine/tools.py`, `views.py`, `briefing.py`, `maps.py`, `scenario.py` and
//! `game.py:891-990`, behind the facade of `citar/engine_api.py`.
