//! The machine key (design/phase4-helper.md P4.3.7): an ed25519 key made on the first run from
//! the OS's random source, kept in `helper/machine-key.json` as `{"v": 1, "public": base64,
//! "secret": base64}` (the secret is the 32-byte seed). The first protocol 2 hello registers the
//! public key on the machine's row; a later hello with another key is refused until the owner
//! resets it. The fingerprint (the first 16 hex digits of blake3 of the public key) is what the
//! Servers page shows and `citar-helper --version` prints, so an owner can tell their machine.
//!
//! **Owner-only.** The file is written owner-only (mode 0600, or an owner-only DACL on Windows,
//! through citar-jobs' `sys` module) before the secret goes into it, and **at start the helper
//! refuses a key file anyone else can read**, naming the command that fixes it. The secret never
//! leaves the machine: it is in no frame, log or scan. Signing a run's claim (4-10,
//! [`MachineKey::sign_b64`]) happens in this process, never in the job host.

use std::io::Write as _;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use crate::home::Home;

/// The helper's machine key.
pub struct MachineKey {
    signing: SigningKey,
}

impl std::fmt::Debug for MachineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret.
        write!(f, "MachineKey({})", self.fingerprint())
    }
}

/// The fingerprint of a public key: 16 hex digits of blake3 over its 32 bytes.
#[must_use]
pub fn fingerprint_of(public: &[u8; 32]) -> String {
    blake3::hash(public).as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl MachineKey {
    /// A new key from the OS's random source.
    ///
    /// # Errors
    /// The OS would not give random bytes.
    pub fn generate() -> Result<MachineKey, String> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| format!("no random bytes for a key: {e}"))?;
        let signing = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(MachineKey { signing })
    }

    /// The public key's 32 bytes.
    #[must_use]
    pub fn public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The public key in standard base64, as the hello carries it.
    #[must_use]
    pub fn public_b64(&self) -> String {
        STANDARD.encode(self.public())
    }

    /// The key's fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.public())
    }

    /// The ed25519 signature of `message` by this key, in standard base64 (64 bytes): a run's
    /// claim (P4.4.4), signed here in the helper and never in the job host, which plays the games.
    #[must_use]
    pub fn sign_b64(&self, message: &[u8]) -> String {
        use ed25519_dalek::Signer as _;
        STANDARD.encode(self.signing.sign(message).to_bytes())
    }

    fn file_text(&self) -> String {
        let doc = json!({
            "v": 1,
            "public": self.public_b64(),
            "secret": STANDARD.encode(self.signing.to_bytes()),
        });
        format!("{doc:#}\n")
    }
}

/// Why the key could not be used.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// Others can read the key file: refused until it is owner-only.
    #[error("{0}")]
    Exposed(String),
    /// The file is not a machine key.
    #[error(
        "{path}: {why}; it is not a CITAR machine key (move it aside to make a new key, which the server will refuse until the machine's key is reset)"
    )]
    Damaged { path: String, why: String },
    /// The file could not be read or written.
    #[error("{0}")]
    Io(String),
}

fn decode32(v: Option<&Value>) -> Option<[u8; 32]> {
    let bytes = STANDARD.decode(v?.as_str()?).ok()?;
    bytes.try_into().ok()
}

/// The key in `path`, if there is one, refused when others can read it.
///
/// # Errors
/// [`KeyError::Exposed`] with the fixing command, a damaged file, or one that cannot be read.
pub fn load(path: &Path) -> Result<Option<MachineKey>, KeyError> {
    if !path.exists() {
        return Ok(None);
    }
    let shown = path.display().to_string();
    match citar_jobs::sys::exposure(path) {
        Ok(None) => {}
        Ok(Some(e)) => {
            return Err(KeyError::Exposed(format!(
                "refusing the machine key: {}",
                e.describe(path)
            )));
        }
        Err(e) => {
            return Err(KeyError::Io(format!("{shown}: its permissions cannot be read: {e}")));
        }
    }
    let raw = std::fs::read_to_string(path).map_err(|e| KeyError::Io(format!("{shown}: {e}")))?;
    let doc: Value = serde_json::from_str(&raw)
        .map_err(|e| KeyError::Damaged { path: shown.clone(), why: format!("not JSON ({e})") })?;
    if doc.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(KeyError::Damaged { path: shown, why: "no \"v\": 1".to_owned() });
    }
    let damaged = |why: &str| KeyError::Damaged { path: shown.clone(), why: why.to_owned() };
    let mut seed = decode32(doc.get("secret")).ok_or_else(|| damaged("no 32-byte secret"))?;
    let public = decode32(doc.get("public")).ok_or_else(|| damaged("no 32-byte public key"))?;
    let signing = SigningKey::from_bytes(&seed);
    seed.fill(0);
    let key = MachineKey { signing };
    if key.public() != public {
        return Err(damaged("its public key is not its secret's"));
    }
    Ok(Some(key))
}

/// The machine key of `home`: the one on file, or a new one written owner-only.
///
/// # Errors
/// As [`load`], or the new file could not be written owner-only.
pub fn load_or_create(home: &Home) -> Result<(MachineKey, bool), KeyError> {
    let path = home.key_file();
    if let Some(k) = load(&path)? {
        return Ok((k, false));
    }
    let key = MachineKey::generate().map_err(KeyError::Io)?;
    write_new(&path, &key)?;
    Ok((key, true))
}

/// Writes a new key file: an empty file made owner-only first, the secret written into it, then
/// moved into place, so the secret is never in a file others could read.
fn write_new(path: &Path, key: &MachineKey) -> Result<(), KeyError> {
    let io = |e: std::io::Error| KeyError::Io(format!("{}: {e}", path.display()));
    let dir = path.parent().ok_or_else(|| KeyError::Io("the key file has no folder".to_owned()))?;
    std::fs::create_dir_all(dir).map_err(io)?;
    let tmp = path.with_extension(format!("json.new-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        citar_jobs::sys::restrict_to_owner(&tmp)?;
        f.write_all(key.file_text().as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _gone = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    match citar_jobs::sys::exposure(path) {
        Ok(None) => Ok(()),
        Ok(Some(e)) => Err(KeyError::Exposed(format!(
            "the new machine key could not be made owner-only: {}",
            e.describe(path)
        ))),
        Err(e) => Err(io(e)),
    }
}

/// A warning when others can read `path` (`helper.json` holds the token), with the fixing
/// command; `None` when only its owner can, or it does not exist.
#[must_use]
pub fn config_warning(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    match citar_jobs::sys::exposure(path) {
        Ok(Some(e)) => Some(format!("it holds the worker token, and {}", e.describe(path))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_round_trips_and_its_fingerprint_is_stable() {
        let dir = std::env::temp_dir().join(format!("citar-helper-key-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        let home = Home::at(&dir);
        let (k, made) = load_or_create(&home).expect("a key");
        assert!(made);
        let (again, made) = load_or_create(&home).expect("the same key");
        assert!(!made);
        assert_eq!(k.public(), again.public());
        assert_eq!(k.fingerprint().len(), 16);
        assert_eq!(k.fingerprint(), fingerprint_of(&k.public()));
        assert!(!format!("{k:?}").contains(&STANDARD.encode(k.signing.to_bytes())));
        let text = std::fs::read_to_string(home.key_file()).expect("read");
        let doc: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(doc["public"], Value::String(k.public_b64()));
        // A damaged file is refused, not replaced.
        std::fs::write(home.key_file(), "{\"v\": 1}").expect("written");
        citar_jobs::sys::restrict_to_owner(&home.key_file()).expect("owner-only");
        assert!(matches!(load_or_create(&home), Err(KeyError::Damaged { .. })));
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn the_fingerprint_is_blake3s_first_sixteen_hex_digits() {
        let public = [7u8; 32];
        let want = blake3::hash(&public).to_hex().to_string();
        assert_eq!(fingerprint_of(&public), want[..16]);
    }
}
