//! What the binary does with its command line (design/phase4-helper.md P4.3.2): `--version`,
//! `--self-test` (the job host's canary and self-check, P4.4.3), `job-host` (the job host itself,
//! on its standard input and output: the helper's child, which reads no settings, writes no file
//! and dials nothing), `scan [FILE]` (the standalone collector: it reads the helper's settings for
//! its model endpoint, writes only FILE and dials nothing but that endpoint, for its runtimes),
//! `setup`, `update check|apply|rollback` (P4.3.8), `service install|uninstall|status|run`
//! (P4.3.9, [`crate::service`]), and `run` (the default; with `--detach` the Run supervisor started
//! in the background, [`crate::service::supervisor`]) with every check in front of it, in this
//! order, so that nothing is written or dialled before it is allowed:
//! 1. the development guard (`dev-guard` builds, and every build with a test feature): an
//!    explicit `--home` outside the per-user base, refused before anything else happens;
//! 2. an update's start path ([`crate::update::files::on_start`]), first thing once the home is
//!    known (P4.3.8: "reads `pending` first thing"): a new binary counts its start, and one that
//!    did not become healthy in three is rolled back (exit 75). It comes before the settings, the
//!    server's address and the log file, so a new binary that refuses any of them (a setting the
//!    old one took) still counts its starts; a command line it cannot read counts too in a
//!    service run ([`unread_start`]);
//! 3. the settings ([`config::resolve`], with the first-run questions at a terminal), and in a
//!    development build a loopback server, refused before anything is dialled;
//! 4. the machine key, refused when others can read it, and a warning for a `helper.json` they
//!    can;
//! 5. the session, until it is stopped (Ctrl-C, SIGTERM, the Windows service manager's stop),
//!    permanently refused, or swapped for an update's binary: a service run (`--service`, or a
//!    supervised one) then exits 75, a console run becomes the new binary's supervisor
//!    ([`crate::update::supervise`]).
//!
//! **In service mode nothing permanent exits** (P4.3.9): a server's permanent refusal idles in the
//! session, and so do settings this binary cannot take (step 3, a key others can read: what exits 2
//! from a console) unless an update is in progress, whose new binary must exit to have its starts
//! counted and be rolled back (P4.3.8). The helper logs why once, reads its settings again after
//! 15 s, then after twice as long each time up to an hour, and at once when its settings file
//! changes, and stops at once when asked, so no service manager restarts it into a loop every 15 s
//! and a passing cause (a file not readable yet at boot) costs seconds, not an hour.
//!
//! Ctrl-C and SIGTERM are listened for only once the session starts (and while a service idles):
//! before that, a Ctrl-C at the first-run questions or at "Press Enter to close" quits the helper.
//!
//! Exit codes (P4.3.9): 0 stopped, 78 a permanent refusal from a console, 75 restart for an
//! update, 2 a command line or settings it cannot take (or a key it refuses), 1 anything else that
//! stops it from starting.
//! A double-clicked binary that cannot start says why and waits for Enter before its window
//! closes, as protocol 1's frozen helper did.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use citar_engine::rules::Ruleset;

use crate::cli::{Cli, Command, ServiceAction, UpdateAction};
use crate::clock::Clock;
use crate::config::{self, Machine, Sources};
use crate::home::{self, Home};
use crate::key;
use crate::llm::HttpEndpoint;
use crate::log::Log;
use crate::net::{self, TcpDialer};
use crate::power::SystemPower;
use crate::protocol::Provider;
use crate::scan::{self, Probes, Runtime, SystemScanner};
use crate::session::{Exit, Facts, Helper, PERMANENT_RETRY, Sensors};
use crate::update::files::{self as update_files, Install, Start, UpdateDir};
use crate::update::{UpdateSetup, supervise};
use crate::version;

/// Whether this is a development or test build: loopback servers only, an explicit `--home`
/// outside the per-user base. P4.3.1: "a helper built with a test feature or `dev-guard` dials
/// loopback addresses only", so a build that cheats on purpose (`test-faults`, 4-11) or trusts an
/// update key given at run time (`test-update-key`, 4-13) can never reach a real server or write
/// a real installation's files, whether or not `dev-guard` was named with it.
pub const DEV_GUARD: bool =
    cfg!(any(feature = "dev-guard", feature = "test-faults", feature = "test-update-key"));

/// A failure that stops the helper before it runs, with its exit code.
struct Stop(u8, String);

/// Runs the command line; returns the exit code. `bare` is true when the binary was started with
/// no arguments at all, as a double-click starts it.
#[must_use]
pub fn main(cli: &Cli, bare: bool) -> u8 {
    match run(cli) {
        Ok(code) => code,
        Err(Stop(code, why)) => {
            eprintln!("citar-helper: {why}");
            if bare && config::interactive() {
                let mut t = config::Terminal;
                let _enter = config::Prompt::ask(&mut t, "Press Enter to close.");
            }
            code
        }
    }
}

fn env(name: &str) -> Option<std::ffi::OsString> {
    home::process_env(name)
}

fn version_line(cli: &Cli) -> String {
    let info = version::build_info();
    let mut line = info.line();
    // The key's fingerprint when the home holds one; a development build reads a home only when
    // it is named.
    let home = if DEV_GUARD {
        cli.opts.home.clone().map(Home::at)
    } else {
        home::resolve(cli.opts.home.as_deref(), &env).ok().map(|(h, _)| h)
    };
    if let Some(h) = home
        && let Ok(Some(k)) = key::load(&h.key_file())
    {
        line.push_str(&format!(", machine key {}", k.fingerprint()));
    }
    line
}

fn run(cli: &Cli) -> Result<u8, Stop> {
    let opts = &cli.opts;
    if opts.version {
        println!("{}", version_line(cli));
        return Ok(0);
    }
    if opts.self_test {
        return Ok(self_test());
    }
    if let Some(Command::Scan { file }) = &cli.command {
        return scan(cli, file.as_deref());
    }
    if cli.command == Some(Command::JobHost) {
        return Ok(job_host());
    }
    if DEV_GUARD {
        home::guard(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
    }
    if let Some(Command::Service { action }) = &cli.command {
        return service_command(cli, action);
    }
    let (home, _source) = home::resolve(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
    let log = Log::stderr(opts.verbose);
    match cli.command.clone().unwrap_or(Command::Run) {
        Command::Setup => setup(cli, &home),
        Command::Run | Command::Scan { .. } => {
            if opts.detach && !opts.supervisor {
                return detach(cli, &home);
            }
            if opts.supervisor {
                return Ok(crate::service::supervisor::main(&home, &log));
            }
            run_helper(cli, &home, log, opts.service, None)
        }
        Command::JobHost => Ok(job_host()),
        Command::Update { action } => update_command(cli, &home, &action, &log),
        Command::Service { .. } => Err(Stop(2, "service: handled above".to_owned())),
    }
}

/// `citar-helper service ...` ([`crate::service`]); `service run` is the helper itself, in service
/// mode (on Windows under the service manager).
fn service_command(cli: &Cli, action: &ServiceAction) -> Result<u8, Stop> {
    let log = Log::stderr(cli.opts.verbose);
    if *action == ServiceAction::Run {
        let (home, _source) =
            home::resolve(cli.opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
        return service_run(cli, &home, log);
    }
    crate::service::command(cli, action, &log).map_err(|(code, why)| Stop(code, why))
}

/// What `service run` says when a console, not the service manager, started it.
pub const SERVICE_RUN_FROM_A_CONSOLE: &str = "`service run` is the Windows service's own entry, \
     which the service manager starts (citar-helper service install --system installs it); to run \
     the helper here, leave out `service run`";

/// `service run` on Windows: the helper under the service manager, which starts it (the service
/// `citar-helper`'s command line) and stops it; its exit code is the service's.
#[cfg(windows)]
fn service_run(cli: &Cli, home: &Home, log: Log) -> Result<u8, Stop> {
    use std::sync::atomic::{AtomicU8, Ordering};
    let (tx, rx) = tokio::sync::watch::channel(false);
    let code = Arc::new(AtomicU8::new(0));
    let (cli2, home2, code2) = (cli.clone(), home.clone(), Arc::clone(&code));
    let main: Box<dyn FnOnce() -> u32 + Send> = Box::new(move || {
        let c = match run_helper(&cli2, &home2, log.clone(), true, Some(rx)) {
            Ok(c) => c,
            Err(Stop(c, why)) => {
                let _file = log.to_file(&home2.log_dir());
                log.error(why);
                c
            }
        };
        code2.store(c, Ordering::SeqCst);
        u32::from(c)
    });
    let stop: Box<dyn Fn() + Send + Sync> = Box::new(move || {
        let _sent = tx.send(true);
    });
    citar_jobs::sys::run_service(crate::service::NAME, stop, main).map_err(|e| {
        if e.raw_os_error() == Some(1063) {
            Stop(2, SERVICE_RUN_FROM_A_CONSOLE.to_owned())
        } else {
            Stop(1, format!("the service could not start: {e}"))
        }
    })?;
    Ok(code.load(Ordering::SeqCst))
}

/// `service run` elsewhere: the helper in service mode, as the systemd unit and the LaunchAgent run
/// it (`run --service`).
#[cfg(not(windows))]
fn service_run(cli: &Cli, home: &Home, log: Log) -> Result<u8, Stop> {
    run_helper(cli, home, log, true, None)
}

/// `run --detach`: the settings checked as `run` would read them (so a mistake is said here, not in
/// a log), then the Run supervisor started in the background, and this process gone.
fn detach(cli: &Cli, home: &Home) -> Result<u8, Stop> {
    let cfg = config::resolve(&Sources { opts: &cli.opts, env: &env, home }, None)
        .map_err(|e| Stop(2, e))?;
    let target = net::target(&cfg.server_url).map_err(|e| Stop(2, e))?;
    if DEV_GUARD {
        net::guard_loopback(&target).map_err(|e| Stop(2, e))?;
    }
    let pid = crate::service::supervisor::detach().map_err(|e| Stop(1, e))?;
    println!(
        "the helper runs in the background (its supervisor's pid {pid}); its logs are in {}",
        home.log_dir().display()
    );
    Ok(0)
}

/// What [`serve`] ended with: an exit code, or, after a console run swapped its binary for an
/// update's, the binary it now supervises.
enum Served {
    Code(u8),
    Supervise(Install),
}

/// Ctrl-C's and SIGTERM's listener ([`stop_on_signal`]), or none when the service manager stops the
/// helper: installed at most once in a process, and only when the session starts (or a service
/// idles). One installed before the settings are read would take a Ctrl-C at the first-run
/// questions for a stop request and leave the question waiting for an answer, where a terminal's
/// Ctrl-C there must quit the helper (as the default handler does until the listener is
/// installed); so must one at a refusal's "Press Enter to close".
struct Signals(Option<Box<dyn FnOnce() + Send>>);

impl Signals {
    /// The listener turning `tx` true, registered on `runtime` when [`Signals::listen`] is first
    /// called ([`listen_for_stop`]).
    fn on(
        runtime: &tokio::runtime::Runtime,
        tx: tokio::sync::watch::Sender<bool>,
        log: Log,
    ) -> Self {
        let handle = runtime.handle().clone();
        Signals(Some(Box::new(move || listen_for_stop(&handle, tx, log))))
    }

    /// Installs the listener, the first time only.
    fn listen(&mut self) {
        if let Some(install) = self.0.take() {
            install();
        }
    }
}

/// What every [`serve`] of one run of the helper shares.
struct Serving<'a> {
    cli: &'a Cli,
    home: &'a Home,
    started: &'a Start,
    runtime: &'a tokio::runtime::Runtime,
    stop: tokio::sync::watch::Receiver<bool>,
    service: bool,
    signals: Signals,
}

/// The first wait before a service reads settings it could not take again; each wait after it is
/// twice the one before, up to [`PERMANENT_RETRY`] (an hour). A cause that passes (a `helper.json`
/// not readable yet at boot, a key file's I/O error) is retried soon, and one that stays costs a
/// file read an hour: nothing is dialled while the settings are wrong.
const SETTINGS_RETRY_FIRST: Duration = Duration::from_secs(15);
/// How often an idling service looks at its settings file: a change is read at once, so a fixed
/// `helper.json` takes effect without a restart of the service.
const SETTINGS_POLL: Duration = Duration::from_secs(2);

/// The helper itself (`run`, `run --service`, `service run`): the update's start path first (the
/// module's doc, step 2), then [`serve`] on a runtime of its own, stopped by `given_stop` (the
/// Windows service manager's) or else by Ctrl-C and SIGTERM (listened for once the session starts,
/// [`Signals`]). In service mode, settings this binary cannot take idle rather than exit (the
/// module's doc), read again after 15 s doubling to an hour, and at once when the settings file
/// changes.
fn run_helper(
    cli: &Cli,
    home: &Home,
    log: Log,
    service: bool,
    given_stop: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<u8, Stop> {
    let started = start_path(home);
    if let Start::RolledBack(why) = &started {
        // The log file too, when it can be opened: the service's log says why.
        let _file = log.to_file(&home.log_dir());
        log.error(format!("the update is rolled back: {why}"));
        return Ok(Exit::Update.code());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| Stop(1, format!("could not start: {e}")))?;
    let (stop, signals) = if let Some(rx) = given_stop {
        (rx, Signals(None))
    } else {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (rx, Signals::on(&runtime, tx, log.clone()))
    };
    let updating = matches!(started, Start::Updating { .. });
    let settings = cli.opts.config.clone().unwrap_or_else(|| home.config_file());
    let mut serving =
        Serving { cli, home, started: &started, runtime: &runtime, stop, service, signals };
    let mut idle_logged = false;
    let mut wait = SETTINGS_RETRY_FIRST;
    // Only the first reading of the settings may ask the first-run questions: a service reading
    // them again has its stop listener by then.
    let mut ask = config::interactive();
    let outcome = loop {
        match serve(&mut serving, log.clone(), ask) {
            Err(Stop(2, why)) if service && !updating => {
                ask = false;
                // Idling, it must still stop when asked.
                serving.signals.listen();
                let _file = log.to_file(&home.log_dir());
                if idle_logged {
                    log.debug(&why);
                } else {
                    log.error(format!(
                        "{why}. Running as a service: idling; the settings are read again in {} s, \
                         then less often (at most hourly), and at once when {} changes (or restart \
                         the service once they are fixed).",
                        wait.as_secs(),
                        settings.display()
                    ));
                    idle_logged = true;
                }
                match idle_on_settings(&runtime, serving.stop.clone(), wait, &settings) {
                    Woke::Stopped => {
                        log.info("stopped");
                        break Ok(Served::Code(Exit::Stopped.code()));
                    }
                    Woke::Changed => {
                        log.info(format!(
                            "{} changed: reading the settings again",
                            settings.display()
                        ));
                        wait = SETTINGS_RETRY_FIRST;
                        // What the changed file still lacks is worth a line of its own.
                        idle_logged = false;
                    }
                    Woke::Elapsed => wait = (wait * 2).min(PERMANENT_RETRY),
                }
            }
            other => break other,
        }
    };
    drop(serving);
    runtime.shutdown_timeout(Duration::from_secs(2));
    match outcome? {
        Served::Code(code) => Ok(code),
        Served::Supervise(install) => {
            let update_dir = UpdateDir::new(home.update_dir());
            let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            Ok(supervise::supervise(&install, &update_dir, &args, &log))
        }
    }
}

/// Why an idling service woke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Woke {
    /// It was asked to stop.
    Stopped,
    /// Its wait ended.
    Elapsed,
    /// Its settings file changed (made, written, removed).
    Changed,
}

/// The settings file's modified time and length, `None` while it is missing: what a change of it
/// is seen by.
async fn stamp(file: &Path) -> Option<(Option<std::time::SystemTime>, u64)> {
    tokio::fs::metadata(file).await.ok().map(|m| (m.modified().ok(), m.len()))
}

/// Waits `wait`, until `stop` turns true, or until `settings` changes (looked at every
/// [`SETTINGS_POLL`]).
fn idle_on_settings(
    runtime: &tokio::runtime::Runtime,
    mut stop: tokio::sync::watch::Receiver<bool>,
    wait: Duration,
    settings: &Path,
) -> Woke {
    runtime.block_on(async move {
        let seen = stamp(settings).await;
        let changed = async {
            let mut tick = tokio::time::interval(SETTINGS_POLL);
            tick.tick().await; // the first tick is at once
            loop {
                tick.tick().await;
                if stamp(settings).await != seen {
                    return;
                }
            }
        };
        let stopped = async {
            if stop.wait_for(|s| *s).await.is_err() {
                // Nobody can stop it any more: the wait or a change ends the idle.
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => Woke::Elapsed,
            () = changed => Woke::Changed,
            () = stopped => Woke::Stopped,
        }
    })
}

/// An update's start path for the home `home` (P4.3.8): this binary's start counted when it is the
/// new one of an update in progress, and the old one restored after three that did not become
/// healthy.
fn start_path(home: &Home) -> Start {
    let install = Install::current().ok();
    update_files::on_start(&UpdateDir::new(home.update_dir()), install.as_ref(), version::VERSION)
}

/// A command line this binary cannot read, in a service run (`--service` or `--supervised` among
/// the raw `args`, the program's name left out): the start counts as any start does (the
/// update's start path, in the home the raw `--home` names, else the default home a release
/// build would use), so a new binary that refuses its service's command line (a flag a release
/// renamed) is rolled back after three starts, as one that refuses its settings is. A console
/// run's mistyped command line counts nothing: its owner sees the error, and a console run's
/// supervisor rolls a failing new binary back by itself. Returns 75 when the start rolled the
/// update back.
#[must_use]
pub fn unread_start(args: &[std::ffi::OsString]) -> Option<u8> {
    if !args.iter().any(|a| a == "--service" || a == "--supervised") {
        return None;
    }
    let mut named: Option<std::path::PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            named = it.next().map(std::path::PathBuf::from);
        } else if let Some(rest) = a.to_str().and_then(|t| t.strip_prefix("--home=")) {
            named = Some(std::path::PathBuf::from(rest));
        }
    }
    if DEV_GUARD {
        // A development build reads a home only when it is named, outside the per-user base.
        home::guard(named.as_deref(), &env).ok()?;
    }
    let (home, _source) = home::resolve(named.as_deref(), &env).ok()?;
    match start_path(&home) {
        Start::RolledBack(why) => {
            eprintln!("citar-helper: the update is rolled back: {why}");
            Some(Exit::Update.code())
        }
        Start::Normal | Start::Updating { .. } => None,
    }
}

/// The update keys this build trusts: `update-keys.pub`'s (none in Phase 4), and in a
/// `test-update-key` build `--update-key`'s.
fn update_keys(cli: &Cli) -> Result<Vec<citar_jobs::update::VerifyingKey>, Stop> {
    let keys = citar_jobs::update::trusted_keys().map_err(|e| Stop(1, e))?;
    given_keys(cli, keys)
}

/// `keys` and `--update-key`'s (a `test-update-key` build).
#[cfg(feature = "test-update-key")]
fn given_keys(
    cli: &Cli,
    mut keys: Vec<citar_jobs::update::VerifyingKey>,
) -> Result<Vec<citar_jobs::update::VerifyingKey>, Stop> {
    if let Some(file) = &cli.opts.update_key {
        let text = std::fs::read_to_string(file)
            .map_err(|e| Stop(2, format!("--update-key {}: {e}", file.display())))?;
        let more = citar_jobs::update::parse_keys(&text)
            .map_err(|e| Stop(2, format!("--update-key {}: {e}", file.display())))?;
        keys.extend(more);
    }
    Ok(keys)
}

/// `keys`: a build without `test-update-key` has no `--update-key`.
#[cfg(not(feature = "test-update-key"))]
#[allow(clippy::unnecessary_wraps)]
fn given_keys(
    _cli: &Cli,
    keys: Vec<citar_jobs::update::VerifyingKey>,
) -> Result<Vec<citar_jobs::update::VerifyingKey>, Stop> {
    Ok(keys)
}

/// A test build's hooks for the update gates (`test-update-key` only): how long a verified update
/// waits for the helper to be idle (`CITAR_HELPER_TEST_IDLE_WAIT_S`), the first wait before a
/// failed download is tried again (`CITAR_HELPER_TEST_RETRY_S`), whether this binary exits at
/// its start before it can become healthy (`CITAR_HELPER_TEST_EXIT_AT_START`, naming the version
/// that does: "a test build made to exit at start", 4-13's gate 3), and whether the binary an
/// update just installed starts its first job host from the one it replaced
/// (`CITAR_HELPER_TEST_STALE_HOST`: a job host left from before the swap, gate 4).
#[cfg(feature = "test-update-key")]
fn test_hooks() -> TestHooks {
    let seconds = |name: &str| {
        env(name)
            .and_then(|v| v.to_string_lossy().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0 && *s < 86_400.0)
            .map(std::time::Duration::from_secs_f64)
    };
    let exit = env("CITAR_HELPER_TEST_EXIT_AT_START").is_some_and(|v| v == version::VERSION);
    let stale = env("CITAR_HELPER_TEST_STALE_HOST").is_some_and(|v| !v.is_empty());
    TestHooks {
        idle_wait: seconds("CITAR_HELPER_TEST_IDLE_WAIT_S"),
        retry: seconds("CITAR_HELPER_TEST_RETRY_S"),
        exit_at_start: exit,
        stale_host: stale,
    }
}

#[cfg(not(feature = "test-update-key"))]
fn test_hooks() -> TestHooks {
    TestHooks::default()
}

/// [`test_hooks`]: none outside a `test-update-key` build.
#[derive(Clone, Copy, Debug, Default)]
struct TestHooks {
    idle_wait: Option<std::time::Duration>,
    retry: Option<std::time::Duration>,
    exit_at_start: bool,
    stale_host: bool,
}

/// The binary an update would replace, as this build may: a development build refuses one in a
/// build's output, so a test never overwrites what the next one runs.
fn own_install() -> Result<Install, Stop> {
    let install = Install::current().map_err(|e| Stop(1, e))?;
    if DEV_GUARD && install.in_build_output() {
        return Err(Stop(
            2,
            format!(
                "this development build runs from a build's output ({}): copy it elsewhere to try \
                 an update",
                install.path().display()
            ),
        ));
    }
    Ok(install)
}

/// `citar-helper update check|apply|rollback` (P4.3.8).
fn update_command(cli: &Cli, home: &Home, action: &UpdateAction, log: &Log) -> Result<u8, Stop> {
    let dir = UpdateDir::new(home.update_dir());
    match action {
        UpdateAction::Rollback => {
            let install = own_install()?;
            install.swap_with_old().map_err(|e| Stop(1, e))?;
            println!(
                "put {} back in place; the binary it replaced (CITAR helper {}) is kept as {}. \
                 Restart the helper (or its service) to run it.",
                install.old().display().to_string().trim_end_matches(".old"),
                version::VERSION,
                install.old().display()
            );
            let _stale = dir.remove_pending();
            // The release rolled away from is held back, as a failed update is: with `auto` the
            // next welcome would otherwise install it again within seconds. Already `reported`:
            // the owner did it, and the refusal of that release says so.
            let held = crate::update::fetch::sha256_file(&install.old())
                .map_err(|e| e.to_string())
                .and_then(|sha| {
                    dir.write_failed_as(
                        version::VERSION,
                        &sha,
                        &format!(
                            "{} was rolled back by hand (citar-helper update rollback)",
                            version::VERSION
                        ),
                        true,
                    )
                    .map_err(|e| e.to_string())
                });
            match held {
                Ok(()) => println!(
                    "This helper does not install CITAR helper {} again by itself; delete {} to \
                     let it.",
                    version::VERSION,
                    dir.path().join("failed.json").display()
                ),
                Err(e) => log.warn(format!(
                    "could not hold CITAR helper {} back ({e}): with updates set to auto it may be \
                     installed again at the next connection; set updates to notify or off",
                    version::VERSION
                )),
            }
            Ok(0)
        }
        UpdateAction::Check => {
            update_check(cli, home, &dir);
            Ok(0)
        }
        UpdateAction::Apply => update_apply(cli, &dir, log),
    }
}

/// `update check`'s `updates:` line: what this build does with an update its server relays. A build
/// that trusts no key reports every manifest `unavailable` whatever its policy (the release's
/// helpers until the owner's key is committed, design/phase5-release.md P5.4.2); a package
/// manager's build defaults to `notify` and refuses `auto` (P4.3.8), which it says rather than
/// letting `auto` read as if it would install.
fn updates_line(
    no_key: bool,
    packaged: bool,
    policy: &Result<crate::protocol::UpdatePolicy, String>,
) -> String {
    use crate::protocol::UpdatePolicy;
    let policy = match policy {
        Err(e) => format!("unknown ({e})"),
        Ok(UpdatePolicy::Auto) if packaged => {
            "auto, which a packaged build refuses: update it through your package manager"
                .to_owned()
        }
        Ok(UpdatePolicy::Notify) if packaged => {
            "notify (a packaged build: its package manager updates it)".to_owned()
        }
        Ok(p) if packaged => format!("{} (a packaged build)", p.as_str()),
        Ok(p) => p.as_str().to_owned(),
    };
    if no_key {
        format!("unavailable, this build trusts no update key; policy: {policy}")
    } else {
        policy
    }
}

/// `update check`: what this helper knows of updates.
fn update_check(cli: &Cli, home: &Home, dir: &UpdateDir) {
    println!("{}", version::build_info().line());
    let keys = update_keys(cli).unwrap_or_default();
    println!(
        "update keys trusted: {}{}",
        keys.len(),
        if keys.is_empty() { " (this build does not update itself)" } else { "" }
    );
    let policy = config::updates(&cli.opts, home);
    println!("updates: {}", updates_line(keys.is_empty(), cfg!(feature = "packaged"), &policy));
    match dir.kept_manifest() {
        None => println!("the server has relayed no update manifest to this home yet"),
        Some((text, sig)) => match citar_jobs::update::verify(text.as_bytes(), &sig, &keys) {
            Err(e) => println!("the last manifest relayed does not verify here: {e}"),
            Ok(m) => {
                let newer = citar_jobs::update::newer(&m.version, version::VERSION) == Some(true);
                println!(
                    "the last manifest relayed: CITAR helper {} (build {}, released {}), {}{}",
                    m.version,
                    m.build,
                    m.released_at,
                    if newer { "newer than this helper" } else { "not newer than this helper" },
                    if m.assets.contains_key(version::TARGET) {
                        String::new()
                    } else {
                        format!(", with no binary for {}", version::TARGET)
                    }
                );
            }
        },
    }
    if let Some(p) = dir.pending() {
        println!(
            "an update is in progress: {} to {} ({} start(s) of {})",
            p.old,
            p.new,
            p.starts,
            update_files::MAX_STARTS
        );
    }
    if let Some(f) = dir.failed()
        && citar_jobs::update::newer(&f.version, version::VERSION) == Some(true)
    {
        println!(
            "held back: {} was rolled back here ({}); delete {} to let it be installed again",
            f.version,
            f.message,
            dir.path().join("failed.json").display()
        );
    }
    if let Ok(i) = Install::current()
        && i.old().is_file()
    {
        println!("the binary the last update replaced is kept as {}", i.old().display());
    }
}

/// `update apply`: the relayed manifest's update installed now, as `auto` would (steps 1 to 5,
/// then the swap), for an owner whose policy is `notify`.
fn update_apply(cli: &Cli, dir: &UpdateDir, log: &Log) -> Result<u8, Stop> {
    if cfg!(feature = "packaged") {
        return Err(Stop(
            2,
            "this helper was installed by a package manager: update it through your package manager"
                .to_owned(),
        ));
    }
    let keys = update_keys(cli)?;
    if keys.is_empty() {
        return Err(Stop(
            2,
            "this build trusts no update key, so it does not update itself".to_owned(),
        ));
    }
    let Some((text, sig)) = dir.kept_manifest() else {
        return Err(Stop(
            2,
            "the server has relayed no update manifest to this home yet".to_owned(),
        ));
    };
    let m = citar_jobs::update::verify(text.as_bytes(), &sig, &keys)
        .map_err(|e| Stop(2, format!("the update was refused: {e}")))?;
    if citar_jobs::update::newer(&m.version, version::VERSION) != Some(true) {
        return Err(Stop(
            2,
            format!("{} is not newer than this helper's {}", m.version, version::VERSION),
        ));
    }
    let asset = m
        .assets
        .get(version::TARGET)
        .cloned()
        .ok_or_else(|| Stop(2, format!("the update has no binary for {}", version::TARGET)))?;
    let url = reqwest::Url::parse(&asset.url)
        .map_err(|e| Stop(2, format!("the update's address: {e}")))?;
    crate::update::fetch::allowed(&url, DEV_GUARD).map_err(|e| Stop(2, e))?;
    let install = own_install()?;
    install.writable().map_err(|e| Stop(2, e))?;
    dir.secure().map_err(|e| Stop(2, e))?;
    let tls = net::verified_tls(cli.opts.ca_file.as_deref()).map_err(|e| Stop(1, e))?;
    let client = crate::update::fetch::client(tls, DEV_GUARD).map_err(|e| Stop(1, e))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Stop(1, format!("could not start: {e}")))?;
    let (part, staged) = (dir.part(&asset.name), dir.staged(&asset.name));
    println!("downloading CITAR helper {} from {}", m.version, asset.url);
    rt.block_on(async {
        crate::update::fetch::download(&client, &asset, &part, &staged, DEV_GUARD).await.map_err(
            |e| match e {
                crate::update::fetch::Fetch::Refused(w) => {
                    Stop(2, format!("the download was refused: {w}"))
                }
                crate::update::fetch::Fetch::Failed(w) => {
                    Stop(1, format!("the download failed: {w}"))
                }
            },
        )?;
        crate::update::self_test(&staged, &m, version::TARGET).await.map_err(|e| {
            let _gone = std::fs::remove_file(&staged);
            Stop(1, format!("the new binary failed its self-test, so it is not installed: {e}"))
        })
    })?;
    dir.begin(version::VERSION, &m.version, &asset.sha256)
        .map_err(|e| Stop(1, format!("{}: {e}", dir.path().display())))?;
    if let Err(e) = install.swap_in(&staged, &asset.sha256) {
        let _gone = dir.remove_pending();
        return Err(Stop(1, e));
    }
    let _staged_gone = std::fs::remove_file(&staged);
    log.info(format!("installed CITAR helper {} in {}", m.version, install.path().display()));
    println!(
        "installed CITAR helper {}; the binary it replaced is kept as {}. Restart the helper (or \
         its service) to run it.",
        m.version,
        install.old().display()
    );
    Ok(0)
}

/// The faults `--test-fault` asks this test build to play (P4.4.6), said in the log; none in a
/// build without the `test-faults` feature, which has no such flag.
#[cfg(feature = "test-faults")]
fn faults(cli: &Cli, log: &Log) -> Result<crate::faults::Faults, Stop> {
    let f = crate::faults::Faults::parse(&cli.opts.test_faults).map_err(|e| Stop(2, e))?;
    if f.active() {
        log.warn(format!("TEST FAULTS: this helper cheats on purpose: {}", f.describe()));
    }
    Ok(f)
}

/// No fault in a shipped build.
#[cfg(not(feature = "test-faults"))]
#[allow(clippy::unnecessary_wraps)]
fn faults(_cli: &Cli, _log: &Log) -> Result<crate::faults::Faults, Stop> {
    Ok(crate::faults::Faults)
}

/// `citar-helper --self-test`: the build, then the canary and the self-check a job host runs
/// before it takes work (P4.4.3), on a thread made as its run threads are. 0 when both pass, 1
/// when either fails: this machine would refuse every job (`unverifiable`).
fn self_test() -> u8 {
    println!("{}", version::build_info().line());
    match citar_jobs::host::self_test(Ruleset::shared(), citar_jobs::canary::Inject::default()) {
        Ok(c) => {
            println!("canary: ok");
            println!(
                "self-check: ok ({}: {} rounds, chain {}, result {}, {:.2} s)",
                c.job, c.rounds, c.chain_head, c.result_hash, c.wall_s
            );
            println!("self-test: passed");
            0
        }
        Err(why) => {
            println!("self-test: FAILED: {why}");
            1
        }
    }
}

/// `citar-helper job-host`: the job host on this process's standard input and output, under the
/// limits its first frame gives (P4.4.3); its exit code is the host's.
fn job_host() -> u8 {
    // This binary's own jobs build (a test build that calls itself another version has another).
    let setup = citar_jobs::host::Setup {
        jobs_build: Some(version::jobs_build()),
        ..citar_jobs::host::Setup::process()
    };
    let code = citar_jobs::host::serve(std::io::stdin().lock(), std::io::stdout(), setup);
    u8::try_from(code).unwrap_or(1)
}

/// How long `scan` waits for its model endpoint's list.
const SCAN_LIST: std::time::Duration = std::time::Duration::from_secs(5);

/// The runtime `scan` reports, as the running helper's scans report it: its endpoint (from its
/// settings), when that is LM Studio or Ollama and answers within [`SCAN_LIST`], with the models
/// it lists within the allowlist ([`scan::runtime_models`]); none with `no_llm`.
fn scan_runtimes(ep: &config::EndpointSettings) -> Vec<Runtime> {
    if ep.no_llm || !matches!(ep.provider, Provider::LmStudio | Provider::Ollama) {
        return Vec::new();
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Vec::new();
    };
    let listed = rt.block_on(async {
        let tls = net::verified_tls(None).ok()?;
        let key = api_key(&ep.api_key_env, ep.provider);
        let endpoint = HttpEndpoint::new(ep.provider, &ep.base_url, key, tls).ok()?;
        tokio::time::timeout(SCAN_LIST, crate::llm::Endpoint::list(&endpoint)).await.ok()?.ok()
    });
    let Some(listed) = listed else { return Vec::new() };
    let models = scan::runtime_models(&listed, ep.models.as_deref());
    scan::endpoint_runtime(ep.provider, &ep.base_url, models).into_iter().collect()
}

/// `citar-helper scan [FILE]`: this machine's hardware report, printed or written to `FILE`, as
/// `python collect_hardware.py` gives it, to import on the Servers page, and as the helper with
/// these settings would report it: its model endpoint, `models` allowlist and `no_llm` are read
/// from the flags, the home's `helper.json` (or `--config`) and the variables, as `run` reads
/// them, and nothing is written but `FILE`. A development build reads a home only when it is
/// named (outside the per-user folder, as `run` requires).
fn scan(cli: &Cli, file: Option<&Path>) -> Result<u8, Stop> {
    let opts = &cli.opts;
    let home = if DEV_GUARD {
        match opts.home.as_deref() {
            Some(h) => {
                home::guard(Some(h), &env).map_err(|e| Stop(2, e))?;
                Some(Home::at(h))
            }
            None => None,
        }
    } else {
        Some(home::resolve(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?.0)
    };
    let endpoint = config::endpoint(opts, &env, home.as_ref()).map_err(|e| Stop(2, e))?;
    let probes = Probes::from_env(&env, DEV_GUARD);
    let runtimes = scan_runtimes(&endpoint);
    let at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let info = scan::collect(&probes, &runtimes, &at);
    let text = serde_json::to_string_pretty(&info)
        .map_err(|e| Stop(1, format!("could not write the report: {e}")))?;
    match file {
        None => println!("{text}"),
        Some(f) => {
            std::fs::write(f, format!("{text}\n"))
                .map_err(|e| Stop(1, format!("{}: {e}", f.display())))?;
            eprintln!("Wrote {}. Import it on CITAR's Servers page.", f.display());
        }
    }
    Ok(0)
}

/// `citar-helper setup`: the first-run questions, also to give the helper a new token; the other
/// settings of the file are kept.
fn setup(cli: &Cli, home: &Home) -> Result<u8, Stop> {
    let path = cli.opts.config.clone().unwrap_or_else(|| home.config_file());
    let data = if path.exists() {
        config::read_file(&path).map_err(|e| Stop(2, e))?
    } else {
        Default::default()
    };
    let mut t = config::Terminal;
    config::first_run(&mut t, data, &path, false).map_err(|e| Stop(2, e))?;
    Ok(0)
}

fn facts() -> (Facts, Machine) {
    let info = version::build_info();
    let rules = citar_engine::rules::Ruleset::shared();
    let mut bots: Vec<String> =
        citar_bot::versions().iter().map(|v| v.id.id().to_owned()).collect();
    bots.sort();
    let sys = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing()
            .with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
    );
    let cpus =
        std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(1));
    let machine = Machine { logical_cpus: cpus, memory_mb: sys.total_memory() / (1024 * 1024) };
    (
        Facts {
            hostname: sysinfo::System::host_name().unwrap_or_default(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            build: info.build,
            version: info.version.to_owned(),
            target: info.target.to_owned(),
            label: info.label.to_owned(),
            bots,
            ruleset: rules.id().to_hex(),
        },
        machine,
    )
}

/// The helper, once: its settings read (with the first-run questions when `ask`), checked, and
/// served until it stops. Ctrl-C and SIGTERM are listened for from the moment the session starts
/// ([`Signals`]): before it, at the first-run questions and at a refusal's "Press Enter to close",
/// a Ctrl-C quits the helper, as a terminal's Ctrl-C does.
fn serve(s: &mut Serving<'_>, log: Log, ask: bool) -> Result<Served, Stop> {
    let (cli, home, started, runtime, service) = (s.cli, s.home, s.started, s.runtime, s.service);
    let stop = s.stop.clone();
    let mut terminal = config::Terminal;
    let prompt: Option<&mut dyn config::Prompt> = if ask { Some(&mut terminal) } else { None };
    let cfg = config::resolve(&Sources { opts: &cli.opts, env: &env, home }, prompt)
        .map_err(|e| Stop(2, e))?;
    log.add_secret(&cfg.token);
    // The server is checked before anything is written or dialled.
    let target = net::target(&cfg.server_url).map_err(|e| Stop(2, e))?;
    if DEV_GUARD {
        net::guard_loopback(&target).map_err(|e| Stop(2, e))?;
    }
    log.to_file(&home.log_dir())
        .map_err(|e| Stop(1, format!("{}: {e}", home.log_dir().display())))?;
    for w in &cfg.warnings {
        log.warn(w);
    }
    let config_path = cli.opts.config.clone().unwrap_or_else(|| home.config_file());
    if let Some(w) = key::config_warning(&config_path) {
        log.warn(w);
    }
    // The update's start path ran first ([`run`]); what it found is logged now that the log
    // file is open.
    let install = Install::current().ok();
    let update_dir = UpdateDir::new(home.update_dir());
    let updating = match started {
        Start::Updating { from, starts } => {
            log.info(format!(
                "this is CITAR helper {} after an update from {from} (start {starts} of {})",
                version::VERSION,
                update_files::MAX_STARTS
            ));
            Some(from.clone())
        }
        Start::Normal | Start::RolledBack(_) => None,
    };
    let hooks = test_hooks();
    if hooks.exit_at_start {
        log.warn("TEST: this build exits at its start (CITAR_HELPER_TEST_EXIT_AT_START)");
        return Ok(Served::Code(1));
    }
    let keys = update_keys(cli)?;
    let (machine_key, made) = key::load_or_create(home).map_err(|e| Stop(2, e.to_string()))?;
    if made {
        log.info(format!(
            "made this machine's key, fingerprint {} (the server registers it at the first connection)",
            machine_key.fingerprint()
        ));
    }
    let (facts, machine) = facts();
    let ceiling = cfg.ceiling(machine);
    let probes = Probes::from_env(&env, DEV_GUARD);
    if DEV_GUARD {
        for var in scan::PROBE_VARS {
            if let Some(v) = env(var).filter(|v| !v.is_empty()) {
                log.info(format!("{var} names the probe {}", v.to_string_lossy()));
            }
        }
    }
    let sensors = Sensors {
        scanner: (cfg.collect_hardware && cfg.hardware_file.is_empty()).then(|| {
            Arc::new(SystemScanner { probes: probes.clone(), clock: Clock::system() })
                as Arc<dyn scan::Scanner>
        }),
        power: (!cfg.no_power)
            .then(|| Arc::new(SystemPower::new(probes)) as Arc<dyn crate::power::PowerProbe>),
        cache: Some(home.scan_file()),
    };
    let reports = if !cfg.collect_hardware {
        "none (--no-hardware)".to_owned()
    } else if cfg.hardware_file.is_empty() {
        "this machine's scan".to_owned()
    } else {
        format!("the file {}", cfg.hardware_file)
    };
    let endpoint: Option<crate::llm::SharedEndpoint> = if cfg.no_llm {
        None
    } else {
        let api_key = api_key(&cfg.api_key_env, cfg.provider);
        if let Some(k) = &api_key {
            log.add_secret(k);
        }
        let tls = net::verified_tls(None).map_err(|e| Stop(1, e))?;
        Some(Arc::new(
            HttpEndpoint::new(cfg.provider, &cfg.base_url, api_key, tls).map_err(|e| Stop(1, e))?,
        ))
    };
    let dialer =
        TcpDialer::new(target.clone(), cfg.ca_file.as_deref(), cfg.insecure, DEV_GUARD, &log)
            .map_err(|e| Stop(2, e))?;

    log.info(format!("CITAR helper {} {}", facts.version, cfg.name).trim_end());
    log.info(format!("  server : {}", cfg.server_url));
    if cfg.no_llm {
        log.info("  models : none (--no-llm)");
    } else {
        log.info(format!("  models : {} ({})", cfg.base_url, cfg.provider.as_str()));
        log.info(format!("  slots  : {}", cfg.max_concurrent));
    }
    log.info(format!(
        "  cpu    : {}",
        if cfg.cpu.enabled {
            "lent, within this machine's own ceiling"
        } else {
            "not lent (its owner has not turned CPU lending on here)"
        }
    ));
    if !cfg.quiet.is_empty() {
        log.info(format!("  quiet  : {} window(s), enforced here", cfg.quiet.len()));
    }
    log.info(format!("  report : {reports}"));
    log.info(format!(
        "  power  : {}",
        if cfg.no_power { "not sampled (--no-power)" } else { "sampled every 10 s" }
    ));
    log.info(format!("  home   : {}", home.root().display()));
    log.info(format!("  key    : {}", machine_key.fingerprint()));
    log.info(format!(
        "  update : {}",
        if keys.is_empty() {
            "unavailable (this build trusts no update key)".to_owned()
        } else {
            format!("{} ({} key(s) trusted)", cfg.updates.as_str(), keys.len())
        }
    ));
    if cfg!(feature = "packaged") && cfg.updates == crate::protocol::UpdatePolicy::Auto {
        log.warn("this helper was installed by a package manager: updates: auto is refused here; update it through your package manager");
    }

    // CPU jobs: this binary's own job hosts as children (`job-host`), results signed by the
    // machine key and kept in the home until acknowledged.
    let machine_key = Arc::new(machine_key);
    let hosts = crate::jobhost::ChildHosts::this_binary(log.clone()).map_err(|e| Stop(1, e))?;
    if hooks.stale_host
        && updating.is_some()
        && let Some(i) = &install
    {
        *hosts.stale.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(i.old());
    }
    let jobs = crate::jobs::JobSetup {
        hosts: Some(Arc::new(hosts) as Arc<dyn crate::jobs::Hosts>),
        key: Some(Arc::clone(&machine_key)),
        results: Some(home.results_dir()),
        state: crate::jobs::JobState::default(),
        faults: faults(cli, &log)?,
    };
    let update = UpdateSetup {
        keys,
        packaged: cfg!(feature = "packaged"),
        install: install.clone(),
        dir: Some(update_dir.clone()),
        tls: Some(net::verified_tls(cfg.ca_file.as_deref()).map_err(|e| Stop(1, e))?),
        loopback_only: DEV_GUARD,
        idle_wait: hooks.idle_wait.unwrap_or(crate::update::IDLE_WAIT),
        retry: hooks.retry.unwrap_or(crate::update::RETRY_FIRST),
        updating,
        downloads: crate::update::Downloads::default(),
    };
    // Every check has passed and nothing asks a question any more: a Ctrl-C (or SIGTERM) from
    // here on is a stop request, which the session ends on.
    s.signals.listen();
    let code = runtime.block_on(async {
        let helper = Helper {
            config: cfg,
            public_key: machine_key.public_b64(),
            facts,
            ceiling,
            log: log.clone(),
            clock: Clock::system(),
            dialer: Arc::new(dialer),
            endpoint,
            service,
            stop,
            in_flight: crate::proxy::InFlight::default(),
            sensors,
            jobs,
            update,
        };
        helper.run().await
    });
    if code != Exit::Update {
        log.info(if code == Exit::Stopped { "stopped" } else { "stopped: permanently refused" });
        return Ok(Served::Code(code.code()));
    }
    if service || cli.opts.supervised {
        log.info("exiting 75: the service manager (or the supervisor) starts the update's binary");
        return Ok(Served::Code(Exit::Update.code()));
    }
    let Some(install) = install else { return Ok(Served::Code(Exit::Update.code())) };
    log.info("the update's binary is in place: this console run supervises it from now on");
    Ok(Served::Supervise(install))
}

/// The API key the endpoint is given: the variable `api_key_env` names, else for `openai` the
/// standard `OPENAI_API_KEY`; no other endpoint is sent a key (protocol 1 sent `OPENAI_API_KEY` to
/// any endpoint).
fn api_key(api_key_env: &str, provider: Provider) -> Option<String> {
    let var = if !api_key_env.is_empty() {
        api_key_env
    } else if provider == Provider::OpenAi {
        "OPENAI_API_KEY"
    } else {
        return None;
    };
    std::env::var(var).ok().filter(|k| !k.is_empty())
}

/// Ctrl-C, and SIGTERM on Unix (what a service manager sends), registered with the OS when this is
/// called (in a runtime's context): the future it returns ends at the first of them. From the
/// call on, those signals no longer end the process by themselves; one the OS cannot register is
/// left as it was (without a console there is still SIGTERM, or the service manager's stop).
fn stop_signals() -> impl std::future::Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    let (mut first, mut second) = {
        use tokio::signal::unix::{SignalKind, signal};
        (signal(SignalKind::interrupt()).ok(), signal(SignalKind::terminate()).ok())
    };
    #[cfg(windows)]
    let (mut first, mut second) =
        (tokio::signal::windows::ctrl_c().ok(), None::<tokio::signal::windows::CtrlC>);
    async move {
        #[cfg(any(unix, windows))]
        {
            let one = async {
                match first.as_mut() {
                    Some(s) => {
                        let _got = s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let other = async {
                match second.as_mut() {
                    Some(s) => {
                        let _got = s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = one => {}
                () = other => {}
            }
        }
        #[cfg(not(any(unix, windows)))]
        std::future::pending::<()>().await;
    }
}

/// Ctrl-C, and SIGTERM on Unix (what a service manager sends), stop the helper (and the Run
/// supervisor): `stop` turns true at the first of them.
pub(crate) async fn stop_on_signal(stop: tokio::sync::watch::Sender<bool>, log: Log) {
    stop_signals().await;
    log.info("stopping");
    let _sent = stop.send(true);
}

/// [`stop_on_signal`] with the signals registered before this returns, on `handle`'s runtime: from
/// then on they are stop requests (what [`Signals::listen`] promises).
fn listen_for_stop(
    handle: &tokio::runtime::Handle,
    stop: tokio::sync::watch::Sender<bool>,
    log: Log,
) {
    let _context = handle.enter();
    let signals = stop_signals();
    let _listening = handle.spawn(async move {
        signals.await;
        log.info("stopping");
        let _sent = stop.send(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("citar-helper-app-{}-{name}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("a scratch directory");
        d
    }

    /// `update check`'s `updates:` line: unavailable without a key whatever the policy; a packaged
    /// build's notify says so, and its auto is said to be refused (the run's warning, P4.3.8).
    #[test]
    fn update_checks_line_says_unavailable_and_a_packaged_builds_refusal() {
        use crate::protocol::UpdatePolicy::{Auto, Notify, Off};
        let line = |no_key, packaged, p| updates_line(no_key, packaged, &Ok(p));
        assert_eq!(line(false, false, Auto), "auto");
        assert_eq!(line(false, false, Notify), "notify");
        assert_eq!(line(false, true, Off), "off (a packaged build)");
        assert_eq!(
            line(false, true, Notify),
            "notify (a packaged build: its package manager updates it)"
        );
        assert!(line(false, true, Auto).starts_with("auto, which a packaged build refuses"));
        let none = line(true, true, Notify);
        assert!(
            none.starts_with("unavailable, this build trusts no update key; policy: notify"),
            "{none}"
        );
        assert!(line(true, false, Auto).ends_with("policy: auto"));
        let unknown = updates_line(false, false, &Err("helper.json: updates must be".to_owned()));
        assert_eq!(unknown, "unknown (helper.json: updates must be)");
    }

    /// Ctrl-C's listener is installed only when the session starts: not before the settings are
    /// read (a Ctrl-C at the first-run questions must quit, 4-14's review), nor when a check after
    /// them refuses (a double-clicked helper then waits at "Press Enter to close", where a Ctrl-C
    /// must quit too); and only once.
    #[test]
    fn no_stop_listener_before_the_session() {
        let dir = scratch("signals");
        let home = Home::at(&dir);
        let args = [
            std::ffi::OsString::from("citar-helper"),
            "run".into(),
            "--home".into(),
            dir.clone().into(),
        ];
        let cli = Cli::try_parse_from(args).expect("parsed");
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        let installed = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&installed);
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let started = Start::Normal;
        let mut serving = Serving {
            cli: &cli,
            home: &home,
            started: &started,
            runtime: &runtime,
            stop,
            service: false,
            signals: Signals(Some(Box::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            }))),
        };
        // No settings: refused at the settings, nothing else done.
        let Err(Stop(code, why)) = serve(&mut serving, Log::memory(), false) else {
            panic!("refused without settings")
        };
        assert_eq!(code, 2, "{why}");
        assert_eq!(installed.load(Ordering::SeqCst), 0, "no listener before the settings are read");
        // Settings it takes, and a check after them that refuses: the log's folder cannot be made
        // (a file stands where `helper/` goes).
        std::fs::write(
            home.config_file(),
            r#"{"server_url": "http://127.0.0.1:9", "token": "cwt_app_test_0123456789abcdef", "no_llm": true}"#,
        )
        .expect("helper.json");
        std::fs::write(home.state_dir(), b"a file where the folder goes").expect("written");
        let Err(Stop(code, why)) = serve(&mut serving, Log::memory(), false) else {
            panic!("refused at the log's folder")
        };
        assert_eq!(code, 1, "{why}");
        assert!(why.contains("logs"), "{why}");
        assert_eq!(installed.load(Ordering::SeqCst), 0, "no listener when a check refuses");
        serving.signals.listen();
        serving.signals.listen();
        assert_eq!(installed.load(Ordering::SeqCst), 1, "installed once");
        drop(serving);
        let _cleaned = std::fs::remove_dir_all(&dir);
    }
}
