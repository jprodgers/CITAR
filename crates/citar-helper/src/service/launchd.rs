//! macOS: the helper as a LaunchAgent (design/phase4-helper.md P4.3.9), `--user` only:
//! `~/Library/LaunchAgents/org.citar.helper.plist`, loaded into the user's GUI domain with
//! `launchctl bootstrap gui/<uid>`. `RunAtLoad` starts it at once and at each login; `KeepAlive`
//! with `SuccessfulExit` false starts it again after any exit but 0 (75, an update's hand-off; a
//! failed start, which an update's rollback counts), at most once every `ThrottleInterval` (15 s).
//! launchd cannot leave out an exit code, which is why a service run never exits on a permanent
//! refusal: it idles and tries again hourly. Its standard output and error go to
//! `helper/logs/launchd.log` in the home, beside the helper's own log.

use std::path::{Path, PathBuf};

use super::LABEL;
use super::plan::{Plan, Quoting, Step};

/// The plist's file name.
#[must_use]
pub fn plist_name() -> String {
    format!("{LABEL}.plist")
}

/// Where the plist goes: `~/Library/LaunchAgents`.
#[must_use]
pub fn plist_path(agents: &Path) -> PathBuf {
    Quoting::Posix.join(agents, &[&plist_name()])
}

/// `s` escaped for a plist's `<string>`.
#[must_use]
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The LaunchAgent's plist: the helper at `binary` in service mode with the home `home`.
#[must_use]
pub fn plist(binary: &Path, home: &Path) -> String {
    let log = Quoting::Posix.join(home, &["helper", "logs", "launchd.log"]);
    let args = [
        binary.display().to_string(),
        "run".to_owned(),
        "--service".to_owned(),
        "--home".to_owned(),
        home.display().to_string(),
    ];
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    s.push_str("<!-- The CITAR helper as a LaunchAgent, written by `citar-helper service install --user`\n");
    s.push_str("     (design/phase4-helper.md P4.3.9); `citar-helper service uninstall` unloads and removes it. -->\n");
    s.push_str("<plist version=\"1.0\">\n<dict>\n");
    s.push_str(&format!("    <key>Label</key>\n    <string>{LABEL}</string>\n"));
    s.push_str("    <key>ProgramArguments</key>\n    <array>\n");
    for a in &args {
        s.push_str(&format!("        <string>{}</string>\n", xml_escape(a)));
    }
    s.push_str("    </array>\n");
    s.push_str("    <key>RunAtLoad</key>\n    <true/>\n");
    s.push_str("    <!-- started again after any exit but 0: 75 (an update), a failed start (an update rolled back) -->\n");
    s.push_str("    <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n");
    s.push_str("    <key>ThrottleInterval</key>\n    <integer>15</integer>\n");
    s.push_str("    <key>ProcessType</key>\n    <string>Background</string>\n");
    s.push_str(&format!(
        "    <key>StandardOutPath</key>\n    <string>{}</string>\n",
        xml_escape(&log.display().to_string())
    ));
    s.push_str(&format!(
        "    <key>StandardErrorPath</key>\n    <string>{}</string>\n",
        xml_escape(&log.display().to_string())
    ));
    s.push_str("</dict>\n</plist>\n");
    s
}

/// The GUI domain of user `uid`, and the service in it.
fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// `install`: the plist written (its log folder made first), any earlier one unloaded, the new one
/// loaded.
#[must_use]
pub fn install_plan(binary: &Path, home: &Path, agents: &Path, uid: u32) -> Plan {
    let path = plist_path(agents);
    Plan {
        title: "citar-helper service install --user".to_owned(),
        quoting: Quoting::Posix,
        steps: vec![
            Step::Folder(agents.to_path_buf()),
            Step::Folder(Quoting::Posix.join(home, &["helper", "logs"])),
            Step::Write { path: path.clone(), text: plist(binary, home), mode: 0o644 },
            Step::Run {
                program: "launchctl".to_owned(),
                args: vec!["bootout".to_owned(), format!("{}/{LABEL}", domain(uid))],
                may_fail: true,
            },
            Step::Run {
                program: "launchctl".to_owned(),
                args: vec!["bootstrap".to_owned(), domain(uid), path.display().to_string()],
                may_fail: false,
            },
        ],
    }
}

/// `uninstall`: unloaded (which stops it), the plist removed.
#[must_use]
pub fn uninstall_plan(agents: &Path, uid: u32) -> Plan {
    Plan {
        title: "citar-helper service uninstall --user".to_owned(),
        quoting: Quoting::Posix,
        steps: vec![
            Step::Run {
                program: "launchctl".to_owned(),
                args: vec!["bootout".to_owned(), format!("{}/{LABEL}", domain(uid))],
                may_fail: true,
            },
            Step::Remove(plist_path(agents)),
        ],
    }
}

/// `status`'s arguments to `launchctl`.
#[must_use]
pub fn status_args(uid: u32) -> Vec<String> {
    vec!["print".to_owned(), format!("{}/{LABEL}", domain(uid))]
}

/// What `launchctl print` says of the agent (`None` when it is not loaded), with whether its plist
/// is there.
#[must_use]
pub fn read_status(printed: Option<&str>, plist_there: bool) -> super::Status {
    let Some(text) = printed else {
        return super::Status {
            installed: plist_there,
            running: false,
            line: format!(
                "{LABEL} (LaunchAgent): {}",
                if plist_there { "installed, not loaded" } else { "not installed" }
            ),
        };
    };
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(k).and_then(|r| r.trim_start().strip_prefix('=')))
            .map(|v| v.trim().to_owned())
    };
    let state = get("state").unwrap_or_else(|| "unknown".to_owned());
    let running = state == "running";
    let mut line = format!("{LABEL} (LaunchAgent): {state}");
    if let Some(pid) = get("pid") {
        line.push_str(&format!(", pid {pid}"));
    }
    if let Some(code) = get("last exit code") {
        line.push_str(&format!(", last exit {code}"));
    }
    super::Status { installed: true, running, line }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_read_from_launchctl_print() {
        let printed = "gui/501/org.citar.helper = {\n\tactive count = 1\n\tpath = /Users/a/Library/LaunchAgents/org.citar.helper.plist\n\tstate = running\n\n\tprogram = /usr/local/bin/citar-helper\n\tpid = 812\n\tlast exit code = (never exited)\n}\n";
        let s = read_status(Some(printed), true);
        assert!(s.installed && s.running, "{s:?}");
        assert_eq!(
            s.line,
            "org.citar.helper (LaunchAgent): running, pid 812, last exit (never exited)"
        );
        let gone = read_status(None, false);
        assert!(!gone.installed && !gone.running);
        assert_eq!(
            read_status(None, true).line,
            "org.citar.helper (LaunchAgent): installed, not loaded"
        );
    }

    #[test]
    fn plist_strings_are_escaped() {
        assert_eq!(xml_escape("a&b<c>"), "a&amp;b&lt;c&gt;");
    }
}
