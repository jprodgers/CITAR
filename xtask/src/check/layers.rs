//! Layering: each top-level module of the engine uses only the layers below it (DESIGN.md 3.1).
//!
//! A file's layer is its top-level module (`game/cities/stats.rs` is in `game`). Every path that
//! reaches the crate root must go on to name a layer the file may use: `crate::`, `$crate::`,
//! `citar_engine::`, and chains of `self::` and `super::`, followed through use groups as well
//! (`use super::{super::api}` climbs twice). `lib.rs` is the root and may use everything.
//!
//! Paths into the root that would hide which layer they reach are refused too: a glob of the
//! root, an alias of the root (`use crate as x`, `use crate::{self as x}`, `use super as x` from
//! a layer's `mod.rs`), and a root item that is not a layer, such as a re-export.
//!
//! A `#[macro_export]` macro lives at the crate root whichever file defines it, so
//! `crate::name!` counts as a use of the layer that defines `name`: a macro whose body reaches
//! into `game` must not become a way for `base` to do so. Macros defined in `lib.rs` may be used
//! everywhere.

use super::Finding;
use super::source::{SourceFile, SourceTree};
use crate::lexer::{Tok, Token};
use std::collections::{BTreeMap, BTreeSet};

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

/// Exported macros by name, with the layer whose file defines each (`None` for `lib.rs`).
type Macros = BTreeMap<String, Option<String>>;

pub fn check(tree: &SourceTree) -> Vec<Finding> {
    let macros = exported_macros(tree);
    let mut out = Vec::new();
    let mut unknown = BTreeSet::new();
    for file in &tree.files {
        let Some(layer) = file.layer() else { continue };
        match lookup(&layer) {
            Some((level, uses)) => FileCheck {
                file,
                layer: &layer,
                level,
                uses,
                macros: &macros,
                consumed: vec![false; file.tokens.len()],
                out: &mut out,
            }
            .run(),
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

/// Every `macro_rules! name` with `#[macro_export]` among its attributes.
fn exported_macros(tree: &SourceTree) -> Macros {
    let mut out = Macros::new();
    for file in &tree.files {
        let toks = &file.tokens;
        for (i, t) in toks.iter().enumerate() {
            if !t.is_ident("macro_rules") || !toks.get(i + 1).is_some_and(|t| t.is_punct('!')) {
                continue;
            }
            let Some(Tok::Ident(name)) = toks.get(i + 2).map(|t| &t.tok) else { continue };
            // `#[cfg_attr(..., macro_export)]` counts: it exports in some builds.
            if attributes_before(toks, i).iter().any(|t| t.is_ident("macro_export")) {
                out.insert(name.clone(), file.layer());
            }
        }
    }
    out
}

/// The tokens of the `#[...]` attributes directly before token `i`.
fn attributes_before(toks: &[Token], i: usize) -> &[Token] {
    let mut start = i;
    while start > 0 && toks[start - 1].is_punct(']') {
        // Find the `[` that opens this attribute.
        let mut depth = 0usize;
        let mut k = start - 1;
        let open = loop {
            if toks[k].is_punct(']') {
                depth += 1;
            } else if toks[k].is_punct('[') {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break Some(k);
                }
            }
            if k == 0 {
                break None;
            }
            k -= 1;
        };
        match open {
            Some(k) if k > 0 && toks[k - 1].is_punct('#') => start = k - 1,
            _ => break,
        }
    }
    &toks[start..i]
}

struct FileCheck<'a> {
    file: &'a SourceFile,
    layer: &'a str,
    level: &'static str,
    uses: &'static [&'static str],
    macros: &'a Macros,
    /// Tokens already read as part of a path, so the items of a use group are not read a second
    /// time as paths of their own, from the wrong module.
    consumed: Vec<bool>,
    out: &'a mut Vec<Finding>,
}

impl FileCheck<'_> {
    fn tok(&self, k: usize) -> Option<&Token> {
        self.file.tokens.get(k)
    }

    fn sep_at(&self, k: usize) -> bool {
        self.tok(k).is_some_and(Token::is_path_sep)
    }

    fn report(&mut self, line: u32, message: String) {
        let message = format!("{}: {message}", self.file.at(line));
        self.out.push(Finding::new(CHECK, message));
    }

    fn run(mut self) {
        let file = self.file;
        let file_depth = file.module_path().len();
        // Inline `mod name { ... }` blocks, as the brace depth each one opened at, so `super`
        // resolves from the right module.
        let mut inline: Vec<usize> = Vec::new();
        let mut depth = 0usize;
        let mut opens_mod = false;
        for i in 0..file.tokens.len() {
            let t = &file.tokens[i];
            let line = t.line;
            let next = |k: usize| file.tokens.get(i + k);

            if t.is_punct('{') {
                if opens_mod {
                    inline.push(depth);
                    opens_mod = false;
                }
                depth += 1;
                continue;
            }
            if t.is_punct('}') {
                depth = depth.saturating_sub(1);
                if inline.last() == Some(&depth) {
                    inline.pop();
                }
                continue;
            }
            if t.is_ident("mod")
                && next(1).is_some_and(|t| matches!(t.tok, Tok::Ident(_)))
                && next(2).is_some_and(|t| t.is_punct('{'))
            {
                opens_mod = true;
                continue;
            }
            if self.consumed[i] || (i > 0 && file.tokens[i - 1].is_path_sep()) {
                continue;
            }

            let then_sep = next(1).is_some_and(Token::is_path_sep);
            let then_as = next(1).is_some_and(|t| t.is_ident("as"));
            let end = if (t.is_ident("crate") || t.is_ident("citar_engine")) && then_sep {
                self.walk(i + 2, 0)
            } else if t.is_ident("crate") && then_as {
                self.report(
                    line,
                    "aliases the crate root, which hides which layers it reaches".into(),
                );
                i + 1
            } else if t.is_ident("extern")
                && next(1).is_some_and(|t| t.is_ident("crate"))
                && next(2).is_some_and(|t| t.is_ident("self"))
            {
                self.report(line, "`extern crate self` aliases the crate root".into());
                i + 1
            } else if (t.is_ident("super") && (then_sep || then_as))
                || (t.is_ident("self") && then_sep)
            {
                self.walk(i, file_depth + inline.len())
            } else {
                continue;
            };
            for c in &mut self.consumed[i..end.max(i + 1)] {
                *c = true;
            }
        }
    }

    /// Follows a path from token `k`, which is `depth` modules below the crate root, through its
    /// `self::` and `super::` segments and into use groups. Returns the index just past the
    /// tokens it read.
    fn walk(&mut self, mut k: usize, mut depth: usize) -> usize {
        loop {
            let Some(t) = self.tok(k) else { return k };
            let is_super = t.is_ident("super");
            if !is_super && !t.is_ident("self") {
                break;
            }
            let line = t.line;
            if self.sep_at(k + 1) {
                if is_super {
                    depth = depth.saturating_sub(1);
                }
                k += 2;
                continue;
            }
            // A last segment: `use super as x`, or `super` inside a group.
            if is_super && depth == 1 && self.tok(k + 1).is_some_and(|t| t.is_ident("as")) {
                self.report(
                    line,
                    "aliases the crate root, which hides which layers it reaches".into(),
                );
            }
            return k + 1;
        }
        if depth == 0 {
            self.root_path(k)
        } else if self.tok(k).is_some_and(|t| t.is_punct('{')) {
            self.group(k, depth)
        } else {
            // A path that goes down from here cannot climb back: `super` may only follow
            // `self`, `super` or the start of a path.
            k
        }
    }

    /// Checks what follows a path that has reached the crate root, starting at token `k`.
    /// Returns the index just past the tokens it read.
    fn root_path(&mut self, k: usize) -> usize {
        let Some(t) = self.tok(k).cloned() else { return k };
        match &t.tok {
            Tok::Ident(name) if name == "self" => {
                self.report(
                    t.line,
                    "aliases the crate root, which hides which layers it reaches".into(),
                );
                k + 1
            }
            Tok::Ident(name) => {
                let bang = self.tok(k + 1).is_some_and(|t| t.is_punct('!'));
                self.root_item(name, bang, t.line);
                k + 1
            }
            Tok::Punct('*') => {
                self.report(
                    t.line,
                    "a glob import of the crate root hides which layers it reaches".into(),
                );
                k + 1
            }
            Tok::Punct('{') => self.group(k, 0),
            _ => k,
        }
    }

    /// A use group opening at token `open`, whose prefix is `depth` modules below the root:
    /// `crate::{a, b::c, {d, e}}` or `super::{self as x, super::y}`. Each item is followed on
    /// from the prefix. Returns the index just past the closing brace.
    fn group(&mut self, open: usize, depth: usize) -> usize {
        let mut k = open + 1;
        loop {
            match self.tok(k) {
                None => return k,
                Some(t) if t.is_punct('}') => return k + 1,
                Some(_) => {}
            }
            if depth == 0 {
                self.root_path(k);
            } else {
                self.walk(k, depth);
            }
            // Skip to the comma that ends this item, or the brace that ends the group.
            let mut nested = 0usize;
            while let Some(t) = self.tok(k) {
                if t.is_punct('{') {
                    nested += 1;
                } else if t.is_punct('}') {
                    if nested == 0 {
                        return k + 1;
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

    /// `name` reached at the crate root: a layer, an exported macro, or anything else.
    fn root_item(&mut self, name: &str, is_macro: bool, line: u32) {
        let macros = self.macros;
        let exported = macros.get(name);
        if is_macro || (exported.is_some() && lookup(name).is_none()) {
            // A macro defined in lib.rs, or one this check cannot see defined, passes.
            if let Some(Some(defined_in)) = exported {
                self.may_use(defined_in, line, Some(name));
            }
            return;
        }
        if lookup(name).is_none() {
            self.report(
                line,
                format!(
                    "reaches `{name}` at the crate root; name what it re-exports through its \
                     layer path instead, so the layer it comes from is visible"
                ),
            );
            return;
        }
        self.may_use(name, line, None);
    }

    /// Reports a use of layer `used` unless this file's layer may use it.
    fn may_use(&mut self, used: &str, line: u32, via_macro: Option<&str>) {
        let Some((level, _)) = lookup(used) else { return };
        if used == self.layer || self.uses.contains(&used) {
            return;
        }
        let via = match via_macro {
            Some(m) => format!(" through the macro `{m}!`, which {used} exports"),
            None => String::new(),
        };
        let may = if self.uses.is_empty() { "nothing".to_string() } else { self.uses.join(", ") };
        self.report(
            line,
            format!(
                "{} (layer {}) may not use {used} (layer {level}){via}; it may use {may} \
                 (DESIGN.md 3.1)",
                self.layer, self.level
            ),
        );
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
    fn nested_groups_are_read_too() {
        // A group inside a group, with no prefix of its own.
        let found = one("game/core.rs", "use crate::{base, {api, rules::x}};");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("may not use api"));
        assert_eq!(one("base/ids.rs", "use crate::{{{game::x}}};").len(), 1);
    }

    #[test]
    fn super_chains_through_self_and_groups() {
        // game/mod.rs is one level down, so self::super is the root.
        assert_eq!(one("game/mod.rs", "use self::super::api as a2;").len(), 1);
        // game/cities.rs is two levels down, and each of these climbs twice.
        for src in [
            "use super::{super::api};",
            "use self::{super::super::api};",
            "use self::super::{self as g, super::api::x};",
            "use super::{cities::x, {super::api}};",
            "fn f() { self::super::super::api::x(); }",
        ] {
            let found = one("game/cities.rs", src);
            assert_eq!(found.len(), 1, "{src}: {found:?}");
            assert!(found[0].contains("may not use api"), "{src}: {found:?}");
        }
        // The same forms stopping short of the root, or reaching an allowed layer, pass.
        for src in ["use super::{self as g, super::base, cities::x};", "use self::super::x;"] {
            assert_eq!(one("game/cities.rs", src), Vec::<String>::new(), "{src}");
        }
    }

    #[test]
    fn super_as_an_alias_of_the_root_is_refused() {
        assert_eq!(one("game/mod.rs", "use super as root;").len(), 1);
        assert_eq!(one("game/cities.rs", "use super::super as root;").len(), 1);
        assert_eq!(one("game/cities.rs", "use super::{super as root};").len(), 1);
        // One level up from game/cities.rs is game, not the root; and pub(super) is no path.
        let fine = "use super as g;\npub(super) fn f() {}";
        assert_eq!(one("game/cities.rs", fine), Vec::<String>::new());
    }

    #[test]
    fn an_exported_macro_counts_as_a_use_of_its_layer() {
        let game_macros = (
            "game/macros.rs",
            "#[macro_export]\nmacro_rules! from_game { () => { $crate::game::x() } }\n\
             #[cfg_attr(test, macro_export)]\nmacro_rules! also_game { () => {} }\n\
             macro_rules! local { () => {} }",
        );
        let lib_macro = ("lib.rs", "#[doc(hidden)]\n#[macro_export]\nmacro_rules! assert_send {}");
        let base = ("base/ids.rs", "fn f() { crate::from_game!(); crate::also_game!(); }");
        let found = run(&[game_macros, lib_macro, base]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].contains(
                "base (layer 0) may not use game (layer 3) through the macro `from_game!`"
            ),
            "{found:?}"
        );
        // Importing it by name is the same use.
        assert_eq!(run(&[game_macros, ("state/x.rs", "use crate::from_game;")]).len(), 1);
        // From a layer that may use game, from game itself, and macros from lib.rs: fine.
        let fine = run(&[
            game_macros,
            lib_macro,
            ("api/x.rs", "fn f() { crate::from_game!(); }"),
            ("game/y.rs", "fn f() { crate::from_game!(); }"),
            ("base/z.rs", "crate::assert_send!(u8);\nuse crate::assert_send;"),
        ]);
        assert_eq!(fine, Vec::<String>::new());
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
