//! CPU jobs on the helper (design/phase4-helper.md P4.3.6, P4.2.5; PROTOCOL.md "Jobs"): the job
//! host as the helper's child, the job frames relayed between it and the server, results signed
//! and kept until acknowledged.
//!
//! **One task** ([`JobSide`]) owns the job host and the runs, apart from the connection: a run goes
//! on across a lost connection, and its result is written to `helper/results/<run>.json` before it
//! is sent, so one made while the helper was disconnected, or sent just before the connection died,
//! is sent again when the welcome's `jobs.resend` asks for it. The session gives it the server's
//! job frames and the settings it acknowledges, and sends what it hands back.
//!
//! - **The job host** ([`Hosts`]: in a running helper `citar-helper job-host`, its own binary, as
//!   a child, [`crate::jobhost`]) is started when CPU work is on in effect (the site's settings and
//!   the machine's own switch, [`crate::settings::ack`]) under the effective limits, and started
//!   again, once its runs have finished, when they change. A `settings_ack` that turns CPU work on
//!   or changes its limits goes once that host is `ready` (or could not start, or [`ACK_HOLD`]
//!   passed), so it carries the host's own `limits` and `memory_per_run_mb`; a `settings` that comes
//!   while a host starts is applied once that start is over, in turn. When a host becomes ready
//!   after the acknowledgement of its settings went without its figures (the hold passed, or its
//!   limits changed while runs played and it was started again once they ended), that
//!   acknowledgement goes again with them, marked `again`. A host that could not start makes the
//!   helper refuse offers `limits` until one does ([`START_RETRY`] later).
//! - **An offer** is checked here first, in this order: `disabled`, `quiet`, `unverifiable`,
//!   `build`, `kind`, `bad_spec` (by citar-jobs' own checks of the spec and its hash), `limits`,
//!   `memory` (above the host's `memory_per_run_mb`), `busy`; then relayed to the host, which checks
//!   it again and answers `job_accept` or `job_reject`, relayed back. One that waits for a host's
//!   start is answered within its time to live (`limits` when the host is still not ready
//!   [`OFFER_MARGIN`] before it), so the server never counts it as abandoned.
//! - **A connection's own:** what its server asked and was not answered when it ended (the
//!   acknowledgements and offers waiting for a host) goes with it, and so does its `jobs_pause`
//!   (the server says it again after the next welcome while its benchmark lasts).
//! - **Pauses:** every running game is held (the host's `pause`) while a completion is in flight
//!   and `yield_to_llm` is on, and between `jobs_pause` and `jobs_resume` whatever `yield_to_llm`
//!   says; `job_progress` says so (`paused`), and `status` counts the runs held.
//! - **Deadlines:** the host ends a run past its deadline itself; the helper does too, as the
//!   backstop for a run stuck inside a seat step ([`DEADLINE_GRACE`] after it): it aborts it, says
//!   `job_failed` `deadline`, and ends a host that does not let the run go within [`ABORT_GRACE`].
//!   A deadline is active time, paused time not counted, and a run that has lasted twice its
//!   deadline in all ends too.
//! - **Results** are signed (the claim, [`citar_jobs::Claim`], by the machine key: the key never
//!   reaches the job host), written to the home, then sent; `job_result_ack` deletes the file.
//! - **The welcome's `jobs`:** the runs of the hello's `running` it does not keep are aborted
//!   (nothing is sent for them); of the hello's `results`, those in `resend` are sent again and the
//!   others deleted; a result made after the hello is sent.
//! - **Stopping** (Ctrl-C, the service manager) ends every run: `job_failed` `aborted_locally`, so
//!   the server gives the job to another machine at once.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use citar_engine::rules::Ruleset;
use citar_jobs::host::{self as jh, FromHost, ToHost};
use citar_jobs::limits::Requested;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::key::MachineKey;
use crate::llm::BoxFuture;
use crate::log::{Log, clip};
use crate::protocol::{
    Chain, Claim, FailCode, Hardness, HelperFrame, JobAbort, JobFailed, JobOffer, JobProgress,
    JobReject, JobResult, Kind, Limits, Outcome, RejectReason, RunRef, SettingsAck, SiteCpu,
};

/// How long a `settings_ack` waits for the job host its settings start.
pub const ACK_HOLD: Duration = Duration::from_secs(10);
/// After a job host could not start, how long offers are refused `limits` before it is tried
/// again.
pub const START_RETRY: Duration = Duration::from_secs(30);
/// How long after a run's deadline the helper ends it itself: the job host ends it first, at its
/// next seat step, unless it is stuck in one.
pub const DEADLINE_GRACE: Duration = Duration::from_secs(1);
/// How long a run the helper aborted may take to end before its job host is ended with it.
pub const ABORT_GRACE: Duration = Duration::from_secs(10);
/// How long writing or deleting a result file may take.
pub const RESULT_IO: Duration = Duration::from_secs(5);
/// The largest result file read back (a result is well under 1 MB).
pub const RESULT_FILE_MAX: u64 = 4 << 20;
/// How long before its time to live an offer waiting for a job host's start is refused `limits`
/// (half its time to live when that is shorter), so the answer reaches the server in time.
pub const OFFER_MARGIN: Duration = Duration::from_secs(5);
/// Why an offer that waited for a job host's start is refused.
const NOT_READY_IN_TIME: &str = "its job host was not ready within the offer's time to live";
/// The most runs a hello lists in `results` (PROTOCOL.md: `[run] ≤1024`); the others are sent
/// after the welcome all the same.
pub const HELLO_RESULTS_MAX: usize = 1024;

/// A job host, ready: its `ready`, where its frames go and where its own come from. Dropping it
/// (its `cmds`) ends the host.
pub struct HostStart {
    pub ready: jh::Ready,
    pub cmds: mpsc::UnboundedSender<ToHost>,
    pub events: mpsc::UnboundedReceiver<HostEvent>,
}

/// What a job host said or did.
#[derive(Debug)]
pub enum HostEvent {
    Frame(FromHost),
    /// It ended: why, and a `job_failed` (`memory` or `host`) for each run it had in flight with
    /// the largest peak that run reported.
    Died {
        death: String,
        failures: Vec<(jh::Failed, Option<f64>)>,
    },
}

/// Why a job host could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartFailure {
    /// It could not hold the limits (or not live under them).
    pub limits: bool,
    pub message: String,
}

/// What starts job hosts: the helper's own binary as a child in a running helper
/// ([`crate::jobhost::ChildHosts`]), a test's fake in the transcripts.
pub trait Hosts: Send + Sync {
    /// The canary and the self-check as a job host here would run them (P4.4.3): whether this
    /// machine computes the golden job as committed.
    fn self_test(&self) -> BoxFuture<'static, Result<(), String>>;
    /// A job host holding `limits`, of `jobs_build`, once it is ready.
    fn start(
        &self,
        limits: Requested,
        jobs_build: String,
    ) -> BoxFuture<'static, Result<HostStart, StartFailure>>;
}

/// What the job side runs with.
#[derive(Clone, Default)]
pub struct JobSetup {
    /// What starts job hosts; without it the helper has no jobs build and plays no job.
    pub hosts: Option<Arc<dyn Hosts>>,
    /// The machine key that signs each result's claim.
    pub key: Option<Arc<MachineKey>>,
    /// `helper/results/`, where results wait for their acknowledgement; without it they are held
    /// in memory only.
    pub results: Option<PathBuf>,
    /// Where the job side says what it holds (the tests read it too).
    pub state: JobState,
}

/// What the job side holds, shared: it writes, the session and the tests read.
#[derive(Clone, Default)]
pub struct JobState(Arc<Mutex<Snapshot>>);

impl JobState {
    /// What the job side holds now.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.0).clone()
    }
}

/// What the session reads of the job side (the hello, `status`, the tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// The runs being played (accepted by the host, not ended): a hello's `running`.
    pub running: Vec<String>,
    /// The runs whose results are held unacknowledged: a hello's `results`.
    pub held: Vec<String>,
    pub jobs_running: u32,
    pub jobs_paused: u32,
    /// The canary and the self-check passed (the helper's own, then each host's).
    pub verifiable: bool,
    /// How hard the last job host said its limits hold.
    pub limits: Option<Limits>,
    /// The memory a run may use on the last job host.
    pub memory_per_run_mb: Option<u64>,
}

/// The job side's handle: the session's way in. Every call but [`JobSide::stop`] returns at once.
pub struct JobSide {
    cmds: mpsc::UnboundedSender<Cmd>,
    state: Arc<Mutex<Snapshot>>,
    jobs_build: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for JobSide {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The session's word to the job side.
enum Cmd {
    Connected {
        out: mpsc::UnboundedSender<HelperFrame>,
        machine: String,
    },
    Disconnected,
    Settings {
        ack: SettingsAck,
        cpu: SiteCpu,
    },
    Welcome(Welcomed),
    /// An offer, and when it came (its time to live runs from then).
    Offer {
        offer: JobOffer,
        quiet: bool,
        at: Instant,
    },
    Abort(JobAbort),
    ResultAck(String),
    Benchmark(bool),
    LlmBusy(bool),
    Stop(oneshot::Sender<()>),
}

/// The welcome's `jobs`, with what the hello they answer listed.
pub struct Welcomed {
    pub keep: Vec<String>,
    pub drop: Vec<String>,
    pub resend: Vec<String>,
    /// The hello's `running`.
    pub running: Vec<String>,
    /// The hello's `results`.
    pub results: Vec<String>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl JobSide {
    /// Starts the job side of a helper that plays jobs: it runs the self-test first (the hello's
    /// `verifiable`), then its task. `local_cpu` is the machine's own switch.
    pub async fn start(
        setup: &JobSetup,
        hosts: Arc<dyn Hosts>,
        local_cpu: bool,
        log: Log,
    ) -> JobSide {
        let rules = Ruleset::shared();
        let jobs_build = citar_jobs::jobs_build(rules);
        let verifiable = match hosts.self_test().await {
            Ok(()) => true,
            Err(why) => {
                log.warn(format!(
                    "this machine fails the job host's self-test, so it takes no CPU job \
                     (unverifiable): {}",
                    clip(&why, 300)
                ));
                false
            }
        };
        let state = Arc::clone(&setup.state.0);
        *lock(&state) = Snapshot { verifiable, ..Snapshot::default() };
        let (cmds, rx) = mpsc::unbounded_channel();
        // The results a previous run left, before the first hello lists them.
        let held = held_results(setup.results.clone(), &log).await;
        let side = Side {
            log,
            hosts,
            key: setup.key.clone(),
            results: setup.results.clone(),
            jobs_build: jobs_build.clone(),
            rules,
            local_cpu,
            verifiable,
            out: None,
            machine: String::new(),
            conn: 0,
            effective: None,
            want: None,
            host: None,
            starting: None,
            start_failed: None,
            held: VecDeque::new(),
            hold_until: None,
            waiting_offers: VecDeque::new(),
            last_ack: None,
            runs: BTreeMap::new(),
            results_held: held
                .into_iter()
                .map(|run| (run, Held { frame: None, sent_on: None }))
                .collect(),
            llm_busy: false,
            bench: false,
            paused: false,
            restart: false,
            state: Arc::clone(&state),
        };
        side.publish();
        let task = tokio::spawn(side.run(rx));
        JobSide { cmds, state, jobs_build, task }
    }

    /// This helper's jobs build.
    #[must_use]
    pub fn jobs_build(&self) -> &str {
        &self.jobs_build
    }

    /// What the job side holds now.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.state).clone()
    }

    fn send(&self, cmd: Cmd) {
        let _gone = self.cmds.send(cmd);
    }

    /// A connection's welcome came: frames for the server go to `out`, results are signed for
    /// `machine`.
    pub fn connected(&self, out: mpsc::UnboundedSender<HelperFrame>, machine: String) {
        self.send(Cmd::Connected { out, machine });
    }

    /// The connection ended: runs go on, results wait on disk.
    pub fn disconnected(&self) {
        self.send(Cmd::Disconnected);
    }

    /// The acknowledgement of a `settings` frame (and the effective CPU work it says), which the
    /// job side sends once the job host it asks for is ready, with that host's limits.
    pub fn settings(&self, ack: SettingsAck) {
        let cpu = ack.effective.cpu.clone();
        self.send(Cmd::Settings { ack, cpu });
    }

    /// The welcome's `jobs`, after its settings' acknowledgement.
    pub fn welcome(&self, w: Welcomed) {
        self.send(Cmd::Welcome(w));
    }

    /// A `job_offer`; `quiet` when the local quiet hours hold now.
    pub fn offer(&self, offer: JobOffer, quiet: bool) {
        self.send(Cmd::Offer { offer, quiet, at: Instant::now() });
    }

    /// A `job_abort`.
    pub fn abort(&self, abort: JobAbort) {
        self.send(Cmd::Abort(abort));
    }

    /// A `job_result_ack`.
    pub fn result_ack(&self, run: String) {
        self.send(Cmd::ResultAck(run));
    }

    /// `jobs_pause` (true) or `jobs_resume` (false).
    pub fn benchmark(&self, on: bool) {
        self.send(Cmd::Benchmark(on));
    }

    /// Whether any completion is in flight (for `yield_to_llm`).
    pub fn llm_busy(&self, busy: bool) {
        self.send(Cmd::LlmBusy(busy));
    }

    /// The helper is stopping: every run ends, `job_failed` `aborted_locally` to a connected server
    /// (its frames are handed out before this returns), and the host with them. Waits at most
    /// `within`.
    pub async fn stop(&self, within: Duration) {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Stop(tx));
        let _done = tokio::time::timeout(within, rx).await;
    }
}

/// Where a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Offered to the host, not yet accepted.
    Offered,
    Running,
    /// Being ended by the helper, for this reason, since then.
    Aborting(Why, Instant),
}

/// Why the helper ends a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// The server's `job_abort`: answered `job_aborted` when it ends.
    Server,
    /// Past its deadline: `job_failed` `deadline` already said.
    Deadline,
    /// The welcome dropped it: nothing more is said.
    Dropped,
    /// The helper is stopping: `aborted_locally` already said.
    Stopping,
}

/// A run the helper holds.
struct Run {
    job: String,
    kind: Kind,
    spec_hash: String,
    jobs_build: String,
    bots: Vec<String>,
    machine: String,
    deadline_s: f64,
    phase: Phase,
    accepted: Option<Instant>,
    /// Paused time before the current pause.
    paused_s: f64,
    /// When the current pause began, while one holds it.
    paused_since: Option<Instant>,
}

impl Run {
    fn paused_total(&self, now: Instant) -> f64 {
        self.paused_s + self.paused_since.map_or(0.0, |t| now.duration_since(t).as_secs_f64())
    }

    /// When the helper itself ends it for its deadline: active time past its deadline, or twice
    /// its deadline in all, and [`DEADLINE_GRACE`] after either.
    fn deadline_at(&self, now: Instant) -> Option<Instant> {
        let start = self.accepted?;
        if self.phase != Phase::Running {
            return None;
        }
        let active = start + Duration::from_secs_f64(self.deadline_s + self.paused_total(now));
        let wall = start + Duration::from_secs_f64(2.0 * self.deadline_s);
        Some(active.min(wall) + DEADLINE_GRACE)
    }

    fn live(&self) -> bool {
        matches!(self.phase, Phase::Offered | Phase::Running)
    }
}

/// What waits while a job host starts (an acknowledgement's figures, and what follows it).
enum Parked {
    /// The acknowledgement of settings already applied, sent once the host is ready.
    Ack(SettingsAck),
    /// A command not yet handled (a later `settings` among them: it is applied in its turn).
    Cmd(Cmd),
}

/// When an offer that waits must be answered: [`OFFER_MARGIN`] before its time to live ends (or
/// half way, for a short one).
fn answer_by(o: &JobOffer, at: Instant) -> Instant {
    let ttl = if o.offer_ttl_s.is_finite() { o.offer_ttl_s.max(0.0) } else { 0.0 };
    let margin = OFFER_MARGIN.as_secs_f64();
    at + Duration::from_secs_f64((ttl - margin).max(ttl / 2.0))
}

/// A result held until its acknowledgement.
struct Held {
    frame: Option<JobResult>,
    /// The connection it was last sent on.
    sent_on: Option<u64>,
}

/// A running job host.
struct Live {
    cmds: mpsc::UnboundedSender<ToHost>,
    events: mpsc::UnboundedReceiver<HostEvent>,
    ready: jh::Ready,
    limits: Requested,
}

/// The job side's task.
struct Side {
    log: Log,
    hosts: Arc<dyn Hosts>,
    key: Option<Arc<MachineKey>>,
    results: Option<PathBuf>,
    jobs_build: String,
    rules: &'static Ruleset,
    local_cpu: bool,
    verifiable: bool,
    out: Option<mpsc::UnboundedSender<HelperFrame>>,
    machine: String,
    /// The connection's number (each `Connected` a new one).
    conn: u64,
    /// The CPU work last acknowledged.
    effective: Option<SiteCpu>,
    /// The limits a host should hold now (CPU work on in effect).
    want: Option<Requested>,
    host: Option<Live>,
    starting: Option<(Requested, tokio::task::JoinHandle<Result<HostStart, StartFailure>>)>,
    start_failed: Option<(Instant, String)>,
    /// What is held while a host starts (acknowledgements and what follows them), until it is
    /// ready or `hold_until`.
    held: VecDeque<Parked>,
    hold_until: Option<Instant>,
    /// Offers that came while a host starts, and when each came.
    waiting_offers: VecDeque<(JobOffer, Instant)>,
    /// The last acknowledgement sent on this connection (what an `again` repeats).
    last_ack: Option<SettingsAck>,
    runs: BTreeMap<String, Run>,
    results_held: BTreeMap<String, Held>,
    llm_busy: bool,
    bench: bool,
    paused: bool,
    /// The host is to be started again under `want` once its runs have ended.
    restart: bool,
    state: Arc<Mutex<Snapshot>>,
}

fn hardness(h: citar_jobs::limits::Hardness) -> Hardness {
    match h {
        citar_jobs::limits::Hardness::Hard => Hardness::Hard,
        citar_jobs::limits::Hardness::Soft => Hardness::Soft,
    }
}

fn requested(cpu: &SiteCpu) -> Requested {
    Requested { threads: cpu.threads, cpu_percent: cpu.cpu_percent, memory_mb: cpu.memory_mb }
}

fn round_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// A number the protocol carries: finite and not below 0.
fn figure(x: f64) -> f64 {
    if x.is_finite() { x.max(0.0) } else { 0.0 }
}

impl Side {
    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
        loop {
            let wake = self.next_wake();
            let host = &mut self.host;
            let host_event = async {
                match host.as_mut() {
                    Some(h) => h.events.recv().await,
                    None => std::future::pending().await,
                }
            };
            let starting = &mut self.starting;
            let started = async {
                match starting.as_mut() {
                    Some((_, task)) => task.await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(c) => self.command(c).await,
                    None => break,
                },
                ev = host_event => self.event(ev).await,
                done = started => {
                    let limits = self.starting.take().map(|(l, _)| l);
                    let done = done.unwrap_or_else(|e| Err(StartFailure {
                        limits: false,
                        message: format!("the job host's start failed: {e}"),
                    }));
                    self.started(limits, done).await;
                }
                () = sleep_until(wake) => self.tick().await,
            }
            self.publish();
        }
        // The helper is gone: so is its host (dropping its handle ends it).
        self.host = None;
    }

    /// The earliest of the runs' deadlines, the aborts' grace, the held acknowledgements' and the
    /// waiting offers' answers.
    fn next_wake(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut at = self.hold_until;
        let mut soonest = |t: Instant| at = Some(at.map_or(t, |a: Instant| a.min(t)));
        for r in self.runs.values() {
            let t = match r.phase {
                Phase::Running => r.deadline_at(now),
                Phase::Aborting(_, since) => Some(since + ABORT_GRACE),
                Phase::Offered => None,
            };
            if let Some(t) = t {
                soonest(t);
            }
        }
        for (o, came) in &self.waiting_offers {
            soonest(answer_by(o, *came));
        }
        for p in &self.held {
            if let Parked::Cmd(Cmd::Offer { offer, at: came, .. }) = p {
                soonest(answer_by(offer, *came));
            }
        }
        at
    }

    fn publish(&self) {
        let running: Vec<String> = self
            .runs
            .iter()
            .filter(|(_, r)| r.phase == Phase::Running)
            .map(|(id, _)| id.clone())
            .collect();
        let live = u32::try_from(self.runs.values().filter(|r| r.live()).count()).unwrap_or(0);
        let mut s = lock(&self.state);
        s.running = running;
        s.held = self.results_held.keys().take(HELLO_RESULTS_MAX).cloned().collect();
        s.jobs_running = live;
        s.jobs_paused = if self.paused { live } else { 0 };
        s.verifiable = self.verifiable;
        if let Some(h) = &self.host {
            s.limits = Some(Limits {
                cpu: hardness(h.ready.limits.cpu),
                memory: hardness(h.ready.limits.memory),
            });
            s.memory_per_run_mb = Some(h.ready.memory_per_run_mb);
        }
    }

    fn emit(&self, frame: HelperFrame) {
        if let Some(out) = &self.out {
            let _gone = out.send(frame);
        }
    }

    fn reject(&self, run: &str, reason: RejectReason, why: &str) {
        self.log.info(format!("refused run {} ({}): {why}", clip(run, 64), reason.as_str()));
        self.emit(HelperFrame::JobReject(JobReject { run: run.to_owned(), reason }));
    }

    /// Whether a command must wait for the host being started (an acknowledgement's limits, and
    /// what follows it).
    fn holding(&self) -> bool {
        self.starting.is_some() && self.hold_until.is_some()
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Connected { out, machine } => {
                self.forget_connection();
                self.conn += 1;
                self.out = Some(out);
                self.machine = machine;
            }
            Cmd::Disconnected => {
                self.out = None;
                self.forget_connection();
            }
            Cmd::Benchmark(on) => {
                self.bench = on;
                self.repause();
            }
            Cmd::LlmBusy(busy) => {
                self.llm_busy = busy;
                self.repause();
            }
            Cmd::Abort(a) => self.server_abort(&a),
            Cmd::ResultAck(run) => self.acknowledged(&run).await,
            Cmd::Stop(done) => {
                self.stop();
                let _told = done.send(());
            }
            other if self.holding() => self.held.push_back(Parked::Cmd(other)),
            Cmd::Settings { ack, cpu } => self.settings(ack, cpu),
            Cmd::Welcome(w) => self.welcome(w).await,
            Cmd::Offer { offer, quiet, at } => self.offer(offer, quiet, at),
        }
    }

    /// A connection ended (or another began): what its server asked and was not answered goes with
    /// it (the acknowledgements and the offers waiting for a host's start, its welcome: the next
    /// connection's hello and welcome say again what holds), and so does its `jobs_pause`, which
    /// the server says again after the next welcome while its benchmark lasts. Runs go on.
    fn forget_connection(&mut self) {
        let parked = self.held.len() + self.waiting_offers.len();
        if parked > 0 {
            self.log.debug(format!(
                "the connection's {parked} acknowledgement(s) and offer(s) waiting for the job host \
                 go with it"
            ));
        }
        self.held.clear();
        self.waiting_offers.clear();
        self.hold_until = None;
        self.last_ack = None;
        if self.bench {
            self.bench = false;
            self.repause();
        }
    }

    // ------------------------------------------------------------------ settings and the host

    fn settings(&mut self, ack: SettingsAck, cpu: SiteCpu) {
        let on = cpu.enabled && self.local_cpu;
        self.effective = Some(cpu.clone());
        let want = on.then(|| requested(&cpu));
        self.want = want;
        let fits = matches!((&self.host, want), (Some(h), Some(w)) if h.limits == w);
        if self.host.is_some() && !fits {
            if self.runs.is_empty() {
                // CPU work off, or other limits: the host goes now, and a new one starts below.
                self.log.info("closing the job host: CPU work changed");
                self.host = None;
                self.restart = false;
            } else {
                // ...or once its runs have ended; no run starts meanwhile.
                self.restart = true;
            }
        } else if fits {
            self.restart = false;
        }
        self.repause();
        if let Some(w) = want
            && self.host.is_none()
        {
            self.start_host(w);
        }
        if want.is_some() && self.starting.is_some() {
            // Sent once the host is ready (or could not start, or the hold is over), with its
            // figures; what comes after it waits behind it.
            if self.hold_until.is_none() {
                self.hold_until = Some(Instant::now() + ACK_HOLD);
            }
            self.held.push_back(Parked::Ack(ack));
            return;
        }
        self.send_ack(ack);
    }

    /// An acknowledgement's `limits` and `memory_per_run_mb`: the running host's, when one holds
    /// the effective limits; else the last host's hardness, and no figure.
    fn fill_ack(&self, ack: &mut SettingsAck) {
        let s = lock(&self.state);
        if let Some(l) = &s.limits {
            ack.limits = l.clone();
        }
        ack.memory_per_run_mb = match (&self.host, self.want) {
            (Some(h), Some(w)) if h.limits == w => Some(h.ready.memory_per_run_mb),
            _ => None,
        };
    }

    /// Sends an acknowledgement with the job host's figures as they are now, and keeps it as this
    /// connection's last.
    fn send_ack(&mut self, mut ack: SettingsAck) {
        self.fill_ack(&mut ack);
        ack.again = None;
        self.emit(HelperFrame::SettingsAck(ack.clone()));
        if self.out.is_some() {
            self.last_ack = Some(ack);
        }
    }

    /// A job host became ready: when the last acknowledgement on this connection did not carry its
    /// figures (it went before the host was ready, or the host was started again under new limits
    /// once its runs ended), it goes again with them, marked `again` (it answers no `settings`), so
    /// the server matches offers to the memory each run has now.
    fn reack(&mut self) {
        let Some(last) = &self.last_ack else { return };
        let mut ack = last.clone();
        self.fill_ack(&mut ack);
        if ack.memory_per_run_mb == last.memory_per_run_mb && ack.limits == last.limits {
            return;
        }
        self.log.info(format!(
            "acknowledging the settings again with the job host's figures ({})",
            ack.memory_per_run_mb.map_or_else(|| "none".to_owned(), |m| format!("{m} MB a run"))
        ));
        ack.again = Some(true);
        self.emit(HelperFrame::SettingsAck(ack.clone()));
        ack.again = None;
        self.last_ack = Some(ack);
    }

    fn start_host(&mut self, limits: Requested) {
        if self.starting.is_some() {
            return;
        }
        self.log.info(format!(
            "starting the job host: {} thread{}, {}% of the CPU, {} MB",
            limits.threads,
            if limits.threads == 1 { "" } else { "s" },
            limits.cpu_percent,
            limits.memory_mb
        ));
        let task = tokio::spawn(self.hosts.start(limits, self.jobs_build.clone()));
        self.starting = Some((limits, task));
    }

    async fn started(&mut self, limits: Option<Requested>, done: Result<HostStart, StartFailure>) {
        match done {
            Ok(h) => {
                let limits =
                    limits.unwrap_or(Requested { threads: 0, cpu_percent: 0, memory_mb: 0 });
                if h.ready.verifiable {
                    self.start_failed = None;
                } else {
                    self.log.warn(format!(
                        "the job host is not verifiable: {}; offers are refused",
                        clip(h.ready.reason.as_deref().unwrap_or("its self-check failed"), 300)
                    ));
                    self.verifiable = false;
                }
                for n in &h.ready.notes {
                    self.log.debug(format!("job host: {}", clip(n, 300)));
                }
                self.log.info(format!(
                    "the job host is ready: CPU {}, memory {}, {} MB a run",
                    hardness(h.ready.limits.cpu).as_str(),
                    hardness(h.ready.limits.memory).as_str(),
                    h.ready.memory_per_run_mb
                ));
                self.host = Some(Live { cmds: h.cmds, events: h.events, ready: h.ready, limits });
                self.start_failed = None;
                self.publish();
                if self.paused
                    && let Some(h) = &self.host
                {
                    let _sent = h.cmds.send(ToHost::Pause);
                }
                // A host started for limits no longer wanted goes at once.
                if self.want != Some(limits) && !self.runs.values().any(Run::live) {
                    self.host = None;
                    if let Some(w) = self.want {
                        self.start_host(w);
                        return;
                    }
                }
            }
            Err(f) => {
                self.log.warn(format!(
                    "the job host could not start{}: {}",
                    if f.limits { " under the limits" } else { "" },
                    clip(&f.message, 600)
                ));
                self.start_failed = Some((Instant::now(), f.message));
            }
        }
        self.release_held().await;
        if self.host.is_some() {
            self.reack();
        }
    }

    /// What was held for the host's start, in order: each acknowledgement sent, each command
    /// handled (a later `settings` applied in its turn, which may start another host and hold what
    /// follows it again); then the offers that waited for it.
    async fn release_held(&mut self) {
        self.hold_until = None;
        let mut parked = std::mem::take(&mut self.held);
        while let Some(p) = parked.pop_front() {
            if self.holding() {
                // A later `settings` started another host: the rest waits for it, in order.
                self.held.push_back(p);
                self.held.extend(parked);
                break;
            }
            match p {
                Parked::Ack(ack) => self.send_ack(ack),
                Parked::Cmd(cmd) => Box::pin(self.command(cmd)).await,
            }
        }
        while self.starting.is_none()
            && let Some((o, at)) = self.waiting_offers.pop_front()
        {
            self.offer(o, false, at);
        }
    }

    /// Offers waiting for a job host's start whose answer is due: refused `limits`, so the server
    /// has its answer within the offer's time to live.
    fn answer_waiting(&mut self, now: Instant) {
        let mut due = Vec::new();
        self.waiting_offers.retain(|(o, at)| {
            let late = answer_by(o, *at) <= now;
            if late {
                due.push(o.run.clone());
            }
            !late
        });
        self.held.retain(|p| match p {
            Parked::Cmd(Cmd::Offer { offer, at, .. }) if answer_by(offer, *at) <= now => {
                due.push(offer.run.clone());
                false
            }
            _ => true,
        });
        for run in due {
            self.reject(&run, RejectReason::Limits, NOT_READY_IN_TIME);
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        self.answer_waiting(now);
        if self.hold_until.is_some_and(|t| now >= t) {
            self.log.warn(format!(
                "the job host is not ready after {} s: acknowledging without its figures (they \
                 follow once it is)",
                ACK_HOLD.as_secs()
            ));
            self.release_held().await;
        }
        let mut overdue = Vec::new();
        let mut stuck = false;
        for (id, r) in &self.runs {
            match r.phase {
                Phase::Running if r.deadline_at(now).is_some_and(|t| now >= t) => {
                    overdue.push(id.clone());
                }
                Phase::Aborting(_, since) if now >= since + ABORT_GRACE => stuck = true,
                _ => {}
            }
        }
        for id in overdue {
            let Some(r) = self.runs.get_mut(&id) else { continue };
            let paused = r.paused_total(now);
            r.phase = Phase::Aborting(Why::Deadline, now);
            let message = format!(
                "past its deadline of {:.0} s ({paused:.0} s paused, not counted)",
                r.deadline_s
            );
            self.log.warn(format!("run {}: {message}: ending it", clip(&id, 64)));
            if let Some(h) = &self.host {
                let _sent = h.cmds.send(ToHost::JobAbort(jh::Abort {
                    run: id.clone(),
                    reason: "past its deadline".to_owned(),
                }));
            }
            self.emit(HelperFrame::JobFailed(JobFailed {
                run: id,
                code: FailCode::Deadline,
                message,
                retryable: true,
            }));
        }
        if stuck {
            self.log.warn(format!(
                "a run did not end within {} s of its abort: ending the job host",
                ABORT_GRACE.as_secs()
            ));
            self.end_host("the job host was ended: a run it was told to stop did not stop");
        }
    }

    /// Ends the host by the helper's own decision: its runs still live fail `host`.
    fn end_host(&mut self, why: &str) {
        self.host = None;
        let ids: Vec<String> = self.runs.keys().cloned().collect();
        for id in ids {
            let Some(r) = self.runs.remove(&id) else { continue };
            match r.phase {
                Phase::Offered | Phase::Running => self.emit(HelperFrame::JobFailed(JobFailed {
                    run: id,
                    code: FailCode::Host,
                    message: why.to_owned(),
                    retryable: true,
                })),
                Phase::Aborting(Why::Server, _) => {
                    self.emit(HelperFrame::JobAborted(RunRef { run: id }));
                }
                Phase::Aborting(..) => {}
            }
        }
        self.after_runs_end();
    }

    /// Once every run has ended: a host to be started again under new limits is.
    fn after_runs_end(&mut self) {
        if !self.restart || !self.runs.is_empty() {
            return;
        }
        self.restart = false;
        self.host = None;
        if let Some(w) = self.want {
            self.log.info("the job host's runs have ended: starting it again under the new limits");
            self.start_host(w);
        }
    }

    fn repause(&mut self) {
        let yield_on = self.effective.as_ref().is_none_or(|c| c.yield_to_llm);
        let paused = self.bench || (yield_on && self.llm_busy);
        if paused == self.paused {
            return;
        }
        self.paused = paused;
        let now = Instant::now();
        for r in self.runs.values_mut() {
            if paused {
                if r.phase == Phase::Running {
                    r.paused_since = Some(now);
                }
            } else if let Some(t) = r.paused_since.take() {
                r.paused_s += now.duration_since(t).as_secs_f64();
            }
        }
        if let Some(h) = &self.host {
            let _sent = h.cmds.send(if paused { ToHost::Pause } else { ToHost::Resume });
        }
        self.log.info(if paused {
            if self.bench {
                "pausing the games: a benchmark holds this machine"
            } else {
                "pausing the games while the model works"
            }
        } else {
            "the games go on"
        });
    }

    // ------------------------------------------------------------------ offers and runs

    fn offer(&mut self, o: JobOffer, quiet: bool, at: Instant) {
        let run = o.run.clone();
        let cpu = self.effective.clone();
        let on = self.local_cpu && cpu.as_ref().is_some_and(|c| c.enabled);
        if !on {
            let why = if self.local_cpu {
                "CPU work is off in this machine's settings"
            } else {
                "its owner has not lent this machine's CPU"
            };
            return self.reject(&run, RejectReason::Disabled, why);
        }
        if quiet {
            return self.reject(&run, RejectReason::Quiet, "its local quiet hours");
        }
        if !self.verifiable {
            return self.reject(
                &run,
                RejectReason::Unverifiable,
                "its canary or self-check failed here",
            );
        }
        if o.jobs_build != self.jobs_build {
            return self.reject(
                &run,
                RejectReason::Build,
                &format!(
                    "the job is for jobs build {}, this helper's is {}",
                    o.jobs_build, self.jobs_build
                ),
            );
        }
        if !cpu.as_ref().is_some_and(|c| c.work.contains(&o.kind)) {
            return self.reject(&run, RejectReason::Kind, "not among the kinds of work it takes");
        }
        if self.runs.contains_key(&run) {
            return self.reject(&run, RejectReason::BadSpec, "a run it is already playing");
        }
        let Some(kind) = citar_jobs::Kind::from_name(o.kind.as_str()) else {
            return self.reject(&run, RejectReason::BadSpec, "an unknown kind");
        };
        let checked = match citar_jobs::check_spec(
            self.rules,
            kind,
            &Value::Object(o.spec.clone()),
            Some(&o.spec_hash),
        ) {
            Ok(c) => c,
            Err(e) => return self.reject(&run, RejectReason::BadSpec, &clip(&e.0, 300)),
        };
        if answer_by(&o, at) <= Instant::now() {
            return self.reject(&run, RejectReason::Limits, NOT_READY_IN_TIME);
        }
        if self.starting.is_some() {
            self.waiting_offers.push_back((o, at));
            return;
        }
        let Some(host) = &self.host else {
            if let Some((failed, why)) = &self.start_failed
                && failed.elapsed() < START_RETRY
            {
                let why = format!("its job host could not start: {}", clip(why, 200));
                return self.reject(&run, RejectReason::Limits, &why);
            }
            match self.want {
                Some(w) => {
                    self.start_host(w);
                    self.waiting_offers.push_back((o, at));
                }
                None => self.reject(&run, RejectReason::Limits, "no job host runs"),
            }
            return;
        };
        if o.mem_mb > host.ready.memory_per_run_mb {
            let why = format!(
                "it needs {} MB a run, and the job host leaves each run {} MB",
                o.mem_mb, host.ready.memory_per_run_mb
            );
            return self.reject(&run, RejectReason::Memory, &why);
        }
        // A run being ended still holds its thread until the host lets it go.
        let live = self.runs.len();
        let threads = usize::try_from(host.limits.threads).unwrap_or(0);
        if live >= threads || self.restart {
            let why = format!(
                "{live} run{} on {threads} thread{}",
                if live == 1 { "" } else { "s" },
                if threads == 1 { "" } else { "s" }
            );
            return self.reject(&run, RejectReason::Busy, &why);
        }
        let sent = host.cmds.send(ToHost::JobOffer(jh::Offer {
            run: run.clone(),
            job: Some(o.job.clone()),
            kind: o.kind.as_str().to_owned(),
            spec: Value::Object(o.spec),
            spec_hash: o.spec_hash.clone(),
            jobs_build: o.jobs_build.clone(),
            deadline_s: o.deadline_s,
            mem_mb: o.mem_mb,
            priority: i64::from(o.priority),
        }));
        if sent.is_err() {
            return self.reject(&run, RejectReason::Limits, "its job host is gone");
        }
        self.runs.insert(
            run,
            Run {
                job: o.job,
                kind: o.kind,
                spec_hash: o.spec_hash,
                jobs_build: o.jobs_build,
                bots: checked.bots(),
                machine: self.machine.clone(),
                deadline_s: o.deadline_s,
                phase: Phase::Offered,
                accepted: None,
                paused_s: 0.0,
                paused_since: None,
            },
        );
    }

    fn server_abort(&mut self, a: &JobAbort) {
        let now = Instant::now();
        match self.runs.get_mut(&a.run) {
            Some(r) if r.live() => {
                r.phase = Phase::Aborting(Why::Server, now);
                self.log.info(format!(
                    "the server aborted run {}: {}",
                    clip(&a.run, 64),
                    clip(&a.reason, 200)
                ));
                if let Some(h) = &self.host {
                    let _sent = h.cmds.send(ToHost::JobAbort(jh::Abort {
                        run: a.run.clone(),
                        reason: clip(&a.reason, 200),
                    }));
                }
            }
            Some(_) => {}
            None => {
                // An offer still waiting for a job host's start (its time to live passed on the
                // server's clock first): it goes, unanswered, as the server asked.
                let before = self.waiting_offers.len() + self.held.len();
                self.waiting_offers.retain(|(o, _)| o.run != a.run);
                self.held.retain(
                    |p| !matches!(p, Parked::Cmd(Cmd::Offer { offer, .. }) if offer.run == a.run),
                );
                if self.waiting_offers.len() + self.held.len() < before {
                    self.log.info(format!(
                        "the server withdrew its offer of run {} before the job host was ready",
                        clip(&a.run, 64)
                    ));
                } else {
                    self.log.debug(format!(
                        "a job_abort for {}, which is not playing",
                        clip(&a.run, 64)
                    ));
                }
            }
        }
    }

    fn stop(&mut self) {
        let now = Instant::now();
        // Offers waiting for a job host's start are answered: this machine lends nothing now.
        let waiting: Vec<String> = self
            .waiting_offers
            .drain(..)
            .map(|(o, _)| o.run)
            .chain(self.held.iter().filter_map(|p| match p {
                Parked::Cmd(Cmd::Offer { offer, .. }) => Some(offer.run.clone()),
                _ => None,
            }))
            .collect();
        self.held.clear();
        self.hold_until = None;
        for run in waiting {
            self.reject(&run, RejectReason::Disabled, "the helper is stopping");
        }
        let ids: Vec<String> = self.runs.keys().cloned().collect();
        for id in ids {
            let Some(r) = self.runs.get_mut(&id) else { continue };
            if !r.live() {
                continue;
            }
            r.phase = Phase::Aborting(Why::Stopping, now);
            self.emit(HelperFrame::JobFailed(JobFailed {
                run: id,
                code: FailCode::AbortedLocally,
                message: "the helper is stopping: its owner stopped lending this machine's CPU"
                    .to_owned(),
                retryable: true,
            }));
        }
        self.host = None;
        self.runs.clear();
    }

    async fn welcome(&mut self, w: Welcomed) {
        let now = Instant::now();
        for id in &w.running {
            if w.keep.contains(id) {
                continue;
            }
            if let Some(r) = self.runs.get_mut(id)
                && r.live()
            {
                self.log.info(format!("the server gave run {} elsewhere: ending it", clip(id, 64)));
                r.phase = Phase::Aborting(Why::Dropped, now);
                if let Some(h) = &self.host {
                    let _sent = h.cmds.send(ToHost::JobAbort(jh::Abort {
                        run: id.clone(),
                        reason: "the server gave it elsewhere".to_owned(),
                    }));
                }
            }
        }
        if !w.drop.is_empty() {
            self.log.debug(format!("the welcome drops {} run(s)", w.drop.len()));
        }
        let held: Vec<String> = self.results_held.keys().cloned().collect();
        for run in held {
            let listed = w.results.contains(&run);
            if listed && !w.resend.contains(&run) {
                self.log.info(format!(
                    "the server no longer wants the result of run {}: deleting it",
                    clip(&run, 64)
                ));
                self.forget_result(&run).await;
                continue;
            }
            self.send_result(&run).await;
        }
    }

    // ------------------------------------------------------------------ the host's frames

    async fn event(&mut self, ev: Option<HostEvent>) {
        match ev {
            Some(HostEvent::Frame(f)) => self.host_frame(f).await,
            Some(HostEvent::Died { death, failures }) => self.died(&death, failures),
            None => self.died("its connection to the helper ended", Vec::new()),
        }
    }

    fn died(&mut self, death: &str, failures: Vec<(jh::Failed, Option<f64>)>) {
        self.log.warn(format!("the job host ended: {}", clip(death, 600)));
        self.host = None;
        for (f, peak) in failures {
            let Some(r) = self.runs.remove(&f.run) else { continue };
            match r.phase {
                Phase::Offered | Phase::Running => {
                    let code = if f.code == jh::FailCode::Memory {
                        FailCode::Memory
                    } else {
                        FailCode::Host
                    };
                    let mut message = f.message.clone();
                    if let Some(p) = peak {
                        message.push_str(&format!(" (its peak reported: {p:.1} MB)"));
                    }
                    self.emit(HelperFrame::JobFailed(JobFailed {
                        run: f.run,
                        code,
                        message: clip(&message, 4000),
                        retryable: true,
                    }));
                }
                Phase::Aborting(Why::Server, _) => {
                    self.emit(HelperFrame::JobAborted(RunRef { run: f.run }));
                }
                Phase::Aborting(..) => {}
            }
        }
        // Runs the host did not name (it died before it took them): they fail as it did.
        let rest: Vec<String> = self.runs.keys().cloned().collect();
        for id in rest {
            if let Some(r) = self.runs.remove(&id)
                && r.live()
            {
                self.emit(HelperFrame::JobFailed(JobFailed {
                    run: id,
                    code: FailCode::Host,
                    message: clip(death, 4000),
                    retryable: true,
                }));
            }
        }
        self.restart = false;
    }

    async fn host_frame(&mut self, f: FromHost) {
        match f {
            FromHost::JobAccept(r) => {
                let now = Instant::now();
                let paused = self.paused;
                if let Some(run) = self.runs.get_mut(&r.run)
                    && run.phase == Phase::Offered
                {
                    run.phase = Phase::Running;
                    run.accepted = Some(now);
                    if paused {
                        run.paused_since = Some(now);
                    }
                    self.emit(HelperFrame::JobAccept(RunRef { run: r.run }));
                }
            }
            FromHost::JobReject(r) => {
                if self.runs.remove(&r.run).is_some() {
                    let reason = match r.reason {
                        jh::RejectReason::Unverifiable => RejectReason::Unverifiable,
                        jh::RejectReason::Build => RejectReason::Build,
                        jh::RejectReason::BadSpec => RejectReason::BadSpec,
                        jh::RejectReason::Memory => RejectReason::Memory,
                        jh::RejectReason::Busy => RejectReason::Busy,
                    };
                    self.reject(
                        &r.run,
                        reason,
                        &format!("its job host: {}", clip(&r.message, 300)),
                    );
                    self.after_runs_end();
                }
            }
            FromHost::JobProgress(p) => {
                let paused = self.paused;
                if let Some(r) = self.runs.get(&p.run)
                    && r.phase == Phase::Running
                {
                    self.emit(HelperFrame::JobProgress(JobProgress {
                        run: p.run,
                        round: round_of(p.round),
                        of: round_of(p.of),
                        elapsed_s: figure(p.elapsed_s),
                        cpu_s: figure(p.cpu_s),
                        paused_s: figure(p.paused_s),
                        peak_mb: figure(p.peak_mb),
                        paused: Some(p.paused || paused),
                    }));
                }
            }
            FromHost::JobResult(r) => {
                let Some(run) = self.runs.remove(&r.run) else { return };
                if run.live() {
                    self.result(&r.run, &run, r.outcome).await;
                } else if matches!(run.phase, Phase::Aborting(Why::Server, _)) {
                    self.emit(HelperFrame::JobAborted(RunRef { run: r.run }));
                }
                self.after_runs_end();
            }
            FromHost::JobFailed(f) => {
                let Some(run) = self.runs.remove(&f.run) else { return };
                match run.phase {
                    Phase::Offered | Phase::Running => {
                        let code = match f.code {
                            jh::FailCode::Memory => FailCode::Memory,
                            jh::FailCode::Deadline => FailCode::Deadline,
                            jh::FailCode::Host => FailCode::Host,
                            jh::FailCode::Internal => FailCode::Internal,
                        };
                        self.log.info(format!(
                            "run {} failed ({}): {}",
                            clip(&f.run, 64),
                            code.as_str(),
                            clip(&f.message, 300)
                        ));
                        self.emit(HelperFrame::JobFailed(JobFailed {
                            run: f.run,
                            code,
                            message: clip(&f.message, 4000),
                            retryable: f.retryable,
                        }));
                    }
                    Phase::Aborting(Why::Server, _) => {
                        self.emit(HelperFrame::JobAborted(RunRef { run: f.run }));
                    }
                    Phase::Aborting(..) => {}
                }
                self.after_runs_end();
            }
            FromHost::JobAborted(r) => {
                if let Some(run) = self.runs.remove(&r.run)
                    && matches!(
                        run.phase,
                        Phase::Aborting(Why::Server, _) | Phase::Offered | Phase::Running
                    )
                {
                    self.emit(HelperFrame::JobAborted(RunRef { run: r.run }));
                }
                self.after_runs_end();
            }
            FromHost::Heartbeat(_) | FromHost::Ready(_) => {}
            FromHost::Fatal(f) => {
                self.log.warn(format!("the job host: {}", clip(&f.message, 300)));
            }
        }
    }

    // ------------------------------------------------------------------ results

    /// A run's result: its claim signed, written to the home, then sent.
    async fn result(&mut self, id: &str, run: &Run, outcome: jh::Outcome) {
        let Value::Object(result) = outcome.result else {
            self.log.error(format!("run {}: the job host's result is no object", clip(id, 64)));
            self.emit(HelperFrame::JobFailed(JobFailed {
                run: id.to_owned(),
                code: FailCode::Internal,
                message: "the job host's result was no object".to_owned(),
                retryable: true,
            }));
            return;
        };
        let Some(key) = &self.key else {
            self.log.error("no machine key to sign a result with");
            return;
        };
        let claim = citar_jobs::Claim {
            v: citar_jobs::claim::CLAIM_VERSION,
            job: run.job.clone(),
            run: id.to_owned(),
            machine: run.machine.clone(),
            kind: run.kind.as_str().to_owned(),
            spec_hash: run.spec_hash.clone(),
            jobs_build: run.jobs_build.clone(),
            bots: run.bots.clone(),
            result_hash: outcome.result_hash.clone(),
            chain_head: outcome.chain.head.clone(),
            rounds: u64::from(outcome.chain.rounds),
            key: key.public_b64(),
        };
        let signature = key.sign_b64(&claim.message());
        let frame = JobResult {
            run: id.to_owned(),
            outcome: Outcome {
                result,
                result_hash: outcome.result_hash,
                chain: Chain {
                    head: outcome.chain.head,
                    rounds: u64::from(outcome.chain.rounds),
                    digests: outcome.chain.digests,
                },
                cpu_s: figure(outcome.cpu_s),
                wall_s: figure(outcome.wall_s),
                paused_s: figure(outcome.paused_s),
                peak_mb: figure(outcome.peak_mb),
            },
            claim: Claim {
                v: claim.v,
                job: claim.job,
                run: claim.run,
                machine: claim.machine,
                kind: run.kind,
                spec_hash: claim.spec_hash,
                jobs_build: claim.jobs_build,
                bots: claim.bots,
                result_hash: claim.result_hash,
                chain_head: claim.chain_head,
                rounds: claim.rounds,
                key: claim.key,
            },
            signature,
        };
        self.log.info(format!(
            "run {} done: {} rounds in {:.1} s",
            clip(id, 64),
            frame.outcome.chain.rounds,
            frame.outcome.wall_s
        ));
        self.keep_result(&frame).await;
        self.results_held.insert(id.to_owned(), Held { frame: Some(frame), sent_on: None });
        self.send_result(id).await;
    }

    /// Writes a result to `helper/results/<run>.json` (beside it first, then renamed over), within
    /// [`RESULT_IO`].
    async fn keep_result(&self, frame: &JobResult) {
        let Some(dir) = self.results.clone() else { return };
        let Ok(text) = serde_json::to_vec(frame) else { return };
        let run = frame.run.clone();
        let write = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{run}.json"));
            let tmp = dir.join(format!("{run}.json.tmp"));
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        });
        match tokio::time::timeout(RESULT_IO, write).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => self.log.warn(format!("could not keep a result on disk: {e}")),
            _ => self.log.warn("could not keep a result on disk in time"),
        }
    }

    /// Sends a held result on the current connection, unless it already went on it.
    async fn send_result(&mut self, run: &str) {
        if self.out.is_none() {
            return;
        }
        let conn = self.conn;
        let need_read = match self.results_held.get(run) {
            Some(h) if h.sent_on == Some(conn) => return,
            Some(h) => h.frame.is_none(),
            None => return,
        };
        if need_read {
            let frame = self.read_result(run).await;
            if let Some(h) = self.results_held.get_mut(run) {
                h.frame = frame;
            }
        }
        let Some(h) = self.results_held.get_mut(run) else { return };
        let Some(frame) = h.frame.clone() else {
            self.log.warn(format!(
                "the held result of run {} does not read: deleting it",
                clip(run, 64)
            ));
            self.forget_result(run).await;
            return;
        };
        h.sent_on = Some(conn);
        self.emit(HelperFrame::JobResult(frame));
    }

    async fn read_result(&self, run: &str) -> Option<JobResult> {
        let path = self.results.as_ref()?.join(format!("{run}.json"));
        let read = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            let len = std::fs::metadata(&path).ok()?.len();
            if len > RESULT_FILE_MAX {
                return None;
            }
            std::fs::read(&path).ok()
        });
        let bytes = tokio::time::timeout(RESULT_IO, read).await.ok()?.ok()??;
        let frame: JobResult = serde_json::from_slice(&bytes).ok()?;
        (frame.run == run).then_some(frame)
    }

    async fn forget_result(&mut self, run: &str) {
        self.results_held.remove(run);
        let Some(dir) = self.results.clone() else { return };
        let path = dir.join(format!("{run}.json"));
        let gone = tokio::task::spawn_blocking(move || std::fs::remove_file(path));
        match tokio::time::timeout(RESULT_IO, gone).await {
            Ok(Ok(Ok(()) | Err(_))) => {}
            _ => self.log.warn("could not delete a held result in time"),
        }
    }

    async fn acknowledged(&mut self, run: &str) {
        if self.results_held.contains_key(run) {
            self.log.debug(format!("run {}'s result is acknowledged", clip(run, 64)));
            self.forget_result(run).await;
        }
    }
}

/// The results a previous run of the helper left in `helper/results/` (`<run>.json`, at most
/// 1,024): held, to be listed in the next hello, and read when they are sent.
async fn held_results(dir: Option<PathBuf>, log: &Log) -> Vec<String> {
    let Some(dir) = dir else { return Vec::new() };
    let list = tokio::task::spawn_blocking(move || -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut runs: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let run = name.strip_suffix(".json")?.to_owned();
                is_run_id(&run).then_some(run)
            })
            .collect();
        runs.sort();
        runs.truncate(1024);
        runs
    });
    let runs =
        tokio::time::timeout(RESULT_IO, list).await.ok().and_then(Result::ok).unwrap_or_default();
    if !runs.is_empty() {
        log.info(format!(
            "{} result{} held from before, kept until the server says",
            runs.len(),
            if runs.len() == 1 { "" } else { "s" }
        ));
    }
    runs
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// A run id as the protocol writes them (`cr_` and up to 61 of `A-Z a-z 0-9 _ -`).
#[must_use]
pub fn is_run_id(run: &str) -> bool {
    run.starts_with("cr_")
        && run.len() <= 64
        && run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The hardness a helper reports before any job host has said: soft, so nothing is claimed hard
/// before it is.
pub const UNKNOWN_LIMITS: Limits = Limits { cpu: Hardness::Soft, memory: Hardness::Soft };
