//! Windows: the helper per user, from a Run value, or as a Windows service (design/phase4-helper.md
//! P4.3.9).
//!
//! **`--user`** (the default) writes the value `CITAR helper` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (with `reg.exe`), which runs `citar-helper
//! run --detach --home …` at each sign-in, and starts it now. `--detach` starts the helper's Run
//! supervisor (`super::supervisor`) with no window and returns: the supervisor runs the helper in
//! service mode and starts it again at once after exit 75 and after a crash with backoff, and stops
//! on 0. Nothing here needs an administrator.
//!
//! **`--system`** installs the Windows service `citar-helper` (with `sc.exe`), started
//! automatically, running as the virtual account `NT SERVICE\citar-helper` (not LocalSystem: code
//! that parses a server's data needs no more than its own account). The binary is copied into
//! `%ProgramData%\CITAR\helper\bin` (so an update, run by that account, can replace it), and the
//! home `%ProgramData%\CITAR\helper` gets a protected DACL of SYSTEM, Administrators and that
//! account alone, owned by Administrators (inherited from `%ProgramData%` it would give Users read
//! of `helper.json`, which holds the token, and of the machine key). Its failure actions start it
//! again 15 s after every failure, with `fFailureActionsOnNonCrashFailures` set (`sc failureflag`),
//! so an exit code that is not 0 counts: 75 (an update), and a new binary's failed start, which an
//! update's rollback counts (the last action repeats, so no count of quick failures stops it).
//! Installing needs an elevated prompt; an unelevated `install --system` prints the elevated
//! command and exits 2. Under `%ProgramData%` every user may make folders and files, and owns
//! what they make, so before anything is made:
//! - **the folders above the home** (its parent to the drive's root) must be no other account's to
//!   take over: each owned by SYSTEM, Administrators, TrustedInstaller or the administrator
//!   installing, none letting another account delete or rename it or what it holds, or change its
//!   permissions or owner, and none a link or junction ([`check_above`], citar-jobs'
//!   `check_ancestors`; 4-14's review). The home's own access list cannot keep the owner of
//!   `%ProgramData%\CITAR` from moving the home aside and putting its own, binary and all, in its
//!   place, where the service's fixed binary path would run it as the service. A missing folder
//!   above the home is made with its access list from its first moment (Administrators' and
//!   SYSTEM's, Users may read), and one another account makes in the meantime is checked as the
//!   others were;
//! - **the home** is made with its access list from its first moment when it is missing, and set
//!   (with what it holds) only when no link, junction or file with a second name (a hard link)
//!   lies in it and everything in it is SYSTEM's, Administrators', the installing administrator's
//!   or the service's (citar-jobs' `restrict_tree_to`, which checks each object again on its own
//!   handle right before it sets it): a home another user made first, or a junction or hard link
//!   planted in one, is refused rather than taken over. Its `bin` is made after, inheriting the
//!   home's entries; and the binary's copy replaces no file that is there (a hard link would carry
//!   the write to its target).

use std::path::{Path, PathBuf};

use super::plan::{Plan, Quoting, Step, windows_quote};
use super::{DISPLAY, NAME, RUN_VALUE};

/// The Run key the per-user value lives under.
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// SYSTEM.
pub const SYSTEM: &str = "S-1-5-18";
/// The Administrators group.
pub const ADMINISTRATORS: &str = "S-1-5-32-544";
/// The Users group.
pub const USERS: &str = "S-1-5-32-545";
/// `NT SERVICE\TrustedInstaller`, which owns a drive's root and Windows' own folders.
pub const TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
/// The service's description.
pub const DESCRIPTION: &str = "Lends this machine's model endpoint and, if its owner says so, its spare CPU to a CITAR server.";
/// How long a failed service waits before the service manager starts it again, in ms.
pub const RESTART_MS: u32 = 15_000;
/// After a day without a failure the service manager counts failures from the first action again.
pub const RESET_S: u32 = 86_400;

/// The SID of the virtual account `NT SERVICE\<name>`: `S-1-5-80-` and the SHA-1 of the service's
/// name in capitals (UTF-16LE), read as five little-endian 32-bit numbers. It is fixed by the name,
/// so the plan can name it before the service exists.
#[must_use]
pub fn service_sid(name: &str) -> String {
    use sha1::Digest as _;
    let upper: Vec<u8> = name.to_uppercase().encode_utf16().flat_map(u16::to_le_bytes).collect();
    let digest = sha1::Sha1::digest(&upper);
    let mut sid = "S-1-5-80".to_owned();
    for chunk in digest.as_chunks::<4>().0 {
        sid.push_str(&format!("-{}", u32::from_le_bytes(*chunk)));
    }
    sid
}

/// The account a service SID of this module belongs to, for the plan's text.
#[must_use]
pub fn named_service_sid(sid: &str) -> Option<String> {
    (sid == service_sid(NAME)).then(|| format!(r"NT SERVICE\{NAME}"))
}

/// The account the service runs as.
#[must_use]
pub fn account() -> String {
    format!(r"NT SERVICE\{NAME}")
}

/// A command line running `binary` with `args`, the binary's path quoted whatever it holds.
fn command_line(binary: &Path, args: &[&str]) -> String {
    let mut s = windows_quote(&binary.display().to_string(), true);
    for a in args {
        s.push(' ');
        s.push_str(&windows_quote(a, false));
    }
    s
}

/// The Run value's data: the helper's Run supervisor, detached, for the home `home`.
#[must_use]
pub fn run_value(binary: &Path, home: &Path) -> String {
    format!(
        "{} --home {}",
        command_line(binary, &["run", "--detach"]),
        windows_quote(&home.display().to_string(), true)
    )
}

/// The service's command line (its `binPath`): `service run` with the home.
#[must_use]
pub fn bin_path(binary: &Path, home: &Path) -> String {
    format!(
        "{} --home {}",
        command_line(binary, &["service", "run"]),
        windows_quote(&home.display().to_string(), true)
    )
}

fn reg(args: &[&str]) -> Step {
    Step::Run {
        program: "reg.exe".to_owned(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        may_fail: false,
    }
}

/// `install --user`: the Run value written, then the helper started now as sign-in will start it.
#[must_use]
pub fn user_install_plan(binary: &Path, home: &Path) -> Plan {
    let value = run_value(binary, home);
    Plan {
        title: "citar-helper service install --user".to_owned(),
        quoting: Quoting::Windows,
        steps: vec![
            Step::StopSupervisor { home: home.to_path_buf() },
            reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"]),
            Step::Start {
                program: binary.to_path_buf(),
                args: vec![
                    "run".to_owned(),
                    "--detach".to_owned(),
                    "--home".to_owned(),
                    home.display().to_string(),
                ],
            },
            Step::Note(format!(
                "the helper starts at each sign-in from the Run value \"{RUN_VALUE}\"; its log is \
                 helper\\logs in {}",
                home.display()
            )),
        ],
    }
}

/// `uninstall --user`: the Run value removed, the running helper stopped.
#[must_use]
pub fn user_uninstall_plan(home: &Path) -> Plan {
    Plan {
        title: "citar-helper service uninstall --user".to_owned(),
        quoting: Quoting::Windows,
        steps: vec![
            Step::Run {
                program: "reg.exe".to_owned(),
                args: vec![
                    "delete".to_owned(),
                    RUN_KEY.to_owned(),
                    "/v".to_owned(),
                    RUN_VALUE.to_owned(),
                    "/f".to_owned(),
                ],
                may_fail: true,
            },
            Step::StopSupervisor { home: home.to_path_buf() },
        ],
    }
}

fn sc(args: &[&str], may_fail: bool) -> Step {
    Step::Run {
        program: "sc.exe".to_owned(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        may_fail,
    }
}

/// What may own a folder above the service's home, and hold rights over it: SYSTEM, Administrators,
/// TrustedInstaller (a drive's root) and the administrator installing.
#[cfg(windows)]
fn trusted_above() -> Result<Vec<String>, String> {
    let me = citar_jobs::sys::current_user_sid().map_err(|e| format!("this account: {e}"))?;
    Ok(vec![SYSTEM.to_owned(), ADMINISTRATORS.to_owned(), TRUSTED_INSTALLER.to_owned(), me])
}

/// What a refusal of a folder above the home says to do.
fn above_refused(e: &std::io::Error, home: &Path) -> String {
    format!(
        "{e}; another account could then move the service's home {} aside and put its own in its \
         place, and its program would run as the service: remove that folder, or give it to \
         Administrators with nobody else allowed to delete, rename or re-permission it, and install \
         again",
        home.display()
    )
}

/// Refuses a home whose folders above it (from its parent to its drive's root) hold a link or
/// junction, or one another account could take over: owned by it, or letting it delete or rename
/// the folder or what it holds, or change the folder's permissions or owner (citar-jobs'
/// `check_ancestors`). Under `%ProgramData%` any user may make a folder, and owns it: a
/// `%ProgramData%\CITAR` another user made before the administrator installs would let that user
/// replace the home under it, binary and all, whatever the home's own access list says. Checked
/// before anything is made; a folder that is missing is made by the plan, secured as it is made.
///
/// # Errors
/// The folder refused and why, with what to do.
pub fn check_above(home: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let trusted = trusted_above()?;
        let t: Vec<&str> = trusted.iter().map(String::as_str).collect();
        citar_jobs::sys::check_ancestors(home, &t).map_err(|e| above_refused(&e, home))
    }
    #[cfg(not(windows))]
    {
        Err(format!("{}: the folders' access lists are Windows' own", home.display()))
    }
}

/// One folder above the home, checked as [`check_above`] checks them (one another account made
/// while the plan made the folders above the home).
///
/// # Errors
/// As [`check_above`].
pub fn check_one_above(folder: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let trusted = trusted_above()?;
        let t: Vec<&str> = trusted.iter().map(String::as_str).collect();
        citar_jobs::sys::check_ancestor(folder, &t).map_err(|e| above_refused(&e, folder))
    }
    #[cfg(not(windows))]
    {
        Err(format!("{}: the folders' access lists are Windows' own", folder.display()))
    }
}

/// The service's binary in its home: `<home>\bin\citar-helper.exe`.
#[must_use]
pub fn service_binary(home: &Path) -> PathBuf {
    Quoting::Windows.join(home, &["bin", &format!("{NAME}.exe")])
}

/// `install --system`: the folders above the home checked (none another account could take over)
/// and the missing ones made secured; the home made secured when missing and its DACL set on it
/// and on what it holds; then its `bin` made (inheriting the home's entries), this binary copied
/// in, the service created with its account, description, SID type, failure actions and their
/// non-crash flag, and started.
#[must_use]
pub fn system_install_plan(source: &Path, home: &Path) -> Plan {
    let binary = service_binary(home);
    let sid = service_sid(NAME);
    let actions = format!("restart/{RESTART_MS}/restart/{RESTART_MS}/restart/{RESTART_MS}");
    let reset = RESET_S.to_string();
    let account = account();
    let bin = bin_path(&binary, home);
    Plan {
        title: "citar-helper service install --system".to_owned(),
        quoting: Quoting::Windows,
        steps: vec![
            Step::CheckAbove { path: home.to_path_buf() },
            Step::FoldersAbove {
                path: home.to_path_buf(),
                owner: ADMINISTRATORS.to_owned(),
                full: vec![SYSTEM.to_owned(), ADMINISTRATORS.to_owned()],
                read: vec![USERS.to_owned()],
            },
            Step::Secure {
                path: home.to_path_buf(),
                owner: ADMINISTRATORS.to_owned(),
                principals: vec![SYSTEM.to_owned(), ADMINISTRATORS.to_owned(), sid],
            },
            Step::Folder(Quoting::Windows.join(home, &["bin"])),
            Step::Copy { from: source.to_path_buf(), to: binary },
            sc(
                &[
                    "create",
                    NAME,
                    "binPath=",
                    &bin,
                    "start=",
                    "auto",
                    "obj=",
                    &account,
                    "DisplayName=",
                    DISPLAY,
                ],
                false,
            ),
            sc(&["description", NAME, DESCRIPTION], false),
            sc(&["sidtype", NAME, "unrestricted"], false),
            sc(&["failure", NAME, "reset=", &reset, "actions=", &actions], false),
            sc(&["failureflag", NAME, "1"], false),
            sc(&["start", NAME], false),
            Step::Note(format!(
                "the service {NAME} runs as {account} with the home {}; its log is helper\\logs there",
                home.display()
            )),
        ],
    }
}

/// `uninstall --system`: stopped, waited for, deleted. The home stays (its settings and the
/// machine's key), as the plan says.
#[must_use]
pub fn system_uninstall_plan(home: &Path) -> Plan {
    Plan {
        title: "citar-helper service uninstall --system".to_owned(),
        quoting: Quoting::Windows,
        steps: vec![
            sc(&["stop", NAME], true),
            Step::WaitStopped { name: NAME.to_owned(), seconds: 30 },
            sc(&["delete", NAME], false),
            Step::Note(format!(
                "the home {} is kept (helper.json and the machine's key); delete it to remove \
                 everything",
                home.display()
            )),
        ],
    }
}

/// A service's state from `sc.exe queryex`'s text: its number (1 stopped, 2 starting, 3 stopping,
/// 4 running, ...) and its process id. The labels `sc.exe` prints are not read: only the numbers,
/// which are the same in every language.
#[must_use]
pub fn read_queryex(text: &str) -> (Option<u32>, Option<u32>) {
    let mut state = None;
    let mut pid = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        let first = value.split_whitespace().next().and_then(|v| v.parse::<u32>().ok());
        if key == "STATE" {
            state = first;
        } else if key == "PID" {
            pid = first;
        }
    }
    (state, pid)
}

/// What `sc.exe queryex` says of the service (`None`: it is not installed).
#[must_use]
pub fn read_system_status(text: Option<&str>) -> super::Status {
    let Some(text) = text else {
        return super::Status {
            installed: false,
            running: false,
            line: format!("the Windows service {NAME}: not installed"),
        };
    };
    let (state, pid) = read_queryex(text);
    let word = match state {
        Some(1) => "stopped",
        Some(2) => "starting",
        Some(3) => "stopping",
        Some(4) => "running",
        Some(5) => "continuing",
        Some(6) => "pausing",
        Some(7) => "paused",
        _ => "in an unknown state",
    };
    let mut line = format!("the Windows service {NAME} ({}): {word}", account());
    if let Some(p) = pid.filter(|p| *p != 0) {
        line.push_str(&format!(", pid {p}"));
    }
    super::Status { installed: true, running: state == Some(4), line }
}

/// The Run value's data from `reg.exe query`'s text, when it is there.
#[must_use]
pub fn read_run_value(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let t = l.trim_start();
        let rest = t.strip_prefix(RUN_VALUE)?;
        let rest = rest.trim_start().strip_prefix("REG_SZ")?;
        Some(rest.trim().to_owned())
    })
}

/// Waits until the service `name` has stopped (or is not installed), at most `seconds`.
///
/// # Errors
/// It still runs after that long.
pub fn wait_stopped(name: &str, seconds: u32) -> Result<(), String> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(u64::from(seconds));
    loop {
        match super::plan::run_program("sc.exe", &["queryex".to_owned(), name.to_owned()]) {
            Err(_) => return Ok(()), // not installed (1060): nothing runs
            Ok(text) => {
                if read_queryex(&text).0 == Some(1) {
                    return Ok(());
                }
            }
        }
        if std::time::Instant::now() >= until {
            return Err(format!("the service {name} had not stopped after {seconds} s"));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_sid_is_the_sha1_of_its_name_in_capitals() {
        // Windows' own: `sc showsid TrustedInstaller`.
        assert_eq!(
            service_sid("TrustedInstaller"),
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        );
        assert_eq!(service_sid("citar-helper"), service_sid("CITAR-HELPER"));
        assert_eq!(
            named_service_sid(&service_sid(NAME)).as_deref(),
            Some(r"NT SERVICE\citar-helper")
        );
    }

    #[test]
    fn sc_and_reg_output_is_read_by_its_numbers() {
        let running = "\r\nSERVICE_NAME: citar-helper \r\n        TYPE               : 10  WIN32_OWN_PROCESS  \r\n        STATE              : 4  RUNNING \r\n                                (STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\r\n        WIN32_EXIT_CODE    : 0  (0x0)\r\n        SERVICE_EXIT_CODE  : 0  (0x0)\r\n        CHECKPOINT         : 0x0\r\n        WAIT_HINT          : 0x0\r\n        PID                : 5120\r\n        FLAGS              : \r\n";
        assert_eq!(read_queryex(running), (Some(4), Some(5120)));
        let s = read_system_status(Some(running));
        assert!(s.installed && s.running);
        assert_eq!(
            s.line,
            r"the Windows service citar-helper (NT SERVICE\citar-helper): running, pid 5120"
        );
        assert!(!read_system_status(None).installed);
        let value = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    CITAR helper    REG_SZ    \"C:\\a\\citar-helper.exe\" run --detach --home \"C:\\h\"\r\n\r\n";
        assert_eq!(
            read_run_value(value).as_deref(),
            Some(r#""C:\a\citar-helper.exe" run --detach --home "C:\h""#)
        );
        assert_eq!(
            read_run_value("\r\nHKEY_CURRENT_USER\\...\\Run\r\n    Other    REG_SZ    x\r\n"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_home_a_junction_leads_to_is_refused() {
        let dir = std::env::temp_dir().join(format!("citar-svc-junction-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("made");
        let link = dir.join("CITAR");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&elsewhere)
            .output()
            .expect("mklink");
        assert!(made.status.success(), "{made:?}");
        // The folder right above the home is a junction: refused there, first, with what to do.
        // (Folders further up that another account could take over are refused too: citar-jobs'
        // sys tests, which can bound the walk to their own folders.)
        let e = check_above(&link.join("helper")).expect_err("refused");
        assert!(e.contains("CITAR: a link or junction"), "{e}");
        assert!(e.contains("could then move the service's home"), "{e}");
        std::fs::remove_dir(&link).expect("the junction removed");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn the_command_lines_quote_their_paths() {
        assert_eq!(
            run_value(
                Path::new(r"C:\Users\a b\citar-helper.exe"),
                Path::new(r"C:\Users\a b\AppData\Local\CITAR")
            ),
            r#""C:\Users\a b\citar-helper.exe" run --detach --home "C:\Users\a b\AppData\Local\CITAR""#
        );
        assert_eq!(
            bin_path(
                &service_binary(Path::new(r"C:\ProgramData\CITAR\helper")),
                Path::new(r"C:\ProgramData\CITAR\helper")
            ),
            r#""C:\ProgramData\CITAR\helper\bin\citar-helper.exe" service run --home "C:\ProgramData\CITAR\helper""#
        );
    }
}
