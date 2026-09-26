//! Layer 2: saving and loading `State`, the canonical digest and the journal chunks.
//!
//! The JSON codec, load-time validation, the digest over the canonical serialisation, and the
//! summary a host can show without loading a game (DESIGN.md 4.9-4.11, package 1a-09). It may use
//! `state` and the layers below it.
//!
//! - [`ctx`]: the ruleset context rule ids are named by, and their forms;
//! - [`columns`]: the map, tiles and memories as base64 columns with palettes;
//! - [`json`]: save format v1, the whole document, and loading it back;
//! - [`canon`]: the canonical encoding's entry points, and the entries the chronicle hashes;
//! - [`chain`]: the state digest and the per-round digest chain;
//! - [`validate`](mod@validate): what a loaded state must satisfy beyond what
//!   `State::from_parts` checks;
//! - [`migrate`]: upgrades of older save versions (none yet);
//! - [`summary`](mod@summary): a save's headline facts, read without loading it;
//! - [`journal`]: the chronicle as chunks, replay frames as keyframes and deltas, and the
//!   chronicle rebuilt from chunks on load.
//!
//! Replaces the save path of `citar/session.py` (`GameState.to_dict` and `from_dict`), `save_rng`
//! (`game.py:995-1002`), `engine_api.state_summary` (`engine_api.py:158-178`) and the frames of
//! `victory.record_frame` (`citar/engine/victory.py:458-485`).

pub mod canon;
pub mod chain;
pub mod columns;
pub mod ctx;
pub mod journal;
pub mod json;
pub mod migrate;
pub mod summary;
pub mod validate;

pub use chain::{DigestChain, Digester, digest};
pub use json::{load, to_json};
pub use summary::{Summary, summary};
pub use validate::{ValidationError, validate};

use crate::base::digest::{CanonError, Digest};
use crate::rules::{Ruleset, RulesetId};
use crate::state::State;
use crate::state::chronicle::Chronicle;

/// Why a save could not be loaded (DESIGN.md 8.5). A corrupt save is refused here, whole, and
/// never loads into a state that could panic later.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// Not JSON, not a CITAR state, or a part of it that does not read, with its place.
    #[error("the save does not read: {0}")]
    Json(String),
    /// A save format version this engine cannot read.
    #[error("save format version {0} is not one this engine reads (it reads up to {v})", v = migrate::CURRENT)]
    Version(u32),
    /// A rule object the loading ruleset does not have.
    #[error("{path}: the ruleset has no {name:?}")]
    UnknownName {
        /// Where in the save, and what kind of object: `players[2] (tech)`.
        path: String,
        /// The name as the save wrote it.
        name: String,
    },
    /// A top-level key this engine does not know, refused in every build.
    #[error("the save has a key this engine does not know: {0:?}")]
    UnknownKey(String),
    /// The parts read, but do not fit together.
    #[error("the save is inconsistent: {}", list(.0))]
    Invalid(Vec<ValidationError>),
}

fn list(errors: &[ValidationError]) -> String {
    const SHOWN: usize = 5;
    let mut parts: Vec<String> = errors.iter().take(SHOWN).map(ToString::to_string).collect();
    if errors.len() > SHOWN {
        parts.push(format!("and {} more", errors.len() - SHOWN));
    }
    parts.join("; ")
}

impl LoadError {
    /// One inconsistency at `path`.
    #[must_use]
    pub fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid(vec![ValidationError::new(path, message)])
    }
}

/// What loading found that the host may want to know, beside the game itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// The save was made under another ruleset: its [`RulesetId`], and the engine version it
    /// names. Every name still resolved, so it loaded; proof-of-work verification, which needs
    /// the exact ruleset, would refuse it.
    pub rules_changed: Option<(RulesetId, String)>,
    /// The journal chunks did not rebuild the whole history the state's heads count, in
    /// sequence, or (under the save's own ruleset, whose ids the running hash folded in) it hashes
    /// differently. The game plays on; only history views are short.
    pub chronicle_incomplete: bool,
    /// The engine version the save names (`0.1.6+<build>`).
    pub engine: String,
}

/// A loaded game: its state, the history rebuilt from the journal, and the report.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub state: State,
    pub chronicle: Chronicle,
    pub report: LoadReport,
}

/// Why a state could not be saved. The engine keeps its state saveable, so this is a bug
/// caught, not a condition to handle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SaveError {
    /// A float is NaN or infinite; JSON would write it as `null` and the save would not load.
    #[error("the state holds a float that is not finite: {0}")]
    NonFinite(CanonError),
    /// A rule id the ruleset does not have, or another value with no JSON form.
    #[error("the state does not write: {0}")]
    Json(String),
}

/// A deep copy of a game's state with its ruleset (DESIGN.md 4.9): taken under the host's lock
/// in a few milliseconds, then written to JSON off it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    rules: &'static Ruleset,
    state: State,
}

impl Snapshot {
    /// A copy of `state`, made under `rules`.
    #[must_use]
    pub fn new(rules: &'static Ruleset, state: &State) -> Self {
        Self { rules, state: state.clone() }
    }

    /// The state.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// The ruleset.
    #[must_use]
    pub const fn rules(&self) -> &'static Ruleset {
        self.rules
    }

    /// The save: format v1 JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, SaveError> {
        to_json(self.rules, &self.state)
    }

    /// The state's digest.
    pub fn digest(&self) -> Result<Digest, CanonError> {
        digest(self.rules, &self.state)
    }
}
