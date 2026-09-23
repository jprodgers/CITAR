//! One version for the whole of CITAR: the Rust workspace and the Python package move together.
//!
//! A release bumps `citar/__init__.py` and `[workspace.package] version` in the root
//! `Cargo.toml` in the same commit; a bump in only one place fails here.

use super::Finding;
use super::metadata::Metadata;
use std::collections::BTreeMap;
use std::path::Path;

const CHECK: &str = "version";
const PYTHON_FILE: &str = "citar/__init__.py";

pub fn check(root: &Path, meta: &Metadata) -> Result<Vec<Finding>, String> {
    let path = root.join(PYTHON_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let python = python_version(&text)
        .ok_or_else(|| format!("{PYTHON_FILE} has no `__version__ = \"...\"` line"))?;
    let members: Vec<(&str, &str)> =
        meta.members().map(|p| (p.name.as_str(), p.version.as_str())).collect();
    Ok(compare(python, &members))
}

/// The value of the `__version__ = "x.y.z"` assignment.
pub fn python_version(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("__version__")?.trim_start().strip_prefix('=')?.trim();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = &rest[1..];
        Some(&inner[..inner.find(quote)?])
    })
}

/// One finding per version that differs from Python's, naming the packages that carry it.
pub fn compare(python: &str, members: &[(&str, &str)]) -> Vec<Finding> {
    let mut wrong: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, version) in members {
        if *version != python {
            wrong.entry(version).or_default().push(name);
        }
    }
    wrong
        .into_iter()
        .map(|(version, names)| {
            Finding::new(
                CHECK,
                format!(
                    "{} at version {version}, but {PYTHON_FILE} says {python}: bump \
                     [workspace.package] version in Cargo.toml and __version__ together",
                    names.join(", ")
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_python_version() {
        let text = "\"\"\"Doc.\"\"\"\n\n__version__ = \"0.1.5\"\n__all__ = [\"__version__\"]\n";
        assert_eq!(python_version(text), Some("0.1.5"));
        assert_eq!(python_version("__version__='1.2.3'"), Some("1.2.3"));
        assert_eq!(python_version("__all__ = ['__version__']"), None);
        assert_eq!(python_version("__version_info__ = (1, 2)"), None);
    }

    #[test]
    fn the_real_file_has_a_version() {
        let text = include_str!("../../../citar/__init__.py");
        assert!(python_version(text).is_some());
    }

    #[test]
    fn agreeing_versions_pass() {
        assert_eq!(compare("0.1.6", &[("citar-engine", "0.1.6"), ("xtask", "0.1.6")]), []);
    }

    #[test]
    fn a_bump_in_one_place_fails() {
        // Cargo.toml bumped, Python not.
        let found = compare("0.1.5", &[("citar-engine", "0.1.6"), ("xtask", "0.1.6")]);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.starts_with("citar-engine, xtask at version 0.1.6"));
        // Python bumped, Cargo.toml not.
        assert_eq!(compare("0.1.7", &[("citar-engine", "0.1.6")]).len(), 1);
    }
}
