//! `citar-fake-probe`: a stand-in for the hardware scan's probe programs (design/phase4-helper.md
//! P4.3.5; package 4-06, gate 3). A development or test build of the helper runs it in place of
//! `nvidia-smi`, PowerShell or `system_profiler` when `CITAR_HELPER_NVIDIA_SMI`,
//! `CITAR_HELPER_POWERSHELL` or `CITAR_HELPER_SYSTEM_PROFILER` names it, so a test knows every
//! answer the helper read and every time it asked, and the machine's real programs (on Windows the
//! NVIDIA driver's `nvidia-smi.exe` in System32, which `PATH` cannot shadow) are never run.
//!
//! - Every call is logged: one line, `<Unix time> <arguments>` (line breaks in them written as
//!   `\n`), appended to the file `CITAR_FAKE_PROBE_LOG` names, else to `<its own path>.log` (so a
//!   test that copies it into a directory of its own, as `nvidia-smi.exe` say, finds its calls
//!   beside it).
//! - It answers what `CITAR_FAKE_PROBE_REPLY` names (a file, printed as it is); otherwise as
//!   `nvidia-smi` would: `--query-gpu=power.draw` prints `CITAR_FAKE_PROBE_WATTS` (default
//!   `123.45`), the scan's query one fixed GPU, `CITAR Fake GPU` with 24 GiB, a 300 W limit and
//!   that draw; anything else prints nothing.
//! - It exits 0, or with `CITAR_FAKE_PROBE_EXIT`'s code.
//!
//! Only builds with the development guard make it (`required-features = ["dev-guard"]`).

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::process::ExitCode;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = var("CITAR_FAKE_PROBE_LOG").map(std::path::PathBuf::from).or_else(|| {
        std::env::current_exe().ok().map(|p| {
            let mut name = p.into_os_string();
            name.push(".log");
            std::path::PathBuf::from(name)
        })
    });
    if let Some(log) = log {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        // Every call, whatever happens to its answer: the test counts them. One line a call: a
        // PowerShell script's own line breaks are written as `\n`.
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let line = args.join(" ").replace('\r', "\\r").replace('\n', "\\n");
            let _logged = writeln!(f, "{now:.3} {line}");
        }
    }
    let watts = var("CITAR_FAKE_PROBE_WATTS").unwrap_or_else(|| "123.45".to_owned());
    let out = if let Some(reply) = var("CITAR_FAKE_PROBE_REPLY") {
        std::fs::read_to_string(reply).unwrap_or_default()
    } else if args.iter().any(|a| a == "--query-gpu=power.draw") {
        format!("{watts}\n")
    } else if args.iter().any(|a| a.starts_with("--query-gpu=name,")) {
        format!("CITAR Fake GPU, 24576, 999.99, 300.00, {watts}, 00000000:01:00.0\n")
    } else {
        String::new()
    };
    let mut stdout = std::io::stdout();
    let _written = stdout.write_all(out.as_bytes()).and_then(|()| stdout.flush());
    let code = var("CITAR_FAKE_PROBE_EXIT").and_then(|c| c.parse::<u8>().ok()).unwrap_or(0);
    ExitCode::from(code)
}
