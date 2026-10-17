//! The helper's update manifest (design/phase4-helper.md P4.3.8, P4.5.9): what a release says its
//! helper binaries are, signed as a whole, read the same way by the helper (which updates itself
//! from it) and by the server (which relays it only once it verifies).
//!
//! - **The manifest** (`citar-helper-manifest.json`, a release asset): `{v: 1, version, protocol,
//!   build, released_at, assets: {<target>: {name, url, sha256, size}}}`, one asset per target the
//!   release builds. Fields a later release adds are read past: the signature covers them, and a
//!   helper of this release has no use for them.
//! - **The signature** (`citar-helper-manifest.json.sig`): ed25519 over [`MANIFEST_DOMAIN`]
//!   (`CITAR-HELPER-MANIFEST-V1` and a zero byte) followed by the manifest file's bytes exactly as
//!   published, standard base64. The domain keeps a manifest's signature from being taken for any
//!   other signature the same key makes. `cargo xtask sign-manifest` makes it (with its own copy of
//!   the domain: xtask links no workspace crate), and this module's tests hold the two equal.
//! - **The trusted keys** ([`trusted_keys`]): `update-keys.pub` beside this crate's manifest, one
//!   standard-base64 public key a line, at most [`MAX_KEYS`] (two, so a key can be rotated: the
//!   next release trusts the old and the new). Blank lines and lines starting with `#` are read
//!   past. **It is empty in Phase 4:** no real key exists yet, so a helper built from this tree
//!   trusts none and reports updates `unavailable`; Phase 5 adds the owner's public key (the
//!   private half lives in a GitHub Actions secret, never in the repository).
//! - **The check** ([`verify`]): the size, then the signature against the keys, and only then the
//!   manifest's fields, each bounded ([`Manifest::read`]): what is not signed by a trusted key is
//!   never parsed past its size.
//! - **Versions** ([`newer`]): semantic versions, pre-releases ordered as semver orders them
//!   (`0.1.6-test.1` < `0.1.6-test.2` < `0.1.6`), so neither side downgrades.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
pub use ed25519_dalek::VerifyingKey;
use ed25519_dalek::{Signature, Signer as _, SigningKey};
use serde::Serialize;
use serde_json::Value;

/// What is signed before the manifest's bytes.
pub const MANIFEST_DOMAIN: &[u8] = b"CITAR-HELPER-MANIFEST-V1\0";
/// The manifest's version.
pub const MANIFEST_VERSION: u64 = 1;
/// The most keys `update-keys.pub` may hold: the current one and its successor.
pub const MAX_KEYS: usize = 2;
/// The largest manifest read, in bytes (PROTOCOL.md: `update.manifest` is at most 65,536).
pub const MANIFEST_MAX_BYTES: usize = 65_536;
/// The most assets a manifest names (the release builds five).
pub const MAX_ASSETS: usize = 32;
/// The largest asset a manifest may name: a helper binary is tens of megabytes.
pub const ASSET_MAX_BYTES: u64 = 512 << 20;
/// The trusted keys compiled in: `update-keys.pub`.
pub const UPDATE_KEYS: &str = include_str!("../update-keys.pub");

/// One release's helper binaries, as its signed manifest says (P4.3.8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_VERSION`].
    pub v: u64,
    /// The release's version (semver).
    pub version: String,
    /// The protocol its helpers speak.
    pub protocol: u64,
    /// The engine build id its helpers carry (`citar_bot::build_id`), 12 hex digits.
    pub build: String,
    /// When it was released (informational).
    pub released_at: String,
    /// Each target's binary, by Rust target triple.
    pub assets: BTreeMap<String, Asset>,
}

/// One target's binary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Asset {
    /// The file's name (a plain name: no folder).
    pub name: String,
    /// Where it is downloaded from.
    pub url: String,
    /// Its SHA-256, 64 lowercase hex digits.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

/// Why a manifest was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// No key is trusted: this build cannot update (`unavailable`).
    #[error("this build trusts no update key")]
    NoKey,
    /// Over [`MANIFEST_MAX_BYTES`].
    #[error("the manifest is {0} bytes, over the {MANIFEST_MAX_BYTES} a manifest may be")]
    TooLarge(usize),
    /// The signature is not one, or no trusted key made it.
    #[error("{0}")]
    Signature(String),
    /// Signed, but not a manifest this release reads.
    #[error("the manifest does not read: {0}")]
    Invalid(String),
}

impl ManifestError {
    /// Whether this is the signature's failure (a bad signature or an unknown key).
    #[must_use]
    pub const fn is_signature(&self) -> bool {
        matches!(self, Self::Signature(_))
    }
}

/// The keys a key file holds (`update-keys.pub`'s format, which `cargo xtask keygen` writes as the
/// public half): one standard-base64 ed25519 public key a line, blank lines and `#` lines read
/// past, at most [`MAX_KEYS`].
///
/// # Errors
/// A line that is not a key, or more than [`MAX_KEYS`] of them.
pub fn parse_keys(text: &str) -> Result<Vec<VerifyingKey>, String> {
    let mut keys = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = crate::claim::verifying_key(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.len() > MAX_KEYS {
        return Err(format!("{} keys, over the {MAX_KEYS} a key file may hold", keys.len()));
    }
    Ok(keys)
}

/// The update keys this build trusts: `update-keys.pub` as compiled in (none in Phase 4).
///
/// # Errors
/// The file does not read as a key file (a test holds the committed one to the format).
pub fn trusted_keys() -> Result<Vec<VerifyingKey>, String> {
    parse_keys(UPDATE_KEYS).map_err(|e| format!("update-keys.pub: {e}"))
}

/// What a manifest's signature covers: [`MANIFEST_DOMAIN`] and the manifest's bytes.
#[must_use]
pub fn message(manifest: &[u8]) -> Vec<u8> {
    let mut m = MANIFEST_DOMAIN.to_vec();
    m.extend_from_slice(manifest);
    m
}

/// The manifest's bytes signed by `key`, standard base64 of the 64-byte signature: what `cargo
/// xtask sign-manifest` writes to the `.sig` file, and what the tests sign with a throwaway key.
#[must_use]
pub fn sign(manifest: &[u8], key: &SigningKey) -> String {
    STANDARD.encode(key.sign(&message(manifest)).to_bytes())
}

/// The manifest `manifest` (its bytes as published) when `signature` (standard base64) is a
/// trusted key's signature over them, read and checked.
///
/// # Errors
/// No key is trusted, the manifest is too large, the signature is not one of a trusted key, or the
/// signed manifest does not read.
pub fn verify(
    manifest: &[u8],
    signature: &str,
    keys: &[VerifyingKey],
) -> Result<Manifest, ManifestError> {
    if keys.is_empty() {
        return Err(ManifestError::NoKey);
    }
    if manifest.len() > MANIFEST_MAX_BYTES {
        return Err(ManifestError::TooLarge(manifest.len()));
    }
    let bytes = STANDARD
        .decode(signature.trim())
        .map_err(|e| ManifestError::Signature(format!("the signature is not base64: {e}")))?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|b: Vec<u8>| {
        ManifestError::Signature(format!("the signature is {} bytes, not 64", b.len()))
    })?;
    let sig = Signature::from_bytes(&bytes);
    let msg = message(manifest);
    if !keys.iter().any(|k| k.verify_strict(&msg, &sig).is_ok()) {
        return Err(ManifestError::Signature(
            "the signature does not verify against the update keys this build trusts (a bad \
             signature, or an unknown key)"
                .to_owned(),
        ));
    }
    Manifest::read(manifest).map_err(ManifestError::Invalid)
}

/// Whether `candidate` is a later version than `current` (semver: a pre-release sorts before its
/// release, `0.1.6-test.1` before `0.1.6-test.2`); `None` when either is not a version.
#[must_use]
pub fn newer(candidate: &str, current: &str) -> Option<bool> {
    let a = semver::Version::parse(candidate.trim()).ok()?;
    let b = semver::Version::parse(current.trim()).ok()?;
    Some(a > b)
}

/// Whether `s` is `n` lowercase hex digits.
fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A plain file name: letters, digits, `.`, `_` and `-`, no folder, not `.` or `..`.
#[must_use]
pub fn is_plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s != "."
        && s != ".."
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Manifest {
    /// Reads a manifest's bytes and checks every field this release uses (unknown ones are read
    /// past): `v` 1, a semver `version`, a `protocol`, a 12-hex `build`, `released_at`, and each
    /// asset's target, plain file name, URL, 64-hex SHA-256 and size.
    ///
    /// # Errors
    /// The first field that does not hold.
    pub fn read(bytes: &[u8]) -> Result<Manifest, String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("the manifest is not JSON: {e}"))?;
        let Value::Object(m) = v else { return Err("the manifest is not an object".to_owned()) };
        let text = |k: &str, max: usize| -> Result<String, String> {
            match m.get(k) {
                Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= max => {
                    Ok(s.clone())
                }
                Some(other) => {
                    Err(format!("{k}: {} is not a string of 1 to {max} characters", cut(other)))
                }
                None => Err(format!("{k} is missing")),
            }
        };
        let number = |k: &str| -> Result<u64, String> {
            m.get(k).and_then(Value::as_u64).ok_or_else(|| format!("{k} is not a whole number"))
        };
        let v = number("v")?;
        if v != MANIFEST_VERSION {
            return Err(format!(
                "v is {v}: this release reads manifests of version {MANIFEST_VERSION}"
            ));
        }
        let version = text("version", 64)?;
        if semver::Version::parse(&version).is_err() {
            return Err(format!("version {:?} is not a semantic version", cut_str(&version)));
        }
        let protocol = number("protocol")?;
        let build = text("build", 12)?;
        if !is_hex(&build, 12) {
            return Err(format!("build {:?} is not 12 lowercase hex digits", cut_str(&build)));
        }
        let released_at = text("released_at", 64)?;
        let Some(Value::Object(listed)) = m.get("assets") else {
            return Err("assets is not an object".to_owned());
        };
        if listed.len() > MAX_ASSETS {
            return Err(format!(
                "{} assets, over the {MAX_ASSETS} a manifest may name",
                listed.len()
            ));
        }
        let mut assets = BTreeMap::new();
        for (target, a) in listed {
            let ok_target = !target.is_empty()
                && target.len() <= 64
                && target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
            if !ok_target {
                return Err(format!(
                    "the asset target {:?} is not a target triple",
                    cut_str(target)
                ));
            }
            let at = |what: &str| format!("assets.{target}.{what}");
            let Value::Object(a) = a else {
                return Err(format!("assets.{target} is not an object"));
            };
            let field = |k: &str, max: usize| -> Result<String, String> {
                match a.get(k) {
                    Some(Value::String(s)) if !s.is_empty() && s.len() <= max => Ok(s.clone()),
                    _ => Err(format!("{} is not a string of 1 to {max} bytes", at(k))),
                }
            };
            let name = field("name", 128)?;
            if !is_plain_name(&name) {
                return Err(format!(
                    "{} {:?} is not a plain file name",
                    at("name"),
                    cut_str(&name)
                ));
            }
            let url = field("url", 2048)?;
            let sha256 = field("sha256", 64)?;
            if !is_hex(&sha256, 64) {
                return Err(format!("{} is not 64 lowercase hex digits", at("sha256")));
            }
            let size =
                a.get("size").and_then(Value::as_u64).filter(|s| (1..=ASSET_MAX_BYTES).contains(s));
            let Some(size) = size else {
                return Err(format!("{} is not a size of 1 byte to {ASSET_MAX_BYTES}", at("size")));
            };
            assets.insert(target.clone(), Asset { name, url, sha256, size });
        }
        Ok(Manifest { v, version, protocol, build, released_at, assets })
    }
}

/// A value for an error message, cut short.
fn cut(v: &Value) -> String {
    cut_str(&v.to_string())
}

fn cut_str(s: &str) -> String {
    s.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn manifest(version: &str) -> String {
        format!(
            "{{\"v\":1,\"version\":\"{version}\",\"protocol\":2,\"build\":\"0b412a18fe95\",\
             \"released_at\":\"2026-11-01T00:00:00Z\",\"assets\":{{\"x86_64-pc-windows-msvc\":\
             {{\"name\":\"citar-helper-windows-x64.exe\",\"url\":\"https://example.com/a.exe\",\
             \"sha256\":\"{}\",\"size\":9437184}}}},\"later\":{{\"field\":true}}}}",
            "ab".repeat(32)
        )
    }

    /// The domain is `cargo xtask sign-manifest`'s too: xtask cannot link this crate, so the two
    /// copies are held equal here (xtask/src/manifest.rs).
    #[test]
    fn the_domain_is_xtasks() {
        let xtask = include_str!("../../../xtask/src/manifest.rs");
        assert!(
            xtask.contains("pub const MANIFEST_DOMAIN: &[u8] = b\"CITAR-HELPER-MANIFEST-V1\\0\";"),
            "xtask/src/manifest.rs must sign with the same domain"
        );
        assert_eq!(MANIFEST_DOMAIN, b"CITAR-HELPER-MANIFEST-V1\0");
    }

    /// Phase 4 ships no key: the committed file is empty, so every build of this tree trusts
    /// none and reports updates unavailable (P4.3.8; the owner's Phase 5 adds the public key).
    #[test]
    fn the_committed_key_file_trusts_no_key() {
        assert_eq!(trusted_keys(), Ok(Vec::new()));
        assert!(UPDATE_KEYS.trim().is_empty(), "crates/citar-jobs/update-keys.pub stays empty");
    }

    #[test]
    fn a_key_file_holds_at_most_two_keys_a_line_each() {
        let a = crate::claim::public_b64(&key(1));
        let b = crate::claim::public_b64(&key(2));
        let c = crate::claim::public_b64(&key(3));
        assert_eq!(parse_keys(&format!("# the owner's key\n{a}\n\n{b}\n")).map(|k| k.len()), Ok(2));
        assert_eq!(parse_keys(&format!("{a}\n{a}\n")).map(|k| k.len()), Ok(1));
        assert!(parse_keys(&format!("{a}\n{b}\n{c}\n")).expect_err("three").contains("over the 2"));
        assert!(parse_keys("not a key\n").expect_err("garbage").starts_with("line 1"));
    }

    #[test]
    fn a_trusted_keys_signature_verifies_and_anything_else_is_refused() {
        let text = manifest("0.1.7");
        let sig = sign(text.as_bytes(), &key(7));
        let trusted = [key(7).verifying_key()];
        let m = verify(text.as_bytes(), &sig, &trusted).expect("verifies");
        assert_eq!(m.version, "0.1.7");
        assert_eq!(m.assets["x86_64-pc-windows-msvc"].size, 9_437_184);
        // Either key of two.
        assert!(
            verify(text.as_bytes(), &sig, &[key(8).verifying_key(), key(7).verifying_key()])
                .is_ok()
        );
        // An unknown key, a changed byte, a bad signature, no key at all.
        assert!(
            verify(text.as_bytes(), &sig, &[key(8).verifying_key()])
                .expect_err("unknown")
                .is_signature()
        );
        let changed = text.replace("0.1.7", "0.1.8");
        assert!(verify(changed.as_bytes(), &sig, &trusted).expect_err("changed").is_signature());
        assert!(verify(text.as_bytes(), "AAAA", &trusted).expect_err("short").is_signature());
        assert!(
            verify(text.as_bytes(), "not base64!", &trusted).expect_err("garbage").is_signature()
        );
        assert_eq!(verify(text.as_bytes(), &sig, &[]), Err(ManifestError::NoKey));
        // A signature over the bytes without the domain is not one.
        let bare = STANDARD.encode(key(7).sign(text.as_bytes()).to_bytes());
        assert!(verify(text.as_bytes(), &bare, &trusted).expect_err("no domain").is_signature());
        let big = vec![b' '; MANIFEST_MAX_BYTES + 1];
        assert_eq!(
            verify(&big, &sig, &trusted),
            Err(ManifestError::TooLarge(MANIFEST_MAX_BYTES + 1))
        );
    }

    #[test]
    fn a_signed_manifest_is_read_field_by_field() {
        let good = manifest("0.1.7");
        assert!(Manifest::read(good.as_bytes()).is_ok());
        for (from, to, why) in [
            ("\"v\":1", "\"v\":2", "v is 2"),
            ("\"version\":\"0.1.7\"", "\"version\":\"seven\"", "not a semantic version"),
            ("\"build\":\"0b412a18fe95\"", "\"build\":\"0B412A18FE95\"", "12 lowercase hex"),
            (
                "\"name\":\"citar-helper-windows-x64.exe\"",
                "\"name\":\"../evil.exe\"",
                "plain file name",
            ),
            ("\"size\":9437184", "\"size\":0", "a size of 1 byte"),
            ("\"sha256\":\"abab", "\"sha256\":\"ABAB", "64 lowercase hex"),
            ("\"protocol\":2,", "", "protocol is not a whole number"),
        ] {
            let bad = good.replace(from, to);
            assert_ne!(bad, good, "{from}");
            let e = Manifest::read(bad.as_bytes()).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
        }
        assert!(Manifest::read(b"[]").is_err());
    }

    #[test]
    fn versions_compare_as_semver_does() {
        assert_eq!(newer("0.1.6-test.2", "0.1.6-test.1"), Some(true));
        assert_eq!(newer("0.1.6-test.1", "0.1.6-test.2"), Some(false));
        assert_eq!(newer("0.1.6", "0.1.6-test.2"), Some(true));
        assert_eq!(newer("0.1.6", "0.1.6"), Some(false));
        assert_eq!(newer("0.1.5", "0.1.6"), Some(false));
        assert_eq!(newer("0.1.10", "0.1.9"), Some(true));
        assert_eq!(newer("seven", "0.1.6"), None);
    }
}
