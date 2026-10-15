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
    Ok(Some(Exposure { who, fix: format!("chmod 600 {}", shell_quote(path)) }))
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

/// This process's resident size in bytes, as the kernel's task info reports it.
///
/// # Errors
/// The OS could not say.
#[cfg(target_os = "macos")]
pub fn resident_bytes() -> io::Result<u64> {
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    let len = libc::c_int::try_from(size).map_err(io::Error::other)?;
    // SAFETY: an all-zero proc_taskinfo is a valid value of that plain C structure.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a buffer of exactly `len` bytes for the call to fill; getpid has no
    // preconditions.
    let got = unsafe {
        libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), len)
    };
    if got != len {
        return Err(io::Error::last_os_error());
    }
    Ok(info.pti_resident_size)
}
