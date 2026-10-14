//! The operating-system calls of Phase 4 that no safe wrapper makes (design/phase4-helper.md
//! P4.3.1), behind safe functions: the only module of the helper crates allowed `unsafe`, in its
//! two per-OS files. In 4-03 it holds the machine key's file permissions (P4.3.7); the job host's
//! limits (Job Objects, `setrlimit`, `setpriority`) join it in 4-08.
//!
//! **Owner-only files.** The helper's machine key (and, in 4-14, the Windows service's home) must
//! be readable by the account that runs the helper and nobody else:
//! - on Unix, mode 0600, and a file whose mode gives the group or others anything is refused, as
//!   SSH refuses a private key;
//! - on Windows, a protected DACL (no inheritance) granting full control to the current user,
//!   SYSTEM and the Administrators group, and nobody else anything. A file with an allow entry for
//!   any other principal is refused: a `--home` such as `C:\citar` inherits Authenticated Users'
//!   modify rights, and anything under `%ProgramData%` inherits Users' read. The DACL is read
//!   through the security API, never through `icacls`, whose output is localised; `icacls` is
//!   only what [`Exposure::fix`] tells a person to run.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use self::windows::{Ace, AceKind, Dacl, current_user_sid, dacl};

/// Who besides the file's owner can reach a file that should be owner-only, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exposure {
    /// Each principal that can: `group` and `others` on Unix; on Windows a SID, with its usual
    /// name when it is a well-known one (`S-1-5-32-545 (Users)`).
    pub who: Vec<String>,
    /// The command that makes the file owner-only again, for the person to run.
    pub fix: String,
}

impl Exposure {
    /// One line naming who can read the file and the command that fixes it.
    #[must_use]
    pub fn describe(&self, path: &Path) -> String {
        format!(
            "{} can be read by {} besides its owner; make it owner-only with: {}",
            path.display(),
            self.who.join(", "),
            self.fix
        )
    }
}

/// Makes `path` the owner's alone: mode 0600 on Unix; on Windows a protected DACL granting full
/// control to the current user, SYSTEM and Administrators, and nothing to anyone else.
///
/// # Errors
/// The OS refused (the file is missing, or the caller may not change its permissions).
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::restrict_to_owner(path)
    }
    #[cfg(windows)]
    {
        windows::restrict_to_owner(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "no owner-only files on this OS"))
    }
}

/// Who else can read `path`, or `None` when only its owner can.
///
/// # Errors
/// The OS could not say (the file is missing, or its permissions cannot be read).
pub fn exposure(path: &Path) -> io::Result<Option<Exposure>> {
    #[cfg(unix)]
    {
        unix::exposure(path)
    }
    #[cfg(windows)]
    {
        windows::exposure(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "no owner-only files on this OS"))
    }
}

/// `path` quoted for a POSIX shell: single quotes, with each single quote spelled `'\''`.
#[must_use]
pub fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("citar-jobs-sys-{}-{name}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn a_restricted_file_is_owner_only() {
        let dir = scratch("restrict");
        let file = dir.join("secret.json");
        std::fs::write(&file, "{}").expect("written");
        restrict_to_owner(&file).expect("restricted");
        assert_eq!(exposure(&file).expect("read back"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        #[cfg(windows)]
        {
            let d = dacl(&file).expect("the DACL");
            assert!(d.protected, "{d:?}");
            let aces = d.aces.expect("a DACL is present");
            assert!(aces.iter().all(|a| !a.inherited), "{aces:?}");
            let me = current_user_sid().expect("the user");
            let sids: Vec<&str> = aces.iter().filter_map(|a| a.sid.as_deref()).collect();
            assert!(sids.contains(&me.as_str()), "{sids:?}");
            for s in &sids {
                assert!([me.as_str(), "S-1-5-18", "S-1-5-32-544"].contains(s), "{s} in {sids:?}");
            }
        }
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn shell_quoting_survives_a_quote() {
        assert_eq!(shell_quote(Path::new("/a b/it's")), r"'/a b/it'\''s'");
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_pass() {
        let dir = scratch("missing");
        assert!(exposure(&dir.join("absent")).is_err());
        assert!(restrict_to_owner(&dir.join("absent")).is_err());
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
