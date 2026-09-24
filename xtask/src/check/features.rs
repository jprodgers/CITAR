//! Which crates may build the engine with its test-only features (DESIGN.md 2.3, 4.12).
//!
//! The engine's `legacy` feature compiles `compat::python`, the Python-state converter that
//! refcheck, testkit and bench use. It never ships (DESIGN.md 1.2): no crate but those three may
//! turn it on, the engine's default features may not include it, and no other crate may depend
//! on one of the three, which would turn it on through them. Dev-dependencies are free, since
//! they never reach a shipped build.

use super::Finding;
use super::metadata::Metadata;

const CHECK: &str = "features";
const ENGINE: &str = "citar-engine";

/// The engine's features that never ship.
const TEST_ONLY: &[&str] = &["legacy"];

/// The crates that may turn them on: the tools and tests, none of which ships.
pub const TEST_CRATES: &[&str] = &["citar-refcheck", "citar-testkit", "citar-bench"];

pub fn check(meta: &Metadata) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(engine) = meta.package(ENGINE) {
        let default = engine.features.get("default").map_or(&[][..], Vec::as_slice);
        for f in TEST_ONLY.iter().filter(|f| default.iter().any(|d| d == *f)) {
            out.push(Finding::new(
                CHECK,
                format!("{ENGINE}'s default features include `{f}`, which never ships"),
            ));
        }
    }
    for member in meta.members().filter(|m| !TEST_CRATES.contains(&m.name.as_str())) {
        for dep in member.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            if dep.name == ENGINE {
                for f in dep.features.iter().filter(|f| TEST_ONLY.contains(&f.as_str())) {
                    out.push(Finding::new(
                        CHECK,
                        format!(
                            "{} turns on {ENGINE}'s `{f}`, which only {} may (DESIGN.md 4.12)",
                            member.name,
                            TEST_CRATES.join(", ")
                        ),
                    ));
                }
            }
            if TEST_CRATES.contains(&dep.name.as_str()) {
                out.push(Finding::new(
                    CHECK,
                    format!(
                        "{} depends on {}, which builds the engine with test-only features",
                        member.name, dep.name
                    ),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `cargo metadata` document: the engine with these default features, and members
    /// with these dependencies on the engine.
    fn meta(default: &str, members: &[(&str, &str)]) -> Metadata {
        let mut packages = vec![format!(
            r#"{{"name": "citar-engine", "version": "0.1.5", "id": "engine",
                 "dependencies": [], "targets": [],
                 "features": {{"default": [{default}], "legacy": []}}}}"#
        )];
        let mut ids = vec![r#""engine""#.to_owned()];
        for (name, deps) in members {
            packages.push(format!(
                r#"{{"name": "{name}", "version": "0.1.5", "id": "{name}",
                     "dependencies": [{deps}], "targets": []}}"#
            ));
            ids.push(format!(r#""{name}""#));
        }
        let json = format!(
            r#"{{"packages": [{}], "workspace_members": [{}], "resolve": null}}"#,
            packages.join(","),
            ids.join(",")
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    fn dep(name: &str, kind: &str, features: &str) -> String {
        format!(
            r#"{{"name": "{name}", "req": "*", "kind": {kind}, "uses_default_features": false,
                 "features": [{features}]}}"#
        )
    }

    #[test]
    fn the_test_crates_may_turn_legacy_on() {
        let d = dep("citar-engine", "null", r#""embedded-ruleset", "legacy""#);
        let found =
            check(&meta(r#""embedded-ruleset""#, &[("citar-refcheck", &d), ("citar-bench", &d)]));
        assert_eq!(found, []);
    }

    #[test]
    fn a_shipping_crate_may_not() {
        let legacy = dep("citar-engine", "null", r#""legacy""#);
        let found = check(&meta(r#""embedded-ruleset""#, &[("citar-py", &legacy)]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("citar-py turns on citar-engine's `legacy`"));
        // Nor through a test crate; a dev-dependency never ships, so it may.
        let via = dep("citar-testkit", "null", "");
        assert_eq!(check(&meta("", &[("citar-sim", &via)])).len(), 1);
        let dev = dep("citar-engine", r#""dev""#, r#""legacy""#);
        assert_eq!(check(&meta("", &[("citar-bot", &dev)])), []);
    }

    #[test]
    fn the_engine_may_not_default_to_legacy() {
        let found = check(&meta(r#""embedded-ruleset", "legacy""#, &[]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("default features include `legacy`"));
    }
}
