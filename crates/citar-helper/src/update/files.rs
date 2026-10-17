//! The files an update keeps (design/phase4-helper.md P4.3.8), in the home's `helper/update/`
//! ([`UpdateDir`]) and beside the binary ([`Install`]):
//!
//! - `manifest.json` and `manifest.json.sig`: the last manifest the server relayed that verified,
//!   byte for byte, for `citar-helper update check` and `update apply`;
//! - `<asset>.part`, then `<asset>`: the download, staged once its size and SHA-256 hold;
//! - `pending.json` (`{v, old, new, sha256, starts, at}`): written at the hand-off, before the
//!   swap; the new binary counts its starts in it, and it goes when the new binary is healthy;
//! - `healthy.json` (`{v, version, at}`): the new binary's first welcome (or a minute of running
//!   while the server could not be reached, or a refusal it understood: the binary works); a
//!   supervisor reads "the child ended before it was healthy" as `pending` without `healthy`;
//! - `failed.json` (`{v, version, sha256, message, at, reported}`): an update rolled back, by the
//!   supervisor, by the start count or by hand (`citar-helper update rollback`, written already
//!   `reported`: its owner did it). The old binary reports it `failed` once after its first
//!   welcome, and the same binary (the same version and SHA-256) is refused until the file is
//!   removed, or that version runs healthy here again (installed by hand);
//! - beside the binary: `<name>.old`, the binary it replaced (kept for a rollback, by the
//!   supervisor, by the start count, or by `citar-helper update rollback`); `.<name>.new` while it
//!   is copied in; `<name>.failed`, a binary rolled back while it ran (Windows cannot delete a
//!   running program's file, only rename it), removed at a later start.
//!
//! **The folder is its owner's alone** ([`UpdateDir::secure`], 4-13's fix round): made owner-only
//! (mode 0700; on Windows a protected DACL for the user, SYSTEM and Administrators that what it
//! holds inherits) and checked again before every download, an update refused while anyone else
//! can still reach it. A downloaded binary waits there between its checks, its self-test and its
//! installation, so another local user who could write the folder could swap it for their own
//! program, which the helper would then run and install as its own; or write `pending.json` (a
//! rollback forced, a downgrade) or `failed.json` (updates blocked).
//!
//! **The swap** ([`Install::swap_in`]) renames, never copies over: the new file is first copied
//! into the binary's own folder (so the last step is a rename on one volume) and its SHA-256
//! checked against the signed one there, the running binary is renamed to `<name>.old` (Windows
//! allows renaming a running program's file, not replacing or deleting it; Unix allows all three),
//! and the new file renamed into its place, the running binary renamed back if that last step
//! fails. This is what the `self-replace` crate does on each OS, without its pre-`main` hook and
//! the copy of the program it starts to delete the old file (the design wants the old file kept,
//! for the rollback).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How many starts the new binary gets to become healthy: the start after the third that did not
/// restores the old binary (P4.3.8, 4-13's gate 3).
pub const MAX_STARTS: u32 = 3;
/// How long a rename that the OS refuses for a moment (an antivirus scanning a new file) is tried
/// again, in all.
const RENAME_PATIENCE: Duration = Duration::from_secs(3);

/// `pending.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub v: u32,
    /// The version it replaced.
    pub old: String,
    /// The version it installed.
    pub new: String,
    /// The installed binary's SHA-256 (the manifest's).
    pub sha256: String,
    /// The new binary's starts so far.
    pub starts: u32,
    /// When the hand-off wrote it (Unix time).
    pub at: f64,
}

/// `failed.json`: an update that was rolled back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Failed {
    pub v: u32,
    /// The version that failed.
    pub version: String,
    /// Its binary's SHA-256.
    pub sha256: String,
    /// What happened.
    pub message: String,
    pub at: f64,
    /// Whether the old binary has reported it to its server.
    pub reported: bool,
}

/// `healthy.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Healthy {
    pub v: u32,
    pub version: String,
    pub at: f64,
}

/// Now, as Unix time.
#[must_use]
pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The home's `helper/update/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateDir {
    dir: PathBuf,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Writes `value` as JSON to `path` (in a folder that exists) through a file beside it, renamed
/// over it.
fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(value).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

impl UpdateDir {
    /// The folder `dir` (`helper/update/` of the home).
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> UpdateDir {
        UpdateDir { dir: dir.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Makes the folder when it is missing, owner-only from the start (the module's doc): the
    /// folders above it as usual, this one with mode 0700 on Unix and the owner-only DACL on
    /// Windows. A folder that is there already is left as it is ([`UpdateDir::secure`] checks it).
    ///
    /// # Errors
    /// It could not be made, or made owner-only (it is then removed again).
    pub fn create(&self) -> io::Result<()> {
        if self.dir.is_dir() {
            return Ok(());
        }
        if let Some(parent) = self.dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        let made = {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(&self.dir)
        };
        #[cfg(not(unix))]
        let made = std::fs::DirBuilder::new().create(&self.dir);
        match made {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.dir.is_dir() => {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        if let Err(e) = citar_jobs::sys::restrict_dir_to_owner(&self.dir) {
            let _empty_gone = std::fs::remove_dir(&self.dir);
            return Err(e);
        }
        Ok(())
    }

    /// Before a download: the folder made (owner-only) when it is missing, then checked. One that
    /// someone besides its owner can reach (made so after this helper made it owner-only, or by
    /// hand) is refused with the command that fixes it, as the machine key is (P4.3.7): the
    /// helper does not quietly take back a folder someone opened on purpose.
    ///
    /// # Errors
    /// Why the update cannot go on: the folder could not be made, its permissions cannot be read,
    /// or someone besides its owner can reach it (with the command that fixes it).
    pub fn secure(&self) -> Result<(), String> {
        let shown = self.dir.display();
        self.create().map_err(|e| format!("the update folder {shown} could not be made: {e}"))?;
        match citar_jobs::sys::dir_exposure(&self.dir) {
            Ok(None) => Ok(()),
            Ok(Some(e)) => Err(format!(
                "another user of this machine could replace the update before it is installed: \
                 {}",
                e.describe_folder(&self.dir)
            )),
            Err(e) => {
                Err(format!("the update folder {shown}: its permissions cannot be read: {e}"))
            }
        }
    }

    /// `pending.json`, when it reads.
    #[must_use]
    pub fn pending(&self) -> Option<Pending> {
        read_json(&self.file("pending.json"))
    }

    /// Writes `pending.json`.
    ///
    /// # Errors
    /// It could not be written.
    pub fn write_pending(&self, p: &Pending) -> io::Result<()> {
        self.create()?;
        write_json(&self.file("pending.json"), p)
    }

    /// Removes `pending.json`.
    ///
    /// # Errors
    /// It is there and could not be removed.
    pub fn remove_pending(&self) -> io::Result<()> {
        remove(&self.file("pending.json"))
    }

    /// Whether `healthy.json` is there.
    #[must_use]
    pub fn healthy_exists(&self) -> bool {
        self.file("healthy.json").is_file()
    }

    /// The new binary is healthy: `healthy.json` written, then `pending.json` removed (so a
    /// supervisor that sees `pending` without `healthy` knows the child never became healthy).
    ///
    /// # Errors
    /// Either file could not be written or removed.
    pub fn mark_healthy(&self, version: &str) -> io::Result<()> {
        self.create()?;
        write_json(
            &self.file("healthy.json"),
            &Healthy { v: 1, version: version.to_owned(), at: now_unix() },
        )?;
        self.remove_pending()
    }

    /// The hand-off: `healthy.json` from an earlier update removed, `pending.json` written.
    ///
    /// # Errors
    /// Either file could not be removed or written.
    pub fn begin(&self, old: &str, new: &str, sha256: &str) -> io::Result<()> {
        remove(&self.file("healthy.json"))?;
        self.write_pending(&Pending {
            v: 1,
            old: old.to_owned(),
            new: new.to_owned(),
            sha256: sha256.to_owned(),
            starts: 0,
            at: now_unix(),
        })
    }

    /// `failed.json`, when it reads.
    #[must_use]
    pub fn failed(&self) -> Option<Failed> {
        read_json(&self.file("failed.json"))
    }

    /// Writes `failed.json` (not yet reported).
    ///
    /// # Errors
    /// It could not be written.
    pub fn write_failed(&self, version: &str, sha256: &str, message: &str) -> io::Result<()> {
        self.write_failed_as(version, sha256, message, false)
    }

    /// Writes `failed.json`, already `reported` or not (a rollback by hand is its owner's own
    /// doing: the refusal of that version says so, with no `failed` to report).
    ///
    /// # Errors
    /// It could not be written.
    pub fn write_failed_as(
        &self,
        version: &str,
        sha256: &str,
        message: &str,
        reported: bool,
    ) -> io::Result<()> {
        self.create()?;
        write_json(
            &self.file("failed.json"),
            &Failed {
                v: 1,
                version: version.to_owned(),
                sha256: sha256.to_owned(),
                message: message.to_owned(),
                at: now_unix(),
                reported,
            },
        )
    }

    /// Removes `failed.json` when it names `version`: that binary runs healthy here now (its owner
    /// installed it again by hand), so it is held back no more.
    ///
    /// # Errors
    /// It is there and could not be removed.
    pub fn forget_failed(&self, version: &str) -> io::Result<()> {
        match self.failed() {
            Some(f) if f.version == version => remove(&self.file("failed.json")),
            _ => Ok(()),
        }
    }

    /// Marks `failed.json` reported.
    ///
    /// # Errors
    /// It could not be written.
    pub fn failed_reported(&self, f: &Failed) -> io::Result<()> {
        write_json(&self.file("failed.json"), &Failed { reported: true, ..f.clone() })
    }

    /// Keeps the manifest the server relayed (it verified) and its signature, byte for byte.
    ///
    /// # Errors
    /// They could not be written.
    pub fn keep_manifest(&self, manifest: &str, signature: &str) -> io::Result<()> {
        self.create()?;
        let (m, s) = (self.file("manifest.json"), self.file("manifest.json.sig"));
        std::fs::write(m.with_extension("json.tmp"), manifest)?;
        std::fs::rename(m.with_extension("json.tmp"), &m)?;
        std::fs::write(&s, format!("{signature}\n"))
    }

    /// The manifest and signature kept, when both are there.
    #[must_use]
    pub fn kept_manifest(&self) -> Option<(String, String)> {
        let m = std::fs::read_to_string(self.file("manifest.json")).ok()?;
        let s = std::fs::read_to_string(self.file("manifest.json.sig")).ok()?;
        Some((m, s.trim().to_owned()))
    }

    /// Where an asset is downloaded to, before its checks.
    #[must_use]
    pub fn part(&self, name: &str) -> PathBuf {
        self.file(&format!("{name}.part"))
    }

    /// Where an asset waits for the hand-off once its checks held.
    #[must_use]
    pub fn staged(&self, name: &str) -> PathBuf {
        self.file(name)
    }
}

/// The helper's own binary, the one an update replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    exe: PathBuf,
}

/// Renames `from` to `to`, trying again for a moment when the OS refuses (an antivirus holding a
/// new file open). A missing `from` fails at once.
///
/// # Errors
/// The rename still fails.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let start = std::time::Instant::now();
    let mut wait = Duration::from_millis(20);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
            Err(e) if start.elapsed() >= RENAME_PATIENCE => return Err(e),
            Err(_) => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(500));
            }
        }
    }
}

/// Makes the binary at `path` runnable (Unix: mode 0755; elsewhere nothing to do): a file put in
/// the binary's place by a swap or a rollback runs whatever mode it came with (a copy, or an
/// `.old` an owner put there by hand, may have none). Best effort: a binary that cannot be made
/// runnable fails at its start, where the supervisor or the start count rolls it back.
fn make_runnable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _mode = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _unix_only = path;
}

/// `path` with `suffix` added to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

impl Install {
    /// The binary at `exe`.
    #[must_use]
    pub fn at(exe: impl Into<PathBuf>) -> Install {
        Install { exe: exe.into() }
    }

    /// This process's own binary.
    ///
    /// # Errors
    /// Its path cannot be read.
    pub fn current() -> Result<Install, String> {
        let exe = std::env::current_exe().map_err(|e| format!("this program's own path: {e}"))?;
        Ok(Install { exe })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.exe
    }

    /// `<name>.old`: the binary the last update replaced.
    #[must_use]
    pub fn old(&self) -> PathBuf {
        with_suffix(&self.exe, ".old")
    }

    /// `<name>.failed`: a binary rolled back.
    #[must_use]
    pub fn failed(&self) -> PathBuf {
        with_suffix(&self.exe, ".failed")
    }

    /// `.<name>.new`, beside it: the new binary while it is copied in.
    fn incoming(&self) -> PathBuf {
        let name = self.exe.file_name().map(std::ffi::OsStr::to_string_lossy).unwrap_or_default();
        self.exe.with_file_name(format!(".{name}.new"))
    }

    fn folder(&self) -> &Path {
        self.exe.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Whether this process may write the binary's folder (a probe file made and removed).
    ///
    /// # Errors
    /// Why it cannot.
    pub fn writable(&self) -> Result<(), String> {
        let probe = self.folder().join(format!(".citar-helper-probe-{}", std::process::id()));
        std::fs::write(&probe, b"probe").map_err(|e| {
            format!(
                "this helper cannot replace its own binary: it may not write {} ({e}); update it \
                 by hand, or run it from a folder it owns",
                self.folder().display()
            )
        })?;
        let _gone = std::fs::remove_file(&probe);
        Ok(())
    }

    /// Whether the binary sits in a build's output (cargo's target folder, or the folder `cargo
    /// xtask develop` copies the extension and the helper into): a development build refuses to
    /// replace a binary there, so a test never overwrites what the next test runs.
    #[must_use]
    pub fn in_build_output(&self) -> bool {
        let dir = self.folder();
        [".cargo-lock", ".fingerprint", "deps", "_engine.pyd", "_engine.abi3.so"]
            .iter()
            .any(|marker| dir.join(marker).exists())
    }

    /// Puts `staged` in the binary's place, keeping the binary as `<name>.old` (the module's doc).
    /// The copy made beside the binary must still have the signed `sha256` (lowercase hex): a
    /// file changed after its checks, however long the hand-off waited, is never installed.
    ///
    /// # Errors
    /// A step failed, or the copy is not the binary that was checked; the binary is then in its
    /// place as before.
    pub fn swap_in(&self, staged: &Path, sha256: &str) -> Result<(), String> {
        let incoming = self.incoming();
        let _stale = remove(&incoming);
        std::fs::copy(staged, &incoming)
            .map_err(|e| format!("copying the new binary beside {}: {e}", self.exe.display()))?;
        match super::fetch::sha256_file(&incoming) {
            Ok(got) if got.eq_ignore_ascii_case(sha256) => {}
            Ok(got) => {
                let _gone = remove(&incoming);
                return Err(format!(
                    "the new binary changed after it was checked (its SHA-256 is {got}, the signed \
                     one {sha256}): it is not installed"
                ));
            }
            Err(e) => {
                let _gone = remove(&incoming);
                return Err(format!("reading the new binary back: {e}"));
            }
        }
        make_runnable(&incoming);
        let old = self.old();
        if remove(&old).is_err() {
            // A running program's file (an old binary still running somewhere): set aside.
            let aside = with_suffix(&old, &format!(".{}", std::process::id()));
            let _aside = rename(&old, &aside);
        }
        if let Err(e) = rename(&self.exe, &old) {
            let _gone = remove(&incoming);
            return Err(format!("setting {} aside: {e}", self.exe.display()));
        }
        if let Err(e) = rename(&incoming, &self.exe) {
            let back = rename(&old, &self.exe);
            let _gone = remove(&incoming);
            return Err(match back {
                Ok(()) => format!("putting the new binary in place: {e} (the old one is back)"),
                Err(b) => format!(
                    "putting the new binary in place: {e}; and the old one could not be put back \
                     ({b}): it is {}",
                    old.display()
                ),
            });
        }
        Ok(())
    }

    /// Restores `<name>.old` in the binary's place (a rollback); the binary it replaces is kept as
    /// `<name>.failed`, renamed rather than removed, since it may be the running program.
    ///
    /// # Errors
    /// There is no `<name>.old`, or a rename failed (the binary is then in its place as before).
    pub fn restore_old(&self) -> Result<(), String> {
        let old = self.old();
        if !old.is_file() {
            return Err(format!("there is no {} to restore", old.display()));
        }
        let failed = self.failed();
        if remove(&failed).is_err() {
            let _aside =
                rename(&failed, &with_suffix(&failed, &format!(".{}", std::process::id())));
        }
        rename(&self.exe, &failed)
            .map_err(|e| format!("setting {} aside: {e}", self.exe.display()))?;
        if let Err(e) = rename(&old, &self.exe) {
            let _back = rename(&failed, &self.exe);
            return Err(format!("restoring {}: {e}", old.display()));
        }
        make_runnable(&self.exe);
        Ok(())
    }

    /// `citar-helper update rollback`: the binary and `<name>.old` change places, so the update can
    /// be put back the same way.
    ///
    /// # Errors
    /// There is no `<name>.old`, or a rename failed (both files are then where they were).
    pub fn swap_with_old(&self) -> Result<(), String> {
        let old = self.old();
        if !old.is_file() {
            return Err(format!("there is no {} to roll back to", old.display()));
        }
        let aside = self.incoming();
        let _stale = remove(&aside);
        rename(&self.exe, &aside)
            .map_err(|e| format!("setting {} aside: {e}", self.exe.display()))?;
        if let Err(e) = rename(&old, &self.exe) {
            let _back = rename(&aside, &self.exe);
            return Err(format!("restoring {}: {e}", old.display()));
        }
        make_runnable(&self.exe);
        if let Err(e) = rename(&aside, &old) {
            return Err(format!(
                "the old binary is back; the one it replaced is {} ({e})",
                aside.display()
            ));
        }
        Ok(())
    }

    /// Removes what earlier swaps left that no program holds any more: `<name>.failed`, the new
    /// file of a swap that did not finish, and old binaries set aside. Best effort.
    pub fn clean_leftovers(&self) {
        let _failed = remove(&self.failed());
        let _incoming = remove(&self.incoming());
        let name =
            self.exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Ok(entries) = std::fs::read_dir(self.folder()) else { return };
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let set_aside = (n.starts_with(&format!("{name}.old."))
                || n.starts_with(&format!("{name}.failed.")))
                && n.rsplit('.').next().is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()));
            if set_aside {
                let _gone = std::fs::remove_file(e.path());
            }
        }
    }
}

/// What a start found of an update (P4.3.8: "the new binary's own start path").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    /// No update in progress.
    Normal,
    /// This binary is the new one of an update, at this start; it is healthy after its first
    /// welcome.
    Updating { from: String, starts: u32 },
    /// The new binary did not become healthy in [`MAX_STARTS`] starts: the old one is back in its
    /// place, and this process exits 75 so its service manager starts it.
    RolledBack(String),
}

/// The start path: reads `pending.json` first thing. This binary the update's new one: its start
/// counted, and on the start after [`MAX_STARTS`] that did not become healthy the old binary is
/// restored (`failed.json` written for it to report). Any other binary: `pending.json` is stale
/// (the old binary is back) and goes.
#[must_use]
pub fn on_start(dir: &UpdateDir, install: Option<&Install>, version: &str) -> Start {
    if let Some(i) = install {
        i.clean_leftovers();
    }
    let Some(mut p) = dir.pending() else { return Start::Normal };
    if p.new != version {
        let _stale = dir.remove_pending();
        return Start::Normal;
    }
    p.starts = p.starts.saturating_add(1);
    if p.starts <= MAX_STARTS {
        let _kept = dir.write_pending(&p);
        return Start::Updating { from: p.old, starts: p.starts };
    }
    let message =
        format!("{} did not become healthy in {MAX_STARTS} starts: {} is back", p.new, p.old);
    let restored = match install {
        Some(i) => i.restore_old(),
        None => Err("this helper does not know its own binary".to_owned()),
    };
    match restored {
        Ok(()) => {
            let _written = dir.write_failed(&p.new, &p.sha256, &message);
            let _gone = dir.remove_pending();
            Start::RolledBack(message)
        }
        Err(e) => {
            // Nothing to go back to: this binary goes on, and the update is over either way.
            let _gone = dir.remove_pending();
            Start::RolledBack(format!("{message} could not be done: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("citar-helper-upd-{name}-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        d
    }

    #[test]
    fn a_swap_keeps_the_old_binary_and_a_rollback_restores_it() {
        let d = scratch("swap");
        let exe = d.join("citar-helper.exe");
        std::fs::write(&exe, b"old").expect("written");
        let staged = d.join("staged");
        std::fs::write(&staged, b"new").expect("written");
        let i = Install::at(&exe);
        let e = i.swap_in(&staged, &"00".repeat(32)).expect_err("not the binary checked");
        assert!(e.contains("changed after it was checked"), "{e}");
        assert_eq!(std::fs::read(&exe).expect("read"), b"old", "nothing installed");
        assert!(!i.old().exists() && !i.incoming().exists());
        let sha = super::super::fetch::sha256_file(&staged).expect("hashed");
        i.swap_in(&staged, &sha).expect("swapped");
        assert_eq!(std::fs::read(&exe).expect("read"), b"new");
        assert_eq!(std::fs::read(i.old()).expect("read"), b"old");
        assert!(!i.incoming().exists());
        i.restore_old().expect("restored");
        assert_eq!(std::fs::read(&exe).expect("read"), b"old");
        assert_eq!(std::fs::read(i.failed()).expect("read"), b"new");
        assert!(!i.old().exists());
        i.clean_leftovers();
        assert!(!i.failed().exists());
        // By hand, both ways.
        std::fs::write(i.old(), b"older").expect("written");
        i.swap_with_old().expect("swapped back");
        assert_eq!(std::fs::read(&exe).expect("read"), b"older");
        assert_eq!(std::fs::read(i.old()).expect("read"), b"old");
        i.swap_with_old().expect("and forward");
        assert_eq!(std::fs::read(&exe).expect("read"), b"old");
        std::fs::remove_file(i.old()).expect("gone");
        assert!(i.restore_old().expect_err("nothing").contains("no "));
        assert!(i.swap_with_old().expect_err("nothing").contains("no "));
        std::fs::remove_dir_all(&d).expect("cleaned");
    }

    #[test]
    fn the_start_path_counts_starts_and_rolls_back_after_three() {
        let d = scratch("starts");
        let exe = d.join("citar-helper");
        std::fs::write(&exe, b"new").expect("written");
        let i = Install::at(&exe);
        std::fs::write(i.old(), b"old").expect("written");
        let u = UpdateDir::new(d.join("update"));
        u.begin("0.1.6-test.1", "0.1.6-test.2", &"ab".repeat(32)).expect("begun");
        for n in 1..=MAX_STARTS {
            assert_eq!(
                on_start(&u, Some(&i), "0.1.6-test.2"),
                Start::Updating { from: "0.1.6-test.1".to_owned(), starts: n }
            );
        }
        let Start::RolledBack(why) = on_start(&u, Some(&i), "0.1.6-test.2") else {
            panic!("rolled back")
        };
        assert!(why.contains("did not become healthy in 3 starts"), "{why}");
        assert_eq!(std::fs::read(&exe).expect("read"), b"old");
        assert!(u.pending().is_none());
        let f = u.failed().expect("failed.json");
        assert_eq!((f.version.as_str(), f.reported), ("0.1.6-test.2", false));
        // The old binary starts normally; a stale pending goes.
        assert_eq!(on_start(&u, Some(&i), "0.1.6-test.1"), Start::Normal);
        u.begin("0.1.6-test.1", "0.1.6-test.2", "x").expect("begun");
        assert_eq!(on_start(&u, Some(&i), "0.1.6-test.1"), Start::Normal);
        assert!(u.pending().is_none());
        // Healthy: the marker written and pending gone.
        u.begin("0.1.6-test.1", "0.1.6-test.2", "x").expect("begun");
        u.mark_healthy("0.1.6-test.2").expect("healthy");
        assert!(u.healthy_exists() && u.pending().is_none());
        u.begin("0.1.6-test.2", "0.1.6-test.3", "x").expect("begun");
        assert!(!u.healthy_exists(), "a hand-off forgets the last update's health");
        std::fs::remove_dir_all(&d).expect("cleaned");
    }

    #[test]
    fn the_update_folder_is_made_owner_only_and_checked() {
        let d = scratch("secure");
        let u = UpdateDir::new(d.join("home").join("helper").join("update"));
        assert_eq!(u.secure(), Ok(()), "made owner-only");
        assert_eq!(citar_jobs::sys::dir_exposure(u.path()).expect("read"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(u.path()).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{mode:o}");
            // Opened to others: refused with the command that fixes it, which works.
            std::fs::set_permissions(u.path(), std::fs::Permissions::from_mode(0o777))
                .expect("chmod");
            let e = u.secure().expect_err("refused");
            assert!(e.contains("its group, others") && e.contains("chmod 700"), "{e}");
            std::fs::set_permissions(u.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod");
            assert_eq!(u.secure(), Ok(()));
        }
        // Its files are written in it as before.
        u.begin("0.1.6-test.1", "0.1.6-test.2", "x").expect("begun");
        assert!(u.pending().is_some());
        std::fs::remove_dir_all(&d).expect("cleaned");
    }

    #[test]
    fn a_build_output_folder_is_recognised() {
        let d = scratch("output");
        let i = Install::at(d.join("citar-helper"));
        assert!(!i.in_build_output());
        std::fs::write(d.join(".cargo-lock"), b"").expect("written");
        assert!(i.in_build_output());
        assert!(i.writable().is_ok());
        std::fs::remove_dir_all(&d).expect("cleaned");
    }
}
