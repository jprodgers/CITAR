//! The console run's supervisor (design/phase4-helper.md P4.3.8, step 7): a helper started by
//! hand that has swapped its binary for an update does not exit; it starts the new binary as its
//! child, with its own arguments and `--supervised`, and waits on it. A supervised helper hands
//! the next update off as a service does, by exiting 75, so one supervisor serves a whole console
//! session however many updates come.
//!
//! - **The child exits 75:** it handed off (or rolled itself back): the binary in place is started
//!   again at once, at most [`MAX_RESTARTS`] times a minute.
//! - **The child fails before it was healthy** (`pending.json` there, `healthy.json` not, and an
//!   exit that is not 0, stopped by its owner, or 78, permanently refused by its server, which a
//!   working binary gives too), **or cannot be started at all** (an antivirus quarantined the new
//!   file; 4-13's fix round): the update failed. The old binary is restored, `failed.json`
//!   written for it to report, and it is started in the new one's place. A new binary stopped
//!   before it was healthy keeps its `pending.json`: its next start counts on.
//! - **Anything else:** the supervisor exits with the child's code (0 stopped, 78 a permanent
//!   refusal, another a crash), as the console run would have.
//!
//! Ctrl-C reaches the child too (it shares the console): the supervisor waits for it to stop and
//! exits with its code. On Unix a `SIGTERM` sent to the supervisor alone is passed on to the child.

use std::ffi::OsString;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use crate::log::Log;
use crate::update::files::{Install, UpdateDir};

/// The exit code of a run that hands off to an update.
pub const EXIT_UPDATE: u8 = 75;
/// The most hand-offs in a minute before the supervisor gives up (a binary that exits 75 at
/// once, again and again, would otherwise spin).
pub const MAX_RESTARTS: usize = 5;
/// The flag a supervised child gets.
pub const SUPERVISED_FLAG: &str = "--supervised";

/// A child's exit as the console run's exit code (a signal's death as 128 plus the signal on Unix,
/// 1 elsewhere).
fn code_of(status: ExitStatus) -> u8 {
    if let Some(c) = status.code() {
        return u8::try_from(c).unwrap_or(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = status.signal() {
            return u8::try_from(128 + sig).unwrap_or(1);
        }
    }
    1
}

/// Runs the binary `install` holds as a child with `args` (this run's own) and `--supervised`,
/// as long as the module's doc says; returns the console run's exit code.
#[must_use]
pub fn supervise(install: &Install, dir: &UpdateDir, args: &[OsString], log: &Log) -> u8 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            log.error(format!("the supervisor could not start: {e}"));
            return 1;
        }
    };
    rt.block_on(async {
        let mut handoffs: Vec<Instant> = Vec::new();
        loop {
            let mut cmd = tokio::process::Command::new(install.path());
            cmd.args(args).arg(SUPERVISED_FLAG).kill_on_drop(false);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    log.error(format!("could not start {}: {e}", install.path().display()));
                    // A new binary this machine will not run (an antivirus quarantined it, a
                    // folder that runs nothing): rolled back as one that fails at its start.
                    if roll_back(install, dir, &format!("could not be started ({e})"), log) {
                        continue;
                    }
                    return 1;
                }
            };
            log.info(format!(
                "supervising {} (pid {})",
                install.path().display(),
                child.id().unwrap_or(0)
            ));
            let status = wait(&mut child, log).await;
            let code = status.map_or(1, code_of);
            if code == EXIT_UPDATE {
                let now = Instant::now();
                handoffs.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                handoffs.push(now);
                if handoffs.len() > MAX_RESTARTS {
                    log.error(format!(
                        "the helper exited {EXIT_UPDATE} (restart for an update) {} times in a \
                         minute: stopping",
                        handoffs.len()
                    ));
                    return 1;
                }
                log.info("the helper restarts for an update");
                continue;
            }
            if !matches!(code, 0 | 78)
                && roll_back(
                    install,
                    dir,
                    &format!("exited with code {code} before it was healthy"),
                    log,
                )
            {
                continue;
            }
            return code;
        }
    })
}

/// When the binary in place is an update's new one that never became healthy (`pending.json`
/// there, `healthy.json` not): the old binary restored, `failed.json` written for it to report
/// (`"<new> <what>: <old> is back"`) and `pending.json` removed. True when the old binary is back,
/// to be run in the new one's place.
fn roll_back(install: &Install, dir: &UpdateDir, what: &str, log: &Log) -> bool {
    let Some(p) = dir.pending() else { return false };
    if dir.healthy_exists() {
        return false;
    }
    let message = format!("{} {what}: {} is back", p.new, p.old);
    log.warn(format!("the update failed: {message}"));
    let restored = install.restore_old();
    let _gone = dir.remove_pending();
    match restored {
        Ok(()) => {
            let _written = dir.write_failed(&p.new, &p.sha256, &message);
            true
        }
        Err(e) => {
            log.error(format!("could not roll the update back: {e}"));
            false
        }
    }
}

/// Waits for the child; Ctrl-C (which the child also gets) is waited through, and on Unix a
/// `SIGTERM` is passed on.
async fn wait(child: &mut tokio::process::Child, log: &Log) -> Option<ExitStatus> {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    loop {
        #[cfg(unix)]
        let terminated = async {
            match term.as_mut() {
                Some(t) => {
                    t.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        #[cfg(not(unix))]
        let terminated = std::future::pending::<()>();
        tokio::select! {
            s = child.wait() => return s.ok(),
            r = tokio::signal::ctrl_c() => {
                if r.is_err() {
                    // No console handler: wait on the child alone.
                    return child.wait().await.ok();
                }
                log.info("stopping: waiting for the helper to stop");
            }
            () = terminated => {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    let _passed = citar_jobs::sys::terminate(pid);
                }
                log.info("stopping: the helper is told to stop");
            }
        }
    }
}
