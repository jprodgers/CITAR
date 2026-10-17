//! Self-update (design/phase4-helper.md P4.3.8; PROTOCOL.md "Updates"): the signed manifest the
//! server relays (`welcome.update`, `update`), checked here again, and under the owner's policy
//! (`updates`: `auto`, `notify` or `off`) the new binary downloaded, checked, self-tested and
//! handed off to.
//!
//! **One task** ([`Updater`]) owns it, apart from the connection (a download goes on across a lost
//! one); it says where it is in `update_status` frames, sent on the connection it is given and
//! said again after a reconnect when the server relays the same manifest. **The steps**, each
//! stopping at its first refusal ([`UpdateState`] `refused`, with the reason):
//! 1. a build that trusts no key ([`UpdateSetup::keys`], `update-keys.pub`: none in Phase 4)
//!    reports `unavailable` and never downloads; otherwise the signature must verify
//!    ([`citar_jobs::update::verify`]); the manifest is then kept in `helper/update/`;
//! 2. only a newer version (no downgrade, not the same), with a binary for this target, and not
//!    one rolled back here before (`failed.json`'s version and SHA-256);
//! 3. the policy: `off` refuses ("updates are off on this machine"), `notify` reports `available`
//!    and stops there, `auto` goes on, except in a `packaged` build (Phase 5's package managers),
//!    where it is refused ("update through your package manager");
//! 4. the asset's URL as [`fetch::allowed`] says, the binary's folder writable, and in a
//!    development build not a build's output ([`files::Install::in_build_output`]); then the
//!    download ([`fetch::download`]: `downloading`), its size and SHA-256 against the signed ones;
//! 5. the staged binary's `--self-test`, whose version line must be the manifest's version,
//!    build and protocol and this target, and which must pass the canary and the self-check
//!    (`verified`, or `failed` and the file removed);
//! 6. idle: the session hands off once no completion or job is in flight, taking work as usual
//!    for up to [`UpdateSetup::idle_wait`] (an hour), then no new work until what runs ends;
//! 7. the hand-off (`crate::session`): the job host stopped, `restarting`, the socket closed, then
//!    `pending.json` written and the binary swapped ([`files::Install::swap_in`], the old one kept
//!    as `<name>.old`); a console run supervises the new binary ([`supervise`]), a service run
//!    exits 75. The new binary reports `applied` after its first welcome ([`files::on_start`],
//!    [`Updater::welcomed`]); a rolled-back one's old binary reports `failed`.
//!
//! Replaces nothing: protocol 1's helper never updated itself.

pub mod fetch;
pub mod files;
pub mod supervise;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use citar_jobs::update::{Asset, Manifest, VerifyingKey, newer, verify};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::log::{Log, clip};
use crate::protocol::{HelperFrame, UpdateOffer, UpdatePolicy, UpdateState, UpdateStatus};
use crate::version::BuildInfo;
use files::{Install, UpdateDir};

/// How long a verified update waits for the helper to be idle, taking work as usual, before it
/// stops taking new work so that what runs can end (P4.3.8, step 6).
pub const IDLE_WAIT: Duration = Duration::from_secs(3600);
/// How long the new binary runs without reaching its server before it counts as healthy.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// How long a staged binary's `--self-test` may take.
pub const SELF_TEST_WAIT: Duration = Duration::from_secs(120);
/// The most characters of an `update_status` message.
pub const MESSAGE_CHARS: usize = 1000;
/// How long a refusal's marking of a new binary healthy may take before the helper goes on.
pub const HEARD_WAIT: Duration = Duration::from_secs(5);

/// The downloads an updater started (the transcripts' `downloads`).
#[derive(Clone, Debug, Default)]
pub struct Downloads(Arc<AtomicUsize>);

impl Downloads {
    #[must_use]
    pub fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    fn add(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// What the updater runs with.
#[derive(Clone)]
pub struct UpdateSetup {
    /// The update keys this build trusts: `update-keys.pub`'s, and in a `test-update-key` build
    /// `--update-key`'s.
    pub keys: Vec<VerifyingKey>,
    /// A `packaged` build (Phase 5's package managers): `auto` is refused.
    pub packaged: bool,
    /// The helper's own binary; without one (a test's helper in-process) nothing is installed.
    pub install: Option<Install>,
    /// The home's `helper/update/`; without one nothing is kept or downloaded.
    pub dir: Option<UpdateDir>,
    /// TLS for the downloads (the OS's trust store, and `--ca-file`'s).
    pub tls: Option<rustls::ClientConfig>,
    /// A development or test build: downloads from loopback addresses only, `http` there too.
    pub loopback_only: bool,
    /// Step 6's wait ([`IDLE_WAIT`]; a test build's hook shortens it).
    pub idle_wait: Duration,
    /// This binary is the new one of an update in progress, from this version (`pending.json`).
    pub updating: Option<String>,
    /// The downloads started.
    pub downloads: Downloads,
}

impl Default for UpdateSetup {
    fn default() -> Self {
        UpdateSetup {
            keys: Vec::new(),
            packaged: false,
            install: None,
            dir: None,
            tls: None,
            loopback_only: true,
            idle_wait: IDLE_WAIT,
            updating: None,
            downloads: Downloads::default(),
        }
    }
}

/// A verified binary waiting for the hand-off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    pub version: String,
    pub sha256: String,
}

/// The session's word to the updater.
enum Cmd {
    Connected(mpsc::UnboundedSender<HelperFrame>),
    Disconnected,
    Offered(UpdateOffer),
    Welcomed,
    Heard(oneshot::Sender<()>),
    SwapFailed(String),
}

/// The updater's handle: every call returns at once.
pub struct Updater {
    cmds: mpsc::UnboundedSender<Cmd>,
    ready: watch::Receiver<Option<Staged>>,
    downloads: Downloads,
    idle_wait: Duration,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Updater {
    /// Starts the updater of a helper of `version` for `target` under `policy`.
    #[must_use]
    pub fn start(
        setup: &UpdateSetup,
        policy: UpdatePolicy,
        version: &str,
        target: &str,
        log: Log,
    ) -> Updater {
        let (cmds, rx) = mpsc::unbounded_channel();
        let (ready_tx, ready) = watch::channel(None);
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();
        let healthy_at = setup.updating.as_ref().map(|_| Instant::now() + HEALTHY_AFTER);
        let task = Task {
            setup: setup.clone(),
            policy,
            version: version.to_owned(),
            target: target.to_owned(),
            log,
            out: None,
            conn: 0,
            last: None,
            last_sent_on: 0,
            current: None,
            pipeline: None,
            generation: 0,
            progress_tx,
            ready: ready_tx,
            healthy_at,
            healthy: false,
            applied_sent: false,
        };
        let downloads = setup.downloads.clone();
        let idle_wait = setup.idle_wait;
        let task = tokio::spawn(task.run(rx, progress_rx));
        Updater { cmds, ready, downloads, idle_wait, task }
    }

    fn send(&self, cmd: Cmd) {
        let _gone = self.cmds.send(cmd);
    }

    /// A connection's welcome came: `update_status` frames go to `out`.
    pub fn connected(&self, out: mpsc::UnboundedSender<HelperFrame>) {
        self.send(Cmd::Connected(out));
    }

    /// The connection ended.
    pub fn disconnected(&self) {
        self.send(Cmd::Disconnected);
    }

    /// The server relayed a manifest (`welcome.update` or `update`).
    pub fn offered(&self, offer: UpdateOffer) {
        self.send(Cmd::Offered(offer));
    }

    /// The welcome came: a new binary is healthy now and says `applied`; a rolled-back one's old
    /// binary says `failed`.
    pub fn welcomed(&self) {
        self.send(Cmd::Welcomed);
    }

    /// The server answered otherwise (a refusal the helper understood): a new binary works, and
    /// counts as healthy. Returns once that is written (at most [`HEARD_WAIT`]), since a console
    /// run may exit right after, and its supervisor reads the marker.
    pub async fn heard(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Heard(tx));
        let _done = tokio::time::timeout(HEARD_WAIT, rx).await;
    }

    /// The hand-off's swap failed: the binary is the old one, which goes on and says `failed`.
    pub fn swap_failed(&self, why: String) {
        self.send(Cmd::SwapFailed(why));
    }

    /// A verified binary waiting for the hand-off, as it changes.
    #[must_use]
    pub fn ready(&self) -> watch::Receiver<Option<Staged>> {
        self.ready.clone()
    }

    /// The binary waiting for the hand-off now, if any.
    #[must_use]
    pub fn staged(&self) -> Option<Staged> {
        self.ready.borrow().clone()
    }

    /// The downloads started.
    #[must_use]
    pub fn downloads(&self) -> usize {
        self.downloads.get()
    }

    /// Step 6's wait.
    #[must_use]
    pub fn idle_wait(&self) -> Duration {
        self.idle_wait
    }
}

/// What the pipeline says as it goes, by its generation (a replaced pipeline's are dropped).
enum Progress {
    Status(u64, UpdateStatus),
    Done(u64, Option<Staged>),
}

/// The manifest being dealt with, and the version it is for once it verified.
struct Current {
    manifest: String,
    signature: String,
    version: Option<String>,
}

/// What a manifest comes to.
enum Decision {
    Say(UpdateState, Option<String>, String),
    Fetch(Manifest, Asset),
}

struct Task {
    setup: UpdateSetup,
    policy: UpdatePolicy,
    version: String,
    target: String,
    log: Log,
    out: Option<mpsc::UnboundedSender<HelperFrame>>,
    /// The connection's number (each `Connected` a new one).
    conn: u64,
    last: Option<UpdateStatus>,
    /// The connection the last status went out on.
    last_sent_on: u64,
    current: Option<Current>,
    pipeline: Option<tokio::task::JoinHandle<()>>,
    generation: u64,
    progress_tx: mpsc::UnboundedSender<Progress>,
    ready: watch::Sender<Option<Staged>>,
    /// When a new binary that has not reached its server counts as healthy.
    healthy_at: Option<Instant>,
    healthy: bool,
    applied_sent: bool,
}

fn cut(s: &str) -> String {
    clip(s, MESSAGE_CHARS)
}

impl Task {
    async fn run(
        mut self,
        mut cmds: mpsc::UnboundedReceiver<Cmd>,
        mut progress: mpsc::UnboundedReceiver<Progress>,
    ) {
        loop {
            let healthy_at = self.healthy_at;
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(c) => self.command(c).await,
                    None => break,
                },
                Some(p) = progress.recv() => self.progress(p),
                () = sleep_until_opt(healthy_at) => {
                    self.healthy_at = None;
                    self.mark_healthy().await;
                }
            }
        }
        if let Some(p) = self.pipeline.take() {
            p.abort();
        }
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Connected(out) => {
                self.conn += 1;
                self.out = Some(out);
            }
            Cmd::Disconnected => self.out = None,
            Cmd::Offered(offer) => self.consider(offer).await,
            Cmd::Welcomed => self.welcome().await,
            Cmd::Heard(done) => {
                self.mark_healthy().await;
                let _told = done.send(());
            }
            Cmd::SwapFailed(why) => {
                let _none = self.ready.send(None);
                let version = self.current.as_ref().and_then(|c| c.version.clone());
                self.say(
                    UpdateState::Failed,
                    version,
                    &format!("the update could not be installed: {why}"),
                );
            }
        }
    }

    /// Says where the updater is: kept as the last word, sent when connected.
    fn say(&mut self, state: UpdateState, version: Option<String>, message: &str) {
        let st =
            UpdateStatus { state, version: version.map(|v| clip(&v, 64)), message: cut(message) };
        self.log.info(format!(
            "update: {}{}: {}",
            state.as_str(),
            st.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default(),
            st.message
        ));
        self.last = Some(st.clone());
        self.emit(st);
    }

    fn emit(&mut self, st: UpdateStatus) {
        if let Some(out) = &self.out
            && out.send(HelperFrame::UpdateStatus(st)).is_ok()
        {
            self.last_sent_on = self.conn;
        }
    }

    /// The pipeline running, or a binary staged.
    fn busy(&self) -> Option<String> {
        if self.pipeline.as_ref().is_some_and(|p| !p.is_finished()) || self.ready.borrow().is_some()
        {
            return self.current.as_ref().and_then(|c| c.version.clone());
        }
        None
    }

    async fn consider(&mut self, offer: UpdateOffer) {
        if let Some(c) = &self.current
            && c.manifest == offer.manifest
            && c.signature == offer.signature
        {
            // The same manifest again (a reconnect's welcome): what it came to is said again on a
            // connection that has not heard it.
            if self.last_sent_on != self.conn
                && let Some(st) = self.last.clone()
            {
                self.emit(st);
            }
            return;
        }
        let decision = self.decide(&offer).await;
        if let Some(busy) = self.busy() {
            let replaces = matches!(&decision, Decision::Fetch(m, _) if newer(&m.version, &busy) == Some(true));
            if !replaces {
                self.log.debug(format!(
                    "a manifest came while {busy} is being installed: it waits for that one"
                ));
                return;
            }
            self.log.info(format!("a newer update replaces {busy}, which was being installed"));
            if let Some(p) = self.pipeline.take() {
                p.abort();
            }
            let _none = self.ready.send(None);
        }
        let version = match &decision {
            Decision::Fetch(m, _) => Some(m.version.clone()),
            Decision::Say(_, v, _) => v.clone(),
        };
        self.current = Some(Current {
            manifest: offer.manifest.clone(),
            signature: offer.signature.clone(),
            version,
        });
        match decision {
            Decision::Say(state, version, message) => self.say(state, version, &message),
            Decision::Fetch(manifest, asset) => self.fetch(manifest, asset),
        }
    }

    /// Steps 1 to 4 of the module's doc, before anything is downloaded.
    async fn decide(&self, offer: &UpdateOffer) -> Decision {
        let refused = |v: Option<&str>, why: String| {
            Decision::Say(UpdateState::Refused, v.map(str::to_owned), why)
        };
        if self.setup.keys.is_empty() {
            return Decision::Say(
                UpdateState::Unavailable,
                None,
                "this build of the helper trusts no update key, so it does not update itself: \
                 download the new helper from the server's Servers page"
                    .to_owned(),
            );
        }
        let m = match verify(offer.manifest.as_bytes(), &offer.signature, &self.setup.keys) {
            Ok(m) => m,
            Err(e) => return refused(None, format!("the update was refused: {e}")),
        };
        if let Some(dir) = self.setup.dir.clone() {
            let (text, sig) = (offer.manifest.clone(), offer.signature.clone());
            let kept = tokio::task::spawn_blocking(move || dir.keep_manifest(&text, &sig)).await;
            if !matches!(kept, Ok(Ok(()))) {
                self.log.warn("could not keep the update's manifest in the home");
            }
        }
        let v = Some(m.version.as_str());
        match newer(&m.version, &self.version) {
            Some(true) => {}
            Some(false) if m.version == self.version => {
                return refused(v, format!("this helper is already at {}", m.version));
            }
            Some(false) => {
                return refused(
                    v,
                    format!(
                        "{} is older than this helper's {}: no downgrade",
                        m.version, self.version
                    ),
                );
            }
            None => {
                return refused(v, format!("{} does not compare with {}", m.version, self.version));
            }
        }
        let Some(asset) = m.assets.get(&self.target).cloned() else {
            return refused(
                v,
                format!("the update has no binary for this machine ({})", self.target),
            );
        };
        if let Some(f) = self.setup.dir.as_ref().and_then(UpdateDir::failed)
            && f.version == m.version
            && f.sha256 == asset.sha256
        {
            return refused(
                v,
                format!(
                    "{} failed here before ({}); delete helper/update/failed.json in the helper's \
                     home to let it try again",
                    m.version, f.message
                ),
            );
        }
        match self.policy {
            UpdatePolicy::Off => {
                return refused(v, "updates are off on this machine (updates: off)".to_owned());
            }
            UpdatePolicy::Notify => {
                return Decision::Say(
                    UpdateState::Available,
                    Some(m.version.clone()),
                    format!(
                        "CITAR helper {} is available; updates are set to notify on this machine",
                        m.version
                    ),
                );
            }
            UpdatePolicy::Auto if self.setup.packaged => {
                return refused(
                    v,
                    "this helper was installed by a package manager: update it through your \
                     package manager"
                        .to_owned(),
                );
            }
            UpdatePolicy::Auto => {}
        }
        match reqwest::Url::parse(&asset.url) {
            Ok(u) => {
                if let Err(why) = fetch::allowed(&u, self.setup.loopback_only) {
                    return refused(v, why);
                }
            }
            Err(e) => return refused(v, format!("the update's address does not read: {e}")),
        }
        let Some(install) = &self.setup.install else {
            return refused(
                v,
                "this helper does not know its own binary, so it cannot replace it".to_owned(),
            );
        };
        if self.setup.loopback_only && install.in_build_output() {
            return refused(
                v,
                format!(
                    "this development build runs from a build's output ({}): copy it elsewhere to \
                     try an update",
                    install.path().display()
                ),
            );
        }
        if let Err(why) = install.writable() {
            return refused(v, why);
        }
        if self.setup.dir.is_none() {
            return refused(v, "this helper has no home to download the update into".to_owned());
        }
        Decision::Fetch(m, asset)
    }

    /// Steps 4 and 5 on a task of their own.
    fn fetch(&mut self, manifest: Manifest, asset: Asset) {
        self.generation += 1;
        let generation = self.generation;
        let tx = self.progress_tx.clone();
        let Some(dir) = self.setup.dir.clone() else { return };
        let tls = self.setup.tls.clone();
        let loopback_only = self.setup.loopback_only;
        let downloads = self.setup.downloads.clone();
        let target = self.target.clone();
        self.pipeline = Some(tokio::spawn(async move {
            let version = manifest.version.clone();
            let status = |state, message: String| {
                Progress::Status(
                    generation,
                    UpdateStatus { state, version: Some(version.clone()), message: cut(&message) },
                )
            };
            let staged = pipeline(
                &manifest,
                &asset,
                &dir,
                tls,
                loopback_only,
                &downloads,
                &target,
                |st, m| {
                    let _sent = tx.send(status(st, m));
                },
            )
            .await;
            let _sent = tx.send(Progress::Done(generation, staged));
        }));
    }

    fn progress(&mut self, p: Progress) {
        match p {
            Progress::Status(g, st) if g == self.generation => {
                self.log.info(format!(
                    "update: {} {}: {}",
                    st.state.as_str(),
                    st.version.as_deref().unwrap_or(""),
                    st.message
                ));
                self.last = Some(st.clone());
                self.emit(st);
            }
            Progress::Done(g, staged) if g == self.generation => {
                self.pipeline = None;
                if let Some(s) = staged {
                    let _ready = self.ready.send(Some(s));
                }
            }
            _ => {}
        }
    }

    /// The new binary is healthy (its first welcome, a refusal it understood, or a minute without
    /// its server): `healthy.json` written and `pending.json` removed.
    async fn mark_healthy(&mut self) {
        if self.healthy || self.setup.updating.is_none() {
            return;
        }
        self.healthy = true;
        self.healthy_at = None;
        if let Some(dir) = self.setup.dir.clone() {
            let version = self.version.clone();
            let done = tokio::task::spawn_blocking(move || dir.mark_healthy(&version)).await;
            if !matches!(done, Ok(Ok(()))) {
                self.log.warn("could not mark this update healthy in the home");
            }
        }
        self.log.info(format!("this binary ({}) is healthy: the update is applied", self.version));
    }

    async fn welcome(&mut self) {
        if let Some(from) = self.setup.updating.clone() {
            self.mark_healthy().await;
            if !self.applied_sent {
                self.applied_sent = true;
                let message = format!(
                    "CITAR helper {} is running: the update from {from} is applied",
                    self.version
                );
                self.say(UpdateState::Applied, Some(self.version.clone()), &message);
            }
        }
        let Some(dir) = self.setup.dir.clone() else { return };
        let failed = tokio::task::spawn_blocking({
            let dir = dir.clone();
            move || dir.failed()
        })
        .await
        .ok()
        .flatten();
        if let Some(f) = failed.filter(|f| !f.reported) {
            self.say(
                UpdateState::Failed,
                Some(f.version.clone()),
                &format!("the update failed: {}", f.message),
            );
            let _marked = tokio::task::spawn_blocking(move || dir.failed_reported(&f)).await;
        }
    }
}

/// Sleeps until `at`, or for ever.
async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// Steps 4 and 5: the download and its checks, then the staged binary's self-test; each step said
/// through `say`. The staged binary, or `None` when a step failed (said already).
#[allow(clippy::too_many_arguments)]
async fn pipeline(
    manifest: &Manifest,
    asset: &Asset,
    dir: &UpdateDir,
    tls: Option<rustls::ClientConfig>,
    loopback_only: bool,
    downloads: &Downloads,
    target: &str,
    say: impl Fn(UpdateState, String),
) -> Option<Staged> {
    let client = match tls
        .ok_or_else(|| "no TLS set up".to_owned())
        .and_then(|t| fetch::client(t, loopback_only))
    {
        Ok(c) => c,
        Err(e) => {
            say(UpdateState::Failed, format!("the download could not start: {e}"));
            return None;
        }
    };
    say(
        UpdateState::Downloading,
        format!("downloading CITAR helper {} ({:.1} MB)", manifest.version, mb(asset.size)),
    );
    downloads.add();
    let (part, staged) = (dir.part(&asset.name), dir.staged(&asset.name));
    match fetch::download(&client, asset, &part, &staged, loopback_only).await {
        Ok(()) => {}
        Err(fetch::Fetch::Refused(why)) => {
            say(UpdateState::Refused, format!("the download was refused: {why}"));
            return None;
        }
        Err(fetch::Fetch::Failed(why)) => {
            say(UpdateState::Failed, format!("the download failed: {why}"));
            return None;
        }
    }
    if let Err(why) = self_test(&staged, manifest, target).await {
        let _gone = tokio::fs::remove_file(&staged).await;
        say(
            UpdateState::Failed,
            format!("the new binary failed its self-test, so it is not installed: {why}"),
        );
        return None;
    }
    say(
        UpdateState::Verified,
        format!(
            "CITAR helper {} is downloaded, checked and self-tested: it is installed when this \
             helper is idle",
            manifest.version
        ),
    );
    Some(Staged { path: staged, version: manifest.version.clone(), sha256: asset.sha256.clone() })
}

#[allow(clippy::cast_precision_loss)]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Step 5: runs `binary --self-test` and checks what it says against the signed manifest and this
/// target.
///
/// # Errors
/// Why the binary does not pass.
pub async fn self_test(binary: &Path, manifest: &Manifest, target: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--self-test")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: a helper without a console opens no window for it.
        cmd.creation_flags(0x0800_0000);
    }
    let out = match tokio::time::timeout(SELF_TEST_WAIT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("it could not be run: {e}")),
        Err(_) => return Err(format!("it did not finish within {} s", SELF_TEST_WAIT.as_secs())),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(said) = text.lines().find_map(BuildInfo::read_line) else {
        return Err(format!("it printed no version line ({})", clip(text.trim(), 200)));
    };
    let wanted = (manifest.version.as_str(), manifest.build.as_str(), manifest.protocol, target);
    let got = (said.version.as_str(), said.build.as_str(), said.protocol, said.target.as_str());
    if got != wanted {
        return Err(format!(
            "it says it is {} (build {}, protocol {}, target {}), and the signed manifest says {} \
             (build {}, protocol {}, target {})",
            got.0, got.1, got.2, got.3, wanted.0, wanted.1, wanted.2, wanted.3
        ));
    }
    if !out.status.success() || !text.lines().any(|l| l.trim() == "self-test: passed") {
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output");
        return Err(format!("{} ({})", clip(last.trim(), 300), out.status));
    }
    Ok(())
}
