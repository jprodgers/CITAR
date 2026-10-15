//! The real model through the real binary (design/phase4-helper.md 4-04, gate 4): opt-in, with
//! `CITAR_TEST_LMSTUDIO=http://localhost:1234` naming the machine's own LM Studio. It is an
//! ignored test, so a run that does not ask for it reports it skipped rather than passed:
//! `CITAR_TEST_LMSTUDIO=http://localhost:1234 cargo nextest run -p citar-helper --features
//! citar-helper/dev-guard --run-ignored only -E 'binary(real_model)' --no-capture` runs it, and
//! asked for without the variable it fails, saying so. It reads LM Studio's model list
//! (`/api/v0/models`) and requires
//! `google/gemma-4-e2b` loaded before it sends anything and again after; it never loads, unloads
//! or swaps a model, and a model that is not loaded fails the test with nothing sent to it
//! (P4.7.1 rule 7). Then, through the binary and a hub of the test's own on loopback:
//! - a request with a tool gets a `done` whose raw message holds a tool call or text;
//! - three short requests time the model's usual answer;
//! - a long generation (up to 1,500 tokens) cancelled at 2 s is acknowledged within 1 s;
//! - three short requests after it each take no longer than 1.5 times the median before plus 1 s
//!   (a generation the cancel did not stop would hold the model, and they would queue behind it).
//!
//! The same checks run in every test run against a stand-in for LM Studio (`common`'s fake
//! endpoint with one model generating one answer at a time), so the checks themselves are tested
//! where the real model is not there: they pass against a server that stops a generation when its
//! client goes, and fail against one that does not (the answers after the cancel wait for the
//! abandoned generation), against a model that is not loaded (with nothing sent to it) and
//! against one unloaded during the run.

mod common;

use std::time::Duration;

use citar_helper::llm::{Endpoint as _, HttpEndpoint, Listed};
use citar_helper::protocol::Provider;
use common::{Behaviour, FakeEndpoint, Models, ServerWs, loopback_hub, scratch};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");
const MODEL: &str = "google/gemma-4-e2b";
const TOKEN: &str = "cwt_real_model_0123456789abcdef";

/// Whether LM Studio at `base` says the model is loaded: its own API (`/api/v0/models`), read
/// through the helper's listing, which only reads.
async fn loaded(base: &str) -> bool {
    let tls = citar_helper::net::verified_tls(None).expect("tls");
    let ep =
        HttpEndpoint::new(Provider::LmStudio, &format!("{base}/v1"), None, tls).expect("a client");
    ep.list().await.is_ok_and(|models| models.iter().any(|m| m.key == MODEL && m.loaded))
}

fn text(m: &Message) -> Option<Value> {
    match m {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

/// The helper's next frame that is not a ping, pong, status or catalog, within `secs`.
async fn next(ws: &mut ServerWs, secs: f64) -> Result<Value, String> {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        let m = tokio::time::timeout_at(until, ws.next())
            .await
            .map_err(|_| format!("no frame from the helper within {secs} s"))?
            .ok_or("the helper's connection ended")?
            .map_err(|e| format!("the helper's connection broke: {e}"))?;
        let Some(v) = text(&m) else { continue };
        match v["t"].as_str() {
            Some("ping") => {
                ws.send(Message::Text(r#"{"t": "pong"}"#.into()))
                    .await
                    .map_err(|e| format!("pong: {e}"))?;
            }
            Some("pong" | "status" | "catalog") => {}
            _ => return Ok(v),
        }
    }
}

async fn send(ws: &mut ServerWs, frame: &Value) -> Result<(), String> {
    ws.send(Message::Text(frame.to_string().into())).await.map_err(|e| format!("send: {e}"))
}

fn request(id: &str, user: &str, max_tokens: u32, tools: Option<Value>) -> Value {
    json!({
        "t": "request", "id": id, "model": MODEL,
        "messages": [{"role": "system", "content": "You are a helpful assistant in a strategy game."},
                     {"role": "user", "content": user}],
        "tools": tools, "params": {"temperature": 0, "max_tokens": max_tokens}, "timeout_s": 300
    })
}

/// One short request's round trip, from sending it to its `done`.
async fn short(ws: &mut ServerWs, id: &str) -> Result<f64, String> {
    let t0 = Instant::now();
    send(ws, &request(id, "Reply with the single word OK.", 16, None)).await?;
    let done = next(ws, 120.0).await?;
    if (done["t"].as_str(), done["id"].as_str()) != (Some("done"), Some(id)) {
        return Err(format!("short request {id} was answered {done}"));
    }
    Ok(t0.elapsed().as_secs_f64())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// What one run of the checks measured.
#[derive(Debug)]
struct Report {
    tool_calls: usize,
    text_len: usize,
    tools_line: String,
    before: Vec<f64>,
    /// When the `cancel` went to the helper.
    cancel_sent: Instant,
    acked: f64,
    tokens_out: u64,
    after: Vec<f64>,
    bound: f64,
}

impl Report {
    fn summary(&self) -> String {
        format!(
            "{}\nbefore {:.3?} (median {:.3}), cancel acknowledged in {:.3} s with {} tokens, after {:.3?}, bound {:.3}",
            self.tools_line,
            self.before,
            median(self.before.clone()),
            self.acked,
            self.tokens_out,
            self.after,
            self.bound
        )
    }
}

/// Gate 4's checks against the LM Studio at `base` (a loopback origin), through the real binary
/// in a scratch home named after `name`: the first check that fails is the error. Nothing is sent
/// to a model that is not loaded.
async fn exercise(base: &str, name: &str) -> Result<Report, String> {
    let host_ok = ["http://localhost:", "http://127.0.0.1:", "http://[::1]:"]
        .iter()
        .any(|p| base.starts_with(p));
    if !host_ok {
        return Err(format!("the real model is this machine's own LM Studio, not {base}"));
    }
    if !loaded(base).await {
        return Err(format!("{MODEL} is not loaded in LM Studio: nothing was sent to it"));
    }

    let root = scratch(name);
    let home = root.join("home");
    std::fs::create_dir_all(&home).map_err(|e| format!("home: {e}"))?;
    let (hub, mut conns) = loopback_hub().await;
    let mut command = tokio::process::Command::new(BIN);
    // Nothing of a real installation: the variables that could name one are removed.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("CITAR_WORKER_") || k == "CITAR_DATA_DIR" {
            command.env_remove(k);
        }
    }
    let mut child = command
        .current_dir(&root)
        .kill_on_drop(true)
        .env("CITAR_DATA_DIR", &home)
        .args(["--home"])
        .arg(&home)
        .args(["--server", &format!("http://127.0.0.1:{hub}"), "--token", TOKEN])
        .args(["--base-url", &format!("{base}/v1"), "--model", MODEL, "--max-concurrent", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("the binary did not start: {e}"))?;
    let checked = async {
        let mut ws = tokio::time::timeout(Duration::from_secs(30), conns.recv())
            .await
            .map_err(|_| "the helper did not connect within 30 s".to_owned())?
            .ok_or("the hub stopped")?;
        checks(&mut ws).await
    }
    .await;
    let _killed = child.kill().await;
    let _cleaned = std::fs::remove_dir_all(&root);
    let report = checked?;
    if !loaded(base).await {
        return Err(format!("{MODEL} is no longer loaded after the test"));
    }
    Ok(report)
}

/// The checks on the helper's connection, from its hello.
async fn checks(ws: &mut ServerWs) -> Result<Report, String> {
    let hello = next(ws, 10.0).await?;
    let lists_it = hello["llm"]["models"]
        .as_array()
        .is_some_and(|m| m.iter().any(|m| m["key"] == MODEL && m["loaded"] == true));
    if hello["t"] != "hello" || hello["llm"]["enabled"] != true || !lists_it {
        return Err(format!("the hello does not offer {MODEL} loaded: {hello}"));
    }
    let welcome = json!({
        "t": "welcome", "protocol": 2, "machine": {"id": "sv_real", "name": "real"},
        "settings": {"llm": {"enabled": true, "slots": 1, "allow_swaps": false},
                     "cpu": {"enabled": false, "threads": 0, "cpu_percent": 0, "memory_mb": 0,
                             "work": [], "yield_to_llm": true}},
        "ping_s": 20, "server": {"version": "0.1.6", "build": "a1b2c3d4e5f6", "jobs_build": null},
        "update": null, "jobs": {"keep": [], "drop": [], "resend": []}
    });
    send(ws, &welcome).await?;
    let ack = next(ws, 10.0).await?;
    if ack["t"] != "settings_ack" {
        return Err(format!("the welcome was answered {ack}"));
    }

    // A request with a tool: the raw message holds a tool call or text.
    let tool = json!([{"type": "function", "function": {
        "name": "end_turn", "description": "End your turn in the game.",
        "parameters": {"type": "object", "properties": {"note": {"type": "string"}}, "required": []}}}]);
    let t0 = Instant::now();
    let user = "It is your turn. You have nothing to do: end your turn now.";
    send(ws, &request("tools", user, 400, Some(tool))).await?;
    let done = next(ws, 180.0).await?;
    if (done["t"].as_str(), done["id"].as_str(), done["streamed"].as_bool())
        != (Some("done"), Some("tools"), Some(true))
    {
        return Err(format!("the request with a tool was answered {done}"));
    }
    let tool_calls = done["message"]["tool_calls"].as_array().map_or(0, Vec::len);
    let text_len = done["message"]["content"].as_str().map_or(0, str::len);
    if tool_calls == 0 && text_len == 0 {
        return Err(format!("neither a tool call nor text: {done}"));
    }
    let tools_line = format!(
        "tools: {tool_calls} tool call(s), {text_len} characters of text, finish {}, {:.2} s (gen_s {}, ttft_s {})",
        done["finish_reason"],
        t0.elapsed().as_secs_f64(),
        done["gen_s"],
        done["ttft_s"]
    );

    let mut before = Vec::new();
    for i in 0..3 {
        before.push(short(ws, &format!("before{i}")).await?);
    }

    // A long generation, cancelled at 2 s.
    let count = "Count from 1 to 2000, one number per line, with nothing else.";
    send(ws, &request("long", count, 1500, None)).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let cancel_sent = Instant::now();
    send(ws, &json!({"t": "cancel", "id": "long"})).await?;
    let ack = next(ws, 5.0).await?;
    let acked = cancel_sent.elapsed().as_secs_f64();
    if (ack["t"].as_str(), ack["id"].as_str()) != (Some("cancelled"), Some("long")) {
        return Err(format!("the cancel was answered {ack}"));
    }
    if acked >= 1.0 {
        return Err(format!("the cancel was acknowledged after {acked:.3} s"));
    }
    let tokens_out = ack["tokens_out"].as_u64().unwrap_or(0);
    if tokens_out == 0 {
        return Err(format!("it was not generating when it was cancelled: {ack}"));
    }

    // Each is measured, then each is held to the bound.
    let mut after = Vec::new();
    for i in 0..3 {
        after.push(short(ws, &format!("after{i}")).await?);
    }
    let bound = 1.5 * median(before.clone()) + 1.0;
    let report = Report {
        tool_calls,
        text_len,
        tools_line,
        before,
        cancel_sent,
        acked,
        tokens_out,
        after,
        bound,
    };
    if let Some((i, a)) = report.after.iter().enumerate().find(|(_, a)| **a > bound) {
        return Err(format!(
            "short request {i} after the cancel took {a:.3} s, over {bound:.3}\n{}",
            report.summary()
        ));
    }
    Ok(report)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in: CITAR_TEST_LMSTUDIO=http://localhost:1234, with google/gemma-4-e2b loaded in LM Studio, and --run-ignored only"]
async fn the_real_model_answers_with_tools_and_a_cancel_stops_it() {
    let base = std::env::var("CITAR_TEST_LMSTUDIO").expect(
        "the real-model test needs CITAR_TEST_LMSTUDIO=http://localhost:1234 (this machine's LM Studio)",
    );
    let base = base.trim_end_matches('/').to_owned();
    match exercise(&base, "real-model").await {
        Ok(report) => eprintln!("{}", report.summary()),
        Err(e) => panic!("{e}"),
    }
}

// The stand-in for LM Studio.

/// Events 50 ms apart in the long answer: the 2 s before the cancel see 30 to 40 of its 100.
const LONG_EVENT: Duration = Duration::from_millis(50);
/// The long answer's events, ten numbers each.
const LONG_EVENTS: usize = 100;

fn chunk(delta: &Value, finish: Option<&str>) -> Value {
    json!({"id": "chatcmpl-stand-in", "object": "chat.completion.chunk", "model": MODEL,
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn usage(prompt: u64, completion: u64) -> Value {
    json!({"id": "chatcmpl-stand-in", "object": "chat.completion.chunk", "model": MODEL, "choices": [],
           "usage": {"prompt_tokens": prompt, "completion_tokens": completion,
                     "total_tokens": prompt + completion}})
}

fn short_answer() -> Behaviour {
    Behaviour::Stream {
        events: vec![
            chunk(&json!({"role": "assistant", "content": "OK"}), None),
            chunk(&json!({}), Some("stop")),
            usage(30, 1),
        ],
        delay: Duration::from_millis(10),
        first: Duration::from_millis(20),
    }
}

/// LM Studio as gate 4 meets it, played by `common`'s fake endpoint: `google/gemma-4-e2b` listed
/// by LM Studio's own API (loaded or not), connections kept open, one model generating one answer
/// at a time (`runs_on`: a generation its client left keeps the model busy to its end), and the
/// eight answers the checks ask for, in their order: a streamed tool call, three short answers, a
/// long count (100 events of ten numbers, 50 ms apart), three short answers.
async fn stand_in(is_loaded: bool, runs_on: bool) -> FakeEndpoint {
    let models = Models::default();
    models.set(vec![Listed {
        key: MODEL.to_owned(),
        label: MODEL.to_owned(),
        context: Some(131_072),
        loaded: is_loaded,
    }]);
    let ep = FakeEndpoint::start(models).await;
    ep.keep_alive(true);
    ep.one_model(runs_on);
    let call = json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_stand_in",
        "type": "function", "function": {"name": "end_turn", "arguments": ""}}]});
    let args = json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"note\": \"nothing to do\"}"}}]});
    ep.respond(Behaviour::Stream {
        events: vec![
            chunk(&call, None),
            chunk(&args, None),
            chunk(&json!({}), Some("tool_calls")),
            usage(80, 12),
        ],
        delay: Duration::from_millis(20),
        first: Duration::from_millis(100),
    });
    for _ in 0..3 {
        ep.respond(short_answer());
    }
    let long: Vec<Value> = (0..LONG_EVENTS)
        .map(|i| {
            let numbers: String = (i * 10 + 1..=i * 10 + 10).map(|n| format!("{n}\n")).collect();
            chunk(&json!({"content": numbers}), None)
        })
        .collect();
    ep.respond(Behaviour::Stream { events: long, delay: LONG_EVENT, first: LONG_EVENT });
    for _ in 0..3 {
        ep.respond(short_answer());
    }
    ep
}

fn origin(ep: &FakeEndpoint) -> String {
    format!("http://127.0.0.1:{}", ep.port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_checks_pass_against_a_stand_in_that_stops_a_cancelled_generation() {
    let ep = stand_in(true, false).await;
    let report = exercise(&origin(&ep), "stand-in").await.unwrap_or_else(|e| panic!("{e}"));
    eprintln!("{}", report.summary());
    assert_eq!((report.tool_calls, report.text_len), (1, 0), "{report:?}");
    // The endpoint's side: eight completions, the first with its tool and streamed, and the long
    // one's client gone within 1 s of the cancel with at most the event on its way after it.
    assert_eq!(ep.requests(), 8);
    let first = ep.seen(0).expect("the request with a tool");
    assert_eq!(first.body["tools"][0]["function"]["name"], "end_turn", "{}", first.body);
    assert_eq!(first.body["stream"], true, "{}", first.body);
    let long = ep.seen(4).expect("the long request");
    let gone = long.gone.expect("the long request's client went");
    assert!(
        gone.saturating_duration_since(report.cancel_sent) < Duration::from_secs(1),
        "the long request's client went {:?} after the cancel",
        gone.saturating_duration_since(report.cancel_sent)
    );
    let late = long.tokens.iter().filter(|t| **t > report.cancel_sent).count();
    assert!(late <= 1, "{late} events went out after the cancel");
    assert!(long.tokens.len() < LONG_EVENTS, "the long answer was not cut short");
    // The tokens received: those sent, but for the one or two still on their way at the cancel.
    let sent = u64::try_from(long.tokens.len()).expect("a count");
    assert!(
        (sent.saturating_sub(2)..=sent).contains(&report.tokens_out),
        "{sent} sent: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_checks_fail_against_a_stand_in_that_runs_on_after_a_cancel() {
    // The cancel is acknowledged at once (the helper's side is right), but the answers after it
    // wait for the abandoned generation's remaining 3 s: the bound catches the held model.
    let ep = stand_in(true, true).await;
    let err = exercise(&origin(&ep), "stand-in-runs-on").await.expect_err("a held model fails");
    eprintln!("{err}");
    assert!(err.contains("short request 0 after the cancel took"), "{err}");
    assert_eq!(ep.requests(), 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_not_loaded_fails_the_checks_with_nothing_sent_to_it() {
    let ep = stand_in(false, false).await;
    let err = exercise(&origin(&ep), "stand-in-not-loaded").await.expect_err("not loaded");
    eprintln!("{err}");
    assert!(err.contains("is not loaded in LM Studio: nothing was sent to it"), "{err}");
    assert_eq!(ep.requests(), 0, "a completion went to a model that is not loaded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_unloaded_during_the_run_fails_the_checks() {
    let ep = stand_in(true, false).await;
    let unload = {
        let ep = ep.clone();
        tokio::spawn(async move {
            // When the last request comes, the model goes (its answer's first event is 20 ms
            // away, so this is before the test's last look at the list).
            let far = Instant::now() + Duration::from_secs(120);
            if ep.request_body(7, far).await.is_some() {
                let mut models = ep.models.get();
                for m in &mut models {
                    m.loaded = false;
                }
                ep.models.set(models);
            }
        })
    };
    let err = exercise(&origin(&ep), "stand-in-unloaded").await.expect_err("unloaded");
    eprintln!("{err}");
    assert!(err.contains("is no longer loaded after the test"), "{err}");
    unload.await.expect("the unloading task");
}
