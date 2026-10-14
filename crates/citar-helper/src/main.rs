//! `citar-helper`, the CITAR helper's binary (design/phase4-helper.md P4.3): the command line
//! (`citar_helper::cli`) handed to `citar_helper::app`. Exit codes (P4.3.9): 0 when stopped, 78
//! for a permanent refusal from a console, 2 for a command line or settings it cannot take, 75
//! (4-13) to restart for an update.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let bare = args.len() <= 1;
    let cli = match citar_helper::cli::Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            // --help prints to standard output and succeeds; a bad command line is argparse's 2.
            let _printed = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    ExitCode::from(citar_helper::app::main(&cli, bare))
}
