//! What the binary does with its command line (design/phase4-helper.md P4.3.2): `--version`,
//! `scan [FILE]` (the standalone collector: it reads no home, writes only FILE and dials nothing
//! but the model endpoint it is given, for its runtimes), `setup`, and `run` (the default) with
//! every check in front of it, in this order, so that nothing is written or dialled before it is
//! allowed:
//! 1. the development guard (`dev-guard` builds, and every build with a test feature): an
//!    explicit `--home` outside the per-user base, and a loopback server, both refused before
//!    anything else happens;
//! 2. the settings ([`config::resolve`], with the first-run questions at a terminal);
//! 3. the machine key, refused when others can read it, and a warning for a `helper.json` they
//!    can;
//! 4. the session, until it is stopped (Ctrl-C, SIGTERM) or permanently refused.
//!
//! Exit codes (P4.3.9): 0 stopped, 78 a permanent refusal from a console, 2 a command line or
//! settings it cannot take (or a key it refuses), 1 anything else that stops it from starting.
//! A double-clicked binary that cannot start says why and waits for Enter before its window
//! closes, as protocol 1's frozen helper did.

use std::path::Path;
use std::sync::Arc;

use crate::cli::{Cli, Command};
use crate::clock::Clock;
use crate::config::{self, Config, Machine, Sources};
use crate::home::{self, Home};
use crate::key;
use crate::llm::HttpEndpoint;
use crate::log::Log;
use crate::net::{self, TcpDialer};
use crate::power::SystemPower;
use crate::protocol::Provider;
use crate::scan::{self, Probes, Runtime, SystemScanner};
use crate::session::{Facts, Helper, Sensors};
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
    if let Some(Command::Scan { file }) = &cli.command {
        return scan(cli, file.as_deref());
    }
    if DEV_GUARD {
        home::guard(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
    }
    let (home, _source) = home::resolve(opts.home.as_deref(), &env).map_err(|e| Stop(2, e))?;
    let log = Log::stderr(opts.verbose);
    match cli.command.clone().unwrap_or(Command::Run) {
        Command::Setup => setup(cli, &home),
        Command::Run | Command::Scan { .. } => serve(cli, &home, log),
    }
}

/// How long `scan` waits for its model endpoint's list.
const SCAN_LIST: std::time::Duration = std::time::Duration::from_secs(5);

/// The runtime `scan` reports: the endpoint `--base-url` and `--provider` name (LM Studio on this
/// machine by default), when it is LM Studio or Ollama and answers within [`SCAN_LIST`].
fn scan_runtimes(cli: &Cli) -> Vec<Runtime> {
    let provider = match cli.opts.provider.as_deref().map(config::provider) {
        None => Provider::LmStudio,
        Some(Ok(p)) => p,
        Some(Err(_)) => return Vec::new(),
    };
    let name = match provider {
        Provider::LmStudio => "LM Studio",
        Provider::Ollama => "Ollama",
        _ => return Vec::new(),
    };
    let base = cli.opts.base_url.clone().unwrap_or_else(|| config::DEFAULT_BASE_URL.to_owned());
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Vec::new();
    };
    let listed = rt.block_on(async {
        let tls = net::verified_tls(None).ok()?;
        let ep = HttpEndpoint::new(provider, &base, None, tls).ok()?;
        tokio::time::timeout(SCAN_LIST, crate::llm::Endpoint::list(&ep)).await.ok()?.ok()
    });
    let Some(listed) = listed else { return Vec::new() };
    let mut models: Vec<(String, Option<u64>)> =
        listed.into_iter().map(|m| (m.key, m.context)).collect();
    models.sort();
    vec![Runtime { name: name.to_owned(), endpoint: base, models }]
}

/// `citar-helper scan [FILE]`: this machine's hardware report, printed or written to `FILE`, as
/// `python collect_hardware.py` gives it, to import on the Servers page. With `--no-llm` it lists
/// no runtime.
fn scan(cli: &Cli, file: Option<&Path>) -> Result<u8, Stop> {
    let probes = Probes::from_env(&env, DEV_GUARD);
    let runtimes = if cli.opts.no_llm { Vec::new() } else { scan_runtimes(cli) };
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
        let api_key = api_key(&cfg);
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
        };
        helper.run().await.code()
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    log.info(if code == 0 { "stopped" } else { "stopped: permanently refused" });
    Ok(code)
}

/// The API key the endpoint is given: the variable `api_key_env` names, else for `openai` the
/// standard `OPENAI_API_KEY`; no other endpoint is sent a key (protocol 1 sent `OPENAI_API_KEY` to
/// any endpoint).
fn api_key(cfg: &Config) -> Option<String> {
    let var = if !cfg.api_key_env.is_empty() {
        cfg.api_key_env.as_str()
    } else if cfg.provider == Provider::OpenAi {
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
