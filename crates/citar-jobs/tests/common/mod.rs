//! The specs' fuzzing bases and mutations (design/phase4-helper.md P4.6), shared by the spec
//! parser's test (`tests/specs.rs`, 4-07) and the job host's, through its standard input
//! (`tests/host.rs`, 4-08). The bases hold list and order overrides (and a level that is one),
//! which a seat may carry.

#![allow(dead_code, reason = "each test binary uses its part")]

use serde_json::{Map, Value, json};

/// A good lab spec with every kind of field.
pub fn lab() -> Value {
    json!({
        "exp": "fuzz", "i": 3, "seed": 1003, "map_type": "pangaea", "size": "duel",
        "speed": "Quick", "turns": 12, "difficulty": "Prince", "barbarians": "normal",
        "barbarian_difficulty": "King", "nation": null, "city_states": 1,
        "seats": [
            {"label": "a", "bot": "basic-1", "aggression": 0.5,
             "params": {"tech_noise": 0.2, "promo_lines": ["Shock", "Drill"],
                        "policy_order_peaceful": ["Liberty", "Tradition"]},
             "profile": "standard", "profile_rev": 1, "profile_name": "Standard",
             "levels": {"tech_noise": 0.2, "promo_lines": ["Shock", "Drill"]},
             "difficulty": "Warlord", "nation": "Rome"},
            {"label": "b", "bot": "idle"}
        ],
        "ai_base_values": "unciv", "map_edges": "wrap_x", "river_density": 1.2,
        "resources": {"density": 1.0, "luxury": {"density": 0.5,
                      "each": {"Silk": {"mode": "share", "value": 2}}}}
    })
}

/// A good reference spec.
pub fn reference() -> Value {
    json!({"i": 0, "seed": 5000, "size": "duel", "map_type": "continents", "barbarians": "normal",
           "speed": "Quick", "turn_limit": 12,
           "params": {"tech_noise": 0.1, "beliefs_pantheon": ["Fertility Rites", "Goddess of Love"]}})
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
pub fn junk(pick: u8, n: u64) -> Value {
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
pub fn mutate(base: &Value, which: usize, op: u8, pick: u8, n: u64) -> Value {
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
