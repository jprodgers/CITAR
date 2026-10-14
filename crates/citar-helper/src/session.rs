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
//!   when the endpoint's models or loaded set change (polled every 30 s); `status` when the
//!   local quiet hours begin or end (at most every 5 s). Local policy holds whatever the server
//!   asks: until 4-04 the helper serves no completion (`error` `unknown_model`), and until 4-10
//!   it takes no job (`job_reject` `build`: it has no jobs build).
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
use crate::llm::{self, SharedEndpoint};
use crate::log::{Level, Log};
use crate::net::{Dialer, Ws};
use crate::protocol::{
    self, Ceiling as WireCeiling, CpuInfo, FrameSet as _, HelloHardwareSource, HelperFrame,
    HelperInfo, JobReject, LlmInfo, MachineInfo, Model, Parsed, RefusedCode, RejectReason, Report,
    RequestError, RequestErrorCode, ServerFrame, Settings, Status,
};
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

/// Until 4-04 the helper serves no completion: its hello says `llm.enabled` false, and its
/// acknowledgement never turns model work on.
pub const SERVES_COMPLETIONS: bool = false;
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

impl Helper {
    /// The model side as this helper allows it.
    fn local(&self) -> Local {
        let slots = if self.config.no_llm { 0 } else { self.config.max_concurrent };
        Local {
            serves_llm: SERVES_COMPLETIONS && !self.config.no_llm,
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

    /// The models to offer now: the endpoint's (empty when it cannot be listed).
    async fn models(&self) -> Vec<Model> {
        let Some(ep) = &self.endpoint else { return Vec::new() };
        let listed = match tokio::time::timeout(llm::LIST_TIMEOUT, ep.list()).await {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                self.log.throttled(
                    "models",
                    Duration::from_secs(600),
                    Level::Warn,
                    format!("could not list the models at {}: {e}", self.config.base_url),
                );
                Vec::new()
            }
            Err(_) => {
                self.log.throttled(
                    "models",
                    Duration::from_secs(600),
                    Level::Warn,
                    format!("{} did not list its models within 10 s", self.config.base_url),
                );
                Vec::new()
            }
        };
        llm::choose(&listed, self.config.models.as_deref())
    }

    /// The hardware report: the owner's file (read at each connection, UTF-16 accepted), or
    /// nothing. The scan itself is 4-06's.
    async fn report(&self) -> Report {
        let none = Report { hardware: None, hardware_source: HelloHardwareSource::Nothing };
        if !self.config.collect_hardware || self.config.hardware_file.is_empty() {
            return none;
        }
        let path = std::path::PathBuf::from(&self.config.hardware_file);
        let read = tokio::task::spawn_blocking(move || read_hardware_file(&path)).await;
        match read {
            Ok(Ok(scan)) => {
                Report { hardware: Some(scan), hardware_source: HelloHardwareSource::File }
            }
            Ok(Err(e)) => {
                self.log.throttled("hardware", Duration::from_secs(3600), Level::Warn, e);
                none
            }
            Err(e) => {
                self.log.warn(format!("reading the hardware file failed: {e}"));
                none
            }
        }
    }

    fn hello(&self, models: Vec<Model>, report: Report) -> protocol::Hello {
        let f = &self.facts;
        let llm_on = SERVES_COMPLETIONS && !self.config.no_llm;
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
                // The endpoint's kind whenever the helper has one: `null` with `--no-llm` only,
                // so the server knows what the machine fronts before its proxy is on (4-04).
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
        loop {
            if self.stopped() {
                return Exit::Stopped;
            }
            let models = self.models().await;
            let report = self.report().await;
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
                Ok(ws) => self.connection(ws, models, report).await,
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
    async fn connection(&self, ws: Ws, models: Vec<Model>, report: Report) -> End {
        let (mut tx, mut rx) = ws.split();
        let hello = self.hello(models.clone(), report);
        let quiet_until = hello.quiet_until;
        if self.send(&mut tx, HelperFrame::Hello(hello)).await.is_err() {
            return End::Reconnect { welcomed: false };
        }
        let welcome = match self.answer(&mut tx, &mut rx).await {
            Ok(w) => w,
            Err(end) => return end,
        };
        self.log.info(format!(
            "connected as \u{201c}{}\u{201d} ({}): {} model{} listed, {} slot{}",
            welcome.machine.name,
            welcome.machine.id,
            models.len(),
            if models.len() == 1 { "" } else { "s" },
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
                    "the welcome names runs to {what} that this helper does not hold: {runs:?}"
                ));
            }
        }
        if self.ack(&mut tx, &welcome.settings).await.is_err() {
            return End::Reconnect { welcomed: true };
        }
        Steady::new(self, models, quiet_until).run(&mut tx, &mut rx).await
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
        match tokio::time::timeout(SEND_TIMEOUT, tx.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.log.info(format!(
                    "the connection failed while sending {}: {e}",
                    frame.type_name()
                ));
                Err(())
            }
            Err(_) => {
                self.log.warn(format!(
                    "sending {} took over 30 s: the connection is dead",
                    frame.type_name()
                ));
                Err(())
            }
        }
    }

    /// The server's answer to the hello: its `welcome`, or how the connection ends.
    async fn answer(&self, tx: &mut Sink, rx: &mut Stream) -> Result<protocol::Welcome, End> {
        let deadline = Instant::now() + HELLO_ANSWER;
        loop {
            let msg = tokio::select! {
                m = tokio::time::timeout_at(deadline, rx.next()) => m,
                () = stop_signal(self.stop.clone()) => {
                    let _closed = tx.send(Message::Close(None)).await;
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
                    r.message
                )),
                RefusedCode::Key => End::Permanent(format!(
                    "The server refused this machine's key (fingerprint shown by `citar-helper \
                     --version`): {} The machine's owner must reset its key on the Servers page.",
                    r.message
                )),
                RefusedCode::Protocol | RefusedCode::Build => End::Upgrade(format!(
                    "The server does not speak this helper's protocol ({}): {} Either the server \
                     must be upgraded or this helper's update applied; trying again every 10 minutes.",
                    r.code.as_str(),
                    r.message
                )),
                RefusedCode::Rate => {
                    let s = r.retry_after_s.unwrap_or(RATE_RETRY.as_secs_f64()).clamp(1.0, 3600.0);
                    End::Rate(Duration::from_secs_f64(s))
                }
            }),
            ServerFrame::V1Refusal(e) => Some(End::Upgrade(format!(
                "The server answered in protocol 1: {} It must be upgraded to CITAR 0.1.6 or \
                 later; trying again every 10 minutes.",
                e.message
            ))),
            ServerFrame::Shutdown(s) => Some(End::Permanent(format!(
                "The server took this machine away ({}): {}",
                s.code.as_str(),
                s.reason
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
                    let _closed = tx.send(Message::Close(None)).await;
                    return end;
                }
            }
        }
    }

    /// How a close with no refusing frame before it ends the connection (PROTOCOL.md "Close
    /// codes").
    fn closed(&self, code: Option<u16>, reason: &str, welcomed: bool) -> End {
        use protocol::close;
        let said = if reason.is_empty() { String::new() } else { format!(": {reason}") };
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
                    self.log.once(
                        &format!("ignored {t}"),
                        Level::Debug,
                        format!("ignoring frames of type {t:?}"),
                    );
                    Read::Nothing
                }
                Err(e) => {
                    let raw: Option<Map<String, Value>> =
                        serde_json::from_str::<Value>(t.as_str()).ok().and_then(|v| match v {
                            Value::Object(m) => Some(m),
                            _ => None,
                        });
                    let t = raw
                        .as_ref()
                        .and_then(|m| m.get("t"))
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned();
                    self.log.throttled(
                        &format!("bad {t}"),
                        Duration::from_secs(60),
                        Level::Warn,
                        format!("a bad {t} frame from the server: {e}"),
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
struct Steady<'h> {
    h: &'h Helper,
    models: Vec<Model>,
    last_rx: Instant,
    pinged: bool,
    status: Status,
    status_sent: Option<Instant>,
    next_quiet: Instant,
    catalog: Option<mpsc::Receiver<Vec<Model>>>,
    poller: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Steady<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.poller.take() {
            p.abort();
        }
    }
}

impl<'h> Steady<'h> {
    fn new(h: &'h Helper, models: Vec<Model>, quiet_until: Option<f64>) -> Steady<'h> {
        let now = Instant::now();
        let (catalog, poller) = match &h.endpoint {
            None => (None, None),
            Some(ep) => {
                let (tx, rx) = mpsc::channel(1);
                let ep = ep.clone();
                let allow = h.config.models.clone();
                let task = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(CATALOG_EVERY).await;
                        let listed = match tokio::time::timeout(llm::LIST_TIMEOUT, ep.list()).await
                        {
                            Ok(Ok(l)) => l,
                            _ => Vec::new(),
                        };
                        if tx.send(llm::choose(&listed, allow.as_deref())).await.is_err() {
                            return;
                        }
                    }
                });
                (Some(rx), Some(task))
            }
        };
        Steady {
            h,
            models,
            last_rx: now,
            pinged: false,
            // What the hello said: the first status goes when that changes.
            status: Status { quiet_until, llm_in_flight: 0, jobs_running: 0, jobs_paused: 0 },
            status_sent: None,
            next_quiet: now + QUIET_TICK,
            catalog,
            poller,
        }
    }

    async fn run(mut self, tx: &mut Sink, rx: &mut Stream) -> End {
        let mut status_due: Option<Instant> = None;
        loop {
            let ping_at = (!self.pinged).then(|| self.last_rx + PING_AFTER);
            let stale_at = self.last_rx + STALE_AFTER;
            let mut wake = stale_at.min(self.next_quiet);
            if let Some(p) = ping_at {
                wake = wake.min(p);
            }
            if let Some(s) = status_due {
                wake = wake.min(s);
            }
            let catalog = async {
                match self.catalog.as_mut() {
                    Some(c) => c.recv().await,
                    None => std::future::pending().await,
                }
            };
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
                Some(models) = catalog => {
                    if models != self.models {
                        let n = models.len();
                        let loaded = models.iter().filter(|m| m.loaded).count();
                        if self.h.send(tx, HelperFrame::Catalog(protocol::Catalog { models: models.clone() })).await.is_err() {
                            return End::Reconnect { welcomed: true };
                        }
                        self.h.log.info(format!("the endpoint's models changed: {n} listed, {loaded} loaded"));
                        self.models = models;
                    }
                }
                () = stop_signal(self.h.stop.clone()) => {
                    let close = CloseFrame { code: CloseCode::Normal, reason: "the helper is stopping".into() };
                    let _closed = tokio::time::timeout(Duration::from_secs(2), tx.send(Message::Close(Some(close)))).await;
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
                        let fresh = Status { quiet_until: self.h.quiet_until(), ..self.status.clone() };
                        if fresh != self.status && status_due.is_none() {
                            let earliest = self.status_sent.map_or(now, |t| t + STATUS_EVERY);
                            status_due = Some(earliest.max(now));
                        }
                    }
                    if status_due.is_some_and(|d| now >= d) {
                        status_due = None;
                        let fresh = Status { quiet_until: self.h.quiet_until(), ..self.status.clone() };
                        if fresh != self.status {
                            if fresh.quiet_until.is_some() {
                                self.h.log.info("local quiet hours: no new work until they end");
                            } else {
                                self.h.log.info("local quiet hours are over");
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
                let report = self.h.report().await;
                if let (Some(scan), HelloHardwareSource::File) =
                    (report.hardware, report.hardware_source)
                {
                    let frame = protocol::Hardware { scan, source: protocol::HardwareSource::File };
                    return self.h.send(tx, HelperFrame::Hardware(frame)).await;
                }
                Ok(())
            }
            ServerFrame::Request(r) => {
                // The proxy is 4-04's: until then the helper offers no model work.
                let e = RequestError {
                    id: r.id,
                    code: RequestErrorCode::UnknownModel,
                    message: "this helper does not serve completions yet".to_owned(),
                    retryable: false,
                };
                self.h.send(tx, HelperFrame::Error(e)).await
            }
            ServerFrame::JobOffer(o) => {
                // Jobs are 4-10's: this helper has no jobs build, so no offer is its build.
                let r = JobReject { run: o.run, reason: RejectReason::Build };
                self.h.send(tx, HelperFrame::JobReject(r)).await
            }
            other => {
                // cancel (nothing in flight), job_result_ack, job_abort, jobs_pause,
                // jobs_resume (no runs), update (4-13), pong, and a second welcome.
                self.h.log.debug(format!("nothing to do for {}", other.type_name()));
                Ok(())
            }
        }
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
