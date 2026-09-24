//! Which checks a game runs on itself (DESIGN.md 9.4). Never saved, and never able to change the
//! course of a game: the checks only read.
//!
//! Replaces nothing in Python, which had neither invariants nor a cache oracle.

/// The checks a game runs at every settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugOptions {
    /// Check every invariant of DESIGN.md 9.4. On by default in debug builds and in release
    /// builds with the `checks` feature.
    pub invariants: bool,
    /// Recompute every cache cold and compare (the cache oracle). Testkit turns it on for
    /// scripts, properties and chaos.
    pub verify_caches: bool,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self { invariants: cfg!(any(debug_assertions, feature = "checks")), verify_caches: false }
    }
}

impl DebugOptions {
    /// Everything off, as a shipped release build runs.
    pub const OFF: Self = Self { invariants: false, verify_caches: false };

    /// Everything on, as testkit runs.
    pub const ALL: Self = Self { invariants: true, verify_caches: true };
}
