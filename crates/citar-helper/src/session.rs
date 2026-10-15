//! The connection (design/phase4-helper.md P4.3.3; PROTOCOL.md "The handshake", "Close codes",
//! "Keep-alive and shutdown"): one task owns the socket, reads every frame and writes every
//! frame, so a slow piece of work (a model listing) never holds up a ping.
//!
//! - **Each connection:** list the endpoint's models and read the hardware report, dial, send the
//!   `hello`, then act on the server's answer: `welcome` (acknowledge its settings, follow its
//!   jobs), `refused`, protocol 1's `error` (a server that does not speak protocol 2), or a close
//!   with no frame, whose code alone decides.
//! - **Reactions** (part of the contract): a permanent refusal (`refused` `token` or `key`, any
//!   `shutdown`, a close 4401, 4403 or 4409) exits 78 from a console; with `--service` the helper
//!   idles, logs once and tries again hourly, so no service manager restarts it into a loop. A
//!   server that does not speak protocol 2 (`refused` `protocol` or `build`, protocol 1's
//!   `error`, a close 4426) is tried again every 10 minutes, never exited on. `refused` `rate` (or
//!   a close 4429) waits `retry_after_s` (60 s without it). Anything else reconnects with backoff:
//!   2 s doubling to 60 s, up to 30% jitter added, reset by a `welcome`.
//! - **While connected:** a `pong` for every `ping`; the helper's own `ping` after 45 s without a
//!   frame, and the connection dropped after 70 s; `settings_ack` for every `settings`; `catalog`
//!   when the endpoint's models or loaded set change (polled every 30 s; a listing that fails is
//!   not a change until [`FAILED_LISTINGS`] fail in a row); `status` when the local quiet hours
//!   begin or end or the completions in flight change (at most every 5 s); `hardware` after a
//!   `rescan`, read off the session's task. Until 4-10 it takes no job (`job_reject` `build`: it
//!   has no jobs build).
//! - **Completions** ([`crate::proxy`]): each `request` the local policy lets through runs as a
//!   task of its own; local policy holds whatever the server asks, in this order: a model outside
//!   the owner's allowlist (or no endpoint, `--no-llm`) is `unknown_model`, the local quiet hours
//!   `closed`, the local slots full `busy`. A `cancel` aborts the task, which closes its connection
//!   to the endpoint, and only then is `cancelled` sent; a cancel that crosses a `done` or an
//!   `error` is ignored. **A lost connection drops every completion in flight** (their connections
//!   to the endpoint with them, P4.2.4), so none holds the model or a slot for an answer nobody
//!   will read; a stop answers each `stopping` before the socket closes.
//! - **A hostile server** (P4.6, threat A) cannot make the helper hold more than it bounds: a
//!   frame type it invents or a value it fills is quoted cut to a few hundred characters
//!   ([`SERVER_TEXT`]), a bad frame's throttling key is one of the known types or `?`, and every
//!   wait has a timeout or the stop signal (P4.7.4).
//!
//! Replaces `citar/worker/agent.py` `session` and `run` (lines 343-456).

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::clock::Clock;
use crate::config::{Ceiling, Config};
use crate::llm::{self, Listed, SharedEndpoint};
use crate::log::{Level, Log, clip};
use crate::net::{Dialer, Ws};
use crate::protocol::{
    self, Ceiling as WireCeiling, CpuInfo, ErrorCode, FrameSet, HelloHardwareSource, HelperFrame,
    HelperInfo, JobReject, LlmInfo, MAX_FRAME_BYTES, MachineInfo, Model, Parsed, ProtocolError,
    RefusedCode, RejectReason, Report, Request, RequestError, RequestErrorCode, ServerFrame,
    Settings, Status,
};
use crate::proxy::{self, Finished, Flights, InFlight, Outcome};
use crate::settings::{self, Local};

/// The first reconnect delay.
pub const BACKOFF_START: Duration = Duration::from_secs(2);
/// The longest reconnect delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Jitter added to each delay, as a share of it.
pub const JITTER: f64 = 0.3;
/// A permanent refusal in service mode: try again after this.
pub const PERMANENT_RETRY: Duration = Duration::from_secs(3600);
/// A server that does not speak protocol 2: try again after this.
pub const UPGRADE_RETRY: Duration = Duration::from_secs(600);
/// `refused` `rate` with no delay given, or a close 4429.
pub const RATE_RETRY: Duration = Duration::from_secs(60);
/// How long the server may take to answer the hello.
pub const HELLO_ANSWER: Duration = Duration::from_secs(30);
/// After a refusal or a shutdown, how long to wait for the server's close.
pub const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Silence after which the helper sends its own ping.
pub const PING_AFTER: Duration = Duration::from_secs(45);
/// Silence after which the connection counts as dead.
pub const STALE_AFTER: Duration = Duration::from_secs(70);
/// How often the endpoint's models are listed again.
pub const CATALOG_EVERY: Duration = Duration::from_secs(30);
/// The fewest seconds between two `status` frames.
pub const STATUS_EVERY: Duration = Duration::from_secs(5);
/// How often the quiet hours are looked at (the wall clock can be set, so no timer is trusted).
pub const QUIET_TICK: Duration = Duration::from_secs(1);
/// How long one write may take before the connection counts as dead.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the helper's own close frame may take to go when it ends a connection.
pub const CLOSE_SEND: Duration = Duration::from_secs(2);
/// Listings in a row that must fail (or time out) before the helper reports its endpoint's
/// models as none: one failed or slow listing is a hiccup, not an endpoint gone (90 s at the
/// 30 s poll).
pub const FAILED_LISTINGS: u32 = 3;
/// How long reading the hardware file may take: a file on a share that does not answer must not
/// hold up a connection.
pub const HARDWARE_READ: Duration = Duration::from_secs(5);
/// The most characters of the server's own text (a refusal's message, a bad frame's error, a
/// list of runs) quoted in a log line.
pub const SERVER_TEXT: usize = 300;
/// The most characters of a frame type the helper does not know quoted in a log line.
pub const TYPE_SHOWN: usize = 64;

/// Until 4-10 the helper runs no job: its hello's `cpu.enabled` is false (it has no jobs build,
/// so it is never offered one), whatever the machine's own switch says.
pub const RUNS_JOBS: bool = false;

/// How the helper ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Stopped by the user or the service manager: exit code 0.
    Stopped,
    /// A permanent refusal from a console run: exit code 78.
    Refused,
}

impl Exit {
    /// The process's exit code (P4.3.9).
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Exit::Stopped => 0,
            Exit::Refused => 78,
        }
    }
}

/// What the hello says about the machine that the session does not work out itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    /// The engine build id (`citar_bot::build_id`).
    pub build: String,
    pub version: String,
    pub target: String,
    pub label: String,
    /// The bot versions compiled in, sorted.
    pub bots: Vec<String>,
    /// The ruleset's id, 64 hex digits.
    pub ruleset: String,
}

/// Everything one helper runs with.
pub struct Helper {
    pub config: Config,
    pub public_key: String,
    pub facts: Facts,
    pub ceiling: Ceiling,
    pub log: Log,
    pub clock: Clock,
    pub dialer: Arc<dyn Dialer>,
    /// The model endpoint; `None` with `--no-llm`.
    pub endpoint: Option<SharedEndpoint>,
    /// Service mode: a permanent refusal idles instead of exiting.
    pub service: bool,
    /// Turns true when the helper must stop.
    pub stop: watch::Receiver<bool>,
    /// The completions whose tasks are alive (the helper's `in_flight`, read by tests).
    pub in_flight: InFlight,
}

/// How one connection ended.
#[derive(Clone, Debug, PartialEq)]
enum End {
    Stopped,
    Reconnect { welcomed: bool },
    Permanent(String),
    Upgrade(String),
    Rate(Duration),
}

/// Reconnect delays: 2 s doubling to 60 s, each with up to 30% jitter added.
#[derive(Debug)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { next: BACKOFF_START }
    }
}

impl Backoff {
    /// The delay to wait now; the one after it doubles.
    pub fn delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        d.mul_f64(1.0 + JITTER * random_fraction())
    }

    /// Back to the first delay (a welcome).
    pub fn reset(&mut self) {
        self.next = BACKOFF_START;
    }
}

/// Resolves when the helper must stop; never, once nobody can tell it to (the sender is gone).
async fn stop_signal(mut stop: watch::Receiver<bool>) {
    if stop.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// A random number in `[0, 1)` from the OS, for jitter (0 when it has none to give).
fn random_fraction() -> f64 {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        return 0.0;
    }
    f64::from(u32::from_le_bytes(b)) / (f64::from(u32::MAX) + 1.0)
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One listing of the endpoint's models, within [`llm::LIST_TIMEOUT`].
async fn list(ep: &SharedEndpoint) -> Result<Vec<Listed>, String> {
    match tokio::time::timeout(llm::LIST_TIMEOUT, ep.list()).await {
        Ok(listed) => listed,
        Err(_) => Err(format!("no answer within {} s", llm::LIST_TIMEOUT.as_secs())),
    }
}

/// What the helper has told the server of its endpoint's models (the hello's list, then each
/// `catalog`), kept across connections. A listing that fails or times out is not a change of the
/// list or the loaded set (P4.2.3): only an answer changes it, or [`FAILED_LISTINGS`] failures in
/// a row, after which the endpoint counts as gone and the models as none (an allowlist's, not
/// loaded), as when it was never reached.
struct Catalog {
    allow: Option<Vec<String>>,
    models: Vec<Model>,
    /// Listings in a row that failed.
    failures: u32,
}

impl Catalog {
    fn new(allow: Option<Vec<String>>) -> Catalog {
        let models = llm::choose(&[], allow.as_deref());
        Catalog { allow, models, failures: 0 }
    }

    /// Takes one listing; true when the models to report changed.
    fn take(&mut self, listed: Result<Vec<Listed>, String>, h: &Helper) -> bool {
        let next = match listed {
            Ok(l) => {
                self.failures = 0;
                llm::choose(&l, self.allow.as_deref())
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                h.log.throttled(
                    "models",
                    Duration::from_secs(600),
                    Level::Warn,
                    format!(
                        "could not list the models at {}: {}",
                        h.config.base_url,
                        clip(&e, 2 * SERVER_TEXT)
                    ),
                );
                if self.failures < FAILED_LISTINGS {
                    return false;
                }
                let none = llm::choose(&[], self.allow.as_deref());
                if none != self.models {
                    h.log.warn(format!(
                        "{} could not be listed {} times in a row: reporting no model loaded \
                         until it answers",
                        h.config.base_url, self.failures
                    ));
                }
                none
            }
        };
        if next == self.models {
            return false;
        }
        self.models = next;
        true
    }
}

/// The hardware report: the owner's file (read again each time, UTF-16 accepted), or nothing;
/// the scan itself is 4-06's. The read runs on a blocking thread for at most
/// [`HARDWARE_READ`]: a file that cannot be read in time reports nothing, logged.
async fn read_report(collect: bool, file: String, log: Log) -> Report {
    let none = Report { hardware: None, hardware_source: HelloHardwareSource::Nothing };
    if !collect || file.is_empty() {
        return none;
    }
    let path = std::path::PathBuf::from(&file);
    let read = tokio::task::spawn_blocking(move || read_hardware_file(&path));
    match tokio::time::timeout(HARDWARE_READ, read).await {
        Ok(Ok(Ok(scan))) => {
            Report { hardware: Some(scan), hardware_source: HelloHardwareSource::File }
        }
        Ok(Ok(Err(e))) => {
            log.throttled("hardware", Duration::from_secs(3600), Level::Warn, e);
            none
        }
        Ok(Err(e)) => {
            log.warn(format!("reading the hardware file failed: {e}"));
            none
        }
        Err(_) => {
            log.throttled(
                "hardware",
                Duration::from_secs(3600),
                Level::Warn,
                format!(
                    "the hardware file {file} could not be read within {} s: reporting none",
                    HARDWARE_READ.as_secs()
                ),
            );
            none
        }
    }
}

/// A protocol error for the log, with what came from the server cut.
fn describe(e: &ProtocolError) -> String {
    let at = if e.path.is_empty() { "the frame".to_owned() } else { clip(&e.path, TYPE_SHOWN) };
    format!("{} at {at}: {}", e.code, clip(&e.message, SERVER_TEXT))
}

impl Helper {
    /// The model side as this helper allows it.
    fn local(&self) -> Local {
        let slots = if self.config.no_llm { 0 } else { self.config.max_concurrent };
        Local {
            serves_llm: !self.config.no_llm,
            slots,
            cpu_enabled: self.config.cpu.enabled,
            ceiling: self.ceiling.clone(),
            limits: settings::UNTIL_THE_JOB_HOST,
        }
    }

    /// The quiet hours' end, as Unix time, when they hold now.
    fn quiet_until(&self) -> Option<f64> {
        let q = crate::quiet::Quiet(self.config.quiet.clone());
        q.until(self.clock.now_local()).map(|t| self.clock.unix_of(t))
    }

    /// Before each connection: list the endpoint's models into `catalog` and read the hardware
    /// report, each within its timeout.
    async fn prepare(&self, catalog: &mut Catalog) -> Report {
        if let Some(ep) = &self.endpoint {
            catalog.take(list(ep).await, self);
        }
        self.report().await
    }

    /// The hardware report as this helper is set up to give it.
    fn report(&self) -> impl std::future::Future<Output = Report> + Send + 'static {
        read_report(
            self.config.collect_hardware,
            self.config.hardware_file.clone(),
            self.log.clone(),
        )
    }

    fn hello(&self, models: Vec<Model>, report: Report) -> protocol::Hello {
        let f = &self.facts;
        let llm_on = !self.config.no_llm;
        protocol::Hello {
            protocol: protocol::PROTOCOL,
            token: self.config.token.clone(),
            helper: HelperInfo {
                version: f.version.clone(),
                build: f.build.clone(),
                jobs_build: None,
                target: f.target.clone(),
                label: cut(&f.label, 200),
            },
            machine: MachineInfo {
                hostname: cut(&f.hostname, 255),
                name: cut(&self.config.name, 120),
                os: f.os.clone(),
                arch: f.arch.clone(),
                public_key: self.public_key.clone(),
            },
            llm: LlmInfo {
                enabled: llm_on,
                // The endpoint's kind whenever the helper has one: `null` with `--no-llm` only.
                provider: (!self.config.no_llm).then_some(self.config.provider),
                slots: if self.config.no_llm { 0 } else { self.config.max_concurrent },
                stream: self.config.stream,
                models,
            },
            cpu: CpuInfo {
                enabled: RUNS_JOBS && self.config.cpu.enabled,
                ceiling: WireCeiling {
                    threads: self.ceiling.threads,
                    cpu_percent: self.ceiling.cpu_percent,
                    memory_mb: self.ceiling.memory_mb,
                    work: self.ceiling.work.clone(),
                },
                limits: settings::UNTIL_THE_JOB_HOST,
                verifiable: false,
                bots: f.bots.clone(),
                ruleset: f.ruleset.clone(),
            },
            report,
            running: Vec::new(),
            results: Vec::new(),
            updates: self.config.updates,
            quiet_until: self.quiet_until(),
        }
    }

    /// Waits `d`, or until the helper is stopped (then `false`).
    async fn pause(&self, d: Duration) -> bool {
        if self.stopped() {
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep(d) => true,
            () = stop_signal(self.stop.clone()) => false,
        }
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    /// Runs the helper until it is stopped or permanently refused (from a console).
    pub async fn run(&self) -> Exit {
        let mut backoff = Backoff::default();
        let mut idle_logged = false;
        let mut first = true;
        let mut catalog = Catalog::new(self.config.models.clone());
        loop {
            if self.stopped() {
                return Exit::Stopped;
            }
            let report = tokio::select! {
                r = self.prepare(&mut catalog) => r,
                () = stop_signal(self.stop.clone()) => return Exit::Stopped,
            };
            let line = format!("connecting to {}", self.config.server_url);
            if first {
                self.log.info(line);
            } else {
                self.log.debug(line);
            }
            first = false;
            let dialled = tokio::select! {
                r = self.dialer.dial() => r,
                () = stop_signal(self.stop.clone()) => return Exit::Stopped,
            };
            let end = match dialled {
                Ok(ws) => self.connection(ws, &mut catalog, report).await,
                Err(e) => {
                    let d = backoff.delay();
                    self.log.warn(format!(
                        "could not connect: {e}; trying again in {:.0} s",
                        d.as_secs_f64()
                    ));
                    if !self.pause(d).await {
                        return Exit::Stopped;
                    }
                    continue;
                }
            };
            let wait = match end {
                End::Stopped => return Exit::Stopped,
                End::Reconnect { welcomed } => {
                    if welcomed {
                        backoff.reset();
                        // Welcomed again: a later refusal is news, and is logged as such.
                        idle_logged = false;
                    }
                    let d = backoff.delay();
                    self.log.info(format!("reconnecting in {:.1} s", d.as_secs_f64()));
                    d
                }
                End::Permanent(why) => {
                    if !self.service {
                        self.log.error(why);
                        return Exit::Refused;
                    }
                    if idle_logged {
                        self.log.debug(why);
                    } else {
                        self.log.error(format!(
                            "{why} Running as a service: idling, and trying again every hour."
                        ));
                        idle_logged = true;
                    }
                    backoff.reset();
                    PERMANENT_RETRY
                }
                End::Upgrade(why) => {
                    self.log.throttled("upgrade", Duration::from_secs(3000), Level::Warn, why);
                    backoff.reset();
                    UPGRADE_RETRY
                }
                End::Rate(d) => {
                    self.log.warn(format!(
                        "the server is refusing too many tokens from this address; trying again in {:.0} s",
                        d.as_secs_f64()
                    ));
                    backoff.reset();
                    d
                }
            };
            if !self.pause(wait).await {
                return Exit::Stopped;
            }
        }
    }

    /// One connection, from the hello to its end.
    async fn connection(&self, ws: Ws, catalog: &mut Catalog, report: Report) -> End {
        let (mut tx, mut rx) = ws.split();
        let n = catalog.models.len();
        let hello = self.hello(catalog.models.clone(), report);
        let quiet_until = hello.quiet_until;
        if self.send(&mut tx, HelperFrame::Hello(hello)).await.is_err() {
            return End::Reconnect { welcomed: false };
        }
        let welcome = match self.answer(&mut tx, &mut rx).await {
            Ok(w) => w,
            Err(end) => return end,
        };
        self.log.info(format!(
            "connected as \u{201c}{}\u{201d} ({}): {n} model{} listed, {} slot{}",
            clip(&welcome.machine.name, 120),
            welcome.machine.id,
            if n == 1 { "" } else { "s" },
            self.config.max_concurrent,
            if self.config.max_concurrent == 1 { "" } else { "s" }
        ));
        for (what, runs) in [
            ("keep", &welcome.jobs.keep),
            ("drop", &welcome.jobs.drop),
            ("resend", &welcome.jobs.resend),
        ] {
            if !runs.is_empty() {
                // This helper plays no job before 4-10, so it holds none to keep or send.
                self.log.warn(format!(
                    "the welcome names {} run{} to {what} that this helper does not hold: {}",
                    runs.len(),
                    if runs.len() == 1 { "" } else { "s" },
                    clip(&runs.join(" "), SERVER_TEXT)
                ));
            }
        }
        if self.ack(&mut tx, &welcome.settings).await.is_err() {
            return End::Reconnect { welcomed: true };
        }
        let mut steady = Steady::new(self, catalog, quiet_until);
        let end = steady.serve(&mut tx, &mut rx).await;
        // However the connection ended, nothing it started outlives it: every completion still in
        // flight is dropped with its connection to the endpoint (P4.2.4), and its slot is free
        // before the next connection's first request.
        let dropped = steady.flights.stop(CLOSE_SEND).await;
        if !dropped.is_empty() {
            self.log.info(format!(
                "the connection ended: {} completion{} in flight stopped",
                dropped.len(),
                if dropped.len() == 1 { "" } else { "s" }
            ));
        }
        end
    }

    async fn ack(&self, tx: &mut Sink, site: &Settings) -> Result<(), ()> {
        let ack = settings::ack(site, &self.local());
        if !ack.capped.is_empty() {
            let names: Vec<&str> = ack.capped.iter().map(|c| c.as_str()).collect();
            self.log.info(format!("the machine's own limits apply to {}", names.join(", ")));
        }
        self.send(tx, HelperFrame::SettingsAck(ack)).await
    }

    async fn send(&self, tx: &mut Sink, frame: HelperFrame) -> Result<(), ()> {
        let text = frame.encode(self.clock.now_unix());
        self.send_text(tx, text, frame.type_name()).await
    }

    async fn send_text(&self, tx: &mut Sink, text: String, what: &str) -> Result<(), ()> {
        match tokio::time::timeout(SEND_TIMEOUT, tx.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.log.info(format!("the connection failed while sending {what}: {e}"));
                Err(())
            }
            Err(_) => {
                self.log.warn(format!("sending {what} took over 30 s: the connection is dead"));
                Err(())
            }
        }
    }

    /// Why this helper will not run `r` now, as the error it answers: the owner's policy first
    /// (an endpoint at all, the allowlist), then the local quiet hours, then the local slots.
    fn refuse(&self, r: &Request, in_flight: usize) -> Option<RequestError> {
        let refusal = |code, message: String, retryable| RequestError {
            id: r.id.clone(),
            code,
            message: cut(&message, proxy::MESSAGE_CHARS),
            retryable,
        };
        let host = clip(&self.facts.hostname, 255);
        if self.endpoint.is_none() {
            return Some(refusal(
                RequestErrorCode::UnknownModel,
                format!("{host} serves no models (--no-llm)"),
                false,
            ));
        }
        if let Some(allow) = &self.config.models
            && !allow.contains(&r.model)
        {
            return Some(refusal(
                RequestErrorCode::UnknownModel,
                format!(
                    "{host} does not serve \u{201c}{}\u{201d}: it is not among the models its owner lends",
                    clip(&r.model, llm::MAX_KEY)
                ),
                false,
            ));
        }
        if self.quiet_until().is_some() {
            return Some(refusal(
                RequestErrorCode::Closed,
                format!("{host} is in its local quiet hours"),
                true,
            ));
        }
        let slots = usize::try_from(self.config.max_concurrent).unwrap_or(usize::MAX);
        if in_flight >= slots {
            return Some(refusal(
                RequestErrorCode::Busy,
                format!(
                    "{host} is at its local limit of {} concurrent request{}",
                    self.config.max_concurrent,
                    if self.config.max_concurrent == 1 { "" } else { "s" }
                ),
                true,
            ));
        }
        None
    }

    /// The server's answer to the hello: its `welcome`, or how the connection ends.
    async fn answer(&self, tx: &mut Sink, rx: &mut Stream) -> Result<protocol::Welcome, End> {
        let deadline = Instant::now() + HELLO_ANSWER;
        loop {
            let msg = tokio::select! {
                m = tokio::time::timeout_at(deadline, rx.next()) => m,
                () = stop_signal(self.stop.clone()) => {
                    let _closed = tokio::time::timeout(CLOSE_SEND, tx.send(Message::Close(None))).await;
                    return Err(End::Stopped);
                }
            };
            let msg = match msg {
                Err(_) => {
                    self.log.warn("the server did not answer the hello within 30 s");
                    return Err(End::Reconnect { welcomed: false });
                }
                Ok(None | Some(Err(_))) => {
                    self.log.info("the connection closed before the server answered the hello");
                    return Err(End::Reconnect { welcomed: false });
                }
                Ok(Some(Ok(m))) => m,
            };
            match self.read(&msg) {
                Read::Close(code, reason) => return Err(self.closed(code, &reason, false)),
                Read::Frame(f) => {
                    let f = *f;
                    if let ServerFrame::Welcome(w) = f {
                        return Ok(w);
                    }
                    if let Some(end) = self.refusal(&f) {
                        return Err(self.await_close(tx, rx, end).await);
                    }
                    self.log.debug(format!("ignoring {} before the welcome", f.type_name()));
                }
                Read::Bad(_) | Read::Nothing => {}
            }
        }
    }

    /// What a refusing frame means: `refused`, protocol 1's `error`, or `shutdown`.
    fn refusal(&self, f: &ServerFrame) -> Option<End> {
        match f {
            ServerFrame::Refused(r) => Some(match r.code {
                RefusedCode::Token => End::Permanent(format!(
                    "The server refused this helper's token: {} Issue a new token on the \
                     server's Servers page and run `citar-helper setup` to give it to this helper.",
                    clip(&r.message, SERVER_TEXT)
                )),
                RefusedCode::Key => End::Permanent(format!(
                    "The server refused this machine's key (fingerprint shown by `citar-helper \
                     --version`): {} The machine's owner must reset its key on the Servers page.",
                    clip(&r.message, SERVER_TEXT)
                )),
                RefusedCode::Protocol | RefusedCode::Build => End::Upgrade(format!(
                    "The server does not speak this helper's protocol ({}): {} Either the server \
                     must be upgraded or this helper's update applied; trying again every 10 minutes.",
                    r.code.as_str(),
                    clip(&r.message, SERVER_TEXT)
                )),
                RefusedCode::Rate => {
                    let s = r.retry_after_s.unwrap_or(RATE_RETRY.as_secs_f64()).clamp(1.0, 3600.0);
                    End::Rate(Duration::from_secs_f64(s))
                }
            }),
            ServerFrame::V1Refusal(e) => Some(End::Upgrade(format!(
                "The server answered in protocol 1: {} It must be upgraded to CITAR 0.1.6 or \
                 later; trying again every 10 minutes.",
                clip(&e.message, SERVER_TEXT)
            ))),
            ServerFrame::Shutdown(s) => Some(End::Permanent(format!(
                "The server took this machine away ({}): {}",
                s.code.as_str(),
                clip(&s.reason, SERVER_TEXT)
            ))),
            _ => None,
        }
    }

    /// After a refusal or a shutdown: the server closes the socket; wait for it a little.
    async fn await_close(&self, tx: &mut Sink, rx: &mut Stream, end: End) -> End {
        let deadline = Instant::now() + CLOSE_WAIT;
        loop {
            match tokio::time::timeout_at(deadline, rx.next()).await {
                Ok(Some(Ok(Message::Close(_)))) | Ok(None | Some(Err(_))) => return end,
                Ok(Some(Ok(_))) => {}
                Err(_) => {
                    let _closed =
                        tokio::time::timeout(CLOSE_SEND, tx.send(Message::Close(None))).await;
                    return end;
                }
            }
        }
    }

    /// How a close with no refusing frame before it ends the connection (PROTOCOL.md "Close
    /// codes").
    fn closed(&self, code: Option<u16>, reason: &str, welcomed: bool) -> End {
        use protocol::close;
        // A close's reason is at most 123 bytes; its control characters are escaped all the same.
        let said =
            if reason.is_empty() { String::new() } else { format!(": {}", clip(reason, 123)) };
        match code {
            Some(close::BAD_HELLO) => {
                self.log.warn(format!("the server refused the hello as invalid (4400{said})"));
                End::Reconnect { welcomed }
            }
            Some(close::REFUSED) => End::Permanent(format!(
                "The server refused this helper's token or key (close 4401{said})."
            )),
            Some(close::SHUTDOWN) => {
                End::Permanent(format!("The server took this machine away (close 4403{said})."))
            }
            Some(close::REPLACED) => End::Permanent(format!(
                "Another helper connected with this machine's token (close 4409{said})."
            )),
            Some(close::PROTOCOL) => End::Upgrade(format!(
                "The server does not speak this helper's protocol (close 4426{said}); trying \
                 again every 10 minutes."
            )),
            Some(close::RATE) => End::Rate(RATE_RETRY),
            Some(c) => {
                self.log.info(format!("the server closed the connection ({c}{said})"));
                End::Reconnect { welcomed }
            }
            None => {
                self.log.info("the connection was lost");
                End::Reconnect { welcomed }
            }
        }
    }

    /// One message off the socket.
    fn read(&self, msg: &Message) -> Read {
        match msg {
            Message::Text(t) => match protocol::parse_server_frame(t.as_str()) {
                Ok(Parsed::Frame { frame, .. }) => Read::Frame(Box::new(frame)),
                Ok(Parsed::Ignored { t }) => {
                    // A type the server made up: quoted cut, and the log keeps a bounded number
                    // of such keys (and none at all below debug level).
                    let t = clip(&t, TYPE_SHOWN);
                    self.log.once(
                        &format!("ignored {t}"),
                        Level::Debug,
                        format!("ignoring frames of type \u{201c}{t}\u{201d}"),
                    );
                    Read::Nothing
                }
                Err(e) => {
                    // Only a frame that passed the envelope (at most 8 MiB, a JSON object of a
                    // type this side knows) and failed a field's check is read again, for the id
                    // or run a bad request or offer is answered by. One too large, too deep or
                    // not an object is not, and its type stays unknown.
                    let raw: Option<Map<String, Value>> = match e.code {
                        ErrorCode::TooLarge
                        | ErrorCode::TooDeep
                        | ErrorCode::NotJson
                        | ErrorCode::NotObject
                        | ErrorCode::NoType => None,
                        _ => serde_json::from_str::<Value>(t.as_str()).ok().and_then(|v| match v {
                            Value::Object(m) => Some(m),
                            _ => None,
                        }),
                    };
                    // The throttling key is one of the known types' own names, or `?`.
                    let known: Option<&'static str> = raw
                        .as_ref()
                        .and_then(|m| m.get("t"))
                        .and_then(Value::as_str)
                        .and_then(|t| ServerFrame::TYPES.iter().copied().find(|k| *k == t));
                    let what = match known {
                        Some(k) => format!("a bad {k} frame from the server"),
                        None => "a bad frame from the server".to_owned(),
                    };
                    self.log.throttled(
                        &format!("bad {}", known.unwrap_or("?")),
                        Duration::from_secs(60),
                        Level::Warn,
                        format!("{what}: {}", describe(&e)),
                    );
                    Read::Bad(raw)
                }
            },
            Message::Binary(_) => {
                self.log.throttled(
                    "binary",
                    Duration::from_secs(60),
                    Level::Warn,
                    "a binary frame from the server, ignored",
                );
                Read::Nothing
            }
            Message::Close(c) => Read::Close(
                c.as_ref().map(|c| u16::from(c.code)),
                c.as_ref().map(|c| c.reason.as_str().to_owned()).unwrap_or_default(),
            ),
            _ => Read::Nothing,
        }
    }
}

type Sink = SplitSink<Ws, Message>;
type Stream = SplitStream<Ws>;

/// A message read.
enum Read {
    Frame(Box<ServerFrame>),
    /// A frame that failed its checks, with its object when it was one.
    Bad(Option<Map<String, Value>>),
    Close(Option<u16>, String),
    /// A WebSocket control frame, or a frame type ignored.
    Nothing,
}

fn read_hardware_file(path: &std::path::Path) -> Result<Map<String, Value>, String> {
    let raw = std::fs::read(path)
        .map_err(|e| format!("could not read the hardware file {}: {e}", path.display()))?;
    // Windows PowerShell 5's `>` writes UTF-16 with a byte-order mark; everything else UTF-8.
    let text = if raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(&[0xfe, 0xff]) {
        let le = raw[0] == 0xff;
        let units: Vec<u16> = raw[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) })
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let s = String::from_utf8_lossy(&raw).into_owned();
        s.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(s)
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m))
            if m.get("format").and_then(Value::as_str) == Some("citar-hardware") =>
        {
            Ok(m)
        }
        Ok(_) => Err(format!(
            "{} is not a CITAR hardware file (run a collector: collect_hardware.py, .sh or .ps1)",
            path.display()
        )),
        Err(e) => Err(format!("could not read the hardware file {}: {e}", path.display())),
    }
}

/// The connection after its welcome.
struct Steady<'a> {
    h: &'a Helper,
    catalog: &'a mut Catalog,
    last_rx: Instant,
    pinged: bool,
    /// The last `status` sent (the hello's figures before the first).
    status: Status,
    status_sent: Option<Instant>,
    /// When the next `status` may go, once the figures changed.
    status_due: Option<Instant>,
    next_quiet: Instant,
    /// The completions in flight on this connection.
    flights: Flights,
    /// Each listing the poller makes, every [`CATALOG_EVERY`].
    polls: Option<mpsc::Receiver<Result<Vec<Listed>, String>>>,
    poller: Option<tokio::task::JoinHandle<()>>,
    /// A `rescan`'s read of the hardware report, off the session's task.
    rescan: Option<tokio::task::JoinHandle<Report>>,
}

impl Drop for Steady<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.poller.take() {
            p.abort();
        }
        if let Some(r) = self.rescan.take() {
            r.abort();
        }
    }
}

impl<'a> Steady<'a> {
    fn new(h: &'a Helper, catalog: &'a mut Catalog, quiet_until: Option<f64>) -> Steady<'a> {
        let now = Instant::now();
        let (polls, poller) = match &h.endpoint {
            None => (None, None),
            Some(ep) => {
                let (tx, rx) = mpsc::channel(1);
                let ep = ep.clone();
                let task = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(CATALOG_EVERY).await;
                        if tx.send(list(&ep).await).await.is_err() {
                            return;
                        }
                    }
                });
                (Some(rx), Some(task))
            }
        };
        Steady {
            h,
            catalog,
            last_rx: now,
            pinged: false,
            // What the hello said: the first status goes when that changes.
            status: Status { quiet_until, llm_in_flight: 0, jobs_running: 0, jobs_paused: 0 },
            status_sent: None,
            status_due: None,
            next_quiet: now + QUIET_TICK,
            flights: Flights::new(h.in_flight.clone()),
            polls,
            poller,
            rescan: None,
        }
    }

    /// The figures a `status` would carry now.
    fn fresh_status(&self) -> Status {
        Status {
            quiet_until: self.h.quiet_until(),
            llm_in_flight: u32::try_from(self.flights.len()).unwrap_or(u32::MAX),
            jobs_running: 0,
            jobs_paused: 0,
        }
    }

    /// Plans a `status` when the figures differ from the last one sent: at once, or 5 s after it.
    fn note_status(&mut self, now: Instant) {
        if self.status_due.is_none() && self.fresh_status() != self.status {
            let earliest = self.status_sent.map_or(now, |t| t + STATUS_EVERY);
            self.status_due = Some(earliest.max(now));
        }
    }

    async fn serve(&mut self, tx: &mut Sink, rx: &mut Stream) -> End {
        loop {
            let ping_at = (!self.pinged).then(|| self.last_rx + PING_AFTER);
            let stale_at = self.last_rx + STALE_AFTER;
            let mut wake = stale_at.min(self.next_quiet);
            if let Some(p) = ping_at {
                wake = wake.min(p);
            }
            if let Some(s) = self.status_due {
                wake = wake.min(s);
            }
            let polls = &mut self.polls;
            let poll = async {
                match polls.as_mut() {
                    Some(c) => c.recv().await,
                    None => std::future::pending().await,
                }
            };
            let rescan = &mut self.rescan;
            let rescanned = async {
                match rescan.as_mut() {
                    Some(r) => r.await,
                    None => std::future::pending().await,
                }
            };
            let finished = self.flights.next();
            tokio::select! {
                m = rx.next() => {
                    let msg = match m {
                        None | Some(Err(_)) => return self.h.closed(None, "", true),
                        Some(Ok(m)) => m,
                    };
                    self.last_rx = Instant::now();
                    self.pinged = false;
                    match self.h.read(&msg) {
                        Read::Close(code, reason) => return self.h.closed(code, &reason, true),
                        Read::Frame(f) => {
                            if let Some(end) = self.h.refusal(&f) {
                                return self.h.await_close(tx, rx, end).await;
                            }
                            if self.frame(tx, *f).await.is_err() {
                                return End::Reconnect { welcomed: true };
                            }
                        }
                        Read::Bad(raw) => {
                            if self.bad(tx, raw.as_ref()).await.is_err() {
                                return End::Reconnect { welcomed: true };
                            }
                        }
                        Read::Nothing => {}
                    }
                }
                Some(listed) = poll => {
                    if self.catalog.take(listed, self.h) {
                        let models = self.catalog.models.clone();
                        let n = models.len();
                        let loaded = models.iter().filter(|m| m.loaded).count();
                        if self.h.send(tx, HelperFrame::Catalog(protocol::Catalog { models })).await.is_err() {
                            return End::Reconnect { welcomed: true };
                        }
                        self.h.log.info(format!("the endpoint's models changed: {n} listed, {loaded} loaded"));
                    }
                }
                (id, how) = finished => {
                    if self.finished(tx, id, how).await.is_err() {
                        return End::Reconnect { welcomed: true };
                    }
                    self.note_status(Instant::now());
                }
                report = rescanned => {
                    self.rescan = None;
                    if let Ok(Report { hardware: Some(scan), hardware_source: HelloHardwareSource::File }) = report {
                        let frame = protocol::Hardware { scan, source: protocol::HardwareSource::File };
                        if self.h.send(tx, HelperFrame::Hardware(frame)).await.is_err() {
                            return End::Reconnect { welcomed: true };
                        }
                    }
                }
                () = stop_signal(self.h.stop.clone()) => {
                    // Each completion in flight is stopped, and the server told so, before the
                    // close: it can give them to another machine at once.
                    let ids = self.flights.stop(CLOSE_SEND).await;
                    let h = self.h;
                    let answered = async {
                        for id in ids {
                            let e = RequestError {
                                id,
                                code: RequestErrorCode::Stopping,
                                message: "the helper is stopping".to_owned(),
                                retryable: true,
                            };
                            if h.send(tx, HelperFrame::Error(e)).await.is_err() {
                                return;
                            }
                        }
                    };
                    let _answered = tokio::time::timeout(CLOSE_SEND, answered).await;
                    let close = CloseFrame { code: CloseCode::Normal, reason: "the helper is stopping".into() };
                    let _closed = tokio::time::timeout(CLOSE_SEND, tx.send(Message::Close(Some(close)))).await;
                    return End::Stopped;
                }
                () = tokio::time::sleep_until(wake) => {
                    let now = Instant::now();
                    if now >= stale_at {
                        self.h.log.warn("no frame from the server for 70 s: the connection is dead");
                        return End::Reconnect { welcomed: true };
                    }
                    if ping_at.is_some_and(|p| now >= p) {
                        self.pinged = true;
                        if self.h.send(tx, HelperFrame::Ping(protocol::Ping {})).await.is_err() {
                            return End::Reconnect { welcomed: true };
                        }
                    }
                    if now >= self.next_quiet {
                        self.next_quiet = now + QUIET_TICK;
                        self.note_status(now);
                    }
                    if self.status_due.is_some_and(|d| now >= d) {
                        self.status_due = None;
                        let fresh = self.fresh_status();
                        if fresh != self.status {
                            if fresh.quiet_until != self.status.quiet_until {
                                if fresh.quiet_until.is_some() {
                                    self.h.log.info("local quiet hours: no new work until they end");
                                } else {
                                    self.h.log.info("local quiet hours are over");
                                }
                            }
                            self.status = fresh.clone();
                            self.status_sent = Some(now);
                            if self.h.send(tx, HelperFrame::Status(fresh)).await.is_err() {
                                return End::Reconnect { welcomed: true };
                            }
                        }
                    }
                }
            }
        }
    }

    /// A frame from the server, after the welcome.
    async fn frame(&mut self, tx: &mut Sink, f: ServerFrame) -> Result<(), ()> {
        match f {
            ServerFrame::Ping(_) => self.h.send(tx, HelperFrame::Pong(protocol::Pong {})).await,
            ServerFrame::Settings(s) => self.h.ack(tx, &s).await,
            ServerFrame::Rescan(_) => {
                // Read off this task (it may take up to its timeout), and sent from the loop when
                // it is done; a rescan while one is under way is that one's.
                if self.rescan.is_none() {
                    self.rescan = Some(tokio::spawn(self.h.report()));
                }
                Ok(())
            }
            ServerFrame::Request(r) => self.request(tx, r).await,
            ServerFrame::Cancel(c) => {
                if !self.flights.cancel(&c.id) {
                    // Finished before the cancel came (its `done` or `error` crossed it), or
                    // never here: nothing more is sent.
                    self.h.log.debug(format!(
                        "a cancel for {}, which is not in flight",
                        clip(&c.id, TYPE_SHOWN)
                    ));
                }
                Ok(())
            }
            ServerFrame::JobOffer(o) => {
                // Jobs are 4-10's: this helper has no jobs build, so no offer is its build.
                let r = JobReject { run: o.run, reason: RejectReason::Build };
                self.h.send(tx, HelperFrame::JobReject(r)).await
            }
            other => {
                // job_result_ack, job_abort, jobs_pause, jobs_resume (no runs), update (4-13),
                // pong, and a second welcome.
                self.h.log.debug(format!("nothing to do for {}", other.type_name()));
                Ok(())
            }
        }
    }

    /// A `request`: refused as local policy says, or started.
    async fn request(&mut self, tx: &mut Sink, r: Request) -> Result<(), ()> {
        if self.flights.contains(&r.id) {
            // One answer per id: the first one's.
            self.h.log.throttled(
                "request again",
                Duration::from_secs(60),
                Level::Warn,
                format!(
                    "the server sent request {} again while it is in flight: ignored",
                    clip(&r.id, TYPE_SHOWN)
                ),
            );
            return Ok(());
        }
        if let Some(e) = self.h.refuse(&r, self.flights.len()) {
            self.h.log.info(format!(
                "refused request {} ({}): {}",
                clip(&r.id, TYPE_SHOWN),
                e.code.as_str(),
                e.message
            ));
            return self.h.send(tx, HelperFrame::Error(e)).await;
        }
        for name in r.params.extra.keys() {
            let name = clip(name, TYPE_SHOWN);
            self.h.log.once(
                &format!("parameter {name}"),
                Level::Info,
                format!(
                    "ignoring the server's parameter \u{201c}{name}\u{201d}: only {} reach the \
                     endpoint",
                    proxy::SERVER_PARAMS.join(", ")
                ),
            );
        }
        let Some(ep) = self.h.endpoint.clone() else { return Ok(()) };
        self.h.log.debug(format!(
            "request {} for {}",
            clip(&r.id, TYPE_SHOWN),
            clip(&r.model, llm::MAX_KEY)
        ));
        self.flights.start(ep, r, self.h.config.stream, self.h.log.clone());
        self.note_status(Instant::now());
        Ok(())
    }

    /// A completion finished: its `done`, `error` or `cancelled` to the server.
    async fn finished(&mut self, tx: &mut Sink, id: String, how: Finished) -> Result<(), ()> {
        let shown = clip(&id, TYPE_SHOWN);
        let error = |code, message: String, retryable| {
            HelperFrame::Error(RequestError { id: id.clone(), code, message, retryable })
        };
        let frame = match how {
            Finished::Outcome(Outcome::Done(done)) => {
                let calls = done.message.tool_calls.len();
                let gen_s = done.gen_s;
                let text = HelperFrame::Done(done).encode(self.h.clock.now_unix());
                if text.len() <= MAX_FRAME_BYTES {
                    self.h.log.info(format!(
                        "request {shown} done in {gen_s:.1} s ({calls} tool call{})",
                        if calls == 1 { "" } else { "s" }
                    ));
                    return self.h.send_text(tx, text, "done").await;
                }
                let message = format!(
                    "the answer is {} bytes as a frame, over the protocol's {MAX_FRAME_BYTES}",
                    text.len()
                );
                self.h.log.warn(format!("request {shown} failed: {message}"));
                error(RequestErrorCode::Endpoint, message, false)
            }
            Finished::Outcome(Outcome::Failed { code, message, retryable }) => {
                self.h.log.info(format!(
                    "request {shown} failed ({}): {}",
                    code.as_str(),
                    clip(&message, 2 * SERVER_TEXT)
                ));
                error(code, message, retryable)
            }
            Finished::Cancelled { gen_s, tokens_out } => {
                self.h.log.info(format!(
                    "request {shown} cancelled after {gen_s:.1} s ({tokens_out} token{} received)",
                    if tokens_out == 1 { "" } else { "s" }
                ));
                HelperFrame::Cancelled(protocol::Cancelled { id: id.clone(), gen_s, tokens_out })
            }
            Finished::Broken(why) => {
                self.h.log.error(format!("request {shown} failed in the helper: {why}"));
                error(
                    RequestErrorCode::Endpoint,
                    "the helper failed while running it".to_owned(),
                    false,
                )
            }
        };
        self.h.send(tx, frame).await
    }

    /// A frame that failed its checks: a `request` whose id can be read gets `bad_request`, a
    /// `job_offer` whose run can be read `bad_spec`; anything else was only logged.
    async fn bad(&mut self, tx: &mut Sink, raw: Option<&Map<String, Value>>) -> Result<(), ()> {
        let Some(raw) = raw else { return Ok(()) };
        let readable = |key: &str, ty: &protocol::Ty| {
            raw.get(key)
                .filter(|v| protocol::check::check(ty, v, &protocol::Path::Root).is_ok())
                .and_then(Value::as_str)
        };
        match raw.get("t").and_then(Value::as_str) {
            Some("request") => {
                if let Some(id) = readable("id", &protocol::Ty::Id) {
                    let e = RequestError {
                        id: id.to_owned(),
                        code: RequestErrorCode::BadRequest,
                        message: "the request frame failed its checks".to_owned(),
                        retryable: false,
                    };
                    return self.h.send(tx, HelperFrame::Error(e)).await;
                }
            }
            Some("job_offer") => {
                if let Some(run) = readable("run", &protocol::Ty::RunId) {
                    let r = JobReject { run: run.to_owned(), reason: RejectReason::BadSpec };
                    return self.h.send(tx, HelperFrame::JobReject(r)).await;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_minute_with_at_most_thirty_percent_jitter() {
        let mut b = Backoff::default();
        let mut want = 2.0f64;
        for _ in 0..8 {
            let d = b.delay().as_secs_f64();
            assert!(d >= want && d <= want * 1.3 + 1e-9, "{d} for {want}");
            want = (want * 2.0).min(60.0);
        }
        b.reset();
        assert!(b.delay() < Duration::from_secs(3));
    }

    #[test]
    fn exit_codes_are_the_contracts() {
        assert_eq!(Exit::Stopped.code(), 0);
        assert_eq!(Exit::Refused.code(), 78);
    }

    #[test]
    fn a_utf16_hardware_file_reads_and_a_foreign_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("citar-helper-hw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let doc = "{\"format\": \"citar-hardware\", \"cpu\": {\"model\": \"x\"}}";
        let mut utf16 = vec![0xff, 0xfe];
        for u in doc.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let f = dir.join("hw16.json");
        std::fs::write(&f, &utf16).expect("written");
        assert_eq!(read_hardware_file(&f).expect("reads")["cpu"]["model"], "x");
        let g = dir.join("other.json");
        std::fs::write(&g, "{\"format\": \"other\"}").expect("written");
        assert!(read_hardware_file(&g).expect_err("refused").contains("not a CITAR hardware file"));
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
