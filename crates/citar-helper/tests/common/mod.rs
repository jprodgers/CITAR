//! The transcript player of the helper's side (PROTOCOL.md "Transcripts", P4.2.9): a fake server
//! that sends the transcript's server frames, checks every frame the helper sends against the
//! transcript's patterns, and plays the fake model endpoint. It drives either the helper's own
//! session in-process (`tests/conformance.rs`, on tokio's paused clock: the fake clock) or the
//! real binary over loopback (`tests/real_binary.rs`, in real time).
//!
//! The rules it keeps: background frames (`ping`, `pong`, `status`, `power`) are skipped unless
//! the next step expects their type, and the helper's own unscripted pings are answered; a
//! `quiet` wait allows only background frames (not of the next step's type) and no new
//! connection; any other frame a wait sees is kept for the steps after it; `within_s` is
//! measured from the end of the previous step. A step this player cannot do yet (a later
//! package's endpoint behaviour, the job host, held results) is refused, never skipped.

#![allow(dead_code, clippy::missing_panics_doc)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use citar_helper::conformance::matcher::{Bindings, NoGolden, matches, realize};
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
    /// Something happens around the helper (an `act` with side `helper`).
    fn act(&mut self, what: &Value);
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
    last: Instant,
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
            last: Instant::now(),
        }
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
        let until = self.last + s.within_s.map_or(DEFAULT_WAIT, Duration::from_secs_f64);
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
            match matches(pattern, &v, &mut self.bindings, &NoGolden) {
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
        let frame =
            realize(pattern, &mut self.bindings, &self.t.values, &NoGolden, example.as_ref())
                .map_err(|e| self.fail(i, e))?;
        if s.invalid.is_none() {
            parse_server_frame(&frame.to_string()).map_err(|e| {
                self.fail(i, format!("the realized frame fails its checks: {e}: {frame}"))
            })?;
        }
        if s.lost {
            return Ok(());
        }
        self.ensure_conn(i).await?;
        self.send_text(frame.to_string()).await.map_err(|e| self.fail(i, e))
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
        let until = self.last + s.within_s.map_or(DEFAULT_WAIT, Duration::from_secs_f64);
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
                    let until =
                        self.last + s.within_s.map_or(DEFAULT_WAIT, Duration::from_secs_f64);
                    let c = self.next_conn(until).await;
                    self.cur = Some(c.ok_or_else(|| {
                        self.fail(i, "the helper opened no new connection in time")
                    })?);
                    self.inbox.clear();
                }
                Action::Wait { s: secs, quiet } => self.wait(i, *secs, *quiet).await?,
                Action::Endpoint(e) => {
                    let keys: Vec<&String> = e.keys().filter(|k| *k != "models").collect();
                    if !keys.is_empty() {
                        return Err(self.fail(
                            i,
                            format!("this player cannot yet play endpoint {keys:?} (4-04)"),
                        ));
                    }
                    self.models.set(listed(&e["models"]));
                }
                Action::Host(_) => {
                    return Err(self.fail(i, "this player has no job host yet (4-10)"));
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
                    let within = s.within_s.map_or(DEFAULT_WAIT, Duration::from_secs_f64);
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
                other => {
                    return Err(self
                        .fail(i, format!("this player cannot yet check the helper's {other:?}")));
                }
            }
        }
        Ok(())
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
