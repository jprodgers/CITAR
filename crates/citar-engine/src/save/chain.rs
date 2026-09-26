//! The state digest and the digest chain (DESIGN.md 4.10).
//!
//! `Digest = blake3("CITAR-DIGEST" ‖ CANON_V1 ‖ RulesetId ‖ canon(State))`. The ruleset is part
//! of it, so the same state under two rulesets digests differently, and the tag names the
//! encoding, so a future `CANON_V2` cannot collide with it.
//!
//! The chain folds each round's digest into one running value, which proof of work checks:
//! `chain_0 = blake3("CITAR-CHAIN" ‖ spec_hash)`, then
//! `chain_t = blake3(chain_{t-1} ‖ turn_le ‖ digest_t)`, `turn_le` being the turn as 4
//! little-endian bytes. Digests are taken at the end of each round, after `end_round` and before
//! the next `begin_turn`, when a game opts in (jobs and tests). The host keeps the chain's head
//! and length beside a save, and [`DigestChain::resume`] carries it on after the load.
//!
//! Replaces nothing in Python, which had no digest.

use serde::Serialize;

use crate::base::digest::{BLOCK_SIZE, CANON_V1, CanonError, CanonSerializer, Digest};
use crate::base::ids::Turn;
use crate::rules::Ruleset;
use crate::state::State;

/// The tag every state digest starts with.
pub const DIGEST_DOMAIN: &[u8] = b"CITAR-DIGEST";

/// The tag every chain starts with.
pub const CHAIN_DOMAIN: &[u8] = b"CITAR-CHAIN";

/// The digest of `st` under `rules`. Refused if a float is NaN or infinite.
pub fn digest(rules: &Ruleset, st: &State) -> Result<Digest, CanonError> {
    Digester::new().digest(rules, st)
}

/// Takes digests with one block buffer, so a digest every round allocates nothing.
#[derive(Clone, Debug, Default)]
pub struct Digester {
    buf: Vec<u8>,
}

impl Digester {
    /// A digester with its buffer.
    #[must_use]
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(BLOCK_SIZE) }
    }

    /// The digest of `st` under `rules`.
    pub fn digest(&mut self, rules: &Ruleset, st: &State) -> Result<Digest, CanonError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(CANON_V1);
        hasher.update(&rules.id().0);
        let mut ser = CanonSerializer::with_buffer(&mut hasher, core::mem::take(&mut self.buf));
        let done = st.serialize(&mut ser);
        let (_, buf) = ser.finish_with_buffer();
        self.buf = buf;
        done?;
        Ok(Digest::from(hasher.finalize()))
    }
}

/// The running chain of per-round digests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigestChain {
    head: Digest,
    rounds: u32,
}

impl DigestChain {
    /// A chain for the game `spec_hash` describes: `chain_0`.
    #[must_use]
    pub fn new(spec_hash: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(CHAIN_DOMAIN);
        h.update(spec_hash);
        Self { head: Digest::from(h.finalize()), rounds: 0 }
    }

    /// A chain carried on after a load: the [`head`](Self::head) and [`rounds`](Self::rounds) it
    /// had when the game was saved.
    ///
    /// The chain covers states, so no state holds it. A host that chains (a proof-of-work job)
    /// keeps the head and the count beside its save (from Phase 2, in the `.citar` container's
    /// session record), and resumes from them, so a job saved and loaded every round chains as
    /// the uninterrupted run does.
    #[must_use]
    pub const fn resume(head: Digest, rounds: u32) -> Self {
        Self { head, rounds }
    }

    /// Folds in the digest of the round that ended on `turn`, and returns the new head.
    pub fn push(&mut self, turn: Turn, digest: &Digest) -> Digest {
        let mut h = blake3::Hasher::new();
        h.update(self.head.as_bytes());
        h.update(&turn.to_le_bytes());
        h.update(digest.as_bytes());
        self.head = Digest::from(h.finalize());
        self.rounds = self.rounds.saturating_add(1);
        self.head
    }

    /// The head: `chain_t` after `t` rounds.
    #[must_use]
    pub const fn head(&self) -> Digest {
        self.head
    }

    /// How many rounds it holds.
    #[must_use]
    pub const fn rounds(&self) -> u32 {
        self.rounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_follows_its_formula() {
        let mut c = DigestChain::new(b"spec");
        let mut h = blake3::Hasher::new();
        h.update(b"CITAR-CHAIN");
        h.update(b"spec");
        let zero = Digest::from(h.finalize());
        assert_eq!(c.head(), zero);
        let d = Digest([7; 32]);
        let one = c.push(3, &d);
        let mut h = blake3::Hasher::new();
        h.update(zero.as_bytes());
        h.update(&3i32.to_le_bytes());
        h.update(&[7; 32]);
        assert_eq!(one, Digest::from(h.finalize()));
        assert_eq!(c.rounds(), 1);
        assert_ne!(DigestChain::new(b"spec").push(4, &d), one, "the turn is chained");
    }

    #[test]
    fn a_resumed_chain_carries_on_as_the_uninterrupted_one() {
        let digests: Vec<Digest> = (0..6u8).map(|i| Digest([i; 32])).collect();
        let mut whole = DigestChain::new(b"job");
        for (turn, d) in (1..).zip(&digests) {
            whole.push(turn, d);
        }
        let mut first = DigestChain::new(b"job");
        for (turn, d) in (1..).zip(&digests[..4]) {
            first.push(turn, d);
        }
        // What a host kept beside its save.
        let (head, rounds) = (first.head().to_hex(), first.rounds());
        let mut resumed = DigestChain::resume(Digest::from_hex(&head).expect("hex"), rounds);
        for (turn, d) in (5..).zip(&digests[4..]) {
            resumed.push(turn, d);
        }
        assert_eq!(resumed, whole);
    }
}
