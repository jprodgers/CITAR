//! The specs' robustness (design/phase4-helper.md P4.6, 4-07's gate 5): 2,000 mutated `LabSpec`
//! and 2,000 mutated `ReferenceSpec` values through `check_spec` never panic it; whatever is
//! refused is refused with a reason (`bad_spec`), and whatever is accepted hashes again to its own
//! hash from its canonical form. The frames that carry them were fuzzed in 4-01, where
//! `job_offer.spec` is an opaque object; the job host's standard input is 4-08's.

use citar_engine::rules::Ruleset;
use citar_jobs::{Kind, check_spec};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

fn lab() -> Value {
    json!({
        "exp": "fuzz", "i": 3, "seed": 1003, "map_type": "pangaea", "size": "duel",
        "speed": "Quick", "turns": 12, "difficulty": "Prince", "barbarians": "normal",
        "barbarian_difficulty": "King", "nation": null, "city_states": 1,
        "seats": [
            {"label": "a", "bot": "basic-1", "params": {"tech_noise": 0.2}, "aggression": 0.5,
             "profile": "standard", "profile_rev": 1, "profile_name": "Standard",
             "levels": {"tech_noise": 0.2}, "difficulty": "Warlord", "nation": "Rome"},
            {"label": "b", "bot": "idle"}
        ],
        "ai_base_values": "unciv", "map_edges": "wrap_x", "river_density": 1.2,
        "resources": {"density": 1.0, "luxury": {"density": 0.5,
                      "each": {"Silk": {"mode": "share", "value": 2}}}}
    })
}

fn reference() -> Value {
    json!({"i": 0, "seed": 5000, "size": "duel", "map_type": "continents", "barbarians": "normal",
           "speed": "Quick", "turn_limit": 12, "params": {"tech_noise": 0.1}})
}

/// Every path to a value in `v`, as keys and indexes.
fn paths(v: &Value, here: &mut Vec<Value>, out: &mut Vec<Vec<Value>>) {
    out.push(here.clone());
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                here.push(json!(k));
                paths(x, here, out);
                here.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                here.push(json!(i));
                paths(x, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

fn at<'a>(v: &'a mut Value, path: &[Value]) -> Option<&'a mut Value> {
    let mut cur = v;
    for step in path {
        cur = match step {
            Value::String(k) => cur.get_mut(k.as_str())?,
            Value::Number(n) => cur.get_mut(usize::try_from(n.as_u64()?).ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// A value of some other kind: what a hostile or broken server could put anywhere.
fn junk(pick: u8, n: u64) -> Value {
    match pick % 14 {
        0 => Value::Null,
        1 => json!(n.is_multiple_of(2)),
        2 => json!(n),
        3 => json!(-(i64::try_from(n % 1_000_000).unwrap_or(0)) - 1),
        4 => json!(u64::MAX),
        5 => json!(1e308),
        6 => json!(-0.0),
        7 => json!("NaN"),
        8 => json!("x".repeat(usize::try_from(n % 400).unwrap_or(0))),
        9 => json!([n, "y", null]),
        10 => json!({"path": "/etc/passwd", "code": "rm -rf /"}),
        11 => {
            let mut deep = json!(1);
            for _ in 0..(n % 60) {
                deep = json!({"d": deep});
            }
            deep
        }
        12 => json!(0.1 + f64::from(u32::try_from(n % 1000).unwrap_or(0))),
        _ => json!("basic-99"),
    }
}

/// `base` with one mutation at the `which`-th path: replaced by junk, removed, or given an
/// unknown key.
fn mutate(base: &Value, which: usize, op: u8, pick: u8, n: u64) -> Value {
    let mut v = base.clone();
    let mut all = Vec::new();
    paths(&v, &mut Vec::new(), &mut all);
    let path = all[which % all.len()].clone();
    match op % 4 {
        0 => {
            if let Some(slot) = at(&mut v, &path) {
                *slot = junk(pick, n);
            }
        }
        1 => {
            if let Some((last, parent)) = path.split_last()
                && let Some(p) = at(&mut v, parent)
            {
                match (p, last) {
                    (Value::Object(m), Value::String(k)) => {
                        m.remove(k);
                    }
                    (Value::Array(a), Value::Number(i)) => {
                        if let Some(i) = i.as_u64().and_then(|i| usize::try_from(i).ok())
                            && i < a.len()
                        {
                            a.remove(i);
                        }
                    }
                    _ => {}
                }
            }
        }
        2 => {
            if let Some(Value::Object(m)) = at(&mut v, &path) {
                m.insert(format!("unknown_{n}"), junk(pick, n));
            }
        }
        _ => {
            // Many seats, or a seat list replaced by a map.
            if let Some(Value::Array(a)) = v.get_mut("seats") {
                let seat = a.first().cloned().unwrap_or(Value::Object(Map::new()));
                a.extend(std::iter::repeat_n(seat, usize::try_from(n % 40).unwrap_or(0)));
            } else if let Some(slot) = at(&mut v, &path) {
                *slot = junk(pick, n);
            }
        }
    }
    v
}

fn check(kind: Kind, spec: &Value) -> Result<(), TestCaseError> {
    let rules = Ruleset::shared();
    match check_spec(rules, kind, spec, None) {
        Ok(c) => {
            // Accepted: its canonical form is accepted again, with the same hash.
            let again = check_spec(rules, kind, &c.canonical_value(), Some(&c.hash.to_hex()));
            prop_assert!(again.is_ok(), "{spec}: {again:?}");
        }
        Err(e) => prop_assert!(!e.0.is_empty(), "a refusal says why"),
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2000,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn mutated_lab_specs_are_refused_with_a_reason_or_accepted_and_never_panic(
        which in 0usize..400, op in any::<u8>(), pick in any::<u8>(), n in any::<u64>(),
        ranked in any::<bool>(),
    ) {
        let kind = if ranked { Kind::RankingGame } else { Kind::LabGame };
        check(kind, &mutate(&lab(), which, op, pick, n))?;
    }

    #[test]
    fn mutated_reference_specs_are_refused_with_a_reason_or_accepted_and_never_panic(
        which in 0usize..100, op in any::<u8>(), pick in any::<u8>(), n in any::<u64>(),
    ) {
        check(Kind::ReferenceGame, &mutate(&reference(), which, op, pick, n))?;
    }
}

/// The base specs are good, and the mutations reach refusals of each sort.
#[test]
fn the_fuzzing_reaches_the_refusals() {
    let rules = Ruleset::shared();
    assert!(check_spec(rules, Kind::LabGame, &lab(), None).is_ok());
    assert!(check_spec(rules, Kind::ReferenceGame, &reference(), None).is_ok());
    let mut reasons = std::collections::BTreeSet::new();
    for which in 0..60 {
        for op in 0..4u8 {
            for pick in 0..14u8 {
                let v = mutate(&lab(), which, op, pick, 7 + u64::from(pick));
                if let Err(e) = check_spec(rules, Kind::LabGame, &v, None) {
                    let r = &e.0;
                    for sort in
                        ["unknown field", "invalid type", "missing field", "more than", "not a"]
                    {
                        if r.contains(sort) {
                            reasons.insert(sort);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(reasons.len(), 5, "{reasons:?}");
}
