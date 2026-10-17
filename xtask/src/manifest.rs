//! The helper's update channel, the release side (design/phase4-helper.md P4.3.8, P4.10): the
//! tools whichever custody the owner chose signs with.
//!
//! - `cargo xtask keygen --out DIR`: a new ed25519 update-signing key, written as
//!   `DIR/citar-update-signing.key` (JSON: `{v, kind, public, secret}`, the secret's 32 bytes in
//!   standard base64; its owner's alone before the secret is written: mode 0600 on Unix, and on
//!   Windows a protected DACL for the user, SYSTEM and Administrators, as the helper's machine key
//!   has) and `DIR/citar-update-signing.pub` (the public key, the
//!   line `crates/citar-jobs/update-keys.pub` takes). It refuses a directory inside this repository
//!   or any git working tree, and never overwrites a key.
//! - `cargo xtask helper-manifest --assets DIR --version V [--base-url URL] [--build B]
//!   [--protocol P] [--released-at T] [--out FILE]`: `citar-helper-manifest.json` over the
//!   release's helper binaries in `DIR` (P4.10's five unversioned names), each with its URL
//!   (`URL/<name>`, by default the release tag's download folder), SHA-256 and size. The engine
//!   build and the protocol are read from an asset that runs here (`--version`), which must also
//!   print `V` and its own target; `--build` and `--protocol` stand in when none runs.
//! - `cargo xtask sign-manifest (--key FILE | --key-env NAME) MANIFEST [--out SIG]`: the
//!   manifest's signature, `MANIFEST.sig` (standard base64 of ed25519 over [`MANIFEST_DOMAIN`] and
//!   the manifest's bytes as published). It refuses a key file inside this repository or any git
//!   working tree (so one known to git too), reads the key from an environment variable instead
//!   with `--key-env` (CI: the `helper-signing` environment's secret, never written to disk), and
//!   says when this tree's `update-keys.pub` does not trust the key it signed with.
//!
//! xtask links no workspace crate (its row of the crate graph is empty), so it signs with
//! `ed25519-dalek` directly over the manifest file's bytes: the domain is the one thing it shares
//! with `citar_jobs::update`, whose test holds the two copies equal.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::develop::is_within;

/// What is signed before the manifest's bytes (`citar_jobs::update::MANIFEST_DOMAIN`).
pub const MANIFEST_DOMAIN: &[u8] = b"CITAR-HELPER-MANIFEST-V1\0";

/// The key file `keygen` writes.
pub const KEY_FILE: &str = "citar-update-signing.key";
/// The public key file `keygen` writes beside it.
pub const PUB_FILE: &str = "citar-update-signing.pub";
/// What a key file says it is.
pub const KEY_KIND: &str = "citar-helper-update-signing-key";
/// The manifest's file name.
pub const MANIFEST_FILE: &str = "citar-helper-manifest.json";
/// Where a release's assets are downloaded from, `{version}` replaced.
pub const DEFAULT_BASE_URL: &str =
    "https://github.com/jprodgers/CITAR/releases/download/v{version}";

/// The release's helper binaries (P4.10): each target and its unversioned asset name.
pub const TARGETS: &[(&str, &str)] = &[
    ("x86_64-pc-windows-msvc", "citar-helper-windows-x64.exe"),
    ("aarch64-apple-darwin", "citar-helper-macos-arm64"),
    ("x86_64-apple-darwin", "citar-helper-macos-x64"),
    ("x86_64-unknown-linux-gnu", "citar-helper-linux-x64"),
    ("aarch64-unknown-linux-gnu", "citar-helper-linux-arm64"),
];

/// A refusal or a failure, with its exit code (2 refused, 1 failed).
#[derive(Debug, PartialEq, Eq)]
pub struct Stop(pub u8, pub String);

fn refused(why: impl Into<String>) -> Stop {
    Stop(2, why.into())
}

fn failed(why: impl Into<String>) -> Stop {
    Stop(1, why.into())
}

/// Runs one of the three commands; prints what it did, or why not.
pub fn run(root: &Path, command: &str, args: &[String]) -> ExitCode {
    let done = match command {
        "keygen" => keygen(root, args),
        "helper-manifest" => helper_manifest(args),
        "sign-manifest" => sign_manifest(root, args, &|name| std::env::var(name).ok()),
        other => Err(refused(format!("no command {other}"))),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop(code, why)) => {
            eprintln!("{command}: {why}");
            ExitCode::from(code)
        }
    }
}

/// A command line's `--name VALUE` pairs.
type Named = Vec<(String, String)>;

/// `--name VALUE` pairs and the bare arguments, in order.
fn flags(args: &[String], known: &[&str]) -> Result<(Named, Vec<String>), Stop> {
    let mut named = Vec::new();
    let mut bare = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            if !known.contains(&name) {
                return Err(refused(format!(
                    "unknown option {a} (it takes --{})",
                    known.join(", --")
                )));
            }
            let v = it.next().ok_or_else(|| refused(format!("{a} needs a value")))?;
            named.push((name.to_owned(), v.clone()));
        } else {
            bare.push(a.clone());
        }
    }
    Ok((named, bare))
}

fn flag<'a>(named: &'a [(String, String)], name: &str) -> Option<&'a str> {
    named.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// `p` made absolute against the current directory.
fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// The git working tree `p` (or its nearest folder that exists) is in, if any.
fn git_tree_of(p: &Path) -> Option<PathBuf> {
    let mut dir = absolute(p);
    while !dir.is_dir() {
        dir = dir.parent()?.to_path_buf();
    }
    let out =
        Command::new("git").arg("-C").arg(&dir).args(["rev-parse", "--show-toplevel"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let top = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            (!top.is_empty()).then(|| PathBuf::from(top))
        }
        _ => None,
    }
}

/// Refuses a place for a private key: inside this repository, or inside any git working tree
/// (where a key is one `git add` from being committed, and may be tracked already).
fn private_place(root: &Path, p: &Path, what: &str) -> Result<(), Stop> {
    let abs = absolute(p);
    if is_within(&abs, root) {
        return Err(refused(format!(
            "{what} {} is inside this repository ({}): keep update-signing keys outside it",
            abs.display(),
            root.display()
        )));
    }
    if let Some(tree) = git_tree_of(&abs) {
        return Err(refused(format!(
            "{what} {} is inside the git working tree {}: keep update-signing keys outside every \
             repository, so no key is ever committed",
            abs.display(),
            tree.display()
        )));
    }
    Ok(())
}

/// A key file's text (`keygen`'s format).
#[must_use]
pub fn key_json(key: &SigningKey) -> String {
    let doc = json!({
        "v": 1,
        "kind": KEY_KIND,
        "public": STANDARD.encode(key.verifying_key().to_bytes()),
        "secret": STANDARD.encode(key.to_bytes()),
    });
    format!("{doc}\n")
}

/// The signing key a key file's text holds, its public half checked against its secret.
///
/// # Errors
/// Not a key file of this kind, or one whose halves differ.
pub fn read_key(text: &str) -> Result<SigningKey, String> {
    let doc: Value = serde_json::from_str(text.trim())
        .map_err(|e| format!("not an update-signing key file: {e}"))?;
    if doc.get("kind").and_then(Value::as_str) != Some(KEY_KIND) || doc.get("v") != Some(&json!(1))
    {
        return Err(format!("not an update-signing key file (its kind must be {KEY_KIND}, v 1)"));
    }
    let secret = doc.get("secret").and_then(Value::as_str).ok_or("it has no secret")?;
    let bytes = STANDARD.decode(secret).map_err(|e| format!("its secret is not base64: {e}"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|b: Vec<u8>| format!("its secret is {} bytes, not 32", b.len()))?;
    let key = SigningKey::from_bytes(&bytes);
    let public = doc.get("public").and_then(Value::as_str).unwrap_or_default();
    if public != STANDARD.encode(key.verifying_key().to_bytes()) {
        return Err("its public key is not its secret's".to_owned());
    }
    Ok(key)
}

/// Writes `text` to `path`, which must not exist yet. When `private`, the file is its owner's
/// alone before anything is written in it: mode 0600 on Unix from its creation; on Windows
/// `owner_only` on the empty file (else a `--out` such as `C:\keys`, which inherits
/// Authenticated Users' modify right, would leave the release's signing key readable and
/// replaceable by every local user).
fn write_new(path: &Path, text: &str, private: bool) -> Result<(), String> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(windows)]
    if private && let Err(e) = owner_only(path) {
        drop(f);
        let _gone = std::fs::remove_file(path);
        return Err(format!(
            "{}: could not make it owner-only ({e}): nothing written",
            path.display()
        ));
    }
    #[cfg(not(windows))]
    let _private = private;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// The local system account and the Administrators group, beside the user.
#[cfg(windows)]
const SYSTEM_AND_ADMINISTRATORS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// Gives `path` a protected DACL granting full control to the current user, SYSTEM and
/// Administrators and nothing to anyone else (citar-jobs' `sys::restrict_to_owner`, which xtask
/// does not link: `icacls`, with the user's SID from `whoami`, so no name is localised).
///
/// # Errors
/// Either tool failed.
#[cfg(windows)]
fn owner_only(path: &Path) -> Result<(), String> {
    let me = current_user_sid()?;
    let mut cmd = Command::new("icacls");
    cmd.arg(path).args(["/inheritance:r", "/grant:r"]).arg(format!("*{me}:F"));
    for sid in SYSTEM_AND_ADMINISTRATORS {
        cmd.arg(format!("*{sid}:F"));
    }
    let out = cmd.output().map_err(|e| format!("icacls: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "icacls: {}",
            String::from_utf8_lossy(&out.stdout).trim().lines().last().unwrap_or("failed")
        ));
    }
    Ok(())
}

/// The SID of the user this runs as (`whoami /user`, as CSV: `"name","S-1-5-..."`).
///
/// # Errors
/// `whoami` failed or printed no SID.
#[cfg(windows)]
fn current_user_sid() -> Result<String, String> {
    let out = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .map_err(|e| format!("whoami: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .rsplit(',')
        .next()
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| {
            s.starts_with("S-1-") && s.bytes().all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
        })
        .map(str::to_owned)
        .ok_or_else(|| format!("whoami printed no SID: {}", text.trim()))
}

fn keygen(root: &Path, args: &[String]) -> Result<(), Stop> {
    let (named, bare) = flags(args, &["out"])?;
    if !bare.is_empty() {
        return Err(refused(format!("unexpected {}", bare.join(" "))));
    }
    let out = PathBuf::from(flag(&named, "out").ok_or_else(|| refused("keygen needs --out DIR"))?);
    private_place(root, &out, "the directory")?;
    std::fs::create_dir_all(&out).map_err(|e| failed(format!("{}: {e}", out.display())))?;
    let (key_path, pub_path) = (out.join(KEY_FILE), out.join(PUB_FILE));
    for p in [&key_path, &pub_path] {
        if p.exists() {
            return Err(refused(format!("{} exists: a key is never overwritten", p.display())));
        }
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| failed(format!("the OS's random source: {e}")))?;
    let key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    let public = STANDARD.encode(key.verifying_key().to_bytes());
    write_new(&key_path, &key_json(&key), true).map_err(failed)?;
    write_new(&pub_path, &format!("{public}\n"), false).map_err(failed)?;
    println!(
        "wrote the private key {} (keep it private: it signs helper updates)",
        key_path.display()
    );
    println!("wrote the public key {}:", pub_path.display());
    println!("  {public}");
    println!("A release trusts it once that line is in crates/citar-jobs/update-keys.pub.");
    Ok(())
}

/// What `citar-helper --version` says of a build: its version, engine build, protocol and target.
#[derive(Debug, PartialEq, Eq)]
pub struct Probed {
    pub version: String,
    pub build: String,
    pub protocol: u64,
    pub target: String,
}

/// Reads `citar-helper <version> (build <b>, jobs build <j>, protocol <p>, target <t>, <label>)`.
#[must_use]
pub fn read_version_line(line: &str) -> Option<Probed> {
    let rest = line.trim().strip_prefix("citar-helper ")?;
    let (version, inner) = rest.split_once(" (")?;
    let mut build = None;
    let mut protocol = None;
    let mut target = None;
    for part in inner.trim_end_matches(')').split(", ") {
        if let Some(b) = part.strip_prefix("build ") {
            build = Some(b.to_owned());
        } else if let Some(p) = part.strip_prefix("protocol ") {
            protocol = p.parse().ok();
        } else if let Some(t) = part.strip_prefix("target ") {
            target = Some(t.to_owned());
        }
    }
    Some(Probed {
        version: version.to_owned(),
        build: build?,
        protocol: protocol?,
        target: target?,
    })
}

/// Runs `exe --version`, when it runs here.
fn probe(exe: &Path) -> Option<Probed> {
    let out = Command::new(exe).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().find_map(read_version_line)
}

/// The SHA-256 (lowercase hex) and size of a file.
fn digest(path: &Path) -> Result<(String, u64), String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((hex, size))
}

/// `t` (seconds since 1970) as RFC 3339 in UTC.
#[must_use]
pub fn rfc3339(t: u64) -> String {
    let days = i64::try_from(t / 86_400).unwrap_or(0);
    let secs = t % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn helper_manifest(args: &[String]) -> Result<(), Stop> {
    let known = ["assets", "version", "base-url", "build", "protocol", "released-at", "out"];
    let (named, bare) = flags(args, &known)?;
    if !bare.is_empty() {
        return Err(refused(format!("unexpected {}", bare.join(" "))));
    }
    let assets =
        PathBuf::from(flag(&named, "assets").ok_or_else(|| refused("it needs --assets DIR"))?);
    let version = flag(&named, "version").ok_or_else(|| refused("it needs --version V"))?;
    if semver::Version::parse(version).is_err() {
        return Err(refused(format!("--version {version} is not a semantic version")));
    }
    let base = flag(&named, "base-url").map_or_else(
        || DEFAULT_BASE_URL.replace("{version}", version),
        |b| b.trim_end_matches('/').to_owned(),
    );
    let mut listed = Map::new();
    let mut probed: Option<Probed> = None;
    for (target, name) in TARGETS {
        let path = assets.join(name);
        if !path.is_file() {
            eprintln!("helper-manifest: no {name} in {} ({target} left out)", assets.display());
            continue;
        }
        let (sha256, size) = digest(&path).map_err(failed)?;
        if let Some(p) = probe(&path) {
            if p.version != version || p.target != *target {
                return Err(refused(format!(
                    "{name} says it is {} for {}, not {version} for {target}",
                    p.version, p.target
                )));
            }
            if probed.as_ref().is_some_and(|q| q.build != p.build || q.protocol != p.protocol) {
                return Err(refused("the assets that run here disagree on their build".to_owned()));
            }
            probed = Some(p);
        }
        listed.insert(
            (*target).to_owned(),
            json!({"name": name, "url": format!("{base}/{name}"), "sha256": sha256, "size": size}),
        );
    }
    if listed.is_empty() {
        return Err(refused(format!("no helper binary in {}", assets.display())));
    }
    let build = match (flag(&named, "build"), &probed) {
        (Some(b), Some(p)) if b != p.build => {
            return Err(refused(format!("--build {b}, but the assets say {}", p.build)));
        }
        (Some(b), _) => b.to_owned(),
        (None, Some(p)) => p.build.clone(),
        (None, None) => {
            return Err(refused("no asset runs here to read its build from: give --build"));
        }
    };
    if build.len() != 12 || !build.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused(format!("the build {build} is not 12 lowercase hex digits")));
    }
    let protocol = match (flag(&named, "protocol"), &probed) {
        (Some(p), _) => {
            p.parse::<u64>().map_err(|_| refused(format!("--protocol {p} is no number")))?
        }
        (None, Some(p)) => p.protocol,
        (None, None) => 2,
    };
    let released_at = flag(&named, "released-at").map_or_else(
        || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            rfc3339(now)
        },
        str::to_owned,
    );
    let doc = json!({
        "v": 1,
        "version": version,
        "protocol": protocol,
        "build": build,
        "released_at": released_at,
        "assets": Value::Object(listed),
    });
    let out = flag(&named, "out").map_or_else(|| assets.join(MANIFEST_FILE), PathBuf::from);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| failed(e.to_string()))? + "\n";
    std::fs::write(&out, text).map_err(|e| failed(format!("{}: {e}", out.display())))?;
    println!(
        "wrote {} ({version}, build {build}, {} assets)",
        out.display(),
        doc["assets"].as_object().map_or(0, Map::len)
    );
    Ok(())
}

/// The signature of a manifest's bytes: standard base64 of ed25519 over the domain and them.
#[must_use]
pub fn signature(manifest: &[u8], key: &SigningKey) -> String {
    let mut msg = MANIFEST_DOMAIN.to_vec();
    msg.extend_from_slice(manifest);
    STANDARD.encode(key.sign(&msg).to_bytes())
}

/// The keys `crates/citar-jobs/update-keys.pub` holds (blank and `#` lines read past).
fn trusted(root: &Path) -> Vec<VerifyingKey> {
    let text =
        std::fs::read_to_string(root.join("crates/citar-jobs/update-keys.pub")).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| STANDARD.decode(l).ok())
        .filter_map(|b| <[u8; 32]>::try_from(b).ok())
        .filter_map(|b| VerifyingKey::from_bytes(&b).ok())
        .collect()
}

/// `sign-manifest`; `env` reads an environment variable (`--key-env`).
fn sign_manifest(
    root: &Path,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), Stop> {
    let (named, bare) = flags(args, &["key", "key-env", "out"])?;
    let [manifest] = bare.as_slice() else {
        return Err(refused("it signs one manifest: sign-manifest --key FILE MANIFEST"));
    };
    let manifest = PathBuf::from(manifest);
    let key_text = match (flag(&named, "key"), flag(&named, "key-env")) {
        (Some(_), Some(_)) => return Err(refused("give --key or --key-env, not both")),
        (Some(file), None) => {
            let file = PathBuf::from(file);
            private_place(root, &file, "the key file")?;
            std::fs::read_to_string(&file)
                .map_err(|e| failed(format!("{}: {e}", file.display())))?
        }
        (None, Some(var)) => env(var)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| refused(format!("the environment variable {var} holds no key")))?,
        (None, None) => return Err(refused("it needs --key FILE or --key-env NAME")),
    };
    let key = read_key(&key_text).map_err(refused)?;
    let bytes =
        std::fs::read(&manifest).map_err(|e| failed(format!("{}: {e}", manifest.display())))?;
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|e| refused(format!("{} is not JSON: {e}", manifest.display())))?;
    if doc.get("v") != Some(&json!(1)) || !doc.get("assets").is_some_and(Value::is_object) {
        return Err(refused(format!(
            "{} is not a helper manifest (v 1, with assets)",
            manifest.display()
        )));
    }
    let out = flag(&named, "out").map_or_else(
        || {
            let mut s = manifest.clone().into_os_string();
            s.push(".sig");
            PathBuf::from(s)
        },
        PathBuf::from,
    );
    std::fs::write(&out, signature(&bytes, &key) + "\n")
        .map_err(|e| failed(format!("{}: {e}", out.display())))?;
    let public = key.verifying_key();
    println!("wrote {} (key {})", out.display(), STANDARD.encode(public.to_bytes()));
    if !trusted(root).contains(&public) {
        println!(
            "note: crates/citar-jobs/update-keys.pub does not hold this key, so helpers built from \
             this tree will not trust the signature"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citar-xtask-{name}-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&d);
        d
    }

    fn root() -> PathBuf {
        crate::workspace_root()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn keygen_refuses_the_repository_and_never_overwrites() {
        let root = root();
        let inside = root.join("target-keys-never");
        let e = keygen(&root, &s(&["--out", &inside.to_string_lossy()])).expect_err("inside");
        assert_eq!(e.0, 2);
        assert!(e.1.contains("inside this repository"), "{}", e.1);
        assert!(!inside.exists(), "nothing was made inside the repository");
        let dir = scratch("keygen");
        keygen(&root, &s(&["--out", &dir.to_string_lossy()])).expect("made");
        let key =
            read_key(&std::fs::read_to_string(dir.join(KEY_FILE)).expect("read")).expect("a key");
        let public = std::fs::read_to_string(dir.join(PUB_FILE)).expect("read");
        assert_eq!(public.trim(), STANDARD.encode(key.verifying_key().to_bytes()));
        let e = keygen(&root, &s(&["--out", &dir.to_string_lossy()])).expect_err("again");
        assert!(e.1.contains("never overwritten"), "{}", e.1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).expect("there").permissions().mode();
            assert_eq!(mode & 0o077, 0, "the key is its owner's alone");
        }
        #[cfg(windows)]
        {
            // The key file's DACL as SDDL (icacls /save, UTF-16): protected, and its entries the
            // user's, SYSTEM's and Administrators' only.
            let me = current_user_sid().expect("the user's SID");
            let saved = dir.join("acl.txt");
            let out = Command::new("icacls")
                .arg(dir.join(KEY_FILE))
                .arg("/save")
                .arg(&saved)
                .output()
                .expect("icacls runs");
            assert!(out.status.success(), "{out:?}");
            let raw = std::fs::read(&saved).expect("saved");
            let units: Vec<u16> =
                raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let text = String::from_utf16_lossy(&units);
            let sddl = text.lines().find(|l| l.contains("D:")).expect("an SDDL line");
            let dacl = &sddl[sddl.find("D:").expect("a DACL")..];
            assert!(dacl.starts_with("D:P"), "protected: {dacl}");
            let allowed = [me.as_str(), "SY", "BA", "S-1-5-18", "S-1-5-32-544"];
            let aces: Vec<&str> = dacl.split('(').skip(1).collect();
            assert!(!aces.is_empty(), "{dacl}");
            for ace in aces {
                let sid = ace.trim_end_matches(')').rsplit(';').next().expect("a SID");
                assert!(allowed.contains(&sid), "{sid} may not reach the key: {dacl}");
            }
        }
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn sign_manifest_refuses_a_key_in_the_repository_and_signs_with_the_domain() {
        let root = root();
        let dir = scratch("sign");
        std::fs::create_dir_all(&dir).expect("made");
        let key = SigningKey::from_bytes(&[5; 32]);
        let key_path = dir.join(KEY_FILE);
        std::fs::write(&key_path, key_json(&key)).expect("written");
        let manifest = dir.join(MANIFEST_FILE);
        let text = "{\"v\": 1, \"version\": \"0.1.7\", \"assets\": {}}\n";
        std::fs::write(&manifest, text).expect("written");
        // A key file inside the repository is refused before it is read (it need not exist).
        let in_repo = root.join("crates").join("citar-update-signing.key");
        let e = sign_manifest(
            &root,
            &s(&["--key", &in_repo.to_string_lossy(), &manifest.to_string_lossy()]),
            &no_env,
        )
        .expect_err("inside");
        assert!(e.1.contains("inside this repository"), "{}", e.1);
        sign_manifest(
            &root,
            &s(&["--key", &key_path.to_string_lossy(), &manifest.to_string_lossy()]),
            &no_env,
        )
        .expect("signed");
        let sig = std::fs::read_to_string(dir.join(format!("{MANIFEST_FILE}.sig"))).expect("read");
        let bytes: [u8; 64] = STANDARD.decode(sig.trim()).expect("b64").try_into().expect("64");
        let mut msg = MANIFEST_DOMAIN.to_vec();
        msg.extend_from_slice(text.as_bytes());
        key.verifying_key()
            .verify_strict(&msg, &ed25519_dalek::Signature::from_bytes(&bytes))
            .expect("the signature covers the domain and the bytes as published");
        // From the environment, as CI signs: the key never touches the disk.
        let json = key_json(&key);
        let env = |name: &str| (name == "CITAR_UPDATE_SIGNING_KEY").then(|| json.clone());
        let out = dir.join("env.sig");
        let args = s(&[
            "--key-env",
            "CITAR_UPDATE_SIGNING_KEY",
            "--out",
            &out.to_string_lossy(),
            &manifest.to_string_lossy(),
        ]);
        sign_manifest(&root, &args, &env).expect("signed from the environment");
        assert_eq!(std::fs::read_to_string(&out).expect("read"), sig);
        let args = s(&["--key-env", "NOT_SET_HERE", &manifest.to_string_lossy()]);
        let e = sign_manifest(&root, &args, &env).expect_err("no key in it");
        assert!(e.1.contains("holds no key"), "{}", e.1);
        // Not a key, not a manifest.
        std::fs::write(&key_path, "{\"secret\": \"AAAA\"}").expect("written");
        let e = sign_manifest(
            &root,
            &s(&["--key", &key_path.to_string_lossy(), &manifest.to_string_lossy()]),
            &no_env,
        )
        .expect_err("not a key");
        assert!(e.1.contains("not an update-signing key file"), "{}", e.1);
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn version_lines_read_back() {
        let p = read_version_line(
            "citar-helper 0.1.6-test.2 (build 0b412a18fe95, jobs build 145973491d0a, protocol 2, \
             target x86_64-pc-windows-msvc, v0.1.5-1454-ga5528803)",
        )
        .expect("reads");
        assert_eq!(
            p,
            Probed {
                version: "0.1.6-test.2".to_owned(),
                build: "0b412a18fe95".to_owned(),
                protocol: 2,
                target: "x86_64-pc-windows-msvc".to_owned()
            }
        );
        assert_eq!(read_version_line("something else"), None);
    }

    #[test]
    fn rfc3339_is_utc_civil_time() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_979_200), "2026-10-14T12:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn the_manifest_lists_each_asset_with_its_hash_and_size() {
        let dir = scratch("manifest");
        std::fs::create_dir_all(&dir).expect("made");
        std::fs::write(dir.join("citar-helper-linux-x64"), b"not a program").expect("written");
        let e = helper_manifest(&s(&["--assets", &dir.to_string_lossy(), "--version", "0.1.7"]))
            .expect_err("no build");
        assert!(e.1.contains("give --build"), "{}", e.1);
        helper_manifest(&s(&[
            "--assets",
            &dir.to_string_lossy(),
            "--version",
            "0.1.7",
            "--build",
            "0b412a18fe95",
            "--released-at",
            "2026-11-01T00:00:00Z",
        ]))
        .expect("written");
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST_FILE)).expect("read"))
                .expect("json");
        let a = &doc["assets"]["x86_64-unknown-linux-gnu"];
        assert_eq!(a["size"], 13);
        assert_eq!(
            a["url"],
            "https://github.com/jprodgers/CITAR/releases/download/v0.1.7/citar-helper-linux-x64"
        );
        assert_eq!(a["sha256"].as_str().map(str::len), Some(64));
        assert_eq!((doc["v"].as_u64(), doc["protocol"].as_u64()), (Some(1), Some(2)));
        let e = helper_manifest(&s(&["--assets", &dir.to_string_lossy(), "--version", "seven"]))
            .expect_err("not a version");
        assert!(e.1.contains("not a semantic version"), "{}", e.1);
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
