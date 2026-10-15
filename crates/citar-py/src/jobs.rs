//! `citar._engine.jobs`: the jobs crate for the server (design/phase4-helper.md P4.4), which the
//! facade's `engine_api.run_job`, `check_job_spec`, `job_result_hash`, `job_chain_check` and
//! `jobs_build` wrap. The server never hashes or checks a job's bytes in Python: the same Rust as
//! the helper's reads the spec, plays the game, hashes the result and folds the chain (P4.4.4).
//!
//! Every value crosses as JSON bytes, as the rest of the module's do. A refusal is `JobRefused`
//! (a `ValueError`) with `.code`: `bad_spec` (the spec does not read, breaks a bound, or does not
//! hash to the hash given), `bad_result` (a result that does not read as the kind's) or
//! `bad_chain` (a chain that does not read, or whose digests do not fold to its head).
//!
//! `run` plays the job with the GIL released one seat step at a time, as `run_game` does, so
//! another thread's game goes on beside it; between steps it checks for signals and hands each
//! new round to `on_round`, whose exception ends the run and is raised. A game that panics is
//! the job's crash outcome, never an exception: a crash is a result (P4.4.2).

use citar_engine::base::ids::PlayerId;
#[cfg(feature = "test-ops")]
use citar_engine::game::SeatDriver;
use citar_jobs::{Checked, Job, Kind, RunError, SpecHash, check_spec};
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::{Value, json};

use crate::Bytes;
use crate::calls::{self, detached};
use crate::errors::{Failure, caught, guarded, parse, shielded};

create_exception!(
    citar._engine,
    JobRefused,
    PyValueError,
    "A job's spec, result or chain refused: `.code` is bad_spec, bad_result or bad_chain, the \
     message the reason."
);

/// A refusal with its code.
fn refused(code: &'static str, message: String) -> PyErr {
    Python::attach(|py| {
        let err = JobRefused::new_err(message);
        match err.value(py).setattr("code", code) {
            Ok(()) => err,
            Err(lost) => lost,
        }
    })
}

/// The kind named `name`, or `bad_spec`.
fn kind_of(name: &str) -> PyResult<Kind> {
    Kind::from_name(name).ok_or_else(|| {
        let all: Vec<&str> = Kind::ALL.iter().map(|k| k.name()).collect();
        refused("bad_spec", format!("unknown job kind {name:?}: the kinds are {}", all.join(", ")))
    })
}

/// The spec read and checked, off the GIL: the first check compiles the ruleset.
fn checked(py: Python<'_>, kind: Kind, spec_json: &[u8], hash: Option<&str>) -> PyResult<Checked> {
    let spec = parse(spec_json, "The job's spec")?;
    let out = guarded(py, || Ok(check_spec(crate::rules::rules(), kind, &spec, hash)))?;
    out.map_err(|e| refused("bad_spec", e.0))
}

/// Reads a job's spec as `kind`'s (`lab_game`, `ranking_game`, `reference_game`), refusing an
/// unknown key, a value out of its bounds, an unknown bot version or, with `spec_hash`, a spec
/// that does not hash to it. Returns `{spec (the canonical spec), spec_hash, bots}` as JSON
/// bytes.
#[pyfunction(name = "check_spec")]
#[pyo3(signature = (kind, spec_json, spec_hash = None))]
pub fn check_spec_py(
    py: Python<'_>,
    kind: &str,
    spec_json: &[u8],
    spec_hash: Option<&str>,
) -> PyResult<Bytes> {
    let c = checked(py, kind_of(kind)?, spec_json, spec_hash)?;
    let out = json!({"spec": c.canonical_value(), "spec_hash": c.hash.to_hex(), "bots": c.bots()});
    Ok(Bytes(serde_json::to_vec(&out).map_err(|e| Failure::Runtime(e.to_string()))?))
}

/// What a step hands back to the GIL: the round that began, if one did.
struct Stepped {
    round: Option<Vec<u8>>,
}

/// Plays a job of `kind` to its end and returns `{spec_hash, result, result_hash, chain: {head,
/// rounds, digests}, wall_s, cpu_s, trace?}` as JSON bytes: the outcome (a finished game's result
/// or its crash), hashed, with what was measured beside it (`wall_s`, `cpu_s` and a crash's
/// `trace` are never hashed). `on_round({turn, turn_limit})` hears the first turn and each turn
/// as it begins.
///
/// `test_panic_json` (`{"player": id, "turn": n}`, a build with the test operations only) seats a
/// bot that panics from that turn in that seat: the crash outcome's way through the bindings, for
/// the lab's tests. A build without them refuses it.
#[pyfunction]
#[pyo3(signature = (kind, spec_json, spec_hash = None, on_round = None, test_panic_json = None))]
pub fn run(
    py: Python<'_>,
    kind: &str,
    spec_json: &[u8],
    spec_hash: Option<&str>,
    on_round: Option<Bound<'_, PyAny>>,
    test_panic_json: Option<&[u8]>,
) -> PyResult<Bytes> {
    let c = checked(py, kind_of(kind)?, spec_json, spec_hash)?;
    let panic_at = match test_panic_json {
        None => None,
        Some(b) => crate::run::test_panic(Some(&parse(b, "test_panic")?))?,
    };
    let hash = c.hash;
    let rules = crate::rules::rules();
    let mut job = detached(py, || {
        caught(|| Job::start_seated(rules, &c, &mut |seats| panicking(seats, panic_at)))
    })
    .map_err(Failure::Crash)?
    .map_err(run_error)?;
    let loads = py.import("json")?.getattr("loads")?;
    let deliver = |round: &[u8]| -> PyResult<()> {
        if let Some(hook) = &on_round {
            let _held = calls::hold(py);
            hook.call1((loads.call1((PyBytes::new(py, round),))?,))?;
        }
        Ok(())
    };
    let first = round_of(&job);
    if let Some(r) = first {
        deliver(&r)?;
    }
    while !job.is_over() {
        let stepped = detached(py, || {
            caught(|| {
                job.step()?;
                Ok::<_, RunError>(Stepped { round: round_of(&job) })
            })
        })
        .map_err(Failure::Crash)?
        .map_err(run_error)?;
        if let Some(r) = stepped.round {
            deliver(&r)?;
        }
        py.check_signals()?;
    }
    let out = detached(py, move || caught(|| job.finish())).map_err(Failure::Crash)?;
    let out = out.map_err(run_error)?;
    let mut value = serde_json::to_value(&out).map_err(|e| Failure::Runtime(e.to_string()))?;
    if let Some(m) = value.as_object_mut() {
        m.insert("spec_hash".to_owned(), Value::String(hash.to_hex()));
    }
    Ok(Bytes(serde_json::to_vec(&value).map_err(|e| Failure::Runtime(e.to_string()))?))
}

/// The seats, the one `panic_at` names given a driver that panics from its turn on.
#[cfg(feature = "test-ops")]
fn panicking(seats: citar_sim::Seats, panic_at: Option<(PlayerId, i32)>) -> citar_sim::Seats {
    let Some((who, turn)) = panic_at else { return seats };
    seats
        .into_iter()
        .map(|(p, bot)| {
            let d: Box<dyn SeatDriver> =
                if p == who { Box::new(PanicAt { bot, turn }) } else { bot };
            (p, d)
        })
        .collect()
}

/// The seats as they are (a build without the test operations refuses `test_panic`).
#[cfg(not(feature = "test-ops"))]
fn panicking(seats: citar_sim::Seats, _: Option<(PlayerId, i32)>) -> citar_sim::Seats {
    seats
}

/// A seat's driver that panics once the game reaches a turn (`test_panic`).
#[cfg(feature = "test-ops")]
struct PanicAt {
    bot: Box<dyn SeatDriver>,
    turn: i32,
}

#[cfg(feature = "test-ops")]
impl SeatDriver for PanicAt {
    fn play_turn(
        &mut self,
        g: &mut citar_engine::game::Game,
        pid: PlayerId,
        mem: &mut citar_engine::state::players::DriverMemory,
    ) -> citar_engine::game::DriverOutcome {
        assert!(
            g.turn() < self.turn,
            "the test bot panics on turn {}, as it was asked to",
            g.turn()
        );
        self.bot.play_turn(g, pid, mem)
    }

    fn respond(
        &mut self,
        g: &mut citar_engine::game::Game,
        pid: PlayerId,
        nid: citar_engine::base::ids::NegotiationId,
        mem: &mut citar_engine::state::players::DriverMemory,
    ) -> citar_engine::game::DriverOutcome {
        self.bot.respond(g, pid, nid, mem)
    }
}

/// The round a job's last step began, as `on_round` hears it, while the game goes on.
fn round_of(job: &Job) -> Option<Vec<u8>> {
    let p = job.progress();
    (p.new_round && !job.is_over())
        .then(|| serde_json::to_vec(&json!({"turn": p.round, "turn_limit": p.of})).ok())
        .flatten()
}

/// A run that gave no result, as Python sees it.
fn run_error(e: RunError) -> PyErr {
    match e {
        RunError::Spec(s) => refused("bad_spec", s.0),
        other => Failure::Runtime(other.to_string()).into(),
    }
}

/// The result hash of a job's result (`result_json`), read as `kind`'s (an unknown key refused)
/// and written back: what the claim must name (P4.4.4).
#[pyfunction]
pub fn result_hash(kind: &str, result_json: &[u8]) -> PyResult<String> {
    let kind = kind_of(kind)?;
    let result = parse(result_json, "The job's result")?;
    let out = shielded(|| Ok(citar_jobs::result_hash(kind, &result)))?;
    out.map_err(|e| refused("bad_result", e.0))
}

/// Checks that a chain (`{head, rounds, digests}`) folds to its head from `chain_0` of
/// `spec_hash` and counts as many rounds as it carries digests; `None`, or `JobRefused`
/// (`bad_chain`) naming what does not hold.
#[pyfunction]
pub fn chain_check(spec_hash: &str, chain_json: &[u8]) -> PyResult<()> {
    let hash = SpecHash::from_hex(spec_hash).ok_or_else(|| {
        refused("bad_chain", format!("the spec hash {spec_hash:?} is not 64 lower-case hex digits"))
    })?;
    let chain = parse(chain_json, "The job's chain")?;
    let out = shielded(|| Ok(citar_jobs::chain_check_value(&hash, &chain)))?;
    out.map_err(|e| refused("bad_chain", e.to_string()))
}

/// This build's jobs build (P4.4.5), under the process's ruleset: 12 hex digits.
#[pyfunction]
pub fn jobs_build(py: Python<'_>) -> PyResult<String> {
    Ok(guarded(py, || Ok(citar_jobs::jobs_build(crate::rules::rules())))?)
}

/// The submodule `citar._engine.jobs`.
pub fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let m = PyModule::new(py, "jobs")?;
    m.add("JobRefused", py.get_type::<JobRefused>())?;
    m.add_function(wrap_pyfunction!(check_spec_py, &m)?)?;
    m.add_function(wrap_pyfunction!(run, &m)?)?;
    m.add_function(wrap_pyfunction!(result_hash, &m)?)?;
    m.add_function(wrap_pyfunction!(chain_check, &m)?)?;
    m.add_function(wrap_pyfunction!(jobs_build, &m)?)?;
    m.add("KINDS", pyo3::types::PyTuple::new(py, Kind::ALL.iter().map(|k| k.name()))?)?;
    Ok(m)
}
