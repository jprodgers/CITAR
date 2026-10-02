//! The crate graph (DESIGN.md P2.2): each workspace crate depends only on the workspace crates
//! its row of [`GRAPH`] names, in every kind of dependency.
//!
//! The engine depends on no workspace crate; the bot on the engine; the store on none, so the
//! helper and the bindings can link it; the runner on the engine and the bot; the bindings on all
//! four. The tools and tests (testkit, refcheck, bench) may add the bot, and bench the runner. A
//! crate that joins the workspace gets a row here, deliberately.

use std::collections::BTreeSet;

use super::Finding;
use super::metadata::Metadata;

const CHECK: &str = "graph";

/// Each workspace crate and the workspace crates it may depend on.
pub const GRAPH: &[(&str, &[&str])] = &[
    ("citar-engine", &[]),
    ("citar-bot", &["citar-engine"]),
    ("citar-store", &[]),
    ("citar-sim", &["citar-engine", "citar-bot"]),
    ("citar-py", &["citar-engine", "citar-bot", "citar-sim", "citar-store"]),
    ("citar-testkit", &["citar-engine", "citar-bot"]),
    ("citar-refcheck", &["citar-engine", "citar-bot"]),
    ("citar-bench", &["citar-engine", "citar-testkit", "citar-bot", "citar-sim"]),
    ("xtask", &[]),
];

pub fn check(meta: &Metadata) -> Vec<Finding> {
    let mut out = Vec::new();
    let members: BTreeSet<&str> = meta.members().map(|m| m.name.as_str()).collect();
    for m in meta.members() {
        let manifest = meta.manifest(m);
        let Some(&(_, allowed)) = GRAPH.iter().find(|(name, _)| *name == m.name) else {
            out.push(Finding::new(
                CHECK,
                format!(
                    "{manifest}: {} is not in the crate graph: give it a row in \
                     xtask/src/check/graph.rs (DESIGN.md P2.2)",
                    m.name
                ),
            ));
            continue;
        };
        let mut seen = BTreeSet::new();
        for dep in &m.dependencies {
            let name = dep.name.as_str();
            if !members.contains(name) || allowed.contains(&name) || !seen.insert(name) {
                continue;
            }
            let may = if allowed.is_empty() {
                "no workspace crate".to_owned()
            } else {
                allowed.join(", ")
            };
            out.push(Finding::new(
                CHECK,
                format!(
                    "{manifest}: {} depends on {name}, which the crate graph does not allow \
                     (DESIGN.md P2.2): it may depend on {may}",
                    m.name
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `cargo metadata` document: these members, each with these workspace
    /// dependencies, `(name, kind)`.
    fn meta(members: &[(&str, &[(&str, &str)])]) -> Metadata {
        let packages: Vec<String> = members
            .iter()
            .map(|(name, deps)| {
                let deps: Vec<String> = deps
                    .iter()
                    .map(|(d, kind)| {
                        format!(
                            r#"{{"name": "{d}", "req": "*", "kind": {kind},
                                 "uses_default_features": true}}"#
                        )
                    })
                    .collect();
                format!(
                    r#"{{"name": "{name}", "version": "0.1.5", "id": "{name}",
                         "manifest_path": "/ws/crates/{name}/Cargo.toml",
                         "dependencies": [{}], "targets": []}}"#,
                    deps.join(",")
                )
            })
            .collect();
        let ids: Vec<String> = members.iter().map(|(n, _)| format!(r#""{n}""#)).collect();
        let json = format!(
            r#"{{"packages": [{}], "workspace_members": [{}], "resolve": null,
                 "workspace_root": "/ws"}}"#,
            packages.join(","),
            ids.join(",")
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    const N: &str = "null";

    #[test]
    fn the_designed_graph_passes() {
        let m = meta(&[
            ("citar-engine", &[]),
            ("citar-bot", &[("citar-engine", N)]),
            ("citar-store", &[]),
            ("citar-sim", &[("citar-engine", N), ("citar-bot", N)]),
            (
                "citar-py",
                &[("citar-engine", N), ("citar-bot", N), ("citar-sim", N), ("citar-store", N)],
            ),
            ("citar-testkit", &[("citar-engine", N), ("citar-bot", N)]),
            ("citar-refcheck", &[("citar-engine", N), ("citar-bot", N)]),
            ("citar-bench", &[("citar-engine", N), ("citar-testkit", N), ("citar-sim", N)]),
            ("xtask", &[]),
        ]);
        assert_eq!(check(&m), []);
    }

    /// Gate 2 of package 2-00a: citar-sim depending on citar-py fails, naming the manifest.
    #[test]
    fn the_runner_depending_on_the_bindings_fails_naming_its_manifest() {
        let m = meta(&[
            ("citar-engine", &[]),
            ("citar-py", &[("citar-engine", N)]),
            ("citar-sim", &[("citar-engine", N), ("citar-py", N)]),
        ]);
        let found = check(&m);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.starts_with(
                "crates/citar-sim/Cargo.toml: citar-sim depends on citar-py, which the crate \
                 graph does not allow"
            ),
            "{found:?}"
        );
        assert!(found[0].message.ends_with("it may depend on citar-engine, citar-bot"));
    }

    #[test]
    fn every_kind_of_dependency_counts_and_a_new_crate_needs_a_row() {
        let m = meta(&[
            ("citar-engine", &[("citar-bot", r#""dev""#)]),
            ("citar-bot", &[("citar-engine", N), ("citar-engine", r#""dev""#)]),
            ("citar-store", &[("citar-engine", r#""build""#)]),
            ("citar-new", &[]),
        ]);
        let found: Vec<String> = check(&m).into_iter().map(|f| f.message).collect();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].contains("citar-engine depends on citar-bot"), "{found:?}");
        assert!(found[0].ends_with("it may depend on no workspace crate"), "{found:?}");
        assert!(found[1].contains("citar-store depends on citar-engine"), "{found:?}");
        assert!(found[2].contains("citar-new is not in the crate graph"), "{found:?}");
        // A crate outside the workspace is no workspace crate.
        assert_eq!(check(&meta(&[("citar-store", &[("zstd", N)])])), []);
    }
}
