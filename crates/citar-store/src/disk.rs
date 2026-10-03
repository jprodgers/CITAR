//! What the container and the journal share about the file system: the retry on a file another
//! program holds for a moment, the OS locks, temporary names beside a target, and making a new
//! name durable.

use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::StoreError;

/// Windows's `ERROR_SHARING_VIOLATION`: another program has the file open without sharing it.
const SHARING_VIOLATION: i32 = 32;
/// Windows's `ERROR_LOCK_VIOLATION`: another handle holds a lock over the bytes.
const LOCK_VIOLATION: i32 = 33;

/// How often, and how far apart, a write meeting a file another program holds is tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Retry {
    /// Tries in all, the first included.
    pub tries: u32,
    /// The wait before each retry.
    pub wait: Duration,
}

impl Retry {
    /// The first try and eight retries, 250 ms apart: what the Python session's save did for
    /// OneDrive and virus scanners (DESIGN.md P2.5.1).
    pub const DEFAULT: Self = Self { tries: 9, wait: Duration::from_millis(250) };
}

/// Whether `e` is a refusal that passes when the other program lets go: `PermissionDenied`, or
/// on Windows a sharing or lock violation (which std leaves uncategorised, and which Python's
/// `PermissionError` covers along with `ERROR_ACCESS_DENIED`).
pub(crate) fn transient(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(SHARING_VIOLATION | LOCK_VIOLATION)))
}

/// `f`, tried again after `policy.wait` while it fails with a [`transient`] error, at most
/// `policy.tries` times in all. Any other error, or the last one, is returned as it is.
pub(crate) fn retry<T>(policy: Retry, mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut left = policy.tries.max(1);
    loop {
        match f() {
            Err(e) if left > 1 && transient(&e) => {
                left -= 1;
                std::thread::sleep(policy.wait);
            }
            done => return done,
        }
    }
}

/// An I/O error on `path` as a [`StoreError`]: a Windows lock violation is another handle's
/// lock, so [`StoreError::Locked`].
pub(crate) fn io_err(path: &Path, source: io::Error) -> StoreError {
    if cfg!(windows) && source.raw_os_error() == Some(LOCK_VIOLATION) {
        return StoreError::Locked { path: path.to_owned() };
    }
    StoreError::Io { path: path.to_owned(), source }
}

/// Which OS lock to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lock {
    /// The one writer's.
    Exclusive,
    /// A reader's, held while it reads.
    Shared,
}

/// Takes `kind` of lock on `file` without waiting: another holder is [`StoreError::Locked`].
///
/// A file system that has no locks (`Unsupported`; some network shares) is written without one
/// rather than not at all: the server's one session per game keeps a second writer away there,
/// as it did before locks.
pub(crate) fn lock(file: &File, path: &Path, kind: Lock) -> Result<(), StoreError> {
    let got = match kind {
        Lock::Exclusive => file.try_lock(),
        Lock::Shared => file.try_lock_shared(),
    };
    match got {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(StoreError::Locked { path: path.to_owned() }),
        Err(TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(TryLockError::Error(e)) => Err(io_err(path, e)),
    }
}

/// A name for a temporary file beside `path` that no other writer uses: its name, this
/// process's id and a counter, ending in `.tmp` so no listing of `*.citar` or `*.cjnl` sees it.
pub(crate) fn temp_beside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().map_or_else(|| "save".into(), |f| f.to_string_lossy());
    path.with_file_name(format!("{name}.{}.{n}.tmp", std::process::id()))
}

/// Makes a new name in `path`'s folder durable: on Unix the folder itself is synced, since a
/// rename or a new file is an entry in it. Windows has no such call (NTFS journals its
/// metadata).
///
/// Best effort: some file systems refuse to sync a folder, and the file's own bytes were synced
/// already, so a refusal is not worth failing a save that is otherwise complete.
pub(crate) fn sync_parent(path: &Path) {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        if let Ok(d) = File::open(dir) {
            let _ignored = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Removes a file it is given on drop, unless [`keep`](Self::keep) was called: a temporary file
/// or a partial fork, when a write fails part way.
pub(crate) struct Cleanup<'a> {
    path: Option<&'a Path>,
}

impl<'a> Cleanup<'a> {
    pub(crate) const fn new(path: &'a Path) -> Self {
        Self { path: Some(path) }
    }

    /// The file is wanted: leave it.
    pub(crate) fn keep(mut self) {
        self.path = None;
    }
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.path {
            // Best effort: the write already failed, and that error is what the caller sees.
            let _ignored = std::fs::remove_file(p);
        }
    }
}

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

/// Whether `s` is exactly `len` lower-case hex digits.
pub(crate) fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const FAST: Retry = Retry { tries: 9, wait: Duration::from_millis(1) };

    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    #[test]
    fn a_held_file_is_tried_again_until_it_is_let_go() {
        let calls = Cell::new(0);
        let got = retry(FAST, || {
            calls.set(calls.get() + 1);
            if calls.get() < 5 { Err(denied()) } else { Ok(7) }
        });
        assert_eq!(got.ok(), Some(7));
        assert_eq!(calls.get(), 5);
    }

    #[test]
    fn the_retries_end_after_eight() {
        let calls = Cell::new(0);
        let got: io::Result<()> = retry(FAST, || {
            calls.set(calls.get() + 1);
            Err(denied())
        });
        assert_eq!(got.map_err(|e| e.kind()), Err(io::ErrorKind::PermissionDenied));
        assert_eq!(calls.get(), 9, "the first try and eight retries");
        assert_eq!(Retry::DEFAULT.tries, 9);
        assert_eq!(Retry::DEFAULT.wait, Duration::from_millis(250));
    }

    #[test]
    fn other_errors_are_not_tried_again() {
        let calls = Cell::new(0);
        let got: io::Result<()> = retry(FAST, || {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert!(got.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn windows_sharing_and_lock_violations_are_transient_there() {
        for code in [SHARING_VIOLATION, LOCK_VIOLATION] {
            assert_eq!(transient(&io::Error::from_raw_os_error(code)), cfg!(windows));
        }
        assert!(transient(&denied()));
        assert!(!transient(&io::Error::from(io::ErrorKind::StorageFull)));
        let locked = io_err(Path::new("j"), io::Error::from_raw_os_error(LOCK_VIOLATION));
        assert_eq!(matches!(locked, StoreError::Locked { .. }), cfg!(windows));
    }

    #[test]
    fn temporary_names_are_unique_and_never_a_save() {
        let p = Path::new("saves").join("g1").join("autosave.citar");
        let (a, b) = (temp_beside(&p), temp_beside(&p));
        assert_ne!(a, b);
        assert_eq!(a.parent(), p.parent());
        let name = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        assert!(name.starts_with("autosave.citar.") && name.ends_with(".tmp"), "{name}");
    }

    #[test]
    fn hex_round_trips_its_digits() {
        assert_eq!(hex(&[0x00, 0x9f, 0xa0, 0xff]), "009fa0ff");
        assert!(is_hex("009fa0ff", 8));
        assert!(!is_hex("009FA0FF", 8), "lower case only");
        assert!(!is_hex("009fa0f", 8));
        assert!(!is_hex("009fa0fg", 8));
    }
}
