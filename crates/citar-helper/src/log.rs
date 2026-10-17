//! The helper's log (design/phase4-helper.md P4.3.2): lines to standard error and to
//! `helper/logs/helper.log` in its home, rotated at 5 MB with three old files kept, and every
//! secret it was told of (the worker token, as protocol 1 redacted it) replaced before a line is
//! written anywhere. Tests keep the lines in memory to read them back.
//!
//! A handle, not a global logger, so that tests can run several helpers in one process, each with
//! its own lines.
//!
//! **Bounded whatever a peer sends** (P4.6, a server against the lender's machine): text from
//! elsewhere is quoted through [`clip`], every line is cut at [`MAX_LINE`] characters, the keys
//! that throttle repeated lines and those that log a line once are each at most [`MAX_KEYS`]
//! (past it, new keys share one; the two are kept apart, so a flood of names logged once never
//! silences a throttled warning), and a line below the log's level records no key at all. A log file that cannot be rotated (another program
//! holds it open) grows to [`HELD_BYTES`] at most, then lines go to standard error only until a
//! rotation, tried again every minute, succeeds.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A log file grows to this before it is rotated.
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated files kept: `helper.log.1` (the newest) to `helper.log.3`.
pub const KEEP: u32 = 3;
/// What stands in for a secret.
pub const REDACTED: &str = "[redacted]";
/// The most characters of a line's message (after its secrets are replaced): a backstop, since
/// text from elsewhere is cut far shorter where it is quoted.
pub const MAX_LINE: usize = 4096;
/// The most keys [`Log::throttled`], and apart from them [`Log::once`], remember; once they are
/// reached, every new key shares one more, so a peer that invents names cannot grow either map.
pub const MAX_KEYS: usize = 256;
/// The longest key remembered, in characters.
const MAX_KEY: usize = 128;
/// The key every new key shares once [`MAX_KEYS`] are remembered.
const OVERFLOW: &str = "\u{1}more keys";
/// How far `helper.log` may grow while it cannot be rotated; past it, lines go to standard error
/// only.
pub const HELD_BYTES: u64 = 2 * ROTATE_BYTES;
/// How long after a failed rotation the next is tried.
const RETRY_ROTATION: Duration = Duration::from_secs(60);

/// Text from elsewhere (a server's frame type or message, an endpoint's answer) made fit to quote
/// in a line: at most `max` characters, control characters escaped (so it cannot start a line of
/// its own), and `…` where it was cut. It reads at most `max + 1` characters however long the
/// text is.
#[must_use]
pub fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == max {
            out.push('\u{2026}');
            break;
        }
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// How much a line matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

#[derive(Default)]
struct Inner {
    secrets: Vec<String>,
    verbose: bool,
    stderr: bool,
    file: Option<Rotating>,
    memory: Option<Vec<String>>,
    /// When each throttled key last logged: at most [`MAX_KEYS`] and the shared one.
    last: BTreeMap<String, Instant>,
    /// The keys logged once: at most [`MAX_KEYS`] and the shared one.
    seen: BTreeMap<String, ()>,
}

impl Inner {
    fn writes(&self, level: Level) -> bool {
        level != Level::Debug || self.verbose
    }
}

/// The key a line is remembered by in `map`: `key` itself (cut to [`MAX_KEY`]) while there is
/// room or it is already there, else the one every further key shares.
fn key_in<V>(map: &BTreeMap<String, V>, key: &str) -> String {
    let k = clip(key, MAX_KEY);
    if map.len() < MAX_KEYS || map.contains_key(&k) { k } else { OVERFLOW.to_owned() }
}

/// A log: cheap to clone, shared by everything one helper runs.
#[derive(Clone, Default)]
pub struct Log {
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Log")
    }
}

impl Log {
    /// A log to standard error, at debug level when `verbose`.
    #[must_use]
    pub fn stderr(verbose: bool) -> Log {
        let log = Log::default();
        {
            let mut g = log.lock();
            g.stderr = true;
            g.verbose = verbose;
        }
        log
    }

    /// A log kept in memory, every level, for tests.
    #[must_use]
    pub fn memory() -> Log {
        let log = Log::default();
        {
            let mut g = log.lock();
            g.memory = Some(Vec::new());
            g.verbose = true;
        }
        log
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while logging leaves nothing half-written that matters: keep logging.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Also writes to `helper.log` in `dir`, which it creates.
    ///
    /// # Errors
    /// The directory or the file cannot be made.
    pub fn to_file(&self, dir: &Path) -> std::io::Result<()> {
        let file = Rotating::open(dir)?;
        self.lock().file = Some(file);
        Ok(())
    }

    /// Never writes `secret` again: every line has it replaced. Short strings are not secrets
    /// worth hiding and would mangle ordinary words.
    pub fn add_secret(&self, secret: &str) {
        if secret.len() >= 4 {
            let mut g = self.lock();
            if !g.secrets.iter().any(|s| s == secret) {
                g.secrets.push(secret.to_owned());
                // The longest first, so one secret inside another is hidden whole.
                g.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
            }
        }
    }

    /// `text` with every secret replaced, as a line would have it: for text that leaves the
    /// machine (an endpoint's error, quoted to the server, could echo the API key it was sent).
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let g = self.lock();
        let mut out = text.to_owned();
        for s in &g.secrets {
            if out.contains(s.as_str()) {
                out = out.replace(s.as_str(), REDACTED);
            }
        }
        out
    }

    /// Writes a line at `level`, its message cut at [`MAX_LINE`] characters.
    pub fn line(&self, level: Level, msg: impl AsRef<str>) {
        let mut g = self.lock();
        if !g.writes(level) {
            return;
        }
        let mut text = msg.as_ref().to_owned();
        for s in &g.secrets {
            if text.contains(s.as_str()) {
                text = text.replace(s.as_str(), REDACTED);
            }
        }
        // Cut after the secrets are replaced, so no part of one can show.
        if let Some((at, _)) = text.char_indices().nth(MAX_LINE) {
            text.truncate(at);
            text.push_str(" \u{2026} (cut)");
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let full = format!("{stamp} {:<7} {text}", level.tag());
        if g.stderr {
            eprintln!("{full}");
        }
        if let Some(f) = g.file.as_mut() {
            f.write_line(&full);
        }
        if let Some(m) = g.memory.as_mut() {
            m.push(full);
        }
    }

    /// Writes a line at `level` unless the same `key` logged within `every`: a peer that sends
    /// the same bad frame a thousand times fills no disk. A line below the log's level records
    /// nothing.
    pub fn throttled(&self, key: &str, every: Duration, level: Level, msg: impl AsRef<str>) {
        {
            let mut g = self.lock();
            if !g.writes(level) {
                return;
            }
            let now = Instant::now();
            let key = key_in(&g.last, key);
            if let Some(t) = g.last.get(&key)
                && now.duration_since(*t) < every
            {
                return;
            }
            g.last.insert(key, now);
        }
        self.line(level, msg);
    }

    /// Writes a line the first time `key` is seen, never again. A line below the log's level
    /// records nothing.
    pub fn once(&self, key: &str, level: Level, msg: impl AsRef<str>) {
        {
            let mut g = self.lock();
            if !g.writes(level) {
                return;
            }
            let key = key_in(&g.seen, key);
            if g.seen.contains_key(&key) {
                return;
            }
            g.seen.insert(key, ());
        }
        self.line(level, msg);
    }

    /// How many keys the log remembers, throttled and logged once together: at most
    /// [`MAX_KEYS`] and one more of each.
    #[must_use]
    pub fn keys(&self) -> usize {
        let g = self.lock();
        g.last.len() + g.seen.len()
    }

    pub fn debug(&self, msg: impl AsRef<str>) {
        self.line(Level::Debug, msg);
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.line(Level::Info, msg);
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        self.line(Level::Warn, msg);
    }

    pub fn error(&self, msg: impl AsRef<str>) {
        self.line(Level::Error, msg);
    }

    /// The lines kept in memory (a [`Log::memory`] log), oldest first.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lock().memory.clone().unwrap_or_default()
    }

    /// Whether a line kept in memory contains `text`.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        self.lock().memory.as_ref().is_some_and(|m| m.iter().any(|l| l.contains(text)))
    }
}

/// `helper.log`, rotated by size.
struct Rotating {
    path: PathBuf,
    file: File,
    /// The file's size as far as this process knows: read from the file at each open.
    size: u64,
    /// After a rotation failed, when to try again.
    retry_at: Option<Instant>,
    retry_every: Duration,
    /// The file reached [`HELD_BYTES`] while it could not be rotated: nothing more is written to
    /// it (the line that says so is its last) until a rotation succeeds.
    full: bool,
}

impl Rotating {
    fn open(dir: &Path) -> std::io::Result<Rotating> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("helper.log");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Rotating { path, file, size, retry_at: None, retry_every: RETRY_ROTATION, full: false })
    }

    fn rotated(&self, n: u32) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn write_line(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.size > 0
            && self.size + bytes > ROTATE_BYTES
            && self.retry_at.is_none_or(|t| Instant::now() >= t)
        {
            self.rotate();
        }
        if self.size + bytes > HELD_BYTES {
            if !self.full {
                self.full = true;
                let note = format!(
                    "{} cannot be rotated (another program may hold it open): the helper logs to \
                     standard error only until it can",
                    self.path.display()
                );
                eprintln!("{note}");
                let _noted = writeln!(self.file, "{note}");
            }
            return;
        }
        // A log that cannot be written must not stop the helper: the line still went to stderr.
        if writeln!(self.file, "{line}").is_ok() {
            self.size += bytes;
        }
    }

    /// `helper.log` becomes `helper.log.1`, the older ones move up one and the oldest goes. The
    /// file is moved aside first, so when that fails (on Windows, another program holding it
    /// open without delete sharing) no older log is touched, and the next try is a minute on.
    fn rotate(&mut self) {
        let aside = self.rotated(0);
        if std::fs::rename(&self.path, &aside).is_err() {
            self.retry_at = Some(Instant::now() + self.retry_every);
            return;
        }
        let _gone = std::fs::remove_file(self.rotated(KEEP));
        for n in (1..KEEP).rev() {
            let _moved = std::fs::rename(self.rotated(n), self.rotated(n + 1));
        }
        let _moved = std::fs::rename(&aside, self.rotated(1));
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => {
                self.size = f.metadata().map_or(0, |m| m.len());
                self.file = f;
                self.retry_at = None;
                self.full = false;
            }
            // Still writing to the file moved aside, up to the same bound; try again later.
            Err(_) => self.retry_at = Some(Instant::now() + self.retry_every),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("citar-helper-log-{name}-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("listed")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn size(p: &Path) -> u64 {
        std::fs::metadata(p).expect("stat").len()
    }

    #[test]
    fn text_from_elsewhere_is_clipped_and_lines_and_keys_stay_bounded() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 3), "abc\u{2026}");
        assert_eq!(clip("a\nb\u{1b}c", 10), "a\\nb\\u{1b}c");
        let log = Log::memory();
        log.add_secret("cwt_secret_token_value");
        let long = format!("cwt_secret_token_value {}", "y".repeat(9_000_000));
        log.warn(&long);
        let line = log.lines().pop().expect("a line");
        assert!(line.len() < MAX_LINE + 100, "{} bytes", line.len());
        assert!(line.contains("[redacted] yyy") && line.ends_with("(cut)"), "{}", &line[..80]);
        for i in 0..1000 {
            log.once(&format!("type {i} {}", "z".repeat(1000)), Level::Debug, format!("type {i}"));
            log.throttled(&format!("bad {i}"), Duration::from_secs(60), Level::Warn, "bad");
        }
        assert_eq!(log.keys(), 2 * (MAX_KEYS + 1), "each kind stops at the cap and one shared");
        // A flood of names logged once leaves a throttled warning its own key.
        let flooded = Log::memory();
        for i in 0..1000 {
            flooded.once(&format!("ignored {i}"), Level::Debug, "ignored");
        }
        flooded.throttled("bad ?", Duration::from_secs(60), Level::Warn, "still warned");
        assert!(flooded.contains("still warned"));
        // A line below the level records no key at all.
        let quiet = Log::default();
        for i in 0..10 {
            quiet.once(&format!("k{i}"), Level::Debug, "never written");
            quiet.throttled(&format!("t{i}"), Duration::from_secs(1), Level::Debug, "never");
        }
        assert_eq!(quiet.keys(), 0);
    }

    #[test]
    fn the_file_rotates_and_keeps_three() {
        let dir = scratch("rotate");
        let mut f = Rotating::open(&dir).expect("a log file");
        let big = "x".repeat(1024 * 1024);
        for _ in 0..30 {
            f.write_line(&big);
        }
        assert_eq!(names(&dir), ["helper.log", "helper.log.1", "helper.log.2", "helper.log.3"]);
        for n in names(&dir) {
            assert!(size(&dir.join(&n)) <= ROTATE_BYTES, "{n}");
        }
        drop(f);
        // Reopened, the size is the file's own.
        let f = Rotating::open(&dir).expect("reopened");
        assert_eq!(f.size, size(&dir.join("helper.log")));
        drop(f);
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// Writes 30 lines of 1 MiB while the log cannot be moved aside, then checks the bound: no
    /// older log was touched, the file stopped at [`HELD_BYTES`], and once the hold is gone the
    /// next rotation succeeds and writing resumes.
    fn held_then_released(dir: &Path, release: impl FnOnce()) {
        let mut f = Rotating::open(dir).expect("a log file");
        f.write_line("first");
        let before = names(dir);
        let big = "x".repeat(1024 * 1024);
        for _ in 0..30 {
            f.write_line(&big);
        }
        let log = dir.join("helper.log");
        assert!(size(&log) <= HELD_BYTES + 4096, "{} bytes", size(&log));
        assert!(size(&log) > ROTATE_BYTES, "it went on past the rotation size first");
        assert_eq!(names(dir), before, "no older log was shifted or deleted");
        assert!(f.full && f.retry_at.is_some());
        release();
        f.retry_every = Duration::ZERO;
        f.retry_at = Some(Instant::now());
        f.write_line("after");
        assert!(!f.full);
        assert!(names(dir).contains(&"helper.log.1".to_owned()), "{:?}", names(dir));
        assert_eq!(std::fs::read_to_string(&log).expect("read"), "after\n");
        drop(f);
    }

    #[cfg(windows)]
    #[test]
    fn a_log_another_program_holds_grows_to_its_bound_and_rotates_once_released() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let dir = scratch("held");
        drop(Rotating::open(&dir).expect("made"));
        // Another program's handle: read and write sharing, no delete sharing (as an editor or
        // PowerShell's Get-Content -Wait holds it), so the file cannot be renamed.
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        let holder = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(dir.join("helper.log"))
            .expect("held");
        held_then_released(&dir, move || drop(holder));
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[cfg(unix)]
    #[test]
    fn a_log_that_cannot_be_renamed_grows_to_its_bound_and_rotates_once_it_can() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("held");
        drop(Rotating::open(&dir).expect("made"));
        // A directory nobody may write: the log is still appended to, but not renamed (unless
        // the test runs as root, which renames anyway: then there is nothing to check).
        let probe = dir.join("probe");
        std::fs::write(&probe, "p").expect("probe");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        if std::fs::rename(&probe, dir.join("probe2")).is_ok() {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            std::fs::remove_dir_all(&dir).expect("cleaned");
            return;
        }
        let d = dir.clone();
        held_then_released(&dir, move || {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        });
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn secrets_never_reach_a_line() {
        let log = Log::memory();
        log.add_secret("cwt_secret_token_value");
        log.add_secret("abc"); // too short to be one
        log.info("hello with cwt_secret_token_value inside, twice: cwt_secret_token_value");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("cwt_secret"), "{lines:?}");
        assert!(lines[0].contains("[redacted] inside, twice: [redacted]"), "{lines:?}");
        log.info("abc stays");
        assert!(log.contains("abc stays"));
    }

    #[test]
    fn throttled_and_once_lines_repeat_as_told() {
        let log = Log::memory();
        for _ in 0..5 {
            log.throttled("k", Duration::from_secs(60), Level::Warn, "bad frame");
            log.once("o", Level::Info, "first time");
        }
        assert_eq!(log.lines().iter().filter(|l| l.contains("bad frame")).count(), 1);
        assert_eq!(log.lines().iter().filter(|l| l.contains("first time")).count(), 1);
        log.throttled("k2", Duration::ZERO, Level::Warn, "every time");
        log.throttled("k2", Duration::ZERO, Level::Warn, "every time");
        assert_eq!(log.lines().iter().filter(|l| l.contains("every time")).count(), 2);
    }
}
