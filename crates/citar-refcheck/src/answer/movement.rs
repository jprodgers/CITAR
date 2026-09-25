//! The `movement` group (DESIGN.md 9.2, package 1c-02): where units can get this turn, and paths
//! with their turns and step costs (`scripts/refcheck/queries.py::movement`).
//!
//! Python sampled the units and the targets and wrote them next to each answer, so the Rust side
//! asks the same questions of the same units: `reachable_this_turn` with `max_moves` for each
//! sampled unit, and for each sampled path `find_path` with a turn limit of 40, then `path_turns`
//! and `enter_cost` for each step. Reachable tiles are compared exactly; a path is compared as a
//! route (`PathEquivalent`: the same ends, adjacent steps, the same turns and summed cost), though
//! the engine walks back its path as Python's search chose it, so the tiles agree too.

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::{Game, movement};
use serde_json::{Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `movement` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Movement;

impl AnswerModule for Movement {
    fn group(&self) -> Group {
        Group::Movement
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let list =
            |key: &str| expected.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let reachable: Vec<Value> = list("reachable").iter().map(|e| reach(g, e)).collect();
        let paths: Vec<Value> = list("paths").iter().map(|e| path(g, e)).collect();
        Ok(json!({ "reachable": reachable, "paths": paths }))
    }
}

/// The unit an entry names.
fn unit_of(e: &Value) -> Option<UnitId> {
    e.get("unit").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).and_then(UnitId::new)
}

/// What an entry says of the unit, from the game: its id, type, owner, tile and moves.
fn head(g: &Game, u: UnitId) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let Some(x) = g.unit(u) else { return m };
    m.insert("unit".into(), json!(u.get()));
    m.insert("type".into(), json!(g.rules().name(x.base).unwrap_or("")));
    m.insert("owner".into(), json!(x.owner().0));
    m.insert("from".into(), json!(x.tile().0));
    m.insert("moves".into(), json!(x.moves));
    m
}

/// One unit's reachable tiles: `[tile, movement left]` by tile.
fn reach(g: &Game, e: &Value) -> Value {
    let Some(u) = unit_of(e) else { return Value::Null };
    let mut m = head(g, u);
    if m.is_empty() {
        return Value::Null;
    }
    m.insert("max_moves".into(), json!(movement::max_moves(g, u)));
    let tiles: Vec<Value> =
        movement::reachable_this_turn(g, u).into_iter().map(|(t, l)| json!([t.0, l])).collect();
    m.insert("reachable".into(), Value::Array(tiles));
    Value::Object(m)
}

/// One sampled path: the path to the recorded target, and when there is one its turns and step
/// costs.
fn path(g: &Game, e: &Value) -> Value {
    let Some(u) = unit_of(e) else { return Value::Null };
    let mut m = head(g, u);
    if m.is_empty() {
        return Value::Null;
    }
    let Some(to) = e.get("to").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) else {
        return Value::Object(m);
    };
    let to = TileIdx(to);
    let from = m.shift_remove("from").unwrap_or(Value::Null);
    let moves = m.shift_remove("moves").unwrap_or(Value::Null);
    m.insert("from".into(), from);
    m.insert("to".into(), json!(to.0));
    m.insert("moves".into(), moves);
    match movement::find_path(g, u, to, 40) {
        None => {
            m.insert("path".into(), Value::Null);
        }
        Some(p) => {
            let costs: Vec<i32> =
                p.windows(2).map(|w| movement::enter_cost(g, u, w[0], w[1])).collect();
            m.insert("path".into(), json!(p.iter().map(|t| t.0).collect::<Vec<_>>()));
            m.insert("turns".into(), json!(movement::path_turns(g, u, &p)));
            m.insert("step_costs".into(), json!(costs));
        }
    }
    Value::Object(m)
}
