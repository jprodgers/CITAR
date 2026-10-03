//! The wheel's build: `pyproject.toml`'s `[tool.maturin]` table (DESIGN.md P2.6.7).
//!
//! maturin builds `citar-py` with the features that table lists, so it decides what every wheel
//! and every `pip install` carries. No wheel has the engine's test operations (`test-ops`) or the
//! Python-state converter (`legacy`): the list may not turn either on, whether by citar-py's own
//! feature, through a feature of citar-py's that reaches one, or as `citar-engine/<feature>`, and
//! `all-features` may not be set. The Python suite's builds add `--features test-ops` on the
//! command line (CI's build-ext job, `cargo xtask develop`), which this file never sees.
//!
//! The features are citar-py's only while the table builds citar-py, so its `manifest-path` must
//! be the bindings' manifest. The wheel's version is Cargo's, which the version check holds equal
//! to `__version__`: `[project]` declares `version` dynamic and never sets one of its own.
//!
//! The crate side (citar-py never enables `legacy`, and its `test-ops` only forwards) is
//! `features`'s.

use std::collections::BTreeSet;
use std::path::Path;

use super::Finding;
use super::features::{self, BINDINGS};
use super::metadata::{Metadata, Package};

const CHECK: &str = "pyproject";
const FILE: &str = "pyproject.toml";
const ENGINE: &str = "citar-engine";

/// The manifest `[tool.maturin] manifest-path` must name.
pub const BINDINGS_MANIFEST: &str = "crates/citar-py/Cargo.toml";

pub fn check(root: &Path, meta: &Metadata) -> Result<Vec<Finding>, String> {
    let path = root.join(FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    check_text(&text, meta)
}

/// The findings for a `pyproject.toml` with this text, against the workspace `meta`.
pub fn check_text(text: &str, meta: &Metadata) -> Result<Vec<Finding>, String> {
    let doc: toml::Table = text.parse().map_err(|e| format!("{FILE}: {e}"))?;
    let mut out = Vec::new();
    let say = |out: &mut Vec<Finding>, message: String| {
        out.push(Finding::new(CHECK, format!("{FILE}: {message}")));
    };

    match doc.get("project").and_then(toml::Value::as_table) {
        None => say(&mut out, "has no [project] table".to_owned()),
        Some(project) => {
            if project.contains_key("version") {
                say(
                    &mut out,
                    "[project] sets `version`, but the wheel's version is Cargo's \
                     ([workspace.package] version, held equal to __version__): declare it in \
                     `dynamic` instead"
                        .to_owned(),
                );
            }
            let dynamic = project.get("dynamic").and_then(toml::Value::as_array);
            if !dynamic.is_some_and(|d| d.iter().any(|v| v.as_str() == Some("version"))) {
                say(
                    &mut out,
                    "[project] `dynamic` does not list \"version\", which maturin takes from \
                     Cargo"
                        .to_owned(),
                );
            }
        }
    }

    let Some(maturin) =
        doc.get("tool").and_then(|t| t.get("maturin")).and_then(toml::Value::as_table)
    else {
        say(&mut out, "has no [tool.maturin] table, which builds the extension".to_owned());
        return Ok(out);
    };
    let manifest = maturin.get("manifest-path").and_then(toml::Value::as_str);
    if manifest.map(|m| m.replace('\\', "/")).as_deref() != Some(BINDINGS_MANIFEST) {
        say(
            &mut out,
            format!(
                "[tool.maturin] manifest-path is {}, not {BINDINGS_MANIFEST}: the wheel builds \
                 {BINDINGS}",
                manifest.map_or_else(|| "unset".to_owned(), |m| format!("{m:?}"))
            ),
        );
    }
    if maturin.get("all-features").and_then(toml::Value::as_bool) == Some(true) {
        say(
            &mut out,
            format!(
                "[tool.maturin] sets all-features: it turns on {BINDINGS}'s `test-ops`, the \
                 engine's test operations, which no wheel carries"
            ),
        );
    }
    let listed = match maturin.get("features") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .map(|a| a.iter().map(|f| f.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>())
            .and_then(|f| f)
            .ok_or_else(|| format!("{FILE}: [tool.maturin] features is not a list of strings"))?,
    };
    let forbidden = Forbidden::new(meta);
    for f in &listed {
        if let Some(why) = forbidden.reached_by(f) {
            say(
                &mut out,
                format!(
                    "[tool.maturin] features lists `{f}`: it turns on {why}, which no wheel \
                     carries (DESIGN.md P2.6.7); the Python suite's builds pass --features \
                     test-ops themselves"
                ),
            );
        }
    }
    Ok(out)
}

/// What a wheel's feature may not reach: citar-py's own `test-ops`, and the engine's features
/// that are test-only or turn on its test operations.
struct Forbidden<'m> {
    bindings: Option<&'m Package>,
    /// The name citar-py's feature table uses for the engine (its rename, if it has one).
    engine_key: String,
    /// The engine's features that reach `legacy` or `test-ops`.
    engine: BTreeSet<String>,
}

impl<'m> Forbidden<'m> {
    fn new(meta: &'m Metadata) -> Self {
        let engine = meta.package(ENGINE);
        let mut set = features::closure(engine, features::TEST_ONLY);
        set.extend(features::closure(engine, &[features::TEST_OPS]));
        let bindings = meta.package(BINDINGS);
        let engine_key = bindings
            .and_then(|b| b.dependencies.iter().find(|d| d.name == ENGINE))
            .map_or(ENGINE, |d| d.rename.as_deref().unwrap_or(&d.name))
            .to_owned();
        Forbidden { bindings, engine_key, engine: set }
    }

    /// What the feature value `f` (citar-py's `x`, or `dep/x`, `dep?/x`) turns on that a wheel
    /// may not have, if anything, followed through citar-py's feature table.
    fn reached_by(&self, f: &str) -> Option<String> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![f.to_owned()];
        while let Some(value) = todo.pop() {
            if !seen.insert(value.clone()) {
                continue;
            }
            if let Some((dep, feature)) = value.split_once('/') {
                let dep = dep.strip_suffix('?').unwrap_or(dep);
                if (dep == self.engine_key || dep == ENGINE) && self.engine.contains(feature) {
                    return Some(format!("{ENGINE}'s `{feature}`"));
                }
                // cargo's `--features citar-py/x` names the package's own feature.
                if dep == BINDINGS {
                    todo.push(feature.to_owned());
                }
                continue;
            }
            if value == features::TEST_OPS {
                return Some(format!("{BINDINGS}'s `{value}` (the engine's test operations)"));
            }
            if let Some(implies) = self.bindings.and_then(|b| b.features.get(&value)) {
                todo.extend(implies.iter().cloned());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace as `cargo metadata` describes it, trimmed: the engine's real feature table,
    /// and citar-py with its real one plus `extra` (a feature table's entries).
    fn workspace(extra: &str) -> Metadata {
        let engine = r#"{"name": "citar-engine", "version": "0.1.5", "id": "engine",
            "dependencies": [], "targets": [],
            "features": {"default": ["embedded-ruleset"], "embedded-ruleset": [], "legacy": [],
                         "test-ops": [], "checks": [], "stats": []}}"#;
        let sep = if extra.is_empty() { "" } else { ", " };
        let py = format!(
            r#"{{"name": "citar-py", "version": "0.1.5", "id": "py",
                "dependencies": [{{"name": "citar-engine", "req": "*", "kind": null,
                                   "uses_default_features": false,
                                   "features": ["embedded-ruleset"]}}],
                "targets": [],
                "features": {{"test-ops": ["citar-engine/test-ops"]{sep}{extra}}}}}"#
        );
        let json = format!(
            r#"{{"packages": [{engine}, {py}], "workspace_members": ["engine", "py"],
                 "resolve": null}}"#
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    /// A pyproject with this `[tool.maturin]` body under a dynamic version.
    fn pyproject(maturin: &str) -> String {
        format!(
            "[project]\nname = \"citar\"\ndynamic = [\"version\"]\n\n[tool.maturin]\n\
             manifest-path = \"crates/citar-py/Cargo.toml\"\n{maturin}\n"
        )
    }

    fn found(text: &str, meta: &Metadata) -> Vec<Finding> {
        check_text(text, meta).expect("the text parses")
    }

    #[test]
    fn the_real_pyproject_passes() {
        let text = include_str!("../../../pyproject.toml");
        assert_eq!(found(text, &workspace("")), []);
    }

    /// Gate 5 of package 2-06b: a pyproject listing test-ops fails.
    #[test]
    fn listing_test_ops_fails() {
        let meta = workspace("");
        let f = found(&pyproject(r#"features = ["test-ops"]"#), &meta);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(
            f[0].message.starts_with("pyproject.toml: [tool.maturin] features lists `test-ops`: ")
        );
        assert!(f[0].message.contains("citar-py's `test-ops`"), "{f:?}");
        assert_eq!(f[0].to_string().split(']').next(), Some("[pyproject"));
        // The engine's, named through the dependency, optional or not.
        for listed in [
            "citar-engine/test-ops",
            "citar-engine?/test-ops",
            "citar-engine/legacy",
            "citar-py/test-ops",
        ] {
            let f = found(&pyproject(&format!("features = [{listed:?}]")), &meta);
            assert_eq!(f.len(), 1, "{listed}: {f:?}");
            let (crate_name, feature) = listed.split_once('/').expect("a dependency's feature");
            let named = format!("{}'s `{feature}`", crate_name.trim_end_matches('?'));
            assert!(f[0].message.contains(&named), "{f:?}");
        }
    }

    #[test]
    fn a_feature_that_reaches_them_fails_and_others_pass() {
        let meta =
            workspace(r#""dev": ["test-ops"], "deep": ["dev"], "fast": ["citar-engine/stats"]"#);
        for listed in ["dev", "deep"] {
            let f = found(&pyproject(&format!("features = [{listed:?}]")), &meta);
            assert_eq!(f.len(), 1, "{listed}: {f:?}");
        }
        assert_eq!(found(&pyproject(r#"features = ["fast", "citar-engine/stats"]"#), &meta), []);
        assert_eq!(found(&pyproject("features = []"), &meta), []);
        assert_eq!(found(&pyproject(""), &meta), []);
        // A cycle in the table ends.
        let cyclic = workspace(r#""a": ["b"], "b": ["a"]"#);
        assert_eq!(found(&pyproject(r#"features = ["a"]"#), &cyclic), []);
    }

    #[test]
    fn all_features_fails() {
        let f = found(&pyproject("all-features = true"), &workspace(""));
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("all-features"), "{f:?}");
        assert_eq!(found(&pyproject("all-features = false"), &workspace("")), []);
    }

    #[test]
    fn the_table_must_build_the_bindings() {
        let meta = workspace("");
        let other = "[project]\ndynamic = [\"version\"]\n[tool.maturin]\n\
                     manifest-path = \"crates/citar-sim/Cargo.toml\"\n";
        let f = found(other, &meta);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("manifest-path is \"crates/citar-sim/Cargo.toml\""));
        let none = "[project]\ndynamic = [\"version\"]\n";
        let f = found(none, &meta);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("no [tool.maturin]"), "{f:?}");
        // A Windows separator is the same path.
        let back = "[project]\ndynamic = [\"version\"]\n[tool.maturin]\n\
                    manifest-path = 'crates\\citar-py\\Cargo.toml'\n";
        assert_eq!(found(back, &meta), []);
    }

    #[test]
    fn the_version_is_cargos() {
        let meta = workspace("");
        let fixed = pyproject("").replace("dynamic = [\"version\"]", "version = \"0.1.5\"");
        let f = found(&fixed, &meta);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f[0].message.contains("sets `version`"), "{f:?}");
        assert!(f[1].message.contains("does not list \"version\""), "{f:?}");
    }

    #[test]
    fn a_broken_file_or_features_value_cannot_be_checked() {
        let meta = workspace("");
        assert!(check_text("[project", &meta).is_err());
        assert!(check_text(&pyproject(r#"features = "test-ops""#), &meta).is_err());
        assert!(check_text(&pyproject("features = [1]"), &meta).is_err());
    }
}
