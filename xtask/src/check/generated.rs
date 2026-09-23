//! Generated files stay in step with their generators.
//!
//! Each committed generated file is listed in [`FILES`] with the function that produces it; the
//! check regenerates it in memory and fails on any difference. Package 1a-05 adds
//! `crates/citar-engine/src/unique/gen.rs` (`cargo xtask gen-uniques`).

use super::Finding;
use std::path::Path;

const CHECK: &str = "generated";

/// A committed file and the function that writes its contents from the workspace root.
pub struct Generated {
    /// Relative to the workspace root.
    pub path: &'static str,
    /// The command that rewrites it, named in the finding.
    pub command: &'static str,
    pub generate: fn(&Path) -> Result<String, String>,
}

/// Every generated file in the workspace.
pub const FILES: &[Generated] = &[];

pub fn check(root: &Path, files: &[Generated]) -> Vec<Finding> {
    let mut out = Vec::new();
    for g in files {
        let stale = |why: String| {
            Finding::new(CHECK, format!("{} is out of date ({why}): run `{}`", g.path, g.command))
        };
        let expected = match (g.generate)(root) {
            Ok(text) => text,
            Err(e) => {
                out.push(Finding::new(CHECK, format!("generating {}: {e}", g.path)));
                continue;
            }
        };
        match std::fs::read_to_string(root.join(g.path)) {
            // A Windows checkout may have CRLF line endings; the contents are what matter.
            Ok(actual) if actual.replace("\r\n", "\n") == expected.replace("\r\n", "\n") => {}
            Ok(_) => out.push(stale("it differs from what the generator writes".into())),
            Err(e) => out.push(stale(e.to_string())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xtask-generated-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn hello(_: &Path) -> Result<String, String> {
        Ok("// GENERATED\nhello\n".into())
    }

    const HELLO: &[Generated] =
        &[Generated { path: "gen.rs", command: "cargo xtask gen-hello", generate: hello }];

    #[test]
    fn a_fresh_file_passes_whatever_its_line_endings() {
        let dir = scratch("fresh");
        std::fs::write(dir.join("gen.rs"), "// GENERATED\r\nhello\r\n").expect("write");
        assert_eq!(check(&dir, HELLO), []);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_stale_or_missing_file_fails() {
        let dir = scratch("stale");
        let found = check(&dir, HELLO);
        assert_eq!(found.len(), 1, "a missing file: {found:?}");
        std::fs::write(dir.join("gen.rs"), "// GENERATED\nhello, edited by hand\n").expect("write");
        let found = check(&dir, HELLO);
        assert_eq!(found.len(), 1, "an edited file: {found:?}");
        assert!(found[0].message.contains("run `cargo xtask gen-hello`"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn nothing_is_generated_yet() {
        assert!(FILES.is_empty(), "1a-05 adds unique/gen.rs; update this test then");
    }
}
