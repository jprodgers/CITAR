//! `NotPorted` errors and `Pending` stages: work that is knowingly unfinished (DESIGN.md 3.4).
//!
//! A rule effect whose dependency is not ported yet returns `NotPorted("combat::nuke")`, and a
//! turn or setup stage whose system is not ported yet is listed as `Pending("1b-05")`, naming
//! the package that will fill it. The switches in `xtask/check.toml` tighten this as the phase
//! goes on: a Pending stage fails once its package is listed as done; from 1c-10 any Pending
//! stage fails; from 1e-04 any NotPorted fails.
//!
//! A marker is counted by its string argument, so the argument must be a string literal, at
//! the marker itself: `Porting::Pending("1b-05")`, `NotPorted("combat::nuke")`, or the helper
//! `not_ported("combat::nuke")` (as a function or a macro). A marker whose argument is anything
//! else, such as a constant or a parameter passed through, would hide from the switches, so it
//! fails the check at once. What is not a marker is left alone: the definitions (an enum's
//! variant, `fn not_ported(...)`) and patterns (`Porting::Pending(pkg) =>`,
//! `matches!(p, Pending(_))`). Once NotPorted is forbidden, any path to it (`ErrCode::NotPorted`)
//! fails too, so a helper built on the bare variant cannot outlive the rule.

use super::Finding;
use super::config::Config;
use super::source::SourceTree;
use crate::lexer::{Tok, Token};

const CHECK: &str = "pending";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NotPorted,
    Pending,
}

/// The names a marker is written under.
const NAMES: &[(&str, Kind)] =
    &[("NotPorted", Kind::NotPorted), ("not_ported", Kind::NotPorted), ("Pending", Kind::Pending)];

#[derive(Debug)]
pub struct Marker {
    pub kind: Kind,
    /// The string argument: a module path for NotPorted, a package for Pending.
    pub arg: String,
    /// `crates/citar-engine/src/<file>:<line>`.
    pub at: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    /// Every marker with a string literal argument, in file order.
    pub markers: Vec<Marker>,
    /// Markers whose argument is not a string literal, as (name, place).
    pub unreadable: Vec<(String, String)>,
    /// Paths to the NotPorted variant that are not markers, such as `ErrCode::NotPorted`.
    pub not_ported_paths: Vec<String>,
}

impl Scan {
    pub fn count(&self, kind: Kind) -> usize {
        self.markers.iter().filter(|m| m.kind == kind).count()
    }
}

/// Every marker in the engine, in file order.
pub fn scan(tree: &SourceTree) -> Scan {
    let mut out = Scan::default();
    for file in &tree.files {
        let toks = &file.tokens;
        // For each open brace, whether it opens an enum body, where `Pending(...)` defines a
        // variant rather than using one.
        let mut braces: Vec<bool> = Vec::new();
        for i in 0..toks.len() {
            let t = &toks[i];
            if t.is_punct('{') {
                braces.push(opens_enum(toks, i));
                continue;
            }
            if t.is_punct('}') {
                braces.pop();
                continue;
            }
            let Some(&(name, kind)) = NAMES.iter().find(|(n, _)| t.is_ident(n)) else {
                continue;
            };
            let at = || file.at(t.line);
            let prev = |k: usize| i.checked_sub(k).map(|j| &toks[j]);
            // `not_ported!(...)` is the same marker as `not_ported(...)`.
            let mut open = i + 1;
            if toks.get(open).is_some_and(|t| t.is_punct('!')) {
                open += 1;
            }
            if !toks.get(open).is_some_and(|t| t.is_punct('(')) {
                if kind == Kind::NotPorted && prev(1).is_some_and(Token::is_path_sep) {
                    out.not_ported_paths.push(at());
                }
                continue;
            }
            let defines = prev(1).is_some_and(|t| t.is_ident("fn") || t.is_ident("struct"))
                || braces.last() == Some(&true);
            if defines {
                continue;
            }
            match toks.get(open + 1).map(|t| &t.tok) {
                Some(Tok::Str(arg)) => {
                    out.markers.push(Marker { kind, arg: arg.clone(), at: at() })
                }
                // `Pending(_)` and `Pending(..)` only ever match.
                Some(Tok::Ident(s)) if s == "_" => {}
                Some(Tok::Punct('.')) => {}
                // A macro passing its argument on: the literal is checked where it is called.
                Some(Tok::Punct('$')) => {}
                _ if is_pattern(toks, open) => {}
                _ => out.unreadable.push((name.to_string(), at())),
            }
        }
    }
    out
}

/// Whether the `{` at `i` opens the body of an `enum`.
fn opens_enum(toks: &[Token], i: usize) -> bool {
    for t in toks[..i].iter().rev() {
        if t.is_ident("enum") {
            return true;
        }
        if t.is_punct(';') || t.is_punct('{') || t.is_punct('}') || t.is_punct('=') {
            return false;
        }
    }
    false
}

/// Whether the parenthesised argument opening at `open` belongs to a pattern: what follows the
/// closing parenthesis (and any that close around it) is `=>`, `=`, `|` or a match guard.
fn is_pattern(toks: &[Token], open: usize) -> bool {
    let mut depth = 0usize;
    let mut k = open;
    while let Some(t) = toks.get(k) {
        if t.is_punct('(') || t.is_punct('[') {
            depth += 1;
        } else if t.is_punct(')') || t.is_punct(']') {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                break;
            }
        }
        k += 1;
    }
    k += 1;
    while toks.get(k).is_some_and(|t| t.is_punct(')') || t.is_punct(']')) {
        k += 1;
    }
    let at = |k: usize| toks.get(k);
    match at(k) {
        Some(t) if t.is_punct('=') => !at(k + 1).is_some_and(|t| t.is_punct('=')),
        Some(t) if t.is_punct('|') => !at(k + 1).is_some_and(|t| t.is_punct('|')),
        Some(t) => t.is_ident("if"),
        None => false,
    }
}

pub fn check(scan: &Scan, config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();
    for (name, at) in &scan.unreadable {
        out.push(Finding::new(
            CHECK,
            format!(
                "{at}: the argument of this {name}(...) is not a string literal, so xtask check \
                 cannot count it: write the package or module path there as a literal (a \
                 pattern binding it must end at `=>`, `=`, `|` or `if`, as in `{name}(p) =>`)"
            ),
        ));
    }
    for m in &scan.markers {
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
    if config.not_ported.forbid {
        for at in &scan.not_ported_paths {
            out.push(Finding::new(
                CHECK,
                format!("{at}: NotPorted is still reachable, and every rule must be ported now"),
            ));
        }
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

    fn scan_one(rel: &str, src: &str) -> Scan {
        scan(&SourceTree { files: vec![SourceFile::new(rel, src)] })
    }

    fn markers() -> Scan {
        scan_one("game/turn/stages.rs", SRC)
    }

    fn config(done: &[&str], forbid_all: bool, forbid_not_ported: bool) -> Config {
        let mut c = Config::default();
        c.pending.done = done.iter().map(|s| s.to_string()).collect();
        c.pending.forbid_all = forbid_all;
        c.not_ported.forbid = forbid_not_ported;
        c
    }

    fn found(scan: &Scan) -> Vec<(Kind, &str)> {
        scan.markers.iter().map(|m| (m.kind, m.arg.as_str())).collect()
    }

    #[test]
    fn markers_are_found_by_their_argument() {
        let m = markers();
        assert_eq!(
            found(&m),
            [(Kind::Pending, "1b-05"), (Kind::Pending, "1c-03"), (Kind::NotPorted, "combat::nuke")]
        );
        assert_eq!(m.markers[0].at, "crates/citar-engine/src/game/turn/stages.rs:3");
        assert!(m.unreadable.is_empty() && m.not_ported_paths.is_empty(), "{m:?}");
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

    #[test]
    fn the_not_ported_helper_is_a_marker_and_the_bare_variant_is_forbidden_later() {
        // DESIGN.md 8.5 makes NotPorted a unit variant of ErrCode, so 1b-01 will likely write a
        // helper around it.
        let src = "pub enum ErrCode { Rule, NotPorted }\n\
                   pub fn not_ported(what: &'static str) -> ActionError {\n\
                       ActionError { code: ErrCode::NotPorted, message: what.into() }\n\
                   }\n\
                   fn nuke() -> Result<(), ActionError> { Err(not_ported(\"combat::nuke\")) }\n\
                   fn spy() -> Result<(), ActionError> { Err(not_ported!(\"espionage::steal\")) }\n";
        let s = scan_one("api/error.rs", src);
        assert_eq!(
            found(&s),
            [(Kind::NotPorted, "combat::nuke"), (Kind::NotPorted, "espionage::steal")]
        );
        assert!(s.unreadable.is_empty(), "{s:?}");
        assert_eq!(s.not_ported_paths, ["crates/citar-engine/src/api/error.rs:3"]);
        assert_eq!(check(&s, &config(&[], false, false)), []);
        // Once forbidden: the two markers, and the path to the variant.
        assert_eq!(check(&s, &config(&[], false, true)).len(), 3);
    }

    #[test]
    fn an_argument_that_is_not_a_literal_fails_at_once() {
        let src = "const PKG: &str = \"1b-05\";\n\
                   const S: Stage = Stage { porting: Porting::Pending(PKG) };\n\
                   fn pending(pkg: &'static str) -> Porting { Porting::Pending(pkg) }\n\
                   fn nuke() -> Result<(), ActionError> { Err(not_ported(PATH)) }\n\
                   fn spy() -> Result<(), ActionError> { Err(NotPorted(&format!(\"x\"))) }\n";
        let s = scan_one("game/turn/stages.rs", src);
        assert!(s.markers.is_empty(), "{s:?}");
        let lines: Vec<&str> = s.unreadable.iter().map(|(_, at)| at.as_str()).collect();
        let at = |l: u32| format!("crates/citar-engine/src/game/turn/stages.rs:{l}");
        assert_eq!(lines, [at(2), at(3), at(4), at(5)]);
        let found = check(&s, &config(&[], false, false));
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found[0].message.contains("is not a string literal"));
    }

    #[test]
    fn definitions_and_patterns_are_not_markers() {
        let src = "#[derive(Debug)]\npub enum Porting { Ported, Pending(PackageId) }\n\
                   pub struct Pending(u8);\n\
                   pub fn not_ported(what: &'static str) -> ActionError { todo(what) }\n\
                   macro_rules! not_ported { ($p:literal) => { not_ported($p) } }\n\
                   fn run(s: &Stage, x: Option<Porting>) {\n\
                       match s.porting { Porting::Pending(pkg) => skip(pkg), Porting::Ported => {} }\n\
                       match s.porting { Porting::Pending(p) if p.is_empty() => {} _ => {} }\n\
                       match s.porting { Porting::Pending(..) | Porting::Ported => {} }\n\
                       if let Some(Porting::Pending(pkg)) = x { skip(pkg); }\n\
                       let _ = matches!(s.porting, Porting::Pending(_));\n\
                       let pending: Pending = Pending::default();\n\
                   }\n";
        let s = scan_one("game/turn/run.rs", src);
        assert!(s.markers.is_empty() && s.unreadable.is_empty(), "{s:?}");
    }
}
