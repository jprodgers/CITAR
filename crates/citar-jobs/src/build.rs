//! The jobs build (design/phase4-helper.md P4.4.5): the compatibility key every job carries.
//!
//! `jobs_build = hex12(blake3("CITAR-JOBS-BUILD" ‖ build_id ‖ SIM_CODE ‖ JOBS_CODE ‖ flavour))`.
//! The build id (`citar_bot::build_id`) covers the engine, the bot and the ruleset; the runner
//! changes games too and this crate builds the results, so their content codes join it; and the
//! build's [`Flavour`] does, because a `ci` and a `release` build can compute different outcomes
//! (a `debug_assert!` that fires is a crash in one and nothing in the other). `hello.helper`,
//! `job_offer`, the claim, `cpu_jobs` and the local executor's report carry it; results are only
//! ever compared within one jobs build, and a job offered to a helper of another is rejected
//! (`build`).
//!
//! **The flavour:** whether the build has debug assertions and overflow checks, and whether the
//! engine compiled its checks (its `checks` feature, or debug assertions: what
//! `DebugOptions::default()` reports). Debug assertions are this crate's `cfg!`, compiled with
//! the engine's profile. Overflow checks have no stable `cfg`, so they are observed once per
//! process: an addition that overflows panics with them and wraps without them. The panic is
//! caught, and the panic hook is set aside for that one call so it prints nothing (a panic on
//! another thread in that instant would print nothing either; its game's crash record is
//! unchanged).

use std::panic::{catch_unwind, set_hook, take_hook};
use std::sync::OnceLock;

use citar_engine::game::DebugOptions;
use citar_engine::rules::Ruleset;

use crate::JOBS_CODE;
use crate::spec::hex;

/// What the jobs build starts with.
pub const JOBS_BUILD_DOMAIN: &[u8] = b"CITAR-JOBS-BUILD";

/// How a build was compiled, as far as it can change a game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Flavour {
    pub debug_assertions: bool,
    pub overflow_checks: bool,
    /// The engine compiled its invariant checks.
    pub checks: bool,
}

impl Flavour {
    /// This build's.
    #[must_use]
    pub fn this_build() -> Self {
        Self {
            debug_assertions: cfg!(debug_assertions),
            overflow_checks: overflow_checks(),
            checks: DebugOptions::default().invariants,
        }
    }

    /// The bytes the jobs build hashes: `debug_assertions=1;overflow_checks=0;checks=1`.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let b = |x: bool| if x { '1' } else { '0' };
        format!(
            "debug_assertions={};overflow_checks={};checks={}",
            b(self.debug_assertions),
            b(self.overflow_checks),
            b(self.checks)
        )
        .into_bytes()
    }

    /// `ci` for a build with debug assertions (the profile every test and QA run uses), else
    /// `release`: a label for people; the jobs build hashes the flavour itself.
    #[must_use]
    pub const fn profile(&self) -> &'static str {
        if self.debug_assertions { "ci" } else { "release" }
    }
}

/// Whether this build checks integer overflow: observed once per process.
fn overflow_checks() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let hook = take_hook();
        set_hook(Box::new(|_| {}));
        let wrapped = catch_unwind(|| {
            let x = std::hint::black_box(u8::MAX);
            std::hint::black_box(x + std::hint::black_box(1u8))
        });
        set_hook(hook);
        wrapped.is_err()
    })
}

/// The jobs build of a build whose build id is `build_id`, whose runner's and jobs crate's
/// content codes are `sim_code` and `jobs_code`, of `flavour`: 12 hex digits.
#[must_use]
pub fn jobs_build_of(build_id: &str, sim_code: &str, jobs_code: &str, flavour: Flavour) -> String {
    let mut h = blake3::Hasher::new();
    let mut put = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    put(JOBS_BUILD_DOMAIN);
    put(build_id.as_bytes());
    put(sim_code.as_bytes());
    put(jobs_code.as_bytes());
    put(&flavour.bytes());
    hex(&h.finalize().as_bytes()[..6])
}

/// This build's jobs build, under `rules` (whose id the build id covers).
#[must_use]
pub fn jobs_build(rules: &Ruleset) -> String {
    jobs_build_of(
        &citar_bot::build_id(rules),
        citar_sim::SIM_CODE,
        JOBS_CODE,
        Flavour::this_build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate 4: the jobs build changes with the runner's code, the jobs crate's code, the build
    /// id and each part of the flavour, and only with them.
    #[test]
    fn the_jobs_build_follows_each_of_its_inputs() {
        let f = Flavour { debug_assertions: true, overflow_checks: true, checks: true };
        let base = jobs_build_of("0b412a18fe95", "1111111111111111", "2222222222222222", f);
        assert_eq!(base.len(), 12);
        assert!(base.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(base, jobs_build_of("0b412a18fe95", "1111111111111111", "2222222222222222", f));
        let release = Flavour { debug_assertions: false, overflow_checks: false, checks: false };
        let variants = [
            jobs_build_of("0b412a18fe96", "1111111111111111", "2222222222222222", f),
            jobs_build_of("0b412a18fe95", "1111111111111112", "2222222222222222", f),
            jobs_build_of("0b412a18fe95", "1111111111111111", "2222222222222223", f),
            jobs_build_of("0b412a18fe95", "1111111111111111", "2222222222222222", release),
            jobs_build_of(
                "0b412a18fe95",
                "1111111111111111",
                "2222222222222222",
                Flavour { debug_assertions: false, ..f },
            ),
            jobs_build_of(
                "0b412a18fe95",
                "1111111111111111",
                "2222222222222222",
                Flavour { overflow_checks: false, ..f },
            ),
            jobs_build_of(
                "0b412a18fe95",
                "1111111111111111",
                "2222222222222222",
                Flavour { checks: false, ..f },
            ),
        ];
        for (n, v) in variants.iter().enumerate() {
            assert_ne!(*v, base, "variant {n}");
        }
        // The parts are length-prefixed: moving a digit from one part to the next is another
        // build.
        assert_ne!(jobs_build_of("0b412a18fe9", "51111111111111111", "2222222222222222", f), base);
    }

    #[test]
    fn this_builds_flavour_is_its_profiles() {
        let f = Flavour::this_build();
        assert_eq!(f.debug_assertions, cfg!(debug_assertions));
        // The workspace's profiles turn overflow checks on with debug assertions (dev, ci) and
        // off without them (release).
        assert_eq!(f.overflow_checks, cfg!(debug_assertions), "{f:?}");
        assert!(!cfg!(debug_assertions) || f.checks, "a debug build has the engine's checks");
        assert_eq!(f.profile(), if cfg!(debug_assertions) { "ci" } else { "release" });
        assert_eq!(Flavour::this_build(), f, "observed once");
        let rules = Ruleset::shared();
        assert_eq!(jobs_build(rules), jobs_build(rules));
        assert_eq!(jobs_build(rules).len(), 12);
    }
}
