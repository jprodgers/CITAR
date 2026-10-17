//! Linux: the helper as a systemd unit (design/phase4-helper.md P4.3.9). `--user` (the default)
//! writes `~/.config/systemd/user/citar-helper.service` and enables and starts it with `systemctl
//! --user`; `--system` writes `/etc/systemd/system/citar-helper.service` with a `User=`, as root.
//!
//! **The restart rules carry the exit codes' contract:** `Restart=on-failure` starts the helper
//! again 15 s after any failure (75, an update's hand-off; 1 or 2, a new binary failing its start,
//! whose fourth start rolls the update back, 4-13), and `RestartPreventExitStatus=78` keeps a
//! permanently refused helper down (a service run never exits 78: it idles and tries again hourly,
//! so this is the second net). systemd's own start limit (5 in 10 s) is never reached at one start
//! every 15 s. The user unit runs only while its user is logged in unless lingering is on, which
//! the plan says and never turns on (`loginctl enable-linger`).

use std::path::{Path, PathBuf};

use super::NAME;
use super::plan::{Plan, Quoting, Step};

/// The unit's file name.
#[must_use]
pub fn unit_name() -> String {
    format!("{NAME}.service")
}

/// `arg` quoted for an `ExecStart=` line: double quotes, `\` and `"` escaped, and systemd's
/// specifier `%` and variable `$` doubled, so a path is taken as written.
#[must_use]
pub fn exec_quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The unit file: the helper at `binary` in service mode with the home `home`; with `user` (a
/// system unit) running as that account.
#[must_use]
pub fn unit(binary: &Path, home: &Path, user: Option<&str>) -> String {
    let exec = format!(
        "{} run --service --home {}",
        exec_quote(&binary.display().to_string()),
        exec_quote(&home.display().to_string())
    );
    let mut s = String::new();
    s.push_str("# The CITAR helper as a systemd service, written by `citar-helper service install");
    s.push_str(if user.is_some() { " --system`" } else { " --user`" });
    s.push_str(" (design/phase4-helper.md P4.3.9).\n");
    s.push_str("# `citar-helper service uninstall` stops and removes it. Its log: helper/logs/helper.log in its home.\n");
    s.push_str("[Unit]\n");
    s.push_str("Description=CITAR helper (lends this machine to a CITAR server)\n");
    s.push_str(
        "Documentation=https://github.com/jprodgers/CITAR/blob/main/docs/server/WORKERS.md\n",
    );
    if user.is_some() {
        s.push_str("Wants=network-online.target\n");
        s.push_str("After=network-online.target\n");
    }
    s.push_str("\n[Service]\n");
    s.push_str("Type=simple\n");
    if let Some(u) = user {
        s.push_str(&format!("User={u}\n"));
    }
    s.push_str(&format!("ExecStart={exec}\n"));
    s.push_str("# 75: restart for an update; 1 and 2: a start that failed (an update's new binary is rolled\n");
    s.push_str("# back at its fourth); 78: refused for good, which a service run never exits with (it idles).\n");
    s.push_str("Restart=on-failure\n");
    s.push_str("RestartSec=15\n");
    s.push_str("RestartPreventExitStatus=78\n");
    s.push_str("NoNewPrivileges=yes\n");
    s.push_str("\n[Install]\n");
    s.push_str(if user.is_some() {
        "WantedBy=multi-user.target\n"
    } else {
        "WantedBy=default.target\n"
    });
    s
}

/// `systemctl`'s arguments for this scope, then `rest`.
fn systemctl(system: bool, rest: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if !system {
        a.push("--user".to_owned());
    }
    a.extend(rest.iter().map(|s| (*s).to_owned()));
    a
}

/// Where the unit goes: `unit_dir` (`~/.config/systemd/user`, or `/etc/systemd/system`).
#[must_use]
pub fn unit_path(unit_dir: &Path) -> PathBuf {
    Quoting::Posix.join(unit_dir, &[&unit_name()])
}

/// `install`: the unit written, systemd told, the unit enabled and (re)started; `user` the account
/// the linger line names (`--user`) or the unit runs as (`--system`).
#[must_use]
pub fn install_plan(binary: &Path, home: &Path, unit_dir: &Path, user: &str, system: bool) -> Plan {
    let path = unit_path(unit_dir);
    let name = unit_name();
    let mut steps = vec![
        Step::Folder(unit_dir.to_path_buf()),
        Step::Write { path, text: unit(binary, home, system.then_some(user)), mode: 0o644 },
        Step::Run {
            program: "systemctl".to_owned(),
            args: systemctl(system, &["daemon-reload"]),
            may_fail: false,
        },
        Step::Run {
            program: "systemctl".to_owned(),
            args: systemctl(system, &["enable", &name]),
            may_fail: false,
        },
        Step::Run {
            program: "systemctl".to_owned(),
            args: systemctl(system, &["restart", &name]),
            may_fail: false,
        },
    ];
    if !system {
        steps.push(Step::Note(format!(
            "a user service runs while {user} is logged in; to keep it running after logging out, run \
             once (not run here): loginctl enable-linger {user}"
        )));
    }
    Plan {
        title: format!("citar-helper service install --{}", if system { "system" } else { "user" }),
        quoting: Quoting::Posix,
        steps,
    }
}

/// `uninstall`: stopped and disabled, the unit removed, systemd told.
#[must_use]
pub fn uninstall_plan(unit_dir: &Path, system: bool) -> Plan {
    let name = unit_name();
    Plan {
        title: format!(
            "citar-helper service uninstall --{}",
            if system { "system" } else { "user" }
        ),
        quoting: Quoting::Posix,
        steps: vec![
            Step::Run {
                program: "systemctl".to_owned(),
                args: systemctl(system, &["disable", "--now", &name]),
                may_fail: true,
            },
            Step::Remove(unit_path(unit_dir)),
            Step::Run {
                program: "systemctl".to_owned(),
                args: systemctl(system, &["daemon-reload"]),
                may_fail: false,
            },
            Step::Run {
                program: "systemctl".to_owned(),
                args: systemctl(system, &["reset-failed", &name]),
                may_fail: true,
            },
        ],
    }
}

/// `systemctl show`'s properties that `status` reads.
pub const SHOWN: &str =
    "LoadState,ActiveState,SubState,MainPID,NRestarts,UnitFileState,FragmentPath";

/// `status`'s arguments to `systemctl`.
#[must_use]
pub fn status_args(system: bool) -> Vec<String> {
    systemctl(system, &["show", &unit_name(), &format!("--property={SHOWN}")])
}

/// What `systemctl show` says of the unit, read: whether it is installed and running, and a line.
#[must_use]
pub fn read_status(shown: &str, system: bool) -> super::Status {
    let get = |k: &str| {
        shown
            .lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let scope = if system { "systemd system unit" } else { "systemd user unit" };
    let load = get("LoadState");
    if load.is_empty() || load == "not-found" {
        return super::Status {
            installed: false,
            running: false,
            line: format!("{} ({scope}): not installed", unit_name()),
        };
    }
    let (active, sub, pid) = (get("ActiveState"), get("SubState"), get("MainPID"));
    let running = active == "active" && sub == "running";
    let mut line = format!("{} ({scope}): {active} ({sub})", unit_name());
    if running && pid != "0" && !pid.is_empty() {
        line.push_str(&format!(", pid {pid}"));
    }
    let enabled = get("UnitFileState");
    if !enabled.is_empty() {
        line.push_str(&format!(", {enabled}"));
    }
    line.push_str(&format!(
        ", restarted {} time(s) by systemd",
        if get("NRestarts").is_empty() { "0".to_owned() } else { get("NRestarts") }
    ));
    let path = get("FragmentPath");
    if !path.is_empty() {
        line.push_str(&format!("; its unit file {path}"));
    }
    super::Status { installed: true, running, line }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting_takes_a_path_as_written() {
        assert_eq!(exec_quote("/opt/citar/citar-helper"), "\"/opt/citar/citar-helper\"");
        assert_eq!(exec_quote(r#"/a b/50%$"q"\x"#), r#""/a b/50%%$$\"q\"\\x""#);
    }

    #[test]
    fn status_is_read_from_systemctl_show() {
        let s = read_status(
            "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=4242\nNRestarts=0\nUnitFileState=enabled\nFragmentPath=/home/alice/.config/systemd/user/citar-helper.service\n",
            false,
        );
        assert!(s.installed && s.running, "{s:?}");
        assert_eq!(
            s.line,
            "citar-helper.service (systemd user unit): active (running), pid 4242, enabled, restarted 0 \
             time(s) by systemd; its unit file /home/alice/.config/systemd/user/citar-helper.service"
        );
        let gone = read_status(
            "LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\n",
            false,
        );
        assert!(!gone.installed && !gone.running, "{gone:?}");
        let failed = read_status(
            "LoadState=loaded\nActiveState=failed\nSubState=failed\nMainPID=0\nNRestarts=3\n",
            true,
        );
        assert!(failed.installed && !failed.running, "{failed:?}");
        assert!(failed.line.contains("restarted 3 time(s)"), "{}", failed.line);
    }
}
