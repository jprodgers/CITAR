//! What this build of the helper is: `citar-helper --version` and `--self-test`, and the hello's
//! `helper` block (PROTOCOL.md: `version`, `build`, `jobs_build`, `target`, `label`). The jobs
//! build is the job host's (P4.4.5); the hello names it once the helper runs jobs (4-10, until
//! when it says none, so no job is offered). `--version` adds the machine key's fingerprint when
//! the home holds a key (`app`).
//!
//! **A test build that calls itself another version** (the `test-update-key` feature only,
//! design/phase4-helper.md P4.3.8 and package 4-13): `CITAR_HELPER_VERSION` at compile time is
//! the version it reports (`0.1.6-test.2`), so the update tests have an older and a newer build of
//! one tree; `CITAR_HELPER_JOBS_SALT` at compile time makes its jobs build another, as another
//! release's would be (its job host is told the same one), so the tests can show that a job host
//! of the build before a swap is never used. A build without the feature reads neither.
//!
//! [`BuildInfo::read_line`] reads a `--version` line back: the update's step 5 checks a staged
//! binary's `--self-test` output against the signed manifest with it.

use std::sync::OnceLock;

use citar_engine::rules::Ruleset;

use crate::protocol::PROTOCOL;

/// The Rust target triple this binary was built for (`build.rs`).
pub const TARGET: &str = env!("CITAR_HELPER_TARGET");

/// The helper's version: CITAR's, or in a `test-update-key` build the one `CITAR_HELPER_VERSION`
/// gave at compile time.
#[cfg(feature = "test-update-key")]
pub const VERSION: &str = match option_env!("CITAR_HELPER_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The helper's version: CITAR's.
#[cfg(not(feature = "test-update-key"))]
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This build's jobs build (P4.4.5): citar-jobs', or in a `test-update-key` build compiled with
/// `CITAR_HELPER_JOBS_SALT`, another one made from it and the salt. The hello, the offers' check,
/// the claims and the job host (`citar-helper job-host` is told it) all use this one.
#[must_use]
pub fn jobs_build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(|| salted(citar_jobs::jobs_build(Ruleset::shared()), salt()))
}

/// The test salt compiled in, if any.
#[cfg(feature = "test-update-key")]
fn salt() -> Option<&'static str> {
    option_env!("CITAR_HELPER_JOBS_SALT").filter(|s| !s.is_empty())
}

#[cfg(not(feature = "test-update-key"))]
const fn salt() -> Option<&'static str> {
    None
}

/// `base`, or with a salt another 12 hex digits made from both.
fn salted(base: String, salt: Option<&str>) -> String {
    let Some(salt) = salt else { return base };
    let mut h = blake3::Hasher::new();
    h.update(b"CITAR-HELPER-TEST-JOBS-SALT\0");
    h.update(base.as_bytes());
    h.update(b"\0");
    h.update(salt.as_bytes());
    h.finalize().to_hex()[..12].to_owned()
}

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
    /// The jobs compatibility key (P4.4.5): what this binary's job host computes with.
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
        jobs_build: Some(jobs_build().to_owned()),
    }
}

/// What a `--version` line says of a build (another binary's: [`BuildInfo::read_line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
    pub version: String,
    pub build: String,
    pub jobs_build: String,
    pub protocol: u64,
    pub target: String,
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

    /// Reads a [`BuildInfo::line`] back (the label, and a machine key's fingerprint after it, are
    /// left out); `None` when the line is not one.
    #[must_use]
    pub fn read_line(line: &str) -> Option<Described> {
        let rest = line.trim().strip_prefix("citar-helper ")?;
        let (version, inner) = rest.split_once(" (")?;
        let inner = inner.split_once(')').map_or(inner, |(i, _)| i);
        let mut build = None;
        let mut jobs_build = None;
        let mut protocol = None;
        let mut target = None;
        for part in inner.split(", ") {
            if let Some(v) = part.strip_prefix("jobs build ") {
                jobs_build = Some(v.to_owned());
            } else if let Some(v) = part.strip_prefix("build ") {
                build = Some(v.to_owned());
            } else if let Some(v) = part.strip_prefix("protocol ") {
                protocol = v.parse().ok();
            } else if let Some(v) = part.strip_prefix("target ") {
                target = Some(v.to_owned());
            }
        }
        Some(Described {
            version: version.to_owned(),
            build: build?,
            jobs_build: jobs_build?,
            protocol: protocol?,
            target: target?,
        })
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

    #[test]
    fn a_version_line_reads_back() {
        let info = build_info();
        let read = BuildInfo::read_line(&format!("{}, machine key 0123456789abcdef", info.line()))
            .expect("reads");
        assert_eq!(read.version, VERSION);
        assert_eq!(read.build, info.build);
        assert_eq!(Some(read.jobs_build), info.jobs_build);
        assert_eq!(read.protocol, 2);
        assert_eq!(read.target, TARGET);
        assert_eq!(BuildInfo::read_line("canary: ok"), None);
        assert!(citar_jobs::update::newer(VERSION, "0.0.0") == Some(true), "{VERSION} is semver");
    }

    #[test]
    fn a_salt_makes_another_jobs_build_and_none_keeps_it() {
        let base = "0123456789ab".to_owned();
        assert_eq!(salted(base.clone(), None), base);
        let a = salted(base.clone(), Some("test.2"));
        assert_eq!(a.len(), 12);
        assert_ne!(a, base);
        assert_ne!(a, salted(base, Some("test.3")));
    }
}
