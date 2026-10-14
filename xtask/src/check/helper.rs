//! The helper's rules (design/phase4-helper.md P4.3.1), from package 4-01:
//!
//! - **`unsafe` stays in one module.** The workspace denies `unsafe_code`; Phase 4's only
//!   exception is citar-jobs' `src/sys/windows.rs` and `src/sys/unix.rs`, the OS calls no safe
//!   wrapper makes (Job Objects, `setpriority`, `setrlimit`, owner-only DACLs). An `unsafe` token
//!   anywhere else in citar-jobs, or anywhere in citar-helper (sources, tests, build script), is
//!   refused, whatever lint allowance the file carries: the helper calls the module's safe
//!   functions. A crate that does not exist yet (citar-jobs before 4-03) is skipped.
//! - **The test features and the development guard never ship.** citar-helper's and
//!   citar-jobs' `test-faults`, `test-update-key`, `test-jobs`, `dev-guard` and `conformance` (the
//!   fixtures' loaders, which read files under the crate's source directory), and every feature of
//!   theirs that turns one on, are refused:
//!   - in their crate's default features;
//!   - in a dependency's feature list, in any crate but the tools and tests;
//!   - in a feature that forwards to one, unless that feature is itself one of them in a helper
//!     crate, or citar-py's own `test-ops` (the Python suite's builds; `features` holds the rest
//!     of that feature);
//!   - in `pyproject.toml`'s maturin features (`pyproject`);
//!   - on any line of `release.yml`, and of `helper.yml`'s artifact jobs (those whose id starts
//!     with `artifact`) and its workflow-wide `env:` and `defaults:`, which apply to every job;
//!     none of these may pass `--all-features`, or a feature list held in a variable
//!     (`--features $FEATURES`, `-F ${{ matrix.features }}`), which no check can read. The
//!     workflows are read as text, as `triggers` reads rust.yml: a comment is not a build line,
//!     and a job id is a key at the indentation of the first key under `jobs:`, whatever it is.
//!
//! The loopback guard itself is the helper's code (4-03), compiled in with `dev-guard`.

use std::collections::BTreeSet;
use std::path::Path;

use super::Finding;
use super::features::{self, BINDINGS, TEST_CRATES, TEST_OPS};
use super::metadata::Metadata;
use super::source::SourceTree;

/// The helper's crate, relative to the workspace root.
pub const HELPER_DIR: &str = "crates/citar-helper";
/// The jobs crate.
pub const JOBS_DIR: &str = "crates/citar-jobs";
/// The files of citar-jobs that may hold `unsafe`, relative to its crate.
pub const JOBS_SYS: &[&str] = &["src/sys/windows.rs", "src/sys/unix.rs"];

/// The crates whose test features are checked.
pub const HELPER_CRATES: &[&str] = &["citar-helper", "citar-jobs"];
/// Their features that never ship.
pub const TEST_FEATURES: &[&str] =
    &["test-faults", "test-update-key", "test-jobs", "dev-guard", "conformance"];

/// The workflow that builds releases, read whole.
pub const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
/// The helper's workflow (from 4-14), whose artifact jobs are read.
pub const HELPER_WORKFLOW: &str = ".github/workflows/helper.yml";
/// helper.yml's jobs that build what ships: their ids start with this.
pub const ARTIFACT_JOB: &str = "artifact";
/// A workflow's top-level keys that apply to every job, so to the artifact jobs too.
pub const WORKFLOW_WIDE: &[&str] = &["env", "defaults"];

/// Every rule above, against the workspace at `root`.
pub fn check(root: &Path, meta: &Metadata) -> Result<Vec<Finding>, String> {
    let mut out = Vec::new();
    for (dir, allowed) in [(HELPER_DIR, &[][..]), (JOBS_DIR, JOBS_SYS)] {
        let path = root.join(dir);
        if path.is_dir() {
            out.extend(check_unsafe(&SourceTree::load_under(dir, &path)?, allowed));
        }
    }
    out.extend(check_features(meta));
    for (file, jobs) in [(RELEASE_WORKFLOW, None), (HELPER_WORKFLOW, Some(ARTIFACT_JOB))] {
        let path = root.join(file);
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            out.extend(check_workflow(file, &text, jobs));
        }
    }
    Ok(out)
}

/// `unsafe` in a crate's files, other than in `allowed` (paths relative to the crate).
pub fn check_unsafe(tree: &SourceTree, allowed: &[&str]) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in tree.files.iter().filter(|f| !allowed.contains(&f.rel.as_str())) {
        for t in file.tokens.iter().filter(|t| t.is_ident("unsafe")) {
            let place = if allowed.is_empty() {
                "the helper calls citar-jobs' sys module, Phase 4's only unsafe code".to_owned()
            } else {
                format!("only {} may hold it", allowed.join(" and "))
            };
            out.push(Finding::new(
                "unsafe",
                format!("{}: `unsafe` outside its one module: {place} (P4.3.1)", file.at(t.line)),
            ));
        }
    }
    out
}

/// The test features of each helper crate present, closed under its own feature table.
fn test_closures(meta: &Metadata) -> Vec<(&'static str, BTreeSet<String>)> {
    HELPER_CRATES
        .iter()
        .filter_map(|&c| meta.package(c).map(|p| (c, features::closure(Some(p), TEST_FEATURES))))
        .collect()
}

/// The features that never ship, as `crate/feature` values: what `pyproject` refuses in the
/// maturin table.
pub fn never_shipped(meta: &Metadata) -> BTreeSet<String> {
    test_closures(meta)
        .into_iter()
        .flat_map(|(c, fs)| fs.into_iter().map(move |f| format!("{c}/{f}")))
        .collect()
}

pub fn check_features(meta: &Metadata) -> Vec<Finding> {
    let mut out = Vec::new();
    let closures = test_closures(meta);
    let test_only =
        |krate: &str, f: &str| closures.iter().any(|(c, fs)| *c == krate && fs.contains(f));
    for &(krate, ref fs) in &closures {
        let Some(p) = meta.package(krate) else { continue };
        let default = p.features.get("default").map_or(&[][..], Vec::as_slice);
        for f in default.iter().filter(|f| fs.contains(*f)) {
            out.push(Finding::new(
                "features",
                format!(
                    "{}: {krate}'s default features include `{f}`, a test or development feature, \
                     which never ships (P4.3.1)",
                    meta.manifest(p)
                ),
            ));
        }
    }
    for member in meta.members().filter(|m| !TEST_CRATES.contains(&m.name.as_str())) {
        let manifest = meta.manifest(member);
        for dep in member.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            if !HELPER_CRATES.contains(&dep.name.as_str()) {
                continue;
            }
            for f in dep.features.iter().filter(|f| test_only(&dep.name, f)) {
                out.push(Finding::new(
                    "features",
                    format!(
                        "{manifest}: {} turns on {}'s `{f}` in its dependency, so every build of \
                         it has a feature that never ships (P4.3.1)",
                        member.name, dep.name
                    ),
                ));
            }
            let key = dep.rename.as_deref().unwrap_or(&dep.name);
            for (own, values) in &member.features {
                let own_ok = (HELPER_CRATES.contains(&member.name.as_str())
                    && test_only(&member.name, own))
                    || (member.name == BINDINGS && own == TEST_OPS);
                for v in values {
                    let Some((d, f)) = v.split_once('/') else { continue };
                    if d.strip_suffix('?').unwrap_or(d) == key && test_only(&dep.name, f) && !own_ok
                    {
                        out.push(Finding::new(
                            "features",
                            format!(
                                "{manifest}: {}'s feature `{own}` turns on {}'s `{f}`, which only \
                                 a test feature of a helper crate or {BINDINGS}'s `{TEST_OPS}` may \
                                 (P4.3.1)",
                                member.name, dep.name
                            ),
                        ));
                    }
                }
            }
        }
    }
    out
}

/// A line without its comment: from the first ` #` (or a `#` starting it), as YAML reads one
/// outside quotes. A `#` inside a quoted string is rare enough in a workflow to read as a comment.
fn code_of(line: &str) -> &str {
    let body = line.trim_start();
    if body.starts_with('#') {
        return "";
    }
    match line.find(" #") {
        Some(at) => line[..at].trim_end(),
        None => line.trim_end(),
    }
}

/// Whether a line passes cargo a feature list held in a variable: `--features $X`,
/// `--features=${{ matrix.features }}`, `-F "$X"`.
fn features_from_a_variable(code: &str) -> bool {
    let words: Vec<&str> = code.split_whitespace().collect();
    words.iter().enumerate().any(|(i, w)| {
        let w = w.trim_start_matches(['"', '\'']);
        let value = if w == "--features" || w == "-F" {
            words.get(i + 1).copied()
        } else {
            w.strip_prefix("--features=").or_else(|| w.strip_prefix("-F="))
        };
        value.is_some_and(|v| v.trim_start_matches(['"', '\'']).starts_with('$'))
    })
}

/// A workflow's lines that would build a test feature into what ships: every line of
/// `release.yml`, or the lines of `helper.yml`'s jobs whose id starts with `jobs_prefix` and of
/// its workflow-wide `env:` and `defaults:`.
pub fn check_workflow(file: &str, text: &str, jobs_prefix: Option<&str>) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut top: Option<String> = None;
    let mut job_indent: Option<usize> = None;
    let mut current_job: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let code = code_of(line);
        let body = code.trim_start();
        if body.is_empty() {
            continue;
        }
        let indent = code.len() - body.len();
        if indent == 0 {
            top = body.strip_suffix(':').map(|k| k.trim_end().to_owned());
            job_indent = None;
            current_job = None;
            continue;
        }
        if top.as_deref() == Some("jobs") {
            let at = *job_indent.get_or_insert(indent);
            if indent == at {
                current_job = body.strip_suffix(':').map(|k| k.trim_end().to_owned());
            }
        }
        let watched = match jobs_prefix {
            None => true,
            Some(prefix) => match top.as_deref() {
                Some("jobs") => current_job.as_deref().is_some_and(|j| j.starts_with(prefix)),
                Some(key) => WORKFLOW_WIDE.contains(&key),
                None => false,
            },
        };
        if !watched {
            continue;
        }
        let mut named: Vec<&str> =
            TEST_FEATURES.iter().copied().filter(|f| body.contains(f)).collect();
        if body.contains("--all-features") {
            named.push("--all-features");
        }
        let place = match (&current_job, top.as_deref()) {
            (Some(j), Some("jobs")) if jobs_prefix.is_some() => format!(" (job {j})"),
            (_, Some(key)) if jobs_prefix.is_some() && key != "jobs" => {
                format!(" (workflow {key})")
            }
            _ => String::new(),
        };
        for what in named {
            out.push(Finding::new(
                "features",
                format!(
                    "{file}:{}{place}: names `{what}`, which no release or artifact build may \
                     carry (P4.3.1)",
                    i + 1
                ),
            ));
        }
        if features_from_a_variable(body) {
            out.push(Finding::new(
                "features",
                format!(
                    "{file}:{}{place}: takes its feature list from a variable, which no check \
                     can read: a release or artifact build names its features (P4.3.1)",
                    i + 1
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::source::SourceFile;
    use std::path::Path;

    #[test]
    fn unsafe_is_refused_in_the_helper_and_outside_the_jobs_sys_module() {
        let helper = SourceTree {
            files: vec![
                SourceFile::under(HELPER_DIR, "src/lib.rs", "// unsafe in a comment\nfn f() {}"),
                SourceFile::under(HELPER_DIR, "tests/x.rs", "\nfn f() { unsafe { g() } }"),
                SourceFile::under(HELPER_DIR, "build.rs", "const S: &str = \"unsafe\";"),
            ],
        };
        let found = check_unsafe(&helper, &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.starts_with("crates/citar-helper/tests/x.rs:2: `unsafe`"));
        let jobs = SourceTree {
            files: vec![
                SourceFile::under(JOBS_DIR, "src/sys/windows.rs", "unsafe { f() }"),
                SourceFile::under(JOBS_DIR, "src/sys/unix.rs", "unsafe fn g() {}"),
                SourceFile::under(JOBS_DIR, "src/sys/mod.rs", "unsafe impl Send for X {}"),
                SourceFile::under(JOBS_DIR, "src/host.rs", "#![allow(unsafe_code)]\nunsafe {}"),
            ],
        };
        let found: Vec<String> =
            check_unsafe(&jobs, JOBS_SYS).into_iter().map(|f| f.message).collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("crates/citar-jobs/src/sys/mod.rs:1:"), "{found:?}");
        assert!(found[1].starts_with("crates/citar-jobs/src/host.rs:2:"), "{found:?}");
    }

    /// The helper as it is has no `unsafe`.
    #[test]
    fn the_helper_as_it_is_passes() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let tree = SourceTree::load_under(HELPER_DIR, &root.join(HELPER_DIR))?;
        assert!(tree.files.iter().any(|f| f.rel == "src/lib.rs"));
        assert!(tree.files.iter().any(|f| f.rel == "build.rs"));
        assert_eq!(check_unsafe(&tree, &[]), []);
        Ok(())
    }

    /// A trimmed workspace: the helper and the jobs crate with these feature tables and
    /// dependencies, and citar-py with its own.
    fn meta(helper: &str, helper_deps: &str, jobs: &str, py: &str, py_deps: &str) -> Metadata {
        let json = format!(
            r#"{{"packages": [
                {{"name": "citar-helper", "version": "0.1.5", "id": "h", "dependencies": [{helper_deps}],
                  "targets": [], "features": {{{helper}}}}},
                {{"name": "citar-jobs", "version": "0.1.5", "id": "j", "dependencies": [],
                  "targets": [], "features": {{{jobs}}}}},
                {{"name": "citar-py", "version": "0.1.5", "id": "p", "dependencies": [{py_deps}],
                  "targets": [], "features": {{{py}}}}},
                {{"name": "citar-testkit", "version": "0.1.5", "id": "t",
                  "dependencies": [{{"name": "citar-jobs", "req": "*", "kind": null,
                                    "uses_default_features": true, "features": ["test-jobs"]}}],
                  "targets": [], "features": {{}}}}],
              "workspace_members": ["h", "j", "p", "t"], "resolve": null}}"#
        );
        Metadata::parse(json.as_bytes()).expect("test metadata parses")
    }

    fn dep(name: &str, features: &str) -> String {
        format!(
            r#"{{"name": "{name}", "req": "*", "kind": null, "uses_default_features": true,
                 "features": [{features}]}}"#
        )
    }

    const HELPER: &str =
        r#""dev-guard": [], "test-faults": ["citar-jobs/test-jobs"], "test-update-key": []"#;
    const JOBS: &str = r#""test-jobs": [], "test-update-key": [], "all-test": ["test-jobs"]"#;

    #[test]
    fn the_designed_features_pass() {
        let py = r#""test-ops": ["citar-engine/test-ops", "citar-jobs/test-jobs"]"#;
        let m = meta(HELPER, &dep("citar-jobs", ""), JOBS, py, &dep("citar-jobs", ""));
        assert_eq!(check_features(&m), []);
        let shipped = never_shipped(&m);
        for f in ["citar-helper/dev-guard", "citar-jobs/test-jobs", "citar-jobs/all-test"] {
            assert!(shipped.contains(f), "{f}: {shipped:?}");
        }
    }

    #[test]
    fn a_test_feature_reaching_a_shipped_build_is_refused() {
        let jobs_dep = dep("citar-jobs", "");
        let cases = [
            // the helper's default features turn one on, directly or through another feature
            (
                format!(r#"{HELPER}, "default": ["dev-guard"]"#),
                jobs_dep.clone(),
                JOBS.to_owned(),
                "",
                "default features",
            ),
            (
                HELPER.to_owned(),
                jobs_dep.clone(),
                format!(r#"{JOBS}, "default": ["all-test"]"#),
                "",
                "default features",
            ),
            // a dependency's list turns one on
            (
                HELPER.to_owned(),
                dep("citar-jobs", r#""all-test""#),
                JOBS.to_owned(),
                "",
                "in its dependency",
            ),
            // a shipping feature forwards to one
            (
                format!(r#"{HELPER}, "fast": ["citar-jobs/test-jobs"]"#),
                jobs_dep.clone(),
                JOBS.to_owned(),
                "",
                "feature `fast`",
            ),
            // citar-py forwards outside its test-ops
            (
                HELPER.to_owned(),
                jobs_dep.clone(),
                JOBS.to_owned(),
                r#""wheel": ["citar-jobs?/test-update-key"]"#,
                "feature `wheel`",
            ),
        ];
        for (helper, helper_deps, jobs, py, why) in cases {
            let m = meta(&helper, &helper_deps, &jobs, py, &dep("citar-jobs", ""));
            let found = check_features(&m);
            assert_eq!(found.len(), 1, "{why}: {found:?}");
            assert!(found[0].message.contains(why), "{why}: {found:?}");
        }
    }

    #[test]
    fn release_and_artifact_lines_may_not_name_a_test_feature() {
        let release = "on:\n  push:\njobs:\n  build-helper:\n    steps:\n      # dev-guard is for tests\n      - run: cargo build --release --locked -p citar-helper\n      - run: cargo build -p citar-helper --features dev-guard\n      - run: cargo build --all-features\n";
        let found = check_workflow(RELEASE_WORKFLOW, release, None);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].message.starts_with(".github/workflows/release.yml:8:"), "{found:?}");
        assert!(found[1].message.contains("`--all-features`"), "{found:?}");
        let helper = "jobs:\n  test:\n    steps:\n      - run: cargo nextest run -p citar-helper --features test-faults\n  artifacts:\n    steps:\n      - run: cargo build --release -p citar-helper\n      - run: cargo build -p citar-helper -F test-update-key\n";
        let found = check_workflow(HELPER_WORKFLOW, helper, Some(ARTIFACT_JOB));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains(":8 (job artifacts)"), "{found:?}");
    }

    /// Any indentation of the jobs, a commented `jobs:`, the workflow-wide `env:` and
    /// `defaults:`, and feature lists from variables (4-01's fix round).
    #[test]
    fn helper_yml_is_read_whatever_its_layout() {
        let four = "on:\n    push:\njobs:   # the jobs\n    test:\n        steps:\n            - run: cargo test --features test-faults\n    artifact-linux:\n        steps:\n            - run: cargo build -p citar-helper --features dev-guard\n";
        let found = check_workflow(HELPER_WORKFLOW, four, Some(ARTIFACT_JOB));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.contains(":9 (job artifact-linux): names `dev-guard`"),
            "{found:?}"
        );
        let wide = "env:\n  FEATURES: test-update-key\ndefaults:\n  run:\n    shell: bash --all-features\njobs:\n  artifact:\n    steps:\n      - run: cargo build --release\n";
        let found: Vec<String> = check_workflow(HELPER_WORKFLOW, wide, Some(ARTIFACT_JOB))
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains(":2 (workflow env): names `test-update-key`"), "{found:?}");
        assert!(found[1].contains(":5 (workflow defaults): names `--all-features`"), "{found:?}");
        let variables = "jobs:\n  test:\n    steps:\n      - run: cargo test --features $FEATURES\n  artifacts:\n    strategy:\n      matrix:\n        features: [\"\"]\n    steps:\n      - run: cargo build --features $FEATURES\n      - run: cargo build --features=${{ matrix.features }}\n      - run: cargo build -F \"$F\"\n      - run: cargo build --features conformance\n      - run: cargo build --features gpu # $not a list\n";
        let found: Vec<String> = check_workflow(HELPER_WORKFLOW, variables, Some(ARTIFACT_JOB))
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(found.len(), 4, "{found:?}");
        for (k, line) in [(0, ":10 "), (1, ":11 "), (2, ":12 ")] {
            assert!(found[k].contains(line) && found[k].contains("from a variable"), "{found:?}");
        }
        assert!(found[3].contains(":13 (job artifacts): names `conformance`"), "{found:?}");
        let release = "jobs:\n  wheels:\n    steps:\n      - run: maturin build -F $EXTRA\n";
        assert_eq!(check_workflow(RELEASE_WORKFLOW, release, None).len(), 1);
    }

    /// The workflows as they are pass.
    #[test]
    fn the_workflows_as_they_are_pass() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let text =
            std::fs::read_to_string(root.join(RELEASE_WORKFLOW)).map_err(|e| e.to_string())?;
        assert_eq!(check_workflow(RELEASE_WORKFLOW, &text, None), []);
        Ok(())
    }
}
