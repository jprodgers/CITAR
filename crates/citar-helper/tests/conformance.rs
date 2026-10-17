//! The transcripts from the helper's side (design/phase4-helper.md P4.2.9; packages 4-03, gate 2,
//! and 4-04, gate 1): every transcript a package up to this one plays from the helper's side runs
//! against the helper's own session code, in-process, through an in-memory connection to the fake
//! server of `common`. Those with no completion run on tokio's paused clock (the fake clock: an
//! hour's idle takes no time); those with one (4-04's) run in real time against `common`'s
//! `FakeEndpoint` over HTTP on loopback, since a paused clock would fire the helper's timers while
//! the endpoint's bytes are on their way, and since what they check is the real connection to the
//! endpoint (its client gone within 1 s of a cancel). The hub's side plays the same files from 4-05
//! on (`tests/test_helper_conformance.py`). From 4-10 the in-process helper plays CPU jobs on
//! `common`'s fake job host, its results kept in a home of its own; from 4-13 it trusts the run's
//! throwaway update key when the transcript says so (`setup.helper.update_key: "test"`), and the
//! fake server relays manifests made for it and signed with that key.

mod common;

use std::sync::Arc;
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::conformance::matcher::Committed;
use citar_helper::conformance::transcript::{Action, Transcript};
use citar_helper::conformance::{canonical_examples, transcripts};
use citar_helper::http1;
use citar_helper::jobs::{JobSetup, JobState, Snapshot};
use citar_helper::key::MachineKey;
use citar_helper::llm::{BoxFuture, Endpoint, HttpEndpoint, Listed, SharedEndpoint};
use citar_helper::log::Log;
use citar_helper::power::Reading;
use citar_helper::protocol::{Kind, Provider, Sender, UpdatePolicy};
use citar_helper::proxy::InFlight;
use citar_helper::quiet::{self, Quiet};
use citar_helper::session::{Exit, Facts, Helper, Sensors};
use common::{
    FakeEndpoint, FakeHosts, FixedPower, FixedScanner, Models, Pipe, Player, UnderTest,
    held_result, listed, test_scan,
};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// The packages whose helper-side transcripts this crate plays (it refuses what it cannot do).
const PLAYED_THROUGH: &str = "4-13";

/// The fake clock's start: Wednesday 2026-10-14, 12:00 UTC, outside any transcript's quiet hours.
const START_UNIX: f64 = 1_791_979_200.0;

struct InProcess {
    task: Option<tokio::task::JoinHandle<Exit>>,
    log: Log,
    clock: Clock,
    quiet: Quiet,
    in_flight: InFlight,
    /// What Ctrl-C or the service manager would set.
    stop: watch::Sender<bool>,
    /// The fake job host, and what the helper's job side holds.
    hosts: FakeHosts,
    jobs: JobState,
    /// The update downloads it made.
    downloads: citar_helper::update::Downloads,
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

    fn in_flight(&self) -> Option<usize> {
        Some(self.in_flight.get())
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
            Some("stop") => {
                let _told = self.stop.send(true);
            }
            other => panic!("this player cannot yet act {other:?} around the helper"),
        }
    }

    fn jobs(&self) -> Option<Snapshot> {
        Some(self.jobs.snapshot())
    }

    fn host(
        &mut self,
        step: &serde_json::Map<String, Value>,
        golden: &Committed,
    ) -> Result<(), String> {
        self.hosts.step(step, golden)
    }

    fn downloads(&self) -> Option<usize> {
        Some(self.downloads.get())
    }
}

/// The live runs' endpoint: the transcript's model list as given (labels and all: LM Studio's own
/// API has none, and the listings over HTTP are `tests/endpoint.rs`'s), and every completion over
/// HTTP to the fake endpoint.
struct Live {
    models: Models,
    http: HttpEndpoint,
}

impl Endpoint for Live {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Listed>, String>> {
        self.models.list()
    }

    fn complete(&self, body: Vec<u8>) -> BoxFuture<'_, Result<http1::Response, http1::Failure>> {
        self.http.complete(body)
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

/// The in-process helper's scanner and meters: a fixed scan, a fixed reading.
fn sensors() -> (Sensors, FixedScanner) {
    let scanner = FixedScanner::new(test_scan("Transcript CPU"));
    let sensors = Sensors {
        scanner: Some(Arc::new(scanner.clone())),
        power: Some(Arc::new(FixedPower(Reading { gpu_w: Some(42.5), cpu_util: Some(0.25) }))),
        cache: None,
    };
    (sensors, scanner)
}

/// Plays `t` with the helper's settings changed by `tweak` (for settings no transcript key names).
fn play_with(
    t: &Transcript,
    canonical: &Canonical,
    tweak: impl FnOnce(&mut Config),
) -> Result<(), String> {
    play_sensed(t, canonical, tweak, &[], sensors().0)
}

/// Whether a transcript runs a completion (a `request` sent, or an endpoint step beyond its model
/// list): it then plays in real time against the fake endpoint over HTTP.
fn runs_completions(t: &Transcript) -> bool {
    t.steps.iter().any(|s| match &s.action {
        Action::Frame { from: Sender::Server, pattern } => {
            pattern.get("t").and_then(Value::as_str) == Some("request")
        }
        Action::Endpoint(e) => e.keys().any(|k| k != "models"),
        _ => false,
    })
}

/// Plays `t` with `binds` bound before the first step, so an `<id:…>` the transcript leaves open
/// must be exactly that value.
fn play_bound(
    t: &Transcript,
    canonical: &Canonical,
    tweak: impl FnOnce(&mut Config),
    binds: &[(&str, Value)],
) -> Result<(), String> {
    play_sensed(t, canonical, tweak, binds, sensors().0)
}

/// [`play_bound`] with the helper's scanner and meters given.
fn play_sensed(
    t: &Transcript,
    canonical: &Canonical,
    tweak: impl FnOnce(&mut Config),
    binds: &[(&str, Value)],
    sensors: Sensors,
) -> Result<(), String> {
    for key in t.setup_helper.keys() {
        if !["config", "clock", "service", "endpoint", "host", "home", "update_key"]
            .contains(&key.as_str())
        {
            return Err(format!("{}: this player cannot yet set up the helper's {key:?}", t.name));
        }
    }
    // The run's throwaway update key, which the helper trusts (`update_key: "test"`); without
    // it the helper trusts none, as a build of this tree does (`update-keys.pub` is empty).
    let update_key = match t.setup_helper.get("update_key").and_then(Value::as_str) {
        None => None,
        Some("test") => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).expect("the OS's random source");
            Some(citar_jobs::claim::SigningKey::from_bytes(&seed))
        }
        Some(other) => {
            return Err(format!("{}: setup.helper.update_key {other:?} is not known", t.name));
        }
    };
    let hosts = FakeHosts::from_setup(t.setup_helper.get("host"))?;
    // Its home's results folder, with what a previous run of the helper left there.
    let results = std::env::temp_dir().join(format!(
        "citar-helper-conf-{}-{}-{}",
        t.name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _fresh = std::fs::remove_dir_all(&results);
    if let Some(home) = t.setup_helper.get("home").and_then(Value::as_object) {
        for (k, v) in home {
            match k.as_str() {
                "results" => {
                    std::fs::create_dir_all(&results).map_err(|e| e.to_string())?;
                    let golden = Committed::load()?;
                    for r in v.as_array().into_iter().flatten() {
                        let name = r
                            .as_str()
                            .and_then(|s| s.strip_prefix("<id:"))
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or_default();
                        let run = t.values.get(name).and_then(Value::as_str).ok_or_else(|| {
                            format!("{}: setup.helper.home.results names no value {r}", t.name)
                        })?;
                        let doc = held_result(run, &golden, &t.values);
                        std::fs::write(results.join(format!("{run}.json")), doc.to_string())
                            .map_err(|e| e.to_string())?;
                    }
                }
                other => {
                    return Err(format!(
                        "{}: this player cannot yet set up the helper's home {other:?}",
                        t.name
                    ));
                }
            }
        }
    }
    let live = runs_completions(t);
    let rt = if live {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()
    } else {
        tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build()
    }
    .map_err(|e| e.to_string())?;
    let facts = facts();
    rt.block_on(async {
        let mut cfg = config(t);
        let models = Models::default();
        if let Some(m) = t.setup_helper.get("endpoint").and_then(|e| e.get("models")) {
            models.set(listed(m));
        }
        let fake = if live { Some(FakeEndpoint::start(models.clone()).await) } else { None };
        if let Some(f) = &fake {
            cfg.base_url = f.base_url();
        }
        tweak(&mut cfg);
        let (conns_tx, conns_rx) = mpsc::unbounded_channel();
        let key = Arc::new(MachineKey::generate()?);
        let log = Log::memory();
        // In real time too, the fake wall clock starts at START_UNIX and can jump.
        let clock = Clock::fake(START_UNIX);
        let (stop_tx, stop_rx) = watch::channel(false);
        let endpoint: Option<SharedEndpoint> = match (&fake, cfg.no_llm) {
            (_, true) => None,
            (None, false) => Some(Arc::new(models.clone())),
            (Some(_), false) => {
                let tls = citar_helper::net::verified_tls(None)?;
                let http = HttpEndpoint::new(cfg.provider, &cfg.base_url, None, tls)?;
                Some(Arc::new(Live { models: models.clone(), http }))
            }
        };
        let ceiling = cfg.ceiling(Machine { logical_cpus: 8, memory_mb: 16_384 });
        let jobs = JobState::default();
        let quiet = Quiet(cfg.quiet.clone());
        let in_flight = InFlight::default();
        let mut player = Player::new(t, conns_rx, models, canonical.clone());
        player.endpoint = fake;
        player.update_key = update_key.clone();
        player.update_for = (facts.version.clone(), facts.target.clone(), facts.build.clone());
        let downloads = citar_helper::update::Downloads::default();
        let update = citar_helper::update::UpdateSetup {
            keys: update_key.iter().map(citar_jobs::claim::SigningKey::verifying_key).collect(),
            downloads: downloads.clone(),
            ..Default::default()
        };
        player.bindings.bind("token", Value::String(cfg.token.clone()));
        player.bindings.bind("key", Value::String(key.public_b64()));
        for (name, v) in binds {
            player.bindings.bind(name, v.clone());
        }
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
            in_flight: in_flight.clone(),
            sensors,
            jobs: JobSetup {
                // A transcript named `jobless-…` plays a helper with no job hosts (no jobs build).
                hosts: (!t.name.starts_with("jobless-"))
                    .then(|| Arc::new(hosts.clone()) as Arc<dyn citar_helper::jobs::Hosts>),
                key: Some(Arc::clone(&key)),
                results: Some(results.clone()),
                state: jobs.clone(),
                ..Default::default()
            },
            update,
        };
        let task = tokio::spawn(async move { helper.run().await });
        let mut under = InProcess {
            task: Some(task),
            log: log.clone(),
            clock,
            quiet,
            in_flight,
            stop: stop_tx,
            hosts,
            jobs,
            downloads,
        };
        let result = player.play(&mut under).await;
        if let Some(task) = under.task.take() {
            task.abort();
        }
        let _cleaned = std::fs::remove_dir_all(&results);
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
    assert_eq!(
        mine.len(),
        79,
        "4-03, 4-04, 4-06, 4-10 and 4-13 play 79 transcripts from the helper's side"
    );
    assert_eq!(mine.iter().filter(|t| t.plays.helper.as_deref() == Some("4-13")).count(), 4);
    let (live, paused): (Vec<&Transcript>, Vec<&Transcript>) =
        mine.iter().partition(|t| runs_completions(t));
    assert_eq!(live.len(), 15, "4-04's 15 run completions");
    assert!(live.iter().all(|t| t.plays.helper.as_deref() == Some("4-04")));
    let mut failures = Vec::new();
    for t in &paused {
        if let Err(e) = play(t, &canonical) {
            failures.push(e);
        }
    }
    // The live ones wait in real time (a second here, two there): side by side.
    let canonical = &canonical;
    std::thread::scope(|scope| {
        let runs: Vec<_> = live.iter().map(|t| scope.spawn(move || play(t, canonical))).collect();
        for r in runs {
            if let Err(e) = r.join().expect("a transcript's thread") {
                failures.push(e);
            }
        }
    });
    assert!(
        failures.is_empty(),
        "{} of {} failed:\n\n{}",
        failures.len(),
        mine.len(),
        failures.join("\n\n")
    );
}

#[test]
fn the_quiet_hours_status_says_when_they_end() {
    // `status-quiet-hours` leaves `quiet_until` open; bound here, the status the helper sends when
    // 22:00-06:00 begins (Wednesday 22:00 on the fake clock, which reads UTC) must carry
    // Thursday 2026-10-15 06:00 UTC exactly.
    let canonical = canonical_examples().expect("the canonical examples read");
    let all = transcripts().expect("the transcripts load");
    let t = all.iter().find(|t| t.name == "status-quiet-hours").expect("the transcript");
    let thursday_six = START_UNIX + 18.0 * 3600.0;
    play_bound(t, &canonical, |_| {}, &[("quiet_until", Value::from(thursday_six))])
        .expect("the status carries the next 06:00");
    let e = play_bound(t, &canonical, |_| {}, &[("quiet_until", Value::from(thursday_six + 60.0))])
        .expect_err("a minute off is not it");
    assert!(e.contains("quiet_until"), "{e}");
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

#[test]
fn a_helper_without_job_hosts_refuses_offers_and_bad_requests_as_the_protocol_says() {
    let canonical = canonical_examples().expect("the canonical examples read");
    let request = |id: &str, messages: Value| {
        serde_json::json!({"t": "request", "id": id, "model": "model-a", "messages": messages,
                           "tools": null, "params": {}, "timeout_s": 60})
    };
    let offer = |run: &str, spec_hash: &str| {
        let mut o =
            canonical[&(citar_helper::protocol::Sender::Server, "job_offer".to_owned())].clone();
        o["run"] = Value::from(run);
        o["spec_hash"] = Value::from(spec_hash);
        o.as_object_mut().expect("an object").remove("ts");
        o
    };
    let mut v: Value = serde_json::from_str(
        &std::fs::read_to_string(
            citar_helper::conformance::fixtures_dir()
                .join("transcripts")
                .join("handshake-welcome.json"),
        )
        .expect("read"),
    )
    .expect("json");
    v["name"] = Value::from("jobless-refusals");
    let steps = v["steps"].as_array_mut().expect("steps");
    steps.extend([
        // A request whose id can be read but that fails its checks: bad_request (the
        // completions themselves are the transcripts' and tests/proxy.rs's).
        serde_json::json!({"s2h": request("r2", serde_json::json!([])), "invalid": "value"}),
        serde_json::json!({"h2s": {"t": "error", "id": "r2", "code": "bad_request", "retryable": false}, "within_s": 2}),
        // An offer: no job hosts, no jobs build, so none is this helper's build.
        serde_json::json!({"s2h": offer("cr_one", &"6".repeat(64))}),
        serde_json::json!({"h2s": {"t": "job_reject", "run": "cr_one", "reason": "build"}, "within_s": 2}),
        // An offer whose run can be read but that fails its checks: bad_spec.
        serde_json::json!({"s2h": offer("cr_two", "not-a-hash"), "invalid": "value"}),
        serde_json::json!({"h2s": {"t": "job_reject", "run": "cr_two", "reason": "bad_spec"}, "within_s": 2}),
        // A cancel for nothing in flight, and an update before 4-13: nothing is answered.
        serde_json::json!({"s2h": {"t": "cancel", "id": "r9"}}),
        serde_json::json!({"wait": {"s": 3, "quiet": true}}),
    ]);
    let t = Transcript::from_value(&v).expect("a transcript");
    play(&t, &canonical).expect("refused as the protocol says");
}
