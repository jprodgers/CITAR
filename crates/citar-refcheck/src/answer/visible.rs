//! The `visible` group (DESIGN.md 9.2, package 1c-01): the tiles each living major civilization
//! sees, as `visibility.visible_tiles(g, pid)` answered on a fresh load of the recorded state
//! (`scripts/refcheck/queries.py::visible`).
//!
//! The Rust side is the loaded game's incremental counts (`game::vis`), which `Game::from_python`
//! built from nothing in its settle, as Python's first refresh did.

use citar_engine::game::Game;
use serde_json::{Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `visible` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Visible;

impl AnswerModule for Visible {
    fn group(&self) -> Group {
        Group::Visible
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        Ok(json!({ "civs": civs(g) }))
    }
}

/// Every living major and the tiles it sees, by index, sorted (`queries._majors`).
fn civs(g: &Game) -> Vec<Value> {
    g.majors(true)
        .map(|p| {
            let tiles: Vec<u32> =
                g.derived().vis().visible(p.id()).map(|v| v.iter().collect()).unwrap_or_default();
            json!({ "pid": p.id().0, "tiles": tiles })
        })
        .collect()
}
