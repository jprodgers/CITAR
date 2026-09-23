//! `cargo refcheck`: the reference-check command line (DESIGN.md 9.2).
//!
//! Its commands arrive with package 1a-04. Exit code 2 is a usage error, as it will stay.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "refcheck: no commands yet (package 1a-04 adds run, explain, suggest, ratchet, changelog and list)"
    );
    ExitCode::from(2)
}
