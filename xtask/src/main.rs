//! `cargo xtask`: the workspace's own checks (DESIGN.md 2.1).
//!
//! `check` enforces the rules of DESIGN.md that clippy cannot see; `gen-uniques` writes the
//! engine's unique types from their two sources (1a-05); `gen-params` writes the bot's parameter
//! struct from its schema (registered in 2-00a, written in 2-01a); `perf` runs the criterion
//! suites and holds them to their budgets (perfgate, 1e-03; the `games` suite from 2-00a);
//! `develop` builds the Python extension for the laptop's venv (DESIGN.md P2.6.7, 2-06a);
//! `keygen`, `helper-manifest`, `sign-manifest` and `verify-manifest` are the release side of the
//! helper's update channel (design/phase4-helper.md P4.3.8, 4-13; design/phase5-release.md P5.4.2,
//! 5-03); `licenses` writes the third-party notices the release's binaries carry
//! (design/phase5-release.md P5.5, 5-04).

#![forbid(unsafe_code)]

mod check;
mod develop;
mod gen_params;
mod gen_uniques;
mod lexer;
mod licenses;
mod manifest;
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
           (exit 0 written, 2 could not)
  develop [--release] [--venv DIR]
           build citar._engine (ci profile and test-ops, or the release profile) into
           $CARGO_TARGET_DIR/citar-ext for this worktree's venv, outside the checkout
           (exit 0 built, 1 failed, 2 refused)
  perf [--check] [--suite kernels|turns|io|games] [-- <bench arguments>]
           run the criterion suites of citar-bench (games only when named), then check every
           budget of crates/citar-bench/thresholds.toml and the pass rounds' ratios to Python
           (--check: only check what a run left; exit 0 within, 1 over, 2 could not run)
  keygen --out DIR
           a new update-signing key: DIR/citar-update-signing.key (private) and .pub; refuses a
           directory inside this repository or any git working tree (exit 0 made, 2 refused)
  helper-manifest --assets DIR --version V [--base-url URL] [--build B] [--protocol P]
                  [--released-at T] [--out FILE]
           citar-helper-manifest.json over the release's helper binaries in DIR
  sign-manifest (--key FILE | --key-env NAME) MANIFEST [--out SIG]
           MANIFEST.sig, the manifest signed for the helpers' updates; refuses a key file inside
           this repository or any git working tree (exit 0 signed, 1 failed, 2 refused)
  verify-manifest --pub FILE MANIFEST [SIG]
           whether SIG (MANIFEST.sig) signs MANIFEST with a public key of FILE (update-keys.pub's
           format), as a helper checks it (exit 0 verified, 1 does not verify, 2 refused)
  licenses [--check] [--metadata FILE] [--out FILE]
           packaging/THIRD-PARTY-LICENSES.txt: every third-party crate citar-py and citar-helper
           link on the release targets, from cargo metadata --locked (or FILE), with its licence
           and notice texts; a licence outside the allow-list is refused (--check: compare, write
           nothing; exit 0 written or current, 1 refused or out of date, 2 could not run)";

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
        ["develop", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|x| (*x).to_owned()).collect();
            develop::run_develop(&workspace_root(), &rest)
        }
        ["perf", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|x| (*x).to_owned()).collect();
            perf::run(&workspace_root(), &rest)
        }
        [
            command @ ("keygen" | "helper-manifest" | "sign-manifest" | "verify-manifest"),
            rest @ ..,
        ] => {
            let rest: Vec<String> = rest.iter().map(|x| (*x).to_owned()).collect();
            manifest::run(&workspace_root(), command, &rest)
        }
        ["licenses", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|x| (*x).to_owned()).collect();
            licenses::run(&workspace_root(), &rest)
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
