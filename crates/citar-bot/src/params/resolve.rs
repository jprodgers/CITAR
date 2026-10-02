//! A spec's parameter names resolved against a ruleset (DESIGN.md P2.3.2): policy branches,
//! beliefs per kind, the pantheon order, the free great-person choices and the promotion lines,
//! turned into ids once per ruleset. Names a ruleset lacks (a mod) are skipped and counted.
//!
//! A placeholder until package 2-01a: the placeholder [`Params`] names nothing.

use citar_engine::rules::Ruleset;

use super::Params;

/// The names of a [`Params`] as one ruleset's ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resolved {
    /// Names the ruleset lacks, skipped.
    pub unknown_names: u32,
}

impl Resolved {
    /// `params`' names in `rules`.
    #[must_use]
    pub fn new(params: &Params, rules: &'static Ruleset) -> Self {
        let _ = (params, rules);
        Self::default()
    }
}
