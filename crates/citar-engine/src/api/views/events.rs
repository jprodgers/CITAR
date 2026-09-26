//! Events as the client and the tools show them (`game.py:880-990`, DESIGN.md 8.4): each event
//! in Python's dict, scrubbed for the viewer.
//!
//! `Game::event_view` does the scrubbing on the typed event: unmet civilizations, city-states,
//! leaders and cities anonymised in the text, coordinates dropped from it, the tile and the name
//! references dropped, and the player fields the viewer does not know cleared. What this module
//! adds is the JSON: the keys Python's `emit` wrote (`id`, `turn`, `type`, `text`, `players`,
//! `idx`, `data`, `refs` where there are some, `x` and `y` where there is a tile), with the name
//! references in code points as Python counted them, the data's rule objects by name, a cleared
//! player field as `null` (Python set it to `None` rather than dropping it), and the United
//! Nations' tally keyed by name, its unmet candidates numbered as unknown (`game.py:980-988`).

use serde_json::{Map, Value, json};

use super::players::{py_tail, un_result};
use crate::base::ids::PlayerId;
use crate::base::text::char_offset;
use crate::game::Game;
use crate::game::cities::construction::item_name;
use crate::game::events::EventOut;
use crate::game::religion;
use crate::state::chronicle::{EngineEvent, Event, EventData, EventType};

/// How far back a view looks for events (`Game.events_for`'s default limit, `game.py:880`).
pub const EVENTS_SCANNED: usize = 200;

/// The events `viewer` may see, the last `limit` of the newest [`EVENTS_SCANNED`], oldest first,
/// as JSON (`client_view`'s `events_for(pid)[-event_limit:]`).
#[must_use]
pub fn events_json(g: &Game, viewer: Option<PlayerId>, limit: i64) -> Vec<Value> {
    let mut seen: Vec<(&Event, EventOut<'_>)> = Vec::new();
    for ev in g.chronicle().events().iter().rev() {
        if seen.len() >= EVENTS_SCANNED {
            break;
        }
        let hears = match (viewer, ev.audience) {
            (None, _) | (_, None) => true,
            (Some(p), Some(a)) => a.contains(p),
        };
        if hears {
            seen.push((ev, g.event_view(ev, viewer)));
        }
    }
    seen.reverse();
    let start = py_tail(seen.len(), limit);
    seen[start..].iter().map(|(ev, out)| event_json(g, ev, out, viewer)).collect()
}

/// One event as `viewer` sees it, in Python's dict: `ev` as it happened, `out` as scrubbed for
/// the viewer.
#[must_use]
pub fn event_json(g: &Game, ev: &Event, out: &EventOut<'_>, viewer: Option<PlayerId>) -> Value {
    let shown = &*out.event;
    let mut m = Map::new();
    m.insert("id".into(), json!(shown.id.get()));
    m.insert("turn".into(), json!(shown.turn));
    m.insert("type".into(), json!(shown.kind.name()));
    m.insert("text".into(), json!(&*shown.text));
    m.insert(
        "players".into(),
        json!(shown.audience.map(|a| a.iter().map(|p| p.0).collect::<Vec<_>>())),
    );
    m.insert("idx".into(), json!(shown.tile.map(|t| t.0)));
    let mut data = data_json(g, ev.data.as_deref(), shown.data.as_deref(), viewer);
    // Python's `city_sacked` named the building burned, `None` when none was (`barbarians.py:468`),
    // which a typed field cannot tell from a key never passed.
    if shown.kind == EventType::Engine(EngineEvent::CitySacked)
        && let Some(d) = data.as_object_mut()
    {
        d.entry("building").or_insert(Value::Null);
    }
    m.insert("data".into(), data);
    if !shown.refs.is_empty() {
        let text = &*shown.text;
        let refs: Vec<Value> = shown
            .refs
            .iter()
            .map(|r| {
                let code = r.kind.code().to_string();
                json!([
                    char_offset(text, r.start as usize),
                    char_offset(text, r.end as usize),
                    r.player.0,
                    code
                ])
            })
            .collect();
        m.insert("refs".into(), Value::Array(refs));
    }
    if let Some(t) = shown.tile {
        let (x, y) = g.xy(t);
        m.insert("x".into(), json!(x));
        m.insert("y".into(), json!(y));
    }
    Value::Object(m)
}

/// An event's data as Python's dict: every field the event was emitted with, rule objects by
/// name, `era` by index and a religion by its key; a player field the scrubbing cleared as null.
fn data_json(
    g: &Game,
    before: Option<&EventData>,
    after: Option<&EventData>,
    viewer: Option<PlayerId>,
) -> Value {
    let mut m = Map::new();
    let Some(d) = before else { return Value::Object(m) };
    let empty = EventData::default();
    let after = after.unwrap_or(&empty);
    let r = g.rules();
    let players = [
        ("player", d.player, after.player),
        ("a", d.a, after.a),
        ("b", d.b, after.b),
        ("attacker", d.attacker, after.attacker),
        ("defender", d.defender, after.defender),
        ("awaiting", d.awaiting, after.awaiting),
        ("killer", d.killer, after.killer),
        ("owner", d.owner, after.owner),
        ("old_owner", d.old_owner, after.old_owner),
        ("new_owner", d.new_owner, after.new_owner),
        ("sender", d.sender, after.sender),
        ("winner", d.winner, after.winner),
    ];
    for (k, was, now) in players {
        if was.is_some() {
            m.insert(k.into(), json!(now.map(|p| p.0)));
        }
    }
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            m.insert(k.into(), v);
        }
    };
    put("unit", d.unit.map(|x| json!(x.get())));
    put("city", d.city.map(|x| json!(x.get())));
    put("deal", d.deal.map(|x| json!(x.get())));
    put("negotiation", d.negotiation.map(|x| json!(x.get())));
    put("message", d.message.map(|x| json!(x.get())));
    put("item", d.item.map(|x| json!(item_name(r, x))));
    put("unit_type", d.unit_type.map(|x| json!(r.name(x))));
    put("building", d.building.map(|x| json!(r.name(x))));
    put("improvement", d.improvement.map(|x| json!(r.name(x))));
    put("tech", d.tech.map(|x| json!(r.name(x))));
    put("policy", d.policy.map(|x| json!(r.name(x))));
    put("belief", d.belief.map(|x| json!(r.name(x))));
    put("era", d.era.map(|x| json!(x.0)));
    put("religion", d.religion.map(|x| json!(religion::key_name(g, x))));
    put("reward", d.reward.and_then(|x| r.ruins().get(x)).map(|x| json!(&*x.name)));
    put("victory", d.victory.map(|x| json!(&*r.victories()[x].name)));
    put("status", d.status.map(|x| json!(x.name())));
    put("gold", d.gold.map(|x| json!(x)));
    put("citizen_killed", d.citizen_killed.map(|x| json!(x)));
    // Python rewrote the results only in an event it scrubbed, which is every event whose tally
    // names a candidate the viewer does not know: the only ones `un_result` changes.
    put("results", d.results.as_deref().map(|res| un_result(g, viewer, res)));
    Value::Object(m)
}
