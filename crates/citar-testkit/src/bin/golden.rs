//! `cargo golden`: checks and blesses the golden digest sets (DESIGN.md 9.6).
//!
//! No golden set exists yet; package 1a-02 adds rng.json, libm.json and pyfmt.json.

#![forbid(unsafe_code)]

use std::process::ExitCode;

#[allow(clippy::disallowed_macros, reason = "a command-line tool reports on stderr")]
fn main() -> ExitCode {
    eprintln!("golden: no golden sets yet (package 1a-02 adds the first)");
    ExitCode::from(2)
}
