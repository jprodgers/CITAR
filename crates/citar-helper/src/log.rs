//! The helper's log (design/phase4-helper.md P4.3.2): lines to standard error and to
//! `helper/logs/helper.log` in its home, rotated at 5 MB with three old files kept, and every
//! secret it was told of (the worker token, as protocol 1 redacted it) replaced before a line is
//! written anywhere. Tests keep the lines in memory to read them back.
//!
//! A handle, not a global logger, so that tests can run several helpers in one process, each with
//! its own lines.

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
    /// When each throttled key last logged.
    last: BTreeMap<String, Instant>,
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

    /// Writes a line at `level`.
    pub fn line(&self, level: Level, msg: impl AsRef<str>) {
        let mut g = self.lock();
        if level == Level::Debug && !g.verbose {
            return;
        }
        let mut text = msg.as_ref().to_owned();
        for s in &g.secrets {
            if text.contains(s.as_str()) {
                text = text.replace(s.as_str(), REDACTED);
            }
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
    /// the same bad frame a thousand times fills no disk.
    pub fn throttled(&self, key: &str, every: Duration, level: Level, msg: impl AsRef<str>) {
        {
            let mut g = self.lock();
            let now = Instant::now();
            if let Some(t) = g.last.get(key)
                && now.duration_since(*t) < every
            {
                return;
            }
            g.last.insert(key.to_owned(), now);
        }
        self.line(level, msg);
    }

    /// Writes a line the first time `key` is seen, never again.
    pub fn once(&self, key: &str, level: Level, msg: impl AsRef<str>) {
        {
            let mut g = self.lock();
            if g.last.contains_key(key) {
                return;
            }
            g.last.insert(key.to_owned(), Instant::now());
        }
        self.line(level, msg);
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
    size: u64,
}

impl Rotating {
    fn open(dir: &Path) -> std::io::Result<Rotating> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("helper.log");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Rotating { path, file, size })
    }

    fn rotated(&self, n: u32) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn write_line(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.size > 0 && self.size + bytes > ROTATE_BYTES {
            self.rotate();
        }
        // A log that cannot be written must not stop the helper: the line still went to stderr.
        if writeln!(self.file, "{line}").is_ok() {
            self.size += bytes;
        }
    }

    fn rotate(&mut self) {
        let _gone = std::fs::remove_file(self.rotated(KEEP));
        for n in (1..KEEP).rev() {
            let _moved = std::fs::rename(self.rotated(n), self.rotated(n + 1));
        }
        let _moved = std::fs::rename(&self.path, self.rotated(1));
        if let Ok(f) = OpenOptions::new().create(true).append(true).truncate(false).open(&self.path)
        {
            self.file = f;
            self.size = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_file_rotates_and_keeps_three() {
        let dir = std::env::temp_dir().join(format!("citar-helper-log-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        let log = Log::default();
        log.to_file(&dir).expect("a log file");
        let big = "x".repeat(1024 * 1024);
        for _ in 0..30 {
            log.info(&big);
        }
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("listed")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        let mut names = names;
        names.sort();
        assert_eq!(names, ["helper.log", "helper.log.1", "helper.log.2", "helper.log.3"]);
        for n in &names {
            let size = std::fs::metadata(dir.join(n)).expect("stat").len();
            assert!(size <= ROTATE_BYTES, "{n}: {size}");
        }
        drop(log);
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
