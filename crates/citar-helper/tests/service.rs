//! The helper's services (design/phase4-helper.md P4.3.9, package 4-14), with the real binary and
//! in-process. Nothing here installs anything: no service, unit, LaunchAgent, Run value or
//! scheduled task is written on any machine these run on (P4.7.1). Real installations run in WSL
//! (`qa_phase4.py service`) and in CI (`helper.yml`'s service legs, `scripts/ci/service_check.py`).
//!
//! - **Gate 1:** `service install|uninstall --dry-run` prints exactly this OS's committed plans
//!   (`tests/data/service/`, which the unit tests hold every OS's plans to on every OS).
//! - **Gate 3:** the Run supervisor, run in the foreground (`run --supervisor`, the switch
//!   `--detach` starts it with): a refused helper under it idles, a killed one is started again
//!   after its backoff, and a stop request ends both; `run --detach` starts it in the background;
//!   in-process on tokio's paused clock, a revoked helper under the supervisor idles with one
//!   attempt an hour and is never restarted.
//! - **Gate 4 (Windows):** the test first checks the process is not elevated and stops if it is;
//!   `service install --system` unelevated prints the elevated command and exits 2, with nothing
//!   installed; `service run` from a console is refused; `status` reads and changes nothing.
//! - Service mode idles on settings this binary cannot take (a console run exits 2).

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use citar_helper::clock::Clock;
use citar_helper::config::{Config, LocalCpu, Machine};
use citar_helper::conformance::canonical_examples;
use citar_helper::key::MachineKey;
use citar_helper::log::Log;
use citar_helper::protocol::{Provider, Sender, UpdatePolicy};
use citar_helper::service::supervisor::{self, Child, Policy};
use citar_helper::session::{Facts, Helper};
#[cfg(feature = "dev-guard")]
use common::loopback_hub;
use common::{Pipe, ServerWs, scratch};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

/// The binary's path as it names itself (`current_exe`): on Windows with backslashes only, where
/// cargo's may mix in the target folder's slashes.
fn bin() -> String {
    if cfg!(windows) { BIN.replace('/', "\\") } else { BIN.to_owned() }
}
const TOKEN: &str = "cwt_service_test_0123456789abcdef";

/// The binary, with the variables that could point it at a real installation removed.
fn command() -> std::process::Command {
    let mut c = std::process::Command::new(BIN);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("CITAR_WORKER_")
            || k == "CITAR_DATA_DIR"
            || k.starts_with("CITAR_HELPER_TEST_")
        {
            c.env_remove(k);
        }
    }
    c
}

fn tokio_command() -> tokio::process::Command {
    let mut c: tokio::process::Command = command().into();
    c.kill_on_drop(true);
    c
}

/// A home holding a `helper.json` a service could start with.
fn home_with_settings(name: &str) -> PathBuf {
    let home = scratch(name).join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join("helper.json"),
        format!(r#"{{"server_url": "http://127.0.0.1:9", "token": "{TOKEN}", "no_llm": true}}"#),
    )
    .expect("helper.json");
    home
}

/// The committed plans of this OS for `scope` (`user` or `system`): install, then uninstall.
fn committed(scope: &str) -> (String, String) {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("service")
        .join(format!("{os}-{scope}.txt"));
    let text = std::fs::read_to_string(&file).expect("the committed plan").replace("\r\n", "\n");
    let (install, rest) =
        text.split_once("\n\ncitar-helper service uninstall").expect("install, then uninstall");
    (format!("{install}\n"), format!("citar-helper service uninstall{rest}"))
}

/// The fixed inputs the committed plans were made from (`service::tests::fixed`), each with what
/// this run has instead.
fn substitutions(scope: &str, home: &Path) -> Vec<(String, String)> {
    let bin = bin();
    let home = home.display().to_string();
    if cfg!(windows) {
        let fixed_home = if scope == "system" {
            r"C:\ProgramData\CITAR\helper"
        } else {
            r"C:\Users\alice\AppData\Local\CITAR"
        };
        vec![
            (r"C:\Users\alice\Downloads\citar-helper-windows-x64.exe".to_owned(), bin),
            (fixed_home.to_owned(), home),
        ]
    } else if cfg!(target_os = "macos") {
        let uid = String::from_utf8(
            std::process::Command::new("id").arg("-u").output().expect("id").stdout,
        )
        .expect("utf-8")
        .trim()
        .to_owned();
        vec![
            ("/Users/alice/bin/citar-helper-macos-arm64".to_owned(), bin),
            ("/Users/alice/Library/Application Support/CITAR".to_owned(), home),
            ("gui/501".to_owned(), format!("gui/{uid}")),
        ]
    } else {
        let fixed_home =
            if scope == "system" { "/srv/citar-helper" } else { "/home/alice/.local/share/citar" };
        vec![("/opt/citar/citar-helper".to_owned(), bin), (fixed_home.to_owned(), home)]
    }
}

/// `service <verb> --<scope> --dry-run --home HOME`, as alice (her `HOME` and `USER`, which on Unix
/// decide the definition's folder and account; nothing is read there in a dry run).
fn dry_run(verb: &str, scope: &str, home: &Path) -> std::process::Output {
    let mut c = command();
    c.args(["service", verb, &format!("--{scope}"), "--dry-run", "--home"]).arg(home);
    if !cfg!(windows) {
        c.env("HOME", if cfg!(target_os = "macos") { "/Users/alice" } else { "/home/alice" })
            .env("USER", "alice")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("SUDO_USER");
    }
    c.output().expect("it runs")
}

/// Gate 1: `--dry-run` prints exactly this OS's committed definitions, for each scope it offers,
/// and changes nothing.
#[test]
fn dry_run_prints_this_os_committed_plans() {
    let home = home_with_settings("dry-run");
    let before = std::fs::read_dir(&home).expect("home").count();
    let scopes: &[&str] = if cfg!(target_os = "macos") { &["user"] } else { &["user", "system"] };
    for scope in scopes {
        let (install, uninstall) = committed(scope);
        let subs = substitutions(scope, &home);
        let real =
            |t: &str| subs.iter().fold(t.to_owned(), |acc, (fixed, now)| acc.replace(fixed, now));
        for (verb, want) in [("install", real(&install)), ("uninstall", real(&uninstall))] {
            let out = dry_run(verb, scope, &home);
            let said = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
            assert_eq!(out.status.code(), Some(0), "{verb} --{scope}: {out:?}");
            assert!(said.starts_with(&want), "{verb} --{scope} printed\n{said}\nnot\n{want}");
            // What follows the plan is notes only (an unelevated --system says how to elevate).
            for extra in said[want.len()..].lines() {
                assert!(extra.starts_with("note: "), "{verb} --{scope}: {extra}");
            }
        }
    }
    assert_eq!(
        std::fs::read_dir(&home).expect("home").count(),
        before,
        "nothing written in the home"
    );
    if cfg!(target_os = "macos") {
        let out = dry_run("install", "system", &home);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("not offered on macOS"), "{out:?}");
    }
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// A service reads its home's helper.json alone: settings flags are refused, and so is an
/// installation whose settings would not start (in a dry run, a note).
#[test]
fn settings_flags_are_refused_and_missing_settings_are_said() {
    let home = scratch("svc-flags").join("home");
    std::fs::create_dir_all(&home).expect("home");
    let out = command()
        .args([
            "service",
            "install",
            "--dry-run",
            "--server",
            "http://127.0.0.1:9",
            "--token",
            "t",
            "--home",
        ])
        .arg(&home)
        .output()
        .expect("it runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--server, --token would not reach it"), "{err}");
    let out = dry_run("install", "user", &home);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("note: the service would not start")
            && said.contains("citar-helper setup --home"),
        "{said}"
    );
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// Gate 4: nothing is installed on Windows. The test stops first if the process is elevated
/// (P4.7.1: then `install --system` could install for real). Unelevated, `install --system` prints
/// the elevated command and exits 2, and the machine has no `citar-helper` service and no home
/// made after it; `service run` from a console is refused; `status` reads and changes nothing.
#[cfg(windows)]
#[test]
fn on_windows_nothing_is_installed_unelevated() {
    assert!(
        !citar_helper::service::elevated(),
        "ABORTED: this test process is elevated, and the test that follows could install a service; \
         run the tests from a prompt that is not elevated"
    );
    let home = home_with_settings("svc-win");
    let out = command()
        .args(["service", "install", "--system", "--home"])
        .arg(&home)
        .output()
        .expect("it runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("elevated prompt (Run as administrator)"), "{err}");
    assert!(
        err.contains(&format!("{} service install --system --home", bin())),
        "the command to run elevated: {err}"
    );
    assert!(!home.join("bin").exists(), "nothing made");
    let sc =
        std::process::Command::new("sc.exe").args(["query", "citar-helper"]).output().expect("sc");
    assert_eq!(sc.status.code(), Some(1060), "no citar-helper service: {sc:?}");
    let out = command()
        .args(["service", "uninstall", "--system", "--home"])
        .arg(&home)
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = command().args(["service", "run", "--home"]).arg(&home).output().expect("it runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    // The whole sentence, as one line with single spaces (a literal's lost line continuations once
    // put runs of spaces in it).
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        format!("citar-helper: {}", citar_helper::app::SERVICE_RUN_FROM_A_CONSOLE),
        "{out:?}"
    );
    assert!(!citar_helper::app::SERVICE_RUN_FROM_A_CONSOLE.contains("  "), "no run of spaces");
    assert!(
        citar_helper::app::SERVICE_RUN_FROM_A_CONSOLE
            .starts_with("`service run` is the Windows service's own entry, which the service manager starts (citar-helper service install --system installs it);"),
        "the sentence"
    );
    for scope in ["--user", "--system"] {
        let out = command()
            .args(["service", "status", scope, "--home"])
            .arg(&home)
            .output()
            .expect("runs");
        assert_eq!(out.status.code(), Some(3), "{scope}: {out:?}");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains("not running") || said.contains("not installed"), "{scope}: {said}");
    }
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// Reads frames until a text one, which it returns parsed.
async fn next_text(ws: &mut ServerWs) -> Option<Value> {
    while let Some(Ok(m)) = ws.next().await {
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).ok();
        }
    }
    None
}

/// The hub's refusal of a token, and the close after it.
async fn refuse(mut ws: ServerWs) {
    let canonical = canonical_examples().expect("examples");
    let frame = canonical[&(Sender::Server, "refused".to_owned())].clone();
    let _sent = ws.send(Message::Text(frame.to_string().into())).await;
    let _closed =
        ws.close(Some(CloseFrame { code: CloseCode::Library(4401), reason: "token".into() })).await;
}

#[cfg(feature = "dev-guard")]
/// Accepts the next connection within `within`, reads its hello and refuses it; the hello.
async fn refuse_next(
    conns: &mut mpsc::UnboundedReceiver<ServerWs>,
    within: Duration,
) -> Option<Value> {
    let mut ws = tokio::time::timeout(within, conns.recv()).await.ok()??;
    let hello = tokio::time::timeout(Duration::from_secs(30), next_text(&mut ws)).await.ok()??;
    refuse(ws).await;
    Some(hello)
}

#[cfg(feature = "dev-guard")]
/// The supervisor's and its helper's process ids, once its record names both.
async fn record_with_child(home: &Path) -> Option<supervisor::Record> {
    for _ in 0..100 {
        if let Some(r) = supervisor::read_record(home)
            && r.child.is_some()
        {
            return Some(r);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[cfg(feature = "dev-guard")]
async fn gone(pid: u32, within: Duration) -> bool {
    let until = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < until {
        if !supervisor::alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !supervisor::alive(pid)
}

/// Stops whatever supervisor a home's record names, whatever happened to the test.
#[cfg(feature = "dev-guard")]
struct Cleanup(PathBuf);

#[cfg(feature = "dev-guard")]
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _stopped = supervisor::stop_running(&self.0, &Log::memory());
    }
}

#[cfg(feature = "dev-guard")]
fn helper_args(port: u16) -> Vec<String> {
    [
        "--server",
        &format!("http://127.0.0.1:{port}"),
        "--token",
        TOKEN,
        "--no-llm",
        "--no-hardware",
        "--no-power",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Gate 3, the real binary: the Run supervisor in the foreground (`run --supervisor`, what
/// `--detach` starts) runs the helper in service mode; refused, the helper idles (one hello, no
/// restart); killed, it is started again after its backoff (shortened by a development build's test
/// hook); a stop request (the home's `supervisor.stop`, as `service uninstall` writes it) ends both,
/// the supervisor with 0, and its record goes.
#[cfg(feature = "dev-guard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_run_supervisor_keeps_the_helper_and_stops_on_request() {
    let home = scratch("svc-supervisor").join("home");
    std::fs::create_dir_all(&home).expect("home");
    let _cleanup = Cleanup(home.clone());
    let (port, mut conns) = loopback_hub().await;
    let mut sup = tokio_command()
        .args(["run", "--supervisor", "--home"])
        .arg(&home)
        .args(helper_args(port))
        .env("CITAR_HELPER_TEST_SUPERVISOR_FIRST_S", "0.5")
        .spawn()
        .expect("the supervisor starts");
    let hello = refuse_next(&mut conns, Duration::from_secs(30)).await.expect("the helper's hello");
    assert_eq!(hello["t"], "hello", "{hello}");
    let record = record_with_child(&home).await.expect("the record names both");
    assert_eq!(Some(record.pid), sup.id(), "{record:?}");
    let child = record.child.expect("the helper");
    assert!(supervisor::alive(child), "the helper runs");
    // Refused for good, it idles: no second hello, the same process.
    assert!(
        tokio::time::timeout(Duration::from_secs(4), conns.recv()).await.is_err(),
        "no restart"
    );
    assert!(supervisor::alive(child), "the same helper still runs");
    // Killed (a crash): the supervisor starts it again after its backoff.
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(child);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    assert!(sys.process(pid).expect("the helper").kill(), "killed");
    let again = refuse_next(&mut conns, Duration::from_secs(30)).await.expect("started again");
    assert_eq!(again["t"], "hello");
    let second = record_with_child(&home).await.and_then(|r| r.child).expect("the new helper");
    assert_ne!(second, child, "a new process");
    let log = std::fs::read_to_string(home.join("helper").join("logs").join(supervisor::LOG_FILE))
        .expect("its log");
    assert!(
        log.contains("starting it again in 0 s") || log.contains("starting it again in 1 s"),
        "{log}"
    );
    // Asked to stop: both end, the supervisor with 0, and its record goes.
    std::fs::write(supervisor::stop_file(&home), b"stop").expect("stop file");
    let status = tokio::time::timeout(Duration::from_secs(30), sup.wait())
        .await
        .expect("it stops")
        .expect("status");
    assert_eq!(status.code(), Some(0));
    assert!(gone(second, Duration::from_secs(10)).await, "the helper ended with it");
    assert!(supervisor::read_record(&home).is_none(), "its record removed");
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// A supervisor ended from outside (Task Manager) leaves its helper running alone: the next one
/// started for the home ends that helper before it starts its own, so two helpers never fight over
/// the machine's connection; and a second supervisor for a home whose supervisor runs leaves at once.
#[cfg(feature = "dev-guard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_supervisor_ends_the_helper_an_ended_one_left() {
    let home = scratch("svc-orphan").join("home");
    std::fs::create_dir_all(&home).expect("home");
    let _cleanup = Cleanup(home.clone());
    let (port, mut conns) = loopback_hub().await;
    let start = || {
        tokio_command()
            .args(["run", "--supervisor", "--home"])
            .arg(&home)
            .args(helper_args(port))
            .spawn()
            .expect("the supervisor starts")
    };
    let mut first = start();
    refuse_next(&mut conns, Duration::from_secs(30)).await.expect("the first helper's hello");
    let orphan = record_with_child(&home).await.and_then(|r| r.child).expect("its helper");
    assert!(supervisor::serves_home(orphan, &home), "the helper serves this home");
    // A second supervisor while the first runs: it leaves at once, and starts nothing. (Were it to
    // stay, the test says so in seconds and ends what it started, rather than hang.)
    let mut second = tokio_command()
        .args(["run", "--supervisor", "--home"])
        .arg(&home)
        .args(helper_args(port))
        .spawn()
        .expect("it starts");
    let left = tokio::time::timeout(Duration::from_secs(15), second.wait()).await;
    if left.is_err() {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let ours: Vec<&sysinfo::Process> = sys
            .processes()
            .values()
            .filter(|p| p.cmd().iter().any(|a| std::path::Path::new(a) == home))
            .collect();
        ours.iter().for_each(|p| {
            let _killed = p.kill();
        });
        panic!("a second supervisor for a home whose supervisor runs did not leave");
    }
    let left = left.expect("in time").expect("its status");
    assert_eq!(left.code(), Some(0), "{left:?}");
    assert!(
        tokio::time::timeout(Duration::from_secs(3), conns.recv()).await.is_err(),
        "no second helper"
    );
    // The first ended from outside: its helper runs on, alone.
    first.kill().await.expect("killed");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(supervisor::alive(orphan), "the helper outlived its supervisor");
    let mut third = start();
    refuse_next(&mut conns, Duration::from_secs(30)).await.expect("the new supervisor's helper");
    assert!(gone(orphan, Duration::from_secs(10)).await, "the helper left alone was ended");
    let log = std::fs::read_to_string(home.join("helper").join("logs").join(supervisor::LOG_FILE))
        .expect("log");
    assert!(log.contains("an earlier supervisor of this home left running"), "{log}");
    std::fs::write(supervisor::stop_file(&home), b"stop").expect("stop file");
    let status = tokio::time::timeout(Duration::from_secs(30), third.wait())
        .await
        .expect("stops")
        .expect("status");
    assert_eq!(status.code(), Some(0));
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// 4-14's review: a record a supervisor ended from outside left behind may name a process id the
/// system has since given to another CITAR helper. That one (here a job host: a `citar-helper`
/// that is no supervisor) is not taken for the home's supervisor: `status` does not call it
/// running, `uninstall`'s stop neither waits for it nor ends it, and a new supervisor of the home
/// starts (and does not end it as a helper left behind).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_record_naming_another_helper_is_no_supervisor() {
    let home = scratch("svc-stale").join("home");
    std::fs::create_dir_all(home.join("helper")).expect("home");
    let mut other = tokio_command()
        .arg("job-host")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a job host");
    let pid = other.id().expect("its pid");
    let stale = format!(r#"{{"pid": {pid}, "child": {pid}, "program": "citar-helper"}}"#);
    std::fs::write(supervisor::pid_file(&home), &stale).expect("the stale record");
    assert!(supervisor::alive(pid), "a live CITAR helper");
    assert!(!supervisor::supervises_home(pid, &home), "but no supervisor of this home");
    assert!(!supervisor::serves_home(pid, &home), "nor a helper serving it");
    #[cfg(windows)]
    {
        let out = command()
            .args(["service", "status", "--user", "--home"])
            .arg(&home)
            .output()
            .expect("runs");
        assert_eq!(out.status.code(), Some(3), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("not running"), "{out:?}");
    }
    let t0 = std::time::Instant::now();
    supervisor::stop_running(&home, &Log::memory()).expect("stopped");
    assert!(t0.elapsed() < Duration::from_secs(5), "no wait for a supervisor that is not there");
    assert!(supervisor::read_record(&home).is_none(), "the stale record removed");
    assert!(other.try_wait().expect("its state").is_none(), "the other helper left running");
    #[cfg(feature = "dev-guard")]
    {
        let _cleanup = Cleanup(home.clone());
        std::fs::write(supervisor::pid_file(&home), &stale).expect("the stale record again");
        let (port, mut conns) = loopback_hub().await;
        let mut sup = tokio_command()
            .args(["run", "--supervisor", "--home"])
            .arg(&home)
            .args(helper_args(port))
            .spawn()
            .expect("the supervisor starts");
        let hello = refuse_next(&mut conns, Duration::from_secs(30))
            .await
            .expect("a new supervisor starts its helper");
        assert_eq!(hello["t"], "hello");
        let log =
            std::fs::read_to_string(home.join("helper").join("logs").join(supervisor::LOG_FILE))
                .expect("its log");
        assert!(!log.contains("already runs"), "{log}");
        assert!(!log.contains("left running"), "{log}");
        std::fs::write(supervisor::stop_file(&home), b"stop").expect("stop file");
        let status = tokio::time::timeout(Duration::from_secs(30), sup.wait())
            .await
            .expect("it stops")
            .expect("status");
        assert_eq!(status.code(), Some(0));
        assert!(other.try_wait().expect("its state").is_none(), "the other helper still runs");
    }
    other.kill().await.expect("the job host ended");
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// Gate 3, `run --detach` (what the Run value starts): it returns at once, the supervisor runs in
/// the background with the helper under it (which connects), and a stop request ends both.
#[cfg(feature = "dev-guard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_detach_leaves_the_supervisor_in_the_background() {
    let home = scratch("svc-detach").join("home");
    std::fs::create_dir_all(&home).expect("home");
    let _cleanup = Cleanup(home.clone());
    let (port, mut conns) = loopback_hub().await;
    let out = tokio_command()
        .args(["run", "--detach", "--home"])
        .arg(&home)
        .args(helper_args(port))
        .output()
        .await
        .expect("it runs");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("runs in the background"), "{out:?}");
    refuse_next(&mut conns, Duration::from_secs(30)).await.expect("the detached helper's hello");
    let record = record_with_child(&home).await.expect("the record");
    let child = record.child.expect("the helper");
    assert!(supervisor::alive(record.pid) && supervisor::alive(child));
    std::fs::write(supervisor::stop_file(&home), b"stop").expect("stop file");
    assert!(gone(record.pid, Duration::from_secs(30)).await, "the supervisor stopped");
    assert!(gone(child, Duration::from_secs(10)).await, "the helper stopped");
    // The detach refuses settings `run` would refuse, before starting anything.
    let bad = command().args(["run", "--detach", "--home"]).arg(&home).output().expect("runs");
    assert_eq!(bad.status.code(), Some(2), "no server, no token: {bad:?}");
    assert!(supervisor::read_record(&home).is_none());
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// In service mode a setting this binary cannot take idles (logged once, read again after 15 s
/// doubling to an hour), where a console run exits 2; so no service manager restarts it every
/// 15 s (P4.3.9). A fixed `helper.json` is read at once (4-14's review: the service's settings are
/// looked at every 2 s), and the same process connects, with no restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_mode_idles_on_settings_it_cannot_take() {
    let home = scratch("svc-idle").join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join("helper.json"),
        format!(r#"{{"server_url": "http://127.0.0.1:9", "token": "{TOKEN}", "updates": 5}}"#),
    )
    .expect("helper.json");
    let console = command().args(["run", "--home"]).arg(&home).output().expect("runs");
    assert_eq!(console.status.code(), Some(2), "a console run exits 2: {console:?}");
    let mut svc = tokio_command()
        .args(["run", "--service", "--no-hardware", "--no-power", "--home"])
        .arg(&home)
        .spawn()
        .expect("starts");
    let log = home.join("helper").join("logs").join("helper.log");
    let mut idled = false;
    for _ in 0..100 {
        if std::fs::read_to_string(&log).is_ok_and(|l| l.contains("Running as a service: idling")) {
            idled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(idled, "the idle line in {}", log.display());
    let said = std::fs::read_to_string(&log).expect("the log");
    assert!(
        said.contains("the settings are read again in 15 s, then less often (at most hourly)"),
        "{said}"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), svc.wait()).await.is_err(),
        "it does not exit"
    );
    #[cfg(feature = "dev-guard")]
    {
        // The owner fixes helper.json: the idling service reads it within seconds, well before its
        // 15 s wait ends, and connects; the same process.
        let (port, mut conns) = loopback_hub().await;
        std::fs::write(
            home.join("helper.json"),
            format!(
                r#"{{"server_url": "http://127.0.0.1:{port}", "token": "{TOKEN}", "no_llm": true}}"#
            ),
        )
        .expect("the fixed helper.json");
        let t0 = std::time::Instant::now();
        let hello =
            refuse_next(&mut conns, Duration::from_secs(20)).await.expect("connected once fixed");
        assert_eq!(hello["t"], "hello", "{hello}");
        assert!(t0.elapsed() < Duration::from_secs(10), "at once, not at the wait's end");
        assert!(svc.try_wait().expect("its state").is_none(), "the same process, never restarted");
        let said = std::fs::read_to_string(&log).expect("the log");
        assert!(said.contains("changed: reading the settings again"), "{said}");
    }
    #[cfg(unix)]
    {
        let pid = svc.id().expect("its pid");
        assert!(
            std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .expect("kill")
                .success()
        );
        let status = tokio::time::timeout(Duration::from_secs(10), svc.wait())
            .await
            .expect("stops")
            .expect("status");
        assert_eq!(status.code(), Some(0), "stopped while idling");
    }
    #[cfg(not(unix))]
    {
        svc.kill().await.expect("killed");
    }
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

/// Ctrl-C (SIGINT) while the session runs is a stop request: the helper says "stopping" and exits
/// 0 (4-14's review: the listener is installed when the session starts, and only then; at the
/// first-run questions a Ctrl-C quits, `tests/binary.rs`).
#[cfg(all(unix, feature = "dev-guard"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_while_serving_stops_the_helper() {
    let home = scratch("svc-sigint").join("home");
    std::fs::create_dir_all(&home).expect("home");
    let (port, mut conns) = loopback_hub().await;
    let mut helper = tokio_command()
        .args(["run", "--home"])
        .arg(&home)
        .args(helper_args(port))
        .spawn()
        .expect("it starts");
    let mut ws = tokio::time::timeout(Duration::from_secs(30), conns.recv())
        .await
        .expect("it dials")
        .expect("a connection");
    let hello = tokio::time::timeout(Duration::from_secs(30), next_text(&mut ws))
        .await
        .expect("its hello")
        .expect("a frame");
    assert_eq!(hello["t"], "hello", "{hello}");
    let pid = helper.id().expect("its pid");
    assert!(
        std::process::Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .expect("kill")
            .success()
    );
    let status = tokio::time::timeout(Duration::from_secs(10), helper.wait())
        .await
        .expect("it stops")
        .expect("status");
    assert_eq!(status.code(), Some(0), "stopped as asked");
    let log = std::fs::read_to_string(home.join("helper").join("logs").join("helper.log"))
        .expect("its log");
    assert!(log.contains("stopping"), "{log}");
    let _cleaned = std::fs::remove_dir_all(home.parent().expect("scratch"));
}

// ---------------------------------------------------------------------------- in-process, paused

/// The fake clock's start.
const START_UNIX: f64 = 1_791_979_200.0;

fn config() -> Config {
    Config {
        server_url: "http://127.0.0.1:1".to_owned(),
        token: TOKEN.to_owned(),
        base_url: "http://127.0.0.1:2/v1".to_owned(),
        provider: Provider::LmStudio,
        api_key_env: String::new(),
        max_concurrent: 1,
        name: String::new(),
        quiet: Vec::new(),
        models: None,
        insecure: false,
        collect_hardware: false,
        hardware_file: String::new(),
        no_llm: true,
        cpu: LocalCpu::default(),
        updates: UpdatePolicy::Off,
        stream: true,
        ca_file: None,
        no_power: true,
        warnings: Vec::new(),
    }
}

fn facts() -> Facts {
    let info = citar_helper::version::build_info();
    Facts {
        hostname: "service-test".to_owned(),
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

/// The helper's own session in service mode, in-process, each start a new one dialling the fake
/// server through `dialer`.
struct InProcess {
    dialer: mpsc::UnboundedSender<ServerWs>,
    starts: Mutex<usize>,
}

impl Child for InProcess {
    fn run(
        &self,
        stop: watch::Receiver<bool>,
    ) -> citar_helper::net::BoxFuture<'_, Result<i32, String>> {
        Box::pin(async move {
            *self.starts.lock().expect("lock") += 1;
            let cfg = config();
            let key = MachineKey::generate()?;
            let helper = Helper {
                ceiling: cfg.ceiling(Machine { logical_cpus: 4, memory_mb: 8192 }),
                config: cfg,
                public_key: key.public_b64(),
                facts: facts(),
                log: Log::memory(),
                clock: Clock::fake(START_UNIX),
                dialer: Arc::new(Pipe(self.dialer.clone())),
                endpoint: None,
                service: true,
                stop,
                in_flight: Default::default(),
                sensors: citar_helper::session::Sensors::none(),
                jobs: citar_helper::jobs::JobSetup::default(),
                update: citar_helper::update::UpdateSetup::default(),
            };
            let code = helper.run().await;
            Ok(i32::from(code.code()))
        })
    }
}

/// Gate 3 on a fake clock: a revoked helper (every hello refused `token`) under the Run supervisor
/// idles: one attempt an hour, the helper never exits, the supervisor never starts it again; a stop
/// ends it with 0.
#[tokio::test(start_paused = true)]
async fn a_revoked_helper_under_the_supervisor_idles_one_attempt_an_hour() {
    let (tx, mut conns) = mpsc::unbounded_channel::<ServerWs>();
    let child = InProcess { dialer: tx, starts: Mutex::new(0) };
    let attempts: Arc<Mutex<Vec<f64>>> = Arc::default();
    let seen = Arc::clone(&attempts);
    let t0 = tokio::time::Instant::now();
    tokio::spawn(async move {
        while let Some(mut ws) = conns.recv().await {
            if next_text(&mut ws).await.is_some_and(|h| h["t"] == "hello") {
                seen.lock().expect("lock").push(t0.elapsed().as_secs_f64());
            }
            refuse(ws).await;
        }
    });
    let (stop_tx, stop_rx) = watch::channel(false);
    let log = Log::memory();
    let supervised =
        async { supervisor::supervise(&child, stop_rx, &Policy::default(), &log).await };
    let clock = async {
        tokio::time::sleep(Duration::from_secs(3 * 3600 + 300)).await;
        stop_tx.send(true).expect("stop");
    };
    let (code, ()) = tokio::join!(supervised, clock);
    assert_eq!(code, 0, "{:?}", log.lines());
    assert_eq!(*child.starts.lock().expect("lock"), 1, "never started again: {:?}", log.lines());
    let at = attempts.lock().expect("lock").clone();
    assert_eq!(at.len(), 4, "one attempt an hour over three hours: {at:?}");
    for pair in at.windows(2) {
        let gap = pair[1] - pair[0];
        assert!((3600.0..3700.0).contains(&gap), "an hour apart: {at:?}");
    }
}
