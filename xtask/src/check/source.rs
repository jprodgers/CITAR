//! A crate's source files, tokenized once for every source check: the engine's, and from package
//! 2-00a the bot's and the bindings'.

use crate::lexer::{self, Token};
use std::path::Path;

pub struct SourceFile {
    /// The path relative to `src/`, with `/` separators: `game/cities/stats.rs`.
    pub rel: String,
    pub tokens: Vec<Token>,
    /// The crate's `src/`, relative to the workspace root, which findings name.
    pub base: &'static str,
}

impl SourceFile {
    /// A file of the engine's sources, for tests.
    #[cfg(test)]
    pub fn new(rel: &str, text: &str) -> Self {
        Self::under(super::ENGINE_SRC, rel, text)
    }

    /// A file of the sources under `base` (relative to the workspace root).
    pub fn under(base: &'static str, rel: &str, text: &str) -> Self {
        SourceFile { rel: rel.to_string(), tokens: lexer::lex(text), base }
    }

    /// The module path the file defines: `lib.rs` is the root, `game/mod.rs` is `game`, and
    /// `game/cities/stats.rs` is `game::cities::stats`.
    pub fn module_path(&self) -> Vec<String> {
        let mut parts: Vec<String> = self.rel.split('/').map(str::to_string).collect();
        let file = parts.pop().unwrap_or_default();
        let stem = file.strip_suffix(".rs").unwrap_or(&file);
        if !(parts.is_empty() && stem == "lib") && stem != "mod" {
            parts.push(stem.to_string());
        }
        parts
    }

    /// The top-level module the file belongs to, or `None` for `lib.rs`.
    pub fn layer(&self) -> Option<String> {
        self.module_path().into_iter().next()
    }

    /// Where a finding points: `crates/citar-engine/src/<rel>:<line>`.
    pub fn at(&self, line: u32) -> String {
        format!("{}/{}:{line}", self.base, self.rel)
    }
}

pub struct SourceTree {
    /// Sorted by path, so reports come out in the same order everywhere.
    pub files: Vec<SourceFile>,
}

impl SourceTree {
    /// The engine's sources, from its `src/`.
    pub fn load(src: &Path) -> Result<Self, String> {
        Self::load_under(super::ENGINE_SRC, src)
    }

    /// The sources in `src`, the folder `base` names relative to the workspace root.
    pub fn load_under(base: &'static str, src: &Path) -> Result<Self, String> {
        let mut files = Vec::new();
        walk(base, src, src, &mut files)?;
        files.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(SourceTree { files })
    }
}

fn walk(
    base: &'static str,
    src: &Path,
    dir: &Path,
    out: &mut Vec<SourceFile>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("listing {}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("listing {}: {e}", dir.display()))?.path();
        if path.is_dir() {
            walk(base, src, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let rel = path
                .strip_prefix(src)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(SourceFile::under(base, &rel, &text));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths() {
        let path = |rel: &str| SourceFile::new(rel, "").module_path().join("::");
        assert_eq!(path("lib.rs"), "");
        assert_eq!(path("game/mod.rs"), "game");
        assert_eq!(path("game/core.rs"), "game::core");
        assert_eq!(path("game/cities/mod.rs"), "game::cities");
        assert_eq!(path("game/cities/stats.rs"), "game::cities::stats");
        assert_eq!(path("base.rs"), "base");
        assert_eq!(SourceFile::new("lib.rs", "").layer(), None);
        assert_eq!(SourceFile::new("game/cities/stats.rs", "").layer().as_deref(), Some("game"));
    }
}
