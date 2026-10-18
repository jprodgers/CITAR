//! Unix: owner-only files by their mode, as SSH holds a private key (P4.3.7), which the standard
//! library's permissions API covers; and the job host's limits (P4.3.6, 4-08), through `libc`:
//! its priority (`setpriority`: `nice` on Linux, the background band on macOS), its private
//! writable memory (`RLIMIT_DATA`, Linux's fallback when no cgroup holds it), and on macOS its
//! resident size, which the host's watchdog reads.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{Exposure, shell_quote};

/// The mode bits that give the group or others anything.
const NOT_OWNER: u32 = 0o077;

pub(super) fn restrict_to_owner(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

pub(super) fn exposure(path: &Path) -> io::Result<Option<Exposure>> {
    exposure_of(path, "600")
}

/// A folder: mode 0700, so only its owner may list it or add, replace or remove what it holds.
pub(super) fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

pub(super) fn dir_exposure(path: &Path) -> io::Result<Option<Exposure>> {
    exposure_of(path, "700")
}

/// Who besides the owner the mode lets in, and the `chmod` to `mode` that shuts them out.
fn exposure_of(path: &Path, mode_fix: &str) -> io::Result<Option<Exposure>> {
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & NOT_OWNER == 0 {
        return Ok(None);
    }
    let mut who = Vec::new();
    if mode & 0o070 != 0 {
        who.push("its group".to_owned());
    }
    if mode & 0o007 != 0 {
        who.push("others".to_owned());
    }
    Ok(Some(Exposure { who, fix: format!("chmod {mode_fix} {}", shell_quote(path)) }))
}

/// Asks the process `pid` to stop (`SIGTERM`): the helper's console supervisor passes a stop on
/// to the helper it runs (design/phase4-helper.md P4.3.8), which the standard library's
/// `Child::kill` (`SIGKILL`) cannot.
///
/// # Errors
/// No such process, or not one this user may signal.
pub fn terminate(pid: u32) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no such process id"))?;
    // SAFETY: kill takes plain integers; a pid above 0 names one process, never a group.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The real user id of this process (`getuid`): launchd's per-user domain is `gui/<uid>`, and
/// systemd's per-user runtime folder `/run/user/<uid>` (the helper's service installation, P4.3.9).
#[must_use]
pub fn user_id() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Whether this process has root's privileges (its effective user id is 0): a system service
/// is installed only so, and the helper says how rather than elevating (P4.3.9).
#[must_use]
pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Sets this process's nice value to `n` (higher is lower priority; raising it needs no
/// privilege).
///
/// # Errors
/// The OS refused.
#[cfg(not(target_os = "macos"))]
pub fn nice(n: i32) -> io::Result<()> {
    // SAFETY: setpriority takes plain integers; `who` 0 is this process.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, n) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Lowers this process's `RLIMIT_DATA` (its private writable memory: heap, anonymous mappings,
/// thread stacks) to `bytes`, or to its hard limit when that is lower. An allocation past it
/// fails, and Rust ends the process with "memory allocation of N bytes failed".
///
/// # Errors
/// The OS refused.
#[cfg(not(target_os = "macos"))]
#[allow(
    clippy::unnecessary_fallible_conversions,
    clippy::useless_conversion,
    reason = "rlim_t is u64 on the 64-bit targets CITAR ships for, and narrower on others"
)]
pub fn limit_data(bytes: u64) -> io::Result<u64> {
    let mut now = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: `now` is a valid rlimit for the call to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_DATA, &raw mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let want = libc::rlim_t::try_from(bytes).unwrap_or(libc::RLIM_INFINITY);
    let cap = if now.rlim_max == libc::RLIM_INFINITY { want } else { want.min(now.rlim_max) };
    let new = libc::rlimit { rlim_cur: cap, rlim_max: cap };
    // SAFETY: `new` is a valid rlimit, read by the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_DATA, &raw const new) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::try_from(cap).unwrap_or(u64::MAX))
}

/// Puts this process in macOS's background band (`PRIO_DARWIN_BG`): low CPU and I/O priority,
/// and on Apple silicon the efficiency cores.
///
/// # Errors
/// The OS refused.
#[cfg(target_os = "macos")]
pub fn background() -> io::Result<()> {
    // SAFETY: setpriority takes plain integers; `who` 0 is this process.
    if unsafe { libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// This process's memory in bytes as macOS counts it against a process: its physical footprint
/// (`proc_pid_rusage`'s `ri_phys_footprint`, what Activity Monitor shows and the kernel's own
/// memory limits count). Not the resident size, which also counts the shared libraries' pages the
/// process has touched and memory it freed that the system has not yet taken back: a run that
/// allocated its share passed the limit by that alone (helper.yml at fbfe28b, aarch64-apple-darwin:
/// 231 MB resident for a host of 15 MB and a run of 157 MB, against 200 MB).
///
/// # Errors
/// The OS could not say.
#[cfg(target_os = "macos")]
pub fn resident_bytes() -> io::Result<u64> {
    // SAFETY: an all-zero rusage_info_v2 is a valid value of that plain C structure.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a rusage_info_v2, the structure RUSAGE_INFO_V2 asks the call to fill
    // (passed, as the C API takes it, as a pointer to `rusage_info_t`); getpid has no
    // preconditions.
    let got = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if got != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ri_phys_footprint)
}
