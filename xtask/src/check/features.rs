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
//!
//! The engine's `test-ops` (`api::testops`, `api::inspect`, the rule scripts' operations) ships in
//! no wheel and no helper either (DESIGN.md P2.2, rule 3): the crates that ship (the bot, the
//! store, the runner) never turn it on, and the bindings only through their own `test-ops`
//! feature, which forwards it and does nothing else, and which no other feature of theirs turns
//! on. The Python suite's builds turn that feature on; pyproject's maturin features never list it
//! (from package 2-06b). Every finding names the manifest at fault.

use std::collections::BTreeSet;

use super::Finding;
use super::metadata::{Metadata, Package};

const CHECK: &str = "features";
const ENGINE: &str = "citar-engine";

/// The engine's features that never ship.
pub const TEST_ONLY: &[&str] = &["legacy"];

/// The crates that may turn them on: the tools and tests, none of which ships.
pub const TEST_CRATES: &[&str] = &["citar-refcheck", "citar-testkit", "citar-bench"];

/// The engine's test operations, which only the tools and tests and the bindings' test builds
/// turn on.
pub const TEST_OPS: &str = "test-ops";

/// The bindings: they may turn on [`TEST_OPS`] through their own feature of that name.
pub const BINDINGS: &str = "citar-py";

/// The engine's features that turn on one of `roots`, those included: their closure under the
/// engine's own feature table.
pub fn closure(engine: Option<&Package>, roots: &[&str]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = roots.iter().map(|&f| f.to_owned()).collect();
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
    let test_only = closure(engine, TEST_ONLY);
    let why = |f: &str| {
        if TEST_ONLY.contains(&f) { String::new() } else { " (which turns on `legacy`)".to_owned() }
    };
    if let Some(engine) = engine {
        let default = engine.features.get("default").map_or(&[][..], Vec::as_slice);
        for f in default.iter().filter(|f| test_only.contains(*f)) {
            out.push(Finding::new(
                CHECK,
                format!(
                    "{}: {ENGINE}'s default features include `{f}`{}, which never ships",
                    meta.manifest(engine),
                    why(f)
                ),
            ));
        }
    }
    for member in meta.members().filter(|m| !TEST_CRATES.contains(&m.name.as_str())) {
        let manifest = meta.manifest(member);
        for dep in member.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            if dep.name == ENGINE {
                for f in dep.features.iter().filter(|f| test_only.contains(*f)) {
                    out.push(Finding::new(
                        CHECK,
                        format!(
                            "{manifest}: {} turns on {ENGINE}'s `{f}`{}, which only {} may \
                             (DESIGN.md 4.12)",
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
                                    "{manifest}: {}'s feature `{own}` turns on {ENGINE}'s `{f}`{}, \
                                     which only {} may (DESIGN.md 4.12)",
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
                        "{manifest}: {} depends on {}, which builds the engine with test-only \
                         features",
                        member.name, dep.name
                    ),
                ));
            }
        }
    }
    out.extend(check_test_ops(meta, &test_only));
    out
}

/// The test operations: no crate but the tools and tests turns them on, except the bindings
/// through their own `test-ops` feature, which only forwards (DESIGN.md P2.2, rule 3). A feature
/// that also reaches `legacy` (`legacy_closure`) is the legacy check's, and not reported twice.
fn check_test_ops(meta: &Metadata, legacy_closure: &BTreeSet<String>) -> Vec<Finding> {
    let mut out = Vec::new();
    let engine = meta.package(ENGINE);
    let ops: BTreeSet<String> =
        closure(engine, &[TEST_OPS]).difference(legacy_closure).cloned().collect();
    if let Some(engine) = engine {
        let default = engine.features.get("default").map_or(&[][..], Vec::as_slice);
        for f in default.iter().filter(|f| ops.contains(*f)) {
            out.push(Finding::new(
                CHECK,
                format!(
                    "{}: {ENGINE}'s default features include `{f}`, the test operations, which \
                     never ship",
                    meta.manifest(engine)
                ),
            ));
        }
    }
    let who =
        format!("only {} and {BINDINGS}'s own `{TEST_OPS}` feature may", TEST_CRATES.join(", "));
    for member in meta.members().filter(|m| !TEST_CRATES.contains(&m.name.as_str())) {
        let manifest = meta.manifest(member);
        let bindings = member.name == BINDINGS;
        let mut key = None;
        for dep in member.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            if dep.name != ENGINE {
                continue;
            }
            for f in dep.features.iter().filter(|f| ops.contains(*f)) {
                out.push(Finding::new(
                    CHECK,
                    format!(
                        "{manifest}: {} turns on {ENGINE}'s `{f}` (the test operations), which \
                         {who} (DESIGN.md P2.2)",
                        member.name
                    ),
                ));
            }
            let k = dep.rename.as_deref().unwrap_or(&dep.name);
            key = Some(k);
            for (own, values) in &member.features {
                for f in values.iter().filter_map(|v| engine_feature(v, k)) {
                    let forwards = bindings && own == TEST_OPS && f == TEST_OPS;
                    if ops.contains(f) && !forwards {
                        out.push(Finding::new(
                            CHECK,
                            format!(
                                "{manifest}: {}'s feature `{own}` turns on {ENGINE}'s `{f}` (the \
                                 test operations), which {who} (DESIGN.md P2.2)",
                                member.name
                            ),
                        ));
                    }
                }
            }
        }
        if !bindings {
            continue;
        }
        // The bindings' own feature forwards, and nothing else of theirs turns it on.
        if let Some(values) = member.features.get(TEST_OPS) {
            let forward = |v: &String| key.and_then(|k| engine_feature(v, k)) == Some(TEST_OPS);
            if values.len() != 1 || !values.iter().all(forward) {
                out.push(Finding::new(
                    CHECK,
                    format!(
                        "{manifest}: {BINDINGS}'s `{TEST_OPS}` feature must only forward to \
                         {ENGINE}'s, but turns on [{}] (DESIGN.md P2.2)",
                        values.join(", ")
                    ),
                ));
            }
        }
        for (own, values) in member.features.iter().filter(|(own, _)| *own != TEST_OPS) {
            if values.iter().any(|v| v == TEST_OPS) {
                out.push(Finding::new(
                    CHECK,
                    format!(
                        "{manifest}: {BINDINGS}'s feature `{own}` turns on its `{TEST_OPS}`, and \
                         with it the engine's test operations, which a shipped wheel never has \
                         (DESIGN.md P2.2)"
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

    /// The engine's real feature table: `test-ops` does not reach `legacy`.
    const REAL_ENGINE: &str = r#""default": ["embedded-ruleset"], "embedded-ruleset": [], "legacy": [],
                            "test-ops": [], "checks": [], "stats": []"#;

    /// A member as cargo metadata writes it, with its manifest's path.
    fn member(name: &str, deps: &str, features: &str) -> String {
        format!(
            r#"{{"name": "{name}", "version": "0.1.5", "id": "{name}",
                 "manifest_path": "/ws/crates/{name}/Cargo.toml",
                 "dependencies": [{deps}], "targets": [], "features": {{{features}}}}}"#
        )
    }

    fn workspace(members: &[String]) -> Metadata {
        let mut packages = vec![member("citar-engine", "", REAL_ENGINE)];
        packages.extend(members.iter().cloned());
        let ids: Vec<String> = packages
            .iter()
            .filter_map(|p| p.split('"').nth(3).map(|n| format!(r#""{n}""#)))
            .collect();
        let json = format!(
            r#"{{"packages": [{}], "workspace_members": [{}], "resolve": null,
                 "workspace_root": "/ws"}}"#,
            packages.join(","),
            ids.join(",")
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    /// Gate 2 of package 2-00a: citar-bot enabling test-ops fails, naming its manifest.
    #[test]
    fn a_shipped_crate_turning_on_the_test_operations_fails_naming_its_manifest() {
        let ops = dep("citar-engine", "null", r#""test-ops""#);
        let found = check(&workspace(&[member("citar-bot", &ops, "")]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.starts_with(
                "crates/citar-bot/Cargo.toml: citar-bot turns on citar-engine's `test-ops`"
            ),
            "{found:?}"
        );
        // Through a feature of its own, for the store and the runner alike.
        let plain = dep("citar-engine", "null", "");
        for name in ["citar-store", "citar-sim"] {
            let f = r#""test-ops": ["citar-engine/test-ops"]"#;
            let found = check(&workspace(&[member(name, &plain, f)]));
            assert_eq!(found.len(), 1, "{name}: {found:?}");
            assert!(found[0].message.starts_with(&format!("crates/{name}/Cargo.toml: ")));
        }
        // A dev-dependency never ships; the tools and tests may.
        let dev = dep("citar-engine", r#""dev""#, r#""test-ops""#);
        assert_eq!(check(&workspace(&[member("citar-bot", &dev, "")])), []);
        assert_eq!(check(&workspace(&[member("citar-testkit", &ops, "")])), []);
    }

    #[test]
    fn the_bindings_may_only_forward_their_own_test_ops_feature() {
        let plain = dep("citar-engine", "null", r#""embedded-ruleset""#);
        let forward = r#""test-ops": ["citar-engine/test-ops"]"#;
        assert_eq!(check(&workspace(&[member("citar-py", &plain, forward)])), []);
        let optional = r#""test-ops": ["citar-engine?/test-ops"]"#;
        assert_eq!(check(&workspace(&[member("citar-py", &plain, optional)])), []);
        let bad = |deps: &str, features: &str, says: &str| {
            let found = check(&workspace(&[member("citar-py", deps, features)]));
            assert_eq!(found.len(), 1, "{features}: {found:?}");
            assert!(found[0].message.starts_with("crates/citar-py/Cargo.toml: "), "{found:?}");
            assert!(found[0].message.contains(says), "{found:?}");
        };
        // In the dependency itself, always on.
        bad(&dep("citar-engine", "null", r#""test-ops""#), "", "citar-py turns on");
        // Through another feature of its own.
        bad(&plain, r#""dev": ["citar-engine/test-ops"]"#, "feature `dev` turns on citar-engine's");
        // A test-ops feature that does more than forward.
        bad(
            &plain,
            r#""test-ops": ["citar-engine/test-ops", "citar-engine/stats"]"#,
            "only forward",
        );
        bad(&plain, r#""test-ops": []"#, "only forward");
        // Its default, or any other feature, turning its test-ops on.
        bad(
            &plain,
            r#""test-ops": ["citar-engine/test-ops"], "default": ["test-ops"]"#,
            "`default`",
        );
    }
}
