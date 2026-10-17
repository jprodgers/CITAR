//! CITAR's CPU jobs (design/phase4-helper.md P4.4): what the helper and the server must compute
//! the same way, once.
//!
//! - [`spec`]: the closed vocabulary of work ([`Kind`]: lab, ranking and reference games), each
//!   spec typed and bounded ([`LabSpec`], [`ReferenceSpec`]), checked by [`check_spec`], with its
//!   canonical bytes and [`SpecHash`] (P4.4.1);
//! - [`lab`]: the lab game's result, [`LabResult`], built in Rust from the game as
//!   `citar/lab.py`'s `play()` built it in Python from the events it heard (P4.4.2);
//! - [`reference`](mod@reference): one game of citar-sim's statistical baseline as a job,
//!   [`ReferenceResult`] (the baseline's line without its times);
//! - [`job`]: [`Job`], one job played step by step under its chain of round digests, never with a
//!   wall-clock budget, a panic its crash outcome; [`run`] plays one to its end under a
//!   [`Control`] that may stop it between seat steps;
//! - [`hash`]: [`result_hash`] over the typed deterministic result and [`chain_check`] over the
//!   chain (P4.4.4);
//! - [`claim`](mod@claim): the [`Claim`] a helper signs over a run's result, the bytes it signs
//!   (4-10), the server's check of it all ([`verify_claim`]: the signature, the claim against the
//!   server's records, the result and the chain; 4-11) and the keyed sample that decides which
//!   results a replica checks ([`sample_fraction`]);
//! - [`fit`]: [`result_fits`], whether a result answers its spec (what the spec decides of it,
//!   its turns against the chain's rounds), and [`rebased`], a golden result under this build's
//!   id;
//! - [`build`]: the jobs build, [`jobs_build`], the compatibility key every job carries (P4.4.5);
//! - [`canary`](mod@canary): the two denormal checks and the self-check a job host runs before it
//!   takes work;
//! - [`golden`]: the `jobs` golden set (`golden/jobs.json`), embedded for the self-check;
//! - [`host`]: the job host (P4.4.3), the one program that plays jobs under limits for the helper
//!   and the server, its JSON-lines protocol, and its parent side ([`host::parent`]: starting it,
//!   under a `systemd --user` scope on Linux, and classifying its death);
//! - [`limits`]: what the host holds per OS (P4.3.6), and how hard;
//! - [`sys`]: the operating-system calls no safe wrapper makes, and the counting allocator
//!   ([`Counting`]) a program that runs a job host declares as its `#[global_allocator]`.
//!
//! The workspace denies `unsafe_code`; [`sys`]'s two per-OS files allow it, each block with its
//! `// SAFETY:` comment, and `cargo xtask check` refuses an `unsafe` token anywhere else in this
//! crate and anywhere in citar-helper, which calls this module's safe functions (P4.3.1).
//!
//! Nothing here decides a game: the engine and the bot do, under their strict rules. What this
//! crate adds to a result (how it is summed up, hashed and checked) is covered by its content
//! code, [`JOBS_CODE`], which the jobs build hashes with the runner's and the build id.

pub mod build;
pub mod canary;
pub mod claim;
pub mod fit;
pub mod golden;
pub mod hash;
pub mod host;
pub mod job;
pub mod lab;
pub mod limits;
pub mod reference;
pub mod spec;
pub mod sys;

pub use self::build::{Flavour, jobs_build, jobs_build_of};
pub use self::canary::{CanaryFailure, Inject, canary, canary_injected, self_check};
pub use self::claim::{
    CLAIM_DOMAIN, Claim, ClaimRefused, ClaimVerdict, Expected, sample_fraction, verify_claim,
};
pub use self::fit::{FitError, rebased, result_fits, result_fits_value};
pub use self::hash::{
    Chain, ChainError, JobResult, ResultError, chain_check, chain_check_value, fold, parted_at,
    result_hash,
};
pub use self::job::{Control, Flow, Job, Output, Progress, RunError, Unattended, run};
pub use self::lab::{LabCrash, LabFacts, LabOutcome, LabPlayer, LabResult};
pub use self::limits::{Hardness, Held};
pub use self::reference::{ReferenceFacts, ReferenceOutcome, ReferenceResult};
pub use self::spec::{
    Checked, Kind, LabSeat, LabSpec, ReferenceSpec, Spec, SpecError, SpecHash, check_spec,
};
pub use self::sys::alloc::Counting;

// The unit tests count as a job host does, so the host's tests in this crate see real peaks.
#[cfg(test)]
#[global_allocator]
static COUNTING: Counting = Counting;

/// The jobs crate's content code (`build.rs`): 16 hex digits over its sources, the golden set,
/// its version and the locked versions of what it links.
pub const JOBS_CODE: &str = env!("CITAR_JOBS_CODE");

// The content code's helper, here so that its tests run with the jobs crate's.
#[cfg(test)]
#[path = "../content_code.rs"]
mod build_code;

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_code_helper_is_the_engines() {
        let norm = |s: &str| s.replace("\r\n", "\n");
        assert_eq!(
            norm(include_str!("../content_code.rs")),
            norm(include_str!("../../citar-engine/content_code.rs")),
            "crates/citar-jobs/content_code.rs must stay a copy of the engine's"
        );
        assert_eq!(super::JOBS_CODE.len(), 16);
    }
}
