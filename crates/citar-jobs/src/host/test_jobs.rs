//! The test jobs (`test-jobs`, design/phase4-helper.md P4.3.1): two kinds a job host takes only in
//! a build made for tests, which cargo xtask check keeps out of every shipped build. They exercise
//! the limits without a game's cost or its variance:
//! - **`burn`** `{seconds}`: spins on its thread for `seconds` of wall-clock time while not
//!   paused, at most [`MAX_SECONDS`];
//! - **`alloc`** `{mb, step_mb?, hold_s?}`: allocates (and writes, so it is committed and
//!   resident) `step_mb` at a time (8 by default) up to `mb`, at most [`MAX_MB`], holds it
//!   `hold_s` seconds (at most [`MAX_SECONDS`]), then frees it.
//!
//! Each asks its control between steps (every 10 ms, or each step), so abort, pause and deadlines
//! act on it as on a game. Their specs are typed and closed and hashed as a job's are (P4.4.1's
//! formula with the test kind's name); their results are `{kind, seconds}` and `{kind, mb}`,
//! hashed by P4.4.4's formula, with a chain of no rounds.

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::save::chain::DigestChain;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::run::Played;
use crate::hash::{Chain, RESULT_DOMAIN};
use crate::job::{Control, Flow, Progress, RunError};
use crate::spec::{SPEC_DOMAIN, hex, unhex32};

/// The `burn` kind.
pub const BURN: &str = "burn";
/// The `alloc` kind.
pub const ALLOC: &str = "alloc";
/// The longest burn or hold, in seconds.
pub const MAX_SECONDS: f64 = 60.0;
/// The most an `alloc` job takes, in MB.
pub const MAX_MB: u64 = 4096;
/// How long a burn spins between two looks at its control.
const SPIN: Duration = Duration::from_millis(10);

/// `burn`'s spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BurnSpec {
    pub seconds: f64,
}

const fn eight() -> u64 {
    8
}

/// `alloc`'s spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocSpec {
    pub mb: u64,
    #[serde(default = "eight")]
    pub step_mb: u64,
    #[serde(default)]
    pub hold_s: f64,
}

/// A test job, read and hashed.
#[derive(Clone, Debug, PartialEq)]
pub enum TestJob {
    Burn(BurnSpec, [u8; 32]),
    Alloc(AllocSpec, [u8; 32]),
}

fn domain_hash(domain: &[u8], kind: &str, bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(kind.as_bytes());
    h.update(b"\0");
    h.update(bytes);
    *h.finalize().as_bytes()
}

/// A test job's spec, written back and hashed as a job's spec is: its canonical form and its
/// hash, for a test's offer.
///
/// # Errors
/// A spec that is not of the kind's shape or within its bounds, or a kind that is not a test
/// kind.
pub fn hashed(kind: &str, spec: &Value) -> Result<(Value, String), String> {
    let (canonical, hash) = canonical(kind, spec)?;
    let value = serde_json::from_slice(&canonical).map_err(|e| e.to_string())?;
    Ok((value, hex(&hash)))
}

fn canonical(kind: &str, spec: &Value) -> Result<(Vec<u8>, [u8; 32]), String> {
    let seconds_ok = |s: f64| s.is_finite() && (0.0..=MAX_SECONDS).contains(&s);
    let bytes = match kind {
        BURN => {
            let b = BurnSpec::deserialize(spec).map_err(|e| format!("{kind} spec: {e}"))?;
            if !seconds_ok(b.seconds) {
                return Err(format!("seconds {} is not 0 to {MAX_SECONDS}", b.seconds));
            }
            serde_json::to_vec(&b)
        }
        ALLOC => {
            let a = AllocSpec::deserialize(spec).map_err(|e| format!("{kind} spec: {e}"))?;
            if !(1..=MAX_MB).contains(&a.mb) || !(1..=MAX_MB).contains(&a.step_mb) {
                return Err(format!("mb and step_mb are 1 to {MAX_MB}"));
            }
            if !seconds_ok(a.hold_s) {
                return Err(format!("hold_s {} is not 0 to {MAX_SECONDS}", a.hold_s));
            }
            serde_json::to_vec(&a)
        }
        other => return Err(format!("{other:?} is not a test kind")),
    }
    .map_err(|e| e.to_string())?;
    let hash = domain_hash(SPEC_DOMAIN, kind, &bytes);
    Ok((bytes, hash))
}

/// An offer's test job: `None` when `kind` is not a test kind; otherwise the job, or why its spec
/// or its hash is refused (`bad_spec`).
#[must_use]
pub fn read(kind: &str, spec: &Value, spec_hash: &str) -> Option<Result<TestJob, String>> {
    if kind != BURN && kind != ALLOC {
        return None;
    }
    Some((|| {
        let (bytes, hash) = canonical(kind, spec)?;
        if unhex32(spec_hash) != Some(hash) {
            return Err(format!("the spec hash is not the hash of the spec, {}", hex(&hash)));
        }
        Ok(if kind == BURN {
            TestJob::Burn(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?, hash)
        } else {
            TestJob::Alloc(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?, hash)
        })
    })())
}

fn at(round: u64, of: u64, new_round: bool) -> Progress {
    let t = |n: u64| i32::try_from(n).unwrap_or(i32::MAX);
    Progress { round: t(round), of: t(of), new_round }
}

fn ask(control: &mut dyn Control, p: &Progress) -> Result<(), RunError> {
    match control.between(p) {
        Flow::Go => Ok(()),
        Flow::Stop => Err(RunError::Stopped),
    }
}

/// Plays a test job under `control`.
pub(crate) fn play(job: &TestJob, control: &mut dyn Control) -> Result<Played, RunError> {
    let started = Instant::now();
    let cpu0 = cpu_time::ThreadTime::try_now().ok().map(|t| t.as_duration());
    let (kind, result, hash) = match job {
        TestJob::Burn(b, hash) => {
            burn(b, control)?;
            (BURN, json!({"kind": BURN, "seconds": b.seconds}), hash)
        }
        TestJob::Alloc(a, hash) => {
            alloc(a, control)?;
            (ALLOC, json!({"kind": ALLOC, "mb": a.mb}), hash)
        }
    };
    let bytes = serde_json::to_vec(&result).map_err(|e| RunError::Internal(e.to_string()))?;
    let now = cpu_time::ThreadTime::try_now().ok().map(|t| t.as_duration());
    Ok(Played {
        result,
        result_hash: hex(&domain_hash(RESULT_DOMAIN, kind, &bytes)),
        chain: Chain::of(&DigestChain::new(hash), &[]),
        wall_s: started.elapsed().as_secs_f64(),
        cpu_s: cpu0.zip(now).map_or(0.0, |(a, b)| b.saturating_sub(a).as_secs_f64()),
    })
}

/// Spins for `seconds` of wall-clock time spent spinning (a pause between spins is not counted).
fn burn(b: &BurnSpec, control: &mut dyn Control) -> Result<(), RunError> {
    let target = Duration::from_secs_f64(b.seconds);
    let of = target.as_secs() + u64::from(target.subsec_nanos() > 0);
    let mut spun = Duration::ZERO;
    let mut whole = u64::MAX;
    loop {
        let now = spun.as_secs();
        ask(control, &at(now, of, now != whole))?;
        whole = now;
        if spun >= target {
            return Ok(());
        }
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < SPIN {
            for _ in 0..1000 {
                x = black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
            }
        }
        black_box(x);
        spun += t.elapsed();
    }
}

/// Allocates up to `a.mb`, writing every byte, holds it, then frees it.
fn alloc(a: &AllocSpec, control: &mut dyn Control) -> Result<(), RunError> {
    let total = a.mb << 20;
    let step = a.step_mb << 20;
    let mut held: Vec<Vec<u8>> = Vec::new();
    let mut have = 0u64;
    while have < total {
        ask(control, &at(have >> 20, a.mb, true))?;
        let n = usize::try_from(step.min(total - have))
            .map_err(|e| RunError::Internal(e.to_string()))?;
        held.push(black_box(vec![0xA5u8; n]));
        have += step.min(total - have);
    }
    let hold = Duration::from_secs_f64(a.hold_s);
    let t = Instant::now();
    loop {
        ask(control, &at(a.mb, a.mb, false))?;
        if t.elapsed() >= hold {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(black_box(held));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Unattended;

    #[test]
    fn test_specs_are_typed_bounded_and_hashed() {
        let (spec, hash) = hashed(BURN, &json!({"seconds": 0.05})).expect("a burn");
        assert_eq!(spec, json!({"seconds": 0.05}));
        assert!(matches!(read(BURN, &spec, &hash), Some(Ok(TestJob::Burn(..)))));
        assert!(matches!(read(BURN, &spec, &"0".repeat(64)), Some(Err(_))));
        assert!(read("lab_game", &spec, &hash).is_none());
        for bad in [json!({"seconds": -1}), json!({"seconds": 61}), json!({"seconds": 1, "x": 1})] {
            assert!(hashed(BURN, &bad).is_err(), "{bad}");
        }
        let (spec, hash) = hashed(ALLOC, &json!({"mb": 4})).expect("an alloc");
        assert_eq!(spec, json!({"mb": 4, "step_mb": 8, "hold_s": 0.0}));
        assert!(matches!(read(ALLOC, &spec, &hash), Some(Ok(TestJob::Alloc(..)))));
        assert!(hashed(ALLOC, &json!({"mb": 0})).is_err());
        assert!(hashed(ALLOC, &json!({"mb": MAX_MB + 1})).is_err());
        assert_ne!(hashed(BURN, &json!({"seconds": 1})), hashed(BURN, &json!({"seconds": 2})));
    }

    #[test]
    fn test_jobs_play_to_a_result_with_an_empty_chain() {
        let (spec, hash) = hashed(BURN, &json!({"seconds": 0.03})).expect("a burn");
        let Some(Ok(job)) = read(BURN, &spec, &hash) else { panic!("read") };
        let out = play(&job, &mut Unattended).expect("played");
        assert!(out.wall_s >= 0.03, "{}", out.wall_s);
        assert_eq!(out.chain.rounds, 0);
        assert_eq!(out.result, json!({"kind": "burn", "seconds": 0.03}));
        let again = play(&job, &mut Unattended).expect("played again");
        assert_eq!(again.result_hash, out.result_hash);
        let (spec, hash) = hashed(ALLOC, &json!({"mb": 2, "step_mb": 1})).expect("an alloc");
        let Some(Ok(job)) = read(ALLOC, &spec, &hash) else { panic!("read") };
        assert_eq!(play(&job, &mut Unattended).expect("played").result["mb"], 2);
        let mut stop = |_: &Progress| Flow::Stop;
        assert_eq!(play(&job, &mut stop).err(), Some(RunError::Stopped));
    }
}
