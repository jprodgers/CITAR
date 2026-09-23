//! Layering: each top-level module of the engine uses only the layers below it (DESIGN.md 3.1).
//!
//! A file's layer is its top-level module (`game/cities/stats.rs` is in `game`). Every path that
//! reaches the crate root, as `crate::`, `$crate::`, `citar_engine::` or enough `super::`s, must
//! go on to name a layer the file may use. `lib.rs` is the root and may use everything.
//!
//! Paths into the root that would hide which layer they reach are refused too: a glob of the
//! root, an alias of the root (`use crate as x`, `use crate::{self as x}`), and a root item
//! that is not a layer, such as a re-export. Macros exported at the root (`crate::name!`) are
//! allowed.

use super::Finding;
use super::source::{SourceFile, SourceTree};
use crate::lexer::{Tok, Token};
use std::collections::BTreeSet;

const CHECK: &str = "layers";

/// Each layer, its level in DESIGN.md 3.1, and the other layers it may use.
pub const LAYERS: &[(&str, &str, &[&str])] = &[
    ("base", "0", &[]),
    ("rules", "1", &["base", "unique"]),
    ("unique", "1", &["base", "rules"]),
    ("state", "2", &["base", "rules", "unique"]),
    ("save", "2", &["base", "rules", "unique", "state"]),
    ("compat", "2", &["base", "rules", "unique", "state", "save"]),
    ("mapgen", "3a", &["base", "rules", "unique", "state"]),
    ("game", "3", &["base", "rules", "unique", "state", "save", "compat", "mapgen"]),
    ("api", "4", &["base", "rules", "unique", "state", "save", "compat", "mapgen", "game"]),
];

fn lookup(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    LAYERS.iter().find(|(n, _, _)| *n == name).map(|(_, level, uses)| (*level, *uses))
}

pub fn check(tree: &SourceTree) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut unknown = BTreeSet::new();
    for file in &tree.files {
        let Some(layer) = file.layer() else { continue };
        match lookup(&layer) {
            Some((level, uses)) => {
                FileCheck { file, layer: &layer, level, uses, out: &mut out }.run()
            }
            None => {
                unknown.insert(layer);
            }
        }
    }
    for layer in unknown {
        out.push(Finding::new(
            CHECK,
            format!(
                "`{layer}` is not a known layer: add it to LAYERS in xtask/src/check/layers.rs \
                 with the layers it may use (DESIGN.md 3.1)"
            ),
        ));
    }
    out
}

struct FileCheck<'a> {
    file: &'a SourceFile,
    layer: &'a str,
    level: &'static str,
    uses: &'static [&'static str],
    out: &'a mut Vec<Finding>,
}

impl FileCheck<'_> {
    fn toks(&self) -> &[Token] {
        &self.file.tokens
    }

    fn report(&mut self, line: u32, message: String) {
        let message = format!("{}: {message}", self.file.at(line));
        self.out.push(Finding::new(CHECK, message));
    }

    fn run(mut self) {
        let file_depth = self.file.module_path().len();
        // Inline `mod name { ... }` blocks, as the brace depth each one opened at, so `super`
        // resolves from the right module.
        let mut inline: Vec<usize> = Vec::new();
        let mut depth = 0usize;
        let mut opens_mod = false;
        let mut i = 0;
        while i < self.toks().len() {
            let t = &self.toks()[i];
            let line = t.line;
            let next = |k: usize| self.toks().get(i + k);
            let starts_path = i == 0 || !self.toks()[i - 1].is_path_sep();
            let then_sep = next(1).is_some_and(Token::is_path_sep);

            if t.is_punct('{') {
                if opens_mod {
                    inline.push(depth);
                    opens_mod = false;
                }
                depth += 1;
            } else if t.is_punct('}') {
                depth = depth.saturating_sub(1);
                if inline.last() == Some(&depth) {
                    inline.pop();
                }
            } else if t.is_ident("mod")
                && next(1).is_some_and(|t| matches!(t.tok, Tok::Ident(_)))
                && next(2).is_some_and(|t| t.is_punct('{'))
            {
                opens_mod = true;
            } else if (t.is_ident("crate") || t.is_ident("citar_engine")) && starts_path && then_sep
            {
                self.root_path(i + 2, line);
            } else if t.is_ident("crate") && next(1).is_some_and(|t| t.is_ident("as")) {
                self.report(
                    line,
                    "aliases the crate root, which hides which layers it reaches".into(),
                );
            } else if t.is_ident("extern")
                && next(1).is_some_and(|t| t.is_ident("crate"))
                && next(2).is_some_and(|t| t.is_ident("self"))
            {
                self.report(line, "`extern crate self` aliases the crate root".into());
            } else if t.is_ident("super") && starts_path && then_sep {
                let mut j = i;
                let mut climbs = 0;
                while self.toks().get(j).is_some_and(|t| t.is_ident("super"))
                    && self.toks().get(j + 1).is_some_and(Token::is_path_sep)
                {
                    climbs += 1;
                    j += 2;
                }
                if climbs == file_depth + inline.len() {
                    self.root_path(j, line);
                }
                i = j;
                continue;
            }
            i += 1;
        }
    }

    /// Checks what follows a path that has reached the crate root, starting at token `k`.
    fn root_path(&mut self, k: usize, line: u32) {
        match self.toks().get(k).map(|t| t.tok.clone()) {
            Some(Tok::Ident(name)) => {
                let bang = self.toks().get(k + 1).is_some_and(|t| t.is_punct('!'));
                self.root_item(&name, bang, line);
            }
            Some(Tok::Punct('*')) => self.glob(line),
            Some(Tok::Punct('{')) => self.root_group(k + 1),
            _ => {}
        }
    }

    /// `crate::{a, b::c, d::{e, f}}`: checks the first segment of each item.
    fn root_group(&mut self, mut k: usize) {
        loop {
            let Some(t) = self.toks().get(k).cloned() else {
                return;
            };
            match &t.tok {
                Tok::Punct('}') => return,
                Tok::Ident(name) if name == "self" => {
                    self.report(
                        t.line,
                        "aliases the crate root, which hides which layers it reaches".into(),
                    );
                }
                Tok::Ident(name) => {
                    let bang = self.toks().get(k + 1).is_some_and(|t| t.is_punct('!'));
                    self.root_item(name, bang, t.line);
                }
                Tok::Punct('*') => self.glob(t.line),
                _ => {}
            }
            // Skip to the comma that ends this item, or the brace that ends the group.
            let mut nested = 0usize;
            while let Some(t) = self.toks().get(k) {
                if t.is_punct('{') {
                    nested += 1;
                } else if t.is_punct('}') {
                    if nested == 0 {
                        return;
                    }
                    nested -= 1;
                } else if t.is_punct(',') && nested == 0 {
                    break;
                }
                k += 1;
            }
            k += 1;
        }
    }

    fn root_item(&mut self, name: &str, is_macro: bool, line: u32) {
        if is_macro {
            return;
        }
        let Some((level, _)) = lookup(name) else {
            self.report(
                line,
                format!(
                    "reaches `{name}` at the crate root; name what it re-exports through its \
                     layer path instead, so the layer it comes from is visible"
                ),
            );
            return;
        };
        if name == self.layer || self.uses.contains(&name) {
            return;
        }
        let may = if self.uses.is_empty() { "nothing".to_string() } else { self.uses.join(", ") };
        self.report(
            line,
            format!(
                "{} (layer {}) may not use {name} (layer {level}); it may use {may} (DESIGN.md 3.1)",
                self.layer, self.level
            ),
        );
    }

    fn glob(&mut self, line: u32) {
        self.report(line, "a glob import of the crate root hides which layers it reaches".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(files: &[(&str, &str)]) -> Vec<String> {
        let tree = SourceTree {
            files: files.iter().map(|(rel, text)| SourceFile::new(rel, text)).collect(),
        };
        check(&tree).into_iter().map(|f| f.message).collect()
    }

    fn one(rel: &str, text: &str) -> Vec<String> {
        run(&[(rel, text)])
    }

    #[test]
    fn the_table_names_only_layers() {
        for (name, _, uses) in LAYERS {
            for used in *uses {
                assert!(lookup(used).is_some(), "{name} uses unknown layer {used}");
                assert_ne!(name, used);
            }
        }
    }

    #[test]
    fn api_inside_game_is_refused() {
        let found = one("game/core.rs", "use crate::api::views;\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with(
                "crates/citar-engine/src/game/core.rs:1: game (layer 3) may not use api"
            )
        );
    }

    #[test]
    fn downward_and_same_layer_paths_pass() {
        let src = "use crate::base::ids::CityId;\nuse crate::game::mutate;\n\
                   fn f() -> crate::rules::Ruleset { crate::state::State::new() }\n";
        assert_eq!(one("game/core.rs", src), Vec::<String>::new());
        assert_eq!(one("rules/load.rs", "use crate::unique::compile;"), Vec::<String>::new());
        assert_eq!(one("unique/compile.rs", "use crate::rules::Ruleset;"), Vec::<String>::new());
    }

    #[test]
    fn every_upward_path_form_is_seen() {
        // Inline paths, $crate in macros, the crate's own name, and macro bodies.
        assert_eq!(one("base/ids.rs", "fn f() { crate::state::x(); }").len(), 1);
        assert_eq!(one("base/ids.rs", "macro_rules! m { () => { $crate::game::x() } }").len(), 1);
        assert_eq!(one("base/ids.rs", "use citar_engine::rules;").len(), 1);
        assert_eq!(one("state/mod.rs", "fn f() { assert!(crate::save::ok()); }").len(), 1);
    }

    #[test]
    fn groups_are_read_item_by_item() {
        let found = one("state/units.rs", "use crate::{base::{ids, sets}, rules, game::Game};");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("may not use game"));
    }

    #[test]
    fn super_is_resolved_through_inline_modules() {
        // game/cities/stats.rs is game::cities::stats: three supers reach the root.
        let upward = "use super::super::super::api::x;";
        assert_eq!(one("game/cities/stats.rs", upward).len(), 1);
        // Inside `mod tests`, the same three supers only reach `game`.
        let inside = "mod tests { use super::super::super::api::x; }";
        assert_eq!(one("game/cities/stats.rs", inside), Vec::<String>::new());
        // ... and four reach the root again, but not after the module closes.
        let four = "mod tests { use super::super::super::super::api::x; }\nuse super::super::x;";
        assert_eq!(one("game/cities/stats.rs", four).len(), 1);
        // Braces that are not modules do not count.
        let fn_body = "fn f() { { use super::super::super::api::x; } }";
        assert_eq!(one("game/cities/stats.rs", fn_body).len(), 1);
        // A layer's mod.rs is one level down.
        assert_eq!(one("game/mod.rs", "use super::api;").len(), 1);
        assert_eq!(one("game/mod.rs", "use super::base;"), Vec::<String>::new());
    }

    #[test]
    fn hidden_routes_to_the_root_are_refused() {
        assert_eq!(one("game/core.rs", "use crate::*;").len(), 1);
        assert_eq!(one("game/mod.rs", "use super::*;").len(), 1);
        assert_eq!(one("game/core.rs", "use crate as root;").len(), 1);
        assert_eq!(one("game/core.rs", "use crate::{self as root};").len(), 1);
        assert_eq!(one("game/core.rs", "extern crate self as root;").len(), 1);
        assert_eq!(one("game/core.rs", "use crate::Game;").len(), 1);
    }

    #[test]
    fn what_is_not_a_path_is_ignored() {
        let src = "// use crate::api;\n/// [`crate::api::Game`]\nconst S: &str = \"crate::api\";\n\
                   pub(crate) fn f() {}\npub(super) fn g() {}\nfn h() { crate::define_id!(X); }";
        assert_eq!(one("base/ids.rs", src), Vec::<String>::new());
    }

    #[test]
    fn the_root_may_use_everything() {
        assert_eq!(one("lib.rs", "pub use crate::api::Game;\nuse crate::*;"), Vec::<String>::new());
    }

    #[test]
    fn unknown_layers_are_reported_once() {
        let found = run(&[("extra/a.rs", ""), ("extra/b.rs", ""), ("stray.rs", "")]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("`extra` is not a known layer"));
        assert!(found[1].starts_with("`stray` is not a known layer"));
    }
}
