//! The result hash and the chain (design/phase4-helper.md P4.4.4).
//!
//! - **`result_hash = blake3("CITAR-RESULT-V1\0" ‖ kind ‖ "\0" ‖ serde_json bytes of the typed
//!   deterministic result)`.** The server never hashes Python's JSON: [`result_hash`] reads the
//!   result it received into the typed result (refusing an unknown key anywhere), writes it back
//!   and hashes that, so two sides that agree on the result agree on its bytes whatever order or
//!   spacing carried it.
//! - **The chain:** every job's game runs under `Game::set_chain(DigestChain::new(spec_hash))`,
//!   whose `chain_0 = blake3("CITAR-CHAIN" ‖ spec_hash)` takes the spec hash's 32 bytes. A
//!   [`Chain`] carries the head, the round count and every round's digest, the round that ended
//!   on turn `t` being the `t`-th: a job's game starts on turn 1 and chains each round from the
//!   first. [`chain_check`] folds the digests from `chain_0` and checks that they reach the head
//!   (replicas compare heads, and where two differ the first differing digest names the round
//!   where two machines parted, [`parted_at`]).
//!
//! Claims and their signatures are [`crate::claim`]'s.

use citar_engine::base::digest::Digest;
use citar_engine::base::ids::Turn;
use citar_engine::save::chain::DigestChain;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lab::LabOutcome;
use crate::reference::ReferenceOutcome;
use crate::spec::{Kind, SpecHash, hex, unhex32};

/// What the result hash starts with.
pub const RESULT_DOMAIN: &[u8] = b"CITAR-RESULT-V1\0";

/// The most rounds a chain may carry: the protocol's bound on `chain.digests` (P4.2.5).
pub const MAX_ROUNDS: usize = 10_000;

/// A job's result: its kind's outcome.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JobResult {
    /// A lab or ranking game's.
    Lab(LabOutcome),
    /// A reference game's.
    Reference(ReferenceOutcome),
}

/// Why a result does not read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ResultError(pub String);

impl JobResult {
    /// Reads a received result as a job of `kind`'s, refusing an unknown key anywhere.
    ///
    /// # Errors
    /// What does not read.
    pub fn read(kind: Kind, v: &Value) -> Result<Self, ResultError> {
        let read = match kind {
            Kind::LabGame | Kind::RankingGame => LabOutcome::read(v).map(Self::Lab),
            Kind::ReferenceGame => ReferenceOutcome::read(v).map(Self::Reference),
        };
        read.map_err(|e| ResultError(format!("a {kind} result: {e}")))
    }

    /// The typed result's bytes: what the hash covers.
    ///
    /// # Errors
    /// A float that is not finite, which serde_json would write as `null` and which would then
    /// not read back. A game computes none (the engine's digest refuses one), so this is a guard.
    pub fn bytes(&self) -> Result<Vec<u8>, ResultError> {
        if has_non_finite(self) {
            return Err(ResultError("a result holds a number that is not finite".to_owned()));
        }
        serde_json::to_vec(self).map_err(|e| ResultError(e.to_string()))
    }

    /// The result hash of this result, for a job of `kind`.
    ///
    /// # Errors
    /// As [`bytes`](Self::bytes).
    pub fn hash(&self, kind: Kind) -> Result<String, ResultError> {
        let mut h = blake3::Hasher::new();
        h.update(RESULT_DOMAIN);
        h.update(kind.name().as_bytes());
        h.update(b"\0");
        h.update(&self.bytes()?);
        Ok(hex(h.finalize().as_bytes()))
    }
}

/// Whether a float of a finished result is not finite. (A JSON number of the overrides or levels
/// is finite by construction.)
fn has_non_finite(r: &JobResult) -> bool {
    match r {
        JobResult::Lab(LabOutcome::Finished(res)) => res.players.values().any(|p| {
            let mut floats = vec![p.aggression, p.score_share, p.unhappy_share];
            floats.extend(p.fixed_aggression);
            for c in p.checkpoints.values() {
                floats.extend([c.gold_per_turn, c.science, c.production, c.culture]);
            }
            floats.iter().any(|x| !x.is_finite())
        }),
        JobResult::Reference(ReferenceOutcome::Finished(res)) => {
            res.civs.iter().any(|c| !c.aggression.is_finite())
        }
        _ => false,
    }
}

/// The hash of a received result, read as a job of `kind`'s and written back (P4.4.4).
///
/// # Errors
/// A result that does not read as `kind`'s, or holds a number that is not finite.
pub fn result_hash(kind: Kind, result: &Value) -> Result<String, ResultError> {
    JobResult::read(kind, result)?.hash(kind)
}

/// A job's chain of round digests, as an outcome carries it (P4.4.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    /// `chain_t` after the last round, 64 hex digits.
    pub head: String,
    /// How many rounds it holds.
    pub rounds: u32,
    /// Each round's digest, the first round's first, 64 hex digits each.
    pub digests: Vec<String>,
}

/// Why a chain does not check.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// It does not read as a chain.
    #[error("not a chain: {0}")]
    Shape(String),
    /// Its round count is not its number of digests.
    #[error("the chain counts {rounds} rounds and carries {digests} digests")]
    Count { rounds: u32, digests: usize },
    /// More rounds than a chain may carry.
    #[error("the chain carries {0} digests, more than {MAX_ROUNDS}")]
    TooLong(usize),
    /// A digest or the head that is not 64 lower-case hex digits.
    #[error("{0} is not 64 lower-case hex digits")]
    Hex(String),
    /// The digests do not fold to the head.
    #[error("the digests do not fold to the head: they give {folded}, the chain says {head}")]
    Head { folded: String, head: String },
}

impl Chain {
    /// The chain a game kept: its head, its round count and the digests taken.
    #[must_use]
    pub fn of(chain: &DigestChain, digests: &[Digest]) -> Self {
        Self {
            head: chain.head().to_hex(),
            rounds: chain.rounds(),
            digests: digests.iter().map(Digest::to_hex).collect(),
        }
    }
}

/// Checks that `chain`'s digests fold to its head from `chain_0` of the spec hash `spec_hash`,
/// and that it counts as many rounds as it carries digests.
///
/// # Errors
/// What does not hold, the first thing found.
pub fn chain_check(spec_hash: &SpecHash, chain: &Chain) -> Result<(), ChainError> {
    if chain.digests.len() > MAX_ROUNDS {
        return Err(ChainError::TooLong(chain.digests.len()));
    }
    if usize::try_from(chain.rounds).ok() != Some(chain.digests.len()) {
        return Err(ChainError::Count { rounds: chain.rounds, digests: chain.digests.len() });
    }
    let head = unhex32(&chain.head).ok_or_else(|| ChainError::Hex("the head".to_owned()))?;
    let folded = fold(spec_hash, &chain.digests)?;
    if unhex32(&folded) != Some(head) {
        return Err(ChainError::Head { folded, head: chain.head.clone() });
    }
    Ok(())
}

/// The head `digests` fold to from `chain_0` of the spec hash `spec_hash`, 64 hex digits: what a
/// chain of those rounds must name (and what a helper built to report broken hardware,
/// `--test-fault digest:R`, folds its altered chain to).
///
/// # Errors
/// More digests than a chain may carry, or one that is not 64 lower-case hex digits.
pub fn fold(spec_hash: &SpecHash, digests: &[String]) -> Result<String, ChainError> {
    if digests.len() > MAX_ROUNDS {
        return Err(ChainError::TooLong(digests.len()));
    }
    let mut chain = DigestChain::new(&spec_hash.0);
    for (n, d) in digests.iter().enumerate() {
        let d = unhex32(d).ok_or_else(|| ChainError::Hex(format!("round {}'s digest", n + 1)))?;
        let turn = Turn::try_from(n + 1).map_err(|_| ChainError::TooLong(digests.len()))?;
        chain.push(turn, &Digest(d));
    }
    Ok(chain.head().to_hex())
}

/// The first round (1 for the first digest) where two chains' digests differ, or `None` when
/// they hold the same digests: where two machines that played one job parted (P4.5.8's `chain`
/// verdict). A chain that is a prefix of the other parts at the round after its last.
#[must_use]
pub fn parted_at(a: &[String], b: &[String]) -> Option<usize> {
    let common = a.iter().zip(b).position(|(x, y)| x != y);
    match common {
        Some(i) => Some(i + 1),
        None if a.len() == b.len() => None,
        None => Some(a.len().min(b.len()) + 1),
    }
}

/// Reads a received chain and checks it ([`chain_check`]).
///
/// # Errors
/// A chain that does not read, or does not check.
pub fn chain_check_value(spec_hash: &SpecHash, chain: &Value) -> Result<(), ChainError> {
    let chain = Chain::deserialize(chain).map_err(|e| ChainError::Shape(e.to_string()))?;
    chain_check(spec_hash, &chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_of(spec: &SpecHash, n: u8) -> Chain {
        let digests: Vec<Digest> = (0..n).map(|i| Digest([i; 32])).collect();
        let mut c = DigestChain::new(&spec.0);
        for (t, d) in (1..).zip(&digests) {
            c.push(t, d);
        }
        Chain::of(&c, &digests)
    }

    #[test]
    fn a_chain_folds_to_its_head_and_an_altered_one_does_not() {
        let spec = SpecHash([9; 32]);
        let good = chain_of(&spec, 5);
        assert_eq!(chain_check(&spec, &good), Ok(()));
        assert_eq!(chain_check(&spec, &chain_of(&spec, 0)), Ok(()), "an empty chain is chain_0");
        let mut altered = good.clone();
        altered.digests[2] = Digest([77; 32]).to_hex();
        assert!(matches!(chain_check(&spec, &altered), Err(ChainError::Head { .. })));
        let mut short = good.clone();
        short.digests.pop();
        assert!(matches!(chain_check(&spec, &short), Err(ChainError::Count { .. })));
        let mut upper = good.clone();
        upper.digests[0] = "AB".repeat(32);
        assert!(matches!(chain_check(&spec, &upper), Err(ChainError::Hex(_))));
        assert!(
            matches!(chain_check(&SpecHash([8; 32]), &good), Err(ChainError::Head { .. })),
            "another spec's chain_0"
        );
        assert_eq!(fold(&spec, &good.digests), Ok(good.head.clone()));
        let refolded = fold(&spec, &altered.digests).expect("a head");
        assert_ne!(refolded, good.head);
        altered.head = refolded;
        assert_eq!(chain_check(&spec, &altered), Ok(()), "an altered chain folded again checks");
        assert_eq!(parted_at(&good.digests, &altered.digests), Some(3), "it parted at round 3");
        assert_eq!(parted_at(&good.digests, &good.digests), None);
        assert_eq!(parted_at(&good.digests, &short.digests), Some(5), "a prefix parts after it");
        let v = serde_json::to_value(&good).expect("a value");
        assert_eq!(chain_check_value(&spec, &v), Ok(()));
        let mut extra = v;
        extra["note"] = serde_json::json!(1);
        assert!(matches!(chain_check_value(&spec, &extra), Err(ChainError::Shape(_))));
    }

    #[test]
    fn the_result_hash_reads_the_result_typed_and_hashes_it_written_back() {
        let crash = serde_json::json!({"crash": {"turn": 3, "player": null, "label": null,
                                                 "message": "boom"}});
        let spaced: Value =
            serde_json::from_str(&serde_json::to_string_pretty(&crash).expect("text"))
                .expect("JSON");
        let h = result_hash(Kind::LabGame, &crash).expect("a hash");
        assert_eq!(result_hash(Kind::LabGame, &spaced).expect("a hash"), h);
        assert_ne!(result_hash(Kind::RankingGame, &crash).expect("a hash"), h, "kind hashed");
        let mut h2 = blake3::Hasher::new();
        h2.update(b"CITAR-RESULT-V1\0lab_game\0");
        h2.update(br#"{"crash":{"turn":3,"player":null,"label":null,"message":"boom"}}"#);
        assert_eq!(h, hex(h2.finalize().as_bytes()));
        assert!(result_hash(Kind::ReferenceGame, &crash).is_err(), "not a reference's crash");
    }

    /// Gate 3's schema test: no hashed result holds a measurement or a trace, at any depth (the
    /// golden set's results, a result of each kind), and a result that carries one, anywhere,
    /// does not read.
    #[test]
    fn no_hashed_result_holds_a_time_or_a_trace() {
        const MEASURED: [&str; 6] =
            ["seconds", "cpu_s", "started_ts", "finished", "trace", "wall_s"];
        fn keys(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    for (k, x) in m {
                        out.push(k.clone());
                        keys(x, out);
                    }
                }
                Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
                _ => {}
            }
        }
        let set = crate::golden::GoldenSet::committed().expect("the golden set");
        let mut kinds = std::collections::BTreeSet::new();
        for (name, job) in &set.jobs {
            kinds.insert(job.kind);
            let mut found = Vec::new();
            keys(&job.result, &mut found);
            for k in MEASURED {
                assert!(!found.iter().any(|f| f == k), "{name}'s result holds {k}");
            }
            assert_eq!(result_hash(job.kind, &job.result).as_ref(), Ok(&job.result_hash), "{name}");
            for k in MEASURED {
                let mut timed = job.result.clone();
                timed[k] = serde_json::json!(1.5);
                assert!(result_hash(job.kind, &timed).is_err(), "{name} with {k} at the top");
            }
            if let Some(p) = job.result.get("players").and_then(|p| p.get("0")) {
                let mut timed = job.result.clone();
                timed["players"]["0"]["seconds"] = serde_json::json!(1.5);
                assert!(result_hash(job.kind, &timed).is_err(), "{name}: a player's time");
                assert!(p.get("fingerprint").is_some());
            }
        }
        assert_eq!(kinds.len(), Kind::ALL.len(), "the set holds every kind");
    }
}
