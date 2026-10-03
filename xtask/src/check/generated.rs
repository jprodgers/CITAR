//! Generated files stay in step with their generators.
//!
//! Each committed generated file is listed in [`FILES`] with the function that produces it; the
//! check regenerates it in memory and fails on any difference. There are two:
//! `crates/citar-engine/src/unique/gen.rs`, from `cargo xtask gen-uniques` (package 1a-05), and
//! `crates/citar-bot/src/params/gen.rs`, from `cargo xtask gen-params` (package 2-01a, rule 4 of
//! DESIGN.md P2.2).

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
    /// The files `generate` reads, relative to the workspace root: a change to one must run the
    /// checks (`triggers`).
    pub inputs: &'static [&'static str],
}

/// Every generated file in the workspace.
pub const FILES: &[Generated] = &[
    Generated {
        path: crate::gen_uniques::OUT,
        command: "cargo xtask gen-uniques",
        generate: crate::gen_uniques::generate,
        inputs: &[crate::gen_uniques::TSV, crate::gen_uniques::TOML],
    },
    Generated {
        path: crate::gen_params::OUT,
        command: "cargo xtask gen-params",
        generate: crate::gen_params::generate,
        inputs: &[crate::gen_params::SCHEMA],
    },
];

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

    const HELLO: &[Generated] = &[Generated {
        path: "gen.rs",
        command: "cargo xtask gen-hello",
        generate: hello,
        inputs: &[],
    }];

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
    fn the_unique_types_and_the_bot_parameters_are_generated() {
        let paths: Vec<&str> = FILES.iter().map(|g| g.path).collect();
        assert_eq!(
            paths,
            ["crates/citar-engine/src/unique/gen.rs", "crates/citar-bot/src/params/gen.rs"]
        );
    }

    /// Rule 4 of DESIGN.md P2.2: the bot's parameter struct is held to its schema, so a hand
    /// edit of the committed file, or a schema changed without regenerating, is a finding.
    #[test]
    fn the_committed_files_are_fresh_and_a_hand_edit_fails() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(check(&root, FILES), [], "every generated file is up to date");
        let dir = scratch("params");
        let out = dir.join("crates/citar-bot/src/params");
        let schema = dir.join("crates/citar-bot/params");
        std::fs::create_dir_all(&out).expect("dirs");
        std::fs::create_dir_all(&schema).expect("dirs");
        std::fs::copy(root.join(crate::gen_params::SCHEMA), schema.join("basic-1.json"))
            .expect("the schema");
        let fresh = std::fs::read_to_string(root.join(crate::gen_params::OUT)).expect("gen.rs");
        let only_params = &FILES[1..];
        std::fs::write(out.join("gen.rs"), &fresh).expect("write");
        assert_eq!(check(&dir, only_params), []);
        let edited = fresh.replacen("pub tech_noise: f64,", "pub tech_noise: f32,", 1);
        assert_ne!(edited, fresh, "the edit lands");
        std::fs::write(out.join("gen.rs"), edited).expect("write");
        let found = check(&dir, only_params);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("run `cargo xtask gen-params`"), "{found:?}");
        std::fs::remove_dir_all(dir).ok();
    }
}
