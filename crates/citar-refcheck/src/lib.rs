//! Reference checks: the Rust engine's answers compared with the recorded Python answers.
//!
//! The Rust side of `refcheck/README.md` (DESIGN.md 9.2). A run:
//!
//! 1. loads each fixture ([`fixture`]), in parallel, results sorted by name;
//! 2. asks each group's answer module ([`answer`]) the recorded questions;
//! 3. compares the two answers as JSON values under the group's [`compare::CompareSpec`];
//! 4. explains each difference from `refcheck/intended.toml` ([`intended`]), and fails the run
//!    for the unexplained ones that `refcheck/enforced.toml` covers ([`enforced`]);
//! 5. reports in dependency order ([`report`]), while `refcheck/ratchet.json` ([`ratchet`])
//!    keeps the counts of every group from rising.
//!
//! Each system package adds its group's answer module (DESIGN.md 3.4, rule 1); the first were
//! `uniques` (package 1a-05) and `state_echo` (1a-10). Each fixture's state is converted once by
//! the engine's Python-state converter (`compat::python`) for every group; a state that does not
//! convert, or whose conversion panics, is a load failure of that fixture alone.

#![forbid(unsafe_code)]

pub mod answer;
pub mod compare;
pub mod enforced;
mod error;
pub mod fixture;
pub mod group;
pub mod intended;
pub mod ratchet;
pub mod report;
pub mod run;
pub mod suggest;

pub use error::{Error, Result};
pub use group::Group;
