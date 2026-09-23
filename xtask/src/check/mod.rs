//! `cargo xtask check`: the rules of DESIGN.md that clippy cannot see.
//!
//! - the engine's normal dependencies stay on the allow-list, with `libm` pinned and plain;
//! - the workspace version equals `__version__` in `citar/__init__.py`;
//! - each layer of the engine uses only the layers below it;
//! - only `game/mutate.rs`, `save/` and `compat/` call `State`'s mutable accessors;
//! - generated files are up to date;
//! - no `Pending` stage outlives its package, and, once switched on, no `NotPorted` or
//!   `Pending` remains at all.
//!
//! Exit code 0 means every check passed, 1 that at least one found a problem, and 2 that the
//! checks could not run.

mod access;
mod config;
mod deps;
mod generated;
mod layers;
mod metadata;
mod pending;
mod source;
mod version;

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

/// One broken rule, reported as `[check] message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub message: String,
}

impl Finding {
    pub fn new(check: &'static str, message: impl Into<String>) -> Self {
        Finding { check, message: message.into() }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.check, self.message)
    }
}

/// Where the engine's sources live, relative to the workspace root.
pub const ENGINE_SRC: &str = "crates/citar-engine/src";

/// Runs every check against the workspace at `root` and prints the result.
pub fn run(root: &Path) -> ExitCode {
    match run_all(root) {
        Ok((findings, summary)) => {
            for finding in &findings {
                println!("{finding}");
            }
            println!("{summary}");
            if findings.is_empty() {
                println!("xtask check: all clear");
                ExitCode::SUCCESS
            } else {
                println!("xtask check: {} problem(s)", findings.len());
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("xtask check: could not run: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_all(root: &Path) -> Result<(Vec<Finding>, String), String> {
    let config = config::Config::load(root)?;
    let meta = metadata::Metadata::load(root)?;
    let tree = source::SourceTree::load(&root.join(ENGINE_SRC))?;

    let mut findings = Vec::new();
    findings.extend(deps::check(&meta));
    findings.extend(version::check(root, &meta)?);
    findings.extend(layers::check(&tree));
    findings.extend(access::check(&tree));
    findings.extend(generated::check(root, generated::FILES));
    let markers = pending::scan(&tree);
    findings.extend(pending::check(&markers, &config));

    let summary = format!(
        "engine: {} source files; {} NotPorted, {} Pending stages",
        tree.files.len(),
        markers.count(pending::Kind::NotPorted),
        markers.count(pending::Kind::Pending),
    );
    Ok((findings, summary))
}
