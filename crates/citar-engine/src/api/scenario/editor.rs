//! What the scenario editor reads of a scenario (`scenario.py:503-645`): its overview, the seats a
//! scenario offers and their check, and a saved scenario's headline facts for lists. The files
//! stay in Python: `save_scenario` writes `Game::snapshot` with the seats, and `load_scenario`
//! reads it back.
//!
//! What differs from Python, on purpose: the civilizations each has met, its policies and a
//! city's buildings are listed by id, where Python kept the order they were met, adopted and
//! built in (`lists-in-rule-order`); a city-state's influence is given with every civilization
//! (`scenario-overview-lists-every-influence`); and a scenario's summary reads the state as the
//! engine saves it (DESIGN.md 4.9), where Python read its own.

use serde_json::{Map, Value, json};

use crate::base::ids::PlayerId;
use crate::base::{num, py};
use crate::game::Game;
use crate::game::diplomacy::relations::{has_embassy, has_pact, is_friends};
use crate::game::error::{ActionError, ErrCode};
use crate::game::query;
use crate::state::players::{PlayerKind, SeatOverrides};

/// The seat types a scenario may offer (`scenario.SEAT_TYPES`, `scenario.py:30`).
pub const SEAT_TYPES: [&str; 6] = ["human", "llm", "bot", "mcp", "hybrid", "script"];

/// The most characters a seat's label keeps (`scenario.py:573`).
const LABEL_CHARS: usize = 60;

/// The settings the editor shows, in Python's order (`scenario.py:533-534`).
fn config(g: &Game) -> Value {
    use crate::state::config::MapSource;
    let r = g.rules();
    let c = g.state().config();
    let k = r.constants();
    let (map, size, map_type) = match &c.map {
        MapSource::Generated { size, map_type, .. } => {
            (Value::Null, &*k.map_sizes[*size].key, &*k.map_types[*map_type].key)
        }
        MapSource::Editor { id, size } => (json!(&**id), &*k.map_sizes[*size].key, "custom"),
    };
    json!({
        "map": map,
        "map_size": size,
        "map_type": map_type,
        "speed": r.name(c.speed),
        "difficulty": r.name(c.difficulty),
        "starting_era": r.name(c.starting_era),
        "turn_limit": c.turn_limit,
        "barbarians": &*k.barbarian_levels[c.barbarians].key,
        "city_states": c.city_states,
    })
}

/// A summary of a scenario's state for its editor (`scenario.overview`, `scenario.py:503-535`):
/// the turn and whose it is; each civilization and city-state with its seat, and a
/// civilization's treasury, techs, era, policies, cities, units and contacts, a city-state's type,
/// influence and ally; the relations between each two living civilizations; the cities; and the
/// settings.
// refcheck: lists-in-rule-order
#[must_use]
pub fn overview(g: &Game) -> Value {
    let r = g.rules();
    let st = g.state();
    let mut players = Vec::new();
    for (id, p) in st.players().iter() {
        if p.is_barbarian() {
            continue;
        }
        let seat = p.seat();
        let auto = seat.auto();
        let mut d = Map::new();
        d.insert("id".into(), json!(id.0));
        d.insert("name".into(), json!(&*p.name));
        d.insert("nation".into(), json!(r.name(p.nation)));
        let kind = if p.kind == PlayerKind::Major { "major" } else { "city_state" };
        d.insert("kind".into(), json!(kind));
        d.insert("color".into(), json!(p.color.to_hex()));
        d.insert("alive".into(), json!(p.alive()));
        d.insert("controller".into(), json!(seat.controller().name()));
        d.insert("handicap".into(), json!(seat.handicap().name()));
        d.insert(
            "auto".into(),
            json!({"un_vote": auto.un_vote, "conquest": auto.conquest, "free_picks": auto.free_picks}),
        );
        d.insert("difficulty".into(), json!(seat.difficulty().and_then(|x| r.name(x))));
        let cities = g.player_cities(id).count();
        if p.is_major() {
            let met: Vec<u8> =
                st.diplo().met_mask(id).iter().filter(|&q| q != id).map(|q| q.0).collect();
            let policies: Vec<&str> = p.policy.adopted.iter().filter_map(|x| r.name(x)).collect();
            d.insert("gold".into(), json!(num::trunc_i64(p.econ.gold)));
            d.insert("faith".into(), json!(num::trunc_i64(p.econ.faith)));
            d.insert("culture".into(), json!(num::trunc_i64(p.econ.culture)));
            d.insert("techs".into(), json!(p.tech.known.len()));
            d.insert("era".into(), json!(r.name(query::era(g, id))));
            d.insert("policies".into(), json!(policies));
            d.insert("cities".into(), json!(cities));
            d.insert("units".into(), json!(g.player_units(id).count()));
            d.insert("met".into(), json!(met));
        } else {
            let cs = p.city_state.as_deref();
            let ty = cs.and_then(|c| c.cs_type).and_then(|t| r.city_state_types().get(t));
            // With every civilization, where Python gave those whose influence had been set.
            // refcheck: scenario-overview-lists-every-influence
            let influence: Map<String, Value> = g
                .majors(false)
                .map(|m| (m.id().0.to_string(), json!(cs.map_or(0.0, |c| c.influence_of(m.id())))))
                .collect();
            d.insert("cs_type".into(), json!(ty.map(|t| &*t.name)));
            d.insert("influence".into(), Value::Object(influence));
            d.insert("ally".into(), json!(cs.and_then(|c| c.ally()).map(|a| a.0)));
            d.insert("cities".into(), json!(cities));
        }
        players.push(Value::Object(d));
    }
    let majors: Vec<PlayerId> = g.majors(true).map(crate::state::players::Player::id).collect();
    let mut relations = Vec::new();
    for (i, &a) in majors.iter().enumerate() {
        for &b in &majors[i + 1..] {
            relations.push(json!({
                "a": a.0,
                "b": b.0,
                "met": g.has_met(a, b),
                "war": g.relation(a, b).is_some_and(|x| x.war),
                "embassies": has_embassy(g, a, b) && has_embassy(g, b, a),
                "friends": is_friends(g, a, b),
                "defensive_pact": has_pact(g, a, b),
                "open_borders": g.has_open_borders(a, b) && g.has_open_borders(b, a),
            }));
        }
    }
    let cities: Vec<Value> = st
        .cities()
        .iter()
        .map(|c| {
            let (x, y) = g.xy(c.tile());
            let buildings: Vec<&str> = c.buildings.iter().filter_map(|b| r.name(b)).collect();
            json!({
                "id": c.id().get(),
                "name": &*c.name,
                "owner": c.owner().0,
                "x": x,
                "y": y,
                "pop": c.pop,
                "buildings": buildings,
            })
        })
        .collect();
    json!({
        "turn": g.turn(),
        "current": g.current().0,
        "players": players,
        "relations": relations,
        "cities": cities,
        "config": config(g),
    })
}

/// The seats a scenario offers by default (`scenario.default_seats`, `scenario.py:560-562`): one
/// per living civilization, played as it was being played (a bot for any other controller),
/// labelled with its name.
#[must_use]
pub fn default_seats(g: &Game) -> Vec<Value> {
    g.majors(true)
        .map(|p| {
            let c = p.seat().controller().name();
            let ty = if SEAT_TYPES.contains(&c) { c } else { "bot" };
            json!({"type": ty, "label": &*p.name})
        })
        .collect()
}

/// A scenario's seat list checked against its civilizations (`scenario.normalize_seats`,
/// `scenario.py:565-581`): the default seats, each changed by the entry given for it: a seat
/// type the list knows, a label (at most 60 characters), the settings of its model or bot (an
/// API key never kept), and its handicap and automatic decisions, checked as the lobby checks
/// them. Entries past the civilizations, and entries that are not objects, are ignored.
///
/// # Errors
/// [`ErrCode::BadParam`], naming the seat, for a handicap or automatic decisions that are not
/// allowed.
pub fn normalize_seats(g: &Game, seats: Option<&Value>) -> Result<Vec<Value>, ActionError> {
    let mut base = default_seats(g);
    let given: &[Value] = match seats {
        Some(Value::Array(a)) => a,
        _ => &[],
    };
    for (i, (b, s)) in base.iter_mut().zip(given).enumerate() {
        let (Some(seat), Value::Object(s)) = (b.as_object_mut(), s) else { continue };
        if let Some(Value::String(t)) = s.get("type")
            && SEAT_TYPES.contains(&t.as_str())
        {
            seat.insert("type".into(), json!(t));
        }
        if let Some(label) = s.get("label").filter(|v| py::truthy(v)) {
            let text = match label {
                Value::String(t) => t.clone(),
                v => py::str_of(v),
            };
            let cut = text.char_indices().nth(LABEL_CHARS).map_or(&*text, |(k, _)| &text[..k]);
            seat.insert("label".into(), json!(cut));
        }
        for k in ["llm", "bot"] {
            if let Some(Value::Object(m)) = s.get(k) {
                let kept: Map<String, Value> = m
                    .iter()
                    .filter(|(kk, _)| *kk != "api_key")
                    .map(|(a, v)| (a.clone(), v.clone()))
                    .collect();
                seat.insert(k.into(), Value::Object(kept));
            }
        }
        let over = SeatOverrides::parse(s.get("handicap"), s.get("auto"))
            .map_err(|e| ActionError::new(ErrCode::BadParam, format!("Seat {i}: {e}")))?;
        if let Some(h) = over.handicap {
            seat.insert("handicap".into(), json!(h.name()));
        }
        // As given, once checked: Python kept the dict it was sent.
        if let Some(Value::Object(auto)) = s.get("auto").filter(|v| py::truthy(v)) {
            seat.insert("auto".into(), Value::Object(auto.clone()));
        }
    }
    Ok(base)
}

/// A saved scenario's headline facts, for lists (`scenario.summary`, `scenario.py:625-633`): its
/// id, name and description, the map's size and the turn, its civilizations, how many
/// city-states it has, its seats and when it was saved. Its `state` is a state as the engine
/// saves it (DESIGN.md 4.9).
///
/// # Errors
/// [`ErrCode::BadParam`] for a scenario without an id, a name or a state that reads as one.
pub fn scenario_summary(doc: &Value) -> Result<Value, ActionError> {
    let bad =
        |what: &str| ActionError::new(ErrCode::BadParam, format!("The scenario has no {what}."));
    let o = doc.as_object().ok_or_else(|| bad("fields: it is not an object"))?;
    let id = o.get("id").ok_or_else(|| bad("id"))?;
    let name = o.get("name").ok_or_else(|| bad("name"))?;
    let st = o.get("state").and_then(Value::as_object).ok_or_else(|| bad("state"))?;
    let map = st.get("map").ok_or_else(|| bad("map in its state"))?;
    let turn =
        st.get("clock").and_then(|c| c.get("turn")).ok_or_else(|| bad("turn in its state"))?;
    let players = st.get("players").and_then(Value::as_array).ok_or_else(|| bad("players"))?;
    fn kind(p: &Value) -> &str {
        p.get("kind").and_then(Value::as_str).unwrap_or("")
    }
    let majors: Vec<Value> = players
        .iter()
        .filter(|p| kind(p) == "major")
        .map(|p| {
            json!({
                "id": p.get("id").cloned().unwrap_or(Value::Null),
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "nation": p.get("nation").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let city_states = players.iter().filter(|p| kind(p) == "city_state").count();
    Ok(json!({
        "id": id,
        "name": name,
        "description": o.get("description").cloned().unwrap_or_else(|| json!("")),
        "width": map.get("width").cloned().unwrap_or(Value::Null),
        "height": map.get("height").cloned().unwrap_or(Value::Null),
        "turn": turn,
        "players": majors,
        "city_states": city_states,
        "seats": o.get("seats").cloned().unwrap_or_else(|| json!([])),
        "modified": o.get("modified").cloned().unwrap_or(Value::Null),
    }))
}

impl Game {
    /// The scenario editor's summary of this game ([`overview`]).
    #[must_use]
    pub fn scenario_overview(&self) -> Value {
        overview(self)
    }
}
