//! Running the scan's probe programs (`nvidia-smi`, PowerShell, `system_profiler`, `lspci`,
//! `rocm-smi`, `lsblk`, `sysctl`), as `citar/hwinfo.py` `_run` does (lines 26-36): best-effort,
//! with a timeout, standard input closed, standard error dropped, and the output only when the
//! program succeeded. A probe that is missing, fails, or outlives its timeout gives an empty
//! string: less information, never an error.
//!
//! On Windows a probe starts with no console window (`CREATE_NO_WINDOW`), so a helper running
//! without one (a service, `run --detach`) never flashes one every ten seconds.

use std::ffi::OsStr;
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The most of a probe's output read: a scan's probes print kilobytes.
pub const MAX_OUTPUT: u64 = 4 * 1024 * 1024;
/// How long the probe may take to end once its output is closed.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// Runs `program args`, for at most `timeout`; its standard output (UTF-8, a byte-order mark
/// dropped, anything else replaced) when it exits successfully, else `""`.
#[must_use]
pub fn run(program: &OsStr, args: &[&str], timeout: Duration) -> String {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `CREATE_NO_WINDOW` (winbase.h).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else { return String::new() };
    let Some(stdout) = child.stdout.take() else {
        let _killed = child.kill();
        let _reaped = child.wait();
        return String::new();
    };
    // Read on a thread of its own: the output ends when the program does (or closes it), and the
    // wait for it has the timeout.
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new().name("citar-probe".into()).spawn(move || {
        let mut buf = Vec::new();
        let _read = stdout.take(MAX_OUTPUT).read_to_end(&mut buf);
        let _sent = tx.send(buf);
    });
    if reader.is_err() {
        let _killed = child.kill();
        let _reaped = child.wait();
        return String::new();
    }
    let deadline = Instant::now() + timeout;
    let Ok(out) = rx.recv_timeout(timeout) else {
        let _killed = child.kill();
        let _reaped = child.wait();
        return String::new();
    };
    // The output is closed: the program has ended, or is about to.
    let until = deadline.max(Instant::now()) + EXIT_WAIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _killed = child.kill();
                let _reaped = child.wait();
                break None;
            }
        }
    };
    match status {
        Some(s) if s.success() => decode(&out),
        _ => String::new(),
    }
}

/// Python's `decode("utf-8-sig", errors="replace")`.
#[must_use]
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_program_gives_nothing() {
        let t = Instant::now();
        assert_eq!(run(OsStr::new("citar-no-such-probe-program"), &[], Duration::from_secs(5)), "");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn decoding_drops_the_byte_order_mark_and_replaces_what_is_not_utf8() {
        assert_eq!(decode(b"\xef\xbb\xbf{\"a\":1}"), "{\"a\":1}");
        assert_eq!(decode(b"a\xffb"), "a\u{fffd}b");
    }
}
