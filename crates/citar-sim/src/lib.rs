//! The headless runner of CITAR games (DESIGN.md P2.4): whole games with a driver in every seat,
//! for the lab, the balance runner, `citar sim` and the statistical comparison with the Python
//! baseline.
//!
//! - [`runner`]: [`Runner`], which owns a game and its drivers and steps it one driven seat at a
//!   time (`Game::drive` with a seat limit of 1), so a host can check a time budget between seats,
//!   hear when the round changes, and in the bindings take the GIL back to deliver each step's
//!   events; [`run_game`], `engine_api.run_game`'s game to its end, and its [`RunResult`];
//! - [`baseline`]: the baseline writer's line, [`BaselineLine`], in the shape
//!   `scripts/refcheck/baseline.py` writes and `summarize.py` reads.
//!
//! A library, so that the bindings and later the helper link it; `src/main.rs` is the
//! developer's CLI.
//!
//! Package 2-00a wrote these signatures: [`Runner`] steps for real, while [`run_game`]'s result
//! and the baseline writer return [`SimError::NotYet`] until package 2-04 writes them and removes
//! the variant.

#![forbid(unsafe_code)]

pub mod baseline;
pub mod runner;

pub use self::baseline::{BaselineCiv, BaselineCrash, BaselineGame, BaselineLine, CheckpointRow};
pub use self::runner::{
    Crash, MajorRow, PlayerRow, RoundInfo, RunResult, RunSpec, Runner, SimError, Step, run_game,
};
