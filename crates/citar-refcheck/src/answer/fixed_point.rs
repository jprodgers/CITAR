//! The `fixed_point` group (DESIGN.md 9.2, package 1c-01): the settle on load changes neither
//! the explored tiles nor who has met whom.
//!
//! A synthetic group: the Python side is the recorded state itself, which Python saved after a
//! refresh (`visibility.refresh`, `visibility.py:128-168`), so every tile a civilization saw was
//! explored and every civilization it saw something of was met. The Rust side is the same
//! reading of the game `Game::from_python` loaded, whose settle built every civilization's sight
//! from nothing and applied what it showed. A tile explored or a meeting made by the port that
//! Python's rule did not make shows up here, where the `visible` group shows only the sight.
//!
//! Each player but the barbarians (whose explored tiles the converter drops, and who meet
//! nobody): its explored tiles by index, and the players it has met, by id.

use std::borrow::Cow;

use citar_engine::base::codec::b64_decode;
use citar_engine::game::Game;
use serde_json::{Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `fixed_point` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixedPoint;

impl AnswerModule for FixedPoint {
    fn group(&self) -> Group {
        Group::FixedPoint
    }

    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        let fixture = cx.fixture.ok_or_else(|| AnswerError::new("fixed_point needs a fixture"))?;
        let state: Value = serde_json::from_str(fixture.state.get())
            .map_err(|e| AnswerError::new(format!("the fixture's state is not JSON: {e}")))?;
        Ok(Cow::Owned(python(&state)?))
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        Ok(rust(g))
    }
}

/// The recorded state's explored tiles and meetings.
fn python(s: &Value) -> Result<Value, AnswerError> {
    let mut civs = Vec::new();
    for p in s["players"].as_array().map_or(&[][..], Vec::as_slice) {
        if p["kind"].as_str() == Some("barbarian") {
            continue;
        }
        let bytes = b64_decode(p["explored"].as_str().unwrap_or_default()).map_err(|e| {
            AnswerError::new(format!("player {}'s explored tiles are not base64: {e}", p["id"]))
        })?;
        let explored: Vec<u32> = bytes
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b != 0)
            .filter_map(|(i, _)| u32::try_from(i).ok())
            .collect();
        let mut met: Vec<u64> = p["met"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(Value::as_u64)
            .filter(|&q| Some(q) != p["id"].as_u64())
            .collect();
        met.sort();
        met.dedup();
        civs.push(json!({ "pid": p["id"], "explored": explored, "met": met }));
    }
    Ok(json!({ "civs": civs }))
}

/// The loaded game's explored tiles and meetings.
fn rust(g: &Game) -> Value {
    let st = g.state();
    let civs: Vec<Value> = st
        .players()
        .iter()
        .filter(|(_, p)| !p.is_barbarian())
        .map(|(id, p)| {
            let explored: Vec<u32> = p.explored.iter().collect();
            let met: Vec<u8> =
                st.players().ids().filter(|&q| q != id && g.has_met(id, q)).map(|q| q.0).collect();
            json!({ "pid": id.0, "explored": explored, "met": met })
        })
        .collect();
    json!({ "civs": civs })
}
