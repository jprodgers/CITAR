//! Layer 2: the persisted game model, and the types that describe a change to it.
//!
//! `State` is everything a save holds and nothing a cache can recompute (DESIGN.md 4, package
//! 1a-08). Its mutable accessors are reachable only from `game/mutate.rs`, `save/` and `compat/`,
//! and `cargo xtask check` enforces that.
//!
//! Replaces `citar/engine/state.py:13-405` and `game.py:32-60`.

pub mod change;
pub mod cities;
pub mod map;
pub mod memory;
pub mod store;
pub mod units;

pub use change::{Change, Changes, TileClaim};
