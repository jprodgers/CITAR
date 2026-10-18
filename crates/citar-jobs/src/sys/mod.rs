//! The operating-system calls of Phase 4 that no safe wrapper makes (design/phase4-helper.md
//! P4.3.1), behind safe functions: the only module of the helper crates allowed `unsafe`, in its
//! two per-OS files and [`alloc`]. It holds the machine key's file permissions (P4.3.7, 4-03), the
//! job host's limits (P4.3.6, 4-08: a Job Object and the process's committed memory on Windows;
//! `setpriority` and `RLIMIT_DATA` on Linux; the background band and the resident size on macOS),
//! and the job host's counting allocator ([`alloc`], P4.4.3), whose `GlobalAlloc` implementation
//! is unsafe by the trait's definition; on Unix also `SIGTERM` to a process (`terminate`, the
//! helper's console supervisor passing a stop on to its child, P4.3.8, 4-13). [`crate::limits`] decides what to hold and calls these.
//! And the helper's services (P4.3.9, 4-14): whether this process is elevated or root
//! ([`is_elevated`], which an unelevated `service install --system` and the tests that could install
//! anything check), the user's id on Unix (`user_id`: launchd's `gui/<uid>` domain), and on Windows
//! the service home's DACL (`restrict_tree_to`, each object checked and set through its own
//! handle), the folders above it no other account may take over (`check_ancestors`), a folder made
//! with its DACL from its first moment (`create_dir_with`), and the service manager's entry
//! (`run_service`).
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
//!
//! **Owner-only folders** ([`restrict_dir_to_owner`], [`dir_exposure`]): the helper's update
//! folder (P4.3.8, 4-13's fix round), where a downloaded binary waits to be self-tested and
//! installed, must be writable by nobody else, or another local user could swap that binary
//! between its checks and its run: mode 0700 on Unix; on Windows the same protected DACL as a
//! file's, its entries inherited by everything made in the folder.

use std::io;
use std::path::Path;

#[allow(unsafe_code)]
pub mod alloc;
#[cfg(unix)]
#[allow(unsafe_code)]
mod unix;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(target_os = "macos")]
pub use self::unix::{background, resident_bytes};
#[cfg(all(unix, not(target_os = "macos")))]
pub use self::unix::{limit_data, nice};
#[cfg(unix)]
pub use self::unix::{terminate, user_id};
#[cfg(windows)]
pub use self::windows::{
    Ace, AceKind, Dacl, JobObject, TAKES_OVER, TRUSTED_INSTALLER, check_ancestor, check_ancestors,
    committed_bytes, create_dir_with, current_user_sid, dacl, keep_std_handles_to_self,
    restrict_tree_to, run_service, takeover,
};

/// Whether this process may install a system service: elevated on Windows (an administrator's
/// "Run as administrator"), root on Unix.
///
/// # Errors
/// The OS could not say (Windows: the token could not be read).
pub fn is_elevated() -> io::Result<bool> {
    #[cfg(unix)]
    {
        Ok(unix::is_root())
    }
    #[cfg(windows)]
    {
        windows::is_elevated()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no elevation on this OS"))
    }
}

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

    /// One line naming who can reach the folder and the command that fixes it.
    #[must_use]
    pub fn describe_folder(&self, path: &Path) -> String {
        format!(
            "the folder {} can be reached by {} besides its owner; make it owner-only with: {}",
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

/// Makes the folder `path` the owner's alone, and what is made in it from then on: mode 0700 on
/// Unix; on Windows a protected DACL granting full control to the current user, SYSTEM and
/// Administrators, inherited by the files and folders in it, and nothing to anyone else.
///
/// # Errors
/// The OS refused (the folder is missing, `path` is not a folder, or the caller may not change
/// its permissions).
pub fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::restrict_dir_to_owner(path)
    }
    #[cfg(windows)]
    {
        windows::restrict_dir_to_owner(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "no owner-only folders on this OS"))
    }
}

/// Who else can reach the folder `path` (list it, or add, replace or remove what it holds), or
/// `None` when only its owner can; [`Exposure::fix`] is the folder's command.
///
/// # Errors
/// The OS could not say (the folder is missing, or its permissions cannot be read).
pub fn dir_exposure(path: &Path) -> io::Result<Option<Exposure>> {
    #[cfg(unix)]
    {
        unix::dir_exposure(path)
    }
    #[cfg(windows)]
    {
        windows::dir_exposure(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "no owner-only folders on this OS"))
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
    fn a_restricted_folder_is_owner_only_and_so_is_what_is_made_in_it() {
        let dir = scratch("restrict-dir");
        let folder = dir.join("update");
        std::fs::create_dir_all(&folder).expect("made");
        std::fs::write(folder.join("before.bin"), b"x").expect("written");
        restrict_dir_to_owner(&folder).expect("restricted");
        assert_eq!(dir_exposure(&folder).expect("read back"), None);
        let after = folder.join("after.bin");
        std::fs::write(&after, b"y").expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&folder).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        #[cfg(windows)]
        {
            // What the folder holds, old and new, inherits its owner-only entries.
            assert_eq!(exposure(&after).expect("read"), None);
            assert_eq!(exposure(&folder.join("before.bin")).expect("read"), None);
            let d = dacl(&folder).expect("the DACL");
            assert!(d.protected, "{d:?}");
        }
        assert!(restrict_dir_to_owner(&after).is_err(), "a file is not a folder");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn this_test_process_is_not_elevated() {
        // P4.7.1: nothing here runs elevated; were it, the tests that could install a service
        // would refuse to run. On CI's runners (administrators, root in containers) this is only
        // read, never asserted.
        let elevated = is_elevated().expect("the OS says");
        if std::env::var_os("CI").is_none() {
            assert!(!elevated, "the test process is elevated");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_restricted_tree_grants_its_principals_alone_on_everything_under_it() {
        let dir = scratch("tree");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("helper").join("logs")).expect("made");
        std::fs::write(home.join("helper.json"), "{}").expect("written");
        std::fs::write(home.join("helper").join("logs").join("helper.log"), "x").expect("written");
        // owner-only for this user first, as `citar-helper setup` writes helper.json
        restrict_to_owner(&home.join("helper.json")).expect("restricted");
        let me = current_user_sid().expect("the user");
        // This user stands in for the service's account (an unelevated test cannot give the
        // objects to Administrators, nor lock itself out of its own scratch folder).
        let principals = ["S-1-5-18", "S-1-5-32-544", me.as_str()];
        // Another owner anywhere refuses the whole call, and nothing is changed.
        let refused =
            restrict_tree_to(&home, None, &principals, &["S-1-5-18"]).expect_err("not ours");
        assert!(refused.to_string().contains("owned by"), "{refused}");
        assert!(!dacl(&home).expect("the DACL").protected, "nothing changed");
        let set = restrict_tree_to(&home, None, &principals, &ours(&me)).expect("set");
        assert_eq!(set, 5, "the home, helper/, logs/, helper.json and helper.log");
        for p in [
            home.clone(),
            home.join("helper.json"),
            home.join("helper").join("logs").join("helper.log"),
        ] {
            let d = dacl(&p).expect("the DACL");
            assert!(d.protected, "{} {d:?}", p.display());
            let aces = d.aces.expect("a DACL");
            assert!(aces.iter().all(|a| !a.inherited), "{} {aces:?}", p.display());
            let mut sids: Vec<String> = aces.iter().filter_map(|a| a.sid.clone()).collect();
            sids.sort();
            let mut want = vec!["S-1-5-18".to_owned(), "S-1-5-32-544".to_owned(), me.clone()];
            want.sort();
            assert_eq!(sids, want, "{}", p.display());
        }
        // What is made there later inherits the folder's entries.
        std::fs::write(home.join("later.json"), "{}").expect("written");
        assert_eq!(exposure(&home.join("later.json")).expect("read"), None);
        assert!(
            restrict_tree_to(&home.join("helper.json"), None, &["S-1-5-18"], &ours(&me)).is_err()
        );
        restrict_dir_to_owner(&home).expect("given back");
        // A junction in the tree, or as the folder itself, refuses the call (junctions need no
        // privilege to make, unlike symbolic links).
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("made");
        let junction = |link: &Path| {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(&elsewhere)
                .output()
                .expect("mklink");
            assert!(made.status.success(), "{made:?}");
        };
        junction(&home.join("helper").join("planted"));
        let e =
            restrict_tree_to(&home, None, &principals, &ours(&me)).expect_err("a junction in it");
        assert!(e.to_string().contains("a link or junction"), "{e}");
        std::fs::remove_dir(home.join("helper").join("planted")).expect("the junction removed");
        std::fs::create_dir_all(elsewhere.join("sub")).expect("made");
        let root = dir.join("junction-home");
        junction(&root);
        let e =
            restrict_tree_to(&root, None, &principals, &ours(&me)).expect_err("a junction itself");
        assert!(e.to_string().contains("a link or junction"), "{e}");
        let e = restrict_tree_to(&root.join("sub"), None, &principals, &ours(&me))
            .expect_err("under one");
        assert!(
            e.to_string().contains("a link or junction"),
            "a folder reached through a junction: {e}"
        );
        std::fs::remove_dir(&root).expect("the junction removed");
        assert!(
            dacl(&elsewhere).expect("the DACL").aces.is_some_and(|a| a.iter().any(|x| x.inherited)),
            "untouched"
        );
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// A tree under `dir`: `home/helper.json` and `home/helper/logs/helper.log`, and a file outside
    /// it (`outside.json`) that inherits its folder's entries, as a file elsewhere would.
    #[cfg(windows)]
    fn tree(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("helper").join("logs")).expect("made");
        std::fs::write(home.join("helper.json"), "{}").expect("written");
        std::fs::write(home.join("helper").join("logs").join("helper.log"), "x").expect("written");
        let outside = dir.join("outside.json");
        std::fs::write(&outside, "{}").expect("written");
        (home, outside)
    }

    /// 4-14's review: a file with two names (a hard link) is refused before anything changes, as a
    /// link or junction is: its other name is elsewhere, where its access list would change too.
    #[cfg(windows)]
    #[test]
    fn a_hard_link_in_the_tree_is_refused_before_anything_changes() {
        let dir = scratch("tree-hardlink");
        let (home, outside) = tree(&dir);
        std::fs::hard_link(&outside, home.join("helper").join("linked.json")).expect("a hard link");
        let me = current_user_sid().expect("the user");
        let before = dacl(&outside).expect("the DACL");
        let e = restrict_tree_to(&home, None, &["S-1-5-18", me.as_str()], &ours(&me))
            .expect_err("a hard link in it");
        assert!(e.to_string().contains("2 names (hard links)"), "{e}");
        assert!(!dacl(&home).expect("the DACL").protected, "nothing changed");
        assert_eq!(dacl(&outside).expect("the DACL"), before, "the file elsewhere untouched");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// 4-14's review: what appears between the first check and the change (planted in that
    /// moment) is refused when it is reached, each object being checked on its own handle right
    /// before it is set; the objects already set are named, and a file elsewhere that a planted
    /// hard link names is untouched (no access list is carried down to what a folder holds).
    #[cfg(windows)]
    #[test]
    fn what_appears_between_the_check_and_the_change_is_refused() {
        let dir = scratch("tree-race");
        let me = current_user_sid().expect("the user");
        // This user, SYSTEM and not Administrators: were an access list carried down to the
        // planted link's file, its Administrators entry would go.
        let principals = ["S-1-5-18", me.as_str()];
        let (home, outside) = tree(&dir);
        let before = dacl(&outside).expect("the DACL");
        assert!(
            before.aces.as_ref().is_some_and(|a| a
                .iter()
                .any(|x| x.inherited && x.sid.as_deref() == Some("S-1-5-32-544"))),
            "the file elsewhere inherits Administrators: {before:?}"
        );
        let planted = home.join("helper").join("planted.json");
        let mut plant = || std::fs::hard_link(&outside, &planted).expect("planted");
        let e = windows::restrict_tree_with(&home, None, &principals, &ours(&me), &mut plant)
            .expect_err("the planted link");
        let said = e.to_string();
        assert!(said.contains("planted.json") && said.contains("hard links"), "{said}");
        assert!(said.contains("found after the first check"), "{said}");
        assert!(said.contains("object(s) already set"), "{said}");
        assert_eq!(dacl(&outside).expect("the DACL"), before, "the file elsewhere untouched");
        std::fs::remove_file(&planted).expect("the link removed");
        // A junction planted in that moment: refused where it is reached.
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("made");
        let before = dacl(&elsewhere).expect("the DACL");
        let junction = home.join("helper").join("logs").join("planted");
        let mut plant = || {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&junction)
                .arg(&elsewhere)
                .output()
                .expect("mklink");
            assert!(made.status.success(), "{made:?}");
        };
        let e = windows::restrict_tree_with(&home, None, &principals, &ours(&me), &mut plant)
            .expect_err("the planted junction");
        let said = e.to_string();
        assert!(said.contains("a link or junction") && said.contains("found after"), "{said}");
        assert_eq!(dacl(&elsewhere).expect("the DACL"), before, "where it leads untouched");
        std::fs::remove_dir(&junction).expect("the junction removed");
        restrict_dir_to_owner(&home).expect("given back");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// The owners of what a test makes: the user, and the Administrators group when the process is
    /// elevated, which Windows makes the owner of what an elevated process creates (CI's Windows
    /// runners are elevated: helper.yml at 943a4d9 refused "owned by S-1-5-32-544" four times).
    #[cfg(windows)]
    fn ours(me: &str) -> Vec<&str> {
        if super::is_elevated().unwrap_or(false) { vec![me, "S-1-5-32-544"] } else { vec![me] }
    }

    /// 4-14's review: the folders above the Windows service's home must be no other account's to
    /// take over, or it could move the home aside and put its own in its place. The owners are
    /// simulated by the trusted list (this test cannot make a folder another account owns).
    #[cfg(windows)]
    #[test]
    fn a_folder_above_the_home_that_another_account_could_take_is_refused() {
        let dir = scratch("above");
        let me = current_user_sid().expect("the user");
        let parent = dir.join("CITAR");
        std::fs::create_dir_all(&parent).expect("made");
        let home = parent.join("helper");
        let trusted = ["S-1-5-18", "S-1-5-32-544", me.as_str()];
        // Owned by this user, trusted: passes (the home itself need not exist yet).
        windows::check_ancestors_until(&home, &trusted, Some(&dir)).expect("ours");
        // The same folder owned by an account that is not trusted (its owner left out: this user, or
        // the Administrators group when the process is elevated, as on CI's Windows runners).
        let untrusted: Vec<&str> = if super::is_elevated().unwrap_or(false) {
            vec!["S-1-5-18", me.as_str()]
        } else {
            vec!["S-1-5-18", "S-1-5-32-544"]
        };
        let e = windows::check_ancestors_until(&home, &untrusted, Some(&dir))
            .expect_err("another account's");
        assert!(e.to_string().contains("CITAR: owned by"), "{e}");
        // Granting Users the right to delete it, or what it holds: refused, named.
        windows::apply(
            &parent,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;;SD;;;BU)"),
            false,
        )
        .expect("set");
        let e = windows::check_ancestors_until(&home, &trusted, Some(&dir)).expect_err("Users");
        let said = e.to_string();
        assert!(
            said.contains("S-1-5-32-545 (Users)") && said.contains("delete or rename it"),
            "{said}"
        );
        windows::apply(
            &parent,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;CI;0x40;;;BU)"),
            false,
        )
        .expect("set");
        let e = windows::check_ancestors_until(&home, &trusted, Some(&dir)).expect_err("Users");
        assert!(e.to_string().contains("delete or rename what it holds"), "{e}");
        // Granting them only what %ProgramData% grants Users (make folders and files, read):
        // passes.
        windows::apply(
            &parent,
            &format!(
                "D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;0x1200a9;;;BU)(A;CI;0x116;;;BU)"
            ),
            false,
        )
        .expect("set");
        windows::check_ancestors_until(&home, &trusted, Some(&dir))
            .expect("what ProgramData grants");
        // A junction on the way: refused.
        restrict_dir_to_owner(&parent).expect("given back");
        let link = dir.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&parent)
            .output()
            .expect("mklink");
        assert!(made.status.success(), "{made:?}");
        let e = windows::check_ancestors_until(&link.join("helper"), &trusted, Some(&dir))
            .expect_err("a junction");
        assert!(e.to_string().contains("a link or junction"), "{e}");
        std::fs::remove_dir(&link).expect("the junction removed");
        // The machine's own %ProgramData% and drive root pass as they are.
        let data = std::env::var_os("ProgramData")
            .map_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"), std::path::PathBuf::from);
        check_ancestors(
            &data.join("citar-check-only-never-made").join("helper"),
            &["S-1-5-18", "S-1-5-32-544", TRUSTED_INSTALLER, me.as_str()],
        )
        .expect("ProgramData and the root as Windows makes them");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// The rule of [`takeover`] on descriptors made up for it.
    #[cfg(windows)]
    #[test]
    fn takeover_reads_the_owner_and_the_rights_that_hand_a_folder_over() {
        let ace = |sid: &str, mask: u32, inherit_only: bool| Ace {
            kind: AceKind::Allow,
            sid: Some(sid.to_owned()),
            mask,
            inherited: false,
            inherit_only,
        };
        let d = |owner: &str, aces: Vec<Ace>| Dacl {
            owner: Some(owner.to_owned()),
            protected: true,
            aces: Some(aces),
        };
        let trusted = ["S-1-5-18", "S-1-5-32-544"];
        let users = "S-1-5-32-545";
        assert_eq!(
            takeover(&d("S-1-5-18", vec![ace("S-1-5-18", 0x1f01ff, false)]), &trusted, false),
            None
        );
        assert!(
            takeover(&d(users, vec![]), &trusted, false)
                .is_some_and(|w| w.contains("owned by S-1-5-32-545 (Users)"))
        );
        let mut none = d("S-1-5-18", vec![]);
        none.aces = None;
        assert!(takeover(&none, &trusted, false).is_some_and(|w| w.contains("no access list")));
        for (mask, words) in [
            (0x10000, "delete or rename it"),
            (0x40, "delete or rename what it holds"),
            (0x40000, "change its permissions"),
            (0x80000, "take its ownership"),
            (0x1000_0000, "do anything"),
        ] {
            let w = takeover(&d("S-1-5-18", vec![ace(users, mask, false)]), &trusted, false);
            assert!(w.as_deref().is_some_and(|w| w.contains(words)), "{mask:#x}: {w:?}");
        }
        // Make folders and files, read: not a takeover.
        assert_eq!(
            takeover(&d("S-1-5-18", vec![ace(users, 0x1201bf, false)]), &trusted, false),
            None
        );
        // Inherit-only entries grant nothing on the folder itself (CREATOR OWNER's on ProgramData).
        assert_eq!(
            takeover(&d("S-1-5-18", vec![ace("S-1-3-0", 0x1000_0000, true)]), &trusted, false),
            None
        );
        // OWNER RIGHTS is the (trusted) owner's.
        assert_eq!(
            takeover(&d("S-1-5-18", vec![ace("S-1-3-4", 0x1f01ff, false)]), &trusted, false),
            None
        );
        // A drive's root cannot be deleted or renamed: DELETE there is harmless, DELETE_CHILD not.
        assert_eq!(
            takeover(&d("S-1-5-18", vec![ace(users, 0x10000, false)]), &trusted, true),
            None
        );
        assert!(takeover(&d("S-1-5-18", vec![ace(users, 0x40, false)]), &trusted, true).is_some());
    }

    /// A folder made with its access list has it from its first moment; one already there is left
    /// as it was.
    #[cfg(windows)]
    #[test]
    fn a_folder_made_with_its_access_list_has_it_from_the_start() {
        let dir = scratch("made-secured");
        let me = current_user_sid().expect("the user");
        let made = dir.join("made");
        let sddl = format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)");
        assert!(create_dir_with(&made, &sddl).expect("made"), "made now");
        let d = dacl(&made).expect("the DACL");
        assert!(d.protected, "{d:?}");
        let aces = d.aces.expect("a DACL");
        assert!(aces.iter().all(|a| !a.inherited), "{aces:?}");
        let mut sids: Vec<String> = aces.iter().filter_map(|a| a.sid.clone()).collect();
        sids.sort();
        sids.dedup();
        let mut want = vec!["S-1-5-18".to_owned(), me.clone()];
        want.sort();
        assert_eq!(sids, want);
        assert!(!create_dir_with(&made, "D:P(A;OICI;FA;;;SY)").expect("there"), "left as it was");
        assert_eq!(dacl(&made).expect("the DACL").aces.map(|a| a.len()), Some(aces.len()));
        assert!(create_dir_with(&dir.join("no").join("parent"), &sddl).is_err());
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[cfg(windows)]
    #[test]
    fn a_console_process_is_no_service() {
        // `citar-helper service run` from a console: the service manager did not start it.
        let e =
            run_service("citar-helper-test", Box::new(|| {}), Box::new(|| 0)).expect_err("refused");
        assert_eq!(e.raw_os_error(), Some(1063), "{e}");
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
        assert!(dir_exposure(&dir.join("absent")).is_err());
        assert!(restrict_dir_to_owner(&dir.join("absent")).is_err());
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
