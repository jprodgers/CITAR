//! Layer 4: the host surface, shaped like `citar/engine_api.py`.
//!
//! Host methods on `Game`, the tool registry, views, the briefing, scenarios, maps and debug
//! (DESIGN.md 8, packages 1d-01 to 1d-03). It may use every other layer; nothing may use it.
//!
//! Replaces `citar/engine/tools.py`, `views.py`, `briefing.py`, `maps.py`, `scenario.py` and
//! `game.py:891-990`, behind the facade of `citar/engine_api.py`.
//!
//! From package 1b-02:
//! - [`game`]: the host methods `apply_ops`, `meet`, `set_controller` and `set_difficulty`;
//! - [`scenario`]: the scenario operations (`scenario.py:36-487`);
//! - [`tools`]: the tools' argument specs and their coercion (`tools.py:113-127`);
//! - `inspect` and `testops` (feature `test-ops`): what rule scripts read and the test
//!   operations they set games up with (DESIGN.md 9.3). Never in a shipped build.
//!
//! From package 1b-04:
//! - [`maps`]: `generate_map`, a generated map as the editor's document (`maps.py:80-100`).
//!
//! From package 1d-01:
//! - [`tools`]: the registry of the 61 tools with their schemas, `Game::execute` and
//!   `Game::execute_query` (`tools.py:15-201`), and the query tools that need no view.
//!
//! From package 1d-02:
//! - [`views`]: the game as a player sees it (`views.py`, the view side of `game.py:880-990`):
//!   `Game::view_json`, the info builders the query tools answer from, `empire_summary`,
//!   `standings`, `path_preview`, the alerts and `replay_data`.

pub mod briefing;
pub mod game;
pub mod maps;
pub mod scenario;
pub mod text;
pub mod tools;
pub mod views;

#[cfg(feature = "test-ops")]
pub mod inspect;
#[cfg(feature = "test-ops")]
pub mod testops;

pub use crate::game::error::{ActionError, EngineError, ErrCode, text_rule_broken};
