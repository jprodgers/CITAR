//! The module's functions (DESIGN.md P2.6.1): the ruleset, the tools, maps and scenarios as
//! values (their files stay in Python), the diplomacy categories, and the bot versions with
//! their schemas, cleaning and fingerprints.
//!
//! Every one that reads the ruleset releases the GIL: the first use in a process compiles it,
//! a heavy call.

use citar_engine::api::views::to_py_json;
use citar_engine::api::{maps, scenario, tools};
use citar_engine::game::diplomacy::category::{
    Category, item_category as category_of, proposal_categories as categories_of,
};
use citar_engine::rules::Ruleset;
use citar_engine::rules::names::NameKind;
use citar_engine::state::diplo::{DealItem, Terms};
use pyo3::prelude::*;
use serde_json::{Map, Value, json};

use crate::Bytes;
use crate::bot::Bot;
use crate::calls::detached;
use crate::errors::{Failure, caught, parse, parse_opt};

/// Runs `work` against the process's ruleset with the GIL released, a panic caught as a crash.
fn ruled<T: Send>(
    py: Python<'_>,
    work: impl FnOnce(&'static Ruleset) -> Result<T, Failure> + Send,
) -> PyResult<T> {
    Ok(detached(py, || caught(|| work(Ruleset::shared())).map_err(Failure::Crash)?)?)
}

// ---- The ruleset ---------------------------------------------------------------------------

/// The ruleset's version, recorded in every save: the format version and the start of its id,
/// `2-0123456789ab` (`engine_api.rules_version`).
#[pyfunction]
pub fn rules_version(py: Python<'_>) -> PyResult<String> {
    ruled(py, |r| Ok(r.version()))
}

/// The whole ruleset as the browser and the agents read it, as JSON bytes
/// (`engine_api.rules_client`).
#[pyfunction]
pub fn rules_client(py: Python<'_>) -> PyResult<Bytes> {
    ruled(py, |r| Ok(Bytes(r.client_json().as_bytes().to_vec())))
}

/// The most seats a game can have.
#[pyfunction]
pub fn max_players(py: Python<'_>) -> PyResult<u8> {
    ruled(py, |r| Ok(r.max_players()))
}

/// `{size: {name, width, height, players, city_states}}` as JSON bytes, in the ruleset's order
/// (`engine_api.map_sizes`).
#[pyfunction]
pub fn map_sizes(py: Python<'_>) -> PyResult<Bytes> {
    ruled(py, |r| {
        let m: Map<String, Value> = r
            .map_sizes()
            .iter()
            .map(|(_, s)| {
                let row = json!({
                    "name": &*s.name, "width": s.width, "height": s.height,
                    "players": s.players, "city_states": s.city_states,
                });
                (s.key.to_string(), row)
            })
            .collect();
        Ok(Bytes(to_py_json(&m)))
    })
}

/// The map generator's map types (`engine_api.map_types`).
#[pyfunction]
pub fn map_types(py: Python<'_>) -> PyResult<Vec<String>> {
    ruled(py, |r| Ok(r.constants().map_types.iter().map(|(_, t)| t.key.to_string()).collect()))
}

/// The game speeds, by name.
#[pyfunction]
pub fn speeds(py: Python<'_>) -> PyResult<Vec<String>> {
    ruled(py, |r| Ok(r.speed_names().map(str::to_owned).collect()))
}

/// The difficulty levels, easiest first.
#[pyfunction]
pub fn difficulties(py: Python<'_>) -> PyResult<Vec<String>> {
    ruled(py, |r| Ok(r.difficulty_names().map(str::to_owned).collect()))
}

/// A ruleset name as the ruleset spells it (`"quick"` gives `"Quick"` for `speed`), or `None`
/// for one it lacks (`engine_api.resolve_name`). `ValueError` for a kind that is no table.
#[pyfunction]
pub fn resolve_name(py: Python<'_>, kind: &str, name: Option<&str>) -> PyResult<Option<String>> {
    let k = NameKind::from_name(kind).ok_or_else(|| {
        let all: Vec<&str> = NameKind::ALL.iter().map(|k| k.as_str()).collect();
        Failure::Value(format!("Unknown kind '{kind}'. Known: {}.", all.join(", ")))
    })?;
    let Some(name) = name else { return Ok(None) };
    ruled(py, |r| Ok(r.resolve_name(k, name).map(str::to_owned)))
}

/// How much the ruleset holds: `{techs, units, buildings, nations, policies}` as JSON bytes.
#[pyfunction]
pub fn ruleset_counts(py: Python<'_>) -> PyResult<Bytes> {
    ruled(py, |r| Ok(Bytes(to_py_json(&r.counts()))))
}

// ---- The tools -----------------------------------------------------------------------------

/// Every player tool with its JSON schema, as JSON bytes; only the `query` or `action` ones if
/// `kind` names one (`engine_api.tool_list`).
#[pyfunction]
#[pyo3(signature = (kind = None))]
pub fn tool_list(kind: Option<&str>) -> PyResult<Bytes> {
    match kind {
        None => Ok(Bytes(tools::schemas_json().as_bytes().to_vec())),
        Some(k) => {
            let k = tools::ToolKind::from_name(k).ok_or_else(|| {
                Failure::Value(format!("Unknown tool kind '{k}'. Known: query, action."))
            })?;
            Ok(Bytes(to_py_json(&tools::schemas(Some(k)))))
        }
    }
}

/// Whether a tool is an `action` or a `query`; `None` for a name that is no tool.
#[pyfunction]
pub fn tool_kind(name: &str) -> Option<&'static str> {
    tools::kind(name).map(tools::ToolKind::name)
}

// ---- Saves, maps and scenarios as values ---------------------------------------------------

/// The headline facts of a saved state (a save's or a scenario's) without loading it, as JSON
/// bytes (`engine_api.state_summary`). `LoadError` for one that does not read.
#[pyfunction]
pub fn state_summary(py: Python<'_>, state_json: &[u8]) -> PyResult<Bytes> {
    Ok(detached(py, || {
        let s = citar_engine::save::summary(state_json)?;
        Ok::<_, Failure>(Bytes(to_py_json(&s)))
    })?)
}

/// A map document checked and cleaned: the map with its problems fixed, as JSON bytes, and
/// what was fixed (`engine_api.validate_map`). `MapError` for one that cannot be a map.
#[pyfunction]
pub fn validate_map(py: Python<'_>, doc_json: &[u8]) -> PyResult<(Bytes, Vec<String>)> {
    ruled(py, |r| {
        let doc = parse(doc_json, "The map")?;
        let (clean, fixed) = maps::validate_map(r, &doc)?;
        Ok((Bytes(to_py_json(&clean)), fixed))
    })
}

/// A map's headline facts, for lists, as JSON bytes. `MapError` for a document with no width,
/// height or tiles.
#[pyfunction]
pub fn map_summary(py: Python<'_>, doc_json: &[u8]) -> PyResult<Bytes> {
    ruled(py, |r| {
        let doc = parse(doc_json, "The map")?;
        Ok(Bytes(to_py_json(&maps::map_summary(r, &doc)?)))
    })
}

/// An empty map of one base terrain, for the editor to start from, as JSON bytes. `MapError`
/// for an unknown base terrain or a side out of bounds.
#[pyfunction]
#[pyo3(signature = (width, height, terrain, name = ""))]
pub fn blank_map(
    py: Python<'_>,
    width: i64,
    height: i64,
    terrain: &str,
    name: &str,
) -> PyResult<Bytes> {
    ruled(py, |r| Ok(Bytes(to_py_json(&maps::blank_map(r, width, height, terrain, name)?))))
}

/// A map from the generator, as the editor's document, as JSON bytes: `settings_json` are the
/// lobby's (`map_size` or `width` and `height`, `map_type`, `map_edges`, `river_density`,
/// `resources`, `players`, `city_states`, `ruins`, `name`), and the caller draws the seed
/// (`engine_api.generate_map`). `MapError` or `ValueError` for settings that make no map.
#[pyfunction]
#[pyo3(signature = (seed, settings_json = None))]
pub fn generate_map(py: Python<'_>, seed: u64, settings_json: Option<&[u8]>) -> PyResult<Bytes> {
    ruled(py, |r| {
        let settings = parse_opt(settings_json, "The map settings")?;
        Ok(Bytes(to_py_json(&maps::generate_map(r, seed, &settings)?)))
    })
}

/// Every scenario edit operation with its parameters, as JSON bytes.
#[pyfunction]
pub fn scenario_ops_help() -> Bytes {
    Bytes(to_py_json(&scenario::ops_help()))
}

/// A scenario's headline facts, for lists, as JSON bytes. `ActionError` for a document that is
/// no scenario.
#[pyfunction]
pub fn scenario_summary(py: Python<'_>, doc_json: &[u8]) -> PyResult<Bytes> {
    Ok(detached(py, || {
        let doc = parse(doc_json, "The scenario")?;
        Ok::<_, Failure>(Bytes(to_py_json(&scenario::scenario_summary(&doc)?)))
    })?)
}

// ---- Diplomacy categories ------------------------------------------------------------------

/// The diplomacy category a deal item belongs to (`gold` is `trades`, `peace_treaty` is
/// `peace`). The item is as the game stores it. `ValueError` for one that does not read.
#[pyfunction]
pub fn item_category(py: Python<'_>, item_json: &[u8]) -> PyResult<&'static str> {
    let item = parse(item_json, "The item")?;
    ruled(py, |r| {
        let i = DealItem::from_json(&item, r).map_err(|e| Failure::Value(e.0))?;
        Ok(category_of(i.kind()).name())
    })
}

/// The categories a proposal touches, in the categories' order; none for `null` (a
/// negotiation that is only talk). The terms are as the game stores them.
#[pyfunction]
pub fn proposal_categories(py: Python<'_>, proposal_json: &[u8]) -> PyResult<Vec<&'static str>> {
    let p = parse(proposal_json, "The proposal")?;
    ruled(py, |r| {
        let terms = match &p {
            Value::Null => None,
            t => Some(Terms::from_json(t, r).map_err(|e| Failure::Value(e.0))?),
        };
        Ok(categories_of(terms.as_ref()).into_iter().map(Category::name).collect())
    })
}

// ---- Bots ----------------------------------------------------------------------------------

/// Every bot version compiled in, the latest first, as JSON bytes: `{id, label, description,
/// latest, memory_kind}` each.
#[pyfunction]
pub fn bot_versions() -> Bytes {
    let rows: Vec<Value> = citar_bot::versions()
        .iter()
        .map(|v| {
            json!({
                "id": v.id.id(), "label": v.label, "description": v.description,
                "latest": v.latest, "memory_kind": v.memory_kind,
            })
        })
        .collect();
    Bytes(to_py_json(&rows))
}

/// A version's parameter schema, `{engine, groups}` (the Bots page's shape), as JSON bytes;
/// `basic` names the latest. `ValueError` for an unknown version.
#[pyfunction]
pub fn bot_schema(version: &str) -> PyResult<Bytes> {
    let s = citar_bot::schema(version).map_err(|e| Failure::Value(e.to_string()))?;
    Ok(Bytes(s.as_bytes().to_vec()))
}

/// Parameter overrides cleaned against a version's schema, as JSON bytes: unknown keys
/// refused, values coerced to their parameter's type, defaults dropped, keys sorted
/// (`profiles.clean_params`). `ValueError` for overrides that do not clean.
#[pyfunction]
#[pyo3(signature = (version, params_json = None))]
pub fn bot_clean_params(version: &str, params_json: Option<&[u8]>) -> PyResult<Bytes> {
    let params = parse_opt(params_json, "The bot's parameters")?;
    let clean = citar_bot::clean(version, &params).map_err(|e| Failure::Value(e.message))?;
    Ok(Bytes(to_py_json(&clean.to_json())))
}

/// A bot's fingerprint: 12 hex digits over the build id, its version, its overrides and the
/// profile's fixed aggression (DESIGN.md P2.8.6).
#[pyfunction]
pub fn bot_fingerprint(py: Python<'_>, bot: &Bound<'_, Bot>) -> String {
    let spec = bot.get().snapshot();
    detached(py, || citar_bot::fingerprint(&spec, &citar_bot::build_id(Ruleset::shared())))
}
