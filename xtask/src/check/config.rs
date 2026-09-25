//! `xtask/check.toml`: the switches later packages turn on.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub pending: PendingConfig,
    pub not_ported: NotPortedConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingConfig {
    /// Packages whose gates have passed; their Pending stages must be gone.
    pub done: Vec<String>,
    /// Any Pending stage fails (from 1c-10).
    pub forbid_all: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotPortedConfig {
    /// Any NotPorted fails (from 1e-04).
    pub forbid: bool,
}

impl Config {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("xtask/check.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_file_parses() {
        let text = include_str!("../../check.toml");
        let config = Config::parse(text).expect("xtask/check.toml parses");
        assert!(config.pending.done.iter().any(|p| p == "1a-01"));
        // Package 1c-10's gate: from here on any Pending stage fails the check.
        assert!(config.pending.forbid_all, "every stage must be real from 1c-10 on");
    }

    #[test]
    fn unknown_switches_are_refused() {
        let text = "[pending]\ndone = []\nforbid_all = false\nforbid_some = true\n\
                    [not_ported]\nforbid = false\n";
        assert!(Config::parse(text).is_err());
    }
}
