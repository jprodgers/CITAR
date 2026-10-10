//! The ruleset as the browser and AI agents read it: Python's `Rules.to_client`
//! (`rules.py:303-331`), key for key and in the same order.
//!
//! The objects are the files' own JSON, not re-serialised typed structs, so every field a mod
//! adds that the engine does not type still reaches the browser, numbers keep the form they were
//! written in, and the tables keep document order, which the browser iterates. On top of the
//! files' values go the defaults Python set at load (`rules.py:119-122, 132-133, 137, 148, 154`)
//! and the derived lists.

use serde_json::{Map, Value, json};

use super::Ruleset;
use super::constants::{PLAYER_COLORS, RULES_VERSION};
use super::source::GAME;

/// The files' JSON, kept for the client: the tables as parsed, and the merged nations.
#[derive(Debug, Default)]
pub(crate) struct ClientSource {
    pub docs: Vec<(&'static str, Value)>,
    pub nations: Map<String, Value>,
}

impl ClientSource {
    fn doc(&self, file: &str) -> Option<&Value> {
        self.docs.iter().find(|(n, _)| *n == file).map(|(_, v)| v)
    }

    /// A table's rows: a whole file, or the part `part` of it.
    fn rows(&self, file: &str, part: Option<&str>) -> Option<&Map<String, Value>> {
        let doc = self.doc(file)?;
        match part {
            Some(p) => doc.get(p)?.as_object(),
            None => doc.as_object(),
        }
    }
}

/// A default Python set with `setdefault`: added at the end, and only if absent.
type Default = (&'static str, fn() -> Value);

/// One table as `to_client`'s `table()` wrote it: every row without its private `_` fields,
/// with Python's load-time defaults.
fn table(rows: Option<&Map<String, Value>>, defaults: &[Default]) -> Value {
    let mut out = Map::new();
    for (key, row) in rows.into_iter().flatten() {
        if key.starts_with('_') {
            continue;
        }
        out.insert(key.clone(), strip(row, defaults, &[]));
    }
    Value::Object(out)
}

/// A row without its `_` fields and the fields in `drop`, with `defaults` added where absent.
fn strip(row: &Value, defaults: &[Default], drop: &[&str]) -> Value {
    let Some(obj) = row.as_object() else { return row.clone() };
    let mut out: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !k.starts_with('_') && !drop.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in defaults {
        if !out.contains_key(*k) {
            out.insert((*k).to_owned(), v());
        }
    }
    Value::Object(out)
}

fn names<'a>(it: impl IntoIterator<Item = &'a str>) -> Value {
    Value::Array(it.into_iter().map(|n| Value::String(n.to_owned())).collect())
}

/// Python's truthiness of a JSON value, for `if v` on a barbarian level (`rules.py:329-330`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The client JSON of `r`.
pub(crate) fn build(r: &Ruleset) -> Value {
    let src = &r.client;
    let game = src.doc(GAME).and_then(Value::as_object);
    let from_game = |k: &str| game.and_then(|g| g.get(k)).cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    let mut put = |k: &str, v: Value| {
        out.insert(k.to_owned(), v);
    };

    put("version", json!(RULES_VERSION));
    put("eras", table(src.rows("ruleset/eras.json", None), &[]));
    // Eras are in order of their number (the loader requires it), which is how Python sorted
    // them.
    put("era_list", names(r.eras.as_slice().iter().map(|e| &*e.name)));
    put("techs", table(src.rows("ruleset/techs.json", Some("techs")), &[]));
    put("tech_order", names(r.derived.tech_order.iter().map(|&t| &*r.techs[t].name)));
    let mut unlocks = Map::new();
    for (t, u) in r.derived.unlocks.iter() {
        unlocks.insert(
            r.techs[t].name.to_string(),
            json!({
                "units": names(u.units.iter().map(|&x| &*r.base_units[x].name)),
                "buildings": names(u.buildings.iter().map(|&x| &*r.buildings[x].name)),
                "improvements": names(u.improvements.iter().map(|&x| &*r.improvements[x].name)),
                "reveals": names(u.reveals.iter().map(|&x| &*r.resources[x].name)),
                "policies": [],
            }),
        );
    }
    put("unlocks", Value::Object(unlocks));
    put(
        "units",
        table(
            src.rows("ruleset/units.json", None),
            &[
                ("strength", || json!(0)),
                ("rangedStrength", || json!(0)),
                ("range", || json!(2)),
                ("cost", || json!(0)),
            ],
        ),
    );
    put("unit_types", table(src.rows("ruleset/unit_types.json", None), &[]));
    put(
        "promotions",
        table(
            src.rows("ruleset/promotions.json", None),
            &[("prerequisites", || json!([])), ("unitTypes", || json!([]))],
        ),
    );
    put("buildings", table(src.rows("ruleset/buildings.json", None), &[("cost", || json!(-1))]));
    put(
        "terrains",
        table(src.rows("ruleset/terrains.json", None), &[("movementCost", || json!(1))]),
    );
    put("resources", table(src.rows("ruleset/resources.json", None), &[]));
    put("improvements", table(src.rows("ruleset/improvements.json", None), &[]));
    put("beliefs", table(src.rows("ruleset/beliefs.json", None), &[]));
    // Python set each branch's `branch` to its own name (`rules.py:154`).
    let mut branches = Map::new();
    for (key, row) in src.rows("ruleset/policies.json", Some("branches")).into_iter().flatten() {
        if key.starts_with('_') {
            continue;
        }
        let mut row = strip(row, &[], &[]);
        if let Some(obj) = row.as_object_mut() {
            obj.insert("branch".to_owned(), Value::String(key.clone()));
        }
        branches.insert(key.clone(), row);
    }
    put("policy_branches", Value::Object(branches));
    put("policies", table(src.rows("ruleset/policies.json", Some("policies")), &[]));
    let mut nations = Map::new();
    for (key, row) in &src.nations {
        nations.insert(key.clone(), strip(row, &[], &["cities"]));
    }
    put("nations", Value::Object(nations));
    put("major_nations", names(r.derived.major_nations.iter().map(|&n| &*r.nations[n].name)));
    put("specialists", table(src.rows("ruleset/specialists.json", None), &[]));
    put("city_state_types", table(src.rows("ruleset/city_state_types.json", None), &[]));
    put("difficulties", table(src.rows("ruleset/difficulties.json", None), &[]));
    put("difficulty_list", names(r.difficulty_names()));
    let speeds = src.rows("ruleset/speeds.json", None);
    put("speeds", table(speeds, &[]));
    put("victories", table(src.rows("ruleset/victories.json", None), &[]));
    for k in ["move_scale", "map_sizes", "map_types", "max_players"] {
        put(k, from_game(k));
    }
    put("player_colors", json!(PLAYER_COLORS));
    for k in ["default_speed", "default_difficulty", "benchmark_speed"] {
        put(k, from_game(k));
    }
    // The last entry of each speed's calendar, as written (`rules.py:228`).
    let mut max_turns = Map::new();
    for (key, row) in speeds.into_iter().flatten() {
        let last = row.get("turns").and_then(Value::as_array).and_then(|t| t.last());
        let until = last.and_then(|t| t.get("untilTurn")).cloned().unwrap_or(Value::Null);
        max_turns.insert(key.clone(), until);
    }
    put("max_turns", Value::Object(max_turns));
    let levels = game
        .and_then(|g| g.get("barbarians"))
        .and_then(|b| b.get("levels"))
        .and_then(Value::as_object);
    let mut barbarian_levels = Map::new();
    let mut barbarian_aggression = Map::new();
    for (key, level) in levels.into_iter().flatten() {
        if truthy(level) {
            barbarian_levels.insert(key.clone(), level.get("name").cloned().unwrap_or(Value::Null));
            barbarian_aggression
                .insert(key.clone(), level.get("aggression").cloned().unwrap_or_else(|| json!(50)));
        } else {
            barbarian_levels.insert(key.clone(), json!("Off"));
        }
    }
    put("barbarian_levels", Value::Object(barbarian_levels));
    put("barbarian_aggression", Value::Object(barbarian_aggression));

    Value::Object(out)
}
