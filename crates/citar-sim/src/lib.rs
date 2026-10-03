//! The headless runner of CITAR games (DESIGN.md P2.4): whole games with a driver in every seat,
//! for the lab, the balance runner, `citar sim` and the statistical comparison with the Python
//! baseline.
//!
//! - [`runner`]: [`Runner`], which owns a game and its drivers and steps it one driven seat at a
//!   time (`Game::drive` with a seat limit of 1), so a host can check a time budget between seats,
//!   hear when the round changes, and in the bindings take the GIL back to deliver each step's
//!   events; [`run_game`], `engine_api.run_game`'s game to its end;
//! - [`result`]: [`RunResult`], `engine_api.run_game`'s dict (`citar/bots/headless.py`);
//! - [`baseline`]: the baseline writer (`scripts/refcheck/baseline.py`) and its line,
//!   [`BaselineLine`], which `summarize.py` reads;
//! - [`play`]: `citar-sim play`, one game printed as `citar sim` prints it (`citar/sim.py`);
//! - [`panics`]: where a caught panic happened, for its crash record.
//!
//! A library, so that the bindings and later the helper link it; `src/main.rs` is the
//! developer's CLI. Nothing here decides a game: the engine and the bot do. This crate keeps
//! time, reads and writes files and runs threads for the games it hosts.

#![forbid(unsafe_code)]

pub mod baseline;
pub mod panics;
pub mod play;
pub mod result;
pub mod runner;

pub use self::baseline::{BaselineCiv, BaselineCrash, BaselineGame, BaselineLine, CheckpointRow};
pub use self::result::{MajorRow, PlayerRow, RunResult};
pub use self::runner::{
    Crash, RoundInfo, RunSpec, Runner, SimError, Step, TRACEBACK_LIMIT, run_game,
};
