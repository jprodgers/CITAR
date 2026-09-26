//! The query tools' answers (`tools.py:200-406`), which change nothing.
//!
//! A query looks up what it names the way the actions do (`game::lookup`: the tile, the
//! caller's unit or city), so its refusals are the tools' own. Package 1d-01 answers the queries
//! that need no view: `read_notes`, `get_events` and `preview_attack`, and checks the arguments
//! of the rest. The others build their answers from the views' info builders and the briefing,
//! which packages 1d-02 (`api::views`) and 1d-03 (`api::briefing`) port; until then they refuse
//! with a `NotPorted` refusal that `cargo xtask check` counts.

use serde_json::{Map, Value, json};

use super::registry::Query;
use crate::base::ids::PlayerId;
use crate::base::py;
use crate::game::Game;
use crate::game::combat::resolve;
use crate::game::error::{ActionError, ErrCode};
use crate::game::lookup::{own_city, own_unit, tile_at};

/// The most events `get_events` returns: an agent that has not read its notifications for fifty
/// turns is not handed a wall of text in place of its turn (`tools.py:382-384`).
const EVENTS_SHOWN: usize = 40;

/// How far back `get_events` looks (`Game.events_for`'s default limit, `game.py:891`).
const EVENTS_SCANNED: usize = 200;

/// The topics of `get_rules`, in the order its refusal lists them (`views.rules_lookup`,
/// `views.py:633-648`).
const RULES_TOPICS: [&str; 19] = [
    "units",
    "buildings",
    "techs",
    "improvements",
    "resources",
    "promotions",
    "terrains",
    "terrain",
    "policies",
    "beliefs",
    "specialists",
    "eras",
    "nations",
    "city_state_types",
    "speeds",
    "difficulties",
    "deal_items",
    "combat",
    "overview",
];

/// A query's answer to `pid`, from its coerced arguments.
pub(super) fn answer(
    g: &Game,
    pid: PlayerId,
    tool: &'static str,
    q: Query,
    args: &Map<String, Value>,
) -> Result<Value, ActionError> {
    match q {
        Query::ReadNotes => Ok(read_notes(g, pid)),
        Query::Events => Ok(events(g, pid, int(args, "since_id").unwrap_or(0))),
        Query::PreviewAttack => {
            let u = own_unit(g, pid, required(args, "unit_id")?)?;
            let t = tile_at(g, required(args, "x")?, required(args, "y")?)?;
            resolve::preview(g, u, t)
        }
        Query::Tile => {
            tile_at(g, required(args, "x")?, required(args, "y")?)?;
            Err(views(tool))
        }
        Query::Unit => {
            own_unit(g, pid, required(args, "unit_id")?)?;
            Err(views(tool))
        }
        Query::City => {
            own_city(g, pid, required(args, "city_id")?)?;
            Err(views(tool))
        }
        Query::Map => {
            // A centre given is checked before the map is drawn, so a bad one is refused rather
            // than drawn somewhere else (`tools.py:226-227`).
            if let (Some(x), Some(y)) = (int(args, "x"), int(args, "y")) {
                tile_at(g, x, y)?;
            }
            Err(briefing(tool))
        }
        Query::Rules => {
            rules_topic(args.get("topic"))?;
            Err(briefing(tool))
        }
        Query::Briefing => Err(briefing(tool)),
        Query::Units
        | Query::Cities
        | Query::Empire
        | Query::Players
        | Query::Diplomacy
        | Query::CityStates
        | Query::TechTree
        | Query::Policies
        | Query::Religion
        | Query::GreatPeople
        | Query::Espionage
        | Query::VictoryStatus => Err(views(tool)),
    }
}

/// An integer argument, as `normalize` left it.
fn int(args: &Map<String, Value>, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

/// A required integer argument, which `normalize` has checked is there and an integer.
fn required(args: &Map<String, Value>, key: &str) -> Result<i64, ActionError> {
    int(args, key).ok_or_else(|| {
        ActionError::new(ErrCode::MissingParam, format!("Missing required parameter(s): {key}."))
    })
}

/// `read_notes`: the caller's notebook, or `(empty)` (`tools.read_notes`, `tools.py:370-374`).
fn read_notes(g: &Game, pid: PlayerId) -> Value {
    let notes = g.player(pid).and_then(|p| p.major.as_deref()).map_or("", |m| &*m.notes);
    json!(if notes.is_empty() { "(empty)" } else { notes })
}

/// `get_events`: the caller's notifications after event id `since`, the last
/// [`EVENTS_SHOWN`] of them, as the caller may see them (`tools.get_events`, `tools.py:377-386`).
fn events(g: &Game, pid: PlayerId, since: i64) -> Value {
    // No event has an id at or below zero, nor above what a u32 holds.
    let since = u32::try_from(since.max(0)).unwrap_or(u32::MAX);
    let seen = g.events_for(Some(pid), since, EVENTS_SCANNED);
    let shown = &seen[seen.len().saturating_sub(EVENTS_SHOWN)..];
    Value::Array(
        shown
            .iter()
            .map(|e| {
                let ev = &e.event;
                json!({
                    "id": ev.id.get(),
                    "turn": ev.turn,
                    "type": ev.kind.name(),
                    "text": &*ev.text,
                })
            })
            .collect(),
    )
}

/// `get_rules`' topic, lower-cased and trimmed (`views.py:632-648`). A topic that is not text
/// is refused as an action's name would be, where Python raised.
// refcheck: tool-arguments-of-the-wrong-type-refused
fn rules_topic(topic: Option<&Value>) -> Result<String, ActionError> {
    let raw = match topic {
        Some(Value::String(s)) => s.as_str(),
        Some(v) if py::truthy(v) => {
            return Err(ActionError::new(ErrCode::BadParam, "Parameter 'topic' must be a string."));
        }
        _ => "",
    };
    let topic = py::strip(&raw.to_lowercase()).to_owned();
    if RULES_TOPICS.contains(&topic.as_str()) {
        return Ok(topic);
    }
    Err(ActionError::new(
        ErrCode::BadParam,
        format!(
            "Unknown topic '{}'. Topics: {}.",
            crate::base::text::echo(&topic),
            RULES_TOPICS.join(", ")
        ),
    ))
}

/// The refusal of a query whose answer the views build (package 1d-02).
fn views(tool: &str) -> ActionError {
    not_ported("api::views", tool)
}

/// The refusal of a query whose answer the briefing builds (package 1d-03).
fn briefing(tool: &str) -> ActionError {
    not_ported("api::briefing", tool)
}

/// The refusal of a query whose system is not ported yet: `path` names it, and `cargo xtask
/// check` counts the calls (DESIGN.md 3.4, rule 4). The text is for a model, so it names the
/// tool rather than the module.
fn not_ported(path: &'static str, tool: &str) -> ActionError {
    debug_assert!(path.starts_with("api::"), "a module of the host surface");
    ActionError::new(
        ErrCode::NotPorted,
        format!("{tool} is not ported to the new engine yet; the other tools work."),
    )
}
