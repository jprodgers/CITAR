//! The transcripts from the helper's side (design/phase4-helper.md P4.2.9; package 4-03, gate 2):
//! every transcript a package up to this one plays from the helper's side runs against the
//! helper's own session code, in-process, on tokio's paused clock (the fake clock: an hour's idle
//! takes no time), through an in-memory connection to the fake server of `common`. The hub's
//! side plays the same files from 4-05 on (`tests/test_helper_conformance.py`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::conformance::transcript::Transcript;
use citar_helper::conformance::{canonical_examples, transcripts};
use citar_helper::key::MachineKey;
use citar_helper::llm::{BoxFuture, SharedEndpoint};
use citar_helper::log::Log;
use citar_helper::net::{Dialer, Io, Ws, ws_config};
use citar_helper::protocol::{Kind, Provider, UpdatePolicy};
use citar_helper::quiet::{self, Quiet};
use citar_helper::session::{Exit, Facts, Helper};
use common::{Models, Player, ServerWs, UnderTest, listed};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// The packages whose helper-side transcripts this crate plays (it refuses what it cannot do).
const PLAYED_THROUGH: &str = "4-03";

/// The fake clock's start: Wednesday 2026-10-14, 12:00 UTC, outside any transcript's quiet hours.
const START_UNIX: f64 = 1_791_979_200.0;

/// An in-memory connection per dial, the fake server's end handed to the player.
struct Pipe(mpsc::UnboundedSender<ServerWs>);

impl Dialer for Pipe {
    fn dial(&self) -> BoxFuture<'_, Result<Ws, String>> {
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 20);
            let tx = self.0.clone();
            // The server's half of the handshake runs beside the client's.
            tokio::spawn(async move {
                let server: Box<dyn Io> = Box::new(server);
                if let Ok(ws) =
                    tokio_tungstenite::accept_async_with_config(server, Some(ws_config())).await
                {
                    let _gone = tx.send(ws);
                }
            });
            let client: Box<dyn Io> = Box::new(client);
            let (ws, _) = tokio_tungstenite::client_async_with_config(
                "ws://127.0.0.1/ws/worker",
                client,
                Some(ws_config()),
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(ws)
        })
    }
}

struct InProcess {
    task: Option<tokio::task::JoinHandle<Exit>>,
    log: Log,
    clock: Clock,
    quiet: Quiet,
}

impl UnderTest for InProcess {
    fn exited(&mut self, within: Duration) -> BoxFuture<'_, Option<i32>> {
        Box::pin(async move {
            let task = self.task.as_mut()?;
            match tokio::time::timeout(within, task).await {
                Ok(Ok(exit)) => {
                    self.task = None;
                    Some(i32::from(exit.code()))
                }
                _ => None,
            }
        })
    }

    fn logged(&self, text: &str) -> bool {
        self.log.contains(text)
    }

    fn act(&mut self, what: &Value) {
        match what.as_str() {
            Some("quiet_begins") => {
                let start = self
                    .quiet
                    .next_start(self.clock.now_local())
                    .expect("the helper has quiet hours");
                self.clock.jump_to(self.clock.unix_of(start));
            }
            other => panic!("this player cannot yet act {other:?} around the helper"),
        }
    }
}

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// The helper's settings from `setup.helper.config`, with the token the transcript gives.
fn config(t: &Transcript) -> Config {
    let c = t.setup_helper.get("config").and_then(Value::as_object).cloned().unwrap_or_default();
    let quiet_entries: Vec<Value> = match c.get("quiet") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(v) => vec![v.clone()],
    };
    let cpu = c.get("cpu").and_then(Value::as_object).cloned().unwrap_or_default();
    let kinds = |v: &Value| -> Vec<Kind> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|k| match k.as_str() {
                        Some("lab_game") => Some(Kind::LabGame),
                        Some("ranking_game") => Some(Kind::RankingGame),
                        Some("reference_game") => Some(Kind::ReferenceGame),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Config {
        server_url: "http://127.0.0.1:1".to_owned(),
        token: t
            .values
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("cwt_transcript_token")
            .to_owned(),
        base_url: "http://127.0.0.1:2/v1".to_owned(),
        provider: Provider::LmStudio,
        api_key_env: String::new(),
        max_concurrent: c.get("max_concurrent").and_then(u32_of).unwrap_or(1),
        name: String::new(),
        quiet: quiet::parse(&quiet_entries).expect("the transcript's quiet hours read"),
        models: c
            .get("models")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
        insecure: false,
        collect_hardware: true,
        hardware_file: String::new(),
        no_llm: c.get("no_llm").and_then(Value::as_bool).unwrap_or(false),
        cpu: LocalCpu {
            enabled: cpu.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            threads: cpu.get("threads").and_then(u32_of),
            cpu_percent: cpu.get("cpu_percent").and_then(u32_of),
            memory_mb: cpu.get("memory_mb").and_then(Value::as_u64),
            work: cpu.get("work").map(kinds),
        },
        updates: match c.get("updates").and_then(Value::as_str) {
            Some("notify") => UpdatePolicy::Notify,
            Some("off") => UpdatePolicy::Off,
            _ => UpdatePolicy::Auto,
        },
        stream: c.get("stream").and_then(Value::as_bool).unwrap_or(true),
        ca_file: None,
        no_power: c.get("no_power").and_then(Value::as_bool).unwrap_or(false),
        warnings: Vec::new(),
    }
}

fn facts() -> Facts {
    let info = citar_helper::version::build_info();
    let mut bots: Vec<String> =
        citar_bot::versions().iter().map(|v| v.id.id().to_owned()).collect();
    bots.sort();
    Facts {
        hostname: "transcript-host".to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        build: info.build,
        version: info.version.to_owned(),
        target: info.target.to_owned(),
        label: info.label.to_owned(),
        bots,
        ruleset: citar_engine::rules::Ruleset::shared().id().to_hex(),
    }
}

type Canonical = std::collections::BTreeMap<(citar_helper::protocol::Sender, String), Value>;

fn play(t: &Transcript, canonical: &Canonical) -> Result<(), String> {
    play_with(t, canonical, |_| {})
}

/// Plays `t` with the helper's settings changed by `tweak` (for settings no transcript key names).
fn play_with(
    t: &Transcript,
    canonical: &Canonical,
    tweak: impl FnOnce(&mut Config),
) -> Result<(), String> {
    for key in t.setup_helper.keys() {
        if !["config", "clock", "service", "endpoint"].contains(&key.as_str()) {
            return Err(format!("{}: this player cannot yet set up the helper's {key:?}", t.name));
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .map_err(|e| e.to_string())?;
    let facts = facts();
    rt.block_on(async {
        let mut cfg = config(t);
        tweak(&mut cfg);
        let models = Models::default();
        if let Some(m) = t.setup_helper.get("endpoint").and_then(|e| e.get("models")) {
            models.set(listed(m));
        }
        let (conns_tx, conns_rx) = mpsc::unbounded_channel();
        let key = MachineKey::generate()?;
        let log = Log::memory();
        let clock = Clock::fake(START_UNIX);
        let (_stop_tx, stop_rx) = watch::channel(false);
        let endpoint: Option<SharedEndpoint> =
            if cfg.no_llm { None } else { Some(Arc::new(models.clone())) };
        let ceiling = cfg.ceiling(Machine { logical_cpus: 8, memory_mb: 16_384 });
        let quiet = Quiet(cfg.quiet.clone());
        let mut player = Player::new(t, conns_rx, models, canonical.clone());
        player.bindings.bind("token", Value::String(cfg.token.clone()));
        player.bindings.bind("key", Value::String(key.public_b64()));
        let helper = Helper {
            config: cfg,
            public_key: key.public_b64(),
            facts,
            ceiling,
            log: log.clone(),
            clock: clock.clone(),
            dialer: Arc::new(Pipe(conns_tx)),
            endpoint,
            service: t.setup_helper.get("service").and_then(Value::as_bool).unwrap_or(false),
            stop: stop_rx,
        };
        let task = tokio::spawn(async move { helper.run().await });
        let mut under = InProcess { task: Some(task), log: log.clone(), clock, quiet };
        let result = player.play(&mut under).await;
        if let Some(task) = under.task.take() {
            task.abort();
        }
        result.map_err(|e| format!("{e}\n  the helper's log:\n    {}", log.lines().join("\n    ")))
    })
}

#[test]
fn the_helper_plays_its_transcripts() {
    let all = transcripts().expect("the transcripts load");
    let canonical = canonical_examples().expect("the canonical examples read");
    let mine: Vec<&Transcript> = all
        .iter()
        .filter(|t| t.plays.helper.as_deref().is_some_and(|p| p <= PLAYED_THROUGH))
        .collect();
    assert_eq!(mine.len(), 31, "4-03 plays 31 transcripts from the helper's side");
    let mut failures = Vec::new();
    for t in &mine {
        if let Err(e) = play(t, &canonical) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} failed:\n\n{}",
        failures.len(),
        mine.len(),
        failures.join("\n\n")
    );
}

/// A transcript with one step's value changed, as a JSON edit.
fn edited(name: &str, edit: impl FnOnce(&mut Value)) -> Transcript {
    let path =
        citar_helper::conformance::fixtures_dir().join("transcripts").join(format!("{name}.json"));
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    edit(&mut v);
    Transcript::from_value(&v).expect("still a transcript")
}

#[test]
fn the_player_fails_a_helper_that_does_not_keep_the_transcript() {
    let canonical = canonical_examples().expect("the canonical examples read");
    // An acknowledgement a helper with one slot sends is not the transcript's two.
    let t = edited("settings-capped", |v| {
        v["setup"]["helper"]["config"]["max_concurrent"] = Value::from(1)
    });
    let e = play(&t, &canonical).expect_err("slots 1 is not the transcript's 2");
    assert!(e.contains("effective.llm.slots"), "{e}");
    // The first reconnect comes after about 2 s, not within a tenth of a second of 1.9 s.
    let t = edited("reconnect-backoff", |v| v["steps"][5]["within_s"] = Value::from(0.05));
    let e = play(&t, &canonical).expect_err("too soon to reconnect");
    assert!(e.contains("no new connection"), "{e}");
    // A service idles a full hour: a quiet wait past it sees the reconnection.
    let t = edited("handshake-refused-token-service", |v| {
        v["steps"][3]["wait"]["s"] = Value::from(3610)
    });
    let e = play(&t, &canonical).expect_err("the hour is over");
    assert!(e.contains("during a quiet wait"), "{e}");
    // A console run exits 78, never 0.
    let t =
        edited("shutdown-revoked", |v| v["steps"][6]["state"]["expect"]["exit"] = Value::from(0));
    let e = play(&t, &canonical).expect_err("78, not 0");
    assert!(e.contains("exit"), "{e}");
}

#[test]
fn a_hardware_file_is_reported_in_the_hello_and_again_on_rescan() {
    let canonical = canonical_examples().expect("the canonical examples read");
    let dir = std::env::temp_dir().join(format!("citar-helper-conf-hw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let file = dir.join("hw.json");
    std::fs::write(&file, r#"{"format": "citar-hardware", "cpu": {"model": "Test CPU"}}"#)
        .expect("written");
    let scan = serde_json::json!({"format": "citar-hardware", "cpu": {"model": "Test CPU"}});
    let mut v: Value = serde_json::from_str(
        &std::fs::read_to_string(
            citar_helper::conformance::fixtures_dir()
                .join("transcripts")
                .join("handshake-welcome.json"),
        )
        .expect("read"),
    )
    .expect("json");
    v["name"] = Value::from("hardware-file");
    v["steps"][0]["h2s"]["report"] =
        serde_json::json!({"hardware": scan, "hardware_source": "file"});
    let steps = v["steps"].as_array_mut().expect("steps");
    steps.push(serde_json::json!({"s2h": {"t": "rescan"}}));
    steps.push(serde_json::json!({"h2s": {"t": "hardware", "scan": scan, "source": "file"}, "within_s": 2}));
    let t = Transcript::from_value(&v).expect("a transcript");
    let path = file.display().to_string();
    play_with(&t, &canonical, |c| c.hardware_file = path).expect("the file reported");
    // --no-hardware reports nothing, file or not.
    let mut v2 = v.clone();
    v2["steps"][0]["h2s"]["report"] =
        serde_json::json!({"hardware": null, "hardware_source": "none"});
    v2["steps"].as_array_mut().expect("steps").truncate(3);
    let t2 = Transcript::from_value(&v2).expect("a transcript");
    let path = file.display().to_string();
    play_with(&t2, &canonical, |c| {
        c.hardware_file = path;
        c.collect_hardware = false;
    })
    .expect("nothing reported");
    std::fs::remove_dir_all(&dir).expect("cleaned");
}
