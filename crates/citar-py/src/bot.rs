//! `Bot`: a handle on a compiled bot (DESIGN.md P2.6.5), the facade's `bot_instance`,
//! `bot_set_diplomacy` and `bot_owns_negotiation`.
//!
//! A handle holds `Mutex<Arc<BotSpec>>`. `set_diplomacy` swaps in a spec with the new owners, in
//! place, as `bot_set_diplomacy` promises and Phase 3's hybrid seats rely on; the `Tuning` and
//! its resolution cache are shared, not rebuilt. Each drive or answer takes every handle's spec
//! once at its start and builds fresh `citar_bot::Bot`s from them, so no Python object is
//! borrowed across a call, two seats may share a handle, and a change during a drive applies to
//! the next one. Only compiled bots run on Rust: a Python bot object is refused (`TypeError`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use citar_bot::{BotSpec, Owners, Tuning, VersionId};
use citar_engine::api::views::to_py_json;
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::Terms;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::Bytes;
use crate::errors::{Failure, guarded, parse, parse_opt, shielded};

/// A bot to seat in a game or a headless run (`engine_api.bot_instance`).
#[pyclass(frozen, module = "citar._engine", name = "Bot")]
pub struct Bot {
    spec: Mutex<Arc<BotSpec>>,
}

impl Bot {
    fn spec(&self) -> MutexGuard<'_, Arc<BotSpec>> {
        // Held only to clone or swap an Arc.
        self.spec.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The spec as it stands now: what a drive or an answer plays.
    pub fn snapshot(&self) -> Arc<BotSpec> {
        Arc::clone(&self.spec())
    }

    /// The fingerprint of what plays now (`Bot.fingerprint`, `bot_fingerprint`), off the GIL:
    /// the build id needs the ruleset, which its first use compiles.
    pub fn fingerprint_now(&self, py: Python<'_>) -> PyResult<String> {
        let spec = self.snapshot();
        Ok(guarded(py, || {
            Ok(citar_bot::fingerprint(&spec, &citar_bot::build_id(Ruleset::shared())))
        })?)
    }
}

/// The bots of a drive or a run by seat, each handle's spec taken once now. `TypeError` for a
/// bot that is not a compiled one.
pub fn seat_bots(bots: &Bound<'_, PyDict>) -> PyResult<Vec<(i64, Arc<BotSpec>)>> {
    let mut out = Vec::with_capacity(bots.len());
    for (k, v) in bots.iter() {
        let pid: i64 = k.extract()?;
        let Ok(bot) = v.cast::<Bot>() else {
            let kind = v.get_type().name().map_or_else(|_| "?".to_owned(), |n| n.to_string());
            return Err(Failure::Type(format!(
                "Only compiled bots run on Rust: player {pid}'s bot is a {kind}, not a \
                 citar._engine.Bot."
            ))
            .into());
        };
        out.push((pid, bot.get().snapshot()));
    }
    Ok(out)
}

#[pymethods]
impl Bot {
    /// A bot of `version` (`basic` names the latest), with `params_json`'s overrides cleaned
    /// against the version's schema, playing with `aggression` (the seat's) unless the profile
    /// fixes one (`fixed_aggression`), else 0.4, held to 0..1. `ValueError` for an unknown
    /// version or parameters that do not clean.
    #[new]
    #[pyo3(signature = (version = "basic", params_json = None, aggression = None, fixed_aggression = None))]
    fn new(
        version: &str,
        params_json: Option<&[u8]>,
        aggression: Option<f64>,
        fixed_aggression: Option<f64>,
    ) -> PyResult<Self> {
        let spec = shielded(|| {
            let v = VersionId::resolve(version).ok_or_else(|| {
                Failure::Value(citar_bot::BotError::UnknownVersion(version.to_owned()).to_string())
            })?;
            let params = parse_opt(params_json, "The bot's parameters")?;
            let overrides =
                citar_bot::clean(version, &params).map_err(|e| Failure::Value(e.message))?;
            let tuning = Arc::new(Tuning::new(v, overrides));
            Ok(BotSpec::new(v, tuning, fixed_aggression, aggression))
        })?;
        Ok(Self { spec: Mutex::new(Arc::new(spec)) })
    }

    /// Says who decides each kind of diplomacy, `{category: "bot" | "llm"}`, `bot` for any not
    /// named; in place, so a seat holding this handle follows from its next drive
    /// (`engine_api.bot_set_diplomacy`). `ValueError` for an unknown category or owner.
    #[pyo3(signature = (owners_json = None))]
    fn set_diplomacy(&self, owners_json: Option<&[u8]>) -> PyResult<()> {
        let owners = shielded(|| {
            Owners::from_json(&parse_opt(owners_json, "The diplomacy owners")?)
                .map_err(|e| Failure::Value(e.0))
        })?;
        // The new spec is made outside the handle's lock, so a panic there cannot poison it.
        let next = shielded(|| Ok(Arc::new(BotSpec::clone(&self.snapshot()).with_owners(owners))))?;
        *self.spec() = next;
        Ok(())
    }

    /// Whether the bot answers this negotiation itself rather than the language model its seat
    /// hands that kind of diplomacy to (`engine_api.bot_owns_negotiation`). The negotiation is
    /// its record as `Game.negotiation` gives it.
    fn owns_negotiation(&self, py: Python<'_>, negotiation_json: &[u8]) -> PyResult<bool> {
        let spec = self.snapshot();
        // The ruleset's first use compiles it: off the GIL.
        Ok(guarded(py, || {
            let n = parse(negotiation_json, "The negotiation")?;
            if !n.is_object() {
                return Err(Failure::Value(
                    "The negotiation must be its record, an object.".to_owned(),
                ));
            }
            let terms = match n.get("proposal") {
                None | Some(Value::Null) => None,
                Some(p) => {
                    Some(Terms::from_json(p, Ruleset::shared()).map_err(|e| Failure::Value(e.0))?)
                }
            };
            Ok(spec.owners.owns_terms(terms.as_ref()))
        })?)
    }

    /// The aggression it plays with, 0 to 1.
    #[getter]
    fn aggression(&self) -> f64 {
        self.spec().aggression
    }

    /// The profile's own aggression, `None` when the seat decides: what the fingerprint hashes.
    #[getter]
    fn fixed_aggression(&self) -> Option<f64> {
        self.spec().fixed_aggression
    }

    /// The version that plays: `basic-1`, `idle`.
    #[getter]
    fn version(&self) -> &'static str {
        self.spec().version.id()
    }

    /// Who decides each kind of diplomacy, `{category: "bot" | "llm"}`, as JSON bytes.
    #[getter]
    fn owners(&self) -> PyResult<Bytes> {
        let spec = self.snapshot();
        Ok(shielded(|| Ok(Bytes(to_py_json(&spec.owners.to_json()))))?)
    }

    /// The cleaned parameter overrides, as JSON bytes.
    #[getter]
    fn params(&self) -> PyResult<Bytes> {
        let spec = self.snapshot();
        Ok(shielded(|| Ok(Bytes(to_py_json(&spec.tuning.overrides().to_json()))))?)
    }

    /// What actually plays, hashed: 12 hex digits over the build id, the version, the
    /// overrides and the profile's fixed aggression (DESIGN.md P2.8.6).
    fn fingerprint(&self, py: Python<'_>) -> PyResult<String> {
        self.fingerprint_now(py)
    }

    fn __repr__(&self) -> String {
        let s = self.spec();
        match s.fixed_aggression {
            Some(a) => format!("<citar._engine.Bot {} fixed aggression {a:.3}>", s.version.id()),
            None => {
                format!("<citar._engine.Bot {} aggression {:.3}>", s.version.id(), s.aggression)
            }
        }
    }
}
