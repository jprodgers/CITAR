//! The `tool_errors` group (DESIGN.md 9.2, package 1d-01): about thirty tool calls that should
//! be refused, each for a different reason, and the text each refusal gives
//! (`scripts/refcheck/queries.py::tool_errors`, through `tools.execute`).
//!
//! Python chose the callers and the arguments and wrote them next to each answer, so the Rust
//! side makes the same calls through `Game::execute`. The group's calls change the game when one
//! unexpectedly succeeds (Python reloaded the state after it), so they run on a copy of the
//! loaded game, and a call that succeeds is answered with its result and followed by a fresh
//! copy; a refusal changes nothing (property P2), so the copy serves the next call as it is. That
//! is checked on every refusal, by the revision and the state's digest, and a refusal that
//! changed the game fails the answer with the call named.

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `tool_errors` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolErrors;

impl AnswerModule for ToolErrors {
    fn group(&self) -> Group {
        Group::ToolErrors
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let base = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let calls = expected.get("calls").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let mut g = base.clone();
        let mut out = Vec::with_capacity(calls.len());
        for call in calls {
            let (answer, changed) = call_once(&mut g, call)?;
            if changed {
                g = base.clone();
            }
            out.push(answer);
        }
        Ok(json!({ "calls": out }))
    }
}

/// One recorded call made again: its caller, tool and arguments as recorded, then its result
/// (`ok`) or its refusal (`error`); and whether it changed the game.
fn call_once(g: &mut Game, call: &Value) -> Result<(Value, bool), AnswerError> {
    let pid = call.get("pid").and_then(Value::as_u64).ok_or_else(|| missing("pid", call))?;
    let tool = call.get("tool").and_then(Value::as_str).ok_or_else(|| missing("tool", call))?;
    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
    // A caller no game can hold is refused as an invalid player, as Python refused it.
    let pid = PlayerId(u8::try_from(pid).unwrap_or(u8::MAX));
    let mut out = Map::new();
    out.insert("pid".into(), call["pid"].clone());
    out.insert("tool".into(), json!(tool));
    out.insert("args".into(), args.clone());
    let before = (g.rev(), g.digest().ok());
    let changed = match g.execute(pid, tool, &args) {
        Ok(done) => {
            out.insert("ok".into(), done.result);
            true
        }
        Err(e) => {
            // The copy serves the next call only if the refusal left it as it was; one that did
            // not is named here, rather than showing as differences on the innocent calls after.
            if (g.rev(), g.digest().ok()) != before {
                return Err(AnswerError::new(format!(
                    "a refused call changed the game (property P2): {call}: {}",
                    e.message
                )));
            }
            out.insert("error".into(), json!(e.message));
            false
        }
    };
    Ok((Value::Object(out), changed))
}

fn missing(key: &str, call: &Value) -> AnswerError {
    AnswerError::new(format!("a tool call without its {key}: {call}"))
}
