//! Restricted mutation access (DESIGN.md 6.4).
//!
//! `State`'s mutable accessors hand out `&mut` without bumping any revision, so a caller outside
//! `game::mutate` could change state behind the memos' backs. They are `pub(crate)`, and only
//! `game/mutate.rs` (which bumps revisions first), `save/` (loading) and `compat/` (conversion)
//! may call them. Defining one (`fn tiles_mut`) is not a call.
//!
//! The list is checked against `state/mod.rs` too: an accessor it names that `State` no longer
//! defines is a finding, so a rename cannot quietly lift the restriction.

use super::Finding;
use super::source::SourceTree;

const CHECK: &str = "mutation";

/// The accessors, from `state/mod.rs` (package 1a-08). The settings are here with the
/// containers: nearly every cache reads them.
pub const ACCESSORS: &[&str] = &[
    "tiles_mut",
    "units_mut",
    "cities_mut",
    "players_mut",
    "diplo_mut",
    "world_mut",
    "config_mut",
];

/// The file that defines them.
pub const DEFINED_IN: &str = "state/mod.rs";

/// The files and directories (paths relative to `src/`) that may call them.
pub const ALLOWED: &[&str] = &["game/mutate.rs", "save/", "compat/"];

fn allowed(rel: &str) -> bool {
    ALLOWED.iter().any(|a| if a.ends_with('/') { rel.starts_with(a) } else { rel == *a })
}

pub fn check(tree: &SourceTree) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in tree.files.iter().filter(|f| !allowed(&f.rel)) {
        for (i, t) in file.tokens.iter().enumerate() {
            let Some(name) = ACCESSORS.iter().find(|a| t.is_ident(a)) else {
                continue;
            };
            if i > 0 && file.tokens[i - 1].is_ident("fn") {
                continue;
            }
            out.push(Finding::new(
                CHECK,
                format!(
                    "{}: calls State::{name}; only game/mutate.rs, save/ and compat/ may, so \
                     that every write bumps its revisions (write through Game's setters or a Touch)",
                    file.at(t.line)
                ),
            ));
        }
    }
    if let Some(state) = tree.files.iter().find(|f| f.rel == DEFINED_IN) {
        for name in ACCESSORS {
            let defined =
                state.tokens.windows(2).any(|w| w[0].is_ident("fn") && w[1].is_ident(name));
            if !defined {
                out.push(Finding::new(
                    CHECK,
                    format!(
                        "{}/{DEFINED_IN} no longer defines State::{name}: update the list of \
                         restricted accessors in xtask/src/check/access.rs",
                        super::ENGINE_SRC
                    ),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::source::SourceFile;

    fn run(rel: &str, text: &str) -> usize {
        let tree = SourceTree { files: vec![SourceFile::new(rel, text)] };
        check(&tree).len()
    }

    #[test]
    fn a_call_from_a_rule_system_is_refused() {
        assert_eq!(
            run("game/cities/founding.rs", "fn f(st: &mut State) { st.cities_mut().x(); }"),
            1
        );
        assert_eq!(run("game/combat/resolve.rs", "let u = State::units_mut(&mut st);"), 1);
        assert_eq!(run("state/cities.rs", "fn g(&mut self) { self.cities_mut(); }"), 1);
        assert_eq!(run("game/setup.rs", "fn g(st: &mut State) { st.config_mut().seed = 1; }"), 1);
    }

    #[test]
    fn the_sanctioned_callers_pass() {
        let call = "fn f(st: &mut State) { st.tiles_mut(); st.world_mut(); }";
        assert_eq!(run("game/mutate.rs", call), 0);
        assert_eq!(run("save/json.rs", call), 0);
        assert_eq!(run("compat/python/convert.rs", call), 0);
    }

    #[test]
    fn definitions_comments_and_strings_are_not_calls() {
        let src = "pub(crate) fn tiles_mut(&mut self) -> &mut Tiles { &mut self.tiles }\n\
                   // st.units_mut()\nconst S: &str = \"players_mut\";";
        assert_eq!(run("state/map.rs", src), 0);
        assert_eq!(run("game/mutate_helpers.rs", "fn f() { st.diplo_mut(); }"), 1);
    }

    #[test]
    fn an_accessor_missing_from_state_is_reported() {
        let most: String = ACCESSORS
            .iter()
            .skip(1)
            .map(|a| format!("pub(crate) fn {a}(&mut self) {{}}\n"))
            .collect();
        assert_eq!(run(DEFINED_IN, &most), 1, "tiles_mut is not defined");
    }

    /// Gate 7 of package 1a-08, on the engine as it is: the real tree passes, and the same tree
    /// with one rule file calling an accessor fails, once per call.
    #[test]
    fn the_engine_passes_and_a_stray_call_fails() -> Result<(), String> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(super::super::ENGINE_SRC);
        let mut tree = SourceTree::load(&src)?;
        assert!(tree.files.iter().any(|f| f.rel == DEFINED_IN), "the engine has state/mod.rs");
        assert_eq!(check(&tree), Vec::new());
        tree.files.push(SourceFile::new(
            "game/cities/founding.rs",
            "fn found(st: &mut State) { st.tiles_mut(); st.players_mut(); }",
        ));
        assert_eq!(check(&tree).len(), 2);
        Ok(())
    }
}
