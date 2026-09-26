//! The query tools' answers (`tools.py:200-406`), which change nothing.
//!
//! A query looks up what it names the way the actions do (`game::lookup`: the tile, the
//! caller's unit or city), so its refusals are the tools' own. Package 1d-01 answered the queries
//! that need no view: `read_notes`, `get_events` and `preview_attack`, and checked the arguments
//! of the rest. Package 1d-02 answers the fifteen the views build (`api::views`: a tile, a unit
//! and the units, a city and the cities, the empire, the players, diplomacy, the city-states, the
//! tech tree, policies, religion, great people, espionage and victory). Package 1d-03 answers the
//! last three: `get_briefing` and `get_map` (`api::briefing`) and `get_rules`
//! (`api::views::rules`).

use serde_json::{Map, Value, json};

use super::registry::Query;
use crate::api::briefing;
use crate::api::text::MAP_LEGEND;
use crate::api::views::rules::{TOPICS, rules_lookup};
use crate::api::views::{cities, empire, info, players, tiles, units};
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

/// How many messages `get_diplomacy` shows by default (`tools.py:291`).
const MESSAGES_SHOWN: i64 = 30;

/// The rows `get_map` shows above and below its centre unless asked otherwise (`tools.py:218`).
const MAP_RADIUS: i64 = 8;

/// A query's answer to `pid`, from its coerced arguments.
pub(super) fn answer(
    g: &Game,
    pid: PlayerId,
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
        Query::Tile => Ok(tiles::tile_info(
            g,
            tile_at(g, required(args, "x")?, required(args, "y")?)?,
            Some(pid),
        )),
        Query::Unit => {
            Ok(units::unit_info(g, own_unit(g, pid, required(args, "unit_id")?)?, Some(pid), true))
        }
        Query::Units => Ok(Value::Array(
            g.player_units(pid).map(|u| units::unit_info(g, u.id(), Some(pid), false)).collect(),
        )),
        Query::City => {
            Ok(cities::city_info(g, own_city(g, pid, required(args, "city_id")?)?, Some(pid), true))
        }
        Query::Cities => Ok(Value::Array(
            g.player_cities(pid).map(|c| cities::city_info(g, c.id(), Some(pid), false)).collect(),
        )),
        Query::Empire => Ok(empire::empire_info(g, pid)),
        Query::Players => Ok(Value::Array(players::players_overview(g, Some(pid)))),
        Query::Diplomacy => Ok(players::diplomacy_info(
            g,
            pid,
            int(args, "message_limit").unwrap_or(MESSAGES_SHOWN),
        )),
        Query::CityStates => Ok(Value::Array(players::city_states_info(g, pid))),
        Query::TechTree => Ok(tech_tree(g, pid, args.get("filter"))),
        Query::Policies => Ok(info::policies_info(g, pid)),
        Query::Religion => Ok(info::religion_info(g, pid)),
        Query::GreatPeople => Ok(info::great_people_info(g, pid)),
        Query::Espionage => Ok(info::espionage_view(g, pid)),
        Query::VictoryStatus => Ok(info::victory_info(g, pid)),
        Query::Map => map(g, pid, args),
        Query::Rules => {
            let topic = rules_topic(args.get("topic"))?;
            rules_lookup(g, &topic, args.get("name"))
        }
        Query::Briefing => Ok(json!(briefing::briefing(g, pid))),
    }
}

/// `get_map`: the ASCII map around a tile, the caller's capital by default, with the legend
/// above it when asked (`tools.get_map`, `tools.py:218-229`). A centre given is checked before
/// the map is drawn, so a bad one is refused rather than drawn somewhere else; with one
/// coordinate only, the map centres on the capital, as Python's did. A radius of 0 is the
/// default one, as Python's `radius or 8` read it.
fn map(g: &Game, pid: PlayerId, args: &Map<String, Value>) -> Result<Value, ActionError> {
    let centre = match (int(args, "x"), int(args, "y")) {
        (Some(x), Some(y)) => {
            let t = tile_at(g, x, y)?;
            Some(g.xy(t))
        }
        _ => None,
    };
    let radius = int(args, "radius").filter(|&r| r != 0).unwrap_or(MAP_RADIUS);
    let text = briefing::ascii_map(g, pid, centre, radius);
    let legend = args.get("legend").is_some_and(py::truthy);
    Ok(json!(if legend { format!("{MAP_LEGEND}\n\n{text}") } else { text }))
}

/// `get_tech_tree`: the tree, its techs filtered to one status, `available` unless the filter
/// says otherwise, or all of them for `all` (`tools.get_tech_tree`, `tools.py:309-321`). A filter
/// that is no status leaves none, as Python's comparison did.
fn tech_tree(g: &Game, pid: PlayerId, filter: Option<&Value>) -> Value {
    let mut tt = info::tech_tree(g, pid);
    let all = matches!(filter, Some(Value::String(s)) if s == "all");
    if !all {
        let want = match filter {
            Some(v) if py::truthy(v) => v.clone(),
            _ => Value::from("available"),
        };
        if let Some(Value::Array(techs)) = tt.get_mut("techs") {
            techs.retain(|t| t.get("status") == Some(&want));
        }
    }
    tt
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
    if TOPICS.contains(&topic.as_str()) {
        return Ok(topic);
    }
    Err(ActionError::new(
        ErrCode::BadParam,
        format!(
            "Unknown topic '{}'. Topics: {}.",
            crate::base::text::echo(&topic),
            TOPICS.join(", ")
        ),
    ))
}
