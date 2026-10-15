//! Whether a machine computes what every other does, checked before a job host takes work
//! (design/phase4-helper.md P4.4.3, DESIGN.md 7.5). Either failing makes the host
//! `verifiable: false`, which refuses every job (`unverifiable`) rather than producing results
//! nobody could check: a target or environment that computes differently is never offered work,
//! so it is never quarantined for it.
//!
//! - **The canary** ([`canary`]): denormals are neither flushed to zero (a tiny number halved is
//!   not 0) nor read as zero (a denormal times four is the tiny number again). A process whose
//!   floating-point environment flushes them (a library that set FTZ or DAZ on x86, a GPU
//!   driver's thread) plays another game. [`canary_injected`] is the test switch: each check's
//!   computed value can be replaced by what a flushing machine would compute.
//! - **The self-check** ([`self_check`]): the golden set's shortest job ([`golden::SELF_CHECK`])
//!   played here, its result hash and chain head compared with the committed ones.
//!
//! [`golden::SELF_CHECK`]: crate::golden::SELF_CHECK

use std::hint::black_box;

use citar_engine::rules::Ruleset;

use crate::golden::{GoldenSet, SELF_CHECK};
use crate::job::{Unattended, run};
use crate::spec::check_spec;

/// Which canary check failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CanaryFailure {
    /// The smallest normal number halved gave 0: denormals are flushed to zero.
    #[error("denormals are flushed to zero")]
    Flushed,
    /// A denormal times four did not give the smallest normal number: denormals are read as 0.
    #[error("denormals are read as zero")]
    ReadAsZero,
}

/// What a check computes, replaced: the canary's test switch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Inject {
    /// In place of `MIN_POSITIVE / 2`.
    pub halved: Option<f64>,
    /// In place of `(MIN_POSITIVE / 4) * 4`.
    pub restored: Option<f64>,
}

/// The canary's two checks, as this machine computes them.
///
/// # Errors
/// The first check that fails.
pub fn canary() -> Result<(), CanaryFailure> {
    canary_injected(&Inject::default())
}

/// The canary with each check's computed value replaced where `inject` gives one.
///
/// # Errors
/// The first check that fails.
pub fn canary_injected(inject: &Inject) -> Result<(), CanaryFailure> {
    // Each operand goes through black_box, so the operation happens at run time, in this
    // thread's floating-point environment, not in the compiler's.
    let halved = inject.halved.unwrap_or_else(|| black_box(f64::MIN_POSITIVE) / 2.0);
    if halved.abs().to_bits() == 0 {
        return Err(CanaryFailure::Flushed);
    }
    let quarter = black_box(f64::MIN_POSITIVE / 4.0);
    let restored = inject.restored.unwrap_or_else(|| quarter * black_box(4.0));
    if restored.to_bits() != f64::MIN_POSITIVE.to_bits() {
        return Err(CanaryFailure::ReadAsZero);
    }
    Ok(())
}

/// The self-check passed: the job it played and what it gave.
#[derive(Clone, Debug, PartialEq)]
pub struct SelfCheck {
    pub job: &'static str,
    pub result_hash: String,
    pub chain_head: String,
    pub rounds: u32,
    /// How long it took.
    pub wall_s: f64,
}

/// Plays the golden set's shortest job under `rules` and compares its result hash and chain head
/// with the committed ones.
///
/// # Errors
/// What differs, or why the job could not be played.
pub fn self_check(rules: &'static Ruleset) -> Result<SelfCheck, String> {
    let set = GoldenSet::committed()?;
    let job = set.jobs.get(SELF_CHECK).ok_or_else(|| format!("no golden job {SELF_CHECK}"))?;
    let checked = check_spec(rules, job.kind, &job.spec, Some(&job.spec_hash))
        .map_err(|e| format!("{SELF_CHECK}'s spec: {e}"))?;
    let out = run(rules, &checked, &mut Unattended).map_err(|e| format!("{SELF_CHECK}: {e}"))?;
    if out.chain.head != job.chain_head || out.chain.rounds != job.rounds {
        return Err(format!(
            "{SELF_CHECK}'s chain ends at {} after {} rounds, not at the committed {} after {}",
            out.chain.head, out.chain.rounds, job.chain_head, job.rounds
        ));
    }
    if out.result_hash != job.result_hash {
        return Err(format!(
            "{SELF_CHECK}'s result hash is {}, not the committed {}",
            out.result_hash, job.result_hash
        ));
    }
    Ok(SelfCheck {
        job: SELF_CHECK,
        result_hash: out.result_hash,
        chain_head: out.chain.head,
        rounds: out.chain.rounds,
        wall_s: out.wall_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate 7: the canary passes here, and each check fails on the value a flushing machine
    /// would compute.
    #[test]
    fn the_canary_passes_and_each_check_fails_on_an_injected_value() {
        assert_eq!(canary(), Ok(()));
        let flushed = Inject { halved: Some(0.0), ..Inject::default() };
        assert_eq!(canary_injected(&flushed), Err(CanaryFailure::Flushed));
        let negative = Inject { halved: Some(-0.0), ..Inject::default() };
        assert_eq!(canary_injected(&negative), Err(CanaryFailure::Flushed));
        let daz = Inject { restored: Some(0.0), ..Inject::default() };
        assert_eq!(canary_injected(&daz), Err(CanaryFailure::ReadAsZero));
        let off = Inject { restored: Some(f64::MIN_POSITIVE * 2.0), ..Inject::default() };
        assert_eq!(canary_injected(&off), Err(CanaryFailure::ReadAsZero));
        // The real values pass when injected as themselves.
        let same =
            Inject { halved: Some(f64::MIN_POSITIVE / 2.0), restored: Some(f64::MIN_POSITIVE) };
        assert_eq!(canary_injected(&same), Ok(()));
    }
}
