//! The `tile_yields` group (DESIGN.md 9.2): what tiles yield (`tiles.tile_stats`).
//!
//! Python recorded every owned tile as its owner sees it with its own city working it, and a
//! sample of unowned tiles as nobody and as the first living major civilization see them. The
//! answer replays those inputs, each through the engine's tile yield memos (package 1b-06).

use citar_engine::base::ids::{CityId, PlayerId, TileIdx};
use citar_engine::base::stats::{Stat, Stats};
use citar_engine::game::{Game, query};
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `tile_yields` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct TileYields;

impl AnswerModule for TileYields {
    fn group(&self) -> Group {
        Group::TileYields
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let replay = |key: &str| -> Result<Vec<Value>, AnswerError> {
            let list = expected
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| AnswerError::new(format!("the recording has no {key} list")))?;
            list.iter().map(|e| entry(g, e)).collect()
        };
        Ok(json!({"owned": replay("owned")?, "sample": replay("sample")?}))
    }
}

/// One recorded tile, answered: its index, viewer and city as Python gave them, and the yields.
fn entry(g: &Game, e: &Value) -> Result<Value, AnswerError> {
    let idx = e
        .get("idx")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| AnswerError::new("a recorded tile has no idx"))?;
    let pid = e.get("pid").and_then(Value::as_u64).and_then(|n| u8::try_from(n).ok()).map(PlayerId);
    let city = e
        .get("city")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new);
    let y = query::tile_yield(g, TileIdx(idx), pid, city);
    Ok(json!({
        "idx": idx,
        "pid": pid.map(|p| p.0),
        "city": city.map(CityId::get),
        "yields": yields(&y),
    }))
}

/// Seven stats as Python's `zero()` dict held them: every key.
pub fn yields(s: &Stats) -> Value {
    Value::Object(
        Stat::ALL.into_iter().map(|k| (k.key().to_owned(), json!(s[k]))).collect::<Map<_, _>>(),
    )
}
