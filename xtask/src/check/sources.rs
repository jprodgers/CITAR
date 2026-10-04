//! Source rules of the Phase 2 crates (DESIGN.md P2.2):
//!
//! - **`&mut Game` stays in one file.** In `crates/citar-bot/src/**` the type `&mut Game` (with or
//!   without a lifetime, by any path) may appear only in `driver.rs`, whose `Turn` is the bot's
//!   one holder of it. Every other bot file reads `&Game` and acts through `Turn::act`, so the bot
//!   cannot reach the engine's public `&mut Game` functions (`relations::set_war`,
//!   `execute_deal`, ...) except through `Game::act`. The other spellings that would carry a
//!   `&mut Game` out of the driver without writing it are refused there too:
//!   - a rename, `use ...::Game as G;` or `{Game as G}` (`<Game as Trait>` is a qualified path,
//!     not a rename);
//!   - a type alias or associated type that is `Game`, `type G = Game;` or
//!     `type Target = Game;` (behind `&` it is a shared reference, and fine);
//!   - an impl for `Game`, `impl Local for Game { fn f(&mut self) }`, whose `self` is one;
//!   - a bound that lends one, `DerefMut<Target = Game>`, `AsMut<Game>`, `BorrowMut<Game>`.
//! - **No hand-written `unsafe` in citar-py.** PyO3's macros expand to the unsafe code the
//!   bindings need; any `unsafe` written in the crate's own sources is refused, whatever lint
//!   allowance the crate carries.
//!
//! Comments and strings are not code: the tokenizer drops the first and keeps the second as
//! literals, so a doc comment that says `&mut Game` is no finding.

use super::Finding;
use super::source::SourceTree;
use crate::lexer::{Tok, Token};

/// The bot's sources, relative to the workspace root.
pub const BOT_SRC: &str = "crates/citar-bot/src";

/// The one bot file that may hold `&mut Game`, relative to [`BOT_SRC`].
pub const BOT_DRIVER: &str = "driver.rs";

/// The bindings' sources, relative to the workspace root.
pub const PY_SRC: &str = "crates/citar-py/src";

/// Whether the tokens from `i` spell `&mut Game`: `&`, an optional lifetime, `mut`, then a path
/// whose last segment is `Game`.
fn mut_game_at(tokens: &[Token], i: usize) -> bool {
    if !tokens[i].is_punct('&') {
        return false;
    }
    let mut j = i + 1;
    if tokens.get(j).is_some_and(|t| t.tok == Tok::Other) {
        j += 1;
    }
    if !tokens.get(j).is_some_and(|t| t.is_ident("mut")) {
        return false;
    }
    j += 1;
    if tokens.get(j).is_some_and(Token::is_path_sep) {
        j += 1;
    }
    let mut last = None;
    while let Some(Tok::Ident(name)) = tokens.get(j).map(|t| &t.tok) {
        last = Some(name.as_str());
        if tokens.get(j + 1).is_some_and(Token::is_path_sep) {
            j += 2;
        } else {
            break;
        }
    }
    last == Some("Game")
}

/// Where the path whose last segment is at `i` starts: back over `segment ::` pairs and a
/// leading `::`.
fn path_start(tokens: &[Token], i: usize) -> usize {
    let mut s = i;
    while s >= 2 && tokens[s - 1].is_path_sep() && matches!(tokens[s - 2].tok, Tok::Ident(_)) {
        s -= 2;
    }
    if s >= 1 && tokens[s - 1].is_path_sep() {
        s -= 1;
    }
    s
}

/// The right-hand sides of the `type` items of a file (aliases, and associated types in impls):
/// the token ranges between `type Name<...> =` and its `;`. `r#type`, which the tokenizer reads
/// as `type`, is never followed by a name and `=`, so a binding named so is no item.
fn alias_bodies(tokens: &[Token]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    for k in 0..tokens.len() {
        if !tokens[k].is_ident("type")
            || !matches!(tokens.get(k + 1).map(|t| &t.tok), Some(Tok::Ident(_)))
        {
            continue;
        }
        let mut j = k + 2;
        let mut depth = 0usize;
        while let Some(t) = tokens.get(j) {
            if t.is_punct('<') {
                depth += 1;
            } else if t.is_punct('>') {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && (t.is_punct('=') || t.is_punct(';') || t.is_punct('{')) {
                break;
            }
            j += 1;
        }
        if !tokens.get(j).is_some_and(|t| t.is_punct('=')) {
            continue;
        }
        let end = (j + 1..tokens.len()).find(|&e| tokens[e].is_punct(';')).unwrap_or(tokens.len());
        out.push(j + 1..end);
    }
    out
}

/// Whether the path starting at `start` stands behind a reference: `&`, `&'a`, `&mut` or
/// `&'a mut` (the last two are `&mut Game`, found as such).
fn behind_a_reference(tokens: &[Token], start: usize) -> bool {
    let mut p = start;
    if p >= 1 && tokens[p - 1].is_ident("mut") {
        p -= 1;
    }
    if p >= 1 && tokens[p - 1].tok == Tok::Other {
        p -= 1;
    }
    p >= 1 && tokens[p - 1].is_punct('&')
}

/// Whether the `for` at `f` is an impl header's: an `impl` before it in the same item, with no
/// `;`, `{` or `}` between (a `for` loop has one of them, or nothing, before it).
fn impl_for(tokens: &[Token], f: usize) -> bool {
    tokens[..f]
        .iter()
        .rev()
        .take_while(|t| !(t.is_punct(';') || t.is_punct('{') || t.is_punct('}')))
        .any(|t| t.is_ident("impl"))
}

/// What the `Game` at `i` (the last segment of a path) does that would carry a `&mut Game` out
/// of the driver without writing it, if anything.
fn game_spelling_at(
    tokens: &[Token],
    i: usize,
    aliases: &[std::ops::Range<usize>],
) -> Option<&'static str> {
    if !tokens[i].is_ident("Game") || tokens.get(i + 1).is_some_and(Token::is_path_sep) {
        return None;
    }
    let start = path_start(tokens, i);
    let prev = start.checked_sub(1).map(|p| &tokens[p]);
    let before_prev = start.checked_sub(2).map(|p| &tokens[p]);
    let next = tokens.get(i + 1);
    if next.is_some_and(|t| t.is_ident("as")) && !prev.is_some_and(|t| t.is_punct('<')) {
        return Some("a rename of `Game`");
    }
    if prev.is_some_and(|t| t.is_ident("for"))
        && next.is_some_and(|t| t.is_punct('{') || t.is_ident("where"))
        && impl_for(tokens, start - 1)
    {
        return Some("an impl for `Game` (its `&mut self` is a `&mut Game`)");
    }
    if aliases.iter().any(|r| r.contains(&i)) && !behind_a_reference(tokens, start) {
        return Some("a type alias of `Game`");
    }
    let target =
        prev.is_some_and(|t| t.is_punct('=')) && before_prev.is_some_and(|t| t.is_ident("Target"));
    let lent = prev.is_some_and(|t| t.is_punct('<'))
        && before_prev.is_some_and(|t| t.is_ident("AsMut") || t.is_ident("BorrowMut"));
    if target || lent {
        return Some("a bound that lends a `&mut Game`");
    }
    None
}

/// `&mut Game`, and the other spellings of one, outside `driver.rs` in the bot's sources.
pub fn check_bot(tree: &SourceTree) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in tree.files.iter().filter(|f| f.rel != BOT_DRIVER) {
        let aliases = alias_bodies(&file.tokens);
        for i in 0..file.tokens.len() {
            let what = if mut_game_at(&file.tokens, i) {
                "`&mut Game`"
            } else if let Some(what) = game_spelling_at(&file.tokens, i, &aliases) {
                what
            } else {
                continue;
            };
            out.push(Finding::new(
                "bot",
                format!(
                    "{}: {what} outside {BOT_SRC}/{BOT_DRIVER}: the bot reads `&Game` and acts \
                     through `Turn::act`, the one holder of `&mut Game` (DESIGN.md P2.2)",
                    file.at(file.tokens[i].line)
                ),
            ));
        }
    }
    out
}

/// Hand-written `unsafe` in the bindings' sources.
pub fn check_py(tree: &SourceTree) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in &tree.files {
        for t in file.tokens.iter().filter(|t| t.is_ident("unsafe")) {
            out.push(Finding::new(
                "unsafe",
                format!(
                    "{}: hand-written `unsafe` in citar-py: the bindings use PyO3's safe API only \
                     (DESIGN.md P2.2)",
                    file.at(t.line)
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::source::SourceFile;
    use std::path::Path;

    fn bot_file(rel: &str, text: &str) -> SourceFile {
        SourceFile::under(BOT_SRC, rel, text)
    }

    fn bot(files: &[(&str, &str)]) -> Vec<Finding> {
        check_bot(&SourceTree { files: files.iter().map(|(r, t)| bot_file(r, t)).collect() })
    }

    #[test]
    fn every_spelling_of_mut_game_outside_the_driver_is_found() {
        for text in [
            "fn f(g: &mut Game) {}",
            "fn f<'g>(g: &'g mut Game) {}",
            "fn f(g: &mut citar_engine::game::Game) {}",
            "fn f(g: & mut Game) {}",
            "struct S<'a> { g: &'a mut ::citar_engine::game::Game }",
            "type G<'a> = &'a mut Game;",
            // A rename, an alias, an impl and a bound each carry one without writing it.
            "use citar_engine::game::Game as G;\nfn f(g: &mut G) {}",
            "use citar_engine::game::{self, Game as G};",
            "type G = Game;",
            "pub(crate) type G = ::citar_engine::game::Game;",
            "impl Deref for W { type Target = Game; fn deref(&self) -> &Game { self.0 } }",
            "impl Local for Game { fn f(&mut self) { relations::set_war(self) } }",
            "impl<'a> Local for citar_engine::game::Game where Self: Sized {}",
            "fn f<T: DerefMut<Target = Game>>(t: T) {}",
            "fn f<T>(t: T) where T: core::ops::DerefMut<Target = citar_engine::game::Game> {}",
            "fn f<T: BorrowMut<Game>>(t: T) {}",
            "fn f(t: impl AsMut<citar_engine::game::Game>) {}",
        ] {
            let found = bot(&[("basic1/units/attack.rs", text)]);
            assert_eq!(found.len(), 1, "{text}: {found:?}");
        }
        for text in [
            "fn f(g: &Game) {}",
            "fn f(g: &mut GameView) {}",
            "fn f(g: &mut Turn<'_>) { let x = &mut self.game; }",
            "// a comment: &mut Game\nfn f() {}",
            "const S: &str = \"&mut Game\";",
            "fn f(g: &mut game::Game2) {}",
            // Reading, naming and qualifying `Game` is fine.
            "use citar_engine::game::{Game, GameView as View};",
            "type View<'a> = &'a Game;\ntype Pair<'a> = (&'a citar_engine::game::Game, u8);",
            "fn f(g: &Game) -> Game { <Game as Clone>::clone(g) }",
            "fn f<T: AsRef<Game>>(t: T) {}\nfn g(gs: &[Game]) { for g in gs {} }",
            "impl Local for GameView {}\nimpl Deref for W { type Target = u8; }",
            "let r#type = SetupStep::Game;\nmatches!(s, SetupStep::Game(_));",
            "fn f() { for Game { turn, .. } in games() {} }",
        ] {
            assert_eq!(bot(&[("basic1/units/attack.rs", text)]), [], "{text}");
        }
        for text in ["type G = Game;", "impl Local for Game {}", "use x::Game as G;"] {
            assert_eq!(bot(&[("driver.rs", text)]), [], "the driver may: {text}");
        }
        assert_eq!(bot(&[("driver.rs", "fn f(g: &mut Game) {}")]), []);
        assert_eq!(bot(&[("basic1/driver.rs", "fn f(g: &mut Game) {}")]).len(), 1);
    }

    /// Gate 2 of package 2-00a: the bot as it is passes; a `&mut Game` planted in another of its
    /// files fails, naming the file and the line.
    #[test]
    fn the_bot_passes_and_a_planted_mut_game_fails_naming_the_file() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut tree = SourceTree::load_under(BOT_SRC, &root.join(BOT_SRC))?;
        assert!(tree.files.iter().any(|f| f.rel == BOT_DRIVER), "the bot has its driver");
        assert!(tree.files.iter().any(|f| f.rel == "idle.rs"), "and other files");
        assert_eq!(check_bot(&tree), []);
        tree.files.push(bot_file("idle.rs", "\n\npub fn sneaky(g: &mut Game) {}\n"));
        let found = check_bot(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.starts_with("crates/citar-bot/src/idle.rs:3: `&mut Game`"));
        tree.files.pop();
        let renamed = "use citar_engine::game::Game as G;\nfn sneaky(g: &mut G) {}\n";
        tree.files.push(bot_file("basic1/war.rs", renamed));
        let found = check_bot(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .message
                .starts_with("crates/citar-bot/src/basic1/war.rs:1: a rename of `Game`"),
            "{found:?}"
        );
        Ok(())
    }

    #[test]
    fn the_bindings_pass_and_hand_written_unsafe_fails() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut tree = SourceTree::load_under(PY_SRC, &root.join(PY_SRC))?;
        assert!(!tree.files.is_empty());
        assert_eq!(check_py(&tree), []);
        let allowed = "#![allow(unsafe_code)]\n// unsafe in a comment\nconst S: &str = \"unsafe\";";
        tree.files.push(SourceFile::under(PY_SRC, "allow.rs", allowed));
        assert_eq!(check_py(&tree), []);
        tree.files.push(SourceFile::under(PY_SRC, "raw.rs", "fn f() { unsafe { g() } }"));
        let found = check_py(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.starts_with("crates/citar-py/src/raw.rs:1:"), "{found:?}");
        Ok(())
    }
}
