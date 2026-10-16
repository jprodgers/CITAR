//! One run of the job host (P4.4.3), on a thread of its own: its memory counted apart, the
//! canary checked on its thread, its game played a seat step at a time under [`Watch`], which
//! between steps stops it for an abort, a deadline or the host closing, holds it while the host is
//! paused, and reports its progress (and its peak, as it grows).
//!
//! **A run's end** (its result, its failure, its abort, a late refusal) is sent only once the run
//! has left the host's list of runs and given back its tally: a parent that offers the next run as
//! soon as it reads the end finds the thread free, never `busy`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use cpu_time::ThreadTime;

use super::{
    FailCode, Failed, FromHost, Outcome, PROGRESS_EVERY, PROGRESS_ROUNDS, ProgressFrame, Reject,
    RejectReason, ResultFrame, RunRef, Shared, Work, log,
};
use crate::canary::canary_injected;
use crate::job::{Control, Flow, Job, Progress, RunError};
use crate::spec::Checked;
use crate::sys::alloc::{Reader, Tally};

/// A run the host is playing, as the host's other threads see it.
#[derive(Debug)]
pub(crate) struct RunState {
    pub(crate) id: String,
    /// The parent asked it to stop.
    pub(crate) abort: AtomicBool,
    /// Its deadline, in active time.
    pub(crate) deadline: Duration,
}

impl RunState {
    pub(crate) fn new(id: &str, deadline_s: f64) -> Self {
        Self {
            id: id.to_owned(),
            abort: AtomicBool::new(false),
            deadline: Duration::try_from_secs_f64(deadline_s).unwrap_or(Duration::MAX),
        }
    }
}

/// Why a run stopped before its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// The parent aborted it: `job_aborted`.
    Aborted,
    /// Past its deadline: `job_failed` `deadline`, with why.
    Deadline(String),
    /// The host is closing: nothing more is sent.
    Closing,
}

/// The run's control between seat steps (P4.4.3): abort, pause, deadline, progress.
pub(crate) struct Watch<'a> {
    shared: &'a Shared,
    run: &'a RunState,
    tally: Option<Reader>,
    started: Instant,
    /// Time held by pauses, before the current one.
    paused: Duration,
    cpu0: Option<Duration>,
    reported_at: Instant,
    reported_round: i32,
    /// The peak, in bytes, its last report gave.
    reported_peak: u64,
    /// The test switch's wait before each seat step of a game ([`super::Setup::step_delay`]).
    delay: Duration,
    pub(crate) stop: Option<Stopped>,
}

fn thread_cpu() -> Option<Duration> {
    ThreadTime::try_now().ok().map(|t| t.as_duration())
}

impl<'a> Watch<'a> {
    fn new(shared: &'a Shared, run: &'a RunState, tally: Option<Reader>, delay: Duration) -> Self {
        let now = Instant::now();
        Self {
            shared,
            run,
            tally,
            started: now,
            paused: Duration::ZERO,
            cpu0: thread_cpu(),
            reported_at: now,
            reported_round: 0,
            reported_peak: 0,
            delay,
            stop: None,
        }
    }

    /// Seconds paused so far, with `held` of a pause still going on.
    fn paused_s(&self, held: Duration) -> f64 {
        (self.paused + held).as_secs_f64()
    }

    fn peak_bytes(&self) -> u64 {
        self.tally.map_or(0, |t| t.peak_bytes())
    }

    fn peak_mb(&self) -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "bytes well under 2^52")]
        let mb = self.peak_bytes() as f64 / f64::from(1 << 20);
        (mb * 1000.0).round() / 1000.0
    }

    /// This thread's CPU seconds since the run began.
    fn cpu_s(&self) -> f64 {
        self.cpu0.zip(thread_cpu()).map_or(0.0, |(a, b)| b.saturating_sub(a).as_secs_f64())
    }

    fn report(&mut self, at: &Progress, held: Option<Duration>) {
        self.reported_peak = self.peak_bytes();
        self.shared.out.send(&FromHost::JobProgress(ProgressFrame {
            run: self.run.id.clone(),
            round: i64::from(at.round),
            of: i64::from(at.of),
            elapsed_s: self.started.elapsed().as_secs_f64(),
            cpu_s: self.cpu_s(),
            paused_s: self.paused_s(held.unwrap_or_default()),
            peak_mb: self.peak_mb(),
            paused: held.is_some(),
        }));
        self.reported_at = Instant::now();
        self.reported_round = at.round;
    }

    /// Why the run must stop now, if it must, with `held` of a pause still going on.
    fn should_stop(&self, held: Duration) -> Option<Stopped> {
        if self.shared.closing.load(Ordering::SeqCst) {
            return Some(Stopped::Closing);
        }
        if self.run.abort.load(Ordering::SeqCst) {
            return Some(Stopped::Aborted);
        }
        let wall = self.started.elapsed();
        let active = wall.saturating_sub(self.paused + held);
        let deadline = self.run.deadline;
        if active > deadline {
            return Some(Stopped::Deadline(format!(
                "{:.1} s of play passed its deadline of {:.1} s",
                active.as_secs_f64(),
                deadline.as_secs_f64()
            )));
        }
        if wall > deadline.saturating_mul(2) {
            return Some(Stopped::Deadline(format!(
                "{:.1} s in all, {:.1} s of it paused, passed twice its deadline of {:.1} s",
                wall.as_secs_f64(),
                (self.paused + held).as_secs_f64(),
                deadline.as_secs_f64()
            )));
        }
        None
    }

    /// Holds the run while the host is paused; why it must stop, if it must.
    fn hold(&mut self, at: &Progress) -> Option<Stopped> {
        let since = Instant::now();
        self.report(at, Some(Duration::ZERO));
        let mut why = None;
        let mut g = self.shared.paused.lock().unwrap_or_else(PoisonError::into_inner);
        while *g {
            if let Some(w) = self.should_stop(since.elapsed()) {
                why = Some(w);
                break;
            }
            let left = self.run.deadline.saturating_mul(2).saturating_sub(self.started.elapsed());
            let wait = left.clamp(Duration::from_millis(1), Duration::from_millis(500));
            g = self.shared.wake.wait_timeout(g, wait).unwrap_or_else(PoisonError::into_inner).0;
            if self.reported_at.elapsed() >= PROGRESS_EVERY {
                drop(g);
                self.report(at, Some(since.elapsed()));
                g = self.shared.paused.lock().unwrap_or_else(PoisonError::into_inner);
            }
        }
        drop(g);
        self.paused += since.elapsed();
        if why.is_none() {
            self.report(at, None);
        }
        why
    }
}

impl Control for Watch<'_> {
    fn between(&mut self, at: &Progress) -> Flow {
        let mut why = self.should_stop(Duration::ZERO);
        if why.is_none() && self.shared.is_paused() {
            why = self.hold(at);
        }
        if let Some(w) = why {
            self.stop = Some(w);
            return Flow::Stop;
        }
        let rounds = at.round.saturating_sub(self.reported_round);
        let grown = self.peak_bytes().saturating_sub(self.reported_peak) >= self.shared.peak_step;
        if (at.new_round && rounds >= PROGRESS_ROUNDS)
            || self.reported_at.elapsed() >= PROGRESS_EVERY
            || grown
        {
            self.report(at, None);
        }
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        Flow::Go
    }
}

/// Plays one run to its end, leaves the host's list of runs, then sends the run's end.
pub(crate) fn play(shared: &Arc<Shared>, run: &Arc<RunState>, work: Work) {
    let tally = Tally::claim();
    if tally.is_none() {
        log(&format!("run {}: no memory tally free; its peak is not counted", run.id));
    }
    let reader = tally.as_ref().map(Tally::reader);
    let end = play_counted(shared, run, work, reader);
    drop(tally);
    shared.runs.lock().unwrap_or_else(PoisonError::into_inner).remove(&run.id);
    if let Some(end) = end {
        shared.out.send(&end);
    }
    shared.heartbeat();
}

/// Plays the run; its end frame, for [`play`] to send (none when the host is closing).
fn play_counted(
    shared: &Shared,
    run: &RunState,
    work: Work,
    tally: Option<Reader>,
) -> Option<FromHost> {
    // FTZ and DAZ are per thread: this one must compute as the self-check's did.
    if let Err(e) = canary_injected(&shared.canary) {
        shared.verifiable.store(false, Ordering::SeqCst);
        log(&format!("run {}: the canary failed on its thread ({e}); no more jobs", run.id));
        let why = format!("the canary failed on the run's thread: {e}");
        return Some(refused(run, RejectReason::Unverifiable, why));
    }
    match work {
        Work::Game(checked) => game(shared, run, &checked, tally),
        #[cfg(feature = "test-jobs")]
        Work::Test(t) => {
            accept(shared, run);
            let mut watch = Watch::new(shared, run, tally, Duration::ZERO);
            match super::test_jobs::play(&t, &mut watch) {
                Ok(out) => Some(result(run, &watch, out)),
                Err(RunError::Stopped) => stopped(run, watch.stop.take()),
                Err(e) => Some(failed(run, FailCode::Internal, e.to_string())),
            }
        }
    }
}

fn refused(run: &RunState, reason: RejectReason, message: String) -> FromHost {
    FromHost::JobReject(Reject { run: run.id.clone(), reason, message })
}

fn accept(shared: &Shared, run: &RunState) {
    shared.out.send(&FromHost::JobAccept(RunRef { run: run.id.clone() }));
    shared.heartbeat();
}

fn failed(run: &RunState, code: FailCode, message: String) -> FromHost {
    log(&format!("run {}: {message}", run.id));
    FromHost::JobFailed(Failed { run: run.id.clone(), code, message, retryable: true })
}

/// A stopped run's end: none when the host is closing.
fn stopped(run: &RunState, why: Option<Stopped>) -> Option<FromHost> {
    match why {
        Some(Stopped::Aborted) => Some(FromHost::JobAborted(RunRef { run: run.id.clone() })),
        Some(Stopped::Deadline(why)) => Some(failed(run, FailCode::Deadline, why)),
        Some(Stopped::Closing) => None,
        None => Some(failed(run, FailCode::Internal, "stopped for no reason".to_owned())),
    }
}

/// A finished run's outcome, as the jobs crate and the test jobs give it.
pub(crate) struct Played {
    pub(crate) result: serde_json::Value,
    pub(crate) result_hash: String,
    pub(crate) chain: crate::hash::Chain,
    pub(crate) wall_s: f64,
    pub(crate) cpu_s: f64,
}

/// A finished run's `job_result`, its peak and paused time read from `watch` (before its tally is
/// given back).
fn result(run: &RunState, watch: &Watch<'_>, out: Played) -> FromHost {
    FromHost::JobResult(Box::new(ResultFrame {
        run: run.id.clone(),
        outcome: Outcome {
            result: out.result,
            result_hash: out.result_hash,
            chain: out.chain,
            cpu_s: out.cpu_s,
            wall_s: out.wall_s,
            paused_s: watch.paused_s(Duration::ZERO),
            peak_mb: watch.peak_mb(),
        },
    }))
}

fn game(
    shared: &Shared,
    run: &RunState,
    checked: &Checked,
    tally: Option<Reader>,
) -> Option<FromHost> {
    let mut job = match Job::start(shared.rules, checked) {
        Ok(j) => j,
        Err(RunError::Spec(e)) => return Some(refused(run, RejectReason::BadSpec, e.0)),
        Err(e) => {
            accept(shared, run);
            return Some(failed(run, FailCode::Internal, e.to_string()));
        }
    };
    accept(shared, run);
    let mut watch = Watch::new(shared, run, tally, shared.step_delay);
    while !job.is_over() {
        if watch.between(&job.progress()) == Flow::Stop {
            return stopped(run, watch.stop.take());
        }
        if let Err(e) = job.step() {
            return Some(failed(run, FailCode::Internal, e.to_string()));
        }
    }
    Some(match job.finish() {
        Ok(out) => {
            if let Some(trace) = &out.trace {
                log(&format!("run {}: its game crashed\n{trace}", run.id));
            }
            match serde_json::to_value(&out.result) {
                Ok(value) => result(
                    run,
                    &watch,
                    Played {
                        result: value,
                        result_hash: out.result_hash,
                        chain: out.chain,
                        wall_s: out.wall_s,
                        cpu_s: out.cpu_s,
                    },
                ),
                Err(e) => failed(run, FailCode::Internal, format!("the result: {e}")),
            }
        }
        Err(e) => failed(run, FailCode::Internal, e.to_string()),
    })
}
