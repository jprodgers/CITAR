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

pub mod game;
pub mod scenario;
pub mod tools;

#[cfg(feature = "test-ops")]
pub mod inspect;
#[cfg(feature = "test-ops")]
pub mod testops;

pub use crate::game::error::{ActionError, EngineError, ErrCode};
