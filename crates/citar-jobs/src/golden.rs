//! The `jobs` golden set (design/phase4-helper.md P4.4.5): `golden/jobs.json`, embedded.
//!
//! Four short jobs (two duels of 30 rounds, a lab game's and a ranking game's; a four-seat lab
//! game on a small map capped at 40 rounds; one reference game capped at 40), each with its
//! spec, spec hash, bots, result, result hash, chain head, rounds and digests. Three things read
//! it:
//! - this crate's own test binary (`tests/golden.rs`), which plays every job and holds it to the
//!   file in both the `ci` and the `release` profiles, locally and on five targets in
//!   `determinism.yml`, so a target that computes another result fails there before any
//!   volunteer's machine does; it is blessed like `cargo golden` (`CITAR_BLESS_JOBS=1`, then a
//!   commit saying why) when what a game computes changes, and not when only the build id moves
//!   ([`GoldenJob::expected`]);
//! - the self-check ([`crate::self_check`]), which plays [`SELF_CHECK`], the shortest job, before a
//!   job host takes work, so a machine whose own build computes it otherwise is offered nothing;
//! - the protocol's transcripts (`crates/citar-helper/protocol/v2/transcripts`), whose
//!   `<golden:jobs/NAME/FIELD>` values are this file's: `jobs[NAME]` and its fields.

use std::collections::BTreeMap;

use citar_engine::rules::Ruleset;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fit::rebased;
use crate::hash::{Chain, result_hash};
use crate::spec::{Checked, Kind, check_spec};

/// The set as committed.
pub const JOBS_JSON: &str = include_str!("../golden/jobs.json");

/// The job the self-check plays: the set's shortest.
pub const SELF_CHECK: &str = "duel-a";

/// One job of the set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenJob {
    pub kind: Kind,
    /// The spec as the wire carries it (its canonical form).
    pub spec: Value,
    pub spec_hash: String,
    /// The bot versions the spec names, sorted: a claim's `bots`.
    pub bots: Vec<String>,
    /// The outcome's result.
    pub result: Value,
    pub result_hash: String,
    pub chain_head: String,
    pub rounds: u32,
    pub digests: Vec<String>,
}

impl GoldenJob {
    /// Its chain, as an outcome carries it.
    #[must_use]
    pub fn chain(&self) -> Chain {
        Chain { head: self.chain_head.clone(), rounds: self.rounds, digests: self.digests.clone() }
    }

    /// The job's spec checked against its committed hash, and the result and result hash this
    /// build must give: the committed result with the keys that name the build (`engine`, each
    /// seat's `build` and `fingerprint`, a reference line's `bot`) given this build's values
    /// ([`crate::rebased`]). A build whose id has moved without any game changing (an edit of
    /// the engine's or the bot's comments, a new version of CITAR) checks against the set as it
    /// is; a change of what a game computes does not, and asks for a bless.
    ///
    /// # Errors
    /// A spec that does not check or hash to its committed hash, or a result that does not read.
    pub fn expected(&self, rules: &'static Ruleset) -> Result<(Checked, Value, String), String> {
        let checked = check_spec(rules, self.kind, &self.spec, Some(&self.spec_hash))
            .map_err(|e| format!("the spec: {e}"))?;
        let result = rebased(rules, &checked, &self.result).map_err(|e| e.to_string())?;
        let hash = result_hash(self.kind, &result).map_err(|e| e.to_string())?;
        Ok((checked, result, hash))
    }
}

/// The set: what it is, and its jobs by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenSet {
    pub about: String,
    pub jobs: BTreeMap<String, GoldenJob>,
}

impl GoldenSet {
    /// The set as committed.
    ///
    /// # Errors
    /// A file that does not read as a set.
    pub fn committed() -> Result<Self, String> {
        Self::parse(JOBS_JSON)
    }

    /// A set from its text.
    ///
    /// # Errors
    /// Text that does not read as a set.
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("golden/jobs.json: {e}"))
    }

    /// The set as the file holds it: pretty JSON with a final newline.
    ///
    /// # Errors
    /// Never in practice: the set is JSON values and strings.
    pub fn text(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map(|t| t + "\n").map_err(|e| e.to_string())
    }
}
