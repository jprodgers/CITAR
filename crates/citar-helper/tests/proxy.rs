//! The LLM proxy beyond the shared transcripts (design/phase4-helper.md P4.3.4; package 4-04,
//! gates 2 and 3), the helper's own session in-process against a fake server written here and
//! `common`'s fake endpoint over real HTTP on loopback, in real time:
//! - **cancel at the endpoint:** the endpoint's client goes within 1 s of the `cancel`, no event
//!   goes out after it, and `cancelled` carries the tokens received, mid-stream and while a model
//!   is thinking before its first token (headers sent, then silence: where a pooled client would
//!   notice nothing until the next byte);
//! - **parameters:** only the five reach the endpoint, each other name logged once; nothing in a
//!   request changes the endpoint, its key or streaming;
//! - an endpoint that refuses streaming with tools is asked again unstreamed, and is then asked so
//!   for tools from the start; one that answers JSON to a streaming request is read as unstreamed;
//! - **errors:** an endpoint that is not there, HTTP errors (retryable by their status, their body
//!   quoted to 300 characters), an error inside the stream, and an answer too large for a frame
//!   (as text, or only once escaped in JSON);
//! - **slots:** two completions at once on two slots, a third `busy`; a repeated id answered once;
//! - a stop answers each completion in flight `stopping`, and its endpoint connection closes;
//! - `status` follows the completions in flight; a `timeout_s` above 900 s is held to 900 s.

mod common;

use std::sync::Arc;
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::http1;
use citar_helper::key::MachineKey;
use citar_helper::llm::{BoxFuture, Endpoint, HttpEndpoint, Listed};
use citar_helper::log::Log;
use citar_helper::protocol::{Provider, Request, RequestErrorCode, UpdatePolicy};
use citar_helper::proxy::{self, InFlight, Outcome};
use citar_helper::session::{Exit, Facts, Helper};
use common::{Behaviour, FakeEndpoint, Models, Pipe, ServerWs};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

fn config() -> Config {
    Config {
        server_url: "http://127.0.0.1:1".to_owned(),
        token: "cwt_proxy_test_0123456789".to_owned(),
        base_url: String::new(),
        provider: Provider::LmStudio,
        api_key_env: String::new(),
        max_concurrent: 1,
        name: String::new(),
        quiet: Vec::new(),
        models: None,
        insecure: false,
        collect_hardware: false,
        hardware_file: String::new(),
        no_llm: false,
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
        hostname: "proxy-test".to_owned(),
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

/// A helper in-process, its connection to the fake server, and the fake endpoint.
struct Run {
    ws: ServerWs,
    ep: FakeEndpoint,
    log: Log,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Exit>,
    in_flight: InFlight,
    /// Every `status` the helper sent, in order.
    statuses: Vec<Value>,
}

fn text(m: &Message) -> Option<Value> {
    match m {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

async fn start(tweak: impl FnOnce(&mut Config)) -> Run {
    start_keyed(tweak, None).await
}

/// [`start`], with an API key for the endpoint (a secret of the log's, as `app.rs` makes it).
async fn start_keyed(tweak: impl FnOnce(&mut Config), key: Option<&str>) -> Run {
    let models = Models::default();
    models.set(vec![Listed {
        key: "model-a".into(),
        label: "model-a".into(),
        context: Some(8192),
        loaded: true,
    }]);
    let ep = FakeEndpoint::start(models).await;
    let mut cfg = config();
    cfg.base_url = ep.base_url();
    tweak(&mut cfg);
    let tls = citar_helper::net::verified_tls(None).expect("tls");
    let http = HttpEndpoint::new(cfg.provider, &cfg.base_url, key.map(str::to_owned), tls)
        .expect("a client");
    let (tx, mut conns) = mpsc::unbounded_channel();
    let log = Log::memory();
    if let Some(k) = key {
        log.add_secret(k);
    }
    let key = MachineKey::generate().expect("a key");
    let (stop, stop_rx) = watch::channel(false);
    let in_flight = InFlight::default();
    let slots = cfg.max_concurrent;
    let ceiling = cfg.ceiling(Machine { logical_cpus: 8, memory_mb: 16_384 });
    let helper = Helper {
        config: cfg,
        public_key: key.public_b64(),
        facts: facts(),
        ceiling,
        log: log.clone(),
        clock: Clock::system(),
        dialer: Arc::new(Pipe(tx)),
        endpoint: Some(Arc::new(http)),
        service: false,
        stop: stop_rx,
        in_flight: in_flight.clone(),
    };
    let task = tokio::spawn(async move { helper.run().await });
    let mut ws = tokio::time::timeout(Duration::from_secs(20), conns.recv())
        .await
        .expect("a connection in time")
        .expect("a connection");
    let hello = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    assert_eq!(
        (hello["t"].as_str(), hello["llm"]["enabled"].as_bool()),
        (Some("hello"), Some(true))
    );
    let welcome = json!({
        "t": "welcome", "protocol": 2, "machine": {"id": "sv_proxy", "name": "proxy"},
        "settings": {"llm": {"enabled": true, "slots": slots, "allow_swaps": false},
                     "cpu": {"enabled": false, "threads": 0, "cpu_percent": 0, "memory_mb": 0,
                             "work": [], "yield_to_llm": true}},
        "ping_s": 20, "server": {"version": "0.1.6", "build": "a1b2c3d4e5f6", "jobs_build": null},
        "update": null, "jobs": {"keep": [], "drop": [], "resend": []}
    });
    ws.send(Message::Text(welcome.to_string().into())).await.expect("welcome sent");
    let ack = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    assert_eq!(ack["effective"]["llm"]["enabled"], true, "{ack}");
    Run { ws, ep, log, stop, task, in_flight, statuses: Vec::new() }
}

fn request(id: &str, extra: Value) -> Value {
    let mut r = json!({
        "t": "request", "id": id, "model": "model-a",
        "messages": [{"role": "system", "content": "You play."}, {"role": "user", "content": "Go."}],
        "tools": null, "params": {}, "timeout_s": 60
    });
    for (k, v) in extra.as_object().expect("an object") {
        r[k] = v.clone();
    }
    r
}

fn tool() -> Value {
    json!([{"type": "function", "function": {"name": "end_turn", "description": "End your turn.",
                                             "parameters": {"type": "object", "properties": {}}}}])
}

/// `n` streamed events of one word each.
fn words(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({"choices": [{"index": 0, "delta": {"content": format!("w{i} ")}, "finish_reason": null}]}))
        .collect()
}

impl Run {
    async fn send(&mut self, v: Value) {
        self.ws.send(Message::Text(v.to_string().into())).await.expect("sent");
    }

    /// The helper's next frame that is not a ping, pong or status (statuses are kept), within
    /// `secs`.
    async fn next(&mut self, secs: f64) -> Value {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            let m = tokio::time::timeout_at(until, self.ws.next())
                .await
                .unwrap_or_else(|_| panic!("no frame within {secs} s; log: {:?}", self.log.lines()))
                .expect("the connection is open")
                .expect("a frame");
            let Some(v) = text(&m) else { continue };
            match v["t"].as_str() {
                Some("ping") => self.send(json!({"t": "pong"})).await,
                Some("pong") => {}
                Some("status") => self.statuses.push(v),
                _ => return v,
            }
        }
    }

    /// Nothing but background frames for `secs`.
    async fn quiet(&mut self, secs: f64) {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        while let Ok(m) = tokio::time::timeout_at(until, self.ws.next()).await {
            let v = text(&m.expect("open").expect("a frame")).expect("text");
            match v["t"].as_str() {
                Some("ping") => self.send(json!({"t": "pong"})).await,
                Some("pong") => {}
                Some("status") => self.statuses.push(v),
                _ => panic!("{v} where nothing was due"),
            }
        }
    }

    /// The endpoint's request `k`, once it has come.
    async fn at_endpoint(&self, k: usize) -> Value {
        self.ep
            .request_body(k, Instant::now() + Duration::from_secs(10))
            .await
            .unwrap_or_else(|| panic!("request {k} never reached the endpoint"))
    }

    /// When the endpoint's client `k` went, waiting up to `secs`.
    async fn gone(&self, k: usize, secs: f64) -> Option<Instant> {
        self.ep.gone(k, Instant::now() + Duration::from_secs_f64(secs)).await
    }

    fn end(self) {
        self.task.abort();
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("rt")
}

/// Gate 2 mid-stream: a 50-word answer, a word every 100 ms, cancelled after a second.
#[test]
fn a_cancel_mid_stream_closes_the_endpoint_connection_within_a_second() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        run.ep.respond(Behaviour::Stream {
            events: words(50),
            delay: Duration::from_millis(100),
            first: Duration::ZERO,
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        let cancel_sent = Instant::now();
        run.send(json!({"t": "cancel", "id": "r1"})).await;
        let ack = run.next(1.0).await;
        let acked = cancel_sent.elapsed();
        assert_eq!(ack["t"], "cancelled", "{ack}");
        let gone = run.gone(0, 1.0).await.expect("the client went");
        assert!(
            gone.duration_since(cancel_sent) < Duration::from_secs(1),
            "{:?}",
            gone - cancel_sent
        );
        assert!(acked < Duration::from_secs(1), "{acked:?}");
        let seen = run.ep.seen(0).expect("the request");
        assert!(seen.tokens.iter().all(|t| *t <= gone), "an event went out after the client went");
        let sent = seen.tokens.len() as u64;
        let received = ack["tokens_out"].as_u64().expect("tokens_out");
        assert!((5..50).contains(&sent), "{sent} sent in about a second");
        assert!(received >= 1 && received <= sent && received + 2 >= sent, "{received} of {sent}");
        assert!(ack["gen_s"].as_f64().is_some_and(|g| (0.9..3.0).contains(&g)), "{ack}");
        // Nothing more is sent for it, and its slot is free.
        run.quiet(1.0).await;
        assert_eq!(run.in_flight.get(), 0);
        run.send(request("r2", json!({}))).await;
        run.at_endpoint(1).await;
        run.ep.respond(Behaviour::Stream {
            events: words(2),
            delay: Duration::ZERO,
            first: Duration::ZERO,
        });
        let done = run.next(5.0).await;
        assert_eq!(
            (done["t"].as_str(), done["message"]["content"].as_str()),
            (Some("done"), Some("w0 w1 "))
        );
        run.end();
    });
}

/// Gate 2 before the first token: the endpoint sent its headers and is thinking. The helper's
/// connection is its own, so the cancel closes it at once; a client whose connection lives in a
/// pool's task would see nothing to read and keep it open until the model's first byte.
#[test]
fn a_cancel_while_the_model_thinks_closes_the_connection_too() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({"tools": tool()}))).await;
        run.at_endpoint(0).await;
        run.ep.respond(Behaviour::Stream {
            events: words(3),
            delay: Duration::from_millis(10),
            first: Duration::from_secs(30),
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cancel_sent = Instant::now();
        run.send(json!({"t": "cancel", "id": "r1"})).await;
        let ack = run.next(1.0).await;
        assert_eq!((ack["t"].as_str(), ack["tokens_out"].as_u64()), (Some("cancelled"), Some(0)));
        let gone = run.gone(0, 1.0).await.expect("the client went while the model thought");
        assert!(gone.duration_since(cancel_sent) < Duration::from_secs(1));
        assert!(run.ep.seen(0).expect("seen").tokens.is_empty());
        // And mid-stream, in a long pause between two tokens.
        run.send(request("r2", json!({}))).await;
        run.at_endpoint(1).await;
        run.ep.respond(Behaviour::Stream {
            events: words(3),
            delay: Duration::from_secs(30),
            first: Duration::ZERO,
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cancel_sent = Instant::now();
        run.send(json!({"t": "cancel", "id": "r2"})).await;
        let ack = run.next(1.0).await;
        assert_eq!((ack["t"].as_str(), ack["tokens_out"].as_u64()), (Some("cancelled"), Some(1)));
        let gone = run.gone(1, 1.0).await.expect("the client went in the pause");
        assert!(gone.duration_since(cancel_sent) < Duration::from_secs(1));
        run.end();
    });
}

/// Gate 3: the five parameters reach the endpoint, every other is dropped and logged once by
/// name; the request cannot turn streaming off or point anywhere else.
#[test]
fn only_the_five_parameters_reach_the_endpoint_and_each_other_is_logged_once() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        let params = json!({"temperature": 0.3, "max_tokens": 77, "top_p": 0.5, "seed": 9,
                            "reasoning_effort": "low", "logit_bias": {"1": 5},
                            "base_url": "http://elsewhere.invalid/v1", "stream": false,
                            "api_key": "sk-from-the-server", "model": "other"});
        for (k, id) in ["r1", "r2"].into_iter().enumerate() {
            run.send(request(id, json!({"params": params}))).await;
            let body = run.at_endpoint(k).await;
            assert_eq!(body["model"], "model-a");
            assert_eq!(
                (
                    &body["temperature"],
                    &body["max_tokens"],
                    &body["top_p"],
                    &body["seed"],
                    &body["reasoning_effort"]
                ),
                (&json!(0.3), &json!(77), &json!(0.5), &json!(9), &json!("low"))
            );
            assert_eq!(
                (&body["stream"], &body["stream_options"]),
                (&json!(true), &json!({"include_usage": true}))
            );
            for absent in ["logit_bias", "base_url", "api_key"] {
                assert!(body.get(absent).is_none(), "{absent} reached the endpoint: {body}");
            }
            run.ep.respond(Behaviour::Stream {
                events: words(1),
                delay: Duration::ZERO,
                first: Duration::ZERO,
            });
            assert_eq!(run.next(5.0).await["t"], "done");
        }
        for name in ["logit_bias", "base_url", "stream", "api_key", "model"] {
            let lines: Vec<String> = run
                .log
                .lines()
                .into_iter()
                .filter(|l| l.contains(&format!("parameter \u{201c}{name}\u{201d}")))
                .collect();
            assert_eq!(lines.len(), 1, "{name}: {lines:?}");
        }
        // The endpoint was dialled at its own address: both requests came to the fake.
        assert_eq!(run.ep.requests(), 2);
        run.end();
    });
}

#[test]
fn an_endpoint_that_will_not_stream_tools_is_asked_unstreamed_from_then_on() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({"tools": tool()}))).await;
        assert_eq!(run.at_endpoint(0).await["stream"], true);
        run.ep.respond(Behaviour::Status(400, "Streaming of tool calls is not supported".into()));
        let again = run.at_endpoint(1).await;
        assert_eq!((again["stream"].as_bool(), again.get("stream_options")), (Some(false), None));
        let answer = json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "end_turn", "arguments": "{}"}}]}}]});
        run.ep.respond(Behaviour::Json(answer.clone()));
        let done = run.next(5.0).await;
        assert_eq!((done["t"].as_str(), done["streamed"].as_bool()), (Some("done"), Some(false)));
        assert_eq!(done["message"]["tool_calls"][0]["function"]["name"], "end_turn");
        assert_eq!(done["ttft_s"], Value::Null, "an unstreamed answer has no first-token time");
        assert!(run.log.contains("refuses to stream a request with tools"));
        // The next request with tools goes unstreamed at once; one without still streams.
        run.send(request("r2", json!({"tools": tool()}))).await;
        assert_eq!(run.at_endpoint(2).await["stream"], false);
        run.ep.respond(Behaviour::Json(answer));
        assert_eq!(run.next(5.0).await["t"], "done");
        run.send(request("r3", json!({}))).await;
        assert_eq!(run.at_endpoint(3).await["stream"], true);
        run.ep.respond(Behaviour::Stream { events: words(1), delay: Duration::ZERO, first: Duration::ZERO });
        assert_eq!(run.next(5.0).await["streamed"], true);
        run.end();
    });
}

#[test]
fn json_to_a_streaming_request_is_read_as_unstreamed() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        run.ep.respond(Behaviour::Json(json!({
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "Hi.", "reasoning": "short"}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        })));
        let done = run.next(5.0).await;
        assert_eq!(
            (done["streamed"].as_bool(), done["message"]["content"].as_str(), done["message"]["reasoning"].as_str()),
            (Some(false), Some("Hi."), Some("short"))
        );
        assert_eq!(done["usage"], json!({"prompt_tokens": 3, "completion_tokens": 1, "reasoning_tokens": null, "cached_tokens": null}));
        run.end();
    });
}

#[test]
fn endpoint_failures_are_answered_endpoint_retryable_as_they_suggest() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        let cases: Vec<(Behaviour, bool, &str)> = vec![
            (Behaviour::Status(503, "busy loading".into()), true, "HTTP 503"),
            (Behaviour::Status(429, "slow down".into()), true, "HTTP 429"),
            (Behaviour::Status(400, "x".repeat(1000)), false, "HTTP 400"),
            (
                Behaviour::Stream {
                    events: vec![
                        json!({"choices": [{"index": 0, "delta": {"content": "a"}}]}),
                        json!({"error": {"message": "model crashed"}}),
                    ],
                    delay: Duration::ZERO,
                    first: Duration::ZERO,
                },
                false,
                "model crashed",
            ),
            (Behaviour::Json(json!({"error": "no such model"})), false, "no such model"),
            (Behaviour::Json(json!({"choices": []})), false, "no choice"),
        ];
        for (k, (behaviour, retryable, says)) in cases.into_iter().enumerate() {
            let id = format!("r{k}");
            run.send(request(&id, json!({}))).await;
            run.at_endpoint(k).await;
            run.ep.respond(behaviour);
            let e = run.next(5.0).await;
            assert_eq!(
                (e["t"].as_str(), e["id"].as_str(), e["code"].as_str()),
                (Some("error"), Some(id.as_str()), Some("endpoint")),
                "{e}"
            );
            assert_eq!(e["retryable"].as_bool(), Some(retryable), "{e}");
            let message = e["message"].as_str().expect("a message");
            assert!(message.contains(says), "{message}");
            assert!(
                message.chars().count() < 400,
                "the body is quoted to 300 characters: {message}"
            );
        }
        run.end();
        // No endpoint at all: refused connections are worth another try.
        let mut run = start(|c| c.base_url = "http://127.0.0.1:9/v1".to_owned()).await;
        run.send(request("r1", json!({}))).await;
        let e = run.next(15.0).await;
        assert_eq!(
            (e["code"].as_str(), e["retryable"].as_bool()),
            (Some("endpoint"), Some(true)),
            "{e}"
        );
        run.end();
    });
}

#[test]
fn an_answer_too_large_for_a_frame_is_an_endpoint_error() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        // Nine megabytes of text: the stream is dropped once it passes eight.
        let mib = "m".repeat(1024 * 1024);
        let events: Vec<Value> =
            (0..9).map(|_| json!({"choices": [{"index": 0, "delta": {"content": mib}}]})).collect();
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        run.ep.respond(Behaviour::Stream { events, delay: Duration::ZERO, first: Duration::ZERO });
        let e = run.next(10.0).await;
        assert_eq!(
            (e["code"].as_str(), e["retryable"].as_bool()),
            (Some("endpoint"), Some(false)),
            "{e}"
        );
        assert!(e["message"].as_str().is_some_and(|m| m.contains("grew past")), "{e}");
        assert!(run.gone(0, 2.0).await.is_some(), "the stream was dropped");
        // Two million control characters are 2 MB of text and 12 MB once JSON escapes them.
        let control = "\u{1}".repeat(2_000_000);
        run.send(request("r2", json!({}))).await;
        run.at_endpoint(1).await;
        run.ep.respond(Behaviour::Json(json!({"choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": control}}]})));
        let e = run.next(10.0).await;
        assert_eq!((e["id"].as_str(), e["code"].as_str()), (Some("r2"), Some("endpoint")), "{e}");
        assert!(e["message"].as_str().is_some_and(|m| m.contains("as a frame")), "{e}");
        run.end();
    });
}

#[test]
fn two_slots_run_two_at_once_and_a_third_is_busy_and_an_id_is_answered_once() {
    rt().block_on(async {
        let mut run = start(|c| c.max_concurrent = 2).await;
        run.send(request("r1", json!({}))).await;
        run.send(request("r2", json!({}))).await;
        run.at_endpoint(0).await;
        run.at_endpoint(1).await;
        assert_eq!(run.in_flight.get(), 2);
        run.send(request("r3", json!({}))).await;
        let busy = run.next(2.0).await;
        assert_eq!(
            (busy["id"].as_str(), busy["code"].as_str(), busy["retryable"].as_bool()),
            (Some("r3"), Some("busy"), Some(true))
        );
        // The server sends r1 again while it runs: it is not run twice, nor answered twice.
        run.send(request("r1", json!({}))).await;
        for _ in 0..2 {
            run.ep.respond(Behaviour::Stream {
                events: words(3),
                delay: Duration::from_millis(50),
                first: Duration::ZERO,
            });
        }
        let mut ids = vec![
            run.next(5.0).await["id"].as_str().expect("an id").to_owned(),
            run.next(5.0).await["id"].as_str().expect("an id").to_owned(),
        ];
        ids.sort();
        assert_eq!(ids, ["r1", "r2"]);
        run.quiet(1.0).await;
        assert_eq!(run.ep.requests(), 2, "r1 reached the endpoint once");
        assert!(run.log.contains("again while it is in flight"));
        // The statuses followed the count: up to 2, back to 0 (at most one every 5 s).
        let counts: Vec<u64> =
            run.statuses.iter().filter_map(|s| s["llm_in_flight"].as_u64()).collect();
        assert!(counts.first().is_some_and(|c| *c >= 1), "{counts:?}");
        run.end();
    });
}

#[test]
fn a_stop_answers_each_completion_stopping_and_closes_its_connection() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        run.ep.respond(Behaviour::Hang);
        tokio::time::sleep(Duration::from_millis(200)).await;
        run.stop.send(true).expect("stopped");
        let e = run.next(3.0).await;
        assert_eq!(
            (e["id"].as_str(), e["code"].as_str(), e["retryable"].as_bool()),
            (Some("r1"), Some("stopping"), Some(true)),
            "{e}"
        );
        assert!(run.gone(0, 1.0).await.is_some());
        let exit = tokio::time::timeout(Duration::from_secs(5), &mut run.task)
            .await
            .expect("ended")
            .expect("joined");
        assert_eq!(exit, Exit::Stopped);
        assert_eq!(run.in_flight.get(), 0);
    });
}

#[test]
fn the_status_carries_the_completions_in_flight() {
    rt().block_on(async {
        let mut run = start(|_| {}).await;
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        run.quiet(1.5).await;
        assert_eq!(
            run.statuses.last().map(|s| s["llm_in_flight"].clone()),
            Some(json!(1)),
            "{:?}",
            run.statuses
        );
        run.ep.respond(Behaviour::Stream {
            events: words(1),
            delay: Duration::ZERO,
            first: Duration::ZERO,
        });
        assert_eq!(run.next(5.0).await["t"], "done");
        // The next status may go 5 s after the first.
        run.quiet(6.0).await;
        assert_eq!(
            run.statuses.last().map(|s| s["llm_in_flight"].clone()),
            Some(json!(0)),
            "{:?}",
            run.statuses
        );
        run.end();
    });
}

/// An endpoint that never answers, with no network: for the paused clock.
struct Silent;

impl Endpoint for Silent {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Listed>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn complete(&self, _body: Vec<u8>) -> BoxFuture<'_, Result<http1::Response, http1::Failure>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn a_timeout_above_fifteen_minutes_is_held_to_fifteen() {
    let req: Request = serde_json::from_value(json!({
        "id": "r1", "model": "m", "messages": [{"role": "user", "content": "x"}],
        "tools": null, "params": {}, "timeout_s": 86_400
    }))
    .expect("a request");
    let started = Instant::now();
    let out =
        proxy::run(Arc::new(Silent), req, true, Arc::default(), Arc::default(), Log::memory())
            .await;
    let Outcome::Failed { code, retryable, .. } = out else { panic!("{out:?}") };
    assert_eq!((code, retryable), (RequestErrorCode::Timeout, false));
    assert_eq!(started.elapsed().as_secs(), 900);
}

/// The API key goes to the configured endpoint only, as a bearer token; an endpoint that echoes
/// it in an error has it replaced before the error goes to the server.
#[test]
fn the_api_key_reaches_the_endpoint_and_never_the_server() {
    rt().block_on(async {
        const KEY: &str = "sk-test-0123456789abcdef";
        let mut run = start_keyed(|_| {}, Some(KEY)).await;
        run.send(request("r1", json!({}))).await;
        run.at_endpoint(0).await;
        let head = run.ep.seen(0).expect("seen").head.to_ascii_lowercase();
        assert!(
            head.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())),
            "{head}"
        );
        run.ep.respond(Behaviour::Status(401, format!("invalid key {KEY} for this model")));
        let e = run.next(5.0).await;
        let message = e["message"].as_str().expect("a message");
        assert!(!message.contains(KEY) && message.contains("[redacted]"), "{message}");
        // In a stream's error too.
        run.send(request("r2", json!({}))).await;
        run.at_endpoint(1).await;
        run.ep.respond(Behaviour::Stream {
            events: vec![json!({"error": {"message": format!("bad key {KEY}")}})],
            delay: Duration::ZERO,
            first: Duration::ZERO,
        });
        let e = run.next(5.0).await;
        assert!(!e.to_string().contains(KEY), "{e}");
        run.end();
    });
}
