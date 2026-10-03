//! `profiles.clean_params` (profiles.py:185-225): overrides checked against the version's
//! schema, each value coerced to its parameter's type, values equal to the default dropped (they
//! are not overrides), keys sorted. The messages are Python's, the version named as the caller
//! named it (`basic` or `basic-1`) and a value quoted as Python's `repr` quotes it, so a profile
//! refused today reads the same.
//!
//! Python's laxities kept, because stored profiles and the editor rely on them (2-00b's table,
//! `tests/data/clean_params_cases.json`, records each): a bool takes `1`, `true`, `yes` and `on`
//! in any case, and anything else is false (`1.0` too: its text is not `"1"`); a number may come
//! as a string (`" 3 "`, `"1e2"`); an order's `"default"` is kept as an override where the
//! default is `null`; a list takes `"default"` though it has no presets.
//!
//! What differs, on purpose (DESIGN.md P2.3.2; the table's `fix` cases):
//! - **int-integral.** An `int` must be integral: `2`, `2.0` and `"2"` are 2, and `2.5` is
//!   refused. Python kept a fractional value as a float, which turned the bot's `//` into float
//!   floor division. It must also fit `i32`, which `Params` holds, where Python made a big integer
//!   of it.
//! - **names-known.** The names in an `order` or a `list` must be among the parameter's
//!   `options`; Python accepted any string. A string is still a preset's name or `"default"`.
//! - **dropped.** `site_cache_turns` and `bv_cache_turns` tuned caches basic-1 does not keep
//!   (P2.3.9, fixes 5 and 6): stored profiles carry them, so they are dropped whatever their value
//!   rather than refused. Any other key basic-1 lacks is refused.
//! - **retyped.** The 41 parameters basic-1 types `float` that Python typed `int` by their
//!   default's literal give a float: `4` becomes `4.0`.
//! - **Finite numbers.** `"inf"` is refused for an `int`, where Python's `int()` raised an
//!   `OverflowError` that `clean_params` did not catch (`"nan"` it refused); `"inf"` and `"nan"`
//!   are refused for a `float`, where Python kept a value no JSON can hold.
//! - **Overrides are an object** (or `null`, none); Python's `dict()` of anything else raised a
//!   `TypeError` or a `ValueError` the caller did not expect.

use std::collections::BTreeMap;

use citar_engine::base::py;
use serde_json::{Map, Value};

use super::schema::{Fixed, Kind, Spec};
use super::{Overrides, ParamError, spec};
use crate::versions::{self, VersionId};

/// The keys basic-1 drops whatever their value (P2.3.9, fixes 5 and 6).
pub(crate) const RETIRED: [&str; 2] = ["site_cache_turns", "bv_cache_turns"];

/// `overrides` for version `v` (`basic` names the latest), cleaned.
pub(crate) fn clean(v: &str, overrides: &Value) -> Result<Overrides, ParamError> {
    let Some(version) = VersionId::resolve(v) else {
        return Err(ParamError::new(
            None,
            format!("'{v}' is not a bot version (the versions are {}).", versions::names()),
        ));
    };
    let empty = Map::new();
    let map = match overrides {
        Value::Null => &empty,
        Value::Object(m) => m,
        _ => return Err(ParamError::new(None, "Bot parameters are an object of {key: value}.")),
    };
    match version {
        // The idle bot has no parameters and ignores any, as Python's did.
        VersionId::Idle => Ok(Overrides::default()),
        VersionId::Basic1 => basic1(v, map),
    }
}

/// basic-1's overrides, in the order given, so the first bad key is the one named, as Python's
/// loop named it.
fn basic1(name: &str, map: &Map<String, Value>) -> Result<Overrides, ParamError> {
    let mut out = BTreeMap::new();
    for (k, v) in map {
        if RETIRED.contains(&k.as_str()) {
            continue;
        }
        let s = spec(k).ok_or_else(|| {
            ParamError::new(Some(k), format!("{k} is not a parameter of {name}."))
        })?;
        let value = coerce(s, v).ok_or_else(|| {
            ParamError::new(
                Some(k),
                format!("{} ({k}): {} is not a valid {}.", s.label, py::repr(v), s.kind.name()),
            )
        })?;
        if !is_default(s, &value) {
            out.insert(k.clone(), value);
        }
    }
    Ok(Overrides::cleaned(out))
}

/// `v` as parameter `s` takes it, or `None` when it refuses it.
fn coerce(s: &Spec, v: &Value) -> Option<Value> {
    match s.kind {
        Kind::Bool => Some(Value::Bool(truth(v))),
        Kind::Int => int(v).map(Value::from),
        Kind::Float => match v {
            Value::Bool(_) => None,
            _ => py::float_of(v).filter(|x| x.is_finite()).map(Value::from),
        },
        Kind::Choice => {
            let name = match v {
                Value::Null => None,
                Value::String(n) => Some(n.as_str()),
                _ => return None,
            };
            s.choices.contains(&name).then(|| v.clone())
        }
        Kind::Order | Kind::List => match v {
            Value::Null => Some(Value::Null),
            Value::String(n) => {
                (n == "default" || s.preset(n).is_some()).then(|| Value::String(n.clone()))
            }
            Value::Array(names) => names
                .iter()
                .all(|x| x.as_str().is_some_and(|n| s.options.contains(&n)))
                .then(|| v.clone()),
            _ => None,
        },
    }
}

/// A bool as `clean_params` read one: itself, or whether its text, in lower case, is `1`,
/// `true`, `yes` or `on`. Python's text of a number is its `repr`, so only the integer 1 is
/// true among numbers; of null, a list or an object, never.
fn truth(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Value::Number(n) => n.as_u64() == Some(1),
        Value::Null | Value::Array(_) | Value::Object(_) => false,
    }
}

/// An int as basic-1 reads one: a number or numeric text that is integral and fits `i32`. A
/// boolean is refused, as Python refused it.
fn int(v: &Value) -> Option<i32> {
    match v {
        Value::Bool(_) => None,
        Value::Number(n) if n.as_i64().is_some() => n.as_i64().and_then(|i| i32::try_from(i).ok()),
        _ => {
            let f = py::float_of(v)?;
            #[allow(clippy::float_cmp, reason = "integral is exactly equal to its truncation")]
            let integral = f.is_finite() && f.trunc() == f;
            let fits = f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX);
            #[allow(clippy::cast_possible_truncation, reason = "integral and inside i32")]
            (integral && fits).then_some(f as i32)
        }
    }
}

/// Whether a cleaned value equals the parameter's default, as Python's `!=` compared them
/// (`1 == 1.0`; names by text; lists element by element).
fn is_default(s: &Spec, v: &Value) -> bool {
    match s.default {
        Fixed::Int(d) => v.as_i64() == Some(i64::from(d)),
        Fixed::Float(d) => v.as_f64().is_some_and(|x| same_float(x, d)),
        Fixed::Bool(d) => v.as_bool() == Some(d),
        Fixed::Name(d) => match v {
            Value::Null => d.is_none(),
            Value::String(n) => d == Some(n.as_str()),
            _ => false,
        },
        Fixed::Names(d) => v.as_array().is_some_and(|a| {
            a.len() == d.len() && a.iter().zip(d).all(|(x, &n)| x.as_str() == Some(n))
        }),
    }
}

/// Python's `==` on two floats: exactly equal.
#[allow(clippy::float_cmp, reason = "Python compared the parsed numbers exactly")]
fn same_float(a: f64, b: f64) -> bool {
    a == b
}
