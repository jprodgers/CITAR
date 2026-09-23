//! Test support for the CITAR engine, and the home of all its integration tests.
//!
//! All integration tests live here rather than in `citar-engine`, which keeps only `#[cfg(test)]`
//! unit tests and doctests; that removes the dev-dependency cycle between the two (DESIGN.md 2.1).
//! Later packages add the rule-script runner, fixture loading, `RandomAgent`, the golden files
//! and the `golden`, `chaos` and `soak` binaries.

#![forbid(unsafe_code)]
