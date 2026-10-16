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

use serde::{Deserialize, Serialize};

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

    #[test]
    fn a_claim_is_closed() {
        let mut v = serde_json::to_value(claim()).expect("a value");
        assert_eq!(serde_json::from_value::<Claim>(v.clone()).expect("reads"), claim());
        v["extra"] = serde_json::Value::from(1);
        assert!(serde_json::from_value::<Claim>(v).is_err());
    }
}
