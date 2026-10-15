//! The real binary against the transcript server on loopback (package 4-03, gates 3 and 5), in
//! real time: it connects, sends its key (whose fingerprint `--version` prints), acknowledges the
//! site's settings with its own ceiling, reports a fake LM Studio's models and a change of the
//! loaded set within 35 s, and on `shutdown` `revoked` exits 78 without reconnecting. A run with
//! `--home` leaves everything outside that home as it was; a development or test build
//! (`dev-guard`, or any test feature alone) refuses a non-loopback server, a missing `--home` and
//! one inside the per-user base.
//!
//! And (4-04) the binary serves completions as the transcripts say, against `common`'s fake
//! endpoint over HTTP: a streamed tool call assembled raw, a cancel mid-stream that closes the
//! endpoint's connection within a second, a timeout, an unstreamed answer, and parameters outside
//! the five dropped.
//!
//! And (4-06, in a development build) the binary reports its own scan, made with
//! `citar-fake-probe` standing in for `nvidia-smi`, PowerShell and `system_profiler`, and sends
//! it again within 5 s of a `rescan`; with `--hardware` it reports a UTF-16 file, again on
//! `rescan`; and `citar-helper scan` reports the endpoint and allowlist of the helper's own
//! settings. The other runs here take `--no-hardware --no-power`: what they check is not the
//! machine's report, and none of them runs the machine's own probe programs.
//!
//! Every run here has its own home under the system's temporary directory, `CITAR_DATA_DIR`
//! pointing at it too, and the `CITAR_WORKER_*` variables removed, so nothing of a real
//! installation is read or written (P4.7.1).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use citar_helper::conformance::canonical_examples;
use citar_helper::conformance::transcript::Transcript;
use citar_helper::key::fingerprint_of;
use citar_helper::llm::{BoxFuture, Listed};
use common::{
    FakeEndpoint, Models, Player, UnderTest, listed as listed_of, loopback_hub, loopback_lmstudio,
    scratch,
};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");
const TOKEN: &str = "cwt_loopback_0123456789abcdef";

/// The binary, with the variables that could point it at a real installation removed.
fn command(cwd: &Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(BIN);
    c.current_dir(cwd).kill_on_drop(true);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("CITAR_WORKER_") || k == "CITAR_DATA_DIR" {
            c.env_remove(k);
        }
    }
    c
}

struct Process {
    child: tokio::process::Child,
    log: PathBuf,
}

impl UnderTest for Process {
    fn exited(&mut self, within: Duration) -> BoxFuture<'_, Option<i32>> {
        Box::pin(async move {
            match tokio::time::timeout(within, self.child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => None,
            }
        })
    }

    fn logged(&self, text: &str) -> bool {
        std::fs::read_to_string(&self.log).is_ok_and(|l| l.contains(text))
    }

    fn act(&mut self, what: &Value) {
        panic!("the real binary's run cannot act {what}");
    }
}

fn models_json(models: &[Listed]) -> Value {
    Value::Array(
        models
            .iter()
            .map(|m| json!({"key": m.key, "label": m.label, "context": m.context, "loaded": m.loaded}))
            .collect(),
    )
}

fn listed(key: &str, context: u64, loaded: bool) -> Listed {
    Listed { key: key.to_owned(), label: key.to_owned(), context: Some(context), loaded }
}

/// The scenario of gate 3, written as a transcript the player plays.
fn scenario(before: &[Listed], after: &[Listed]) -> Transcript {
    let site = json!({
        "llm": {"enabled": false, "slots": 4, "allow_swaps": false},
        "cpu": {"enabled": true, "threads": 8, "cpu_percent": 25, "memory_mb": 0,
                "work": ["lab_game", "ranking_game"], "yield_to_llm": true}
    });
    let t = json!({
        "name": "real-binary-loopback",
        "about": "The real binary on loopback: key, settings capped by its ceiling, catalog on change, shutdown.",
        "refs": ["P4.3.3"],
        "plays": {"helper": "4-03", "hub": null},
        "one_side": "a scenario of the real binary's test, not a shared transcript",
        "values": {"token": TOKEN, "key": "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", "machine": "sv_loopback"},
        "setup": {"helper": {}, "hub": {}},
        "steps": [
            {"h2s": {"t": "hello", "protocol": 2, "token": "<id:token>",
                     "machine": {"public_key": "<id:key>"},
                     "llm": {"enabled": true, "provider": "lmstudio", "slots": 2, "stream": true,
                             "models": models_json(before)},
                     "cpu": {"enabled": false,
                             "ceiling": {"threads": 2, "cpu_percent": 25, "memory_mb": 512, "work": ["lab_game"]}},
                     "running": [], "results": []},
             "within_s": 20},
            {"s2h": {"t": "welcome", "protocol": 2, "machine": {"id": "<id:machine>", "name": "loopback"},
                     "settings": site, "ping_s": 20, "server": {"version": "0.1.6", "build": "<hex12>", "jobs_build": null},
                     "update": null, "jobs": {"keep": [], "drop": [], "resend": []}}},
            {"h2s": {"t": "settings_ack",
                     "effective": {"llm": {"enabled": false, "slots": 2},
                                   "cpu": {"enabled": true, "threads": 2, "cpu_percent": 25, "memory_mb": 512,
                                           "work": ["lab_game"], "yield_to_llm": true}},
                     "capped": ["llm.slots", "cpu.threads", "cpu.work"],
                     "limits": {"cpu": "soft", "memory": "soft"}},
             "within_s": 5},
            {"endpoint": {"models": models_json(after)}},
            {"h2s": {"t": "catalog", "models": models_json(after)}, "within_s": 35},
            {"s2h": {"t": "shutdown", "reason": "the token was revoked", "code": "revoked"}},
            {"close": {"by": "server", "code": 4403}},
            {"state": {"side": "helper", "expect": {"exit": 78}}, "within_s": 10}
        ]
    });
    Transcript::from_value(&t).expect("the scenario is a transcript")
}

/// The files P4.7.5's S10 fingerprints, as they are: size, modification time and a hash of the
/// content, or absent. Read only.
fn s10() -> BTreeMap<String, String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let env = |k: &str| std::env::var_os(k);
    let mut files: Vec<PathBuf> =
        vec![repo.join("config").join("servers.json"), repo.join("config").join("worker.json")];
    if let Some(base) = citar_helper::home::platform_base(&env) {
        for f in [
            "config/servers.json",
            "citar.db",
            "secret_key",
            "helper.json",
            "machine-key.json",
            "helper/machine-key.json",
        ] {
            files.push(base.join(f));
        }
    }
    files
        .into_iter()
        .map(|p| {
            let state = match std::fs::metadata(&p) {
                Err(_) => "absent".to_owned(),
                Ok(m) => {
                    let hash = std::fs::read(&p)
                        .map(|b| blake3::hash(&b).to_hex().to_string())
                        .unwrap_or_default();
                    format!("{} {:?} {hash}", m.len(), m.modified().ok())
                }
            };
            (p.display().to_string(), state)
        })
        .collect()
}

/// Every file under `root` but those under `except`, with its size and modification time.
fn tree(root: &Path, except: &Path) -> BTreeMap<PathBuf, (u64, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.starts_with(except) {
                continue;
            }
            match e.metadata() {
                Ok(m) if m.is_dir() => {
                    out.insert(p.clone(), (0, None));
                    stack.push(p);
                }
                Ok(m) => {
                    out.insert(p, (m.len(), m.modified().ok()));
                }
                Err(_) => {}
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_binary_connects_acknowledges_reports_and_exits_78_on_shutdown() {
    let root = scratch("real");
    let home = root.join("home");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&cwd).expect("cwd");
    // The machine's own ceiling, below the site's figures; CPU lending turned on here.
    std::fs::write(
        home.join("helper.json"),
        json!({"cpu": {"enabled": true, "threads": 2, "cpu_percent": 25, "memory_mb": 512, "work": ["lab_game"]}})
            .to_string(),
    )
    .expect("helper.json");
    let before = vec![listed("model-a", 8192, true)];
    let after = vec![listed("model-a", 8192, false), listed("model-b", 4096, true)];
    let models = Models::default();
    models.set(before.clone());
    let (hub, conns) = loopback_hub().await;
    let endpoint = loopback_lmstudio(models.clone()).await;
    let s10_before = s10();
    let outside_before = tree(&root, &home);

    let child = command(&cwd)
        .args(["--home"])
        .arg(&home)
        .args(["--server", &format!("http://127.0.0.1:{hub}"), "--token", TOKEN])
        .args(["--base-url", &format!("http://127.0.0.1:{endpoint}/v1"), "--max-concurrent", "2"])
        .args(["--no-hardware", "--no-power"])
        .env("CITAR_DATA_DIR", &home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the binary starts");
    let mut helper = Process { child, log: home.join("helper").join("logs").join("helper.log") };
    let t = scenario(&before, &after);
    let mut player = Player::new(&t, conns, models, canonical_examples().expect("examples"));
    player.bindings.bind("token", Value::String(TOKEN.to_owned()));
    let started = tokio::time::Instant::now();
    let played = player.play(&mut helper).await;
    let log = std::fs::read_to_string(&helper.log).unwrap_or_default();
    played.unwrap_or_else(|e| panic!("{e}\n  the helper's log:\n{log}"));
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());

    // The key it sent is the one `--version` names.
    let key =
        player.bindings.get("key").and_then(Value::as_str).expect("the hello's key").to_owned();
    let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&key)
        .expect("base64")
        .try_into()
        .expect("32 bytes");
    let out =
        command(&cwd).arg("--version").arg("--home").arg(&home).output().await.expect("--version");
    let line = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(line.contains(&format!("machine key {}", fingerprint_of(&bytes))), "{line}");
    assert!(!log.contains(TOKEN), "the token reached the log");
    assert!(log.contains("[redacted]") || !log.contains("cwt_"), "{log}");

    // Gate 5: nothing outside the home changed.
    assert_eq!(s10(), s10_before, "the S10 files changed");
    assert_eq!(tree(&root, &home), outside_before, "files outside the home changed");
    std::fs::remove_dir_all(&root).expect("cleaned");
}

/// Run by every build the guard is on in: `dev-guard`, and each test feature alone (P4.3.1).
#[cfg(any(feature = "dev-guard", feature = "test-faults", feature = "test-update-key"))]
mod guard {
    use super::*;

    /// The per-user base, pointed at a scratch directory, so the real one is never even named.
    fn fake_base(root: &Path, c: &mut tokio::process::Command) -> PathBuf {
        let la = root.join("localappdata");
        let h = root.join("userhome");
        let xdg = root.join("xdg");
        c.env("LOCALAPPDATA", &la)
            .env("HOME", &h)
            .env("XDG_DATA_HOME", &xdg)
            .env("USERPROFILE", &h);
        let env = |k: &str| match k {
            "LOCALAPPDATA" => Some(la.clone().into_os_string()),
            "HOME" | "USERPROFILE" => Some(h.clone().into_os_string()),
            "XDG_DATA_HOME" => Some(xdg.clone().into_os_string()),
            _ => None,
        };
        citar_helper::home::platform_base(&env).expect("a base")
    }

    async fn refused(c: &mut tokio::process::Command) -> String {
        let out = tokio::time::timeout(Duration::from_secs(20), c.output())
            .await
            .expect("refused at once")
            .expect("the binary runs");
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    #[tokio::test]
    async fn a_development_build_refuses_what_it_must_before_anything_else() {
        let root = scratch("guard");
        let cwd = root.join("cwd");
        let home = root.join("home");
        std::fs::create_dir_all(&cwd).expect("cwd");

        let mut c = command(&cwd);
        let _base = fake_base(&root, &mut c);
        c.args(["--home"]).arg(&home).args(["--server", "https://example.com", "--token", "t"]);
        let err = refused(&mut c).await;
        assert!(err.contains("loopback"), "{err}");
        assert!(!home.join("helper").exists(), "refused before the key or the log was written");

        let mut c = command(&cwd);
        let _base = fake_base(&root, &mut c);
        c.args(["--server", "http://127.0.0.1:9", "--token", "t"]);
        let err = refused(&mut c).await;
        assert!(err.contains("explicit --home"), "{err}");

        let mut c = command(&cwd);
        let base = fake_base(&root, &mut c);
        c.arg("--home").arg(base.join("inner")).args([
            "--server",
            "http://127.0.0.1:9",
            "--token",
            "t",
        ]);
        let err = refused(&mut c).await;
        assert!(err.contains("inside the per-user CITAR folder"), "{err}");
        assert!(!base.exists(), "nothing was written in the base");
        std::fs::remove_dir_all(&root).expect("cleaned");
    }
}

/// The completion transcripts the binary plays here: none checks the helper's own state (a process
/// does not show its completions in flight) or moves its clock.
const SERVED: &[&str] = &[
    "completion-streamed-tool-call",
    "cancel-mid-stream",
    "completion-timeout",
    "completion-not-streamed",
    "completion-params-whitelisted",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_binary_serves_streams_cancels_and_times_out_completions() {
    let canonical = canonical_examples().expect("examples");
    for name in SERVED {
        let path = citar_helper::conformance::fixtures_dir()
            .join("transcripts")
            .join(format!("{name}.json"));
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
        // LM Studio's own API names no label, so the binary reports a model's key as its label;
        // these runs check the completions, and the hello's labels are the in-process runs'.
        for step in v["steps"].as_array_mut().expect("steps") {
            if step["h2s"]["t"] == "hello" {
                for m in step["h2s"]["llm"]["models"].as_array_mut().expect("models") {
                    m["label"] = Value::from("<string>");
                }
            }
        }
        let t = Transcript::from_value(&v).expect("a transcript");
        let setup = &t.setup_helper;
        assert!(setup.keys().all(|k| k == "config" || k == "endpoint"), "{name}: {setup:?}");
        let root = scratch(&format!("served-{name}"));
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join("helper.json"), setup["config"].to_string()).expect("helper.json");
        let models = Models::default();
        models.set(listed_of(&setup["endpoint"]["models"]));
        let fake = FakeEndpoint::start(models.clone()).await;
        let (hub, conns) = loopback_hub().await;
        let token = t.values["token"].as_str().expect("a token").to_owned();
        let child = command(&root)
            .args(["--home"])
            .arg(&home)
            .args(["--server", &format!("http://127.0.0.1:{hub}"), "--token", &token])
            .args(["--base-url", &fake.base_url()])
            .args(["--no-hardware", "--no-power"])
            .env("CITAR_DATA_DIR", &home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the binary starts");
        let mut helper =
            Process { child, log: home.join("helper").join("logs").join("helper.log") };
        let mut player = Player::new(&t, conns, models, canonical.clone());
        player.endpoint = Some(fake);
        player.bindings.bind("token", Value::String(token));
        let played = player.play(&mut helper).await;
        let _killed = helper.child.kill().await;
        let log = std::fs::read_to_string(&helper.log).unwrap_or_default();
        played.unwrap_or_else(|e| panic!("{e}\n  the helper's log:\n{log}"));
        std::fs::remove_dir_all(&root).expect("cleaned");
    }
}

/// The binary's own scan (4-06), with `citar-fake-probe` for the probe programs: only a
/// development build makes the fake, and only such a build reads the variables that name it.
#[cfg(feature = "dev-guard")]
mod scan {
    use super::*;

    const FAKE: &str = env!("CARGO_BIN_EXE_citar-fake-probe");

    /// `hardware-rescan` with what this run checks of the scans written in: the hello's and the
    /// rescan's come from the binary's scan (its collector, the fake GPU's readable draw) or, with
    /// `--hardware`, from the file.
    fn rescan_transcript(source: &str, report: Value) -> Transcript {
        let path = citar_helper::conformance::fixtures_dir()
            .join("transcripts")
            .join("hardware-rescan.json");
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
        for step in v["steps"].as_array_mut().expect("steps") {
            if step["h2s"]["t"] == "hello" {
                step["h2s"]["report"] =
                    json!({"hardware": report.clone(), "hardware_source": source});
                step["within_s"] = json!(20);
            }
            if step["h2s"]["t"] == "hardware" {
                step["h2s"]["scan"] = report.clone();
                step["h2s"]["source"] = json!(source);
                assert_eq!(step["within_s"], 5, "the transcript's own bound");
            }
        }
        Transcript::from_value(&v).expect("a transcript")
    }

    async fn play_rescan(name: &str, extra: &[&str], t: &Transcript) -> PathBuf {
        let root = scratch(name);
        let home = root.join("home");
        let probes = root.join("probes");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&probes).expect("probes");
        let fake = |n: &str| {
            let to = probes.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
            std::fs::copy(FAKE, &to).expect("the fake copied");
            to
        };
        std::fs::write(home.join("helper.json"), t.setup_helper["config"].to_string())
            .expect("helper.json");
        let (hub, conns) = loopback_hub().await;
        let token = t.values["token"].as_str().expect("a token").to_owned();
        let child = command(&root)
            .args(["--home"])
            .arg(&home)
            .args(["--server", &format!("http://127.0.0.1:{hub}"), "--token", &token])
            .args(["--base-url", "http://127.0.0.1:9/v1"])
            .args(extra)
            .env("CITAR_DATA_DIR", &home)
            .env("CITAR_HELPER_NVIDIA_SMI", fake("nvidia-smi"))
            .env("CITAR_HELPER_POWERSHELL", fake("powershell"))
            .env("CITAR_HELPER_SYSTEM_PROFILER", fake("system_profiler"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the binary starts");
        let mut helper =
            Process { child, log: home.join("helper").join("logs").join("helper.log") };
        let mut player =
            Player::new(t, conns, Models::default(), canonical_examples().expect("examples"));
        player.bindings.bind("token", Value::String(token));
        let played = player.play(&mut helper).await;
        let _killed = helper.child.kill().await;
        let log = std::fs::read_to_string(&helper.log).unwrap_or_default();
        played.unwrap_or_else(|e| panic!("{e}\n  the helper's log:\n{log}"));
        root
    }

    fn calls(probe: &Path) -> Vec<String> {
        std::fs::read_to_string(probe)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once(' ').map_or("", |(_, a)| a).to_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_binary_reports_its_scan_and_sends_it_again_within_five_seconds_of_a_rescan() {
        let report = json!({"format": "citar-hardware", "collector": "citar-helper",
                            "collector_version": 2, "power": {"gpu_sampling": true}});
        let t = rescan_transcript("scan", report);
        let root = play_rescan("scan-rescan", &[], &t).await;
        let probes = root.join("probes");
        let nvidia = calls(&probes.join(format!("nvidia-smi{}.log", std::env::consts::EXE_SUFFIX)));
        let scans = nvidia.iter().filter(|c| c.starts_with("--query-gpu=name,")).count();
        assert_eq!(scans, 2, "the hello's scan and the rescan's: {nvidia:?}");
        assert!(nvidia.iter().all(|c| c.starts_with("--query-gpu=")), "{nvidia:?}");
        if cfg!(windows) {
            let ps = calls(&probes.join("powershell.exe.log"));
            assert_eq!(ps.len(), 2, "{ps:?}");
            assert!(ps.iter().all(|c| c.starts_with("-NoProfile -NonInteractive -Command")));
        }
        // The scan was kept in the home.
        let kept = root.join("home").join("helper").join("scan.json");
        let kept: Value =
            serde_json::from_slice(&std::fs::read(kept).expect("scan.json")).expect("JSON");
        assert_eq!(kept["scan"]["gpus"][0]["name"], "CITAR Fake GPU");
        std::fs::remove_dir_all(&root).expect("cleaned");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_binary_reports_a_utf16_hardware_file_and_reads_it_again_on_rescan() {
        let file_scan = json!({"format": "citar-hardware", "cpu": {"model": "Files Own CPU"}});
        let t = rescan_transcript("file", file_scan.clone());
        let dir = scratch("scan-file-utf16");
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("hw16.json");
        let mut utf16 = vec![0xff, 0xfe];
        for u in file_scan.to_string().encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&file, &utf16).expect("written");
        let path = file.display().to_string();
        let root = play_rescan("scan-file", &["--hardware", &path], &t).await;
        let nvidia = calls(
            &root.join("probes").join(format!("nvidia-smi{}.log", std::env::consts::EXE_SUFFIX)),
        );
        assert!(nvidia.iter().all(|c| !c.starts_with("--query-gpu=name,")), "no scan: {nvidia:?}");
        std::fs::remove_dir_all(&root).expect("cleaned");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// `citar-helper scan` with `--home`, the fakes as its probes: its output.
    async fn standalone(root: &Path, home: &Path, extra: &[&str]) -> Value {
        let probes = root.join("probes");
        std::fs::create_dir_all(&probes).expect("probes");
        let fake = |n: &str| {
            let to = probes.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
            std::fs::copy(FAKE, &to).expect("the fake copied");
            to
        };
        let out = command(root)
            .arg("scan")
            .arg("--home")
            .arg(home)
            .args(extra)
            .env("CITAR_HELPER_NVIDIA_SMI", fake("nvidia-smi"))
            .env("CITAR_HELPER_POWERSHELL", fake("powershell"))
            .env("CITAR_HELPER_SYSTEM_PROFILER", fake("system_profiler"))
            .output()
            .await
            .expect("the binary runs");
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).expect("a scan on standard output")
    }

    /// `scan` says what this helper would send: its endpoint and allowlist come from its settings
    /// (`helper.json` in its home, as `run` reads them), and its runtime lists the models the
    /// endpoint lists within the allowlist, not an allowlisted model that is not installed;
    /// `no_llm` there lists none; a flag still wins over the file. Nothing is written in the home.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_reports_the_endpoint_and_allowlist_of_the_helpers_settings() {
        let root = scratch("scan-settings");
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let models = Models::default();
        models.set(vec![listed("model-a", 8192, true), listed("model-b", 4096, false)]);
        let port = loopback_lmstudio(models).await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let settings = home.join("helper.json");
        std::fs::write(
            &settings,
            json!({"provider": "lmstudio", "base_url": base, "models": ["model-a", "not-installed"]})
                .to_string(),
        )
        .expect("helper.json");
        let scan = standalone(&root, &home, &[]).await;
        assert_eq!(
            scan["runtimes"],
            json!([{"name": "LM Studio", "endpoint": base,
                    "models": [{"key": "model-a", "max_context": 8192}]}])
        );
        assert_eq!(scan["gpus"][0]["name"], "CITAR Fake GPU");
        // A flag wins over the file, as for `run`: `--model` replaces the file's allowlist.
        let flagged = standalone(&root, &home, &["--model", "model-b"]).await;
        assert_eq!(
            flagged["runtimes"][0]["models"],
            json!([{"key": "model-b", "max_context": 4096}])
        );
        // `no_llm` in the settings: no runtime, the endpoint not asked.
        std::fs::write(&settings, json!({"base_url": base, "no_llm": true}).to_string())
            .expect("helper.json");
        let none = standalone(&root, &home, &[]).await;
        assert_eq!(none["runtimes"], json!([]));
        let mut left: Vec<String> = std::fs::read_dir(&home)
            .expect("the home")
            .map(|e| e.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["helper.json"], "scan wrote nothing in the home");
        std::fs::remove_dir_all(&root).expect("cleaned");
    }
}
