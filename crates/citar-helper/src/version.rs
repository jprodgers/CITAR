//! What this build of the helper is: `citar-helper --version`, and the hello's `helper` block
//! (PROTOCOL.md: `version`, `build`, `jobs_build`, `target`, `label`). The jobs build is `none`
//! until 4-07 gives the helper one; `--version` adds the machine key's fingerprint when the home
//! holds a key (`app`).

use citar_engine::rules::Ruleset;

use crate::protocol::PROTOCOL;

/// The Rust target triple this binary was built for (`build.rs`).
pub const TARGET: &str = env!("CITAR_HELPER_TARGET");

/// The helper's version: CITAR's.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This build, as the hello and `--version` report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    /// CITAR's version.
    pub version: &'static str,
    /// The engine build id over the ruleset compiled in (`citar_bot::build_id`).
    pub build: String,
    /// The Rust target triple.
    pub target: &'static str,
    /// `git describe` when the build set one, else `unknown`.
    pub label: &'static str,
    /// The protocol it speaks.
    pub protocol: u32,
    /// The jobs compatibility key (P4.4.5); `None` before 4-07, and never offered a job.
    pub jobs_build: Option<String>,
}

/// This build.
#[must_use]
pub fn build_info() -> BuildInfo {
    let info = citar_bot::build_info(Ruleset::shared());
    BuildInfo {
        version: VERSION,
        build: info.build_id,
        target: TARGET,
        label: info.label,
        protocol: PROTOCOL,
        jobs_build: None,
    }
}

impl BuildInfo {
    /// The `--version` text: one line.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "citar-helper {} (build {}, jobs build {}, protocol {}, target {}, {})",
            self.version,
            self.build,
            self.jobs_build.as_deref().unwrap_or("none"),
            self.protocol,
            self.target,
            self.label
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_names_the_build_the_protocol_and_the_target() {
        let info = build_info();
        assert_eq!(info.build.len(), 12, "{info:?}");
        assert!(info.build.bytes().all(|b| b.is_ascii_hexdigit()), "{info:?}");
        assert_eq!(info.protocol, 2);
        assert!(!info.target.is_empty() && info.target != "unknown", "{info:?}");
        let line = info.line();
        assert!(
            line.starts_with(&format!("citar-helper {VERSION} (build {}", info.build)),
            "{line}"
        );
        assert!(line.contains("protocol 2") && line.contains(TARGET), "{line}");
    }
}
