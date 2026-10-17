//! The claim a helper signs over a run's result (design/phase4-helper.md P4.4.4; PROTOCOL.md
//! `job_result`): which run of which job, on which machine, of which spec and jobs build, with
//! which bots, came to which result and chain. Typed and closed, its fields in a fixed order, so
//! its bytes are the same wherever they are made: the helper signs them (4-10) and the server
//! checks the signature against the machine's registered key through the binding (4-11). The run
//! and the machine inside it make a signature useless for any other run; the spec hash and the
//! jobs build bind it to exactly what was asked for.
//!
//! **What is signed:** [`CLAIM_DOMAIN`] (`CITAR-CLAIM-V1` and a zero byte) followed by the claim
//! as compact JSON, its fields in this struct's order ([`Claim::bytes`]). The domain keeps a claim's
//! signature from being taken for any other signature the same key makes.
//!
//! **The server's check** ([`verify_claim`], P4.4.4, through the binding's `verify_claim`): the
//! signature against the machine's registered key, the claim against the server's records (its
//! job, run, machine, kind, spec hash, jobs build, the spec's bots and the key), the result's hash
//! against the claim's, and the chain against the claim's head and rounds and its own fold, then
//! the result against its spec ([`crate::result_fits`]). Each failure is one of P4.5.8's tampering
//! verdicts ([`ClaimVerdict`]): a correct helper never produces one, so the server rejects the run
//! and quarantines the machine and its owner.
//!
//! **Sampling** ([`sample_fraction`], P4.5.8 rule 1): whether a provisional or trusted machine's
//! result is checked by a replica is `blake3(server secret, job id)` read as a fraction, keyed so
//! that a helper cannot tell which of its jobs will be sampled, and decided only once its result
//! is in.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
/// The machine key's type, for the helper and the tests that sign claims.
pub use ed25519_dalek::SigningKey;
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fit::{FitError, result_fits};
use crate::hash::{Chain, JobResult};
use crate::spec::Checked;

/// What is signed before the claim's bytes.
pub const CLAIM_DOMAIN: &[u8] = b"CITAR-CLAIM-V1\0";
/// The claim's version.
pub const CLAIM_VERSION: u32 = 1;

/// A run's claim (PROTOCOL.md "Claim"): closed, its fields in this order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// [`CLAIM_VERSION`].
    pub v: u32,
    /// The job (`cj_…`).
    pub job: String,
    /// The run (`cr_…`).
    pub run: String,
    /// The machine's id, as the welcome gave it.
    pub machine: String,
    /// The job's kind, as the wire writes it (`lab_game`, …).
    pub kind: String,
    /// The spec's hash (64 hex digits).
    pub spec_hash: String,
    /// The jobs build that played it.
    pub jobs_build: String,
    /// The bot versions the spec names, sorted ([`crate::Checked::bots`]).
    pub bots: Vec<String>,
    /// The result's hash ([`crate::result_hash`]).
    pub result_hash: String,
    /// The chain's head.
    pub chain_head: String,
    /// The rounds the chain holds.
    pub rounds: u64,
    /// The machine's public key, base64 of its 32 bytes.
    pub key: String,
}

impl Claim {
    /// The claim as compact JSON, its fields in order: what [`Claim::message`] puts after the
    /// domain.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        // A struct of strings and integers always serializes.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// What the machine key signs: [`CLAIM_DOMAIN`] and [`Claim::bytes`].
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        let mut m = CLAIM_DOMAIN.to_vec();
        m.extend_from_slice(&self.bytes());
        m
    }

    /// The claim signed by `key`: its ed25519 signature over [`Claim::message`], standard base64
    /// (64 bytes). The helper signs with its machine key, in its own process (never the job
    /// host's); tests with a throwaway one.
    #[must_use]
    pub fn sign(&self, key: &SigningKey) -> String {
        STANDARD.encode(key.sign(&self.message()).to_bytes())
    }
}

/// One of P4.5.8's tampering verdicts a claim's check gives: each quarantines the machine and its
/// owner (rule 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimVerdict {
    /// The signature does not verify against the machine's registered key.
    Signature,
    /// The claim names another job, run, machine, kind, spec, jobs build, bots, key, result or
    /// chain than the server's records and what was sent, or the result does not answer its spec.
    Claim,
    /// The digests do not fold to the head the claim names.
    ChainInconsistent,
}

impl ClaimVerdict {
    /// The verdict as `cpu_job_runs.error` and the evidence name it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::Claim => "claim",
            Self::ChainInconsistent => "chain_inconsistent",
        }
    }
}

/// A claim that does not hold, and why.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{why}")]
pub struct ClaimRefused {
    pub verdict: ClaimVerdict,
    pub why: String,
}

fn refused(verdict: ClaimVerdict, why: impl Into<String>) -> ClaimRefused {
    ClaimRefused { verdict, why: why.into() }
}

/// What the server's records say a run's claim must name (P4.4.4): the run it offered, to which
/// machine, of which job and jobs build, and the key that machine registered.
#[derive(Clone, Copy, Debug)]
pub struct Expected<'a> {
    pub job: &'a str,
    pub run: &'a str,
    pub machine: &'a str,
    pub jobs_build: &'a str,
    /// The machine's registered public key, base64 of its 32 bytes.
    pub key: &'a str,
}

/// A key's public half as the hello and the claim carry it: standard base64 of its 32 bytes.
#[must_use]
pub fn public_b64(key: &SigningKey) -> String {
    STANDARD.encode(key.verifying_key().to_bytes())
}

/// A public key as the hello and the claim carry it.
///
/// # Errors
/// Not base64 of 32 bytes, or not a point of the curve.
pub fn verifying_key(b64: &str) -> Result<VerifyingKey, String> {
    let bytes = STANDARD.decode(b64).map_err(|e| format!("not base64: {e}"))?;
    let bytes: [u8; 32] =
        bytes.try_into().map_err(|b: Vec<u8>| format!("{} bytes, not 32", b.len()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| format!("not an ed25519 public key: {e}"))
}

/// Checks a run's signed claim, its result and its chain against the job `checked` describes and
/// the server's records `expected` (P4.4.4): the signature against the registered key, the claim
/// against the records, the result's hash, the chain's head, rounds and fold, and the result
/// against its spec, in that order, the first failure its verdict.
///
/// # Errors
/// The first thing that does not hold, as a [`ClaimRefused`].
pub fn verify_claim(
    rules: &citar_engine::rules::Ruleset,
    checked: &Checked,
    expected: &Expected<'_>,
    claim: &Value,
    signature: &str,
    result: &Value,
    chain: &Value,
) -> Result<(), ClaimRefused> {
    use ClaimVerdict::{ChainInconsistent, Claim as Named, Signature as Signed};
    let claim = Claim::deserialize(claim)
        .map_err(|e| refused(Named, format!("the claim does not read: {e}")))?;
    let key = verifying_key(expected.key)
        .map_err(|e| refused(Signed, format!("the machine's registered key is {e}")))?;
    let sig = STANDARD
        .decode(signature)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
        .ok_or_else(|| refused(Signed, "the signature is not base64 of 64 bytes"))?;
    key.verify_strict(&claim.message(), &Signature::from_bytes(&sig)).map_err(|_| {
        refused(Signed, "a bad signature: it does not verify against the machine's registered key")
    })?;
    let spec_hash = checked.hash.to_hex();
    let bots = checked.bots();
    let named: [(&str, bool); 9] = [
        ("version", claim.v == CLAIM_VERSION),
        ("job", claim.job == expected.job),
        ("run", claim.run == expected.run),
        ("machine", claim.machine == expected.machine),
        ("kind", claim.kind == checked.kind.name()),
        ("spec hash", claim.spec_hash == spec_hash),
        ("jobs build", claim.jobs_build == expected.jobs_build),
        ("bots", claim.bots == bots),
        ("key", claim.key == expected.key),
    ];
    if let Some((what, _)) = named.iter().find(|(_, ok)| !ok) {
        return Err(refused(
            Named,
            format!("the claim names another {what} than the server's records"),
        ));
    }
    let typed = JobResult::read(checked.kind, result)
        .map_err(|e| refused(Named, format!("the result does not read: {e}")))?;
    let hash = typed.hash(checked.kind).map_err(|e| refused(Named, format!("the result: {e}")))?;
    if hash != claim.result_hash {
        return Err(refused(
            Named,
            format!(
                "the claim names result {}, and the result sent hashes to {hash}",
                claim.result_hash
            ),
        ));
    }
    let chain = Chain::deserialize(chain)
        .map_err(|e| refused(ChainInconsistent, format!("the chain does not read: {e}")))?;
    if chain.head != claim.chain_head || u64::from(chain.rounds) != claim.rounds {
        return Err(refused(
            Named,
            format!(
                "the claim names a chain of {} rounds to {}, and the chain sent has {} to {}",
                claim.rounds, claim.chain_head, chain.rounds, chain.head
            ),
        ));
    }
    result_fits(rules, checked, &typed, &chain).map_err(|e| match e {
        FitError::Chain(c) => refused(ChainInconsistent, format!("its chain: {c}")),
        FitError::Result(why) => {
            refused(Named, format!("its result does not answer its spec: {why}"))
        }
    })
}

/// The domain the sampling key is derived in.
const SAMPLE_CONTEXT: &str = "CITAR 2026-10 job sampling v1";

/// Where the server's sample of `job` falls, in `[0, 1)` (P4.5.8 rule 1): blake3 keyed by a key
/// derived from the server's secret, over the job's id, its first 8 bytes read as a fraction. A
/// job is sampled at rate `p` when this is below `p`; without the secret, which never leaves the
/// server, nobody can tell which jobs will be.
#[must_use]
pub fn sample_fraction(secret: &[u8], job: &str) -> f64 {
    let key = blake3::derive_key(SAMPLE_CONTEXT, secret);
    let h = blake3::keyed_hash(&key, job.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.as_bytes()[..8]);
    // 53 bits, so the fraction is exact as an f64 and below 1.
    #[allow(clippy::cast_precision_loss)]
    let x = (u64::from_le_bytes(first) >> 11) as f64;
    x / 9_007_199_254_740_992.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> Claim {
        Claim {
            v: CLAIM_VERSION,
            job: "cj_0123456789abcdef".to_owned(),
            run: "cr_0123456789abcdef".to_owned(),
            machine: "sv_0123456789abcdef".to_owned(),
            kind: "lab_game".to_owned(),
            spec_hash: "a".repeat(64),
            jobs_build: "0f1e2d3c4b5a".to_owned(),
            bots: vec!["basic1".to_owned()],
            result_hash: "b".repeat(64),
            chain_head: "c".repeat(64),
            rounds: 30,
            key: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_owned(),
        }
    }

    #[test]
    fn the_bytes_are_compact_json_in_the_fields_order() {
        let text = String::from_utf8(claim().bytes()).expect("utf-8");
        let want = format!(
            "{{\"v\":1,\"job\":\"cj_0123456789abcdef\",\"run\":\"cr_0123456789abcdef\",\
             \"machine\":\"sv_0123456789abcdef\",\"kind\":\"lab_game\",\"spec_hash\":\"{}\",\
             \"jobs_build\":\"0f1e2d3c4b5a\",\"bots\":[\"basic1\"],\"result_hash\":\"{}\",\
             \"chain_head\":\"{}\",\"rounds\":30,\
             \"key\":\"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=\"}}",
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        );
        assert_eq!(text, want);
        let message = claim().message();
        assert!(message.starts_with(b"CITAR-CLAIM-V1\0"));
        assert_eq!(&message[CLAIM_DOMAIN.len()..], want.as_bytes());
    }

    /// A run of the golden `duel-a` as a helper would send it: its claim signed by a throwaway
    /// key, the records the server keeps, the result and the chain.
    struct Sent {
        checked: Checked,
        signing: SigningKey,
        key: String,
        claim: Value,
        signature: String,
        result: Value,
        chain: Value,
    }

    impl Sent {
        fn honest() -> Sent {
            let rules = citar_engine::rules::Ruleset::shared();
            let set = crate::golden::GoldenSet::committed().expect("the golden set");
            let job = &set.jobs[crate::golden::SELF_CHECK];
            let (checked, result, result_hash) = job.expected(rules).expect("its expected result");
            let signing = SigningKey::from_bytes(&[7; 32]);
            let key = STANDARD.encode(signing.verifying_key().to_bytes());
            let c = Claim {
                job: "cj_0123456789abcdef".to_owned(),
                run: "cr_0123456789abcdef".to_owned(),
                machine: "sv_0123456789abcdef".to_owned(),
                kind: checked.kind.name().to_owned(),
                spec_hash: checked.hash.to_hex(),
                jobs_build: "0f1e2d3c4b5a".to_owned(),
                bots: checked.bots(),
                result_hash,
                chain_head: job.chain_head.clone(),
                rounds: u64::from(job.rounds),
                key: key.clone(),
                ..claim()
            };
            let signature = c.sign(&signing);
            let claim = serde_json::to_value(&c).expect("a value");
            let chain = serde_json::to_value(job.chain()).expect("a value");
            Sent { checked, signing, key, claim, signature, result, chain }
        }

        fn check(&self) -> Result<(), ClaimRefused> {
            let expected = Expected {
                job: "cj_0123456789abcdef",
                run: "cr_0123456789abcdef",
                machine: "sv_0123456789abcdef",
                jobs_build: "0f1e2d3c4b5a",
                key: &self.key,
            };
            verify_claim(
                citar_engine::rules::Ruleset::shared(),
                &self.checked,
                &expected,
                &self.claim,
                &self.signature,
                &self.result,
                &self.chain,
            )
        }

        /// Signs the claim as it is now, as a cheater holding the machine's key would.
        fn resign(&mut self) {
            let c = Claim::deserialize(&self.claim).expect("a claim");
            self.signature = c.sign(&self.signing);
        }
    }

    /// A string with its last character changed: one byte.
    fn one_byte(v: &Value) -> Value {
        let s = v.as_str().expect("a string");
        let (head, last) = s.split_at(s.len() - 1);
        Value::from(format!("{head}{}", if last == "0" { "1" } else { "0" }))
    }

    #[test]
    fn an_honest_claim_verifies() {
        assert_eq!(Sent::honest().check(), Ok(()));
    }

    #[test]
    fn a_changed_byte_in_any_claim_field_fails_with_its_reason() {
        for (field, named) in [
            ("job", "job"),
            ("run", "run"),
            ("machine", "machine"),
            ("kind", "kind"),
            ("spec_hash", "spec hash"),
            ("jobs_build", "jobs build"),
            ("result_hash", "claim names result"),
            ("chain_head", "claim names a chain"),
            ("key", "key"),
        ] {
            // changed after signing: the signature no longer covers it
            let mut sent = Sent::honest();
            let changed = if field == "kind" {
                Value::from("ranking_game")
            } else {
                one_byte(&sent.claim[field])
            };
            sent.claim[field] = changed;
            let e = sent.check().expect_err(field);
            assert_eq!(e.verdict, ClaimVerdict::Signature, "{field}: {e}");
            assert!(e.why.contains("bad signature"), "{field}: {e}");
            // signed again, as a cheater with the key would: the claim names what the records do not
            sent.resign();
            let e = sent.check().expect_err(field);
            assert_eq!(e.verdict, ClaimVerdict::Claim, "{field} re-signed: {e}");
            assert!(e.why.contains(named), "{field} re-signed: {e}");
        }
        for (field, value) in [
            ("v", Value::from(2)),
            ("rounds", Value::from(31)),
            ("bots", serde_json::json!(["basic1", "idle"])),
        ] {
            let mut sent = Sent::honest();
            sent.claim[field] = value;
            assert_eq!(sent.check().expect_err(field).verdict, ClaimVerdict::Signature, "{field}");
            sent.resign();
            assert_eq!(
                sent.check().expect_err(field).verdict,
                ClaimVerdict::Claim,
                "{field} re-signed"
            );
        }
        let mut sent = Sent::honest();
        sent.claim["extra"] = Value::from(1);
        let e = sent.check().expect_err("an unknown key");
        assert_eq!(
            (e.verdict, e.why.contains("does not read")),
            (ClaimVerdict::Claim, true),
            "{e}"
        );
    }

    #[test]
    fn a_changed_result_digest_or_signature_fails_with_its_reason() {
        // one score of the result raised: the claim names another result
        let mut sent = Sent::honest();
        let players = sent.result["players"].as_object_mut().expect("players");
        let first = players.values_mut().next().expect("a player");
        first["score"] = Value::from(first["score"].as_i64().expect("a score") + 1);
        let e = sent.check().expect_err("a changed result");
        assert_eq!(e.verdict, ClaimVerdict::Claim, "{e}");
        assert!(e.why.contains("hashes to"), "{e}");
        // one digest changed: the chain no longer folds to the head the claim names
        let mut sent = Sent::honest();
        sent.chain["digests"][11] = one_byte(&sent.chain["digests"][11]);
        let e = sent.check().expect_err("a changed digest");
        assert_eq!(e.verdict, ClaimVerdict::ChainInconsistent, "{e}");
        assert!(e.why.contains("do not fold"), "{e}");
        // the signature changed in one byte, or made by another key
        let mut sent = Sent::honest();
        let mut sig = STANDARD.decode(&sent.signature).expect("base64");
        sig[10] ^= 1;
        sent.signature = STANDARD.encode(sig);
        assert_eq!(sent.check().expect_err("a changed signature").verdict, ClaimVerdict::Signature);
        let mut sent = Sent::honest();
        let c = Claim::deserialize(&sent.claim).expect("a claim");
        sent.signature = c.sign(&SigningKey::from_bytes(&[8; 32]));
        let e = sent.check().expect_err("another key");
        assert_eq!(e.verdict, ClaimVerdict::Signature, "{e}");
        let mut sent = Sent::honest();
        sent.signature = "not a signature".to_owned();
        assert_eq!(sent.check().expect_err("no signature").verdict, ClaimVerdict::Signature);
        let mut sent = Sent::honest();
        sent.key = "AAAA".to_owned();
        assert_eq!(sent.check().expect_err("no key").verdict, ClaimVerdict::Signature);
    }

    #[test]
    #[allow(clippy::float_cmp)] // the same inputs give the same bits
    fn sampling_is_keyed_by_the_secret() {
        let a = sample_fraction(b"one secret", "cj_0123456789abcdef");
        assert!((0.0..1.0).contains(&a));
        assert_eq!(a, sample_fraction(b"one secret", "cj_0123456789abcdef"));
        assert_ne!(a, sample_fraction(b"another", "cj_0123456789abcdef"));
        assert_ne!(a, sample_fraction(b"one secret", "cj_0123456789abcdee"));
    }

    #[test]
    fn a_claim_is_closed() {
        let mut v = serde_json::to_value(claim()).expect("a value");
        assert_eq!(serde_json::from_value::<Claim>(v.clone()).expect("reads"), claim());
        v["extra"] = serde_json::Value::from(1);
        assert!(serde_json::from_value::<Claim>(v).is_err());
    }
}
