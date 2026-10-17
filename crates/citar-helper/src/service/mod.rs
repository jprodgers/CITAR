//! Service installation (design/phase4-helper.md P4.3.9): `citar-helper service
//! install|uninstall|status|run [--user|--system] [--dry-run]`.
//!
//! Every definition runs the helper with `--service`, an explicit `--home` and the absolute path of
//! the binary; `--dry-run` prints what would be written and run and changes nothing ([`plan`]).
//!
//! | OS | `--user` (the default) | `--system` |
//! |---|---|---|
//! | Linux | a systemd user unit ([`systemd`]) | a system unit with `User=`, as root |
//! | macOS | a LaunchAgent ([`launchd`]) | not offered |
//! | Windows | a Run value and the Run supervisor ([`windows`], [`supervisor`]) | a Windows service as `NT SERVICE\citar-helper`, from an elevated prompt |
//!
//! **Exit codes are part of the contract:** 0, stopped by the user or the service manager; 75,
//! restart for an update; 78, a permanent refusal, from a console run only; anything else, a
//! crash. In service mode a permanent refusal does not exit: the helper idles, logs once and tries
//! again hourly (the session, and `app` for settings this binary cannot take), so no service
//! manager restarts it into a loop.
//!
//! **What the commands refuse:** a system service unless elevated (root on Linux, an elevated
//! prompt on Windows: they print the command to run and exit 2, and never elevate themselves);
//! `--system` on macOS; settings flags (`--server`, `--token` and the rest), since a service reads
//! its settings from its home's `helper.json` alone (`citar-helper setup --home DIR` writes it); an
//! installation whose settings would not start (no `helper.json` with a server and a token). A
//! development or test build (`dev-guard`) installs nothing on Windows at all, only `--dry-run`
//! (P4.7.1: no service, Run entry or registry value is ever written on the laptop the packages are
//! built on); on Linux and macOS it installs, for the tests in WSL and CI, with its loopback guard.

pub mod launchd;
pub mod plan;
pub mod supervisor;
pub mod systemd;
pub mod windows;

use std::path::{Path, PathBuf};

use crate::cli::{Cli, Options, ServiceAction, ServiceScope};
use crate::home::{self, Home};
use crate::log::Log;

pub use plan::{Plan, Quoting, Step};

/// The systemd unit's stem, the Windows service's name.
pub const NAME: &str = "citar-helper";
/// The LaunchAgent's label.
pub const LABEL: &str = "org.citar.helper";
/// The Run value's name, and the Windows service's display name.
pub const RUN_VALUE: &str = "CITAR helper";
/// The Windows service's display name.
pub const DISPLAY: &str = "CITAR helper";

/// The operating systems a plan is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// This one.
    #[must_use]
    pub fn this() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// Per user, or for the whole machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

/// What a service command needs to know of the machine, so that a plan is a function of it alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inputs {
    pub os: Os,
    pub scope: Scope,
    /// The binary the definition runs (on Windows `--system`, the copy in the home's `bin`).
    pub binary: PathBuf,
    /// The binary running now (what `--system` on Windows copies).
    pub source: PathBuf,
    /// The helper's home.
    pub home: PathBuf,
    /// The account: the linger line's, a system unit's `User=`.
    pub user: String,
    /// The user's id (launchd's `gui/<uid>`).
    pub uid: u32,
    /// Where the definition goes: the unit's folder, `~/Library/LaunchAgents`.
    pub dir: PathBuf,
}

/// The installation plan for `inputs`.
///
/// # Errors
/// What this OS does not offer.
pub fn install_plan(i: &Inputs) -> Result<Plan, String> {
    match (i.os, i.scope) {
        (Os::Linux, s) => {
            Ok(systemd::install_plan(&i.binary, &i.home, &i.dir, &i.user, s == Scope::System))
        }
        (Os::Macos, Scope::User) => Ok(launchd::install_plan(&i.binary, &i.home, &i.dir, i.uid)),
        (Os::Macos, Scope::System) => Err(MACOS_SYSTEM.to_owned()),
        (Os::Windows, Scope::User) => Ok(windows::user_install_plan(&i.binary, &i.home)),
        (Os::Windows, Scope::System) => Ok(windows::system_install_plan(&i.source, &i.home)),
    }
}

/// The removal plan for `inputs`.
///
/// # Errors
/// What this OS does not offer.
pub fn uninstall_plan(i: &Inputs) -> Result<Plan, String> {
    match (i.os, i.scope) {
        (Os::Linux, s) => Ok(systemd::uninstall_plan(&i.dir, s == Scope::System)),
        (Os::Macos, Scope::User) => Ok(launchd::uninstall_plan(&i.dir, i.uid)),
        (Os::Macos, Scope::System) => Err(MACOS_SYSTEM.to_owned()),
        (Os::Windows, Scope::User) => Ok(windows::user_uninstall_plan(&i.home)),
        (Os::Windows, Scope::System) => Ok(windows::system_uninstall_plan(&i.home)),
    }
}

const MACOS_SYSTEM: &str = "a system service is not offered on macOS: install the helper for your user \
     (citar-helper service install --user, a LaunchAgent)";

/// What `status` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub installed: bool,
    pub running: bool,
    pub line: String,
}

/// Whether this process is elevated (an administrator's elevated prompt on Windows, root on Unix),
/// as a system service needs. An OS that cannot say counts as elevated, so that a test refusing to
/// run elevated (P4.7.1) refuses then too.
#[must_use]
pub fn elevated() -> bool {
    citar_jobs::sys::is_elevated().unwrap_or(true)
}

/// The settings flags a service never sees (it reads its home's `helper.json`): their names, when
/// any is given.
fn settings_flags(o: &Options) -> Vec<&'static str> {
    let mut given = Vec::new();
    let mut flag = |on: bool, name: &'static str| {
        if on {
            given.push(name);
        }
    };
    flag(o.server_url.is_some(), "--server");
    flag(o.token.is_some(), "--token");
    flag(o.config.is_some(), "--config");
    flag(o.base_url.is_some(), "--base-url");
    flag(o.provider.is_some(), "--provider");
    flag(o.api_key_env.is_some(), "--api-key-env");
    flag(o.max_concurrent.is_some(), "--max-concurrent");
    flag(o.name.is_some(), "--name");
    flag(!o.models.is_empty(), "--model");
    flag(!o.quiet.is_empty(), "--quiet");
    flag(o.insecure, "--insecure");
    flag(o.ca_file.is_some(), "--ca-file");
    flag(o.no_hardware, "--no-hardware");
    flag(o.hardware_file.is_some(), "--hardware");
    flag(o.no_llm, "--no-llm");
    flag(o.cpu, "--cpu");
    flag(o.no_power, "--no-power");
    flag(o.updates.is_some(), "--updates");
    given
}

/// A refusal: its exit code and why.
pub type Refusal = (u8, String);

fn env(name: &str) -> Option<std::ffi::OsString> {
    home::process_env(name)
}

/// A path made absolute (never canonicalised: Windows would give it the `\\?\` form).
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The user's name: `SUDO_USER` under sudo, else `USER`, `LOGNAME` or `USERNAME`.
fn user_name(under_sudo: bool) -> String {
    let get = |k: &str| env(k).map(|v| v.to_string_lossy().into_owned()).filter(|v| !v.is_empty());
    let sudo = if under_sudo { get("SUDO_USER") } else { None };
    sudo.or_else(|| get("USER"))
        .or_else(|| get("LOGNAME"))
        .or_else(|| get("USERNAME"))
        .unwrap_or_default()
}

/// The user's id on Unix (0 elsewhere).
fn uid() -> u32 {
    #[cfg(unix)]
    {
        citar_jobs::sys::user_id()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The scope the flags ask for (`--user` the default).
fn scope_of(s: &ServiceScope) -> Scope {
    if s.system { Scope::System } else { Scope::User }
}

/// This machine's inputs for `scope`, the home from `--home` (or, on Windows `--system`,
/// `%ProgramData%\CITAR\helper`).
///
/// # Errors
/// No home, or no folder for the definition.
pub fn inputs(opts: &Options, scope: Scope) -> Result<Inputs, Refusal> {
    let os = Os::this();
    let source = std::env::current_exe().map_err(|e| (1, format!("this binary's path: {e}")))?;
    let source = absolute(&source);
    let home = if os == Os::Windows && scope == Scope::System {
        match &opts.home {
            Some(h) => absolute(h),
            None => {
                let data = env("ProgramData")
                    .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
                Quoting::Windows.join(&data, &["CITAR", "helper"])
            }
        }
    } else {
        let (h, _) = home::resolve(opts.home.as_deref(), &env).map_err(|e| (2, e))?;
        absolute(h.root())
    };
    let user_home = || {
        env("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| (2, "HOME is not set: no folder for the definition".to_owned()))
    };
    let dir = match (os, scope) {
        (Os::Linux, Scope::User) => match env("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(x) => Quoting::Posix.join(Path::new(&x), &["systemd", "user"]),
            None => Quoting::Posix.join(&user_home()?, &[".config", "systemd", "user"]),
        },
        (Os::Linux, Scope::System) => PathBuf::from("/etc/systemd/system"),
        (Os::Macos, _) => Quoting::Posix.join(&user_home()?, &["Library", "LaunchAgents"]),
        (Os::Windows, _) => PathBuf::new(),
    };
    let binary = if os == Os::Windows && scope == Scope::System {
        windows::service_binary(&home)
    } else {
        source.clone()
    };
    Ok(Inputs {
        os,
        scope,
        binary,
        source,
        home,
        user: user_name(scope == Scope::System),
        uid: uid(),
        dir,
    })
}

/// Prepares a program a plan runs: `systemctl --user` reaches the user's manager through
/// `XDG_RUNTIME_DIR`, which a session without a login shell (`wsl -e`, `su`) may not set.
pub(crate) fn prepare(cmd: &mut std::process::Command, program: &str) {
    #[cfg(unix)]
    if program == "systemctl" && env("XDG_RUNTIME_DIR").is_none_or(|v| v.is_empty()) {
        let run = PathBuf::from(format!("/run/user/{}", uid()));
        if run.is_dir() {
            cmd.env("XDG_RUNTIME_DIR", run);
        }
    }
    #[cfg(not(unix))]
    let _ = (cmd, program);
}

/// Whether the settings in `home` would start a service: `helper.json` with a server and a token
/// (no flags, no variables: a service has neither), and in a development build a loopback server.
///
/// # Errors
/// Why not, with what to do.
pub fn check_settings(home: &Path) -> Result<(), String> {
    let h = Home::at(home);
    let opts = Options { home: Some(home.to_path_buf()), ..Options::default() };
    let none = |_: &str| None;
    let fix = format!(
        "a service reads its settings from {} alone: run `citar-helper setup --home {}` first",
        h.config_file().display(),
        home.display()
    );
    let cfg =
        crate::config::resolve(&crate::config::Sources { opts: &opts, env: &none, home: &h }, None)
            .map_err(|e| format!("the service would not start ({e}); {fix}"))?;
    let target = crate::net::target(&cfg.server_url)
        .map_err(|e| format!("the service would not start ({e}); {fix}"))?;
    if crate::app::DEV_GUARD {
        crate::net::guard_loopback(&target)
            .map_err(|e| format!("the service would not start ({e})"))?;
    }
    Ok(())
}

/// `citar-helper service install|uninstall|status` (`run` is `app`'s: it is the helper itself).
/// Returns the exit code.
///
/// # Errors
/// A refusal, with its exit code and why.
pub fn command(cli: &Cli, action: &ServiceAction, log: &Log) -> Result<u8, Refusal> {
    let (scope_flags, dry_run, installing) = match action {
        ServiceAction::Install(c) => (&c.scope, c.dry_run, Some(true)),
        ServiceAction::Uninstall(c) => (&c.scope, c.dry_run, Some(false)),
        ServiceAction::Status(s) => (s, false, None),
        ServiceAction::Run => {
            return Err((2, "service run is handled by the helper itself".to_owned()));
        }
    };
    let scope = scope_of(scope_flags);
    let given = settings_flags(&cli.opts);
    if !given.is_empty() {
        return Err((
            2,
            format!(
                "a service reads its settings from its home's helper.json alone, so {} would not reach \
                 it: put them there (citar-helper setup --home DIR) and run this again without them",
                given.join(", ")
            ),
        ));
    }
    if scope == Scope::System && Os::this() == Os::Linux && cli.opts.home.is_none() {
        return Err((
            2,
            "a system unit needs its home named: --home DIR, a folder of the account the unit runs as \
             (the one that runs sudo)"
                .to_owned(),
        ));
    }
    let i = inputs(&cli.opts, scope)?;
    let Some(installing) = installing else { return Ok(status(&i)) };
    let plan =
        if installing { install_plan(&i) } else { uninstall_plan(&i) }.map_err(|e| (2, e))?;
    if dry_run {
        print!("{}", plan.render());
        if installing && let Err(e) = check_settings(&i.home) {
            println!("note: {e}");
        }
        if scope == Scope::System && !citar_jobs::sys::is_elevated().unwrap_or(false) {
            println!("note: {}", elevation_needed(&i, installing));
        }
        return Ok(0);
    }
    // An unelevated system command says how to run it elevated, before anything else is decided.
    if scope == Scope::System && !citar_jobs::sys::is_elevated().map_err(|e| (1, e.to_string()))? {
        return Err((2, elevation_needed(&i, installing)));
    }
    if crate::app::DEV_GUARD && i.os == Os::Windows {
        return Err((
            2,
            "this is a development or test build: on Windows it installs and removes nothing (no service, \
             no Run value), only --dry-run; a release build does (design P4.7.1)"
                .to_owned(),
        ));
    }
    if scope == Scope::System && i.os == Os::Linux && (i.user.is_empty() || i.user == "root") {
        return Err((
            2,
            "a system unit runs the helper as the account that ran sudo, never as root: run this through \
             sudo from that account"
                .to_owned(),
        ));
    }
    if installing {
        check_settings(&i.home).map_err(|e| (2, e))?;
        if i.os == Os::Windows && i.scope == Scope::System && windows_service_exists() {
            return Err((
                2,
                format!(
                    "the Windows service {NAME} is already installed: run citar-helper service uninstall --system first"
                ),
            ));
        }
    }
    plan.run(log).map_err(|f| (1, format!("step {} failed: {}", f.step, f.what)))?;
    if installing {
        println!("installed: {}", read_status(&i).line);
    } else {
        println!("removed");
    }
    Ok(0)
}

/// Whether the Windows service exists (`sc.exe query` answers for it).
fn windows_service_exists() -> bool {
    plan::run_program("sc.exe", &["query".to_owned(), NAME.to_owned()]).is_ok()
}

/// What an unelevated system command says: the command to run elevated.
fn elevation_needed(i: &Inputs, installing: bool) -> String {
    let verb = if installing { "install" } else { "uninstall" };
    match i.os {
        Os::Windows => format!(
            "a Windows service is installed from an elevated prompt (Run as administrator), which this is \
             not; in one, run: {} service {verb} --system --home {}",
            plan::windows_quote(&i.source.display().to_string(), false),
            plan::windows_quote(&i.home.display().to_string(), true)
        ),
        _ => format!(
            "a system unit is installed as root, which this is not; run: sudo {} service {verb} --system --home {}",
            plan::posix_quote(&i.source.display().to_string()),
            plan::posix_quote(&i.home.display().to_string())
        ),
    }
}

/// `status`: the service's state, printed; 0 when it runs, 3 when it does not (or is not
/// installed), as `systemctl status` and LSB say.
fn status(i: &Inputs) -> u8 {
    let s = read_status(i);
    println!("{}", s.line);
    if s.running { 0 } else { 3 }
}

/// What the OS says of the service.
#[must_use]
pub fn read_status(i: &Inputs) -> Status {
    match (i.os, i.scope) {
        (Os::Linux, s) => {
            let system = s == Scope::System;
            match plan::run_program("systemctl", &systemd::status_args(system)) {
                Ok(text) => systemd::read_status(&text, system),
                Err((_, said)) => Status {
                    installed: false,
                    running: false,
                    line: format!("systemctl could not say: {said}"),
                },
            }
        }
        (Os::Macos, _) => {
            let printed = plan::run_program("launchctl", &launchd::status_args(i.uid)).ok();
            launchd::read_status(printed.as_deref(), launchd::plist_path(&i.dir).is_file())
        }
        (Os::Windows, Scope::System) => {
            let text = plan::run_program("sc.exe", &["queryex".to_owned(), NAME.to_owned()]).ok();
            windows::read_system_status(text.as_deref())
        }
        (Os::Windows, Scope::User) => {
            let value = plan::run_program(
                "reg.exe",
                &[
                    "query".to_owned(),
                    windows::RUN_KEY.to_owned(),
                    "/v".to_owned(),
                    RUN_VALUE.to_owned(),
                ],
            )
            .ok()
            .and_then(|t| windows::read_run_value(&t));
            let record = supervisor::read_record(&i.home);
            let sup = record.as_ref().filter(|r| supervisor::alive(r.pid));
            let child = sup.and_then(|r| r.child).filter(|c| supervisor::alive(*c));
            let mut line = match &value {
                Some(v) => format!("the Run value \"{RUN_VALUE}\" starts {v} at each sign-in"),
                None => format!("no Run value \"{RUN_VALUE}\""),
            };
            match (sup, child) {
                (Some(r), Some(c)) => {
                    line.push_str(&format!("; running: supervisor pid {}, helper pid {c}", r.pid))
                }
                (Some(r), None) => line.push_str(&format!(
                    "; the supervisor runs (pid {}), its helper is starting again",
                    r.pid
                )),
                (None, _) => line.push_str("; not running"),
            }
            Status { installed: value.is_some(), running: child.is_some(), line }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed plans (gate 1 of 4-14): each OS's install and uninstall for both scopes, from
    /// fixed inputs, on every OS. `CITAR_BLESS_SERVICE=1` writes them after a deliberate change.
    fn fixed(os: Os, scope: Scope) -> Inputs {
        match os {
            Os::Linux => Inputs {
                os,
                scope,
                binary: PathBuf::from("/opt/citar/citar-helper"),
                source: PathBuf::from("/opt/citar/citar-helper"),
                home: PathBuf::from(if scope == Scope::System {
                    "/srv/citar-helper"
                } else {
                    "/home/alice/.local/share/citar"
                }),
                user: "alice".to_owned(),
                uid: 1000,
                dir: PathBuf::from(if scope == Scope::System {
                    "/etc/systemd/system"
                } else {
                    "/home/alice/.config/systemd/user"
                }),
            },
            Os::Macos => Inputs {
                os,
                scope,
                binary: PathBuf::from("/Users/alice/bin/citar-helper-macos-arm64"),
                source: PathBuf::from("/Users/alice/bin/citar-helper-macos-arm64"),
                home: PathBuf::from("/Users/alice/Library/Application Support/CITAR"),
                user: "alice".to_owned(),
                uid: 501,
                dir: PathBuf::from("/Users/alice/Library/LaunchAgents"),
            },
            Os::Windows => {
                let source =
                    PathBuf::from(r"C:\Users\alice\Downloads\citar-helper-windows-x64.exe");
                let home = PathBuf::from(if scope == Scope::System {
                    r"C:\ProgramData\CITAR\helper"
                } else {
                    r"C:\Users\alice\AppData\Local\CITAR"
                });
                let binary = if scope == Scope::System {
                    windows::service_binary(&home)
                } else {
                    source.clone()
                };
                Inputs {
                    os,
                    scope,
                    binary,
                    source,
                    home,
                    user: "alice".to_owned(),
                    uid: 0,
                    dir: PathBuf::new(),
                }
            }
        }
    }

    /// Every OS and scope's committed file: the install plan, then the uninstall plan.
    pub(crate) fn rendered(os: Os, scope: Scope) -> Result<String, String> {
        let i = fixed(os, scope);
        Ok(format!("{}\n{}", install_plan(&i)?.render(), uninstall_plan(&i)?.render()))
    }

    pub(crate) fn expected_file(os: Os, scope: Scope) -> PathBuf {
        let os = match os {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        };
        let scope = if scope == Scope::System { "system" } else { "user" };
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("data")
            .join("service")
            .join(format!("{os}-{scope}.txt"))
    }

    #[test]
    fn every_plan_is_its_committed_file() {
        let bless = std::env::var_os("CITAR_BLESS_SERVICE").is_some();
        let mut checked = 0;
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            for scope in [Scope::User, Scope::System] {
                let Ok(text) = rendered(os, scope) else {
                    assert_eq!(
                        (os, scope),
                        (Os::Macos, Scope::System),
                        "only macOS has no system service"
                    );
                    continue;
                };
                let file = expected_file(os, scope);
                if bless {
                    std::fs::create_dir_all(file.parent().expect("a folder")).expect("made");
                    std::fs::write(&file, &text).expect("blessed");
                }
                let want = std::fs::read_to_string(&file)
                    .expect("the committed plan")
                    .replace("\r\n", "\n");
                assert_eq!(
                    text,
                    want,
                    "{} differs from the plan (CITAR_BLESS_SERVICE=1 rewrites it)",
                    file.display()
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 5);
    }

    #[test]
    fn the_definitions_carry_the_restart_rules_and_the_account() {
        let linux = rendered(Os::Linux, Scope::User).expect("a plan");
        for want in [
            "Restart=on-failure",
            "RestartSec=15",
            "RestartPreventExitStatus=78",
            "NoNewPrivileges=yes",
            "run --service --home",
            "loginctl enable-linger alice",
        ] {
            assert!(linux.contains(want), "{want}");
        }
        assert!(rendered(Os::Linux, Scope::System).expect("a plan").contains("User=alice"));
        let mac = rendered(Os::Macos, Scope::User).expect("a plan");
        for want in [
            "<key>KeepAlive</key>",
            "<key>SuccessfulExit</key>\n            <false/>",
            "<key>RunAtLoad</key>",
            "<integer>15</integer>",
            "launchctl bootstrap gui/501",
        ] {
            assert!(mac.contains(want), "{want}");
        }
        let svc = rendered(Os::Windows, Scope::System).expect("a plan");
        for want in [
            r#"obj= "NT SERVICE\citar-helper""#,
            "failureflag citar-helper 1",
            "actions= restart/15000/restart/15000/restart/15000",
            "sidtype citar-helper unrestricted",
            "O:S-1-5-32-544D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;FA;;;S-1-5-80-",
        ] {
            assert!(svc.contains(want), "{want} in\n{svc}");
        }
        let run = rendered(Os::Windows, Scope::User).expect("a plan");
        assert!(run.contains(r"\CurrentVersion\Run /v "), "{run}");
        assert!(run.contains("run --detach --home"), "{run}");
    }

    #[test]
    fn macos_offers_no_system_service() {
        assert!(install_plan(&fixed(Os::Macos, Scope::System)).is_err());
    }
}
