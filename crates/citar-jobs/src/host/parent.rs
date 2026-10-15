//! The job host's parent side (design/phase4-helper.md P4.4.3, P4.3.6): what the helper runs its
//! host with (the server's is `citar/jobs/host.py`, the same rules in Python).
//!
//! - **Starting a host** ([`spawn`]): the program (the helper's own binary with `job-host`), its
//!   pipes, and on Linux, when the user's service manager answers ([`Wrapper::detect`]), under
//!   `systemd-run --user --scope` with the limits as the scope's properties ([`scope_args`]), so
//!   the kernel holds them; then the `limits` frame with the parent's jobs build, and the host's
//!   `ready`, whose jobs build must be the parent's: a host of another build (one left from before
//!   a binary swap) is ended, never used.
//! - **Its frames** ([`Host::next`]), and which runs are still in flight on it.
//! - **Its death** ([`Host::death`], [`classify`]): a memory death ends the whole host, and every
//!   run of it fails `memory` ([`Host::failures`]); it shows as the allocation-failure line on
//!   the host's standard error (`memory allocation of N bytes failed`: a Job Object's limit or
//!   `RLIMIT_DATA` refused a commit), as the `oom_kill` count of the scope's parent cgroup rising
//!   (the kernel killed it at the scope's `MemoryMax`; the parent's count, since the scope's own
//!   cgroup goes with it), or as the macOS watchdog's line or exit code. Any other death fails its
//!   runs `host`, and both are retried elsewhere.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{FailCode, Failed, Fatal, FatalCode, FromHost, LimitsFrame, Ready, ToHost};
use crate::limits::{EXIT_MEMORY, Requested, WATCHDOG_LINE, cgroup};
use crate::spec::clip;

/// How long a parent waits for a host's `ready` (its limits, the canary and the self-check) by
/// default.
pub const READY_WAIT: Duration = Duration::from_secs(120);
/// How many lines of a host's log a parent keeps, for the classification and the message.
pub const TAIL: usize = 64;
/// How long the user manager has to answer.
const PROBE_WAIT: Duration = Duration::from_secs(5);

/// How a host is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrapper {
    /// As itself.
    Direct,
    /// Under `systemd-run --user --scope` (Linux).
    Scope,
}

impl Wrapper {
    /// The scope where it is available (Linux, with a reachable user manager and `systemd-run`),
    /// else direct.
    #[must_use]
    pub fn detect() -> Self {
        if scope_available() { Self::Scope } else { Self::Direct }
    }
}

/// Runs `cmd` to its end within `wait`; its standard output when it succeeded.
fn answer(cmd: &mut Command, wait: Duration) -> Option<String> {
    let mut child =
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let until = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut text = String::new();
                let mut out = child.stdout.take()?;
                std::io::Read::read_to_string(&mut out, &mut text).ok()?;
                return status.success().then_some(text);
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _killed = child.kill();
                let _reaped = child.wait();
                return None;
            }
        }
    }
}

/// Whether this machine can start a host under a `systemd --user` scope: Linux, `systemd-run`
/// present, and the user's service manager answering.
#[must_use]
pub fn scope_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let manager = answer(
        Command::new("systemctl").args(["--user", "show", "-p", "Version", "--value"]),
        PROBE_WAIT,
    );
    manager.is_some_and(|v| !v.trim().is_empty())
        && answer(Command::new("systemd-run").arg("--version"), PROBE_WAIT).is_some()
}

/// `systemd-run`'s arguments for a host holding `req` on a machine of `cpus` logical CPUs:
/// `CPUQuota` the share of the whole machine (`cpu_percent x cpus`%), `MemoryMax` the memory
/// with no swap beside it (`MemorySwapMax=0`: swapped pages would let the scope past its memory),
/// and `TasksMax` the runs' threads and the host's own. `--collect` unloads the scope when it
/// ends, however it ends.
#[must_use]
pub fn scope_args(req: &Requested, cpus: u32) -> Vec<String> {
    let quota = u64::from(req.cpu_percent) * u64::from(cpus);
    vec![
        "--user".to_owned(),
        "--scope".to_owned(),
        "--quiet".to_owned(),
        "--collect".to_owned(),
        "-p".to_owned(),
        format!("CPUQuota={quota}%"),
        "-p".to_owned(),
        format!("MemoryMax={}M", req.memory_mb),
        "-p".to_owned(),
        "MemorySwapMax=0".to_owned(),
        "-p".to_owned(),
        format!("TasksMax={}", u64::from(req.threads) + 16),
        "--".to_owned(),
    ]
}

/// The program a parent runs as its host.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Variables added to the parent's environment.
    pub env: Vec<(OsString, OsString)>,
}

/// What a host said or did.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Frame(FromHost),
    /// A line that is not one of its frames.
    Garbled(String),
    /// Its standard output closed: it is ending or has ended ([`Host::death`]).
    Gone,
}

/// Why a host ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Death {
    /// The memory limit ended it, and what showed it.
    Memory(String),
    /// It refused to start.
    Refused(Fatal),
    /// Anything else: its exit and the last line of its log.
    Other(String),
}

impl std::fmt::Display for Death {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(why) => write!(f, "the memory limit ended the job host: {why}"),
            Self::Refused(fatal) => {
                write!(f, "the job host refused to start ({:?}): {}", fatal.code, fatal.message)
            }
            Self::Other(why) => write!(f, "the job host ended: {why}"),
        }
    }
}

/// Whether `line` is Rust's report of an allocation that failed.
#[must_use]
pub fn is_allocation_failure(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("memory allocation of ") && l.ends_with(" bytes failed")
}

/// Why a host ended, from its exit `status`, the last lines of its log, its scope parent's
/// `oom_kill` count before and after (Linux), and the `fatal` frame it sent, if any.
#[must_use]
pub fn classify(
    status: Option<ExitStatus>,
    tail: &[String],
    oom: Option<(u64, u64)>,
    fatal: Option<&Fatal>,
) -> Death {
    if let Some(line) = tail.iter().find(|l| is_allocation_failure(l)) {
        return Death::Memory(format!("an allocation failed: \"{}\"", line.trim()));
    }
    if let Some((before, after)) = oom
        && after > before
    {
        return Death::Memory(format!("the cgroup's oom_kill count rose from {before} to {after}"));
    }
    if let Some(line) = tail.iter().find(|l| l.contains(WATCHDOG_LINE)) {
        return Death::Memory(format!("its watchdog: \"{}\"", line.trim()));
    }
    if status.and_then(|s| s.code()) == Some(EXIT_MEMORY) && fatal.is_none() {
        return Death::Memory(format!("its exit code {EXIT_MEMORY} (the memory watchdog's)"));
    }
    if let Some(f) = fatal {
        return Death::Refused(f.clone());
    }
    let exit =
        status.map_or_else(|| "it did not exit, and was ended".to_owned(), |s| s.to_string());
    let last = tail.last().map(|l| format!("; its log's last line: {}", clip(l.trim(), 300)));
    Death::Other(format!("{exit}{}", last.unwrap_or_default()))
}

/// Why a host could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("the job host could not be started: {0}")]
    Start(#[source] std::io::Error),
    #[error("the job host refused to start ({:?}): {}", .0.code, .0.message)]
    Refused(Fatal),
    #[error("the job host's jobs build is {got}, not this build's {want}")]
    Build { want: String, got: String },
    #[error("the job host ended before it was ready: {0}")]
    Died(Death),
    #[error("the job host was not ready within {0:?}")]
    Timeout(Duration),
    #[error("the job host sent something else before `ready`: {0}")]
    Protocol(String),
}

impl SpawnError {
    /// Whether the host could not hold the limits (or not live under them): a parent's
    /// `job_reject` `limits`.
    #[must_use]
    pub const fn is_limits(&self) -> bool {
        matches!(
            self,
            Self::Refused(Fatal { code: FatalCode::Limits, .. }) | Self::Died(Death::Memory(_))
        )
    }
}

/// A running host.
#[derive(Debug)]
pub struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    tail: Arc<Mutex<VecDeque<String>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    ready: Ready,
    /// The scope's parent cgroup and its `oom_kill` count at `ready` (Linux).
    oom: Option<(String, u64)>,
    in_flight: BTreeSet<String>,
    fatal: Option<Fatal>,
    /// The thread reading its log, which ends when the log closes.
    log_reader: Option<std::thread::JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts `program` as a job host holding `limits`, as `wrapper` says, and waits up to
/// `ready_wait` for its `ready`, which must name `jobs_build`.
///
/// # Errors
/// It could not be started, refused, died, was not ready in time, or is of another jobs build:
/// in every case it is ended first.
pub fn spawn(
    program: &Program,
    wrapper: Wrapper,
    limits: &Requested,
    jobs_build: &str,
    ready_wait: Duration,
) -> Result<Host, SpawnError> {
    let mut cmd = match wrapper {
        Wrapper::Direct => Command::new(&program.program),
        Wrapper::Scope => {
            let mut c = Command::new("systemd-run");
            c.args(scope_args(limits, crate::limits::machine_cpus())).arg(&program.program);
            c
        }
    };
    cmd.args(&program.args)
        .envs(program.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: a helper without a console starts no console window for its host.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(SpawnError::Start)?;
    let (tx, events) = channel();
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let log = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        let _reader =
            std::thread::Builder::new().name("citar-host-frames".to_owned()).spawn(move || {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event = FromHost::read(&line)
                        .map_or_else(|_| Event::Garbled(clip(&line, 300)), Event::Frame);
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                let _parent_gone = tx.send(Event::Gone);
            });
    }
    let mut log_reader = None;
    if let Some(err) = child.stderr.take() {
        let (tail, log) = (Arc::clone(&tail), Arc::clone(&log));
        log_reader = std::thread::Builder::new()
            .name("citar-host-log".to_owned())
            .spawn(move || {
                for line in BufReader::new(err).lines() {
                    let Ok(line) = line else { break };
                    for (keep, at_most) in [(&tail, TAIL), (&log, 1000)] {
                        let mut q = lock(keep);
                        if q.len() >= at_most {
                            q.pop_front();
                        }
                        q.push_back(clip(&line, 2000));
                    }
                }
            })
            .ok();
    }
    drop(tx);
    let mut host = Host {
        child,
        stdin: None,
        events,
        tail,
        log,
        ready: Ready {
            limits: crate::limits::Held {
                cpu: crate::limits::Hardness::Soft,
                memory: crate::limits::Hardness::Soft,
            },
            verifiable: false,
            jobs_build: String::new(),
            threads: 0,
            reason: None,
            cgroup: None,
            cpu_s: 0.0,
            pid: 0,
            counting: false,
        },
        oom: None,
        in_flight: BTreeSet::new(),
        fatal: None,
        log_reader,
    };
    host.stdin = host.child.stdin.take();
    let first = ToHost::Limits(LimitsFrame {
        jobs_build: jobs_build.to_owned(),
        threads: limits.threads,
        cpu_percent: limits.cpu_percent,
        memory_mb: limits.memory_mb,
    });
    // A host that is gone already shows below, as its death.
    let _sent = host.send(&first);
    match host.events.recv_timeout(ready_wait) {
        Ok(Event::Frame(FromHost::Ready(r))) => {
            if r.jobs_build != jobs_build {
                let got = r.jobs_build;
                host.end(Duration::ZERO);
                return Err(SpawnError::Build { want: jobs_build.to_owned(), got });
            }
            host.oom = r.cgroup.as_deref().and_then(|c| {
                let parent = c.rsplit_once('/').map(|(p, _)| p.to_owned())?;
                let n = cgroup::oom_kills(&parent)?;
                Some((parent, n))
            });
            host.ready = r;
            Ok(host)
        }
        Ok(Event::Frame(FromHost::Fatal(f))) => {
            host.fatal = Some(f.clone());
            host.end(Duration::from_secs(2));
            Err(SpawnError::Refused(f))
        }
        Ok(Event::Gone) | Err(RecvTimeoutError::Disconnected) => {
            Err(SpawnError::Died(host.death(Duration::from_secs(5))))
        }
        Ok(Event::Frame(other)) => {
            host.end(Duration::ZERO);
            Err(SpawnError::Protocol(other.t().to_owned()))
        }
        Ok(Event::Garbled(line)) => {
            host.end(Duration::ZERO);
            Err(SpawnError::Protocol(line))
        }
        Err(RecvTimeoutError::Timeout) => {
            host.end(Duration::ZERO);
            Err(SpawnError::Timeout(ready_wait))
        }
    }
}

impl Host {
    /// Its `ready`.
    #[must_use]
    pub const fn ready(&self) -> &Ready {
        &self.ready
    }

    /// Its process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The runs offered to it that have not ended.
    #[must_use]
    pub const fn in_flight(&self) -> &BTreeSet<String> {
        &self.in_flight
    }

    /// Sends a frame.
    ///
    /// # Errors
    /// Its standard input is closed (it is ending).
    pub fn send(&mut self, frame: &ToHost) -> std::io::Result<()> {
        if let ToHost::JobOffer(o) = frame {
            self.in_flight.insert(o.run.clone());
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| std::io::Error::other("closed"))?;
        let mut line = frame.line();
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// The next thing it said or did, waiting up to `wait`.
    pub fn next(&mut self, wait: Duration) -> Option<Event> {
        let event = match self.events.recv_timeout(wait) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => Event::Gone,
        };
        if let Event::Frame(f) = &event {
            match f {
                FromHost::JobReject(_)
                | FromHost::JobResult(_)
                | FromHost::JobFailed(_)
                | FromHost::JobAborted(_) => {
                    if let Some(run) = f.run() {
                        self.in_flight.remove(run);
                    }
                }
                FromHost::Fatal(fatal) => self.fatal = Some(fatal.clone()),
                _ => {}
            }
        }
        Some(event)
    }

    /// Its log's lines since the last call (at most the last 1,000).
    #[must_use]
    pub fn take_log(&self) -> Vec<String> {
        lock(&self.log).drain(..).collect()
    }

    /// Closes its standard input (it ends its runs and exits within two seconds), waits up to
    /// `wait` for it to exit, and ends it if it has not.
    fn end(&mut self, wait: Duration) -> Option<ExitStatus> {
        drop(self.stdin.take());
        let until = Instant::now() + wait;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _killed = self.child.kill();
                    return self.child.wait().ok();
                }
            }
        }
    }

    /// Why it ended, once its output closed ([`Event::Gone`]): it is waited for up to `wait`
    /// (and ended past it), and its death classified ([`classify`]).
    pub fn death(&mut self, wait: Duration) -> Death {
        let status = self.end(wait);
        // Its log's last lines arrive as its log closes: wait for the reader to reach the end (a
        // second at most, in case something it started holds the pipe open).
        let until = Instant::now() + Duration::from_secs(1);
        while self.log_reader.as_ref().is_some_and(|r| !r.is_finished()) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let tail: Vec<String> = lock(&self.tail).iter().cloned().collect();
        let oom = self
            .oom
            .as_ref()
            .and_then(|(parent, before)| cgroup::oom_kills(parent).map(|after| (*before, after)));
        classify(status, &tail, oom, self.fatal.as_ref())
    }

    /// A `job_failed` for each run still in flight on a host that died of `death`: `memory` for
    /// a memory death, `host` otherwise; each retryable.
    #[must_use]
    pub fn failures(&self, death: &Death) -> Vec<FromHost> {
        let code =
            if matches!(death, Death::Memory(_)) { FailCode::Memory } else { FailCode::Host };
        self.in_flight
            .iter()
            .map(|run| {
                FromHost::JobFailed(Failed {
                    run: run.clone(),
                    code,
                    message: death.to_string(),
                    retryable: true,
                })
            })
            .collect()
    }

    /// Closes it: its runs are aborted and it exits within two seconds (ended if not); its exit.
    pub fn close(mut self) -> Option<ExitStatus> {
        self.end(Duration::from_secs(3))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.end(Duration::from_secs(3));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_holds_the_limits_as_properties() {
        let req = Requested { threads: 2, cpu_percent: 10, memory_mb: 200 };
        let args = scope_args(&req, 12);
        let at = |p: &str| args.iter().any(|a| a == p);
        assert!(at("--user") && at("--scope") && at("--collect") && at("--quiet"));
        assert!(at("CPUQuota=120%") && at("MemoryMax=200M"), "{args:?}");
        assert!(at("MemorySwapMax=0") && at("TasksMax=18"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn deaths_are_classified_from_what_shows_them() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let alloc = lines(&["citar job host: ready", "memory allocation of 8388608 bytes failed"]);
        assert!(matches!(classify(None, &alloc, None, None), Death::Memory(_)));
        assert!(matches!(classify(None, &[], Some((2, 3)), None), Death::Memory(_)));
        assert!(matches!(classify(None, &[], Some((3, 3)), None), Death::Other(_)));
        let watch = lines(&[&format!("{WATCHDOG_LINE}: 300 MB of 200 MB")]);
        assert!(matches!(classify(None, &watch, None, None), Death::Memory(_)));
        // A game's panic quoting the words is not an allocation that failed.
        let panic = lines(&["thread panicked: memory allocation of 8 bytes failed, said the bot"]);
        assert!(matches!(classify(None, &panic, None, None), Death::Other(_)));
        let fatal = Fatal { code: FatalCode::Build, message: "another build".to_owned() };
        assert_eq!(classify(None, &[], None, Some(&fatal)), Death::Refused(fatal));
        let other = classify(None, &lines(&["one", "two"]), None, None);
        assert!(matches!(&other, Death::Other(why) if why.ends_with("two")), "{other:?}");
    }

    #[test]
    fn no_scope_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(!scope_available());
            assert_eq!(Wrapper::detect(), Wrapper::Direct);
        }
    }
}
