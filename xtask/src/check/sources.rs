//! Source rules of the Phase 2 crates (DESIGN.md P2.2):
//!
//! - **`&mut Game` stays in one file.** In `crates/citar-bot/src/**` the type `&mut Game` (with or
//!   without a lifetime, by any path) may appear only in `driver.rs`, whose `Turn` is the bot's
//!   one holder of it. Every other bot file reads `&Game` and acts through `Turn::act`, so the bot
//!   cannot reach the engine's public `&mut Game` functions (`relations::set_war`,
//!   `execute_deal`, ...) except through `Game::act`.
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

/// `&mut Game` outside `driver.rs` in the bot's sources.
pub fn check_bot(tree: &SourceTree) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in tree.files.iter().filter(|f| f.rel != BOT_DRIVER) {
        for i in 0..file.tokens.len() {
            if mut_game_at(&file.tokens, i) {
                out.push(Finding::new(
                    "bot",
                    format!(
                        "{}: `&mut Game` outside {BOT_SRC}/{BOT_DRIVER}: the bot reads `&Game` and \
                         acts through `Turn::act`, the one holder of `&mut Game` (DESIGN.md P2.2)",
                        file.at(file.tokens[i].line)
                    ),
                ));
            }
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
        ] {
            assert_eq!(bot(&[("basic1/units/attack.rs", text)]), [], "{text}");
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
