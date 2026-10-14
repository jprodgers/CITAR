//! The real model through the real binary (design/phase4-helper.md 4-04, gate 4): opt-in, with
//! `CITAR_TEST_LMSTUDIO=http://localhost:1234` naming the machine's own LM Studio, and skipped
//! otherwise. It reads LM Studio's model list (`/api/v0/models`) and requires
//! `google/gemma-4-e2b` loaded before it sends anything and again after; it never loads, unloads
//! or swaps a model, and a model that is not loaded fails the test with nothing sent to it
//! (P4.7.1 rule 7). Then, through the binary and a hub of the test's own on loopback:
//! - a request with a tool gets a `done` whose raw message holds a tool call or text;
//! - three short requests time the model's usual answer;
//! - a long generation (up to 1,500 tokens) cancelled at 2 s is acknowledged within 1 s;
//! - three short requests after it each take no longer than 1.5 times the median before plus 1 s
//!   (a generation the cancel did not stop would hold the model, and they would queue behind it).

mod common;

use std::time::Duration;

use citar_helper::llm::{Endpoint as _, HttpEndpoint};
use citar_helper::protocol::Provider;
use common::{ServerWs, loopback_hub, scratch};
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

/// The helper's next frame that is not a ping, pong or status, within `secs`.
async fn next(ws: &mut ServerWs, secs: f64) -> Value {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        let m = tokio::time::timeout_at(until, ws.next())
            .await
            .unwrap_or_else(|_| panic!("no frame within {secs} s"))
            .expect("the connection is open")
            .expect("a frame");
        let Some(v) = text(&m) else { continue };
        match v["t"].as_str() {
            Some("ping") => {
                ws.send(Message::Text(r#"{"t": "pong"}"#.into())).await.expect("pong");
            }
            Some("pong" | "status" | "catalog") => {}
            _ => return v,
        }
    }
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
async fn short(ws: &mut ServerWs, id: &str) -> f64 {
    let t0 = Instant::now();
    ws.send(Message::Text(
        request(id, "Reply with the single word OK.", 16, None).to_string().into(),
    ))
    .await
    .expect("sent");
    let done = next(ws, 120.0).await;
    assert_eq!((done["t"].as_str(), done["id"].as_str()), (Some("done"), Some(id)), "{done}");
    t0.elapsed().as_secs_f64()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_model_answers_with_tools_and_a_cancel_stops_it() {
    let Ok(base) = std::env::var("CITAR_TEST_LMSTUDIO") else {
        eprintln!("skipped: set CITAR_TEST_LMSTUDIO=http://localhost:1234 to run it");
        return;
    };
    let base = base.trim_end_matches('/').to_owned();
    let host_ok = ["http://localhost:", "http://127.0.0.1:", "http://[::1]:"]
        .iter()
        .any(|p| base.starts_with(p));
    assert!(host_ok, "the real model is this machine's own LM Studio, not {base}");
    assert!(loaded(&base).await, "{MODEL} is not loaded in LM Studio: nothing was sent to it");

    let root = scratch("real-model");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
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
        .expect("the binary starts");
    let mut ws = tokio::time::timeout(Duration::from_secs(30), conns.recv())
        .await
        .expect("a connection in time")
        .expect("a connection");
    let hello = next(&mut ws, 10.0).await;
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["llm"]["enabled"], true);
    assert!(
        hello["llm"]["models"]
            .as_array()
            .is_some_and(|m| m.iter().any(|m| m["key"] == MODEL && m["loaded"] == true)),
        "{hello}"
    );
    let welcome = json!({
        "t": "welcome", "protocol": 2, "machine": {"id": "sv_real", "name": "real"},
        "settings": {"llm": {"enabled": true, "slots": 1, "allow_swaps": false},
                     "cpu": {"enabled": false, "threads": 0, "cpu_percent": 0, "memory_mb": 0,
                             "work": [], "yield_to_llm": true}},
        "ping_s": 20, "server": {"version": "0.1.6", "build": "a1b2c3d4e5f6", "jobs_build": null},
        "update": null, "jobs": {"keep": [], "drop": [], "resend": []}
    });
    ws.send(Message::Text(welcome.to_string().into())).await.expect("welcome");
    assert_eq!(next(&mut ws, 10.0).await["t"], "settings_ack");

    // A request with a tool: the raw message holds a tool call or text.
    let tool = json!([{"type": "function", "function": {
        "name": "end_turn", "description": "End your turn in the game.",
        "parameters": {"type": "object", "properties": {"note": {"type": "string"}}, "required": []}}}]);
    let t0 = Instant::now();
    ws.send(Message::Text(
        request(
            "tools",
            "It is your turn. You have nothing to do: end your turn now.",
            400,
            Some(tool),
        )
        .to_string()
        .into(),
    ))
    .await
    .expect("sent");
    let done = next(&mut ws, 180.0).await;
    assert_eq!(
        (done["t"].as_str(), done["streamed"].as_bool()),
        (Some("done"), Some(true)),
        "{done}"
    );
    let calls = done["message"]["tool_calls"].as_array().map_or(0, Vec::len);
    let text_len = done["message"]["content"].as_str().map_or(0, str::len);
    assert!(calls > 0 || text_len > 0, "neither a tool call nor text: {done}");
    eprintln!(
        "tools: {calls} tool call(s), {text_len} characters of text, finish {}, {:.2} s (gen_s {}, ttft_s {})",
        done["finish_reason"],
        t0.elapsed().as_secs_f64(),
        done["gen_s"],
        done["ttft_s"]
    );

    let mut before = Vec::new();
    for i in 0..3 {
        before.push(short(&mut ws, &format!("before{i}")).await);
    }

    // A long generation, cancelled at 2 s.
    ws.send(Message::Text(
        request(
            "long",
            "Count from 1 to 2000, one number per line, with nothing else.",
            1500,
            None,
        )
        .to_string()
        .into(),
    ))
    .await
    .expect("sent");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let cancel_sent = Instant::now();
    ws.send(Message::Text(r#"{"t": "cancel", "id": "long"}"#.into())).await.expect("cancel");
    let ack = next(&mut ws, 5.0).await;
    let acked = cancel_sent.elapsed().as_secs_f64();
    assert_eq!((ack["t"].as_str(), ack["id"].as_str()), (Some("cancelled"), Some("long")), "{ack}");
    assert!(acked < 1.0, "the cancel was acknowledged after {acked:.3} s");
    assert!(ack["tokens_out"].as_u64().is_some_and(|n| n > 0), "it was generating: {ack}");

    let mut after = Vec::new();
    for i in 0..3 {
        after.push(short(&mut ws, &format!("after{i}")).await);
    }
    let bound = 1.5 * median(before.clone()) + 1.0;
    eprintln!(
        "before {before:.3?} (median {:.3}), cancel acknowledged in {acked:.3} s with {} tokens, after {after:.3?}, bound {bound:.3}",
        median(before.clone()),
        ack["tokens_out"]
    );
    for (i, a) in after.iter().enumerate() {
        assert!(*a <= bound, "short request {i} after the cancel took {a:.3} s, over {bound:.3}");
    }

    let _killed = child.kill().await;
    assert!(loaded(&base).await, "{MODEL} is no longer loaded after the test");
    std::fs::remove_dir_all(&root).expect("cleaned");
}
