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
//!    keeps the unexplained count of every group from rising.
//!
//! There are no answer modules yet: each system package adds its own (DESIGN.md 3.4, rule 1).

#![forbid(unsafe_code)]

pub mod compare;
mod error;
pub mod group;

pub use error::{Error, Result};
pub use group::Group;
