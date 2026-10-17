//! What the binary does with its command line (design/phase4-helper.md P4.3.2): `--version`,
//! `--self-test` (the job host's canary and self-check, P4.4.3), `job-host` (the job host itself,
//! on its standard input and output: the helper's child, which reads no settings, writes no file
//! and dials nothing), `scan [FILE]` (the standalone collector: it reads the helper's settings for
//! its model endpoint, writes only FILE and dials nothing but that endpoint, for its runtimes),
//! `setup`, `update check|apply|rollback` (P4.3.8), and `run` (the default) with
//! every check in front of it, in this order, so that nothing is written or dialled before it is
//! allowed:
//! 1. the development guard (`dev-guard` builds, and every build with a test feature): an
//!    explicit `--home` outside the per-user base, and a loopback server, both refused before
//!    anything else happens;
//! 2. the settings ([`config::resolve`], with the first-run questions at a terminal);
//! 3. the machine key, refused when others can read it, and a warning for a `helper.json` they
//!    can;
//! 4. an update's start path ([`crate::update::files::on_start`]): a new binary counts its start,
//!    and one that did not become healthy in three is rolled back (exit 75);
//! 5. the session, until it is stopped (Ctrl-C, SIGTERM), permanently refused, or swapped for an
//!    update's binary: a service run (`--service`, or a supervised one) then exits 75, a console
//!    run becomes the new binary's supervisor ([`crate::update::supervise`]).
//!
//! Exit codes (P4.3.9): 0 stopped, 78 a permanent refusal from a console, 75 restart for an
//! update, 2 a command line or settings it cannot take (or a key it refuses), 1 anything else that
//! stops it from starting.
//! A double-clicked binary that cannot start says why and waits for Enter before its window
//! closes, as protocol 1's frozen helper did.

use std::path::Path;
use std::sync::Arc;

use citar_engine::rules::Ruleset;

use crate::cli::{Cli, Command, UpdateAction};
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
use crate::session::{Exit, Facts, Helper, Sensors};
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
    let (home, _source) = home::resolve(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
    let log = Log::stderr(opts.verbose);
    match cli.command.clone().unwrap_or(Command::Run) {
        Command::Setup => setup(cli, &home),
        Command::Run | Command::Scan { .. } => serve(cli, &home, log),
        Command::JobHost => Ok(job_host()),
        Command::Update { action } => update_command(cli, &home, &action, &log),
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
/// waits for the helper to be idle (`CITAR_HELPER_TEST_IDLE_WAIT_S`), whether this binary exits at
/// its start before it can become healthy (`CITAR_HELPER_TEST_EXIT_AT_START`, naming the version
/// that does: "a test build made to exit at start", 4-13's gate 3), and whether the binary an
/// update just installed starts its first job host from the one it replaced
/// (`CITAR_HELPER_TEST_STALE_HOST`: a job host left from before the swap, gate 4).
#[cfg(feature = "test-update-key")]
fn test_hooks() -> TestHooks {
    let wait = env("CITAR_HELPER_TEST_IDLE_WAIT_S")
        .and_then(|v| v.to_string_lossy().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(std::time::Duration::from_secs_f64);
    let exit = env("CITAR_HELPER_TEST_EXIT_AT_START").is_some_and(|v| v == version::VERSION);
    let stale = env("CITAR_HELPER_TEST_STALE_HOST").is_some_and(|v| !v.is_empty());
    TestHooks { idle_wait: wait, exit_at_start: exit, stale_host: stale }
}

#[cfg(not(feature = "test-update-key"))]
fn test_hooks() -> TestHooks {
    TestHooks::default()
}

/// [`test_hooks`]: none outside a `test-update-key` build.
#[derive(Clone, Copy, Debug, Default)]
struct TestHooks {
    idle_wait: Option<std::time::Duration>,
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
                "put {} back in place; the binary it replaced is kept as {}. Restart the helper \
                 (or its service) to run it.",
                install.old().display().to_string().trim_end_matches(".old"),
                install.old().display()
            );
            let _stale = dir.remove_pending();
            Ok(0)
        }
        UpdateAction::Check => {
            update_check(cli, home, &dir);
            Ok(0)
        }
        UpdateAction::Apply => update_apply(cli, &dir, log),
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
    let policy = config::resolve(&Sources { opts: &cli.opts, env: &env, home }, None)
        .map_or_else(|e| format!("unknown ({e})"), |c| c.updates.as_str().to_owned());
    println!(
        "updates: {policy}{}",
        if cfg!(feature = "packaged") { " (a packaged build)" } else { "" }
    );
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
    if let Some(f) = dir.failed() {
        println!("an update was rolled back: {} ({})", f.version, f.message);
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
    if let Err(e) = install.swap_in(&staged) {
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

fn serve(cli: &Cli, home: &Home, log: Log) -> Result<u8, Stop> {
    let prompt_ok = config::interactive();
    let mut terminal = config::Terminal;
    let prompt: Option<&mut dyn config::Prompt> =
        if prompt_ok { Some(&mut terminal) } else { None };
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
    // An update's start path, first thing after the checks (P4.3.8).
    let install = Install::current().ok();
    let update_dir = UpdateDir::new(home.update_dir());
    let updating = match update_files::on_start(&update_dir, install.as_ref(), version::VERSION) {
        Start::Normal => None,
        Start::Updating { from, starts } => {
            log.info(format!(
                "this is CITAR helper {} after an update from {from} (start {starts} of {})",
                version::VERSION,
                update_files::MAX_STARTS
            ));
            Some(from)
        }
        Start::RolledBack(why) => {
            log.error(format!("the update is rolled back: {why}"));
            return Ok(Exit::Update.code());
        }
    };
    let hooks = test_hooks();
    if hooks.exit_at_start {
        log.warn("TEST: this build exits at its start (CITAR_HELPER_TEST_EXIT_AT_START)");
        return Ok(1);
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
        updating,
        downloads: crate::update::Downloads::default(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| Stop(1, format!("could not start: {e}")))?;
    let code = runtime.block_on(async {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(stop_on_signal(stop_tx, log.clone()));
        let helper = Helper {
            config: cfg,
            public_key: machine_key.public_b64(),
            facts,
            ceiling,
            log: log.clone(),
            clock: Clock::system(),
            dialer: Arc::new(dialer),
            endpoint,
            service: cli.opts.service,
            stop: stop_rx,
            in_flight: crate::proxy::InFlight::default(),
            sensors,
            jobs,
            update,
        };
        helper.run().await
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    if code != Exit::Update {
        log.info(if code == Exit::Stopped { "stopped" } else { "stopped: permanently refused" });
        return Ok(code.code());
    }
    if cli.opts.service || cli.opts.supervised {
        log.info("exiting 75: the service manager (or the supervisor) starts the update's binary");
        return Ok(Exit::Update.code());
    }
    let Some(install) = install else { return Ok(Exit::Update.code()) };
    log.info("the update's binary is in place: this console run supervises it from now on");
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    Ok(supervise::supervise(&install, &update_dir, &args, &log))
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

/// Ctrl-C, and SIGTERM on Unix (what a service manager sends), stop the helper.
async fn stop_on_signal(stop: tokio::sync::watch::Sender<bool>, log: Log) {
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    let ctrl_c = async {
        // Without a console handler there is still SIGTERM, or the service manager's stop.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
    log.info("stopping");
    let _sent = stop.send(true);
}
