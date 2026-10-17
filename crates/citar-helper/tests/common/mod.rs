//! The transcript player of the helper's side (PROTOCOL.md "Transcripts", P4.2.9): a fake server
//! that sends the transcript's server frames, checks every frame the helper sends against the
//! transcript's patterns, and plays the fake model endpoint. It drives either the helper's own
//! session in-process (`tests/conformance.rs`: on tokio's paused clock, the fake clock, when no
//! completion runs; in real time against [`FakeEndpoint`] when one does) or the real binary over
//! loopback (`tests/real_binary.rs`, in real time).
//!
//! [`FakeEndpoint`] is the fake model endpoint over real HTTP on loopback (4-04): it lists models
//! as LM Studio does, answers each completion with the behaviour the transcript scripts for it
//! (an SSE stream with delays between its events, a JSON body, an HTTP error, no answer, or the
//! connection closed at once), and records each request's body, the connection it came on, when
//! each streamed event went out and when its client went. It answers one request per connection
//! (`connection: close`) unless told to keep connections open (`keep_alive`), as LM Studio and
//! the others do, for the helper's reuse of a finished answer's connection. Told to have one
//! model (`one_model`), it streams one answer at a time, as a server with one model loaded does,
//! and can play a server that does not stop a generation when its client goes.
//!
//! [`FakeHosts`] is the fake job host (4-10): it accepts every offer the helper relays, lets an
//! aborted run go (unless it hangs), and does what the transcript's `host` steps say: report a
//! run's progress, end it with a golden job's outcome, die (of memory or otherwise), or hang.
//!
//! The rules it keeps: background frames (`ping`, `pong`, `status`, `power`) are skipped unless
//! the next step expects their type, and the helper's own unscripted pings are answered; a
//! `quiet` wait allows only background frames (not of the next step's type) and no new
//! connection; any other frame a wait sees is kept for the steps after it; `within_s` is
//! measured from the end of the previous step. A step this player cannot do yet (a later
//! package's) is refused, never skipped. Golden values are the committed `jobs` set's.

#![allow(dead_code, clippy::missing_panics_doc)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use citar_helper::conformance::matcher::{Bindings, Committed, Golden, matches, realize};
use citar_helper::conformance::transcript::{Action, BACKGROUND, Side, Step, Transcript};
use citar_helper::llm::{BoxFuture, Endpoint, Listed};
use citar_helper::net::Io;
use citar_helper::protocol::{Sender, parse_helper_frame, parse_server_frame};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

/// How long an `h2s` step without `within_s` waits for its frame.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);
/// A step's `within_s` is inclusive: a frame due exactly at its end counts. On the paused clock a
/// frame the helper times to the same instant as the step's end (a `status` 5 s after the last,
/// in `jobs-pause`) would otherwise race the step's own timer.
pub const WITHIN_SLACK: Duration = Duration::from_millis(10);

/// How long a step may take: its `within_s` (inclusive), else [`DEFAULT_WAIT`].
fn step_time(s: &Step) -> Duration {
    s.within_s.map_or(DEFAULT_WAIT, |w| Duration::from_secs_f64(w) + WITHIN_SLACK)
}

/// The fake model endpoint's list, shared with whatever serves it; it can be told to fail its
/// next listings.
#[derive(Clone, Default)]
pub struct Models {
    list: Arc<Mutex<Vec<Listed>>>,
    failing: Arc<Mutex<u32>>,
}

/// What a listing of [`Models`] that fails says.
pub const MODELS_FAIL: &str = "the fake endpoint refused the connection";

impl Models {
    pub fn set(&self, models: Vec<Listed>) {
        *self.list.lock().expect("models") = models;
    }

    pub fn get(&self) -> Vec<Listed> {
        self.list.lock().expect("models").clone()
    }

    /// The next `n` listings fail with [`MODELS_FAIL`].
    pub fn fail_next(&self, n: u32) {
        *self.failing.lock().expect("failing") = n;
    }
}

impl Endpoint for Models {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Listed>, String>> {
        let fail = {
            let mut f = self.failing.lock().expect("failing");
            let fail = *f > 0;
            *f = f.saturating_sub(1);
            fail
        };
        let models = self.get();
        Box::pin(async move { if fail { Err(MODELS_FAIL.to_owned()) } else { Ok(models) } })
    }
}

/// An in-memory connection per dial: the fake server's end of each goes down the channel, its
/// WebSocket handshake done.
pub struct Pipe(pub mpsc::UnboundedSender<ServerWs>);

impl citar_helper::net::Dialer for Pipe {
    fn dial(&self) -> BoxFuture<'_, Result<citar_helper::net::Ws, String>> {
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 20);
            let tx = self.0.clone();
            // The server's half of the handshake runs beside the client's.
            tokio::spawn(async move {
                let server: Box<dyn Io> = Box::new(server);
                let cfg = Some(citar_helper::net::ws_config());
                if let Ok(ws) = tokio_tungstenite::accept_async_with_config(server, cfg).await {
                    let _gone = tx.send(ws);
                }
            });
            let client: Box<dyn Io> = Box::new(client);
            let (ws, _) = tokio_tungstenite::client_async_with_config(
                "ws://127.0.0.1/ws/worker",
                client,
                Some(citar_helper::net::ws_config()),
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(ws)
        })
    }
}

/// A transcript's `models` value as the endpoint lists them.
pub fn listed(v: &Value) -> Vec<Listed> {
    v.as_array()
        .expect("models is a list")
        .iter()
        .map(|m| Listed {
            key: m["key"].as_str().expect("a key").to_owned(),
            label: m["label"].as_str().unwrap_or_default().to_owned(),
            context: m["context"].as_u64(),
            loaded: m["loaded"].as_bool().unwrap_or(false),
        })
        .collect()
}

/// The fake server's side of one connection.
pub type ServerWs = WebSocketStream<Box<dyn Io>>;

/// What the player knows of the helper under test.
pub trait UnderTest {
    /// The helper's exit code, once it has exited within `within`; `None` if it is still running.
    fn exited(&mut self, within: Duration) -> BoxFuture<'_, Option<i32>>;
    /// Whether the helper's log holds `text`.
    fn logged(&self, text: &str) -> bool;
    /// The completions in flight on the helper, when the player can see them.
    fn in_flight(&self) -> Option<usize> {
        None
    }
    /// Something happens around the helper (an `act` with side `helper`).
    fn act(&mut self, what: &Value);
    /// What the helper's job side holds (`held_results`, `running`), when the player can see it.
    fn jobs(&self) -> Option<citar_helper::jobs::Snapshot> {
        None
    }
    /// The fake job host's next behaviour (a `host` step), when the helper runs one.
    fn host(&mut self, step: &Map<String, Value>, golden: &Committed) -> Result<(), String> {
        let _unused = (step, golden);
        Err("this player has no fake job host".to_owned())
    }
    /// The update downloads the helper made (`downloads`), when the player can see them.
    fn downloads(&self) -> Option<usize> {
        None
    }
}

/// The manifest the fake server relays to a helper of `version` for `target` (the transcripts'
/// `<string>` manifest): one version on (its patch number plus one), a binary for this target at
/// an address it never reaches in the transcripts (each refuses or stops before a download).
#[must_use]
pub fn test_manifest(version: &str, target: &str, build: &str) -> String {
    let v = semver_bump(version);
    serde_json::json!({
        "v": 1,
        "version": v,
        "protocol": 2,
        "build": build,
        "released_at": "2026-11-01T00:00:00Z",
        "assets": {
            target: {
                "name": "citar-helper-test",
                "url": format!("https://127.0.0.1:9/download/v{v}/citar-helper-test"),
                "sha256": "ab".repeat(32),
                "size": 1024,
            }
        }
    })
    .to_string()
}

/// `version` with its patch number one higher (its pre-release dropped).
fn semver_bump(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts: Vec<u64> = core.split('.').filter_map(|p| p.parse().ok()).collect();
    parts.resize(3, 0);
    parts[2] += 1;
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

/// Whether a pattern's value is a matcher (`<string>`, `<base64>`, …) rather than a literal.
fn is_matcher(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| s.starts_with('<') && s.ends_with('>'))
}

/// Something the helper did on the wire.
#[derive(Debug)]
enum Event {
    Frame(Value),
    Closed(Option<u16>),
}

/// The next text frame or the end of a connection; WebSocket control frames are passed over.
async fn recv_event(ws: &mut ServerWs) -> Event {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                return Event::Frame(serde_json::from_str(t.as_str()).unwrap_or(Value::Null));
            }
            Some(Ok(Message::Close(c))) => return Event::Closed(c.map(|c| u16::from(c.code))),
            Some(Ok(_)) => {}
            None | Some(Err(_)) => return Event::Closed(None),
        }
    }
}

/// The fake server, playing one transcript.
pub struct Player<'t> {
    pub t: &'t Transcript,
    pub bindings: Bindings,
    canonical: BTreeMap<(Sender, String), Value>,
    /// Accepted connections, in the order the helper opened them.
    conns: mpsc::UnboundedReceiver<ServerWs>,
    arrived: VecDeque<ServerWs>,
    cur: Option<ServerWs>,
    /// What the current connection saw and no step has taken yet.
    inbox: VecDeque<Event>,
    pub models: Models,
    /// The fake model endpoint over HTTP, for the transcripts that run completions.
    pub endpoint: Option<FakeEndpoint>,
    /// The next completion request an `expect_request` step looks at.
    expect_cursor: usize,
    /// `respond` steps played: the last one answers request `responds - 1`.
    responds: usize,
    /// When the last `cancel` frame went to the helper.
    cancel_sent: Option<Instant>,
    last: Instant,
    /// The committed golden sets the transcripts' `<golden:…>` values come from.
    pub golden: Committed,
    /// The throwaway update key the run's helper trusts (`setup.helper.update_key: "test"`), and
    /// the helper's version, target and build: a relayed manifest the transcript leaves open
    /// (`<string>`) is one for it, signed by this key when the transcript leaves the signature
    /// open too (a literal signature is sent as it is: none of this key's).
    pub update_key: Option<citar_jobs::claim::SigningKey>,
    pub update_for: (String, String, String),
}

fn t_of(v: &Value) -> &str {
    v.get("t").and_then(Value::as_str).unwrap_or("?")
}

impl<'t> Player<'t> {
    pub fn new(
        t: &'t Transcript,
        conns: mpsc::UnboundedReceiver<ServerWs>,
        models: Models,
        canonical: BTreeMap<(Sender, String), Value>,
    ) -> Player<'t> {
        Player {
            t,
            bindings: Bindings::default(),
            canonical,
            conns,
            arrived: VecDeque::new(),
            cur: None,
            inbox: VecDeque::new(),
            models,
            endpoint: None,
            expect_cursor: 0,
            responds: 0,
            cancel_sent: None,
            last: Instant::now(),
            golden: Committed::load().expect("the golden sets read"),
            update_key: None,
            update_for: (String::new(), String::new(), String::new()),
        }
    }

    /// A relayed manifest the pattern leaves open, made for the helper and signed (the field's
    /// doc); `update` is the frame's `update` object (or the `update` frame itself).
    fn realize_update(&self, pattern: Option<&Value>, update: &mut Value) {
        let Some(pattern) = pattern else { return };
        if !is_matcher(pattern.get("manifest")) {
            return;
        }
        let (version, target, build) = &self.update_for;
        let manifest = test_manifest(version, target, build);
        if is_matcher(pattern.get("signature"))
            && let Some(k) = &self.update_key
        {
            update["signature"] = Value::String(citar_jobs::update::sign(manifest.as_bytes(), k));
        }
        update["manifest"] = Value::String(manifest);
    }

    fn fail(&self, i: usize, why: impl std::fmt::Display) -> String {
        format!("{} step {i}: {why}", self.t.name)
    }

    /// The next connection the helper opens, within `until`.
    async fn next_conn(&mut self, until: Instant) -> Option<ServerWs> {
        if let Some(c) = self.arrived.pop_front() {
            return Some(c);
        }
        tokio::time::timeout_at(until, self.conns.recv()).await.ok().flatten()
    }

    /// The current connection, waiting for the helper's first one when there is none yet.
    async fn ensure_conn(&mut self, i: usize) -> Result<(), String> {
        if self.cur.is_none() {
            let c = self.next_conn(Instant::now() + DEFAULT_WAIT).await;
            self.cur = Some(c.ok_or_else(|| self.fail(i, "the helper opened no connection"))?);
            self.inbox.clear();
        }
        Ok(())
    }

    async fn send_text(&mut self, text: String) -> Result<(), String> {
        let ws = self.cur.as_mut().ok_or("no connection to send on")?;
        ws.send(Message::Text(text.into())).await.map_err(|e| format!("send: {e}"))
    }

    /// Reads the current connection until `until`: an event, or `None` at the deadline. A
    /// connection that ended yields `Closed` once and is then gone.
    async fn read(&mut self, until: Instant) -> Option<Event> {
        if let Some(e) = self.inbox.pop_front() {
            return Some(e);
        }
        let ws = self.cur.as_mut()?;
        let ev = tokio::time::timeout_at(until, recv_event(ws)).await.ok()?;
        if matches!(ev, Event::Closed(_)) {
            self.cur = None;
        }
        Some(ev)
    }

    /// The type the step after `i` expects from the helper, if it is a frame step.
    fn next_expects(&self, i: usize) -> Option<String> {
        self.t.steps.get(i + 1).and_then(|s| match &s.action {
            Action::Frame { from: Sender::Helper, pattern } => Some(t_of(pattern).to_owned()),
            _ => None,
        })
    }

    /// Answers the helper's unscripted ping.
    async fn pong(&mut self) -> Result<(), String> {
        self.send_text(r#"{"t": "pong"}"#.to_owned()).await
    }

    fn check_helper_frame(&self, i: usize, v: &Value) -> Result<(), String> {
        parse_helper_frame(&v.to_string()).map(|_| ()).map_err(|e| {
            self.fail(i, format!("the helper sent a frame that fails its checks: {e}: {v}"))
        })
    }

    async fn h2s(&mut self, i: usize, s: &Step, pattern: &Value) -> Result<(), String> {
        self.ensure_conn(i).await?;
        let want = t_of(pattern).to_owned();
        let until = self.last + step_time(s);
        loop {
            let ev = self.read(until).await.ok_or_else(|| {
                self.fail(i, format!("no {want} from the helper within the step's time: {pattern}"))
            })?;
            let v = match ev {
                Event::Frame(v) => v,
                Event::Closed(c) => {
                    return Err(self.fail(i, format!("the connection ended ({c:?}) before {want}")));
                }
            };
            self.check_helper_frame(i, &v)?;
            let got = t_of(&v).to_owned();
            let background = BACKGROUND.contains(&got.as_str());
            if got != want && background {
                if got == "ping" {
                    self.pong().await?;
                }
                continue;
            }
            match matches(pattern, &v, &mut self.bindings, &self.golden) {
                Ok(()) => return Ok(()),
                Err(_) if got == want && background => {}
                Err(m) => {
                    return Err(self.fail(i, format!("{m}\n  sent    {v}\n  pattern {pattern}")));
                }
            }
        }
    }

    async fn s2h(&mut self, i: usize, s: &Step, pattern: &Value) -> Result<(), String> {
        let t = t_of(pattern).to_owned();
        let example = self.canonical.get(&(Sender::Server, t.clone())).cloned();
        let mut frame =
            realize(pattern, &mut self.bindings, &self.t.values, &self.golden, example.as_ref())
                .map_err(|e| self.fail(i, e))?;
        if t == "update" {
            let mut u = frame.clone();
            self.realize_update(Some(pattern), &mut u);
            frame = u;
        } else if t == "welcome" && frame.get("update").is_some_and(Value::is_object) {
            let mut u = frame["update"].clone();
            self.realize_update(pattern.get("update"), &mut u);
            frame["update"] = u;
        }
        if s.invalid.is_none() {
            parse_server_frame(&frame.to_string()).map_err(|e| {
                self.fail(i, format!("the realized frame fails its checks: {e}: {frame}"))
            })?;
        }
        if s.lost {
            return Ok(());
        }
        self.ensure_conn(i).await?;
        self.send_text(frame.to_string()).await.map_err(|e| self.fail(i, e))?;
        if t == "cancel" {
            self.cancel_sent = Some(Instant::now());
        }
        Ok(())
    }

    async fn wait(&mut self, i: usize, secs: f64, quiet: bool) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        let next = self.next_expects(i);
        // What was read before the wait comes before anything the wait sees.
        let mut kept: VecDeque<Event> = std::mem::take(&mut self.inbox);
        loop {
            let ev = match self.cur.as_mut() {
                Some(ws) => tokio::select! {
                    e = recv_event(ws) => Some(Ok(e)),
                    c = self.conns.recv() => Some(Err(c)),
                    () = tokio::time::sleep_until(until) => None,
                },
                None => tokio::select! {
                    c = self.conns.recv() => Some(Err(c)),
                    () = tokio::time::sleep_until(until) => None,
                },
            };
            match ev {
                None => break,
                Some(Err(Some(c))) => {
                    if quiet {
                        return Err(
                            self.fail(i, "the helper opened a connection during a quiet wait")
                        );
                    }
                    self.arrived.push_back(c);
                }
                Some(Err(None)) => {
                    tokio::time::sleep_until(until).await;
                    break;
                }
                Some(Ok(Event::Closed(c))) => {
                    self.cur = None;
                    if quiet {
                        return Err(self.fail(
                            i,
                            format!("the helper ended the connection ({c:?}) during a quiet wait"),
                        ));
                    }
                    kept.push_back(Event::Closed(c));
                }
                Some(Ok(Event::Frame(v))) => {
                    let got = t_of(&v).to_owned();
                    if BACKGROUND.contains(&got.as_str()) && next.as_deref() != Some(got.as_str()) {
                        if got == "ping" {
                            self.pong().await?;
                        }
                        continue;
                    }
                    if quiet {
                        return Err(
                            self.fail(i, format!("the helper sent {got} during a quiet wait: {v}"))
                        );
                    }
                    kept.push_back(Event::Frame(v));
                }
            }
        }
        self.inbox = kept;
        Ok(())
    }

    async fn helper_closes(&mut self, i: usize, s: &Step, code: Option<u16>) -> Result<(), String> {
        let until = self.last + step_time(s);
        loop {
            match self.read(until).await {
                None if self.cur.is_none() => {
                    return if code.is_none() {
                        Ok(())
                    } else {
                        Err(self.fail(i, "the connection is gone, with no close to read"))
                    };
                }
                None => return Err(self.fail(i, "the helper did not end the connection in time")),
                Some(Event::Closed(c)) => {
                    return match code {
                        None => Ok(()),
                        Some(want) if c == Some(want) => Ok(()),
                        Some(want) => Err(self.fail(i, format!("closed with {c:?}, not {want}"))),
                    };
                }
                Some(Event::Frame(v)) if BACKGROUND.contains(&t_of(&v)) => {}
                Some(Event::Frame(v)) => {
                    return Err(
                        self.fail(i, format!("{v} where the helper should end the connection"))
                    );
                }
            }
        }
    }

    /// Plays the transcript's steps against the helper.
    pub async fn play(&mut self, helper: &mut dyn UnderTest) -> Result<(), String> {
        self.last = Instant::now();
        for (i, s) in self.t.steps.iter().enumerate() {
            match &s.action {
                Action::Frame { from: Sender::Helper, pattern } => self.h2s(i, s, pattern).await?,
                Action::Frame { from: Sender::Server, pattern } => self.s2h(i, s, pattern).await?,
                Action::Close { by: Sender::Server, code, reason } => {
                    self.ensure_conn(i).await?;
                    let mut ws = self.cur.take().ok_or_else(|| self.fail(i, "no connection"))?;
                    let frame = CloseFrame {
                        code: CloseCode::from(*code),
                        reason: reason.clone().unwrap_or_default().into(),
                    };
                    let _sent = ws.close(Some(frame)).await;
                    self.inbox.clear();
                }
                Action::Close { by: Sender::Helper, code, .. } => {
                    self.helper_closes(i, s, Some(*code)).await?;
                }
                Action::Drop { by: Sender::Server } => {
                    self.ensure_conn(i).await?;
                    self.cur = None;
                    self.inbox.clear();
                }
                Action::Drop { by: Sender::Helper } => self.helper_closes(i, s, None).await?,
                Action::Connect => {
                    let until = self.last + step_time(s);
                    let c = self.next_conn(until).await;
                    self.cur = Some(c.ok_or_else(|| {
                        self.fail(i, "the helper opened no new connection in time")
                    })?);
                    self.inbox.clear();
                }
                Action::Wait { s: secs, quiet } => self.wait(i, *secs, *quiet).await?,
                Action::Endpoint(e) => self.endpoint_step(i, s, e).await?,
                Action::Host(h) => {
                    helper.host(h, &self.golden).map_err(|e| self.fail(i, e))?;
                }
                Action::Act { side: Side::Hub, .. } | Action::State { side: Side::Hub, .. } => {}
                Action::Act { side: Side::Helper, what } => helper.act(what),
                Action::State { side: Side::Helper, expect } => {
                    self.state(i, s, expect, helper).await?
                }
            }
            self.last = Instant::now();
        }
        Ok(())
    }

    /// An `endpoint` step: the fake endpoint's next behaviour, or an expectation of it.
    async fn endpoint_step(
        &mut self,
        i: usize,
        s: &Step,
        e: &Map<String, Value>,
    ) -> Result<(), String> {
        if let Some(m) = e.get("models") {
            self.models.set(listed(m));
        }
        let rest: Vec<&String> = e.keys().filter(|k| *k != "models").collect();
        if rest.is_empty() {
            return Ok(());
        }
        let Some(ep) = self.endpoint.clone() else {
            return Err(self.fail(i, format!("this player has no endpoint to play {rest:?} with")));
        };
        let until = self.last + step_time(s);
        if let Some(pattern) = e.get("expect_request") {
            let k = self.expect_cursor;
            self.expect_cursor += 1;
            let body = ep.request_body(k, until).await.ok_or_else(|| {
                self.fail(i, format!("the endpoint saw no request {k} within the step's time"))
            })?;
            matches(pattern, &body, &mut self.bindings, &self.golden).map_err(|m| {
                self.fail(i, format!("the endpoint's request: {m}\n  body    {body}"))
            })?;
            for key in e.get("absent").and_then(Value::as_array).into_iter().flatten() {
                let key = key.as_str().unwrap_or_default();
                if body.get(key).is_some() {
                    return Err(self.fail(i, format!("the endpoint's request has {key:?}: {body}")));
                }
            }
        }
        if let Some(r) = e.get("respond") {
            let behaviour = Behaviour::from_step(r).map_err(|m| self.fail(i, m))?;
            // The endpoint answers a request it has: the step waits for it to come.
            let k = self.responds;
            if ep.request_body(k, until).await.is_none() {
                return Err(
                    self.fail(i, format!("no request {k} came to answer within the step's time"))
                );
            }
            ep.respond(behaviour);
            self.responds += 1;
        }
        let current = self.responds.checked_sub(1);
        if let Some(ms) = e.get("expect_disconnect_within_ms").and_then(Value::as_f64) {
            let k = current.ok_or_else(|| self.fail(i, "no completion was answered yet"))?;
            let by = self.last + Duration::from_secs_f64(ms / 1000.0);
            if ep.gone(k, by).await.is_none() {
                return Err(self.fail(
                    i,
                    format!(
                        "the endpoint's client {k} did not go within {ms} ms of the step before"
                    ),
                ));
            }
        }
        if let Some(after) = e.get("expect_no_tokens_after") {
            if after != "cancel" {
                return Err(self.fail(i, format!("expect_no_tokens_after {after} is not known")));
            }
            // Measured from the cancel as the fake server sent it, not from the fake endpoint's
            // own record of the disconnect (which no event can pass): at most the one event that
            // was already on its way may leave after it.
            let cancel = self.cancel_sent.ok_or_else(|| self.fail(i, "no cancel was sent"))?;
            let k = current.ok_or_else(|| self.fail(i, "no completion was answered yet"))?;
            let seen = ep.seen(k).ok_or_else(|| self.fail(i, "no such request"))?;
            let late = seen.tokens.iter().filter(|t| **t > cancel).count();
            if late > 1 {
                return Err(self.fail(i, format!("{late} events went out after the cancel")));
            }
        }
        Ok(())
    }

    async fn state(
        &mut self,
        i: usize,
        s: &Step,
        expect: &Map<String, Value>,
        helper: &mut dyn UnderTest,
    ) -> Result<(), String> {
        for (k, v) in expect {
            match k.as_str() {
                "exit" => {
                    let within = step_time(s);
                    let code = helper.exited(within).await;
                    let want = v.as_i64().map(|c| i32::try_from(c).unwrap_or(-1));
                    if code != want {
                        return Err(
                            self.fail(i, format!("the helper's exit is {code:?}, not {want:?}"))
                        );
                    }
                    // "Without reconnecting": no connection opened after the refusal.
                    if let Ok(c) = self.conns.try_recv() {
                        drop(c);
                        return Err(self.fail(i, "the helper reconnected before it exited"));
                    }
                }
                "in_flight" => {
                    let want = v.as_u64().and_then(|n| usize::try_from(n).ok());
                    // A dropped completion leaves the count when its task is gone: give it a
                    // moment, as `logged` does.
                    let until = Instant::now() + Duration::from_secs(2);
                    let mut got = helper.in_flight();
                    while got.is_some() && got != want && Instant::now() < until {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        got = helper.in_flight();
                    }
                    if got.is_none() {
                        return Err(self.fail(i, "this player cannot see the helper's in_flight"));
                    }
                    if got != want {
                        return Err(self.fail(
                            i,
                            format!("the helper has {got:?} completions in flight, not {want:?}"),
                        ));
                    }
                }
                "logged" => {
                    let text = v.as_str().unwrap_or_default();
                    // The log line is written when the close is read: give it a moment.
                    let until = Instant::now() + Duration::from_secs(2);
                    while !helper.logged(text) && Instant::now() < until {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    if !helper.logged(text) {
                        return Err(
                            self.fail(i, format!("the helper's log has no line with {text:?}"))
                        );
                    }
                }
                "downloads" => {
                    let want = v.as_u64().and_then(|n| usize::try_from(n).ok());
                    let got = helper.downloads();
                    if got.is_none() {
                        return Err(self.fail(i, "this player cannot see the helper's downloads"));
                    }
                    if got != want {
                        return Err(self.fail(
                            i,
                            format!("the helper made {got:?} update downloads, not {want:?}"),
                        ));
                    }
                }
                "held_results" | "running" => {
                    // The job side's state follows the frames it hands out: give it a moment.
                    let until = Instant::now() + Duration::from_secs(2);
                    loop {
                        let snap = helper.jobs().ok_or_else(|| {
                            self.fail(i, "this player cannot see the helper's job side")
                        })?;
                        let have = if k == "held_results" { snap.held } else { snap.running };
                        let have = Value::from(have);
                        let mut b = self.bindings.clone();
                        match matches(v, &have, &mut b, &self.golden) {
                            Ok(()) => {
                                self.bindings = b;
                                break;
                            }
                            Err(m) if Instant::now() >= until => {
                                return Err(self.fail(i, format!("the helper's {k}: {m}: {have}")));
                            }
                            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                        }
                    }
                }
                other => {
                    return Err(self
                        .fail(i, format!("this player cannot yet check the helper's {other:?}")));
                }
            }
        }
        Ok(())
    }
}

/// A scanner for the in-process helper (4-06): it answers the scan it holds at once (a test may
/// change it), and counts the scans it made.
#[derive(Clone)]
pub struct FixedScanner {
    pub scan: Arc<Mutex<Map<String, Value>>>,
    pub made: Arc<std::sync::atomic::AtomicUsize>,
}

impl FixedScanner {
    #[must_use]
    pub fn new(scan: Map<String, Value>) -> FixedScanner {
        FixedScanner { scan: Arc::new(Mutex::new(scan)), made: Arc::default() }
    }

    /// The scans made so far.
    #[must_use]
    pub fn made(&self) -> usize {
        self.made.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// What the next scans find.
    pub fn set(&self, scan: Map<String, Value>) {
        *self.scan.lock().expect("the scan") = scan;
    }
}

impl citar_helper::scan::Scanner for FixedScanner {
    fn scan(
        &self,
        _runtimes: Vec<citar_helper::scan::Runtime>,
    ) -> BoxFuture<'static, Option<Map<String, Value>>> {
        self.made.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let scan = self.scan.lock().expect("the scan").clone();
        Box::pin(async move { Some(scan) })
    }
}

/// A small scan in the helper's own format, with `cpu` as its CPU's name.
#[must_use]
pub fn test_scan(cpu: &str) -> Map<String, Value> {
    let v = serde_json::json!({
        "format": citar_helper::scan::FORMAT, "collector_version": citar_helper::scan::COLLECTOR_VERSION,
        "collector": citar_helper::scan::COLLECTOR, "collected_at": "2026-10-14T12:00:00",
        "hostname": "transcript-host", "system": {"form": "desktop"},
        "cpu": {"model": cpu, "cores": 8, "threads": 16}, "memory": {"ram_gb": 32.0, "unified": false},
        "gpus": [], "physical_disks": [], "volumes": [], "runtimes": [],
        "power": {"gpu_sampling": false}
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!("an object"),
    }
}

/// Power readings for the in-process helper: the same reading every time.
pub struct FixedPower(pub citar_helper::power::Reading);

impl citar_helper::power::PowerProbe for FixedPower {
    fn read(&self) -> BoxFuture<'static, Option<citar_helper::power::Reading>> {
        let r = self.0;
        Box::pin(async move { Some(r) })
    }
}

/// A fake hub on loopback: every connection accepted, its WebSocket handshake done, handed to the
/// player. Returns the port.
pub async fn loopback_hub() -> (u16, mpsc::UnboundedReceiver<ServerWs>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let io: Box<dyn Io> = Box::new(tcp);
                let cfg = citar_helper::net::ws_config();
                if let Ok(ws) = tokio_tungstenite::accept_async_with_config(io, Some(cfg)).await {
                    let _gone = tx.send(ws);
                }
            });
        }
    });
    (port, rx)
}

/// A fake LM Studio on loopback: `/api/v0/models` lists `models` as they are when asked; any
/// other path is 404. Returns the port.
pub async fn loopback_lmstudio(models: Models) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let models = models.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let line =
                    String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_owned();
                let (status, body) = if line.starts_with("GET /api/v0/models ") {
                    let data: Vec<Value> = models
                        .get()
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "id": m.key, "object": "model", "type": "llm",
                                "state": if m.loaded { "loaded" } else { "not-loaded" },
                                "max_context_length": m.context,
                            })
                        })
                        .collect();
                    ("200 OK", serde_json::json!({"object": "list", "data": data}).to_string())
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _sent = tcp.write_all(resp.as_bytes()).await;
                let _closed = tcp.shutdown().await;
            });
        }
    });
    port
}

/// A fresh scratch directory for one test, under the system's temporary directory.
pub fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("citar-helper-it-{}-{name}", std::process::id()));
    let _fresh = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a scratch directory");
    d
}

/// A fake HTTP endpoint on loopback serving fixed routes (`"GET /path"` to a status and a JSON
/// body; anything else 404), and keeping every request's head. Returns the port.
pub async fn loopback_routes(
    routes: BTreeMap<String, (u16, String)>,
    heads: Arc<Mutex<Vec<String>>>,
) -> u16 {
    loopback_routes_shared(Arc::new(Mutex::new(routes)), heads).await
}

/// [`loopback_routes`] with routes the test may change between requests. A 3xx route's "body" is
/// the `location` it redirects to.
pub async fn loopback_routes_shared(
    routes: Arc<Mutex<BTreeMap<String, (u16, String)>>>,
    heads: Arc<Mutex<Vec<String>>>,
) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let routes = routes.clone();
            let heads = heads.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                heads.lock().expect("heads").push(text);
                let key = line.rsplit_once(' ').map_or(line.as_str(), |(k, _)| k).to_owned();
                let route = routes.lock().expect("routes").get(&key).cloned();
                let (status, body) = route.unwrap_or((404, "{}".to_owned()));
                let resp = if (300..400).contains(&status) {
                    format!(
                        "HTTP/1.1 {status} X\r\nlocation: {body}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    )
                } else {
                    format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _sent = tcp.write_all(resp.as_bytes()).await;
                let _closed = tcp.shutdown().await;
            });
        }
    });
    port
}

/// What [`FakeEndpoint`] does with one completion request.
#[derive(Clone, Debug)]
pub enum Behaviour {
    /// A `text/event-stream` answer: its head at once, the first event after `first` (a model
    /// thinking before its first token), the others `delay` apart, then `data: [DONE]`. It stops
    /// the moment its client goes.
    Stream { events: Vec<Value>, delay: Duration, first: Duration },
    /// A JSON answer.
    Json(Value),
    /// An HTTP error with this body.
    Status(u16, String),
    /// No answer at all, until the client goes.
    Hang,
    /// The connection closed at once with no answer: an endpoint closing a kept connection just
    /// as a request came on it.
    Close,
}

impl Behaviour {
    /// A transcript's `respond` value.
    pub fn from_step(r: &Value) -> Result<Behaviour, String> {
        if let Some(events) = r.get("stream").and_then(Value::as_array) {
            let delay =
                Duration::from_millis(r.get("delay_ms").and_then(Value::as_u64).unwrap_or(0));
            return Ok(Behaviour::Stream { events: events.clone(), delay, first: Duration::ZERO });
        }
        if let Some(body) = r.get("json") {
            return Ok(Behaviour::Json(body.clone()));
        }
        if let Some(status) = r.get("status").and_then(Value::as_u64) {
            let body = r.get("body").and_then(Value::as_str).unwrap_or_default().to_owned();
            return Ok(Behaviour::Status(u16::try_from(status).unwrap_or(500), body));
        }
        if r.get("hang").and_then(Value::as_bool) == Some(true) {
            return Ok(Behaviour::Hang);
        }
        Err(format!("a respond the fake endpoint cannot play: {r}"))
    }
}

/// One completion request [`FakeEndpoint`] saw.
#[derive(Clone, Debug)]
pub struct Seen {
    /// The request's line and headers.
    pub head: String,
    pub body: Value,
    pub at: Instant,
    /// The connection it came on, numbered from 0 as they were accepted.
    pub conn: usize,
    /// When each streamed event went out (taken while the client was still there).
    pub tokens: Vec<Instant>,
    /// When its connection's client went, once it has.
    pub gone: Option<Instant>,
}

/// One connection [`FakeEndpoint`] accepted.
#[derive(Default)]
struct ConnRec {
    /// What the client sent that no request has taken yet.
    buf: Vec<u8>,
    /// When the client went (its end closed, or the connection broke).
    closed: Option<Instant>,
    /// The completion requests it carried.
    requests: Vec<usize>,
    /// Waiting for its next request, with none in flight.
    idle: bool,
    /// Told to close while idle ([`FakeEndpoint::close_idle`]).
    kill: bool,
}

#[derive(Default)]
struct FakeState {
    seen: Vec<Seen>,
    /// The behaviour for each request, in the order they came.
    behaviours: Vec<Behaviour>,
    conns: Vec<ConnRec>,
    /// Keep each connection open after an answer, for the next request (off: `connection:
    /// close`, one request per connection).
    keep_alive: bool,
    /// One model behind every streamed answer ([`FakeEndpoint::one_model`]).
    one_model: Option<Arc<tokio::sync::Mutex<()>>>,
    /// A streamed answer whose client went keeps the model to its last event.
    runs_on: bool,
}

/// A request read whole from a connection's bytes: its head, and where it and its body end.
fn whole_request(buf: &[u8]) -> Option<(String, usize, usize)> {
    let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    (buf.len() >= head_end + length).then_some((head, head_end, length))
}

/// The fake model endpoint over HTTP on loopback (see the module's doc).
#[derive(Clone)]
pub struct FakeEndpoint {
    pub port: u16,
    pub models: Models,
    state: Arc<Mutex<FakeState>>,
    changed: Arc<tokio::sync::Notify>,
}

impl FakeEndpoint {
    /// Listening on a loopback port, listing `models`.
    pub async fn start(models: Models) -> FakeEndpoint {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
        let port = listener.local_addr().expect("its address").port();
        let ep = FakeEndpoint {
            port,
            models,
            state: Arc::new(Mutex::new(FakeState::default())),
            changed: Arc::new(tokio::sync::Notify::new()),
        };
        let server = ep.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let ep = server.clone();
                tokio::spawn(async move { ep.serve(tcp).await });
            }
        });
        ep
    }

    /// The base URL a helper is given.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("the fake endpoint's state")
    }

    /// Whether answers keep their connection open for the next request (from the next answer).
    pub fn keep_alive(&self, on: bool) {
        self.lock().keep_alive = on;
    }

    /// One model generates every streamed answer, one at a time, as a local server with one model
    /// loaded does: an answer waits for the one before it to end. With `runs_on`, a generation
    /// whose client went keeps the model busy until its last event would have gone (a server that
    /// does not stop a generation when its client goes), so the answers after it wait; without,
    /// the model is free the moment the client goes.
    pub fn one_model(&self, runs_on: bool) {
        let mut s = self.lock();
        s.one_model = Some(Arc::new(tokio::sync::Mutex::new(())));
        s.runs_on = runs_on;
    }

    /// Closes every connection that waits for its next request, as an endpoint's idle timeout
    /// does.
    pub fn close_idle(&self) {
        for c in &mut self.lock().conns {
            if c.idle && c.closed.is_none() {
                c.kill = true;
            }
        }
        self.changed.notify_waiters();
    }

    /// The connections accepted so far.
    pub fn connections(&self) -> usize {
        self.lock().conns.len()
    }

    /// The connections still open.
    pub fn open_connections(&self) -> usize {
        self.lock().conns.iter().filter(|c| c.closed.is_none()).count()
    }

    /// The behaviour for the next completion request without one.
    pub fn respond(&self, b: Behaviour) {
        self.lock().behaviours.push(b);
        self.changed.notify_waiters();
    }

    /// Request `k` as it is now.
    pub fn seen(&self, k: usize) -> Option<Seen> {
        self.lock().seen.get(k).cloned()
    }

    /// The completion requests seen so far.
    pub fn requests(&self) -> usize {
        self.lock().seen.len()
    }

    /// Waits until `f` of the state gives a value, or `until`.
    async fn wait_for<T>(&self, until: Instant, f: impl Fn(&FakeState) -> Option<T>) -> Option<T> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(v) = f(&self.lock()) {
                return Some(v);
            }
            if tokio::time::timeout_at(until, notified).await.is_err() {
                return f(&self.lock());
            }
        }
    }

    /// Request `k`'s body, once it has come (by `until`).
    pub async fn request_body(&self, k: usize, until: Instant) -> Option<Value> {
        self.wait_for(until, |s| s.seen.get(k).map(|r| r.body.clone())).await
    }

    /// When request `k`'s client went, once it has (by `until`).
    pub async fn gone(&self, k: usize, until: Instant) -> Option<Instant> {
        self.wait_for(until, |s| s.seen.get(k).and_then(|r| r.gone)).await
    }

    /// Serves one connection: its requests one after another while answers keep it open.
    async fn serve(&self, tcp: tokio::net::TcpStream) {
        let c = {
            let mut s = self.lock();
            s.conns.push(ConnRec::default());
            s.conns.len() - 1
        };
        let (mut rd, mut wr) = tcp.into_split();
        // Everything the client sends is kept for the requests to take; the end of it is when the
        // client went.
        let reader = {
            let ep = self.clone();
            tokio::spawn(async move {
                let mut b = [0u8; 4096];
                loop {
                    match rd.read(&mut b).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            ep.lock().conns[c].buf.extend_from_slice(&b[..n]);
                            ep.changed.notify_waiters();
                        }
                    }
                }
                let now = Instant::now();
                {
                    let mut s = ep.lock();
                    s.conns[c].closed = Some(now);
                    for k in s.conns[c].requests.clone() {
                        s.seen[k].gone.get_or_insert(now);
                    }
                }
                ep.changed.notify_waiters();
            })
        };
        let far = Instant::now() + Duration::from_secs(3600);
        while let Some(req) = self.next_request(c, far).await {
            let keep = self.lock().keep_alive;
            match self.answer(c, req, &mut wr, !keep).await {
                // An answer given in full on a connection kept open: the next request.
                Some(true) if keep => continue,
                Some(true) => {
                    // Wait for the client to close its end before ours goes, so it reads it all.
                    let _gone = self
                        .wait_for(Instant::now() + Duration::from_secs(10), |s| s.conns[c].closed)
                        .await;
                    break;
                }
                _ => break,
            }
        }
        reader.abort();
        drop(wr);
        // Closed from this end, if the client had not gone first.
        let now = Instant::now();
        {
            let mut s = self.lock();
            s.conns[c].closed.get_or_insert(now);
            for k in s.conns[c].requests.clone() {
                s.seen[k].gone.get_or_insert(now);
            }
        }
        self.changed.notify_waiters();
    }

    /// Waits for the connection's next whole request, and takes its bytes: its head and body
    /// (`None` when the connection ends or is told to close first).
    async fn next_request(&self, c: usize, until: Instant) -> Option<(String, Vec<u8>)> {
        self.lock().conns[c].idle = true;
        let got = self
            .wait_for(until, |s| {
                let conn = &s.conns[c];
                if conn.kill {
                    return Some(None);
                }
                if let Some(r) = whole_request(&conn.buf) {
                    return Some(Some(r));
                }
                if conn.closed.is_some() || conn.buf.len() > 65_536 {
                    return Some(None);
                }
                None
            })
            .await
            .flatten()?;
        let (head, head_end, length) = got;
        let mut s = self.lock();
        let conn = &mut s.conns[c];
        conn.idle = false;
        let bytes: Vec<u8> = conn.buf.drain(..head_end + length).collect();
        Some((head, bytes[head_end..].to_vec()))
    }

    /// Answers one request; `Some(true)` when the answer went in full (the connection can carry
    /// another), `Some(false)` when it ended the connection, `None` when the client went.
    async fn answer(
        &self,
        c: usize,
        (head, body): (String, Vec<u8>),
        wr: &mut tokio::net::tcp::OwnedWriteHalf,
        close: bool,
    ) -> Option<bool> {
        let line = head.lines().next().unwrap_or_default().to_owned();
        if line.starts_with("GET ") && line.contains("/models") {
            let data: Vec<Value> = self
                .models
                .get()
                .iter()
                .map(|m| {
                    serde_json::json!({"id": m.key, "object": "model", "type": "llm",
                        "state": if m.loaded { "loaded" } else { "not-loaded" },
                        "max_context_length": m.context})
                })
                .collect();
            let body = serde_json::json!({"object": "list", "data": data}).to_string();
            return write_whole(wr, 200, "application/json", &body, close)
                .await
                .ok()
                .map(|()| true);
        }
        if !(line.starts_with("POST ") && line.contains("/chat/completions")) {
            return write_whole(wr, 404, "application/json", "{}", close).await.ok().map(|()| true);
        }
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let k = {
            let mut s = self.lock();
            let gone = s.conns[c].closed;
            s.seen.push(Seen { head, body, at: Instant::now(), conn: c, tokens: Vec::new(), gone });
            let k = s.seen.len() - 1;
            s.conns[c].requests.push(k);
            k
        };
        self.changed.notify_waiters();
        let far = Instant::now() + Duration::from_secs(3600);
        let behaviour = self
            .wait_for(far, |s| {
                if s.seen[k].gone.is_some() {
                    return Some(None);
                }
                s.behaviours.get(k).cloned().map(Some)
            })
            .await
            .flatten()?;
        match behaviour {
            Behaviour::Json(v) => {
                write_whole(wr, 200, "application/json", &v.to_string(), close).await.ok()?;
            }
            Behaviour::Status(status, body) => {
                write_whole(wr, status, "text/plain", &body, close).await.ok()?;
            }
            Behaviour::Hang => {
                let _gone = self.gone(k, far).await;
                return None;
            }
            Behaviour::Close => return Some(false),
            Behaviour::Stream { events, delay, first } => {
                // The model, when there is one, is held from the head to the last event.
                let (model, runs_on) = {
                    let s = self.lock();
                    (s.one_model.clone(), s.runs_on)
                };
                let _model = match model {
                    Some(m) => Some(m.lock_owned().await),
                    None => None,
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n{}\r\n",
                    if close { "connection: close\r\n" } else { "" }
                );
                wr.write_all(head.as_bytes()).await.ok()?;
                let datas: Vec<String> = events
                    .iter()
                    .map(Value::to_string)
                    .chain(std::iter::once("[DONE]".to_owned()))
                    .collect();
                let last = datas.len() - 1;
                // A model that runs on after its client went is busy for the events left.
                let run_on = |left: usize| async move {
                    if runs_on {
                        let left = u32::try_from(left).unwrap_or(u32::MAX);
                        tokio::time::sleep(delay.saturating_mul(left)).await;
                    }
                };
                for (n, data) in datas.into_iter().enumerate() {
                    let wait = if n == 0 { first } else { delay };
                    if !wait.is_zero() {
                        // A wait the client's going ends at once, as a model stops.
                        let until = Instant::now() + wait;
                        if self.gone(k, until).await.is_some() {
                            run_on(last - n).await;
                            return None;
                        }
                    }
                    let gone = {
                        // The token's time is taken while the client is known to be there.
                        let mut s = self.lock();
                        let gone = s.seen[k].gone.is_some();
                        if !gone && n < last {
                            s.seen[k].tokens.push(Instant::now());
                        }
                        gone
                    };
                    if gone {
                        run_on(last - n).await;
                        return None;
                    }
                    let event = format!("data: {data}\n\n");
                    let framed = format!("{:x}\r\n{event}\r\n", event.len());
                    if wr.write_all(framed.as_bytes()).await.is_err() {
                        run_on(last - n).await;
                        return None;
                    }
                }
                wr.write_all(b"0\r\n\r\n").await.ok()?;
            }
        }
        wr.flush().await.ok()?;
        Some(true)
    }
}

/// A whole response with a body, closing the connection after it when `close` says so.
async fn write_whole<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &str,
    close: bool,
) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n{}\r\n{body}",
        body.len(),
        if close { "connection: close\r\n" } else { "" }
    );
    w.write_all(resp.as_bytes()).await?;
    w.flush().await
}

// ------------------------------------------------------------------ the fake job host

/// The fake job host's shared state: the host started last, its runs, its pause.
#[derive(Default)]
pub struct FakeCtl {
    events: Option<mpsc::UnboundedSender<citar_helper::jobs::HostEvent>>,
    /// The runs it plays, oldest first.
    runs: Vec<String>,
    hung: std::collections::BTreeSet<String>,
    paused_since: Option<Instant>,
    paused_s: f64,
    /// The hosts started.
    pub starts: u32,
    /// The pauses and resumes it was told, in order.
    pub pauses: Vec<bool>,
}

impl FakeCtl {
    fn paused_total(&self) -> f64 {
        self.paused_s + self.paused_since.map_or(0.0, |t| t.elapsed().as_secs_f64())
    }
}

/// The fake job host (`setup.helper.host`): `verifiable` false fails its self-test, `start`
/// `"limits"` cannot start under the limits.
#[derive(Clone)]
pub struct FakeHosts {
    pub verifiable: bool,
    pub start_fails: bool,
    pub ctl: Arc<Mutex<FakeCtl>>,
}

impl FakeHosts {
    /// A fake host as `setup.helper.host` describes it (`None`: the default, which works).
    pub fn from_setup(setup: Option<&Value>) -> Result<FakeHosts, String> {
        let setup = setup.and_then(Value::as_object).cloned().unwrap_or_default();
        let start_fails = match setup.get("start").and_then(Value::as_str) {
            None => false,
            Some("limits") => true,
            Some(other) => return Err(format!("setup.helper.host.start {other:?} is not known")),
        };
        Ok(FakeHosts {
            verifiable: setup.get("verifiable").and_then(Value::as_bool).unwrap_or(true),
            start_fails,
            ctl: Arc::default(),
        })
    }

    fn send(&self, ev: citar_helper::jobs::HostEvent) -> Result<(), String> {
        let ctl = self.ctl.lock().expect("the fake host");
        let tx = ctl.events.as_ref().ok_or("no fake job host runs")?;
        tx.send(ev).map_err(|_| "the fake job host's helper is gone".to_owned())
    }

    fn current(&self) -> Result<String, String> {
        let ctl = self.ctl.lock().expect("the fake host");
        ctl.runs.last().cloned().ok_or_else(|| "the fake job host plays no run".to_owned())
    }

    /// A `host` step.
    pub fn step(&self, step: &Map<String, Value>, golden: &Committed) -> Result<(), String> {
        use citar_helper::jobs::HostEvent;
        use citar_jobs::host::{FailCode, Failed, FromHost, Outcome, ProgressFrame, ResultFrame};
        if let Some(p) = step.get("progress") {
            let run = self.current()?;
            let round = p.get("round").and_then(Value::as_i64).unwrap_or(1);
            let (paused_s, paused) = {
                let ctl = self.ctl.lock().expect("the fake host");
                (ctl.paused_total(), ctl.paused_since.is_some())
            };
            self.send(HostEvent::Frame(FromHost::JobProgress(ProgressFrame {
                run,
                round,
                of: 30,
                elapsed_s: 1.5,
                cpu_s: 1.25,
                paused_s,
                peak_mb: 12.5,
                paused,
            })))?;
        }
        if let Some(f) = step.get("finish") {
            let name = f
                .as_str()
                .and_then(|s| s.strip_prefix("<golden:jobs/"))
                .and_then(|s| s.strip_suffix('>'))
                .ok_or_else(|| format!("finish {f} is no golden job"))?;
            let job = golden
                .value("jobs", &[name.to_owned()])
                .ok_or_else(|| format!("no golden job {name}"))?;
            let run = self.current()?;
            let paused_s = self.ctl.lock().expect("the fake host").paused_total();
            let digests: Vec<String> = serde_json::from_value(job["digests"].clone())
                .map_err(|e| format!("the golden digests: {e}"))?;
            let outcome = Outcome {
                result: job["result"].clone(),
                result_hash: job["result_hash"].as_str().unwrap_or_default().to_owned(),
                chain: citar_jobs::Chain {
                    head: job["chain_head"].as_str().unwrap_or_default().to_owned(),
                    rounds: u32::try_from(job["rounds"].as_u64().unwrap_or(0)).unwrap_or(0),
                    digests,
                },
                cpu_s: 0.5,
                wall_s: 0.75 + paused_s,
                paused_s,
                peak_mb: 12.5,
            };
            self.ctl.lock().expect("the fake host").runs.retain(|r| *r != run);
            self.send(HostEvent::Frame(FromHost::JobResult(Box::new(ResultFrame {
                run,
                outcome,
            }))))?;
        }
        if let Some(c) = step.get("crash_host") {
            let memory = match c.as_str() {
                Some("memory") => true,
                Some("other") => false,
                _ => return Err(format!("crash_host {c} is not known")),
            };
            let (runs, tx) = {
                let mut ctl = self.ctl.lock().expect("the fake host");
                (std::mem::take(&mut ctl.runs), ctl.events.take())
            };
            let death = if memory {
                "the memory limit ended the job host: an allocation failed (the fake host)"
            } else {
                "the job host ended: exit code 101 (the fake host)"
            };
            let failures = runs
                .into_iter()
                .map(|run| {
                    let f = Failed {
                        run,
                        code: if memory { FailCode::Memory } else { FailCode::Host },
                        message: death.to_owned(),
                        retryable: true,
                    };
                    (f, Some(12.5))
                })
                .collect();
            let tx = tx.ok_or("no fake job host runs")?;
            tx.send(HostEvent::Died { death: death.to_owned(), failures })
                .map_err(|_| "the fake job host's helper is gone".to_owned())?;
        }
        if step.get("hang").and_then(Value::as_bool) == Some(true) {
            let run = self.current()?;
            self.ctl.lock().expect("the fake host").hung.insert(run);
        }
        Ok(())
    }
}

impl citar_helper::jobs::Hosts for FakeHosts {
    fn self_test(&self) -> BoxFuture<'static, Result<(), String>> {
        let ok = self.verifiable;
        Box::pin(async move {
            if ok { Ok(()) } else { Err("the fake job host's canary failed (setup)".to_owned()) }
        })
    }

    fn start(
        &self,
        limits: citar_jobs::limits::Requested,
        jobs_build: String,
    ) -> BoxFuture<'static, Result<citar_helper::jobs::HostStart, citar_helper::jobs::StartFailure>>
    {
        use citar_helper::jobs::{HostEvent, HostStart, StartFailure};
        use citar_jobs::host::{FromHost, Ready, RunRef, ToHost};
        let fails = self.start_fails;
        let verifiable = self.verifiable;
        let ctl = Arc::clone(&self.ctl);
        Box::pin(async move {
            if fails {
                return Err(StartFailure {
                    limits: true,
                    build: false,
                    message: "the job host refused to start (Limits): the fake host holds no \
                              limits (setup)"
                        .to_owned(),
                });
            }
            let (cmds_tx, mut cmds_rx) = mpsc::unbounded_channel::<ToHost>();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            {
                let mut c = ctl.lock().expect("the fake host");
                c.events = Some(events_tx.clone());
                c.runs.clear();
                c.starts += 1;
            }
            let held = Arc::clone(&ctl);
            tokio::spawn(async move {
                while let Some(cmd) = cmds_rx.recv().await {
                    let mut c = held.lock().expect("the fake host");
                    match cmd {
                        ToHost::JobOffer(o) => {
                            c.runs.push(o.run.clone());
                            let _sent = events_tx
                                .send(HostEvent::Frame(FromHost::JobAccept(RunRef { run: o.run })));
                        }
                        ToHost::JobAbort(a) => {
                            if !c.hung.contains(&a.run) && c.runs.contains(&a.run) {
                                c.runs.retain(|r| *r != a.run);
                                let _sent = events_tx.send(HostEvent::Frame(FromHost::JobAborted(
                                    RunRef { run: a.run },
                                )));
                            }
                        }
                        ToHost::Pause => {
                            c.pauses.push(true);
                            if c.paused_since.is_none() {
                                c.paused_since = Some(Instant::now());
                            }
                        }
                        ToHost::Resume => {
                            c.pauses.push(false);
                            if let Some(t) = c.paused_since.take() {
                                c.paused_s += t.elapsed().as_secs_f64();
                            }
                        }
                        ToHost::Limits(_) => {}
                    }
                }
            });
            let threads = limits.threads.max(1);
            let ready = Ready {
                limits: citar_jobs::Held {
                    cpu: citar_jobs::Hardness::Hard,
                    memory: citar_jobs::Hardness::Hard,
                },
                verifiable,
                jobs_build,
                threads,
                reason: None,
                memory_per_run_mb: limits.memory_mb * 9 / 10 / u64::from(threads),
                own_mb: 5.0,
                cgroup: None,
                cpu_s: 0.0,
                pid: 1,
                counting: true,
                notes: Vec::new(),
            };
            Ok(HostStart { ready, cmds: cmds_tx, events: events_rx })
        })
    }
}

/// A result file as a previous run of the helper left it in `helper/results/` (`setup.helper.home`):
/// the golden `duel-a`'s outcome for `run`.
pub fn held_result(run: &str, golden: &Committed, values: &Map<String, Value>) -> Value {
    let job = golden.value("jobs", &["duel-a".to_owned()]).expect("the golden duel-a");
    let v = |k: &str, d: &str| values.get(k).and_then(Value::as_str).unwrap_or(d).to_owned();
    serde_json::json!({
        "run": run,
        "outcome": {
            "result": job["result"], "result_hash": job["result_hash"],
            "chain": {"head": job["chain_head"], "rounds": job["rounds"], "digests": job["digests"]},
            "cpu_s": 0.5, "wall_s": 0.75, "paused_s": 0.0, "peak_mb": 12.5
        },
        "claim": {
            "v": 1, "job": v("job", "cj_0123456789abcdef"), "run": run,
            "machine": v("machine", "sv_0123456789abcdef"), "kind": "lab_game",
            "spec_hash": job["spec_hash"], "jobs_build": v("jobs_build", "0f1e2d3c4b5a"),
            "bots": job["bots"], "result_hash": job["result_hash"], "chain_head": job["chain_head"],
            "rounds": job["rounds"], "key": v("key", "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")
        },
        "signature": "A".repeat(86) + "=="
    })
}
