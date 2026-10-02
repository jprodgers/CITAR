//! `cargo xtask gen-params`: writes `crates/citar-bot/src/params/gen.rs`, the `Params` struct of
//! bot version `basic-1`, from its schema `crates/citar-bot/params/basic-1.json` (DESIGN.md
//! P2.3.2): one field per key, an enum per choice, `NameList` for an order or a list,
//! `serde(deny_unknown_fields)` and no defaults.
//!
//! Registered by package 2-00a; the generator is package 2-01a's, which also adds the file to
//! `check::generated::FILES` so that a stale or hand-edited `gen.rs` fails `cargo xtask check`.
//! Until then the command writes nothing.

use std::path::Path;

/// The file it writes, relative to the workspace root.
pub const OUT: &str = "crates/citar-bot/src/params/gen.rs";

/// The schema it reads, relative to the workspace root.
pub const SCHEMA: &str = "crates/citar-bot/params/basic-1.json";

/// The text of `gen.rs` for the workspace at `root`.
///
/// # Errors
/// Until package 2-01a: always, saying so.
pub fn generate(root: &Path) -> Result<String, String> {
    let _ = root;
    Err(format!(
        "the generator of {OUT} from {SCHEMA} is written in package 2-01a; until then gen.rs is \
         a placeholder"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_writes_nothing_and_says_why() {
        let e = generate(Path::new(".")).expect_err("a stub");
        assert!(e.contains("2-01a"), "{e}");
    }
}
