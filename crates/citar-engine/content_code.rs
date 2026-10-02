//! A crate's content code (DESIGN.md P2.2.1): 16 hex digits that change exactly when the code
//! that decides a game can.
//!
//! The code is blake3 over:
//! - a domain tag naming the crate (`citar-engine-code-v1`, `citar-bot-code-v1`);
//! - the package version;
//! - every input file, in the order of its name (a label and its path below its folder, with `/`
//!   separators), each with CRLF line endings turned to LF, so a Windows checkout with
//!   `core.autocrlf` gives the same code as a Linux one;
//! - the `name version` of every package of `Cargo.lock` the crate's normal dependencies reach,
//!   sorted: a bump of serde_json, or of the ryu that formats its floats, can change a save.
//!
//! Nothing else is read, so a commit that touches only docs, tests or another crate leaves the
//! code as it is, and a `git describe` label (which moves on every commit) plays no part.
//!
//! The same file is `crates/citar-engine/content_code.rs` and `crates/citar-bot/content_code.rs`
//! (citar-bot's tests hold the two equal): each build script includes its own copy, so either
//! crate builds from its own folder, and each crate's tests include it to check it.

// A build-time helper: it reads the crate's files and prints nothing. It runs in build.rs, never
// inside a game, so the engine's rules for game code do not apply to it.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "build-time file hashing: it reads the sources, which game code never does"
)]
#![allow(dead_code, reason = "each crate's build script uses only some of the helpers")]

use std::path::{Path, PathBuf};

/// One input of a content code.
pub enum Input {
    /// Every file below `dir`, at any depth, whose extension is `ext`, each named
    /// `<label><path below dir>`. A folder that does not exist gives no files.
    Tree { label: &'static str, dir: PathBuf, ext: &'static str },
    /// One file, named `label`. A missing file is an error.
    File { label: &'static str, path: PathBuf },
}

/// The bytes with every CRLF turned to LF; a lone CR stays.
#[must_use]
pub fn normalise(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Every file below `dir` whose extension is `ext`: (path below `dir` with `/`, full path).
fn walk(
    dir: &Path,
    ext: &str,
    below: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("listing {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("listing {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if below.is_empty() { name } else { format!("{below}/{name}") };
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            walk(&path, ext, &rel, out)?;
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push((rel, path));
        }
    }
    Ok(())
}

/// The input files by name, sorted: (name, full path).
///
/// # Errors
/// If a folder cannot be listed, or a named file is missing.
pub fn input_files(inputs: &[Input]) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    for input in inputs {
        match input {
            Input::Tree { label, dir, ext } => {
                if !dir.is_dir() {
                    continue;
                }
                let mut found = Vec::new();
                walk(dir, ext, "", &mut found)?;
                out.extend(found.into_iter().map(|(rel, p)| (format!("{label}{rel}"), p)));
            }
            Input::File { label, path } => {
                if !path.is_file() {
                    return Err(format!("{} is missing", path.display()));
                }
                out.push(((*label).to_owned(), path.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The crates a manifest's `[dependencies]` table names (the package's name where the entry
/// renames it): its normal dependencies. Build, dev and target tables are not normal ones.
#[must_use]
pub fn normal_dependencies(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]";
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let name = key.split('.').next().unwrap_or(key).trim().trim_matches('"');
        // `x = { package = "y", ... }`: the dependency is y.
        let renamed = value.split("package").nth(1).and_then(|rest| {
            let rest = rest.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_owned())
        });
        out.push(renamed.unwrap_or_else(|| name.to_owned()));
    }
    out.sort();
    out.dedup();
    out
}

/// One package of `Cargo.lock`.
struct Locked {
    name: String,
    version: String,
    /// Its dependencies as the lock file writes them: `name`, or `name version` when the name is
    /// ambiguous, possibly followed by a source in brackets.
    deps: Vec<(String, Option<String>)>,
}

/// The packages of a `Cargo.lock`.
fn parse_lock(lock: &str) -> Vec<Locked> {
    let mut out: Vec<Locked> = Vec::new();
    let mut in_deps = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            out.push(Locked { name: String::new(), version: String::new(), deps: Vec::new() });
            in_deps = false;
            continue;
        }
        let Some(pkg) = out.last_mut() else { continue };
        if in_deps {
            if line.starts_with(']') {
                in_deps = false;
            } else if let Some(entry) = line.trim_end_matches(',').strip_prefix('"') {
                let entry = entry.trim_end_matches('"');
                let mut words = entry.split(' ');
                let name = words.next().unwrap_or_default().to_owned();
                pkg.deps.push((name, words.next().map(str::to_owned)));
            }
            continue;
        }
        let quoted = |key: &str| {
            let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
            Some(rest.trim_matches('"').to_owned())
        };
        if let Some(name) = quoted("name") {
            pkg.name = name;
        } else if let Some(version) = quoted("version") {
            pkg.version = version;
        } else if line.starts_with("dependencies") && line.ends_with('[') {
            in_deps = true;
        }
    }
    out
}

/// The `name version` of every package of `lock` that the dependencies named `direct` reach,
/// sorted. A direct name the lock holds in several versions counts each of them.
#[must_use]
pub fn locked_versions(direct: &[String], lock: &str) -> Vec<String> {
    let packages = parse_lock(lock);
    let mut seen = vec![false; packages.len()];
    let mut queue: Vec<usize> = packages
        .iter()
        .enumerate()
        .filter(|(_, p)| direct.iter().any(|d| *d == p.name))
        .map(|(i, _)| i)
        .collect();
    while let Some(i) = queue.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        for (name, version) in &packages[i].deps {
            for (j, p) in packages.iter().enumerate() {
                if p.name == *name && version.as_ref().is_none_or(|v| *v == p.version) && !seen[j] {
                    queue.push(j);
                }
            }
        }
    }
    let mut out: Vec<String> = packages
        .iter()
        .zip(&seen)
        .filter(|(_, s)| **s)
        .map(|(p, _)| format!("{} {}", p.name, p.version))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn put(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// The content code, 16 lower-case hex digits: the first 8 bytes of blake3 over `tag`,
/// `version`, every input file (name and LF-normalised bytes, in name order) and the locked
/// versions.
///
/// # Errors
/// If an input cannot be read.
pub fn content_code(
    tag: &str,
    version: &str,
    inputs: &[Input],
    locked: &[String],
) -> Result<String, String> {
    let mut h = blake3::Hasher::new();
    put(&mut h, tag.as_bytes());
    put(&mut h, version.as_bytes());
    let files = input_files(inputs)?;
    h.update(&(files.len() as u64).to_le_bytes());
    for (name, path) in &files {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        put(&mut h, name.as_bytes());
        put(&mut h, &normalise(&bytes));
    }
    h.update(&(locked.len() as u64).to_le_bytes());
    for l in locked {
        put(&mut h, l.as_bytes());
    }
    let digest = h.finalize();
    Ok(digest.as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect())
}

/// The locked versions a crate's code covers: its manifest's normal dependencies, followed
/// through the workspace's `Cargo.lock` at `lock`. Without a lock file (a crate built from a
/// package that carries none) there are none, and the code says so by covering none.
///
/// # Errors
/// If the manifest cannot be read.
pub fn crate_locked(manifest: &Path, lock: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("reading {}: {e}", manifest.display()))?;
    let direct = normal_dependencies(&text);
    Ok(std::fs::read_to_string(lock).map_or_else(|_| Vec::new(), |l| locked_versions(&direct, &l)))
}

/// The engine's code's domain tag.
pub const ENGINE_TAG: &str = "citar-engine-code-v1";

/// The engine's inputs, from its manifest's folder: its sources and the ruleset files it embeds
/// (`rules::source::embedded`, which reads `citar/data/`).
#[must_use]
pub fn engine_inputs(dir: &Path) -> Vec<Input> {
    let data = dir.join("../../citar/data");
    vec![
        Input::Tree { label: "src/", dir: dir.join("src"), ext: "rs" },
        Input::Tree { label: "data/ruleset/", dir: data.join("ruleset"), ext: "json" },
        Input::Tree { label: "data/custom/", dir: data.join("custom"), ext: "json" },
        Input::File { label: "data/game.json", path: data.join("game.json") },
    ]
}

/// The bot's code's domain tag.
pub const BOT_TAG: &str = "citar-bot-code-v1";

/// The bot's inputs, from its manifest's folder: its sources and its versions' parameter
/// schemas.
#[must_use]
pub fn bot_inputs(dir: &Path) -> Vec<Input> {
    vec![
        Input::Tree { label: "src/", dir: dir.join("src"), ext: "rs" },
        Input::Tree { label: "params/", dir: dir.join("params"), ext: "json" },
    ]
}

/// The code of the crate whose manifest is in `dir`, at `version`, over `inputs`: what its build
/// script computes. The workspace's `Cargo.lock` is two folders up.
///
/// # Errors
/// If an input or the manifest cannot be read.
pub fn crate_code(
    tag: &str,
    version: &str,
    dir: &Path,
    inputs: &[Input],
) -> Result<String, String> {
    let locked = crate_locked(&dir.join("Cargo.toml"), &dir.join("../../Cargo.lock"))?;
    content_code(tag, version, inputs, &locked)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder for one test, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("citar-content-code-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch folder");
            Self(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("folders");
            std::fs::write(path, text).expect("written");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = "[package]\nname = \"x\"\n\n[dependencies]\nserde.workspace = true\n\
                            other = { package = \"renamed\", version = \"1\" }\n# a comment\n\n\
                            [dev-dependencies]\nproptest.workspace = true\n\n\
                            [build-dependencies]\nblake3 = \"1\"\n";

    const LOCK: &str = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.1\"\n\
                        source = \"registry\"\ndependencies = [\n \"serde_derive\",\n]\n\n\
                        [[package]]\nname = \"serde_derive\"\nversion = \"1.0.1\"\n\
                        dependencies = [\n \"syn 2.0.0\",\n]\n\n\
                        [[package]]\nname = \"syn\"\nversion = \"1.0.0\"\n\n\
                        [[package]]\nname = \"syn\"\nversion = \"2.0.0\"\n\n\
                        [[package]]\nname = \"renamed\"\nversion = \"0.3.0\"\n\n\
                        [[package]]\nname = \"proptest\"\nversion = \"1.11.0\"\n\n\
                        [[package]]\nname = \"blake3\"\nversion = \"1.8.7\"\n";

    #[test]
    fn normal_dependencies_and_what_they_reach_in_the_lock() {
        assert_eq!(normal_dependencies(MANIFEST), ["renamed", "serde"]);
        let locked = locked_versions(&normal_dependencies(MANIFEST), LOCK);
        // Neither dev nor build dependencies, and only the syn the lock names.
        assert_eq!(locked, ["renamed 0.3.0", "serde 1.0.1", "serde_derive 1.0.1", "syn 2.0.0"]);
        assert_eq!(normal_dependencies(&MANIFEST.replace('\n', "\r\n")), ["renamed", "serde"]);
        assert_eq!(
            locked_versions(&normal_dependencies(MANIFEST), &LOCK.replace('\n', "\r\n")),
            locked
        );
    }

    fn code_of(root: &Path) -> String {
        let inputs = [
            Input::Tree { label: "src/", dir: root.join("src"), ext: "rs" },
            Input::Tree { label: "data/", dir: root.join("data"), ext: "json" },
            Input::File { label: "game.json", path: root.join("game.json") },
        ];
        let locked =
            crate_locked(&root.join("Cargo.toml"), &root.join("Cargo.lock")).expect("the manifest");
        content_code("test-code-v1", "0.1.5", &inputs, &locked).expect("a code")
    }

    /// A small crate, in LF or CRLF.
    fn fill(s: &Scratch, eol: &str) {
        let t = |text: &str| text.replace('\n', eol);
        s.write("src/lib.rs", &t("//! A crate.\npub mod a;\n"));
        s.write("src/a/mod.rs", &t("pub fn f() -> u32 {\n    1\n}\n"));
        s.write("src/notes.txt", &t("not an input\n"));
        s.write("data/ruleset/techs.json", &t("{\n  \"x\": 1\n}\n"));
        s.write("game.json", &t("{}\n"));
        s.write("Cargo.toml", &t(MANIFEST));
        s.write("Cargo.lock", &t(LOCK));
        s.write("docs/guide.md", &t("# A guide\n"));
    }

    /// Gate 4 of package 2-00a: an LF and a CRLF copy give the same code; editing a source file
    /// changes it, editing a file under docs/ (or any file that is no input) does not.
    #[test]
    fn line_endings_do_not_change_the_code_and_only_inputs_do() {
        let lf = Scratch::new("lf");
        fill(&lf, "\n");
        let crlf = Scratch::new("crlf");
        fill(&crlf, "\r\n");
        let code = code_of(&lf.0);
        assert_eq!(code.len(), 16);
        assert!(code.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(code_of(&crlf.0), code, "a CRLF checkout gives the same code");

        lf.write("docs/guide.md", "# A guide, rewritten\n");
        lf.write("src/notes.txt", "still not an input\n");
        lf.write("README.md", "a new file beside the crate\n");
        assert_eq!(code_of(&lf.0), code, "files that are no input change nothing");

        lf.write("src/a/mod.rs", "pub fn f() -> u32 {\n    2\n}\n");
        let edited = code_of(&lf.0);
        assert_ne!(edited, code, "a source edit changes the code");
        lf.write("src/a/mod.rs", "pub fn f() -> u32 {\n    1\n}\n");
        assert_eq!(code_of(&lf.0), code, "and undoing it brings the code back");

        lf.write("data/ruleset/techs.json", "{\n  \"x\": 2\n}\n");
        assert_ne!(code_of(&lf.0), code, "a data edit changes the code");
        lf.write("data/ruleset/techs.json", "{\n  \"x\": 1\n}\n");
        lf.write("src/b.rs", "// a new module\n");
        assert_ne!(code_of(&lf.0), code, "a new source file changes the code");
        std::fs::remove_file(lf.0.join("src/b.rs")).expect("removed");
        lf.write("Cargo.lock", &LOCK.replace("1.0.1", "1.0.2"));
        assert_ne!(code_of(&lf.0), code, "a locked dependency bump changes the code");
        lf.write("Cargo.lock", &LOCK.replace("1.11.0", "1.12.0"));
        assert_eq!(code_of(&lf.0), code, "a dev dependency's bump does not");
    }

    /// This crate's tag, the code its build script computed, and its recipe.
    fn this_crate() -> (&'static str, &'static str, fn(&Path) -> Vec<Input>) {
        match (option_env!("CITAR_ENGINE_CODE"), option_env!("CITAR_BOT_CODE")) {
            (Some(code), _) => (ENGINE_TAG, code, engine_inputs),
            (None, Some(code)) => (BOT_TAG, code, bot_inputs),
            (None, None) => panic!("built without a content code"),
        }
    }

    /// Copies the crate's inputs, its manifest and the workspace's lock file from the crate at
    /// `real` to the same places below `ws` (the crate at `ws/crates/<name>`), with every line
    /// ending `eol`; returns the copy's crate folder.
    fn copy_crate(real: &Path, ws: &Path, recipe: fn(&Path) -> Vec<Input>, eol: &str) -> PathBuf {
        let name = real.file_name().expect("a crate folder");
        let dir = ws.join("crates").join(name);
        let copy = |from: &Path, to: &Path| {
            let text = normalise(&std::fs::read(from).expect("readable"));
            let text = String::from_utf8(text).expect("UTF-8 sources").replace('\n', eol);
            std::fs::create_dir_all(to.parent().expect("a parent")).expect("folders");
            std::fs::write(to, text).expect("written");
        };
        for (from, to) in recipe(real).iter().zip(recipe(&dir).iter()) {
            match (from, to) {
                (Input::Tree { dir: a, ext, .. }, Input::Tree { dir: b, .. }) => {
                    let mut found = Vec::new();
                    if a.is_dir() {
                        walk(a, ext, "", &mut found).expect("listed");
                    }
                    for (rel, path) in found {
                        copy(&path, &b.join(rel));
                    }
                }
                (Input::File { path: a, .. }, Input::File { path: b, .. }) => copy(a, b),
                _ => unreachable!("one recipe"),
            }
        }
        copy(&real.join("Cargo.toml"), &dir.join("Cargo.toml"));
        copy(&real.join("../../Cargo.lock"), &ws.join("Cargo.lock"));
        dir
    }

    /// Gate 4 of package 2-00a on this crate's real sources: the code its build script computed
    /// is the code of its tree, and of an LF and a CRLF copy of it; editing one source file
    /// changes it, editing a file under docs/ does not.
    #[test]
    fn the_built_code_is_the_trees_and_an_lf_and_a_crlf_copy_give_it_too() {
        let (tag, built, recipe) = this_crate();
        let real = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(crate_code(tag, version, &real, &recipe(&real)), Ok(built.to_owned()));
        let lf = Scratch::new(&format!("{tag}-lf"));
        let crlf = Scratch::new(&format!("{tag}-crlf"));
        let lf_dir = copy_crate(&real, &lf.0, recipe, "\n");
        let crlf_dir = copy_crate(&real, &crlf.0, recipe, "\r\n");
        assert_eq!(crate_code(tag, version, &lf_dir, &recipe(&lf_dir)), Ok(built.to_owned()));
        assert_eq!(crate_code(tag, version, &crlf_dir, &recipe(&crlf_dir)), Ok(built.to_owned()));
        lf.write("docs/ARCHITECTURE.md", "# Edited\n");
        assert_eq!(crate_code(tag, version, &lf_dir, &recipe(&lf_dir)), Ok(built.to_owned()));
        let lib = lf_dir.join("src/lib.rs");
        let mut text = std::fs::read_to_string(&lib).expect("lib.rs");
        text.push_str("// one more line\n");
        std::fs::write(&lib, text).expect("written");
        let edited = crate_code(tag, version, &lf_dir, &recipe(&lf_dir)).expect("a code");
        assert_ne!(edited, built, "a source edit moves the code");
    }

    #[test]
    fn a_missing_file_is_an_error_and_a_missing_folder_is_empty() {
        let s = Scratch::new("missing");
        let none = [Input::Tree { label: "params/", dir: s.0.join("params"), ext: "json" }];
        assert!(content_code("t", "1", &none, &[]).is_ok());
        let file = [Input::File { label: "game.json", path: s.0.join("game.json") }];
        assert!(content_code("t", "1", &file, &[]).is_err());
    }

    #[test]
    fn crlf_becomes_lf_and_nothing_else_moves() {
        assert_eq!(normalise(b"a\r\nb\rc\n\r\n"), b"a\nb\rc\n\n");
        assert_eq!(normalise(b""), b"");
        assert_eq!(normalise(b"\r"), b"\r");
    }
}
