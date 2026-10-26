//! The machine key on disk (package 4-03, gate 4; design/phase4-helper.md P4.3.7): the binary
//! writes a new key owner-only (mode 0600 on Unix; on Windows a protected DACL with no inherited
//! entry and nobody but the user, SYSTEM and Administrators, read back through citar-jobs' `sys`
//! module), refuses at start a key file the test opened to others, naming the command that fixes
//! it (which the test then runs), and warns about a `helper.json` others can read.
//!
//! Every file here is in the test's own temporary directory; on Windows the test opens one to
//! the Users group with `icacls`, as P4.7.1 allows for a file of its own.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use citar_helper::home::Home;
use common::{loopback_hub, scratch};
use futures_util::StreamExt as _;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

/// Runs the binary with `home` against a hub that refuses every hello with a 4401 close.
///
/// With `--no-hardware --no-power`, as real_binary.rs's runs that do not check the scan: what is
/// checked here is the key, not the machine's report, and the machine's own probes (PowerShell's
/// hardware queries on Windows) can take most of a minute on a cold CI runner (helper.yml at
/// cfa205a: 59 to 63 s for these tests, against 4 to 8 s before), past this run's 60 s.
async fn run_refused(home: &Path) -> std::process::Output {
    let (port, mut conns) = loopback_hub().await;
    tokio::spawn(async move {
        while let Some(mut ws) = conns.recv().await {
            let _hello = ws.next().await;
            let close = CloseFrame { code: CloseCode::from(4401), reason: "".into() };
            let _closed = ws.close(Some(close)).await;
        }
    });
    let mut c = tokio::process::Command::new(BIN);
    c.arg("--home")
        .arg(home)
        .args(["--server", &format!("http://127.0.0.1:{port}"), "--token", "cwt_keys_test"])
        .args(["--base-url", "http://127.0.0.1:9/v1", "--no-llm"])
        .args(["--no-hardware", "--no-power"])
        .env("CITAR_DATA_DIR", home)
        .env_remove("CITAR_WORKER_SERVER")
        .env_remove("CITAR_WORKER_TOKEN")
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(60), c.output())
        .await
        .expect("the binary ends")
        .expect("the binary runs")
}

/// Opens `path` to others: mode 0644, or an allow entry for Users.
fn expose(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
    #[cfg(windows)]
    {
        let out = Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-5-32-545:R"])
            .output()
            .expect("icacls runs");
        assert!(out.status.success(), "{out:?}");
    }
}

/// Runs the command the helper named to fix the file.
fn run_fix(fix: &str) {
    #[cfg(unix)]
    let out = Command::new("sh").args(["-c", fix]).output().expect("sh runs");
    #[cfg(windows)]
    let out = {
        use std::os::windows::process::CommandExt as _;
        Command::new("cmd").arg("/C").raw_arg(fix).output().expect("cmd runs")
    };
    assert!(out.status.success(), "{fix}: {out:?}");
}

fn key_file(home: &Path) -> PathBuf {
    Home::at(home).key_file()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_key_is_owner_only() {
    let root = scratch("key-new");
    let home = root.join("home");
    let out = run_refused(&home).await;
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    let path = key_file(&home);
    assert_eq!(citar_jobs::sys::exposure(&path).expect("read back"), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
    #[cfg(windows)]
    {
        let d = citar_jobs::sys::dacl(&path).expect("the DACL");
        assert!(d.protected, "inheritance is off: {d:?}");
        let aces = d.aces.expect("a DACL");
        assert!(!aces.is_empty() && aces.iter().all(|a| !a.inherited), "{aces:?}");
        let me = citar_jobs::sys::current_user_sid().expect("the user");
        for a in &aces {
            let sid = a.sid.as_deref().expect("a SID");
            assert!([me.as_str(), "S-1-5-18", "S-1-5-32-544"].contains(&sid), "{sid} in {aces:?}");
        }
    }
    std::fs::remove_dir_all(&root).expect("cleaned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_others_can_read_is_refused_with_the_command_that_fixes_it() {
    let root = scratch("key-exposed");
    let home = root.join("home");
    // A key made by a first run, then opened to others.
    let first = run_refused(&home).await;
    assert_eq!(first.status.code(), Some(78), "{first:?}");
    let path = key_file(&home);
    let public = std::fs::read_to_string(&path).expect("the key");
    expose(&path);
    let exposure = citar_jobs::sys::exposure(&path).expect("read").expect("now exposed");

    let out = run_refused(&home).await;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refusing the machine key"), "{err}");
    assert!(err.contains(&exposure.fix), "the fix in {err}");
    #[cfg(windows)]
    assert!(err.contains("S-1-5-32-545 (Users)"), "{err}");
    #[cfg(unix)]
    assert!(err.contains("chmod 600"), "{err}");

    run_fix(&exposure.fix);
    assert_eq!(citar_jobs::sys::exposure(&path).expect("read"), None, "the fix works");
    let again = run_refused(&home).await;
    assert_eq!(again.status.code(), Some(78), "{again:?}");
    assert_eq!(std::fs::read_to_string(&path).expect("the key"), public, "the same key, kept");
    std::fs::remove_dir_all(&root).expect("cleaned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_json_others_can_read_is_warned_about() {
    let root = scratch("key-config");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let config = home.join("helper.json");
    std::fs::write(&config, "{\"name\": \"exposed\"}").expect("helper.json");
    expose(&config);
    let out = run_refused(&home).await;
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("it holds the worker token"), "{err}");
    std::fs::remove_dir_all(&root).expect("cleaned");
}
