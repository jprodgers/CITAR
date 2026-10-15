//! The hardware report and the power samples beyond the shared transcripts (design/phase4-helper.md
//! P4.3.5; package 4-06), driven in-process against a fake server written here, on tokio's paused
//! clock (six hours take no time):
//! - a hello carries the machine's scan; a scan six hours later is sent only when the machine
//!   changed;
//! - a slow scan lets the hello go without it, and its `hardware` frame follows;
//! - the last scan is kept in `helper/scan.json` and serves a restart for six hours, and no longer;
//! - one `power` frame a minute with the minute's readings, none with `--no-power` or when nothing
//!   could be measured;
//! - a `rescan` of a helper reporting no hardware sends nothing.
//!
//! And, in a development build, the machine's own meters and scan against `citar-fake-probe`
//! copied in as `nvidia-smi` (and as PowerShell and `system_profiler`): the draw it prints, each
//! call in its log, and a scan whose hash does not move when nothing changed.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::conformance::canonical_examples;
use citar_helper::key::MachineKey;
use citar_helper::llm::BoxFuture;
use citar_helper::log::Log;
use citar_helper::power::{PowerProbe, Reading};
use citar_helper::protocol::{Provider, Sender, UpdatePolicy};
use citar_helper::scan::{Runtime, Scanner};
use citar_helper::session::{Exit, Facts, Helper, Sensors};
use common::{FixedPower, FixedScanner, Pipe, ServerWs, test_scan};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// The fake clock's start: Wednesday 2026-10-14, 12:00 UTC.
const START_UNIX: f64 = 1_791_979_200.0;
const SIX_HOURS: f64 = 6.0 * 3600.0;

fn config() -> Config {
    Config {
        server_url: "http://127.0.0.1:1".to_owned(),
        token: "cwt_scan_test_0123456789".to_owned(),
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
        hostname: "scan-test".to_owned(),
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

struct Run {
    conns: mpsc::UnboundedReceiver<ServerWs>,
    log: Log,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Exit>,
}

impl Run {
    async fn stopped(self) {
        let _stop = self.stop.send(true);
        assert_eq!(self.task.await.expect("joined"), Exit::Stopped);
    }
}

fn start(cfg: Config, sensors: Sensors, clock: Clock) -> Run {
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
        clock,
        dialer: Arc::new(Pipe(tx)),
        endpoint: None,
        service: false,
        stop: stop_rx,
        in_flight: Default::default(),
        sensors,
    };
    let task = tokio::spawn(async move { helper.run().await });
    Run { conns, log, stop, task }
}

fn text(m: &Message) -> Option<Value> {
    match m {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

/// The next connection, its hello (and when it came), a welcome sent and the acknowledgement read.
async fn welcome(run: &mut Run) -> (ServerWs, Value, f64) {
    let t0 = Instant::now();
    let mut ws = tokio::time::timeout(Duration::from_secs(120), run.conns.recv())
        .await
        .expect("a connection in time")
        .expect("a connection");
    let hello = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    let at = t0.elapsed().as_secs_f64();
    assert_eq!(hello["t"], "hello", "{hello}");
    let canonical = canonical_examples().expect("examples");
    let mut w = canonical[&(Sender::Server, "welcome".to_owned())].clone();
    w["jobs"] = json!({"keep": [], "drop": [], "resend": []});
    w["ping_s"] = json!(20);
    ws.send(Message::Text(w.to_string().into())).await.expect("welcome sent");
    let ack = text(&ws.next().await.expect("a frame").expect("a frame")).expect("text");
    assert_eq!(ack["t"], "settings_ack", "{ack}");
    (ws, hello, at)
}

/// Every frame but pings and pongs the helper sends for `secs` (answering its pings, and pinging
/// it as a server does every 20 s), with when it came.
async fn frames_for(ws: &mut ServerWs, secs: f64) -> Vec<(f64, Value)> {
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(secs);
    let mut next_ping = t0 + Duration::from_secs(20);
    let mut out = Vec::new();
    loop {
        let wake = until.min(next_ping);
        match tokio::time::timeout_at(wake, ws.next()).await {
            Err(_) if Instant::now() >= until => break,
            Err(_) => {
                ws.send(Message::Text(r#"{"t": "ping"}"#.into())).await.expect("ping");
                next_ping += Duration::from_secs(20);
            }
            Ok(Some(Ok(m))) => {
                if let Some(v) = text(&m) {
                    if v["t"] == "ping" {
                        ws.send(Message::Text(r#"{"t": "pong"}"#.into())).await.expect("pong");
                    } else if v["t"] != "pong" {
                        out.push((t0.elapsed().as_secs_f64(), v));
                    }
                }
            }
            Ok(_) => panic!("the helper ended the connection"),
        }
    }
    out
}

fn of_type<'a>(frames: &'a [(f64, Value)], t: &str) -> Vec<&'a (f64, Value)> {
    frames.iter().filter(|(_, v)| v["t"] == t).collect()
}

fn paused() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a runtime")
}

/// A scanner whose scans find these machines in turn (the last one from then on).
#[derive(Clone)]
struct Sequence {
    scans: Arc<Mutex<Vec<Map<String, Value>>>>,
    made: Arc<Mutex<usize>>,
    /// How long each scan takes.
    takes: Duration,
}

impl Sequence {
    fn new(scans: Vec<Map<String, Value>>, takes: Duration) -> Sequence {
        Sequence { scans: Arc::new(Mutex::new(scans)), made: Arc::default(), takes }
    }

    fn made(&self) -> usize {
        *self.made.lock().expect("made")
    }
}

impl Scanner for Sequence {
    fn scan(&self, _runtimes: Vec<Runtime>) -> BoxFuture<'static, Option<Map<String, Value>>> {
        *self.made.lock().expect("made") += 1;
        let mut scans = self.scans.lock().expect("scans");
        let scan = if scans.len() > 1 { scans.remove(0) } else { scans[0].clone() };
        let takes = self.takes;
        Box::pin(async move {
            tokio::time::sleep(takes).await;
            Some(scan)
        })
    }
}

fn scanning(scanner: impl Scanner + 'static) -> Sensors {
    Sensors { scanner: Some(Arc::new(scanner)), power: None, cache: None }
}

#[test]
fn the_hello_carries_the_scan_and_a_later_scan_is_sent_only_when_the_machine_changed() {
    paused().block_on(async {
        let a = test_scan("CPU A");
        let b = test_scan("CPU B");
        let seq = Sequence::new(vec![a.clone(), a.clone(), b.clone()], Duration::ZERO);
        let mut run = start(config(), scanning(seq.clone()), Clock::fake(START_UNIX));
        let (mut ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"]["hardware_source"], "scan");
        assert_eq!(hello["report"]["hardware"]["cpu"]["model"], "CPU A");
        assert_eq!(seq.made(), 1);
        // Six hours on, the machine is scanned again: the same machine, no frame.
        let frames = frames_for(&mut ws, SIX_HOURS + 60.0).await;
        assert_eq!(seq.made(), 2, "scanned again after six hours");
        assert!(of_type(&frames, "hardware").is_empty(), "{frames:?}");
        // Six more: it changed, and the frame comes at once.
        let frames = frames_for(&mut ws, SIX_HOURS).await;
        assert_eq!(seq.made(), 3);
        let hw = of_type(&frames, "hardware");
        assert_eq!(hw.len(), 1, "{frames:?}");
        assert_eq!(hw[0].1["source"], "scan");
        assert_eq!(hw[0].1["scan"]["cpu"]["model"], "CPU B");
        assert!((SIX_HOURS - 61.0..SIX_HOURS - 59.0).contains(&hw[0].0), "at {}", hw[0].0);
        assert!(run.log.contains("hardware scan changed"));
        // A rescan sends it again, unchanged as it is.
        ws.send(Message::Text(r#"{"t": "rescan"}"#.into())).await.expect("rescan");
        let frames = frames_for(&mut ws, 2.0).await;
        assert_eq!(of_type(&frames, "hardware").len(), 1, "{frames:?}");
        assert_eq!(seq.made(), 4);
        run.stopped().await;
    });
}

#[test]
fn a_slow_scan_lets_the_hello_go_and_its_frame_follows() {
    paused().block_on(async {
        let seq = Sequence::new(vec![test_scan("Slow CPU")], Duration::from_secs(45));
        let mut run = start(config(), scanning(seq.clone()), Clock::fake(START_UNIX));
        let (mut ws, hello, at) = welcome(&mut run).await;
        assert!((30.0..31.0).contains(&at), "the hello waited {at} s");
        assert_eq!(hello["report"], json!({"hardware": null, "hardware_source": "none"}));
        assert!(run.log.contains("connecting without it"));
        let frames = frames_for(&mut ws, 20.0).await;
        let hw = of_type(&frames, "hardware");
        assert_eq!(hw.len(), 1, "{frames:?}");
        assert_eq!(hw[0].1["scan"]["cpu"]["model"], "Slow CPU");
        assert!((14.0..16.0).contains(&hw[0].0), "the scan's frame at {} s", hw[0].0);
        assert_eq!(seq.made(), 1, "one scan, not one per wait");
        run.stopped().await;
    });
}

#[test]
fn the_last_scan_serves_a_restart_for_six_hours_and_no_longer() {
    paused().block_on(async {
        let dir = common::scratch("scan-cache");
        let cache = dir.join("helper").join("scan.json");
        let sensed = |seq: &Sequence| Sensors {
            scanner: Some(Arc::new(seq.clone())),
            power: None,
            cache: Some(cache.clone()),
        };
        // The first run scans, and keeps it.
        let first = Sequence::new(vec![test_scan("Kept CPU")], Duration::ZERO);
        let mut run = start(config(), sensed(&first), Clock::fake(START_UNIX));
        let (_ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"]["hardware"]["cpu"]["model"], "Kept CPU");
        run.stopped().await;
        let kept: Value =
            serde_json::from_slice(&std::fs::read(&cache).expect("scan.json")).expect("JSON");
        assert_eq!(kept["v"], 1);
        assert_eq!(kept["scan"]["cpu"]["model"], "Kept CPU");
        let scan: Map<String, Value> = serde_json::from_value(kept["scan"].clone()).expect("a map");
        assert_eq!(kept["hash"], citar_helper::scan::hash(&scan));
        assert!(!dir.join("helper").join("scan.json.tmp").exists());

        // Five hours later, a restart reports it without scanning.
        let second = Sequence::new(vec![test_scan("New CPU")], Duration::ZERO);
        let mut run = start(config(), sensed(&second), Clock::fake(START_UNIX + 5.0 * 3600.0));
        let (_ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"]["hardware"]["cpu"]["model"], "Kept CPU");
        assert_eq!(second.made(), 0);
        run.stopped().await;

        // Seven hours later, it scans.
        let third = Sequence::new(vec![test_scan("New CPU")], Duration::ZERO);
        let mut run = start(config(), sensed(&third), Clock::fake(START_UNIX + 7.0 * 3600.0));
        let (_ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"]["hardware"]["cpu"]["model"], "New CPU");
        assert_eq!(third.made(), 1);
        run.stopped().await;

        // A file that does not read, or whose hash is not its scan's, is not used.
        for broken in [
            "not json".to_owned(),
            json!({"v": 1, "at": START_UNIX + 7.0 * 3600.0, "hash": "0", "scan": test_scan("Bad")})
                .to_string(),
        ] {
            std::fs::write(&cache, broken).expect("written");
            let fourth = Sequence::new(vec![test_scan("Fresh CPU")], Duration::ZERO);
            let mut run = start(config(), sensed(&fourth), Clock::fake(START_UNIX + 7.5 * 3600.0));
            let (_ws, hello, _) = welcome(&mut run).await;
            assert_eq!(hello["report"]["hardware"]["cpu"]["model"], "Fresh CPU");
            assert_eq!(fourth.made(), 1);
            run.stopped().await;
        }
        std::fs::remove_dir_all(&dir).expect("cleaned");
    });
}

/// A meter that measures nothing.
struct Blind;

impl PowerProbe for Blind {
    fn read(&self) -> BoxFuture<'static, Option<Reading>> {
        Box::pin(async { None })
    }
}

#[test]
fn power_goes_once_a_minute_with_what_could_be_measured() {
    paused().block_on(async {
        let reading = Reading { gpu_w: None, cpu_util: Some(0.5) };
        let sensors =
            Sensors { scanner: None, power: Some(Arc::new(FixedPower(reading))), cache: None };
        let mut run = start(config(), sensors.clone(), Clock::fake(START_UNIX));
        let (mut ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"]["hardware_source"], "none", "no scanner, no scan");
        let frames = frames_for(&mut ws, 125.0).await;
        let power = of_type(&frames, "power");
        assert_eq!(power.len(), 2, "{frames:?}");
        for (k, (at, f)) in power.iter().enumerate() {
            let samples = f["samples"].as_array().expect("samples");
            assert_eq!(samples.len(), 6, "{f}");
            assert!((*at - 60.0 * (k as f64 + 1.0)).abs() < 1.0, "frame {k} at {at}");
            let ts: Vec<f64> = samples.iter().map(|s| s["t"].as_f64().expect("t")).collect();
            for w in ts.windows(2) {
                assert!((w[1] - w[0] - 10.0).abs() < 0.01, "{ts:?}");
            }
            assert!(samples.iter().all(|s| s["gpu_w"].is_null() && s["cpu_util"] == 0.5));
        }
        run.stopped().await;

        // --no-power: no meter at all.
        let mut cfg = config();
        cfg.no_power = true;
        let mut run = start(cfg, sensors, Clock::fake(START_UNIX));
        let (mut ws, _, _) = welcome(&mut run).await;
        let frames = frames_for(&mut ws, 125.0).await;
        assert!(of_type(&frames, "power").is_empty(), "{frames:?}");
        run.stopped().await;

        // Nothing measurable: no frame, and the log says so.
        let blind = Sensors { scanner: None, power: Some(Arc::new(Blind)), cache: None };
        let mut run = start(config(), blind, Clock::fake(START_UNIX));
        let (mut ws, _, _) = welcome(&mut run).await;
        let frames = frames_for(&mut ws, 125.0).await;
        assert!(of_type(&frames, "power").is_empty(), "{frames:?}");
        assert!(run.log.contains("no power reading could be taken this minute"));
        run.stopped().await;
    });
}

#[test]
fn a_rescan_of_a_helper_that_reports_no_hardware_sends_nothing() {
    paused().block_on(async {
        let mut cfg = config();
        cfg.collect_hardware = false;
        let scanner = FixedScanner::new(test_scan("Unasked CPU"));
        let sensors =
            Sensors { scanner: Some(Arc::new(scanner.clone())), power: None, cache: None };
        let mut run = start(cfg, sensors, Clock::fake(START_UNIX));
        let (mut ws, hello, _) = welcome(&mut run).await;
        assert_eq!(hello["report"], json!({"hardware": null, "hardware_source": "none"}));
        ws.send(Message::Text(r#"{"t": "rescan"}"#.into())).await.expect("rescan");
        let frames = frames_for(&mut ws, SIX_HOURS + 60.0).await;
        assert!(of_type(&frames, "hardware").is_empty(), "{frames:?}");
        assert_eq!(scanner.made(), 0, "--no-hardware never scans");
        assert!(run.log.contains("reports no hardware (--no-hardware)"));
        run.stopped().await;
    });
}

/// The machine's own meters and scan against `citar-fake-probe`, which only a development build
/// makes.
#[cfg(feature = "dev-guard")]
mod fake_probe {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use citar_helper::power::SystemPower;
    use citar_helper::scan::{self, Probes};

    use super::*;

    const FAKE: &str = env!("CARGO_BIN_EXE_citar-fake-probe");

    /// The fake copied into `dir` as `name` (an `.exe` on Windows): its log is `<copy>.log`.
    fn copied(dir: &Path, name: &str) -> PathBuf {
        let to = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(FAKE, &to).expect("the fake copied");
        to
    }

    fn calls(probe: &Path) -> Vec<String> {
        let mut log = probe.as_os_str().to_owned();
        log.push(".log");
        std::fs::read_to_string(PathBuf::from(log))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once(' ').map_or("", |(_, args)| args).to_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_meters_read_the_draw_the_fake_prints_and_every_call_is_logged() {
        let dir = common::scratch("fake-meter");
        let nvidia = copied(&dir, "nvidia-smi");
        let probes = Probes { nvidia_smi: OsString::from(&nvidia), ..Probes::system() };
        let power = SystemPower::new(probes);
        for _ in 0..3 {
            let r = power.read().await.expect("a reading");
            assert_eq!(r.gpu_w, Some(123.45));
            assert!(r.cpu_util.is_some_and(|u| (0.0..=1.0).contains(&u)), "{r:?}");
        }
        let calls = calls(&nvidia);
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls.iter().all(|c| c == "--query-gpu=power.draw --format=csv,noheader,nounits"));
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_scan_with_the_fakes_holds_their_gpu_and_hashes_the_same_twice() {
        let dir = common::scratch("fake-scan");
        let probes = Probes {
            nvidia_smi: copied(&dir, "nvidia-smi").into_os_string(),
            powershell: copied(&dir, "powershell").into_os_string(),
            system_profiler: copied(&dir, "system_profiler").into_os_string(),
        };
        let one = scan::collect(&probes, &[], "2026-10-15T06:00:00");
        let runtimes = [Runtime {
            name: "LM Studio".into(),
            endpoint: "http://127.0.0.1:1234/v1".into(),
            models: vec![("model-a".into(), Some(8192))],
        }];
        let two = scan::collect(&probes, &runtimes, "2026-10-15T12:00:00");
        let v = Value::Object(one.clone());
        assert_eq!(
            (v["format"].as_str(), v["collector"].as_str()),
            (Some("citar-hardware"), Some("citar-helper"))
        );
        assert_eq!(v["collector_version"], 2);
        assert_eq!(
            v["gpus"][0],
            json!({"name": "CITAR Fake GPU", "vendor": "NVIDIA", "vram_gb": 24.0,
            "driver": "999.99", "power_limit_w": 300.0, "power_now_w": 123.45,
            "bus": "00000000:01:00.0", "power_readable": true})
        );
        assert_eq!(v["power"]["gpu_sampling"], true);
        assert_eq!(v["power"]["suggested"]["source"], "guess");
        assert!(v["cpu"]["threads"].as_u64().is_some_and(|t| t >= 1));
        assert!(v["memory"]["ram_gb"].as_f64().is_some_and(|r| r > 0.0));
        assert_eq!(two["runtimes"][0]["models"], json!([{"key": "model-a", "max_context": 8192}]));
        let mut two_without = two.clone();
        two_without.insert("runtimes".into(), json!([]));
        assert_eq!(scan::hash(&one), scan::hash(&two_without), "the same machine an hour later");
        assert_ne!(scan::hash(&one), scan::hash(&two), "a runtime's models are the machine's");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
