//! `NotPorted` errors and `Pending` stages: work that is knowingly unfinished (DESIGN.md 3.4).
//!
//! A rule effect whose dependency is not ported yet returns `NotPorted("combat::nuke")`, and a
//! turn or setup stage whose system is not ported yet is listed as `Pending("1b-05")`, naming
//! the package that will fill it. Both are found by their string argument, so the definitions
//! of the variants themselves never count.
//!
//! The switches in `xtask/check.toml` tighten this as the phase goes on: a Pending stage fails
//! once its package is listed as done; from 1c-10 any Pending stage fails; from 1e-04 any
//! NotPorted fails.

use super::Finding;
use super::config::Config;
use super::source::SourceTree;
use crate::lexer::Tok;

const CHECK: &str = "pending";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NotPorted,
    Pending,
}

#[derive(Debug)]
pub struct Marker {
    pub kind: Kind,
    /// The string argument: a module path for NotPorted, a package for Pending.
    pub arg: String,
    /// `crates/citar-engine/src/<file>:<line>`.
    pub at: String,
}

/// Every `NotPorted("...")` and `Pending("...")` in the engine, in file order.
pub fn scan(tree: &SourceTree) -> Vec<Marker> {
    let mut out = Vec::new();
    for file in &tree.files {
        for w in file.tokens.windows(3) {
            let kind = if w[0].is_ident("NotPorted") {
                Kind::NotPorted
            } else if w[0].is_ident("Pending") {
                Kind::Pending
            } else {
                continue;
            };
            if let (true, Tok::Str(arg)) = (w[1].is_punct('('), &w[2].tok) {
                out.push(Marker { kind, arg: arg.clone(), at: file.at(w[0].line) });
            }
        }
    }
    out
}

pub fn check(markers: &[Marker], config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();
    for m in markers {
        let why = match m.kind {
            Kind::NotPorted if config.not_ported.forbid => {
                format!("NotPorted(\"{}\") remains, and every rule must be ported now", m.arg)
            }
            Kind::Pending if config.pending.forbid_all => {
                format!("stage Pending(\"{}\") remains, and every stage must be real now", m.arg)
            }
            Kind::Pending if config.pending.done.contains(&m.arg) => format!(
                "stage Pending(\"{}\") remains, but package {} is done: port the stage, or \
                 correct the package it names",
                m.arg, m.arg
            ),
            _ => continue,
        };
        out.push(Finding::new(CHECK, format!("{}: {why}", m.at)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::source::SourceFile;

    const SRC: &str = "pub enum StageFn { Real(fn(&mut Game)), Pending(&'static str) }\n\
                       pub enum EngineError { NotPorted(&'static str) }\n\
                       const TURN: &[StageFn] = &[StageFn::Pending(\"1b-05\"), Pending(\"1c-03\")];\n\
                       fn nuke() -> Result<(), EngineError> { Err(EngineError::NotPorted(\"combat::nuke\")) }\n";

    fn markers() -> Vec<Marker> {
        scan(&SourceTree { files: vec![SourceFile::new("game/turn/stages.rs", SRC)] })
    }

    fn config(done: &[&str], forbid_all: bool, forbid_not_ported: bool) -> Config {
        let mut c = Config::default();
        c.pending.done = done.iter().map(|s| s.to_string()).collect();
        c.pending.forbid_all = forbid_all;
        c.not_ported.forbid = forbid_not_ported;
        c
    }

    #[test]
    fn markers_are_found_by_their_argument() {
        let m = markers();
        let found: Vec<_> = m.iter().map(|m| (m.kind, m.arg.as_str())).collect();
        assert_eq!(
            found,
            [(Kind::Pending, "1b-05"), (Kind::Pending, "1c-03"), (Kind::NotPorted, "combat::nuke")]
        );
        assert_eq!(m[0].at, "crates/citar-engine/src/game/turn/stages.rs:3");
    }

    #[test]
    fn nothing_fails_while_the_switches_are_off() {
        assert_eq!(check(&markers(), &config(&["1a-01"], false, false)), []);
    }

    #[test]
    fn a_done_package_may_not_keep_a_pending_stage() {
        let found = check(&markers(), &config(&["1a-01", "1b-05"], false, false));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("Pending(\"1b-05\")"));
    }

    #[test]
    fn the_later_switches_forbid_everything() {
        assert_eq!(check(&markers(), &config(&[], true, false)).len(), 2);
        assert_eq!(check(&markers(), &config(&[], false, true)).len(), 1);
    }
}
