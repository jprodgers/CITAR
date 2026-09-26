//! The `views` group (DESIGN.md 9.2): what the browser receives for the first two living major
//! civilizations (`views.client_view(g, pid)`).
//!
//! Package 1d-02 answers it whole with `Game::view_json`, the bytes a host sends, read back as
//! JSON: the map as each civilization knows it, the units and cities it sees and remembers,
//! everyone it knows of, the settings, its events scrubbed for it, its empire, its diplomacy,
//! its notes and its alerts.

use serde_json::{Value, json};

use citar_engine::base::ids::PlayerId;

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// How many events the recorded views asked for (`client_view`'s default).
const EVENT_LIMIT: u32 = 150;

/// The `views` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Views;

impl AnswerModule for Views {
    fn group(&self) -> Group {
        Group::Views
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let civs = expected
            .get("civs")
            .and_then(Value::as_array)
            .ok_or_else(|| AnswerError::new("the recorded views have no civs"))?;
        let mut out = Vec::new();
        for c in civs {
            let pid = c
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| AnswerError::new("a recorded view has no pid"))?;
            let bytes = g.view_json(Some(PlayerId(pid)), EVENT_LIMIT);
            let view: Value = serde_json::from_slice(&bytes)
                .map_err(|e| AnswerError::new(format!("the view of {pid} is not JSON: {e}")))?;
            out.push(json!({"pid": pid, "view": view}));
        }
        Ok(json!({ "civs": out }))
    }
}
