//! The job host (design/phase4-helper.md P4.4.3): the one program that plays jobs under limits,
//! for both parents, the helper (`citar-helper job-host`) and the server (`python -m citar.jobs
//! host`). It replaces 0.1.5's lab runner, its orphan adoption and its `taskkill`.
//!
//! **Its protocol** is JSON lines: one object a line, `t` its type, on its standard input (from
//! the parent) and its standard output (to the parent); its standard error is its log, which the
//! parent reads too ([`parent`]). They are the job frames of protocol 2 (PROTOCOL.md "Jobs")
//! without what only the server and the helper share (the offer's time to live, the claim, the
//! signature, the acknowledgement), and these:
//! - **`limits`** (parent, first and once): `{jobs_build, threads, cpu_percent, memory_mb}`. A
//!   host whose own jobs build differs refuses to start (`fatal` `build`), so a host left from
//!   before a binary swap never computes with the old code; so does one whose limits the OS will
//!   not put in place (`fatal` `limits`; [`crate::limits`]).
//! - **`ready`** (host, once): `{limits: {cpu, memory}, verifiable, jobs_build, threads, reason?,
//!   cgroup, cpu_s, pid, counting, notes}`: how hard each limit holds; whether the canary and the
//!   self-check passed (`reason` says what failed: the host then refuses every job,
//!   `unverifiable`); its jobs build, which the parent holds to its own; its cgroup (Linux), whose
//!   memory events the parent reads when it dies; its CPU seconds so far; whether its memory is
//!   counted ([`crate::sys::alloc`]); how its limits are held, a line each, for the log.
//! - **`fatal`** (host, in place of `ready`): `{code: build | limits | protocol, message}`, then it
//!   exits with [`EXIT_REFUSED`].
//! - **`job_offer`** (parent): `{run, job?, kind, spec, spec_hash, jobs_build, deadline_s, mem_mb,
//!   priority?}`; answered `job_accept {run}` or `job_reject {run, reason, message}`, `reason` one
//!   of `unverifiable`, `build`, `bad_spec`, `memory` (`mem_mb` above `memory_mb / threads`) and
//!   `busy` (`threads` runs already), checked in that order.
//! - **`job_progress`** (host): `{run, round, of, elapsed_s, cpu_s, paused_s, peak_mb, paused}`,
//!   every [`PROGRESS_EVERY`] or [`PROGRESS_ROUNDS`] rounds, and when the run is held by a pause
//!   and let go again.
//! - **`job_result`** (host): `{run, outcome: {result, result_hash, chain, cpu_s, wall_s,
//!   paused_s, peak_mb}}`: a crash is a result (P4.4.2), whose trace goes to the log.
//! - **`job_failed`** (host): `{run, code: deadline | internal, message, retryable}`; the parent
//!   adds `memory` and `host` for the runs of a host that died.
//! - **`job_abort`** (parent) `{run, reason?}`, answered `job_aborted {run}`.
//! - **`pause`**, **`resume`** (parent): hold every run at its next seat step, and let them go.
//! - **`heartbeat`** (host): `{running, paused, cpu_s}`, every [`HEARTBEAT_EVERY`] and whenever a
//!   run starts or ends.
//!
//! An unknown type is ignored (logged once); a frame that does not read is logged, and an offer
//! whose `run` reads is refused `bad_spec`. A line is at most [`MAX_LINE`] bytes.
//!
//! **Inside:** at most `threads` runs, each on a thread of its own with its own game, its memory
//! counted apart (a [`crate::sys::alloc::Tally`]); between seat steps each run checks its abort
//! flag, the pause and its deadline, so each takes effect within one seat step. **A deadline**
//! is active time (paused time not counted), and a run paused so long that it has lasted twice its
//! deadline in all ends too: `job_failed` `deadline`.
//!
//! **Verifiable or not, at start:** after the limits, the canary and the self-check
//! ([`crate::canary`]) run on a thread made as run threads are (FTZ and DAZ are per thread), and
//! each run's thread runs the canary again before it plays.
//!
//! **Ending:** standard input closing (the parent died, or let it go) aborts every run, and the
//! host returns within [`EOF_GRACE`] whether or not they stopped; its parent's program then exits,
//! taking any run still in a seat step with it. No game outlives its parent.

pub mod parent;
mod run;
#[cfg(feature = "test-jobs")]
pub mod test_jobs;

pub use self::parent::spawn;

use std::collections::BTreeMap;
use std::io::{BufRead, Read as _, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use citar_engine::rules::Ruleset;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canary::{Inject, SelfCheck, canary_injected, self_check};
use crate::hash::Chain;
use crate::limits::{self, Held, Requested};
use crate::spec::{Checked, Kind, check_spec, clip};
use crate::sys::alloc;

/// How often the host says it is alive.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// How often a run reports its progress, at least.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);
/// How many rounds a run plays between reports, at most.
pub const PROGRESS_ROUNDS: i32 = 25;
/// The longest line either side writes: protocol 2's frame cap.
pub const MAX_LINE: usize = 8 << 20;
/// How long the host waits for its runs to stop once its standard input closes.
pub const EOF_GRACE: Duration = Duration::from_millis(1500);
/// The exit code of a host that refused to start (after its `fatal` frame).
pub const EXIT_REFUSED: i32 = 2;
/// The stack of a run's thread: a game's deepest call chains with room to spare, as citar-sim's
/// baseline workers have.
pub const RUN_STACK: usize = 16 << 20;
/// The shortest and longest deadline an offer may carry, in seconds (the protocol's bounds).
pub const DEADLINE_S: (f64, f64) = (1.0, 86_400.0);
/// The longest message the host quotes from what it was sent.
const QUOTE: usize = 200;

/// `limits`: the parent's jobs build and the limits to hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitsFrame {
    pub jobs_build: String,
    pub threads: u32,
    pub cpu_percent: u32,
    pub memory_mb: u64,
}

impl LimitsFrame {
    /// The limits asked for.
    #[must_use]
    pub const fn requested(&self) -> Requested {
        Requested {
            threads: self.threads,
            cpu_percent: self.cpu_percent,
            memory_mb: self.memory_mb,
        }
    }
}

/// `job_offer`: a run of a job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    pub kind: String,
    pub spec: Value,
    pub spec_hash: String,
    pub jobs_build: String,
    pub deadline_s: f64,
    #[serde(default)]
    pub mem_mb: u64,
    #[serde(default)]
    pub priority: i64,
}

/// `job_abort`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Abort {
    pub run: String,
    #[serde(default)]
    pub reason: String,
}

/// What the parent sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ToHost {
    Limits(LimitsFrame),
    JobOffer(Offer),
    JobAbort(Abort),
    Pause,
    Resume,
}

impl ToHost {
    /// The frame as one line of JSON (no newline).
    #[must_use]
    pub fn line(&self) -> String {
        let (t, body) = match self {
            Self::Limits(f) => ("limits", serde_json::to_value(f)),
            Self::JobOffer(f) => ("job_offer", serde_json::to_value(f)),
            Self::JobAbort(f) => ("job_abort", serde_json::to_value(f)),
            Self::Pause => ("pause", Ok(Value::Object(serde_json::Map::new()))),
            Self::Resume => ("resume", Ok(Value::Object(serde_json::Map::new()))),
        };
        tagged(t, body.unwrap_or(Value::Null))
    }
}

/// `{"t": t, ...body}` as text.
fn tagged(t: &str, body: Value) -> String {
    let mut m = match body {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert("t".to_owned(), Value::String(t.to_owned()));
    Value::Object(m).to_string()
}

/// Why a host refused to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FatalCode {
    /// The parent's jobs build is not this host's.
    Build,
    /// The limits are out of their bounds, or the OS would not put them in place.
    Limits,
    /// The first line was not a `limits` frame.
    Protocol,
}

/// `fatal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fatal {
    pub code: FatalCode,
    pub message: String,
}

/// `ready`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    pub limits: Held,
    pub verifiable: bool,
    pub jobs_build: String,
    pub threads: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub cgroup: Option<String>,
    pub cpu_s: f64,
    pub pid: u32,
    pub counting: bool,
    /// How the limits are held, one line each (a Job Object, a cgroup, `nice`, `RLIMIT_DATA`, the
    /// background band, the watchdog): for the parent's log.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Why the host turned an offer down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    Unverifiable,
    Build,
    BadSpec,
    Memory,
    Busy,
}

/// `job_reject`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reject {
    pub run: String,
    pub reason: RejectReason,
    pub message: String,
}

/// `job_accept`, `job_aborted`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    pub run: String,
}

/// `job_progress`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgressFrame {
    pub run: String,
    pub round: i64,
    pub of: i64,
    pub elapsed_s: f64,
    pub cpu_s: f64,
    pub paused_s: f64,
    pub peak_mb: f64,
    /// Whether the run is held by a pause now.
    #[serde(default)]
    pub paused: bool,
}

/// What a finished run produced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub result: Value,
    pub result_hash: String,
    pub chain: Chain,
    pub cpu_s: f64,
    pub wall_s: f64,
    pub paused_s: f64,
    pub peak_mb: f64,
}

/// `job_result`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultFrame {
    pub run: String,
    pub outcome: Outcome,
}

/// Why a run failed (the host's two; its parent adds `memory` and `host` for a host that died,
/// and the helper `aborted_locally`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailCode {
    Memory,
    Deadline,
    Host,
    Internal,
}

/// `job_failed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub run: String,
    pub code: FailCode,
    pub message: String,
    pub retryable: bool,
}

/// `heartbeat`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub running: u32,
    pub paused: bool,
    pub cpu_s: f64,
}

/// What the host sends.
#[derive(Clone, Debug, PartialEq)]
pub enum FromHost {
    Ready(Ready),
    Fatal(Fatal),
    JobAccept(RunRef),
    JobReject(Reject),
    JobProgress(ProgressFrame),
    JobResult(Box<ResultFrame>),
    JobFailed(Failed),
    JobAborted(RunRef),
    Heartbeat(Heartbeat),
}

impl FromHost {
    /// The frame's type.
    #[must_use]
    pub const fn t(&self) -> &'static str {
        match self {
            Self::Ready(_) => "ready",
            Self::Fatal(_) => "fatal",
            Self::JobAccept(_) => "job_accept",
            Self::JobReject(_) => "job_reject",
            Self::JobProgress(_) => "job_progress",
            Self::JobResult(_) => "job_result",
            Self::JobFailed(_) => "job_failed",
            Self::JobAborted(_) => "job_aborted",
            Self::Heartbeat(_) => "heartbeat",
        }
    }

    /// The run it is about, if any.
    #[must_use]
    pub fn run(&self) -> Option<&str> {
        match self {
            Self::JobAccept(r) | Self::JobAborted(r) => Some(&r.run),
            Self::JobReject(r) => Some(&r.run),
            Self::JobProgress(p) => Some(&p.run),
            Self::JobResult(r) => Some(&r.run),
            Self::JobFailed(f) => Some(&f.run),
            Self::Ready(_) | Self::Fatal(_) | Self::Heartbeat(_) => None,
        }
    }

    /// The frame as one line of JSON (no newline).
    #[must_use]
    pub fn line(&self) -> String {
        let body = match self {
            Self::Ready(f) => serde_json::to_value(f),
            Self::Fatal(f) => serde_json::to_value(f),
            Self::JobAccept(f) | Self::JobAborted(f) => serde_json::to_value(f),
            Self::JobReject(f) => serde_json::to_value(f),
            Self::JobProgress(f) => serde_json::to_value(f),
            Self::JobResult(f) => serde_json::to_value(f),
            Self::JobFailed(f) => serde_json::to_value(f),
            Self::Heartbeat(f) => serde_json::to_value(f),
        };
        tagged(self.t(), body.unwrap_or(Value::Null))
    }

    /// Reads a line the host wrote.
    ///
    /// # Errors
    /// Text that is not one of the host's frames.
    pub fn read(line: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
        let t = v.get("t").and_then(Value::as_str).ok_or("no type")?.to_owned();
        let e = |e: serde_json::Error| format!("{t}: {e}");
        Ok(match t.as_str() {
            "ready" => Self::Ready(Ready::deserialize(&v).map_err(e)?),
            "fatal" => Self::Fatal(Fatal::deserialize(&v).map_err(e)?),
            "job_accept" => Self::JobAccept(RunRef::deserialize(&v).map_err(e)?),
            "job_reject" => Self::JobReject(Reject::deserialize(&v).map_err(e)?),
            "job_progress" => Self::JobProgress(ProgressFrame::deserialize(&v).map_err(e)?),
            "job_result" => Self::JobResult(Box::new(ResultFrame::deserialize(&v).map_err(e)?)),
            "job_failed" => Self::JobFailed(Failed::deserialize(&v).map_err(e)?),
            "job_aborted" => Self::JobAborted(RunRef::deserialize(&v).map_err(e)?),
            "heartbeat" => Self::Heartbeat(Heartbeat::deserialize(&v).map_err(e)?),
            other => return Err(format!("unknown type {:?}", clip(other, 64))),
        })
    }
}

/// A line the parent sent, read.
#[derive(Debug)]
enum Received {
    Frame(ToHost),
    /// A type the host does not know.
    Unknown(String),
    /// A frame that does not read: why, and its run when that reads.
    Bad {
        why: String,
        offer_run: Option<String>,
    },
}

fn receive(line: &[u8]) -> Received {
    let bad = |why: String| Received::Bad { why, offer_run: None };
    let v: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => return bad(format!("not JSON: {e}")),
    };
    let Some(t) = v.get("t").and_then(Value::as_str) else {
        return bad("no type".to_owned());
    };
    let what = |e: serde_json::Error| format!("{t}: {e}");
    let frame = match t {
        "limits" => LimitsFrame::deserialize(&v).map(ToHost::Limits),
        "job_offer" => match Offer::deserialize(&v) {
            Ok(o) => Ok(ToHost::JobOffer(o)),
            Err(e) => {
                let run = v.get("run").and_then(Value::as_str).map(str::to_owned);
                return Received::Bad { why: what(e), offer_run: run };
            }
        },
        "job_abort" => Abort::deserialize(&v).map(ToHost::JobAbort),
        "pause" => Ok(ToHost::Pause),
        "resume" => Ok(ToHost::Resume),
        other => return Received::Unknown(clip(other, 64)),
    };
    match frame {
        Ok(f) => Received::Frame(f),
        Err(e) => bad(what(e)),
    }
}

/// Whether `id` is an id as the protocol writes them: 1 to 64 of `A-Z a-z 0-9 _ -`.
fn is_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// How the host is set up beyond its frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct Setup {
    /// Put the limits on this process: always, except in tests that play a host inside their
    /// own process (whose limits would hold the test runner).
    pub apply_limits: bool,
    /// The canary's test switch ([`Inject`]): nothing outside the tests.
    pub canary: Inject,
    /// A wait before every seat step of every game: nothing outside the tests, where it makes a
    /// short game last long enough to pause it (a result never depends on it, 4-07's gate 3).
    /// The test jobs keep their own pace.
    pub step_delay: Duration,
}

impl Setup {
    /// A host process's: the limits applied, the canary as this machine computes it and no
    /// delay, except in a build with the test jobs, where `CITAR_JOBS_TEST_CANARY=flushed` or
    /// `read_as_zero` fails one of the canary's checks on purpose and `CITAR_JOBS_TEST_STEP_MS`
    /// slows every seat step.
    #[must_use]
    pub fn process() -> Self {
        let (canary, step_delay) = test_switches();
        Self { apply_limits: true, canary, step_delay }
    }
}

#[cfg(feature = "test-jobs")]
fn test_switches() -> (Inject, Duration) {
    let canary = match std::env::var("CITAR_JOBS_TEST_CANARY").as_deref() {
        Ok("flushed") => Inject { halved: Some(0.0), ..Inject::default() },
        Ok("read_as_zero") => Inject { restored: Some(0.0), ..Inject::default() },
        _ => Inject::default(),
    };
    let ms = std::env::var("CITAR_JOBS_TEST_STEP_MS").ok().and_then(|v| v.parse::<u64>().ok());
    (canary, Duration::from_millis(ms.unwrap_or(0).min(1000)))
}

#[cfg(not(feature = "test-jobs"))]
fn test_switches() -> (Inject, Duration) {
    (Inject::default(), Duration::ZERO)
}

/// Where frames go: the parent's pipe, one line each, whole.
#[derive(Clone)]
pub(crate) struct Out(Arc<Mutex<Box<dyn Write + Send>>>);

impl Out {
    fn new(w: impl Write + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(w))))
    }

    /// Sends a frame. A parent that stopped reading is a parent gone, which the host learns from
    /// its standard input: the error is dropped here.
    pub(crate) fn send(&self, frame: &FromHost) {
        let mut line = frame.line();
        line.push('\n');
        let mut w = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let _gone = w.write_all(line.as_bytes()).and_then(|()| w.flush());
    }
}

/// A line of the host's log (its standard error).
pub(crate) fn log(line: &str) {
    eprintln!("citar job host: {line}");
}

/// What the host's threads share.
pub(crate) struct Shared {
    pub(crate) out: Out,
    pub(crate) rules: &'static Ruleset,
    pub(crate) jobs_build: String,
    pub(crate) limits: Requested,
    /// The pause, and the condition its waiters (and the runs' aborts) wake on.
    pub(crate) paused: Mutex<bool>,
    pub(crate) wake: Condvar,
    /// Standard input closed: every run stops.
    pub(crate) closing: AtomicBool,
    /// The canary and the self-check passed (and every run thread's canary since).
    pub(crate) verifiable: AtomicBool,
    pub(crate) canary: Inject,
    pub(crate) step_delay: Duration,
    pub(crate) runs: Mutex<BTreeMap<String, Arc<run::RunState>>>,
}

impl Shared {
    pub(crate) fn is_paused(&self) -> bool {
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn running(&self) -> u32 {
        let n = self.runs.lock().unwrap_or_else(PoisonError::into_inner).len();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    pub(crate) fn heartbeat(&self) {
        self.out.send(&FromHost::Heartbeat(Heartbeat {
            running: self.running(),
            paused: self.is_paused(),
            cpu_s: process_cpu_s(),
        }));
    }
}

/// This process's CPU seconds so far.
#[must_use]
pub fn process_cpu_s() -> f64 {
    cpu_time::ProcessTime::try_now().map_or(0.0, |t| t.as_duration().as_secs_f64())
}

/// What a line read gave.
enum Line {
    Text,
    TooLong,
    Eof,
}

/// Reads one line of at most [`MAX_LINE`] bytes into `buf` (without its newline); a longer one
/// is skipped to its end.
fn next_line(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    buf.clear();
    let cap = u64::try_from(MAX_LINE).unwrap_or(u64::MAX) + 1;
    let n = input.by_ref().take(cap).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(Line::Text);
    }
    if buf.len() <= MAX_LINE {
        // The last line, with no newline after it.
        return Ok(Line::Text);
    }
    // Too long: skip the rest of it.
    let mut rest = Vec::new();
    loop {
        rest.clear();
        let m = input.by_ref().take(64 << 10).read_until(b'\n', &mut rest)?;
        if m == 0 || rest.last() == Some(&b'\n') {
            return Ok(Line::TooLong);
        }
    }
}

/// The job host of this process: reads the parent's frames from `input` and writes its own to
/// `output` until `input` closes; the exit code (0, or [`EXIT_REFUSED`] after a `fatal` frame).
/// The limits are put on this process (`citar-helper job-host`, `python -m citar.jobs host`).
pub fn main(input: impl BufRead, output: impl Write + Send + 'static) -> i32 {
    serve(input, output, Setup::process())
}

/// [`main`], set up as `setup` says.
pub fn serve(mut input: impl BufRead, output: impl Write + Send + 'static, setup: Setup) -> i32 {
    let out = Out::new(output);
    let fatal = |code: FatalCode, message: String| {
        log(&format!("refusing to start: {message}"));
        out.send(&FromHost::Fatal(Fatal { code, message }));
        EXIT_REFUSED
    };
    let mut buf = Vec::new();
    let first = loop {
        match next_line(&mut input, &mut buf) {
            Ok(Line::Eof) | Err(_) => return 0,
            Ok(Line::TooLong) => {
                return fatal(FatalCode::Protocol, "the first line is too long".to_owned());
            }
            Ok(Line::Text) if buf.iter().all(u8::is_ascii_whitespace) => {}
            Ok(Line::Text) => break receive(&buf),
        }
    };
    let Received::Frame(ToHost::Limits(asked)) = first else {
        return fatal(FatalCode::Protocol, "the first frame must be `limits`".to_owned());
    };
    let rules = Ruleset::shared();
    let mine = crate::build::jobs_build(rules);
    if asked.jobs_build != mine {
        return fatal(
            FatalCode::Build,
            format!(
                "the parent's jobs build is {}, and this host's is {mine}",
                clip(&asked.jobs_build, 64)
            ),
        );
    }
    let req = asked.requested();
    let applied = if setup.apply_limits {
        match limits::apply(&req) {
            Ok(a) => {
                for n in &a.notes {
                    log(n);
                }
                Some(a)
            }
            Err(e) => return fatal(FatalCode::Limits, e),
        }
    } else {
        if let Err(e) = req.check() {
            return fatal(FatalCode::Limits, e);
        }
        None
    };
    citar_sim::panics::install();
    let (held, cgroup, notes) = applied.as_ref().map_or_else(
        || {
            let soft = Held { cpu: limits::Hardness::Soft, memory: limits::Hardness::Soft };
            (
                soft,
                None,
                vec!["no limits put on this process (a host inside its parent)".to_owned()],
            )
        },
        |a| (a.held, a.cgroup.clone(), a.notes.clone()),
    );
    let check = self_test(rules, setup.canary).map(|_| ());
    if let Err(why) = &check {
        log(&format!("not verifiable: {why}"));
    }
    let shared = Arc::new(Shared {
        out: out.clone(),
        rules,
        jobs_build: mine.clone(),
        limits: req,
        paused: Mutex::new(false),
        wake: Condvar::new(),
        closing: AtomicBool::new(false),
        verifiable: AtomicBool::new(check.is_ok()),
        canary: setup.canary,
        step_delay: setup.step_delay,
        runs: Mutex::new(BTreeMap::new()),
    });
    out.send(&FromHost::Ready(Ready {
        limits: held,
        verifiable: check.is_ok(),
        jobs_build: mine,
        threads: req.threads,
        reason: check.err(),
        cgroup,
        cpu_s: process_cpu_s(),
        pid: std::process::id(),
        counting: alloc::installed(),
        notes,
    }));
    let ticker = Arc::clone(&shared);
    let _ticks = std::thread::Builder::new()
        .name("citar-host-heartbeat".to_owned())
        .spawn(move || beat(&ticker));
    let mut said_unknown = std::collections::BTreeSet::new();
    loop {
        match next_line(&mut input, &mut buf) {
            Ok(Line::Eof) | Err(_) => break,
            Ok(Line::TooLong) => log(&format!("a line longer than {MAX_LINE} bytes, skipped")),
            Ok(Line::Text) if buf.iter().all(u8::is_ascii_whitespace) => {}
            Ok(Line::Text) => match receive(&buf) {
                Received::Frame(f) => handle(&shared, f),
                Received::Unknown(t) => {
                    if said_unknown.len() < 64 && said_unknown.insert(t.clone()) {
                        log(&format!("ignoring frames of type {t:?}"));
                    }
                }
                Received::Bad { why, offer_run } => {
                    log(&format!("a frame that does not read: {}", clip(&why, QUOTE)));
                    if let Some(run) = offer_run {
                        reject(&shared.out, &run, RejectReason::BadSpec, clip(&why, QUOTE));
                    }
                }
            },
        }
    }
    shared.closing.store(true, Ordering::SeqCst);
    shared.wake.notify_all();
    let until = Instant::now() + EOF_GRACE;
    while shared.running() > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    0
}

/// The canary (with `inject`, the test switch, in place of what it computes where it gives a
/// value) and the self-check, on a thread made as run threads are: what a host runs before
/// `ready`, and `citar-helper --self-test`.
///
/// # Errors
/// What failed: the canary's check, or how the self-check's job came out otherwise.
pub fn self_test(rules: &'static Ruleset, inject: Inject) -> Result<SelfCheck, String> {
    let checked = std::thread::Builder::new()
        .name("citar-host-self-check".to_owned())
        .stack_size(RUN_STACK)
        .spawn(move || {
            canary_injected(&inject).map_err(|e| format!("the canary: {e}"))?;
            self_check(rules).map_err(|e| format!("the self-check: {e}"))
        })
        .map_err(|e| format!("the self-check's thread: {e}"))?;
    checked.join().unwrap_or_else(|_| Err("the self-check panicked".to_owned()))
}

/// The heartbeat, until the host closes.
fn beat(shared: &Shared) {
    while !shared.closing.load(Ordering::SeqCst) {
        let g = shared.paused.lock().unwrap_or_else(PoisonError::into_inner);
        let (g, _timeout) =
            shared.wake.wait_timeout(g, HEARTBEAT_EVERY).unwrap_or_else(PoisonError::into_inner);
        drop(g);
        if shared.closing.load(Ordering::SeqCst) {
            return;
        }
        shared.heartbeat();
    }
}

fn reject(out: &Out, run: &str, reason: RejectReason, message: String) {
    out.send(&FromHost::JobReject(Reject { run: run.to_owned(), reason, message }));
}

/// One frame from the parent.
fn handle(shared: &Arc<Shared>, frame: ToHost) {
    match frame {
        ToHost::Limits(_) => log("a second `limits` frame, ignored: limits are set at start"),
        ToHost::JobOffer(o) => offer(shared, o),
        ToHost::JobAbort(a) => {
            let runs = shared.runs.lock().unwrap_or_else(PoisonError::into_inner);
            match runs.get(&a.run) {
                Some(r) => {
                    r.abort.store(true, Ordering::SeqCst);
                    drop(runs);
                    shared.wake.notify_all();
                }
                None => log(&format!("no run {} to abort", clip(&a.run, 64))),
            }
        }
        ToHost::Pause | ToHost::Resume => {
            let pause = matches!(frame, ToHost::Pause);
            *shared.paused.lock().unwrap_or_else(PoisonError::into_inner) = pause;
            shared.wake.notify_all();
            shared.heartbeat();
        }
    }
}

/// What a run plays.
pub(crate) enum Work {
    Game(Box<Checked>),
    #[cfg(feature = "test-jobs")]
    Test(test_jobs::TestJob),
}

/// Reads an offer's kind and spec.
fn read_work(rules: &'static Ruleset, o: &Offer) -> Result<Work, String> {
    #[cfg(feature = "test-jobs")]
    if let Some(t) = test_jobs::read(&o.kind, &o.spec, &o.spec_hash) {
        return t.map(Work::Test);
    }
    let kind = Kind::from_name(&o.kind)
        .ok_or_else(|| format!("unknown job kind {:?}", clip(&o.kind, 64)))?;
    check_spec(rules, kind, &o.spec, Some(&o.spec_hash))
        .map(|c| Work::Game(Box::new(c)))
        .map_err(|e| e.0)
}

/// An offer: checked in the order the module's doc gives, then played on a thread of its own.
fn offer(shared: &Arc<Shared>, o: Offer) {
    let out = &shared.out;
    if !is_id(&o.run) {
        reject(out, &o.run, RejectReason::BadSpec, "the run's id is not an id".to_owned());
        return;
    }
    if !shared.verifiable.load(Ordering::SeqCst) {
        let why = "this host failed its canary or its self-check".to_owned();
        reject(out, &o.run, RejectReason::Unverifiable, why);
        return;
    }
    if o.jobs_build != shared.jobs_build {
        let why = format!(
            "the job is for jobs build {}, and this host's is {}",
            clip(&o.jobs_build, 64),
            shared.jobs_build
        );
        reject(out, &o.run, RejectReason::Build, why);
        return;
    }
    if !(o.deadline_s.is_finite() && (DEADLINE_S.0..=DEADLINE_S.1).contains(&o.deadline_s)) {
        let why =
            format!("deadline_s {} is not {} to {}", o.deadline_s, DEADLINE_S.0, DEADLINE_S.1);
        reject(out, &o.run, RejectReason::BadSpec, why);
        return;
    }
    let work = match read_work(shared.rules, &o) {
        Ok(w) => w,
        Err(why) => {
            reject(out, &o.run, RejectReason::BadSpec, clip(&why, QUOTE));
            return;
        }
    };
    let per_thread = shared.limits.memory_per_thread_mb();
    if o.mem_mb > per_thread {
        let why = format!("the job needs {} MB, and a run here has {per_thread} MB", o.mem_mb);
        reject(out, &o.run, RejectReason::Memory, why);
        return;
    }
    let mut runs = shared.runs.lock().unwrap_or_else(PoisonError::into_inner);
    if runs.contains_key(&o.run) {
        drop(runs);
        reject(out, &o.run, RejectReason::BadSpec, "the run is already playing".to_owned());
        return;
    }
    if runs.len() >= usize::try_from(shared.limits.threads).unwrap_or(usize::MAX) {
        drop(runs);
        let why = format!("{} runs already, its most", shared.limits.threads);
        reject(out, &o.run, RejectReason::Busy, why);
        return;
    }
    let state = Arc::new(run::RunState::new(&o.run, o.deadline_s));
    runs.insert(o.run.clone(), Arc::clone(&state));
    drop(runs);
    let sh = Arc::clone(shared);
    let started = std::thread::Builder::new()
        .name(format!("citar-run-{}", clip(&o.run, 40)))
        .stack_size(RUN_STACK)
        .spawn(move || run::play(&sh, &state, work));
    if let Err(e) = started {
        shared.runs.lock().unwrap_or_else(PoisonError::into_inner).remove(&o.run);
        let why = format!("no thread for the run: {e}");
        reject(out, &o.run, RejectReason::Busy, why);
    }
}

#[cfg(test)]
mod tests;
