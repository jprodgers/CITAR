//! The `uniques` group (DESIGN.md 9.2, package 1a-05): every unique text of the ruleset, as the
//! Rust compiler compiled it, against how the Python engine read it.
//!
//! The Python side is `refcheck/uniques.json.gz`, recorded by `scripts/refcheck/uniques_dump.py`:
//! one record per text, with its type and placeholder, each parameter as Python read it (numbers
//! for amounts, all seven stats for stats, text for names and filters), whether it is local, its
//! timer, and its modifiers. The Rust answer writes the same record for the unique compiled from
//! the same position of the same object, from the compiled data alone: types from the payload,
//! parameters from their ids and handles, modifiers from the conditionals, the trigger and what
//! the compiler folded. The group runs once per run, not per fixture: the ruleset is the same in
//! every one.

use std::borrow::Cow;

use citar_engine::base::stats::Stat;
use citar_engine::rules::Ruleset;
use citar_engine::unique::params::{Param, ParamValue};
use citar_engine::unique::{SourceUniques, UFlags, UniqueType};
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;
use crate::fixture::read_gz;

/// Where the recording lives, from the repository root.
pub const RECORDING: &str = "refcheck/uniques.json.gz";

/// The `uniques` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Uniques;

impl AnswerModule for Uniques {
    fn group(&self) -> Group {
        Group::Uniques
    }

    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        let path = cx.root.join(RECORDING);
        let bytes = read_gz(&path).map_err(|e| {
            AnswerError::new(format!(
                "{}: {e} (record it with scripts/refcheck/uniques_dump.py)",
                path.display()
            ))
        })?;
        serde_json::from_slice(&bytes)
            .map(Cow::Owned)
            .map_err(|e| AnswerError::new(format!("{RECORDING} is not JSON: {e}")))
    }

    fn answer(&self, _cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        answer(Ruleset::shared(), expected)
    }
}

/// Every source object's uniques, by (source kind, object name).
fn sources(r: &Ruleset) -> Vec<(&'static str, &str, &SourceUniques)> {
    let mut out: Vec<(&'static str, &str, &SourceUniques)> = Vec::new();
    out.extend(r.nations().as_slice().iter().map(|x| ("Nation", &*x.name, &x.uniques)));
    out.extend(r.buildings().as_slice().iter().map(|x| ("Building", &*x.name, &x.uniques)));
    out.extend(r.policies().as_slice().iter().map(|x| ("Policy", &*x.name, &x.uniques)));
    out.extend(r.techs().as_slice().iter().map(|x| ("Tech", &*x.name, &x.uniques)));
    out.extend(r.eras().as_slice().iter().map(|x| ("Era", &*x.name, &x.uniques)));
    let cs = r.city_state_types().as_slice();
    out.extend(cs.iter().map(|x| ("CityStateFriend", &*x.name, &x.friend)));
    out.extend(cs.iter().map(|x| ("CityStateAlly", &*x.name, &x.ally)));
    out.extend(cs.iter().map(|x| ("CityStateType", &*x.name, &x.uniques)));
    out.extend(r.beliefs().as_slice().iter().map(|x| ("Belief", &*x.name, &x.uniques)));
    out.extend(r.resources().as_slice().iter().map(|x| ("Resource", &*x.name, &x.uniques)));
    out.push(("Global", "", r.global_uniques()));
    out.extend(r.terrains().as_slice().iter().map(|x| ("Terrain", &*x.name, &x.uniques)));
    out.extend(r.improvements().as_slice().iter().map(|x| ("Improvement", &*x.name, &x.uniques)));
    out.extend(r.unit_types().as_slice().iter().map(|x| ("UnitType", &*x.name, &x.uniques)));
    out.extend(r.base_units().as_slice().iter().map(|x| ("Unit", &*x.name, &x.uniques)));
    out.extend(r.promotions().as_slice().iter().map(|x| ("Promotion", &*x.name, &x.uniques)));
    out.extend(r.ruins().as_slice().iter().map(|x| ("Ruins", &*x.name, &x.uniques)));
    out
}

/// The Rust records for the Python records of `expected`, in the same order and shape.
pub fn answer(r: &Ruleset, expected: &Value) -> Result<Value, AnswerError> {
    let rows = expected
        .get("uniques")
        .and_then(Value::as_array)
        .ok_or_else(|| AnswerError::new(format!("{RECORDING} has no `uniques` list")))?;
    let sources = sources(r);
    let find = |kind: &str, name: &str| {
        sources.iter().find(|(k, n, _)| *k == kind && *n == name).map(|(_, _, u)| *u)
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default();
        let (kind, name) = (field("source"), field("name"));
        let index = row.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX);
        let id = find(kind, name).and_then(|u| {
            let i = u16::try_from(index).ok()?;
            let at = u.all.start.checked_add(i)?;
            u.all.contains(&at).then_some(at)
        });
        out.push(match id {
            None => json!({"id": field("id"), "missing": "Rust compiled no unique here"}),
            Some(id) => record(r, citar_engine::base::ids::UniqueId(id), row),
        });
    }
    // Every compiled unique that is not a timed unique's variant stands for one text.
    let count = r.uniques().iter().filter(|(_, u)| !u.flags().contains(UFlags::TEMPORARY)).count();
    Ok(json!({"format": 1, "count": count, "uniques": out}))
}

/// One compiled unique written as the Python record `row` is.
fn record(r: &Ruleset, id: citar_engine::base::ids::UniqueId, row: &Value) -> Value {
    let table = r.uniques();
    let u = table.get(id);
    let meta = table.meta(id);
    let text = table.text_of(id);
    let (ty, placeholder) = match meta.ty {
        Some(t) => (Value::from(t.name()), t.placeholder().to_owned()),
        None => (Value::Null, text.to_owned()),
    };
    let modifiers: Vec<Value> =
        table.modifiers(id).into_iter().map(|(t, params)| modifier(r, t, &params)).collect();
    let mut o = Map::new();
    for k in ["id", "source", "name", "index"] {
        o.insert(k.into(), row.get(k).cloned().unwrap_or(Value::Null));
    }
    o.insert("text".into(), text.into());
    o.insert("type".into(), ty);
    o.insert("placeholder".into(), placeholder.into());
    o.insert("params".into(), params(r, &u.data.params()));
    o.insert("local".into(), u.flags().contains(UFlags::LOCAL).into());
    o.insert("timed".into(), meta.timed.map_or(Value::Null, Value::from));
    o.insert("modifiers".into(), modifiers.into());
    Value::Object(o)
}

fn modifier(r: &Ruleset, t: UniqueType, p: &[Param]) -> Value {
    json!({"type": t.name(), "placeholder": t.placeholder(), "params": params(r, p)})
}

fn params(r: &Ruleset, p: &[Param]) -> Value {
    p.iter().map(|&x| value(x.value(r))).collect::<Vec<_>>().into()
}

/// A parameter as the recording writes it: stats as all seven keys, in Python's order.
fn value(v: ParamValue) -> Value {
    match v {
        ParamValue::Int(n) => n.into(),
        ParamValue::Real(x) => x.into(),
        ParamValue::Text(s) => s.into(),
        ParamValue::Stats(s) => {
            let mut o = Map::new();
            for st in Stat::ALL {
                o.insert(st.key().into(), s[st].into());
            }
            Value::Object(o)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_is_listed_once() {
        let r = Ruleset::shared();
        let s = sources(r);
        let mut keys: Vec<(&str, &str)> = s.iter().map(|(k, n, _)| (*k, *n)).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "a (kind, name) pair twice");
        let texts: usize = s.iter().map(|(_, _, u)| u.all.len()).sum();
        assert_eq!(texts, 1615, "DESIGN.md 5.1");
    }

    #[test]
    fn the_recording_agrees_and_a_changed_parameter_shows() {
        use crate::compare::spec::CompareSpec;
        use crate::compare::{Options, compare};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let cx = Ctx { root: &root, fixture: None };
        let expected = Uniques.expected(&cx).expect("the recording").into_owned();
        let actual = Uniques.answer(&cx, &expected).expect("the answer");
        let spec = CompareSpec::for_group(Group::Uniques);
        let opts = Options { with_bot: false, grid: None };
        assert_eq!(expected["count"], 1615);
        assert_eq!(compare(&spec, &expected, &actual, &opts), Vec::new());
        // Babylon's great scientists come 50% faster; say 51 and the group sees it.
        let mut changed = expected.clone();
        assert_eq!(changed["uniques"][1]["id"], "Nation/Babylon/1");
        changed["uniques"][1]["params"][1] = json!(51.0);
        changed["uniques"][0]["modifiers"][0]["params"][0] = json!("Pottery");
        let diffs = compare(&spec, &changed, &actual, &opts);
        // The number, and the modifier as one missing and one extra element of a multiset.
        assert_eq!(diffs.len(), 3, "{diffs:?}");
    }

    #[test]
    fn a_record_the_ruleset_has_not_is_missing() {
        let r = Ruleset::shared();
        let expected = json!({"uniques": [{"id": "Building/Nowhere/0", "source": "Building",
                                           "name": "Nowhere", "index": 0}]});
        let got = answer(r, &expected).expect("an answer");
        assert!(got["uniques"][0]["missing"].is_string(), "{got}");
    }
}
