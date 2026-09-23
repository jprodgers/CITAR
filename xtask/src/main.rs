//! `cargo xtask`: the workspace's own checks (DESIGN.md 2.1).
//!
//! `check` enforces the rules of DESIGN.md that clippy cannot see. Later packages add
//! `gen-uniques` (1a-05), `perf` (1e-03) and `ci-local`.

#![forbid(unsafe_code)]

mod check;
mod lexer;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check    dependency allow-list, version, layering, mutation access, generated files,
           NotPorted and Pending stages (exit 0 clean, 1 problems, 2 could not run)";

fn workspace_root() -> PathBuf {
    // xtask/ sits at the root; this is fixed at build time, so each worktree checks itself.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["check"] => check::run(&workspace_root()),
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
