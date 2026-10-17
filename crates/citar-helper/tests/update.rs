//! Self-update (design/phase4-helper.md P4.3.8; package 4-13's gates 2 and 3), in-process and
//! with the real binary:
//!
//! - **Refusals** (gate 2): the updater, given a manifest the way the session gives it, against a
//!   loopback asset server: a bad signature, an unknown key, a wrong SHA-256, a wrong size, a
//!   downgrade, the same version, a non-loopback `http` URL, a redirect to one, a missing target,
//!   policy `off`, `auto` in a `packaged` build: each reported `refused` with its reason and the
//!   installed binary's SHA-256 unchanged; `notify` reports `available` and downloads nothing; a
//!   build that trusts no key reports `unavailable`.
//! - **The download, its checks and the self-test**: a good manifest behind two redirects gives
//!   `downloading`, then `verified` and a staged binary (this crate's own, whose `--self-test`
//!   passes); one whose self-test does not say what the manifest says is `failed` and removed,
//!   never staged (gate 3's first case).
//! - **The start count** (gate 3, service mode): the real binary, copied into an install folder of
//!   its own with `<name>.old` beside it and `pending.json` naming it the new version, made to exit
//!   at its start by the test hook, is started by this test as a service manager would, again
//!   after each exit: three starts die, the fourth restores `<name>.old` and exits 75.
//! - **`update rollback`** by hand puts `<name>.old` back, and the binary it replaced becomes
//!   `<name>.old` and is held back: offered again, it is refused, with nothing downloaded.
//!
//! 4-13's fix round adds: a download that fails in a way that may pass (a 503, a body cut short)
//! and a swap that fails are tried again after the retry wait, and verify then; a staged binary
//! changed after it was checked is never installed; an update folder another user can reach is
//! refused with the command that fixes it; a service start the new binary refuses before its
//! session (a setting, a command line) still counts, so the fourth restores `<name>.old`; and a new
//! binary the supervisor cannot even start is rolled back.
//!
//! Every test makes its throwaway update key at run time (`citar_jobs::update::sign`); nothing
//! here holds or writes a real key.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use citar_helper::log::Log;
use citar_helper::protocol::{HelperFrame, UpdateOffer, UpdatePolicy, UpdateState, UpdateStatus};
use citar_helper::update::files::{Install, UpdateDir};
use citar_helper::update::{Downloads, UpdateSetup, Updater};
use citar_jobs::claim::SigningKey;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("citar-helper-update-{name}-{}", std::process::id()));
    let _fresh = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A loopback HTTP server: `GET /<name>` answers what `routes` holds for it: a body, a redirect
/// to an address, or a flaky body (the first time a 503, the second half of the body with no
/// `Content-Length` and the connection closed, then the body).
#[derive(Clone)]
enum Route {
    Body(Vec<u8>),
    Redirect(String),
    Flaky(Arc<AtomicUsize>, Vec<u8>),
}

async fn serve(routes: Vec<(String, Route)>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut got = 0;
                while !buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf[got..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => got += n,
                    }
                }
                let head = String::from_utf8_lossy(&buf[..got]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let answer = match routes.iter().find(|(p, _)| *p == path).map(|(_, r)| r.clone()) {
                    Some(Route::Body(b)) => {
                        let mut a = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            b.len()
                        )
                        .into_bytes();
                        a.extend_from_slice(&b);
                        a
                    }
                    Some(Route::Redirect(to)) => format!(
                        "HTTP/1.1 302 Found\r\nlocation: {to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    )
                    .into_bytes(),
                    Some(Route::Flaky(asked, b)) => match asked.fetch_add(1, Ordering::SeqCst) {
                        0 => b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_vec(),
                        1 => {
                            let mut a = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
                            a.extend_from_slice(&b[..b.len() / 2]);
                            a
                        }
                        _ => {
                            let mut a = format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                b.len()
                            )
                            .into_bytes();
                            a.extend_from_slice(&b);
                            a
                        }
                    },
                    None => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_vec(),
                };
                let _sent = sock.write_all(&answer).await;
                let _shut = sock.shutdown().await;
            });
        }
    });
    port
}

/// A manifest of `version` with one asset for `target`.
fn manifest(version: &str, build: &str, target: &str, url: &str, bytes: &[u8]) -> String {
    json!({
        "v": 1, "version": version, "protocol": 2, "build": build,
        "released_at": "2026-11-01T00:00:00Z",
        "assets": {target: {"name": "citar-helper-next", "url": url, "sha256": sha256(bytes), "size": bytes.len()}}
    })
    .to_string()
}

struct Case {
    dir: PathBuf,
    install: Install,
    installed_sha: String,
    downloads: Downloads,
    updater: Updater,
    out: mpsc::UnboundedReceiver<HelperFrame>,
}

/// An updater of a helper of `own` version under `policy`, trusting `trusted`, its binary a file
/// of its own in a scratch install folder, its home's update folder beside it.
fn case(
    name: &str,
    own: &str,
    policy: UpdatePolicy,
    trusted: &[&SigningKey],
    packaged: bool,
) -> Case {
    case_with(name, own, policy, trusted, packaged, |_, _| {})
}

/// [`case`], with `tweak` given the setup (and the case's folder) before the updater starts.
fn case_with(
    name: &str,
    own: &str,
    policy: UpdatePolicy,
    trusted: &[&SigningKey],
    packaged: bool,
    tweak: impl FnOnce(&mut UpdateSetup, &Path),
) -> Case {
    let dir = scratch(name);
    let install = Install::at(dir.join("bin").join("citar-helper-under-test"));
    std::fs::create_dir_all(dir.join("bin")).expect("bin");
    std::fs::write(install.path(), b"the installed helper").expect("written");
    let installed_sha = sha256(b"the installed helper");
    let downloads = Downloads::default();
    let mut setup = UpdateSetup {
        keys: trusted.iter().map(|k| k.verifying_key()).collect(),
        packaged,
        install: Some(install.clone()),
        dir: Some(UpdateDir::new(dir.join("home").join("helper").join("update"))),
        tls: Some(citar_helper::net::verified_tls(None).expect("tls")),
        loopback_only: true,
        idle_wait: Duration::from_secs(3600),
        retry: citar_helper::update::RETRY_FIRST,
        updating: None,
        downloads: downloads.clone(),
    };
    tweak(&mut setup, &dir);
    let updater = Updater::start(&setup, policy, own, citar_helper::version::TARGET, Log::memory());
    let (tx, out) = mpsc::unbounded_channel();
    updater.connected(tx);
    Case { dir, install, installed_sha, downloads, updater, out }
}

impl Case {
    fn offer(&self, manifest: &str, signature: String) {
        self.updater.offered(UpdateOffer { manifest: manifest.to_owned(), signature });
    }

    /// The statuses until one that ends the update's run here (or 60 s).
    async fn statuses(&mut self) -> Vec<UpdateStatus> {
        let mut seen = Vec::new();
        loop {
            let f = tokio::time::timeout(Duration::from_secs(60), self.out.recv())
                .await
                .expect("a status in time")
                .expect("the updater is there");
            let HelperFrame::UpdateStatus(st) = f else { continue };
            let last = !matches!(st.state, UpdateState::Downloading);
            seen.push(st);
            if last {
                return seen;
            }
        }
    }

    fn unchanged(&self) {
        let now = sha256(&std::fs::read(self.install.path()).expect("read"));
        assert_eq!(now, self.installed_sha, "the installed binary is untouched");
        assert!(!self.install.old().exists(), "nothing was swapped");
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _cleaned = std::fs::remove_dir_all(&self.dir);
    }
}

fn build() -> String {
    citar_helper::version::build_info().build
}

/// 64 bytes no key of these tests made.
const NOBODYS_SIGNATURE: &str =
    "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==";

/// One refusal of gate 2: the helper's version and policy, the manifest and who signed it, the
/// reason the status must give, and the downloads it may have made.
struct Refusal {
    name: &'static str,
    own: &'static str,
    policy: UpdatePolicy,
    packaged: bool,
    text: String,
    signer: Option<SigningKey>,
    why: &'static str,
    downloads: usize,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_refusal_leaves_the_binary_and_says_why() {
    let k = key(7);
    let target = citar_helper::version::TARGET;
    let good = b"a newer helper".to_vec();
    let port = serve(vec![
        ("/good".to_owned(), Route::Body(good.clone())),
        ("/hop".to_owned(), Route::Redirect("/good".to_owned())),
        ("/away".to_owned(), Route::Redirect("http://10.0.0.5/x".to_owned())),
    ])
    .await;
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    let refused = |sts: &[UpdateStatus], why: &str| {
        let last = sts.last().expect("a status");
        assert_eq!(last.state, UpdateState::Refused, "{sts:?}");
        assert!(last.message.contains(why), "{why} in {:?}", last.message);
    };
    // (case, own version, policy, packaged, manifest, signed by, the reason, downloads)
    let m = |v: &str, u: &str, body: &[u8]| manifest(v, &build(), target, u, body);
    let lied_size =
        m("0.2.0", &url("/good"), &good).replace(&format!("\"size\":{}", good.len()), "\"size\":5");
    let lied_hash = m("0.2.0", &url("/good"), &good).replace(&sha256(&good), &"cd".repeat(32));
    let no_target =
        manifest("0.2.0", &build(), "riscv64gc-unknown-linux-gnu", &url("/good"), &good);
    let auto = |name, own, text, signer, why, downloads| Refusal {
        name,
        own,
        policy: UpdatePolicy::Auto,
        packaged: false,
        text,
        signer,
        why,
        downloads,
    };
    let good_m = || m("0.2.0", &url("/good"), &good);
    let cases = [
        auto("badsig", "0.1.0", good_m(), None, "does not verify", 0),
        auto("unknown", "0.1.0", good_m(), Some(key(8)), "does not verify", 0),
        auto("sha", "0.1.0", lied_hash, Some(k.clone()), "wrong sha256", 1),
        auto("size", "0.1.0", lied_size, Some(k.clone()), "wrong size", 1),
        auto("down", "0.3.0", good_m(), Some(k.clone()), "no downgrade", 0),
        auto("same", "0.2.0", good_m(), Some(k.clone()), "already at 0.2.0", 0),
        auto(
            "http",
            "0.1.0",
            m("0.2.0", "http://10.0.0.5/x", &good),
            Some(k.clone()),
            "not https",
            0,
        ),
        auto(
            "redirect",
            "0.1.0",
            m("0.2.0", &url("/away"), &good),
            Some(k.clone()),
            "not https",
            1,
        ),
        auto("target", "0.1.0", no_target, Some(k.clone()), "no binary for this machine", 0),
        Refusal {
            policy: UpdatePolicy::Off,
            ..auto("off", "0.1.0", good_m(), Some(k.clone()), "updates are off", 0)
        },
        Refusal {
            packaged: true,
            ..auto("packaged", "0.1.0", good_m(), Some(k.clone()), "package manager", 0)
        },
    ];
    for r in cases {
        let mut c = case(r.name, r.own, r.policy, &[&k], r.packaged);
        let sig = match &r.signer {
            Some(s) => citar_jobs::update::sign(r.text.as_bytes(), s),
            None => NOBODYS_SIGNATURE.to_owned(),
        };
        c.offer(&r.text, sig);
        let sts = c.statuses().await;
        refused(&sts, r.why);
        assert_eq!(c.downloads.get(), r.downloads, "{}: downloads", r.name);
        assert!(c.updater.staged().is_none(), "{}: nothing staged", r.name);
        c.unchanged();
    }
    // notify: available, and nothing downloaded.
    let mut c = case("notify", "0.1.0", UpdatePolicy::Notify, &[&k], false);
    let text = m("0.2.0", &url("/good"), &good);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    assert_eq!(sts.last().map(|s| s.state), Some(UpdateState::Available), "{sts:?}");
    assert_eq!(sts.last().and_then(|s| s.version.clone()).as_deref(), Some("0.2.0"));
    assert_eq!(c.downloads.get(), 0);
    c.unchanged();
    // Said again on the next connection when the same manifest comes again, not downloaded again.
    let (tx, mut out) = mpsc::unbounded_channel();
    c.updater.connected(tx);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let again = tokio::time::timeout(Duration::from_secs(10), out.recv())
        .await
        .expect("again")
        .expect("a frame");
    assert!(
        matches!(again, HelperFrame::UpdateStatus(ref s) if s.state == UpdateState::Available),
        "{again:?}"
    );
    // No trusted key: unavailable, whatever the manifest.
    let mut c = case("nokey", "0.1.0", UpdatePolicy::Auto, &[], false);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    assert_eq!(sts.last().map(|s| s.state), Some(UpdateState::Unavailable), "{sts:?}");
    assert_eq!(c.downloads.get(), 0);
    c.unchanged();
}

/// The real binary (this crate's) as the update: downloaded behind two redirects, checked, its
/// `--self-test` matching the manifest, staged. One whose self-test says another version is
/// failed and removed, never staged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_good_update_is_staged_and_one_failing_its_self_test_is_not() {
    let k = key(9);
    let bin = std::fs::read(BIN).expect("the helper binary");
    let port = serve(vec![
        ("/release/next".to_owned(), Route::Redirect("/cdn/one".to_owned())),
        ("/cdn/one".to_owned(), Route::Redirect("/cdn/two".to_owned())),
        ("/cdn/two".to_owned(), Route::Body(bin.clone())),
    ])
    .await;
    let me = citar_helper::version::VERSION;
    let target = citar_helper::version::TARGET;
    let url = format!("http://127.0.0.1:{port}/release/next");
    // The binary says it is `me`: the helper under test calls itself older.
    let mut c = case("good", "0.0.1", UpdatePolicy::Auto, &[&k], false);
    let text = manifest(me, &build(), target, &url, &bin);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    let states: Vec<UpdateState> = sts.iter().map(|s| s.state).collect();
    assert_eq!(states, [UpdateState::Downloading, UpdateState::Verified], "{sts:?}");
    assert_eq!(c.downloads.get(), 1);
    let staged = c.updater.staged().expect("staged");
    assert_eq!(staged.version, me);
    assert_eq!(sha256(&std::fs::read(&staged.path).expect("staged")), sha256(&bin));
    c.unchanged();
    // A manifest naming another version than the binary's own: its self-test does not match.
    let mut c = case("selftest", "0.0.1", UpdatePolicy::Auto, &[&k], false);
    let text = manifest("99.0.0", &build(), target, &url, &bin);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    let last = sts.last().expect("a status");
    assert_eq!(last.state, UpdateState::Failed, "{sts:?}");
    assert!(
        last.message.contains("failed its self-test") && last.message.contains("99.0.0"),
        "{}",
        last.message
    );
    assert!(c.updater.staged().is_none());
    let staged_path = c.dir.join("home").join("helper").join("update").join("citar-helper-next");
    assert!(!staged_path.exists(), "the binary that failed is removed");
    c.unchanged();
}

/// A copy of the helper binary in an install folder of its own, as `name`.
fn installed(dir: &Path, extra: &[u8]) -> PathBuf {
    let exe = dir.join(if cfg!(windows) { "citar-helper.exe" } else { "citar-helper" });
    let mut bytes = std::fs::read(BIN).expect("the helper binary");
    // Trailing bytes tell copies apart; a PE or ELF program runs all the same.
    bytes.extend_from_slice(extra);
    std::fs::write(&exe, bytes).expect("installed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
    exe
}

/// Gate 3, service mode: three starts that die before `healthy`, then the fourth restores
/// `<name>.old` and exits 75 (a test build made to exit at its start, the hook
/// `CITAR_HELPER_TEST_EXIT_AT_START`; this test stands in for the service manager).
#[cfg(feature = "test-update-key")]
#[test]
fn a_new_binary_that_dies_three_starts_in_a_row_is_rolled_back() {
    let d = scratch("starts");
    let bin_dir = d.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    let exe = installed(&bin_dir, b"NEW");
    let install = Install::at(&exe);
    let old_bytes = {
        let mut b = std::fs::read(BIN).expect("read");
        b.extend_from_slice(b"OLD");
        b
    };
    std::fs::write(install.old(), &old_bytes).expect("old");
    let home = d.join("home");
    let dir = UpdateDir::new(home.join("helper").join("update"));
    let me = citar_helper::version::VERSION;
    dir.begin("0.0.1-old", me, &"ab".repeat(32)).expect("pending");
    let start = || {
        std::process::Command::new(&exe)
            .args(["run", "--service", "--home"])
            .arg(&home)
            .args(["--server", "http://127.0.0.1:9", "--token", "t", "--no-llm", "--no-hardware"])
            .env("CITAR_DATA_DIR", &home)
            .env("CITAR_HELPER_TEST_EXIT_AT_START", me)
            .output()
            .expect("it runs")
    };
    for n in 1..=3 {
        let out = start();
        assert_eq!(out.status.code(), Some(1), "start {n} dies: {out:?}");
        assert_eq!(dir.pending().map(|p| p.starts), Some(n), "start {n} counted");
    }
    let out = start();
    assert_eq!(out.status.code(), Some(75), "the fourth rolls back and exits 75: {out:?}");
    assert_eq!(std::fs::read(&exe).expect("read"), old_bytes, "the old binary is back");
    assert!(dir.pending().is_none());
    let f = dir.failed().expect("failed.json");
    assert_eq!((f.version.as_str(), f.reported), (me, false));
    assert!(f.message.contains("did not become healthy in 3 starts"), "{}", f.message);
    let _cleaned = std::fs::remove_dir_all(&d);
}

/// `citar-helper update rollback` puts `<name>.old` back by hand, and keeps the binary it replaced
/// as `<name>.old`. The `.old` here is written without an executable bit (as a copy made by hand
/// may be): the binary put back runs all the same. The release rolled away from is held back
/// (`failed.json`, already reported): offered again under `auto`, it is refused with nothing
/// downloaded, where before the next welcome installed it again within seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_rollback_puts_the_old_binary_back() {
    let d = scratch("rollback");
    let bin_dir = d.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    let exe = installed(&bin_dir, b"NEW");
    let install = Install::at(&exe);
    let new_bytes = std::fs::read(&exe).expect("read");
    let mut old_bytes = std::fs::read(BIN).expect("read");
    old_bytes.extend_from_slice(b"OLD");
    std::fs::write(install.old(), &old_bytes).expect("old");
    let home = d.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let out = std::process::Command::new(&exe)
        .args(["update", "rollback", "--home"])
        .arg(&home)
        .env("CITAR_DATA_DIR", &home)
        .output()
        .expect("it runs");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read(&exe).expect("read"), old_bytes, "the old binary is back");
    assert_eq!(std::fs::read(install.old()).expect("read"), new_bytes, "the replaced one is .old");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&exe).expect("meta").permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the binary put back is runnable: {mode:o}");
    }
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("does not install CITAR helper"), "{said}");
    let me = citar_helper::version::VERSION;
    let update_dir = UpdateDir::new(home.join("helper").join("update"));
    let held = update_dir.failed().expect("failed.json");
    assert_eq!(held.version, me);
    assert_eq!(held.sha256, sha256(&new_bytes), "the binary rolled away from");
    assert!(held.reported, "its owner's own doing: no failed to report");
    assert!(held.message.contains("rolled back by hand"), "{}", held.message);
    // The same release offered again, as the next welcome relays it: refused, nothing downloaded.
    let k = key(11);
    let port = serve(vec![("/next".to_owned(), Route::Body(new_bytes.clone()))]).await;
    let mut c = case_with("rollback-offer", "0.0.1", UpdatePolicy::Auto, &[&k], false, |s, _| {
        s.dir = Some(update_dir.clone());
    });
    let text = manifest(
        me,
        &build(),
        citar_helper::version::TARGET,
        &format!("http://127.0.0.1:{port}/next"),
        &new_bytes,
    );
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    let last = sts.last().expect("a status");
    assert_eq!(last.state, UpdateState::Refused, "{sts:?}");
    assert!(last.message.contains("was rolled back on this machine"), "{}", last.message);
    assert_eq!(c.downloads.get(), 0);
    c.unchanged();
    // Nothing to roll back to: refused, nothing changed.
    std::fs::remove_file(install.old()).expect("gone");
    let out = std::process::Command::new(&exe)
        .args(["update", "rollback", "--home"])
        .arg(&home)
        .env("CITAR_DATA_DIR", &home)
        .output()
        .expect("it runs");
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(std::fs::read(&exe).expect("read"), old_bytes);
    let _cleaned = std::fs::remove_dir_all(&d);
}

/// A download that fails in a way that may pass is tried again after the retry wait, as is a
/// failed swap: the first GET answered 503, the second cut short with no `Content-Length`, both
/// `failed` with when they are tried again (the same manifest offered again in between downloads
/// nothing); the third verifies. A swap that fails is tried again the same way. A staged binary
/// changed after it was verified is never installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_that_may_pass_is_tried_again_and_a_changed_binary_never_installed() {
    let k = key(12);
    let bin = std::fs::read(BIN).expect("the helper binary");
    let asked = Arc::new(AtomicUsize::new(0));
    let port =
        serve(vec![("/flaky".to_owned(), Route::Flaky(Arc::clone(&asked), bin.clone()))]).await;
    let me = citar_helper::version::VERSION;
    let target = citar_helper::version::TARGET;
    let url = format!("http://127.0.0.1:{port}/flaky");
    let mut c = case_with("retry", "0.0.1", UpdatePolicy::Auto, &[&k], false, |s, _| {
        s.retry = Duration::from_millis(300);
    });
    let text = manifest(me, &build(), target, &url, &bin);
    let sig = citar_jobs::update::sign(text.as_bytes(), &k);
    c.offer(&text, sig.clone());
    let first = c.statuses().await;
    let states = |sts: &[UpdateStatus]| sts.iter().map(|s| s.state).collect::<Vec<_>>();
    assert_eq!(states(&first), [UpdateState::Downloading, UpdateState::Failed], "{first:?}");
    let why = &first[1].message;
    assert!(why.contains("503") && why.contains("tried again in 1 s"), "{why}");
    // The same manifest again (a reconnect's welcome) before the retry: nothing more.
    c.offer(&text, sig.clone());
    let second = c.statuses().await;
    assert_eq!(states(&second), [UpdateState::Downloading, UpdateState::Failed], "{second:?}");
    let why = &second[1].message;
    assert!(why.contains("ended after") && why.contains("tried again in 1 s"), "{why}");
    let third = c.statuses().await;
    assert_eq!(states(&third), [UpdateState::Downloading, UpdateState::Verified], "{third:?}");
    assert_eq!((c.downloads.get(), asked.load(Ordering::SeqCst)), (3, 3));
    let staged = c.updater.staged().expect("staged");
    // The swap fails (the binary's folder full, say): tried again the same way.
    c.updater.swap_failed("no space left on the device".to_owned());
    let after_swap = c.statuses().await;
    assert_eq!(states(&after_swap), [UpdateState::Failed], "{after_swap:?}");
    assert!(
        after_swap[0].message.contains("could not be installed")
            && after_swap[0].message.contains("tried again"),
        "{}",
        after_swap[0].message
    );
    let again = c.statuses().await;
    assert_eq!(states(&again), [UpdateState::Downloading, UpdateState::Verified], "{again:?}");
    assert_eq!(c.downloads.get(), 4);
    // A staged binary changed after its checks (another local user, a disk fault): not installed.
    let staged = c.updater.staged().unwrap_or(staged);
    let mut changed = std::fs::read(&staged.path).expect("staged");
    changed.extend_from_slice(b"SOMEONE ELSE'S");
    std::fs::write(&staged.path, &changed).expect("changed");
    let e = c.install.swap_in(&staged.path, &staged.sha256).expect_err("not installed");
    assert!(e.contains("changed after it was checked"), "{e}");
    c.unchanged();
}

/// Opens `path` (a folder) to others: mode 0777, or an allow entry for Users.
fn expose(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-5-32-545:R"])
            .output()
            .expect("icacls runs");
        assert!(out.status.success(), "{out:?}");
    }
}

/// Runs the command the helper named to fix a folder.
fn run_fix(fix: &str) {
    #[cfg(unix)]
    let out = std::process::Command::new("sh").args(["-c", fix]).output().expect("sh runs");
    #[cfg(windows)]
    let out = {
        use std::os::windows::process::CommandExt as _;
        std::process::Command::new("cmd").arg("/C").raw_arg(fix).output().expect("cmd runs")
    };
    assert!(out.status.success(), "{fix}: {out:?}");
}

/// The home's update folder, where a downloaded binary waits to be self-tested and installed, is
/// made owner-only; one another local user can reach is refused before anything is downloaded,
/// with the command that fixes it, and once that is run the update goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_folder_others_can_reach_is_refused_with_the_fix() {
    let k = key(13);
    let body = b"a newer helper".to_vec();
    let port = serve(vec![("/next".to_owned(), Route::Body(body.clone()))]).await;
    let url = format!("http://127.0.0.1:{port}/next");
    let target = citar_helper::version::TARGET;
    let mut c = case("exposed", "0.1.0", UpdatePolicy::Auto, &[&k], false);
    let update_dir = c.dir.join("home").join("helper").join("update");
    UpdateDir::new(&update_dir).create().expect("made");
    assert_eq!(citar_jobs::sys::dir_exposure(&update_dir).expect("read"), None, "owner-only");
    expose(&update_dir);
    let text = manifest("0.2.0", &build(), target, &url, &body);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    let last = sts.last().expect("a status");
    assert_eq!(last.state, UpdateState::Refused, "{sts:?}");
    assert!(last.message.contains("another user of this machine"), "{}", last.message);
    let exposure = citar_jobs::sys::dir_exposure(&update_dir).expect("read").expect("exposed");
    assert!(last.message.contains(&exposure.fix), "the fix in {}", last.message);
    assert_eq!(c.downloads.get(), 0);
    c.unchanged();
    run_fix(&exposure.fix);
    assert_eq!(citar_jobs::sys::dir_exposure(&update_dir).expect("read"), None, "the fix works");
    // The next manifest goes on to its download (this body is no helper: its self-test fails).
    let text = manifest("0.2.1", &build(), target, &url, &body);
    c.offer(&text, citar_jobs::update::sign(text.as_bytes(), &k));
    let sts = c.statuses().await;
    assert_eq!(sts.first().map(|s| s.state), Some(UpdateState::Downloading), "{sts:?}");
    assert_eq!(c.downloads.get(), 1);
    c.unchanged();
}

/// An install folder with the new binary in place (this crate's binary with `NEW` after it), the
/// old one as `<name>.old` (`OLD` after it), and `pending.json` naming this version as the
/// update's new one: (the folder, the install, the old bytes, the home, its update folder).
fn pending_install(name: &str) -> (PathBuf, Install, Vec<u8>, PathBuf, UpdateDir) {
    let d = scratch(name);
    let bin_dir = d.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    let exe = installed(&bin_dir, b"NEW");
    let install = Install::at(&exe);
    let mut old_bytes = std::fs::read(BIN).expect("read");
    old_bytes.extend_from_slice(b"OLD");
    std::fs::write(install.old(), &old_bytes).expect("old");
    let home = d.join("home");
    let dir = UpdateDir::new(home.join("helper").join("update"));
    dir.begin("0.0.1-old", citar_helper::version::VERSION, &"ab".repeat(32)).expect("pending");
    (d, install, old_bytes, home, dir)
}

/// Gate 3's service mode, for a new binary that refuses its start before its session: a setting
/// the old one took (`"updates": 5` in `helper.json`), or a command line it cannot read (a flag a
/// release renamed). Each start exits 2 and is counted; the fourth restores `<name>.old` and exits
/// 75, as the service manager's restarts would see it.
#[test]
fn a_new_binary_refusing_its_start_is_rolled_back_after_three() {
    for (case, refusal) in [("settings", "setting"), ("cli", "flag")] {
        let (d, install, old_bytes, home, dir) = pending_install(&format!("refusing-{case}"));
        std::fs::create_dir_all(&home).expect("home");
        let mut args: Vec<&str> = vec!["run", "--service"];
        if refusal == "setting" {
            std::fs::write(home.join("helper.json"), br#"{"updates": 5}"#).expect("helper.json");
        } else {
            args.push("--a-flag-this-release-renamed");
        }
        let start = || {
            std::process::Command::new(install.path())
                .args(&args)
                .arg("--home")
                .arg(&home)
                .args(["--server", "http://127.0.0.1:9", "--token", "t", "--no-llm"])
                .env("CITAR_DATA_DIR", &home)
                .env_remove("CITAR_HELPER_TEST_EXIT_AT_START")
                .output()
                .expect("it runs")
        };
        for n in 1..=3 {
            let out = start();
            assert_eq!(out.status.code(), Some(2), "{case}: start {n} is refused: {out:?}");
            assert_eq!(dir.pending().map(|p| p.starts), Some(n), "{case}: start {n} counted");
        }
        let out = start();
        assert_eq!(out.status.code(), Some(75), "{case}: the fourth rolls back: {out:?}");
        assert_eq!(std::fs::read(install.path()).expect("read"), old_bytes, "{case}: old is back");
        assert!(dir.pending().is_none());
        let f = dir.failed().expect("failed.json");
        assert!(f.message.contains("did not become healthy in 3 starts"), "{}", f.message);
        let _cleaned = std::fs::remove_dir_all(&d);
    }
}

/// The console supervisor cannot even start the new binary (here a file that is no program, as an
/// antivirus that quarantined it would leave): the old binary is restored and run in its place,
/// and `failed.json` says why.
#[test]
fn a_new_binary_the_supervisor_cannot_start_is_rolled_back() {
    let (d, install, old_bytes, _home, dir) = pending_install("unstartable");
    // A new file (no executable bit on Unix), not the program rewritten in place.
    std::fs::remove_file(install.path()).expect("removed");
    std::fs::write(install.path(), b"not a program at all").expect("written");
    let args = vec![std::ffi::OsString::from("--version")];
    let code = citar_helper::update::supervise::supervise(&install, &dir, &args, &Log::memory());
    assert_eq!(code, 0, "the old binary ran in its place (--version)");
    assert_eq!(std::fs::read(install.path()).expect("read"), old_bytes, "the old one is back");
    assert!(dir.pending().is_none());
    let f = dir.failed().expect("failed.json");
    assert!(f.message.contains("could not be started"), "{}", f.message);
    let _cleaned = std::fs::remove_dir_all(&d);
}
