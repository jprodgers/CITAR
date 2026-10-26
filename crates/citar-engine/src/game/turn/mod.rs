//! Turn flow (DESIGN.md 6.2, 6.7, 6.12).
//!
//! - [`settle`]: the fixed point every successful write, and every settle point of a turn, ends
//!   in (package 1b-01);
//! - [`stages`]: the stage tables of a player's turn and of the round's end, each row a system's
//!   step, `Pending` until its package ports it;
//! - [`driver`]: beginning and ending turns and rounds (`Game.begin_turn`, `Game.end_turn`), and
//!   the optional chain of round digests;
//! - [`drive`]: the seat drivers and the loop that plays them (package 1b-03).
//!
//! Replaces `turns.py:20-118, 190-202` and `game.py:1004-1037`, and the `g.invalidate()` calls at
//! `turns.py:38, 56, 87, 108, 115`, which become settle points.

pub mod drive;
pub mod driver;
pub mod settle;
pub mod stages;

pub use self::drive::{DriveOptions, DriverOutcome, Drivers, SeatDriver, Stop};
pub use self::settle::SETTLE_PASSES;
