//! Test support for the CITAR engine, and the home of all its integration tests.
//!
//! All integration tests live here rather than in `citar-engine`, which keeps only `#[cfg(test)]`
//! unit tests and doctests; that removes the dev-dependency cycle between the two (DESIGN.md 2.1).
//!
//! - [`agents`]: seat drivers for tests, `RandomAgent` among them (DESIGN.md 9.5);
//! - [`bots`]: bot seats that count what each of their turns did, for the bots' whole games
//!   (DESIGN.md P2.3.11);
//! - [`calls`]: tool calls as models make them, right and wrong, for every tool of the registry;
//! - [`chaos`]: the chaos driver of the `chaos` binary: random games with tool calls mixed in,
//!   every step checked, each failure kept as a replay (DESIGN.md 9.5);
//! - [`checks`]: the engine's invariants and cache oracle as testkit asks them;
//! - [`fixtures`]: the Python states refcheck recorded, for the converter's tests and goldens;
//! - [`fuzz`]: what the cargo-fuzz targets of `crates/citar-engine/fuzz` run;
//! - [`games`]: whole games, a `RandomAgent` in every seat or a Python state passed round after
//!   round, with a hook after every round, for the whole-game tests and golden sets;
//! - [`golden`]: the golden sets that must come out identical on all five targets, and the
//!   `golden` binary's logic (DESIGN.md 9.6);
//! - [`rulesets`]: test rulesets made from the shipped one by overlays, the kitchen sink among
//!   them, which uses every unique type the engine supports;
//! - [`script`]: the Rust runner of the rule scripts in `tests/rules/` (DESIGN.md 9.3);
//! - [`soak`]: the soak driver of the `soak` binary: long random games on every map size with
//!   the invariants on, reporting panics, violations, turn-time outliers and peak memory;
//! - [`spec`]: tool calls described by indices and bound to a game when made (`ActionSpec`);
//! - [`stability`]: the properties P1 to P8, as one runner the property tests, chaos and the
//!   fuzz targets share;
//! - [`states`]: synthetic game states with every field filled, for the save, digest and journal
//!   tests, the golden states and the digest benchmark.
//!
//! Package 1e-01 adds the `chaos` binary beside `golden`; 1e-02 adds `soak`.

#![forbid(unsafe_code)]

pub mod agents;
pub mod bots;
pub mod calls;
pub mod chaos;
pub mod checks;
pub mod fixtures;
pub mod fuzz;
pub mod games;
pub mod golden;
pub mod rulesets;
pub mod script;
pub mod soak;
pub mod spec;
pub mod stability;
pub mod states;
