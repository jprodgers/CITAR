//! The engine's dependency allow-list (DESIGN.md 2.3).
//!
//! Every crate the engine links can change a game, so the list is short and deliberate: adding
//! to it means editing [`ALLOWED`] here as well as the manifest. Their transitive closure comes
//! with them. Dev-dependencies are free, since they never reach a game.

use super::Finding;
use super::metadata::Metadata;

const CHECK: &str = "deps";
const ENGINE: &str = "citar-engine";

/// The crates `citar-engine` may depend on: exactly the engine block of the root manifest.
pub const ALLOWED: &[&str] = &[
    "serde",
    "serde_json",
    "indexmap",
    "rustc-hash",
    "smallvec",
    "libm",
    "blake3",
    "thiserror",
    "base64",
    "aho-corasick",
    "bitflags",
];

/// The one `libm` release whose answers the goldens hold; a bump re-blesses them.
const LIBM_REQ: &str = "=0.2.16";

pub fn check(meta: &Metadata) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(engine) = meta.package(ENGINE) else {
        out.push(Finding::new(CHECK, format!("no package named {ENGINE} in the workspace")));
        return out;
    };

    for dep in &engine.dependencies {
        let label = match dep.kind.as_deref() {
            None => "dependency",
            Some("build") => "build-dependency",
            Some(_) => continue,
        };
        if !ALLOWED.contains(&dep.name.as_str()) {
            out.push(Finding::new(
                CHECK,
                format!(
                    "{ENGINE} has the {label} `{}`, which is not on the allow-list \
                     (xtask/src/check/deps.rs, DESIGN.md 2.3)",
                    dep.name
                ),
            ));
        }
        if dep.name == "libm" {
            if dep.req != LIBM_REQ {
                out.push(Finding::new(
                    CHECK,
                    format!("{ENGINE} must pin libm as `{LIBM_REQ}`, not `{}`", dep.req),
                ));
            }
            if dep.uses_default_features {
                out.push(Finding::new(
                    CHECK,
                    format!("{ENGINE} must take libm with default-features = false (no `arch`)"),
                ));
            }
        }
    }

    // Features unify across the workspace, so any crate turning on libm's `arch` turns it on
    // for the engine too.
    for libm in meta.packages.iter().filter(|p| p.name == "libm") {
        let features = meta.features_of(&libm.id).unwrap_or_default();
        if let Some(bad) = features.iter().find(|f| *f == "arch" || *f == "default") {
            out.push(Finding::new(
                CHECK,
                format!(
                    "libm {} is built with the `{bad}` feature; something in the workspace \
                     turns it on, and the engine must not get hardware maths paths",
                    libm.version
                ),
            ));
        }
    }

    if engine.targets.iter().any(|t| t.kind.iter().any(|k| k == "custom-build")) {
        out.push(Finding::new(
            CHECK,
            format!("{ENGINE} has a build script; the engine is pure Rust with no build step"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `cargo metadata` document: the engine with the given dependencies, and libm
    /// resolved with the given features.
    fn meta(deps: &str, libm_features: &str, targets: &str) -> Metadata {
        let json = format!(
            r#"{{
                "packages": [
                    {{"name": "citar-engine", "version": "0.1.5", "id": "engine",
                      "manifest_path": "crates/citar-engine/Cargo.toml",
                      "dependencies": [{deps}], "targets": [{targets}]}},
                    {{"name": "libm", "version": "0.2.16", "id": "libm",
                      "manifest_path": "libm/Cargo.toml", "dependencies": [], "targets": []}}
                ],
                "workspace_members": ["engine"],
                "resolve": {{"nodes": [
                    {{"id": "engine", "features": []}},
                    {{"id": "libm", "features": [{libm_features}]}}
                ]}}
            }}"#
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    fn dep(name: &str, req: &str, kind: &str, default: bool) -> String {
        format!(
            r#"{{"name": "{name}", "req": "{req}", "kind": {kind}, "uses_default_features": {default}}}"#
        )
    }

    const LIB: &str = r#"{"kind": ["lib"]}"#;

    #[test]
    fn the_allowed_set_passes() {
        let deps = [
            dep("serde", "^1.0.229", "null", true),
            dep("libm", "=0.2.16", "null", false),
            dep("proptest", "^1.11", "\"dev\"", true),
        ]
        .join(",");
        assert_eq!(check(&meta(&deps, "", LIB)), []);
    }

    #[test]
    fn num_traits_is_refused() {
        let deps = [dep("serde", "^1", "null", true), dep("num-traits", "^0.2", "null", true)];
        let found = check(&meta(&deps.join(","), "", LIB));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("`num-traits`"));
    }

    #[test]
    fn build_dependencies_are_held_to_the_list() {
        let found = check(&meta(&dep("cc", "^1", "\"build\"", true), "", LIB));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("build-dependency `cc`"));
    }

    #[test]
    fn libm_must_be_pinned_and_plain() {
        let found = check(&meta(&dep("libm", "^0.2.16", "null", true), "\"arch\"", LIB));
        let text: Vec<_> = found.iter().map(|f| f.message.as_str()).collect();
        assert_eq!(found.len(), 3, "{text:?}");
        assert!(text[0].contains("=0.2.16"));
        assert!(text[1].contains("default-features"));
        assert!(text[2].contains("`arch`"));
    }

    #[test]
    fn a_build_script_is_refused() {
        let targets = format!(r#"{LIB}, {{"kind": ["custom-build"]}}"#);
        let found = check(&meta("", "", &targets));
        assert_eq!(found.len(), 1, "{found:?}");
    }
}
