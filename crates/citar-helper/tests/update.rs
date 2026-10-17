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
//!   `<name>.old`.
//!
//! Every test makes its throwaway update key at run time (`citar_jobs::update::sign`); nothing
//! here holds or writes a real key.

use std::path::{Path, PathBuf};
use std::sync::Arc;
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

/// A loopback HTTP server: `GET /<name>` answers what `routes` holds for it: a body, or a
/// redirect to an address.
#[derive(Clone)]
enum Route {
    Body(Vec<u8>),
    Redirect(String),
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
    let dir = scratch(name);
    let install = Install::at(dir.join("bin").join("citar-helper-under-test"));
    std::fs::create_dir_all(dir.join("bin")).expect("bin");
    std::fs::write(install.path(), b"the installed helper").expect("written");
    let installed_sha = sha256(b"the installed helper");
    let downloads = Downloads::default();
    let setup = UpdateSetup {
        keys: trusted.iter().map(|k| k.verifying_key()).collect(),
        packaged,
        install: Some(install.clone()),
        dir: Some(UpdateDir::new(dir.join("home").join("helper").join("update"))),
        tls: Some(citar_helper::net::verified_tls(None).expect("tls")),
        loopback_only: true,
        idle_wait: Duration::from_secs(3600),
        updating: None,
        downloads: downloads.clone(),
    };
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
/// may be): the binary put back runs all the same.
#[test]
fn update_rollback_puts_the_old_binary_back() {
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
