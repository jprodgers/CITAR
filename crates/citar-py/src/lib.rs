//! `citar._engine`: the CITAR engine, its bots, its runner and its saves for Python (DESIGN.md
//! P2.6), built by maturin as one abi3 extension per OS and imported only by the facade,
//! `citar/engine_api.py`.
//!
//! The rules the module keeps, from package 2-06a on:
//! - every value that would be a dict crosses as JSON bytes, decoded by the facade; counts, ids
//!   and names cross as ints and strs;
//! - heavy calls run inside `Python::detach`, with the game's `Mutex` taken inside, so no thread
//!   ever waits for a game while holding the GIL; cheap reads come from a small `Heads` copy;
//! - panics never cross the boundary: a caught panic poisons the game and raises `EngineCrash`.
//!
//! Package 2-00a wrote the skeleton that settles PyO3's names before 2-06a: `build_info()`, and a
//! frozen `Game` holding `Mutex<citar_engine::game::Game>` whose methods run inside
//! `Python::detach`.

use std::sync::{Mutex, PoisonError};

use citar_engine::game::setup::config_from_value;
use citar_engine::rules::Ruleset;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// One game (DESIGN.md P2.6.1). Frozen, so it must be `Sync`: the engine's `Game` is `Send`
/// and not `Sync`, and the `Mutex` around it makes it shareable.
#[pyclass(frozen, module = "citar._engine", name = "Game")]
struct Game {
    game: Mutex<citar_engine::game::Game>,
}

#[pymethods]
impl Game {
    /// A new game from a lobby configuration as JSON bytes. Raises ValueError for one that does
    /// not make a game.
    #[staticmethod]
    fn new(py: Python<'_>, config_json: &[u8]) -> PyResult<Self> {
        let made = py.detach(|| {
            let rules = Ruleset::shared();
            let config = serde_json::from_slice(config_json).map_err(|e| e.to_string())?;
            let setup = config_from_value(rules, config).map_err(|e| e.to_string())?;
            citar_engine::game::Game::new(rules, &setup).map_err(|e| e.to_string())
        });
        let (game, _created) = made.map_err(PyValueError::new_err)?;
        Ok(Self { game: Mutex::new(game) })
    }

    /// The current turn, read under the game's lock with the GIL released.
    fn turn(&self, py: Python<'_>) -> PyResult<i32> {
        py.detach(|| self.game.lock().map(|g| g.turn()).map_err(|e| lock_lost(&e)))
            .map_err(PyRuntimeError::new_err)
    }
}

/// What a poisoned lock says: a thread panicked while holding the game. From package 2-06a a
/// caught panic poisons the game instead, so the lock is never poisoned.
fn lock_lost<T>(_: &PoisonError<T>) -> String {
    "the game's lock was poisoned by a panic".to_owned()
}

/// What this build is, as JSON bytes: version, build id, label, ruleset id and both content
/// codes (DESIGN.md P2.2.1).
#[pyfunction]
fn build_info(py: Python<'_>) -> PyResult<Bound<'_, PyBytes>> {
    let info = citar_bot::build_info(Ruleset::shared());
    let bytes = serde_json::to_vec(&info).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(PyBytes::new(py, &bytes))
}

/// The module.
#[pymodule]
fn _engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Game>()?;
    m.add_function(wrap_pyfunction!(build_info, m)?)?;
    m.add("HAS_TEST_OPS", cfg!(feature = "test-ops"))?;
    Ok(())
}
