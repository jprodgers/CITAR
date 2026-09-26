//! Benchmarks of the CITAR engine, kept out of the engine's manifest (DESIGN.md 2.1, 9.7).
//!
//! Criterion suites measure wall clock on the laptop and gungraun suites count instructions in
//! CI. Later packages add the suites, `thresholds.toml` and `perfgate` (`cargo xtask perf`).
//!
//! Every game a benchmark times goes through [`unchecked`] first.

#![forbid(unsafe_code)]

use citar_engine::game::{DebugOptions, Game};

/// `g` with every check off, as a shipped build plays: what a benchmark times.
///
/// The bench builds testkit without its `checks` feature, but cargo unifies features across the
/// packages a command selects: a `cargo bench` at the workspace root also builds testkit with its
/// default features, which turn on the engine's `checks`, and `DebugOptions::default()` then runs
/// the invariants at every settle (DESIGN.md 9.4). Turning them off here keeps the numbers the
/// same whichever way the benches are run.
#[must_use]
pub fn unchecked(mut g: Game) -> Game {
    g.set_debug_options(DebugOptions::OFF);
    g
}

#[cfg(test)]
mod tests {
    use citar_engine::game::{DebugOptions, Game};
    use citar_engine::rules::Ruleset;

    #[test]
    fn a_benchmarked_game_runs_no_check_whatever_the_features() {
        let r = Ruleset::shared();
        let setup =
            Game::config_from_json(r, br#"{"map_size": "duel", "seed": 3}"#).expect("settings");
        let (g, _) = Game::new(r, &setup).expect("a new game");
        let g = super::unchecked(g);
        assert_eq!(g.debug_options(), DebugOptions::OFF);
    }
}
