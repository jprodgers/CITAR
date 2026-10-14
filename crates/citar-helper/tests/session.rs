//! The session's own rules beyond the shared transcripts (design/phase4-helper.md P4.3.3, P4.6,
//! P4.7.4), driven in-process against a fake server written here:
//! - a hostile server's invented frame types, values and sizes leave the log bounded: a few
//!   hundred keys at most, no line over a few KB, the connection still answering;
//! - a listing that fails once is not news (no `catalog`); three in a row report no model; the
//!   next answer reports the models again; and a reconnection whose listing fails keeps the last
//!   good list in its hello;
//! - quiet hours that cover the whole week send no `status` every second, once a week at most;
//! - a stop while the endpoint does not answer ends the helper at once;
//! - a `rescan` whose hardware file never answers holds up neither pings nor the session, and gives
//!   up after its timeout (in real time, with a pipe that is never written).
//!
//! Most run on tokio's paused clock: minutes take no time.

mod common;

use std::sync::Arc;
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::conformance::canonical_examples;
use citar_helper::key::MachineKey;
use citar_helper::llm::{Listed, SharedEndpoint};
use citar_helper::log::{Log, MAX_KEYS};
use citar_helper::protocol::{Provider, Sender, UpdatePolicy};
use citar_helper::quiet;
use citar_helper::session::{Exit, Facts, Helper};
use common::{MODELS_FAIL, Models, Pipe, ServerWs};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// The fake clock's start: Wednesday 2026-10-14, 12:00 UTC.
const START_UNIX: f64 = 1_791_979_200.0;

fn config() -> Config {
    Config {
        server_url: "http://127.0.0.1:1".to_owned(),
        token: "cwt_session_test_0123456789".to_owned(),
        base_url: "http://127.0.0.1:2/v1".to_owned(),
        provider: Provider::LmStudio,
        api_key_env: String::new(),
        max_concurrent: 1,
        name: String::new(),
        quiet: Vec::new(),
        models: None,
        insecure: false,
        collect_hardware: true,
        hardware_file: String::new(),
        no_llm: true,
        cpu: LocalCpu::default(),
        updates: UpdatePolicy::Auto,
        stream: true,
        ca_file: None,
        no_power: false,
        warnings: Vec::new(),
    }
}

fn facts() -> Facts {
    let info = citar_helper::version::build_info();
    Facts {
        hostname: "session-test".to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        build: info.build,
        version: info.version.to_owned(),
        target: info.target.to_owned(),
        label: info.label.to_owned(),
        bots: vec!["basic".to_owned()],
        ruleset: "0".repeat(64),
    }
}

/// A helper running in-process, and the fake server's side of it.
struct Run {
    conns: mpsc::UnboundedReceiver<ServerWs>,
    log: Log,
    clock: Clock,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Exit>,
}

fn start(cfg: Config, endpoint: Option<SharedEndpoint>, clock: Clock) -> Run {
    let (tx, conns) = mpsc::unbounded_channel();
    let key = MachineKey::generate().expect("a key");
    let log = Log::memory();
    let (stop, stop_rx) = watch::channel(false);
    let ceiling = cfg.ceiling(Machine { logical_cpus: 8, memory_mb: 16_384 });
    let helper = Helper {
        config: cfg,
        public_key: key.public_b64(),
        facts: facts(),
        ceiling,
        log: log.clone(),
        clock: clock.clone(),
        dialer: Arc::new(Pipe(tx)),
        endpoint,
        service: false,
        stop: stop_rx,
    };
    let task = tokio::spawn(async move { helper.run().await });
    Run { conns, log, clock, stop, task }
}

fn text(m: &Message) -> Option<Value> {
    match m {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

/// The next connection, its hello, a welcome sent and the acknowledgement read.
async fn welcome(run: &mut Run) -> (ServerWs, Value) {
    let mut ws = tokio::time::timeout(Duration::from_secs(60), run.conns.recv())
        .await
        .expect("a connection in time")
        .expect("a connection");
    let hello = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    assert_eq!(hello["t"], "hello", "{hello}");
    let canonical = canonical_examples().expect("examples");
    let mut w = canonical[&(Sender::Server, "welcome".to_owned())].clone();
    w["jobs"] = json!({"keep": [], "drop": [], "resend": []});
    ws.send(Message::Text(w.to_string().into())).await.expect("welcome sent");
    let ack = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    assert_eq!(ack["t"], "settings_ack", "{ack}");
    (ws, hello)
}

/// Every frame the helper sends for `secs` (answering its pings), with when it came.
async fn frames_for(ws: &mut ServerWs, secs: f64) -> Vec<(f64, Value)> {
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(secs);
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout_at(until, ws.next()).await {
            Err(_) => break,
            Ok(Some(Ok(m))) => {
                if let Some(v) = text(&m) {
                    if v["t"] == "ping" {
                        ws.send(Message::Text(r#"{"t": "pong"}"#.into())).await.expect("pong");
                    }
                    out.push((t0.elapsed().as_secs_f64(), v));
                }
            }
            Ok(_) => panic!("the helper ended the connection"),
        }
    }
    out
}

/// A ping, and the helper's pong: everything sent before it has been read.
async fn round_trip(ws: &mut ServerWs) {
    ws.send(Message::Text(r#"{"t": "ping"}"#.into())).await.expect("ping");
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let m = tokio::time::timeout_at(until, ws.next())
            .await
            .expect("a pong in time")
            .expect("open")
            .expect("a frame");
        if text(&m).is_some_and(|v| v["t"] == "pong") {
            return;
        }
    }
}

fn paused() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a runtime")
}

#[test]
fn a_hostile_servers_types_values_and_sizes_leave_the_log_bounded() {
    paused().block_on(async {
        let mut run = start(config(), None, Clock::fake(START_UNIX));
        let (mut ws, _) = welcome(&mut run).await;
        // A thousand types nobody knows, each 8 KB long and each different, and ten of 1 MB.
        for i in 0..1000 {
            let f = json!({"t": format!("zz{i:04}{}", "z".repeat(8 * 1024))});
            ws.send(Message::Text(f.to_string().into())).await.expect("sent");
        }
        for i in 0..10 {
            let f = json!({"t": format!("big{i}{}", "b".repeat(1024 * 1024))});
            ws.send(Message::Text(f.to_string().into())).await.expect("sent");
        }
        // A frame over the 8 MiB cap (the socket takes up to 16): its type is never read.
        let huge = json!({"t": format!("yy{}", "y".repeat(9_000_000))});
        ws.send(Message::Text(huge.to_string().into())).await.expect("sent");
        // A known type whose value is a megabyte the checks quote, and whose unknown field's name
        // is another (a shutdown with a bad code is a bad frame, not a shutdown).
        let bad = json!({"t": "shutdown", "reason": "r", "code": "c".repeat(1024 * 1024)});
        ws.send(Message::Text(bad.to_string().into())).await.expect("sent");
        let unknown = json!({"t": "ping", "u".repeat(1024 * 1024): 1});
        ws.send(Message::Text(unknown.to_string().into())).await.expect("sent");
        round_trip(&mut ws).await;

        assert!(run.log.keys() <= 2 * (MAX_KEYS + 1), "{} keys", run.log.keys());
        let lines = run.log.lines();
        let longest = lines.iter().map(String::len).max().unwrap_or(0);
        assert!(longest <= 2048, "a line of {longest} bytes");
        let bytes: usize = lines.iter().map(String::len).sum();
        assert!(bytes < 256 * 1024, "{bytes} bytes of lines");
        assert!(run.log.contains("a bad frame from the server: too_large"), "{lines:#?}");
        assert!(run.log.contains("a bad shutdown frame from the server: value at code"));
        assert!(!run.task.is_finished(), "still running");
        let _stop = run.stop.send(true);
        assert_eq!(run.task.await.expect("joined"), Exit::Stopped);
    });
}

fn model_a() -> Vec<Listed> {
    vec![Listed {
        key: "model-a".into(),
        label: "Model A".into(),
        context: Some(8192),
        loaded: true,
    }]
}

fn catalogs(frames: &[(f64, Value)]) -> Vec<(f64, Vec<String>)> {
    frames
        .iter()
        .filter(|(_, v)| v["t"] == "catalog")
        .map(|(at, v)| {
            let keys = v["models"]
                .as_array()
                .expect("models")
                .iter()
                .map(|m| m["key"].as_str().expect("a key").to_owned())
                .collect();
            (*at, keys)
        })
        .collect()
}

#[test]
fn a_failed_listing_is_not_news_and_three_in_a_row_are() {
    paused().block_on(async {
        let models = Models::default();
        models.set(model_a());
        let mut cfg = config();
        cfg.no_llm = false;
        let mut run = start(cfg, Some(Arc::new(models.clone())), Clock::fake(START_UNIX));
        let (mut ws, hello) = welcome(&mut run).await;
        assert_eq!(hello["llm"]["models"][0]["key"], "model-a");

        // One listing fails (the poll at 30 s), the next answers as before: nothing is sent.
        models.fail_next(1);
        let frames = frames_for(&mut ws, 65.0).await;
        assert_eq!(catalogs(&frames), [], "one failure is not a change");
        assert!(run.log.contains(MODELS_FAIL), "the reason is logged");

        // Three in a row (the polls at 30, 60 and 90 s from here): the third reports none, and
        // the answer after them (120 s) reports the model again.
        models.fail_next(3);
        let frames = frames_for(&mut ws, 125.0).await;
        let got = catalogs(&frames);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].1.is_empty() && (84.0..96.0).contains(&got[0].0), "{got:?}");
        assert_eq!(got[1].1, ["model-a"]);
        assert!((114.0..126.0).contains(&got[1].0), "{got:?}");

        // A new connection whose own listing fails says what was last known, not nothing.
        models.fail_next(1);
        drop(ws);
        let (mut ws, hello) = welcome(&mut run).await;
        assert_eq!(hello["llm"]["models"][0]["key"], "model-a", "{hello}");
        let frames = frames_for(&mut ws, 35.0).await;
        assert_eq!(catalogs(&frames), []);
        let _stop = run.stop.send(true);
        assert_eq!(run.task.await.expect("joined"), Exit::Stopped);
    });
}

#[test]
fn quiet_hours_all_week_are_announced_once_a_week_not_every_second() {
    paused().block_on(async {
        let mut cfg = config();
        cfg.quiet = quiet::parse(&[json!("00:00-00:00")]).expect("quiet");
        let mut run = start(cfg, None, Clock::fake(START_UNIX));
        let (mut ws, hello) = welcome(&mut run).await;
        // Wednesday 2026-10-14: the start of the week after next is Monday 2026-10-26, 00:00.
        assert_eq!(hello["quiet_until"], json!(1_792_972_800.0), "{hello}");
        let frames = frames_for(&mut ws, 60.0).await;
        let statuses = || frames.iter().filter(|(_, v)| v["t"] == "status").count();
        assert_eq!(statuses(), 0, "the hello said it, and it has not changed");
        // Six days on the week has turned: one status, with the next anchor, then none.
        run.clock.jump_to(START_UNIX + 6.0 * 86_400.0);
        let frames = frames_for(&mut ws, 60.0).await;
        let sent: Vec<&Value> =
            frames.iter().filter(|(_, v)| v["t"] == "status").map(|(_, v)| v).collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["quiet_until"], json!(1_793_577_600.0));
        let lines = run.log.lines().iter().filter(|l| l.contains("local quiet hours")).count();
        assert_eq!(lines, 1);
        let _stop = run.stop.send(true);
        assert_eq!(run.task.await.expect("joined"), Exit::Stopped);
    });
}

/// An endpoint that never answers.
struct Silent;

impl citar_helper::llm::Endpoint for Silent {
    fn list(&self) -> citar_helper::llm::BoxFuture<'_, Result<Vec<Listed>, String>> {
        Box::pin(std::future::pending())
    }
}

#[test]
fn a_stop_while_the_endpoint_does_not_answer_ends_the_helper_at_once() {
    paused().block_on(async {
        let mut cfg = config();
        cfg.no_llm = false;
        let run = start(cfg, Some(Arc::new(Silent)), Clock::fake(START_UNIX));
        tokio::time::sleep(Duration::from_secs(1)).await;
        let t0 = Instant::now();
        let _stop = run.stop.send(true);
        assert_eq!(run.task.await.expect("joined"), Exit::Stopped);
        // Not after the listing's 10 s timeout.
        assert!(t0.elapsed() < Duration::from_millis(100), "{:?}", t0.elapsed());
    });
}

/// A hardware "file" that can be opened and never answers: a named pipe on Windows (two
/// instances, for the hello's read and the rescan's), a FIFO elsewhere. Dropping it lets the
/// blocked reads end.
#[cfg(windows)]
struct Never {
    path: String,
    _pipes: Vec<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
fn never(_dir: &std::path::Path) -> Never {
    use tokio::net::windows::named_pipe::ServerOptions;
    let path = format!(r"\\.\pipe\citar-helper-never-{}", std::process::id());
    let first = ServerOptions::new().first_pipe_instance(true).create(&path).expect("a pipe");
    let second = ServerOptions::new().create(&path).expect("a second instance");
    Never { path, _pipes: vec![first, second] }
}

#[cfg(unix)]
struct Never {
    path: String,
}

#[cfg(unix)]
fn never(dir: &std::path::Path) -> Never {
    let path = dir.join("never.fifo");
    let made = std::process::Command::new("mkfifo").arg(&path).status().expect("mkfifo runs");
    assert!(made.success());
    Never { path: path.display().to_string() }
}

#[cfg(unix)]
impl Drop for Never {
    fn drop(&mut self) {
        // A writer that opens and closes ends every blocked open with end of file. On a thread of
        // its own, so that nothing waits on it if no reader is left.
        let path = self.path.clone();
        std::thread::spawn(move || drop(std::fs::OpenOptions::new().write(true).open(path)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_whose_file_never_answers_holds_up_nothing() {
    let dir = common::scratch("never");
    let pipe = never(&dir);
    let mut cfg = config();
    cfg.hardware_file = pipe.path.clone();
    let t0 = Instant::now();
    let mut run = start(cfg, None, Clock::system());
    // The hello waits for the file at most its 5 s, then reports none.
    let (mut ws, hello) = welcome(&mut run).await;
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_secs(4) && waited < Duration::from_secs(9), "{waited:?}");
    assert_eq!(hello["report"]["hardware_source"], "none", "{hello}");
    // A rescan reads the file again: the session answers a ping at once all the same.
    ws.send(Message::Text(r#"{"t": "rescan"}"#.into())).await.expect("rescan");
    let t1 = Instant::now();
    round_trip(&mut ws).await;
    assert!(t1.elapsed() < Duration::from_secs(1), "{:?}", t1.elapsed());
    // And gives up after its timeout, sending no hardware.
    let frames = frames_for(&mut ws, 7.0).await;
    assert!(frames.iter().all(|(_, v)| v["t"] != "hardware"), "{frames:?}");
    assert!(run.log.contains("could not be read within 5 s"), "{:#?}", run.log.lines());
    let _stop = run.stop.send(true);
    assert_eq!(run.task.await.expect("joined"), Exit::Stopped);
    drop(pipe);
    let _cleaned = std::fs::remove_dir_all(&dir);
}
