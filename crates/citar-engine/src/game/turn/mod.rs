//! Turn flow (DESIGN.md 6.2, 6.7).
//!
//! - [`settle`]: the fixed point every successful write, and every settle point of a turn, ends
//!   in (package 1b-01);
//! - the stage tables, `end_turn`, `end_round` and the drivers join it in package 1b-03.
//!
//! Replaces `turns.py` (control flow in 1b-03) and the `g.invalidate()` calls at
//! `turns.py:38, 56, 87, 108, 115`, which become settle points.

pub mod settle;

pub use self::settle::SETTLE_PASSES;
