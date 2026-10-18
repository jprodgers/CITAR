//! The Run supervisor (design/phase4-helper.md P4.3.9): what keeps the helper running on Windows
//! for one user, where no service manager does. The Run value starts `citar-helper run --detach`
//! at sign-in; `--detach` starts this supervisor (`run --supervisor`, internal) with no window and
//! returns at once, and the supervisor runs the helper as its child in service mode (`run
//! --service`, its window hidden) until it is asked to stop:
//!
//! - **exit 75** (an update's hand-off, P4.3.8, or a new binary rolling itself back): started again
//!   at once, the binary in place being the update's (at most [`Policy::handoffs_per_minute`] a
//!   minute; more are a crash's);
//! - **any other failure** (a crash, a start that failed): started again after a backoff, 15 s
//!   doubling to 10 minutes, back to 15 s once a helper has run for 10 minutes. A new binary that
//!   fails its start counts its starts itself and rolls the update back at its fourth (4-13);
//! - **exit 0** (stopped) or 78 (refused for good, which a service run never exits with: it idles
//!   and tries again hourly): the supervisor stops too.
//!
//! **Stopping it:** `citar-helper service uninstall --user` (and `install` again) writes
//! `helper/supervisor.stop` in the home, which the supervisor looks for every second: it ends the
//! helper and exits. On Unix `SIGTERM` does the same (the helper is passed a `SIGTERM`); on Windows
//! the helper is ended at once, as closing its window would (a game it played is played again
//! elsewhere; results it holds are sent at its next start). `helper/supervisor.json` names the
//! supervisor's process and the helper's, which `service status` reads; one supervisor per home:
//! a second one started while the first runs leaves at once. A process the record names counts
//! only when its command line says what it is (`--supervisor` or `--service`, and this home):
//! a record a supervisor ended from outside left behind may name process ids the system has since
//! given to other CITAR helpers. Its log is `helper/logs/supervisor.log`.
//!
//! The supervising itself ([`supervise`]) is written over a [`Child`], so the tests run it on
//! tokio's paused clock against children of their own, among them the helper's own session
//! refused by a fake server (gate 3: a revoked helper under it idles, one attempt an hour).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::home::Home;
use crate::log::Log;
use crate::net::BoxFuture;

/// The supervisor's record in the home's `helper/`: its process, the helper's, its program.
pub const PID_FILE: &str = "supervisor.json";
/// The file whose appearance in the home's `helper/` asks the supervisor to stop.
pub const STOP_FILE: &str = "supervisor.stop";
/// The supervisor's own log, beside the helper's.
pub const LOG_FILE: &str = "supervisor.log";
/// The flag `--detach` starts the supervisor with (internal).
pub const SUPERVISOR_FLAG: &str = "--supervisor";
/// The exit code of an update's hand-off.
pub const EXIT_UPDATE: i32 = 75;
/// The exit code of a permanent refusal.
pub const EXIT_REFUSED: i32 = 78;

/// How the supervisor paces its restarts.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// The wait after the first crash.
    pub first: Duration,
    /// The longest wait.
    pub max: Duration,
    /// A helper that ran this long resets the wait.
    pub healthy_after: Duration,
    /// More hand-offs (exit 75) than this in a minute are a crash's.
    pub handoffs_per_minute: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            first: Duration::from_secs(15),
            max: Duration::from_secs(600),
            healthy_after: Duration::from_secs(600),
            handoffs_per_minute: 5,
        }
    }
}

/// What the supervisor runs.
pub trait Child: Send + Sync {
    /// Starts the helper and waits for it to end, or for `stop` to turn true, which ends it: its
    /// exit code, or why it could not be started.
    fn run(&self, stop: watch::Receiver<bool>) -> BoxFuture<'_, Result<i32, String>>;
}

async fn stopped(mut stop: watch::Receiver<bool>) {
    if stop.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Runs `child` again and again as the module's doc says, until `stop` turns true (then 0) or the
/// helper stops by itself (0 or 78: its code).
pub async fn supervise(
    child: &dyn Child,
    stop: watch::Receiver<bool>,
    policy: &Policy,
    log: &Log,
) -> u8 {
    let mut backoff = policy.first;
    let mut handoffs: Vec<Instant> = Vec::new();
    loop {
        if *stop.borrow() {
            return 0;
        }
        let started = Instant::now();
        let result = child.run(stop.clone()).await;
        if *stop.borrow() {
            log.info("stopped, as asked");
            return 0;
        }
        let ran = started.elapsed();
        if ran >= policy.healthy_after {
            backoff = policy.first;
        }
        let crash = |backoff: &mut Duration, why: String| {
            let wait = *backoff;
            *backoff = (*backoff * 2).min(policy.max);
            log.warn(format!(
                "{why} after {:.0} s: starting it again in {:.0} s",
                ran.as_secs_f64(),
                wait.as_secs_f64()
            ));
            wait
        };
        let wait = match result {
            Ok(0) => {
                log.info("the helper stopped (exit 0): the supervisor stops too");
                return 0;
            }
            Ok(EXIT_REFUSED) => {
                log.error("the helper was refused for good (exit 78): the supervisor stops; see helper.log");
                return 78;
            }
            Ok(EXIT_UPDATE) => {
                let now = Instant::now();
                handoffs.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                handoffs.push(now);
                if handoffs.len() > policy.handoffs_per_minute {
                    crash(
                        &mut backoff,
                        format!("the helper exited 75 {} times in a minute", handoffs.len()),
                    )
                } else {
                    log.info("the helper exited 75 (an update): starting the binary in place now");
                    Duration::ZERO
                }
            }
            Ok(code) => crash(&mut backoff, format!("the helper exited with code {code}")),
            Err(why) => crash(&mut backoff, format!("the helper could not be started ({why})")),
        };
        if !wait.is_zero() {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = stopped(stop.clone()) => {
                    log.info("stopped, as asked");
                    return 0;
                }
            }
        }
    }
}

/// The supervisor's record: `helper/supervisor.json` in the home.
#[must_use]
pub fn pid_file(home: &Path) -> PathBuf {
    Home::at(home).state_dir().join(PID_FILE)
}

/// The stop request: `helper/supervisor.stop` in the home.
#[must_use]
pub fn stop_file(home: &Path) -> PathBuf {
    Home::at(home).state_dir().join(STOP_FILE)
}

/// A supervisor's record, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub pid: u32,
    pub child: Option<u32>,
    pub program: String,
}

/// The home's record, when there is one that reads.
#[must_use]
pub fn read_record(home: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(pid_file(home)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Record {
        pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
        child: v.get("child").and_then(Value::as_u64).and_then(|c| u32::try_from(c).ok()),
        program: v.get("program").and_then(Value::as_str).unwrap_or_default().to_owned(),
    })
}

fn write_record(home: &Path, r: &Record) {
    let path = pid_file(home);
    let tmp = path.with_extension("json.tmp");
    let text = json!({"pid": r.pid, "child": r.child, "program": r.program}).to_string();
    if std::fs::write(&tmp, text).is_ok() {
        let _moved = std::fs::rename(&tmp, &path);
    }
}

/// Whether the process `pid` runs, and is a CITAR helper (its program's name starts so: a process
/// id the system gave to something else since is not one). Any CITAR helper: what the supervisor
/// of a home is, or a helper serving it, is told by [`supervises_home`] and [`serves_home`].
#[must_use]
pub fn alive(pid: u32) -> bool {
    let sys_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
    sys.process(sys_pid)
        .is_some_and(|p| p.name().to_string_lossy().to_lowercase().starts_with("citar-helper"))
}

/// Two homes as the same folder: equal paths, and on Windows, whose paths ignore case, equal
/// once lowered.
fn same_home(a: &Path, b: &Path) -> bool {
    a == b
        || (cfg!(windows)
            && Path::new(&a.to_string_lossy().to_lowercase())
                == Path::new(&b.to_string_lossy().to_lowercase()))
}

/// Whether a command line names the home `home`: `--home HOME` or `--home=HOME`.
fn names_home(cmd: &[OsString], home: &Path) -> bool {
    let mut args = cmd.iter();
    while let Some(a) = args.next() {
        let named = if a == "--home" {
            args.next().map(PathBuf::from)
        } else {
            a.to_str().and_then(|t| t.strip_prefix("--home=")).map(PathBuf::from)
        };
        if named.is_some_and(|n| same_home(&n, home)) {
            return true;
        }
    }
    false
}

/// The command line of the CITAR helper `pid`, when it runs and is one.
fn helper_cmd(pid: u32) -> Option<Vec<OsString>> {
    let sys_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[sys_pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
    );
    let p = sys.process(sys_pid)?;
    p.name().to_string_lossy().to_lowercase().starts_with("citar-helper").then(|| p.cmd().to_vec())
}

/// Whether `pid` is a helper in service mode for the home `home`: its command line names both, as
/// the supervisor of that home starts it (a process id the system has given to another program, or
/// to a helper of another home, since its supervisor ended is not one).
#[must_use]
pub fn serves_home(pid: u32, home: &Path) -> bool {
    helper_cmd(pid).is_some_and(|cmd| {
        cmd.iter().any(|a| a == "--service")
            && !cmd.iter().any(|a| a == SUPERVISOR_FLAG)
            && names_home(&cmd, home)
    })
}

/// Whether `pid` is the Run supervisor of the home `home`: a CITAR helper whose command line has
/// [`SUPERVISOR_FLAG`] and names the home. A record left by a supervisor that was ended (at
/// sign-out, or from Task Manager) may name a process id the system has since given to another
/// CITAR helper (a console helper of another home, the Windows service, another supervisor's
/// helper): that one is no supervisor of this home, so it neither keeps a new one from starting
/// nor is ended by `uninstall`.
#[must_use]
pub fn supervises_home(pid: u32, home: &Path) -> bool {
    helper_cmd(pid)
        .is_some_and(|cmd| cmd.iter().any(|a| a == SUPERVISOR_FLAG) && names_home(&cmd, home))
}

/// Ends the process `pid` at once, if it is a CITAR helper.
fn kill(pid: u32) {
    let sys_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
    if let Some(p) = sys.process(sys_pid)
        && p.name().to_string_lossy().to_lowercase().starts_with("citar-helper")
    {
        let _killed = p.kill();
    }
}

/// How long `uninstall` waits for a supervisor to stop by itself.
pub const STOP_WAIT: Duration = Duration::from_secs(20);

/// Stops the supervisor this home's record names, if it runs (it is this home's supervisor by its
/// command line, [`supervises_home`]): the stop file written, then, past [`STOP_WAIT`], the
/// supervisor and its helper ended. A record whose supervisor is gone is removed, and a helper it
/// left serving this home ended ([`serves_home`]); a process id the record names that the system
/// has given to anything else is left alone.
///
/// # Errors
/// The stop file could not be written.
pub fn stop_running(home: &Path, log: &Log) -> Result<(), String> {
    let Some(r) = read_record(home) else { return Ok(()) };
    let left_child = || {
        if let Some(c) = read_record(home).and_then(|r| r.child).filter(|c| serves_home(*c, home)) {
            kill(c);
        }
    };
    if !supervises_home(r.pid, home) {
        left_child();
        let _gone = std::fs::remove_file(pid_file(home));
        return Ok(());
    }
    let stop = stop_file(home);
    std::fs::write(&stop, b"stop\n").map_err(|e| format!("{}: {e}", stop.display()))?;
    let until = std::time::Instant::now() + STOP_WAIT;
    while supervises_home(r.pid, home) && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
    if supervises_home(r.pid, home) {
        log.warn(format!(
            "the supervisor (pid {}) did not stop within {} s: ending it",
            r.pid,
            STOP_WAIT.as_secs()
        ));
        kill(r.pid);
        left_child();
    }
    let _gone = std::fs::remove_file(pid_file(home));
    let _gone = std::fs::remove_file(&stop);
    Ok(())
}

/// The helper as a process: `program` with `args`, no window, its output discarded (it logs to its
/// own file); its process id kept in the home's record.
pub struct ProcessChild {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub home: PathBuf,
}

/// A child's exit as an exit code (a signal's death as 128 plus the signal on Unix, 1 elsewhere).
fn code_of(status: std::process::ExitStatus) -> i32 {
    if let Some(c) = status.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

impl Child for ProcessChild {
    fn run(&self, stop: watch::Receiver<bool>) -> BoxFuture<'_, Result<i32, String>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(&self.program);
            cmd.args(&self.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            // CREATE_NO_WINDOW: the helper, a console program, gets a console nobody sees.
            cmd.creation_flags(0x0800_0000);
            let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.program.display()))?;
            let pid = child.id();
            if let Some(mut r) = read_record(&self.home) {
                r.child = pid;
                write_record(&self.home, &r);
            }
            tokio::select! {
                status = child.wait() => status.map(code_of).map_err(|e| e.to_string()),
                () = stopped(stop) => {
                    end(&mut child).await;
                    Ok(0)
                }
            }
        })
    }
}

/// Ends the helper: on Unix `SIGTERM`, then after 10 s a kill; on Windows at once.
async fn end(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id()
        && citar_jobs::sys::terminate(pid).is_ok()
        && tokio::time::timeout(Duration::from_secs(10), child.wait()).await.is_ok()
    {
        return;
    }
    let _killed = child.start_kill();
    let _waited = child.wait().await;
}

/// The command line the supervisor gives the helper: its own, less `--detach` and
/// `--supervisor`, with `--service`.
#[must_use]
pub fn child_args(own: &[OsString]) -> Vec<OsString> {
    let mut args: Vec<OsString> =
        own.iter().filter(|a| *a != "--detach" && *a != SUPERVISOR_FLAG).cloned().collect();
    if !args.iter().any(|a| a == "--service") {
        args.push(OsString::from("--service"));
    }
    args
}

/// A development or test build's shorter first wait after a crash
/// (`CITAR_HELPER_TEST_SUPERVISOR_FIRST_S`), for the tests of the real binary; none in a release
/// build.
fn test_policy() -> Policy {
    let mut p = Policy::default();
    if crate::app::DEV_GUARD
        && let Some(s) = std::env::var("CITAR_HELPER_TEST_SUPERVISOR_FIRST_S")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0 && *s < 3600.0)
    {
        p.first = Duration::from_secs_f64(s);
    }
    p
}

/// `citar-helper run --supervisor` (internal; what `--detach` starts): one supervisor per home,
/// recorded in it, supervising this binary in service mode until stopped. Returns its exit code.
#[must_use]
pub fn main(home: &Home, log: &Log) -> u8 {
    let _file = log.to_named_file(&home.log_dir(), LOG_FILE);
    let root = home.root().to_path_buf();
    let me = std::process::id();
    if let Some(r) = read_record(&root)
        && r.pid != me
        && supervises_home(r.pid, &root)
    {
        log.info(format!("a supervisor already runs for this home (pid {}): leaving it be", r.pid));
        return 0;
    }
    // A helper whose supervisor was ended (by Task Manager, say) runs on alone: a second one with
    // the same token would take the machine's connection from it, and each would be told it was
    // replaced in turn. It goes first.
    if let Some(orphan) =
        read_record(&root).and_then(|r| r.child).filter(|c| serves_home(*c, &root))
    {
        log.warn(format!(
            "ending the helper (pid {orphan}) an earlier supervisor of this home left running"
        ));
        kill(orphan);
    }
    let program = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            log.error(format!("the supervisor cannot find its own binary: {e}"));
            return 1;
        }
    };
    let _made = std::fs::create_dir_all(home.state_dir());
    let _stale = std::fs::remove_file(stop_file(&root));
    write_record(&root, &Record { pid: me, child: None, program: program.display().to_string() });
    let own: Vec<OsString> = std::env::args_os().skip(1).collect();
    let child =
        ProcessChild { program: program.clone(), args: child_args(&own), home: root.clone() };
    log.info(format!(
        "supervising {} for the home {} (pid {me})",
        program.display(),
        root.display()
    ));
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            log.error(format!("the supervisor could not start: {e}"));
            return 1;
        }
    };
    let code = rt.block_on(async {
        let (tx, rx) = watch::channel(false);
        tokio::spawn(watch_stop_file(stop_file(&root), tx.clone()));
        tokio::spawn(crate::app::stop_on_signal(tx, log.clone()));
        supervise(&child, rx, &test_policy(), log).await
    });
    if read_record(&root).is_some_and(|r| r.pid == me) {
        let _gone = std::fs::remove_file(pid_file(&root));
    }
    let _gone = std::fs::remove_file(stop_file(&root));
    code
}

/// Turns `tx` true once `path` exists (looked for every second).
async fn watch_stop_file(path: PathBuf, tx: watch::Sender<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let _sent = tx.send(true);
            return;
        }
    }
}

/// `citar-helper run --detach`: starts this binary's supervisor (`run --supervisor`, with this
/// run's other arguments) as a process of its own, with no window (Windows) or in a process group
/// of its own (Unix), and returns at once.
///
/// # Errors
/// The supervisor could not be started.
pub fn detach() -> Result<u32, String> {
    let program = std::env::current_exe().map_err(|e| format!("this binary's path: {e}"))?;
    let own: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut args: Vec<OsString> = own.into_iter().filter(|a| a != "--detach").collect();
    args.push(OsString::from(SUPERVISOR_FLAG));
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console, and none of its signals.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
        // The supervisor outlives this process: it must hold no copy of this process's own output
        // (a pipe a script reads would stay open for as long as the supervisor runs).
        citar_jobs::sys::keep_std_handles_to_self();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A child that ends with the codes given, each after the time given, and notes when each
    /// started.
    struct Scripted {
        ends: Mutex<Vec<(f64, i32)>>,
        starts: Mutex<Vec<f64>>,
        t0: Instant,
    }

    impl Child for Scripted {
        fn run(&self, stop: watch::Receiver<bool>) -> BoxFuture<'_, Result<i32, String>> {
            Box::pin(async move {
                self.starts.lock().expect("lock").push(self.t0.elapsed().as_secs_f64());
                let next = {
                    let mut e = self.ends.lock().expect("lock");
                    if e.is_empty() { None } else { Some(e.remove(0)) }
                };
                let Some((after, code)) = next else {
                    stopped(stop).await;
                    return Ok(0);
                };
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs_f64(after)) => Ok(code),
                    () = stopped(stop) => Ok(0),
                }
            })
        }
    }

    fn scripted(ends: &[(f64, i32)]) -> Scripted {
        Scripted {
            ends: Mutex::new(ends.to_vec()),
            starts: Mutex::new(Vec::new()),
            t0: Instant::now(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_handoff_restarts_at_once_a_crash_after_a_backoff_and_zero_stops() {
        let child = scripted(&[(5.0, 75), (5.0, 1), (5.0, 2), (5.0, 1), (5.0, 0)]);
        let (_tx, rx) = watch::channel(false);
        let log = Log::memory();
        let code = supervise(&child, rx, &Policy::default(), &log).await;
        assert_eq!(code, 0);
        // 75 at 5 s: at once; 1 at 10: 15 s; 2 at 30: 30 s; 1 at 65: 60 s; 0 at 130.
        let starts = child.starts.lock().expect("lock").clone();
        assert_eq!(starts, [0.0, 5.0, 25.0, 60.0, 125.0], "{:?}", log.lines());
        assert!(log.contains("the helper exited 75 (an update)"), "{:?}", log.lines());
        assert!(log.contains("starting it again in 15 s"), "{:?}", log.lines());
        assert!(log.contains("the supervisor stops too"), "{:?}", log.lines());
    }

    #[tokio::test(start_paused = true)]
    async fn the_backoff_doubles_to_ten_minutes_and_a_long_run_resets_it() {
        let mut ends: Vec<(f64, i32)> = vec![(1.0, 1); 8];
        ends.push((700.0, 1)); // a helper that ran past ten minutes, then crashed
        ends.push((1.0, 0));
        let child = scripted(&ends);
        let (_tx, rx) = watch::channel(false);
        let code = supervise(&child, rx, &Policy::default(), &Log::memory()).await;
        assert_eq!(code, 0);
        let starts = child.starts.lock().expect("lock").clone();
        let waits: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        // each run 1 s, then 15, 30, 60, 120, 240, 480, 600, 600; the long run, then 15 again
        assert_eq!(
            waits,
            [16.0, 31.0, 61.0, 121.0, 241.0, 481.0, 601.0, 601.0, 715.0],
            "{starts:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn too_many_handoffs_in_a_minute_are_a_crash_and_78_stops() {
        let child = scripted(&[
            (1.0, 75),
            (1.0, 75),
            (1.0, 75),
            (1.0, 75),
            (1.0, 75),
            (1.0, 75),
            (1.0, 78),
        ]);
        let (_tx, rx) = watch::channel(false);
        let log = Log::memory();
        assert_eq!(supervise(&child, rx, &Policy::default(), &log).await, 78);
        let starts = child.starts.lock().expect("lock").clone();
        assert_eq!(starts, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 21.0], "{:?}", log.lines());
        assert!(log.contains("exited 75 6 times in a minute"), "{:?}", log.lines());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_ends_the_child_and_the_wait() {
        let child = scripted(&[(1.0, 3)]);
        let (tx, rx) = watch::channel(false);
        let log = Log::memory();
        let task = async { supervise(&child, rx, &Policy::default(), &log).await };
        let stopper = async {
            tokio::time::sleep(Duration::from_secs(5)).await; // in the 15 s wait after the crash
            tx.send(true).expect("sent");
        };
        let (code, ()) = tokio::join!(task, stopper);
        assert_eq!(code, 0);
        assert_eq!(child.starts.lock().expect("lock").len(), 1);
    }

    #[test]
    fn a_command_line_names_its_home_only_as_the_home() {
        let cmd = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        let home = Path::new("/srv/h");
        assert!(names_home(&cmd(&["run", "--home", "/srv/h", SUPERVISOR_FLAG]), home));
        assert!(names_home(&cmd(&["run", "--home=/srv/h/"]), home), "--home= and a trailing slash");
        assert!(!names_home(&cmd(&["run", "--home", "/srv/other"]), home), "another home");
        assert!(!names_home(&cmd(&["scan", "/srv/h"]), home), "the path, but not as the home");
        assert!(!names_home(&cmd(&["run", "--home"]), home), "no value");
        #[cfg(windows)]
        assert!(
            names_home(
                &cmd(&["run", "--home", r"C:\Users\A\CITAR"]),
                Path::new(r"c:\users\a\citar")
            ),
            "Windows' paths ignore case"
        );
    }

    #[test]
    fn the_child_runs_in_service_mode_without_the_supervisor_flags() {
        let own: Vec<OsString> = ["run", "--detach", "--home", "C:/h", SUPERVISOR_FLAG, "-v"]
            .iter()
            .map(OsString::from)
            .collect();
        let got: Vec<String> =
            child_args(&own).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(got, ["run", "--home", "C:/h", "-v", "--service"]);
    }
}
