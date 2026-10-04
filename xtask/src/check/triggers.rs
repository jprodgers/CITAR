//! rust.yml runs these checks, and its `paths` filters decide when it runs.
//!
//! A pull request that changes only files no filter matches never runs `cargo xtask check`, so a
//! file the checks read but the filters miss can break a rule unseen: until 2-06b's fix round
//! `pyproject.toml` was such a file, and a pull request could have listed `legacy` in its maturin
//! features without any check running. Every file the checks read must therefore be matched by
//! the filter of each event that has one (an event without `paths` runs on every change). So
//! must the files of [`TEST_READS`], which rust.yml's tests read from outside the trees its
//! filters already take whole.
//!
//! The workflow is read as text, not as YAML: the `on:` block's events at two spaces, their
//! `paths:` at four and the patterns at six, as rust.yml writes them. A pattern is GitHub's glob,
//! `*` within one name and `**` across folders; negated (`!`) patterns and `paths-ignore` are not
//! read, and a workflow that uses them is reported rather than guessed at.

use std::collections::BTreeMap;
use std::path::Path;

use super::Finding;

const CHECK: &str = "triggers";

/// The workflow that runs `cargo xtask check`.
pub const WORKFLOW: &str = ".github/workflows/rust.yml";

/// Data files a Rust test reads from outside the trees rust.yml's filters take whole (`crates/`,
/// `tests/rules/`, `refcheck/`, `citar/data/`), which another tool rewrites on its own:
/// citar-testkit's bot params test holds `clean()` to 2-00b's clean-params table, and
/// `python -m tests.test_bot_params --record` re-records it after a change to Python's profiles.
/// A change to one alone must run the tests.
pub const TEST_READS: &[&str] = &["tests/data/clean_params_cases.json"];

pub fn check(root: &Path, reads: &[String]) -> Result<Vec<Finding>, String> {
    let path = root.join(WORKFLOW);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(check_text(&text, reads))
}

/// The findings for a workflow with this text, against the files the checks read.
pub fn check_text(text: &str, reads: &[String]) -> Vec<Finding> {
    let say = |message: String| Finding::new(CHECK, format!("{WORKFLOW}: {message}"));
    let filters = match path_filters(text) {
        Ok(filters) => filters,
        Err(e) => return vec![say(e)],
    };
    let mut out = Vec::new();
    for (event, patterns) in &filters {
        for file in reads {
            if !patterns.iter().any(|p| glob_matches(p, file)) {
                out.push(say(format!(
                    "the `{event}` paths filter does not match `{file}`, which cargo xtask check \
                     or a test reads, so a change to it alone would never run them: add it"
                )));
            }
        }
    }
    out
}

/// Each event of the `on:` block that has a `paths` filter, with its patterns.
fn path_filters(text: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut filters: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut in_on = false;
    let mut found_on = false;
    let mut event: Option<String> = None;
    let mut in_paths = false;
    for line in text.lines() {
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indent = line.len() - body.len();
        if indent == 0 {
            in_on = body.trim_end() == "on:";
            found_on |= in_on;
            event = None;
            in_paths = false;
            continue;
        }
        if !in_on {
            continue;
        }
        let key = body.split('#').next().unwrap_or("").trim_end();
        match indent {
            2 => {
                event = key.strip_suffix(':').map(str::to_owned);
                in_paths = false;
            }
            4 => {
                in_paths = key == "paths:";
                if key.starts_with("paths-ignore") {
                    return Err("uses `paths-ignore`, which this check does not read".to_owned());
                }
                if !in_paths && key.starts_with("paths:") {
                    return Err(format!(
                        "`{key}` lists its paths inline, which this check does not read"
                    ));
                }
                if in_paths && let Some(event) = &event {
                    filters.entry(event.clone()).or_default();
                }
            }
            6 if in_paths => {
                let Some(item) = key.strip_prefix("- ") else {
                    return Err(format!("`{body}` is not a list item of a `paths` filter"));
                };
                let pattern = item.trim().trim_matches('"').trim_matches('\'');
                if pattern.starts_with('!') {
                    return Err(format!(
                        "the negated pattern `{pattern}` is one this check does not read"
                    ));
                }
                if let Some(event) = &event {
                    filters.entry(event.clone()).or_default().push(pattern.to_owned());
                }
            }
            _ => {}
        }
    }
    if found_on { Ok(filters) } else { Err("has no `on:` block".to_owned()) }
}

/// GitHub's filter glob: `**` matches anything, `*` anything but `/`, every other byte itself.
fn glob_matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| go(rest, &s[i..])),
            [b'*', rest @ ..] => {
                let name = s.iter().position(|&c| c == b'/').unwrap_or(s.len());
                (0..=name).any(|i| go(rest, &s[i..]))
            }
            [c, rest @ ..] => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW_TEXT: &str = "\
name: rust

on:
  push:
    branches: [main]
    paths:
      - \"crates/**\"
      # a comment between the patterns
      - \"Cargo.*\"
      - 'pyproject.toml'
  pull_request:
    paths:
      - \"crates/**\"
      - \"Cargo.*\"
  workflow_dispatch:
    inputs:
      x:
        description: not a filter

concurrency:
  group: rust
";

    fn reads(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| (*f).to_owned()).collect()
    }

    #[test]
    fn the_filters_are_read_per_event() {
        let filters = path_filters(WORKFLOW_TEXT).unwrap();
        assert_eq!(filters.keys().collect::<Vec<_>>(), ["pull_request", "push"]);
        assert_eq!(filters["push"], ["crates/**", "Cargo.*", "pyproject.toml"]);
        assert_eq!(filters["pull_request"], ["crates/**", "Cargo.*"]);
    }

    #[test]
    fn a_file_one_filter_misses_is_reported_for_that_event() {
        let found = check_text(
            WORKFLOW_TEXT,
            &reads(&["crates/citar-py/Cargo.toml", "Cargo.lock", "pyproject.toml"]),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .message
                .contains("`pull_request` paths filter does not match `pyproject.toml`")
        );
    }

    #[test]
    fn an_event_without_a_filter_runs_on_everything() {
        let text = "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n";
        assert!(check_text(text, &reads(&["anything.toml"])).is_empty());
    }

    #[test]
    fn what_the_check_cannot_read_is_reported() {
        let ignore = "on:\n  push:\n    paths-ignore:\n      - \"docs/**\"\n";
        let negated = "on:\n  push:\n    paths:\n      - \"crates/**\"\n      - \"!crates/x/**\"\n";
        let inline = "on:\n  push:\n    paths: [\"crates/**\"]\n";
        for (text, why) in [
            (ignore, "paths-ignore"),
            (negated, "negated pattern"),
            (inline, "inline"),
            ("name: x\n", "no `on:`"),
        ] {
            let found = check_text(text, &reads(&["Cargo.toml"]));
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(found[0].message.contains(why), "{found:?}");
        }
    }

    #[test]
    fn globs_match_as_github_matches_them() {
        assert!(glob_matches("crates/**", "crates/citar-engine/src/lib.rs"));
        assert!(glob_matches("Cargo.*", "Cargo.lock"));
        assert!(!glob_matches("Cargo.*", "Cargo/x.lock"));
        assert!(glob_matches("citar/data/**", "citar/data/ruleset/techs.json"));
        assert!(!glob_matches("citar/data/**", "citar/database.py"));
        assert!(glob_matches("pyproject.toml", "pyproject.toml"));
        assert!(!glob_matches("pyproject.toml", "pyproject.toml.bak"));
        assert!(glob_matches("*.toml", "rustfmt.toml"));
        assert!(!glob_matches("*.toml", "xtask/check.toml"));
    }

    #[test]
    fn rust_yml_runs_on_every_file_the_checks_read() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let reads = super::super::reads(&super::super::metadata::Metadata::load(&root).unwrap());
        assert!(reads.contains(&"pyproject.toml".to_owned()), "{reads:?}");
        assert!(reads.contains(&"crates/citar-py/Cargo.toml".to_owned()), "{reads:?}");
        for file in TEST_READS {
            assert!(reads.contains(&(*file).to_owned()), "{file}: {reads:?}");
            assert!(root.join(file).is_file(), "{file} is gone: take it out of TEST_READS");
        }
        let found = check(&root, &reads).unwrap();
        assert!(found.is_empty(), "{found:?}");
        // The check can fail on the real file: without its lines for a file the checks read, or
        // for one a test reads, both events miss it.
        let text = std::fs::read_to_string(root.join(WORKFLOW)).unwrap();
        for file in ["pyproject.toml", TEST_READS[0]] {
            let without: String = text
                .lines()
                .filter(|l| !l.contains(&format!("\"{file}\"")))
                .map(|l| format!("{l}\n"))
                .collect();
            let found = check_text(&without, &reads);
            assert_eq!(found.len(), 2, "{found:?}");
            assert!(found.iter().all(|f| f.message.contains(&format!("`{file}`"))), "{found:?}");
        }
    }
}
