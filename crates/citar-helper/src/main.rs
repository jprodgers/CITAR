//! `citar-helper`, the CITAR helper's binary (design/phase4-helper.md P4.3).
//!
//! Package 4-01 builds it to print `--version` only; the commands (`run`, `setup`, `scan`,
//! `service`, `update`, `job-host`, `--self-test`) arrive with packages 4-03 to 4-14. Exit codes
//! (P4.3.9): 0 when stopped, 75 to restart for an update, 78 for a permanent refusal from a
//! console, 2 for a command line it does not take.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--version" | "-V"] => {
            println!("{}", citar_helper::version::build_info().line());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "citar-helper {}: this build only prints --version; the helper's commands come \
                 with the Rust helper's later packages (design/phase4-helper.md, 4-03)",
                citar_helper::version::VERSION
            );
            ExitCode::from(2)
        }
    }
}
