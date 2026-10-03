//! The module's exceptions, how the engine's errors become them (DESIGN.md P2.6.3), and the
//! guard that keeps a panic from crossing into Python (P2.6.4).
//!
//! | Engine | Python |
//! |---|---|
//! | `ActionError { code, message }` | `ActionError(message)` with `.code` (`"not_your_turn"`, ...) |
//! | `ErrCode::Poisoned`, `EngineError::Poisoned`, a caught panic | `EngineCrash(RuntimeError)`, never `ActionError` |
//! | `EngineError::Config`, a bad argument | `ValueError` |
//! | `EngineError::Map`, a map refusal | `MapError(ValueError)` |
//! | `LoadError` | `LoadError(ValueError)` |
//!
//! A call fails with a [`Failure`] made without the GIL, which becomes the exception once the
//! GIL is back.

use std::panic::{AssertUnwindSafe, catch_unwind};

use citar_engine::game::{ActionError as EngineAction, EngineError, ErrCode};
use citar_engine::save::LoadError as EngineLoad;
use pyo3::exceptions::{PyException, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::{PyErr, create_exception};

create_exception!(
    citar._engine,
    ActionError,
    PyException,
    "A refusal the caller can fix: a rule, a turn, a missing or bad argument. The message is \
     what a model reads; `.code` names the kind (`not_your_turn`, `bad_param`, ...)."
);
create_exception!(
    citar._engine,
    MapError,
    PyValueError,
    "A map document that cannot be a map, or a map the generator cannot make."
);
create_exception!(
    citar._engine,
    LoadError,
    PyValueError,
    "A saved game or a save's part that does not load."
);
create_exception!(
    citar._engine,
    EngineCrash,
    PyRuntimeError,
    "The engine stopped after an internal error (a panic): the game takes no more commands, and \
     still answers reads and saves. Never an ActionError: a crash is not a refusal."
);

/// Why a call failed, made without the GIL and raised with it.
#[derive(Debug)]
pub enum Failure {
    /// A refusal: `ActionError`, or `EngineCrash` when the game is poisoned.
    Action(EngineAction),
    /// The engine stopped after an internal error.
    Crash(String),
    /// A bad argument or setting: `ValueError`.
    Value(String),
    /// A map refusal: `MapError`.
    Map(String),
    /// A save that does not load: `LoadError`.
    Load(String),
    /// An argument of the wrong kind: `TypeError`.
    Type(String),
    /// Something the engine rules out happened anyway (a state that does not write).
    Runtime(String),
}

impl Failure {
    /// The engine's refusal of a poisoned game, as a crash.
    pub fn poisoned(why: &str) -> Self {
        Self::Crash(format!(
            "The game stopped after an internal error and takes no more commands: {why}"
        ))
    }
}

impl From<EngineAction> for Failure {
    fn from(e: EngineAction) -> Self {
        if e.code == ErrCode::Poisoned { Self::Crash(e.message) } else { Self::Action(e) }
    }
}

impl From<EngineError> for Failure {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Action(a) => a.into(),
            EngineError::Config(m) => Self::Value(m),
            EngineError::Map(m) => Self::Map(m),
            EngineError::Load(l) => l.into(),
            EngineError::Poisoned(why) => Self::poisoned(&why),
            other => Self::Runtime(other.to_string()),
        }
    }
}

impl From<EngineLoad> for Failure {
    fn from(e: EngineLoad) -> Self {
        Self::Load(e.to_string())
    }
}

impl From<Failure> for PyErr {
    fn from(f: Failure) -> Self {
        match f {
            Failure::Action(e) => Python::attach(|py| {
                let err = ActionError::new_err(e.message);
                match err.value(py).setattr("code", e.code.name()) {
                    Ok(()) => err,
                    Err(lost) => lost,
                }
            }),
            Failure::Crash(m) => EngineCrash::new_err(m),
            Failure::Value(m) => PyValueError::new_err(m),
            Failure::Map(m) => MapError::new_err(m),
            Failure::Load(m) => LoadError::new_err(m),
            Failure::Type(m) => PyTypeError::new_err(m),
            Failure::Runtime(m) => PyRuntimeError::new_err(m),
        }
    }
}

/// What a caught panic says, with where it happened when the panic hook saw it
/// (`citar_sim::panics`).
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned());
    let at = citar_sim::panics::take().map(|t| t.text(citar_sim::TRACEBACK_LIMIT));
    match at {
        Some(at) if !at.is_empty() => format!("panic: {message}\n{at}"),
        _ => format!("panic: {message}"),
    }
}

/// Runs `work`, catching a panic as the text of a crash; `Err` holds that text.
pub fn caught<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    // A trace left by a panic caught earlier on this thread is not this call's.
    citar_sim::panics::forget();
    catch_unwind(AssertUnwindSafe(work)).map_err(|p| panic_text(p.as_ref()))
}

/// Parses JSON bytes the caller sent; `what` names them in the error.
pub fn parse(bytes: &[u8], what: &str) -> Result<serde_json::Value, Failure> {
    serde_json::from_slice(bytes).map_err(|e| Failure::Value(format!("{what} is not JSON: {e}")))
}

/// Parses optional JSON bytes; `None` is JSON's `null`.
pub fn parse_opt(bytes: Option<&[u8]>, what: &str) -> Result<serde_json::Value, Failure> {
    bytes.map_or(Ok(serde_json::Value::Null), |b| parse(b, what))
}
