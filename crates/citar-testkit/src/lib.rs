//! Test support for the CITAR engine, and the home of all its integration tests.
//!
//! All integration tests live here rather than in `citar-engine`, which keeps only `#[cfg(test)]`
//! unit tests and doctests; that removes the dev-dependency cycle between the two (DESIGN.md 2.1).
//!
//! - [`golden`]: the golden sets that must come out identical on all five targets, and the
//!   `golden` binary's logic (DESIGN.md 9.6);
//! - [`rulesets`]: test rulesets made from the shipped one by overlays, the kitchen sink among
//!   them, which uses every unique type the engine supports;
//! - [`states`]: synthetic game states with every field filled, for the save, digest and journal
//!   tests, the golden states and the digest benchmark.
//!
//! Later packages add the rule-script runner, fixture loading, `RandomAgent`, and the `chaos` and
//! `soak` binaries.

#![forbid(unsafe_code)]

pub mod golden;
pub mod rulesets;
pub mod states;
