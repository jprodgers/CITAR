//! `citar._engine`: the CITAR engine, its bots and its runner for Python (DESIGN.md P2.6),
//! built as one abi3 extension per OS and imported only by the facade, `citar/engine_api.py`.
//! `citar/_engine.pyi` describes every name.
//!
//! The rules the module keeps:
//! - **Values cross as bytes or small Python values.** Every value that would be a dict comes
//!   back as JSON bytes (floats as Python writes them), decoded by the facade; counts, ids and
//!   names come back as ints and strs. JSON arguments go in as bytes.
//! - **The GIL is released for every heavy call** (`calls::detached`), with the game's lock
//!   taken inside, so no thread waits for a game while holding the GIL, and games on two threads
//!   use two cores. Cheap reads come from `Heads` with the GIL held (`game`).
//! - **Panics never cross the boundary.** A game's calls catch them inside the game's lock: the
//!   game is poisoned, its lock is not, and the caller gets `EngineCrash`. Every other entry
//!   point catches them too, as an `EngineCrash` (`errors`).
//! - **Interpreter exit waits for the calls in flight:** the module registers `shutdown` with
//!   `atexit`, so a daemon thread's call completes and the thread then parks, never
//!   re-attaching to a finalizing interpreter (`calls`).
//!
//! - **One ruleset a process** (`rules`): the one compiled in, or the one in the directory
//!   `CITAR_RULESET_DIR` names, read when the module loads (DESIGN.md P2.8.4). Every game,
//!   ruleset function and fingerprint uses it, and `build_info` reports its id.
//!
//! Modules: `game` (`Game`), `bot` (`Bot`), `run` (`run_game`), `save` (`Journal`,
//! `SaveSnapshot`, `Save` and the save files: saves v2, package 2-11), `funcs` (the ruleset,
//! tools, maps, scenarios, categories and bot versions), `rules` (the process's ruleset and
//! `check_ruleset`), `errors` (the exceptions) and `calls` (the GIL, the calls in flight and the
//! shutdown).

mod bot;
mod calls;
mod errors;
mod funcs;
mod game;
mod rules;
mod run;
mod save;

use std::convert::Infallible;

use citar_engine::api::game::DebugAction;
use citar_engine::api::text::{MAP_LEGEND, RULES_OVERVIEW};
use citar_engine::game::diplomacy::category::CATEGORIES;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};

use crate::errors::{ActionError, EngineCrash, Failure, LoadError, MapError, guarded};

/// JSON bytes for Python: a value that would be a dict or a list, which the facade decodes.
pub struct Bytes(pub Vec<u8>);

impl<'py> IntoPyObject<'py> for Bytes {
    type Target = PyBytes;
    type Output = Bound<'py, PyBytes>;
    type Error = Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        Ok(PyBytes::new(py, &self.0))
    }
}

/// What this build is, as JSON bytes: version, build id, label, ruleset id and both content
/// codes (DESIGN.md P2.2.1), all of the process's ruleset, and `ruleset_dir`, the directory
/// `CITAR_RULESET_DIR` gave it (null for the ruleset compiled in).
#[pyfunction]
fn build_info(py: Python<'_>) -> PyResult<Bytes> {
    // The first call in a process parses and compiles the embedded ruleset for its id, a heavy
    // call, so it runs with the GIL released (DESIGN.md P2.6.2) like every other.
    Ok(guarded(py, || {
        let mut info = serde_json::to_value(citar_bot::build_info(rules::rules()))
            .map_err(|e| Failure::Runtime(e.to_string()))?;
        let dir = rules::dir().map(|d| d.to_string_lossy().into_owned());
        if let Some(map) = info.as_object_mut() {
            map.insert("ruleset_dir".to_owned(), dir.into());
        }
        serde_json::to_vec(&info).map(Bytes).map_err(|e| Failure::Runtime(e.to_string()))
    })?)
}

/// The module.
#[pymodule]
fn _engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // Where a caught panic happened, for its crash record (citar_sim::panics): process-wide,
    // chained to the hook it replaces, so the panic is still printed.
    citar_sim::panics::install();
    // A modded ruleset is the process's from the start, or the import fails saying why.
    rules::adopt_from_env(py)?;

    m.add_class::<game::Game>()?;
    m.add_class::<bot::Bot>()?;
    m.add_class::<save::Journal>()?;
    m.add_class::<save::SaveSnapshot>()?;
    m.add_class::<save::Save>()?;
    m.add("ActionError", py.get_type::<ActionError>())?;
    m.add("MapError", py.get_type::<MapError>())?;
    m.add("LoadError", py.get_type::<LoadError>())?;
    m.add("EngineCrash", py.get_type::<EngineCrash>())?;

    m.add_function(wrap_pyfunction!(build_info, m)?)?;
    m.add_function(wrap_pyfunction!(rules::check_ruleset, m)?)?;
    m.add_function(wrap_pyfunction!(calls::calls_in_flight, m)?)?;
    m.add_function(wrap_pyfunction!(calls::shutdown, m)?)?;
    m.add_function(wrap_pyfunction!(run::run_game, m)?)?;
    m.add_function(wrap_pyfunction!(save::read_save, m)?)?;
    m.add_function(wrap_pyfunction!(save::save_header, m)?)?;
    m.add_function(wrap_pyfunction!(save::fork_journal, m)?)?;
    m.add_function(wrap_pyfunction!(save::journal_in_use, m)?)?;
    #[cfg(feature = "test-ops")]
    m.add_function(wrap_pyfunction!(save::saves_read, m)?)?;
    for f in [
        wrap_pyfunction!(funcs::rules_version, m)?,
        wrap_pyfunction!(funcs::rules_client, m)?,
        wrap_pyfunction!(funcs::max_players, m)?,
        wrap_pyfunction!(funcs::map_sizes, m)?,
        wrap_pyfunction!(funcs::map_types, m)?,
        wrap_pyfunction!(funcs::speeds, m)?,
        wrap_pyfunction!(funcs::difficulties, m)?,
        wrap_pyfunction!(funcs::resolve_name, m)?,
        wrap_pyfunction!(funcs::ruleset_counts, m)?,
        wrap_pyfunction!(funcs::tool_list, m)?,
        wrap_pyfunction!(funcs::tool_kind, m)?,
        wrap_pyfunction!(funcs::state_summary, m)?,
        wrap_pyfunction!(funcs::validate_map, m)?,
        wrap_pyfunction!(funcs::map_summary, m)?,
        wrap_pyfunction!(funcs::blank_map, m)?,
        wrap_pyfunction!(funcs::generate_map, m)?,
        wrap_pyfunction!(funcs::scenario_ops_help, m)?,
        wrap_pyfunction!(funcs::scenario_summary, m)?,
        wrap_pyfunction!(funcs::item_category, m)?,
        wrap_pyfunction!(funcs::proposal_categories, m)?,
        wrap_pyfunction!(funcs::bot_versions, m)?,
        wrap_pyfunction!(funcs::bot_schema, m)?,
        wrap_pyfunction!(funcs::bot_clean_params, m)?,
        wrap_pyfunction!(funcs::bot_fingerprint, m)?,
    ] {
        m.add_function(f)?;
    }

    let categories: Vec<&str> = CATEGORIES.iter().map(|c| c.name()).collect();
    m.add("DIPLOMACY_CATEGORIES", PyTuple::new(py, categories)?)?;
    let debug: Vec<&str> = DebugAction::ALL.iter().map(|a| a.name()).collect();
    m.add("DEBUG_ACTIONS", PyTuple::new(py, debug)?)?;
    m.add("RULES_OVERVIEW", RULES_OVERVIEW)?;
    m.add("MAP_LEGEND", MAP_LEGEND)?;
    m.add("HAS_TEST_OPS", cfg!(feature = "test-ops"))?;

    // A daemon thread inside a call when Python exits must not re-attach to the finalizing
    // interpreter (DESIGN.md P2.6.4): `shutdown` bars it, and waits a while for the calls in
    // flight. Registered here, so every importer is covered, the facade or not.
    py.import("atexit")?.call_method1("register", (m.getattr("shutdown")?,))?;
    Ok(())
}
