//! `cargo xtask`: the workspace's own checks (DESIGN.md 2.1).
//!
//! `check` enforces the rules of DESIGN.md that clippy cannot see; `gen-uniques` writes the
//! engine's unique types from their two sources (1a-05); `gen-params` writes the bot's parameter
//! struct from its schema (registered in 2-00a, written in 2-01a); `perf` runs the criterion
//! suites and holds them to their budgets (perfgate, 1e-03; the `games` suite from 2-00a).

#![forbid(unsafe_code)]

mod check;
mod gen_params;
mod gen_uniques;
mod lexer;
mod perf;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check    dependency allow-list, version, layering, mutation access, generated files,
           NotPorted and Pending stages (exit 0 clean, 1 problems, 2 could not run)
  gen-uniques
           write crates/citar-engine/src/unique/gen.rs from unique_types.tsv and
           unique_supported.toml (exit 0 written, 2 could not)
  gen-params
           write crates/citar-bot/src/params/gen.rs from crates/citar-bot/params/basic-1.json
           (exit 0 written, 2 could not; the generator comes with package 2-01a)
  perf [--check] [--suite kernels|turns|io|games] [-- <bench arguments>]
           run the criterion suites of citar-bench (games only when named), then check every
           budget of crates/citar-bench/thresholds.toml and the pass rounds' ratios to Python
           (--check: only check what a run left; exit 0 within, 1 over, 2 could not run)";

fn workspace_root() -> PathBuf {
    // xtask/ sits at the root; this is fixed at build time, so each worktree checks itself.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

fn write_gen_uniques(root: &Path) -> ExitCode {
    let written = gen_uniques::generate(root).and_then(|text| {
        let path = root.join(gen_uniques::OUT);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    });
    match written {
        Ok(()) => {
            println!("wrote {}", gen_uniques::OUT);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gen-uniques: {e}");
            ExitCode::from(2)
        }
    }
}

fn write_gen_params(root: &Path) -> ExitCode {
    let written = gen_params::generate(root).and_then(|text| {
        let path = root.join(gen_params::OUT);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    });
    match written {
        Ok(()) => {
            println!("wrote {}", gen_params::OUT);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gen-params: {e}");
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["check"] => check::run(&workspace_root()),
        ["gen-uniques"] => write_gen_uniques(&workspace_root()),
        ["gen-params"] => write_gen_params(&workspace_root()),
        ["perf", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|x| (*x).to_owned()).collect();
            perf::run(&workspace_root(), &rest)
        }
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
