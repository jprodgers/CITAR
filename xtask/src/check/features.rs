//! Which crates may build the engine with its test-only features (DESIGN.md 2.3, 4.12).
//!
//! The engine's `legacy` feature compiles `compat::python`, the Python-state converter that
//! refcheck, testkit and bench use. It never ships (DESIGN.md 1.2): no crate but those three may
//! turn it on, the engine's default features may not include it, and no other crate may depend
//! on one of the three, which would turn it on through them. Dev-dependencies are free, since
//! they never reach a shipped build.
//!
//! A feature can be turned on in three ways, and each is checked: in a dependency's `features`
//! list, through a feature of the dependent's own (`x = ["citar-engine/legacy"]`, or
//! `citar-engine?/legacy` for an optional dependency, under the dependency's rename if it has
//! one), and through another feature of the engine's that implies it (`y = ["legacy"]`). So every
//! engine feature from which `legacy` can be reached counts as test-only.

use std::collections::BTreeSet;

use super::Finding;
use super::metadata::{Metadata, Package};

const CHECK: &str = "features";
const ENGINE: &str = "citar-engine";

/// The engine's features that never ship.
const TEST_ONLY: &[&str] = &["legacy"];

/// The crates that may turn them on: the tools and tests, none of which ships.
pub const TEST_CRATES: &[&str] = &["citar-refcheck", "citar-testkit", "citar-bench"];

/// The engine's features that turn on a test-only one, those included: the closure of
/// [`TEST_ONLY`] under the engine's own feature table.
fn test_only(engine: Option<&Package>) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = TEST_ONLY.iter().map(|&f| f.to_owned()).collect();
    let Some(engine) = engine else { return out };
    loop {
        let before = out.len();
        for (f, implies) in &engine.features {
            if !out.contains(f) && implies.iter().any(|v| out.contains(v)) {
                out.insert(f.clone());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// The engine feature a member's feature value turns on, if it names one through `dep`:
/// `dep/f` or `dep?/f`.
fn engine_feature<'v>(value: &'v str, dep: &str) -> Option<&'v str> {
    let (name, f) = value.split_once('/')?;
    (name.strip_suffix('?').unwrap_or(name) == dep).then_some(f)
}

pub fn check(meta: &Metadata) -> Vec<Finding> {
    let mut out = Vec::new();
    let engine = meta.package(ENGINE);
    let test_only = test_only(engine);
    let why = |f: &str| {
        if TEST_ONLY.contains(&f) { String::new() } else { " (which turns on `legacy`)".to_owned() }
    };
    if let Some(engine) = engine {
        let default = engine.features.get("default").map_or(&[][..], Vec::as_slice);
        for f in default.iter().filter(|f| test_only.contains(*f)) {
            out.push(Finding::new(
                CHECK,
                format!("{ENGINE}'s default features include `{f}`{}, which never ships", why(f)),
            ));
        }
    }
    for member in meta.members().filter(|m| !TEST_CRATES.contains(&m.name.as_str())) {
        for dep in member.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            if dep.name == ENGINE {
                for f in dep.features.iter().filter(|f| test_only.contains(*f)) {
                    out.push(Finding::new(
                        CHECK,
                        format!(
                            "{} turns on {ENGINE}'s `{f}`{}, which only {} may (DESIGN.md 4.12)",
                            member.name,
                            why(f),
                            TEST_CRATES.join(", ")
                        ),
                    ));
                }
                let key = dep.rename.as_deref().unwrap_or(&dep.name);
                for (own, values) in &member.features {
                    for f in values.iter().filter_map(|v| engine_feature(v, key)) {
                        if test_only.contains(f) {
                            out.push(Finding::new(
                                CHECK,
                                format!(
                                    "{}'s feature `{own}` turns on {ENGINE}'s `{f}`{}, which only \
                                     {} may (DESIGN.md 4.12)",
                                    member.name,
                                    why(f),
                                    TEST_CRATES.join(", ")
                                ),
                            ));
                        }
                    }
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

    /// A trimmed `cargo metadata` document: the engine with this feature table, and members, each
    /// with these dependencies and this feature table.
    fn meta_with(engine_features: &str, members: &[(&str, &str, &str)]) -> Metadata {
        let mut packages = vec![format!(
            r#"{{"name": "citar-engine", "version": "0.1.5", "id": "engine",
                 "dependencies": [], "targets": [], "features": {{{engine_features}}}}}"#
        )];
        let mut ids = vec![r#""engine""#.to_owned()];
        for (name, deps, features) in members {
            packages.push(format!(
                r#"{{"name": "{name}", "version": "0.1.5", "id": "{name}",
                     "dependencies": [{deps}], "targets": [], "features": {{{features}}}}}"#
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

    /// The engine with these default features, and members with these dependencies.
    fn meta(default: &str, members: &[(&str, &str)]) -> Metadata {
        let engine = format!(r#""default": [{default}], "legacy": []"#);
        let members: Vec<(&str, &str, &str)> = members.iter().map(|&(n, d)| (n, d, "")).collect();
        meta_with(&engine, &members)
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

    #[test]
    fn a_feature_of_the_shipping_crate_may_not_forward_to_legacy() {
        let plain = dep("citar-engine", "null", r#""embedded-ruleset""#);
        let engine = r#""default": ["embedded-ruleset"], "legacy": []"#;
        for features in
            [r#""default": ["citar-engine/legacy"]"#, r#""compat": ["citar-engine?/legacy"]"#]
        {
            let found = check(&meta_with(engine, &[("citar-py", &plain, features)]));
            assert_eq!(found.len(), 1, "{features}: {found:?}");
            assert!(found[0].message.contains("citar-py's feature"), "{found:?}");
        }
        // Under the dependency's rename, which the feature table uses.
        let renamed = r#"{"name": "citar-engine", "rename": "engine", "req": "*", "kind": null,
                          "uses_default_features": true, "features": []}"#;
        let found =
            check(&meta_with(engine, &[("citar-sim", renamed, r#""x": ["engine/legacy"]"#)]));
        assert_eq!(found.len(), 1, "{found:?}");
        // Another crate's feature of that name is not the engine's; a test crate may forward.
        let found = check(&meta_with(engine, &[("citar-sim", &plain, r#""x": ["serde/legacy"]"#)]));
        assert_eq!(found, []);
        let test = [("citar-testkit", plain.as_str(), r#""x": ["citar-engine/legacy"]"#)];
        assert_eq!(check(&meta_with(engine, &test)), []);
    }

    #[test]
    fn an_engine_feature_that_implies_legacy_is_test_only_too() {
        let engine = r#""default": ["embedded-ruleset"], "legacy": [], "test-ops": ["legacy"],
                        "everything": ["test-ops", "stats"], "stats": []"#;
        let ops = dep("citar-engine", "null", r#""everything""#);
        let found = check(&meta_with(engine, &[("citar-py", &ops, "")]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("`everything` (which turns on `legacy`)"), "{found:?}");
        let stats = dep("citar-engine", "null", r#""stats""#);
        assert_eq!(check(&meta_with(engine, &[("citar-py", &stats, "")])), []);
        let forwarded = dep("citar-engine", "null", "");
        let found = check(&meta_with(
            engine,
            &[("citar-py", &forwarded, r#""x": ["citar-engine/test-ops"]"#)],
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        let defaults = r#""default": ["test-ops"], "legacy": [], "test-ops": ["legacy"]"#;
        let found = check(&meta_with(defaults, &[]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("default features include `test-ops`"), "{found:?}");
    }
}
