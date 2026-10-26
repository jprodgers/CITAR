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
//! A library, so that the bindings, the jobs crate and the helper link it; `src/main.rs` is the
//! developer's CLI. Nothing here decides a game: the engine and the bot do. This crate keeps
//! time, reads and writes files and runs threads for the games it hosts. What it does to a game
//! all the same (a seat with no driver passes, a chat nobody answers expires, a baseline line's
//! sums) is covered by [`SIM_CODE`], which the jobs build hashes (design/phase4-helper.md P4.4.5).

#![forbid(unsafe_code)]

pub mod baseline;
pub mod panics;
pub mod play;
pub mod result;
pub mod runner;

pub use self::baseline::{BaselineCiv, BaselineCrash, BaselineGame, BaselineLine, CheckpointRow};
pub use self::result::{MajorRow, PlayerRow, RunResult};
pub use self::runner::{
    Crash, RoundInfo, RunSpec, Runner, Seats, SimError, Step, TRACEBACK_LIMIT, run_game,
};

/// The runner's content code (`build.rs`): 16 hex digits over its sources, its version and the
/// locked versions of what it links.
pub const SIM_CODE: &str = env!("CITAR_SIM_CODE");

// The content code's helper, here so that its tests run with the runner's.
#[cfg(test)]
#[path = "../content_code.rs"]
mod build_code;

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_code_helper_is_the_engines() {
        let norm = |s: &str| s.replace("\r\n", "\n");
        assert_eq!(
            norm(include_str!("../content_code.rs")),
            norm(include_str!("../../citar-engine/content_code.rs")),
            "crates/citar-sim/content_code.rs must stay a copy of the engine's"
        );
        assert_eq!(super::SIM_CODE.len(), 16);
    }
}
