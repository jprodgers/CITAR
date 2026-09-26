//! The whole game as one player sees it, which the browser renders from (`views.client_view`,
//! `views.py:716-763`), and the route a move order would take (`EngineGame.path_preview`,
//! `engine_api.py:735-746`).
//!
//! [`Game::view_json`] writes the view straight to JSON bytes. Its largest parts, the tiles, the
//! units and the cities, are typed, and serialise without building a value for each; the rest
//! are the builders' values.

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::alerts::alert_items;
use super::cities::{CityView, city_view};
use super::empire::empire_info;
use super::events::events_json;
use super::players::{diplomacy_info, players_overview};
use super::tiles::{Known, resource_seen, route_name};
use super::units::{UnitView, unit_view};
use crate::base::ids::{PlayerId, UnitId};
use crate::base::sets::FeatureSet;
use crate::game::vis::sight::unit_visible_to;
use crate::game::{Game, movement, victory};
use crate::rules::Ruleset;
use crate::state::Phase;
use crate::state::chronicle::StatsRow;
use crate::state::config::MapSource;

/// How many diplomacy messages the client view shows (`diplomacy_info`'s default,
/// `views.py:440`).
const MESSAGES_SHOWN: i64 = 30;

/// How many thoughts and messages a spectator's view shows, the latest (`views.py:760-761`).
const SPECTATOR_FEED: usize = 80;

/// How many negotiations a spectator's view shows, the latest (`views.py:762`).
const SPECTATOR_NEGOTIATIONS: usize = 20;

/// How many turns ahead `path_preview` looks (`movement.find_path`'s default).
const PATH_TURNS: u32 = 40;

/// One explored tile of the client view, as Python's list: `[idx, terrain, features, natural
/// wonder, river bits, resource, improvement, route, pillaged, route pillaged, owner, visible]`
/// (`views.py:726-727`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TileRow<'a>(
    pub u32,
    pub &'a str,
    pub FeatureNames<'a>,
    pub Option<&'a str>,
    pub u8,
    pub Option<&'a str>,
    pub Option<&'a str>,
    pub Option<&'static str>,
    pub u8,
    pub u8,
    pub Option<u8>,
    pub u8,
);

/// A tile's features as the list of their names, lowest layer first, written without collecting
/// them: the tiles are most of a view.
#[derive(Clone, Copy, Debug)]
pub struct FeatureNames<'a>(pub FeatureSet, pub &'a Ruleset);

/// The same features named by the same ruleset.
impl PartialEq for FeatureNames<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && core::ptr::eq(self.1, other.1)
    }
}

impl Eq for FeatureNames<'_> {}

impl Serialize for FeatureNames<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let r = self.1;
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for f in self.0.iter() {
            if let Some(name) = r.derived().features.get(f).and_then(|&t| r.name(t)) {
                seq.serialize_element(name)?;
            }
        }
        seq.end()
    }
}

/// The client view (`views.client_view`): the map as the viewer knows it, the units and cities it
/// sees (and the cities it remembers), everyone it knows of, the settings and its events; for a
/// player its empire, diplomacy, notes and alerts, for a spectator the last statistics, every
/// empire and the latest thoughts, messages and negotiations.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClientView<'a> {
    pub turn: i32,
    pub year: String,
    pub current_player: u8,
    pub phase: &'static str,
    pub winner: Option<u8>,
    pub victory: Option<&'a str>,
    pub you: Option<u8>,
    pub width: u16,
    pub height: u16,
    pub wrap_x: bool,
    pub wrap_y: bool,
    pub tiles: Vec<TileRow<'a>>,
    pub units: Vec<UnitView<'a>>,
    pub cities: Vec<CityEntry<'a>>,
    pub players: Vec<Value>,
    pub turn_limit: i32,
    pub config: Value,
    pub events: Vec<Value>,
    /// A player's `empire`, `diplomacy`, `notes` and `alerts`, or a spectator's `stats`,
    /// `empires`, `thoughts`, `messages` and `negotiations`.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// A city in the client view: one the viewer sees, or one it remembers from when it last saw it
/// (`stale`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CityEntry<'a> {
    Seen(Box<CityView<'a>>),
    Remembered { id: u32, name: &'a str, owner: u8, x: i32, y: i32, pop: u16, stale: bool },
}

/// The route a move order would take (`EngineGame.path_preview`): the tiles from the unit's to
/// the target and the turns it takes, or no path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathPreview {
    /// Each tile as `[x, y]`, the unit's first; `None` when there is no path.
    pub path: Option<Vec<[i32; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
}

/// The settings the client reads (`views.py:746-747`).
pub(crate) fn config_json(g: &Game) -> Value {
    let r = g.rules();
    let c = g.state().config();
    let k = r.constants();
    let (size, map_type) = match &c.map {
        MapSource::Generated { size, map_type, .. } => {
            (&*k.map_sizes[*size].key, &*k.map_types[*map_type].key)
        }
        MapSource::Editor { size, .. } => (&*k.map_sizes[*size].key, "custom"),
    };
    let victories: Map<String, Value> = r
        .victories()
        .iter()
        .map(|(id, v)| (v.name.to_string(), json!(c.victory_enabled(id))))
        .collect();
    json!({
        "map_size": size,
        "map_type": map_type,
        "speed": r.name(c.speed),
        "difficulty": r.name(c.difficulty),
        "barbarian_difficulty": r.name(c.barbarian_difficulty),
        "barbarians": &*k.barbarian_levels[c.barbarians].key,
        "barbarian_aggression": c.barbarian_aggression,
        "turn_limit": c.turn_limit,
        "victories": victories,
        "tech_trading": c.tech_trading,
        "religion": c.religion,
        "espionage": c.espionage,
    })
}

/// A round's statistics as Python's row: `turn`, and each major's by id, an eliminated one's as
/// `{"alive": false, "score": 0}` (`victory.record_stats`, `victory.py:424-455`).
#[must_use]
pub fn stats_row_json(row: &StatsRow) -> Value {
    let players: Map<String, Value> = row
        .civs
        .iter()
        .map(|c| {
            let v = if c.alive {
                json!({
                    "alive": true, "score": c.score, "cities": c.cities,
                    "population": c.population, "land": c.land, "techs": c.techs,
                    "policies": c.policies, "military": c.military, "gold": c.gold,
                    "gold_per_turn": c.gold_per_turn, "science": c.science, "culture": c.culture,
                    "faith": c.faith, "production": c.production, "happiness": c.happiness,
                    "era": c.era.0, "units": c.units, "golden_age": c.golden_age,
                })
            } else {
                json!({"alive": false, "score": 0})
            };
            (c.player.0.to_string(), v)
        })
        .collect();
    json!({"turn": row.turn, "players": players})
}

/// The tiles of the client view: every tile a player has explored (all, for a spectator), as it
/// knows them.
fn tile_rows<'a>(g: &'a Game, r: &'a Ruleset, viewer: Option<PlayerId>) -> Vec<TileRow<'a>> {
    let explored = viewer.and_then(|v| g.player(v)).map(|p| &p.explored);
    let vis = viewer.and_then(|v| g.derived().vis().visible(v));
    let mut rows = Vec::new();
    for (t, tile) in g.state().tiles().iter() {
        if viewer.is_some() && !explored.is_some_and(|e| e.contains(t.0)) {
            continue;
        }
        let visible = viewer.is_none() || vis.is_some_and(|v| v.contains(t.0));
        let k = Known::of(g, t, tile, viewer, visible);
        let res = tile.resource().filter(|&x| resource_seen(g, viewer, x)).and_then(|x| r.name(x));
        rows.push(TileRow(
            t.0,
            r.name(tile.terrain()).unwrap_or(""),
            FeatureNames(k.features, r),
            tile.wonder().and_then(|w| r.name(w)),
            tile.river_mask(),
            res,
            k.improvement.and_then(|i| r.name(i)),
            k.route.map(route_name),
            u8::from(k.pillaged),
            u8::from(k.route_pillaged),
            k.owner.map(|p| p.0),
            u8::from(visible),
        ));
    }
    rows
}

impl Game {
    /// The client view as JSON bytes (`EngineGame.view`, DESIGN.md 8.1): what `viewer` sees, or
    /// everything for `None`, with the last `event_limit` of its newest 200 events, its floats as
    /// Python wrote them ([`super::PyJson`]).
    #[must_use]
    pub fn view_json(&self, viewer: Option<PlayerId>, event_limit: u32) -> Vec<u8> {
        super::to_py_json(&self.client_view(viewer, i64::from(event_limit)))
    }

    /// The client view as a typed value, before it is written (`views.client_view`). An
    /// `event_limit` reads as Python's `[-n:]` does: 0 keeps every event it scanned.
    #[must_use]
    pub fn client_view(&self, viewer: Option<PlayerId>, event_limit: i64) -> ClientView<'_> {
        let g = self;
        let r = g.rules();
        let st = g.state();
        let clock = st.clock();
        let vis = viewer.and_then(|v| g.derived().vis().visible(v));
        let sees =
            |t: crate::base::ids::TileIdx| viewer.is_none() || vis.is_some_and(|x| x.contains(t.0));
        let units: Vec<UnitView<'_>> = st
            .units()
            .iter()
            .filter(|u| viewer.is_none_or(|v| sees(u.tile()) && unit_visible_to(g, v, u.id())))
            .filter_map(|u| unit_view(g, u.id(), viewer))
            .collect();
        let explored = |t: crate::base::ids::TileIdx| {
            viewer.and_then(|v| g.player(v)).is_some_and(|p| p.explored.contains(t.0))
        };
        let memory =
            viewer.and_then(|v| g.player(v)).and_then(|p| p.major.as_deref()).map(|m| &m.memory);
        let mut cities = Vec::new();
        for c in st.cities().iter() {
            if sees(c.tile()) {
                cities.extend(city_view(g, c.id(), viewer).map(|v| CityEntry::Seen(Box::new(v))));
            } else if explored(c.tile())
                && let Some(mem) = memory.and_then(|m| m.city(c.tile()))
            {
                let (x, y) = g.xy(c.tile());
                cities.push(CityEntry::Remembered {
                    id: c.id().get(),
                    name: &mem.name,
                    owner: mem.owner.0,
                    x,
                    y,
                    pop: mem.pop,
                    stale: true,
                });
            }
        }
        let mut rest = Map::new();
        match viewer {
            Some(p) => {
                let pl = g.player(p);
                rest.insert("empire".into(), empire_info(g, p));
                rest.insert("diplomacy".into(), diplomacy_info(g, p, MESSAGES_SHOWN));
                let notes = pl.and_then(|x| x.major.as_deref()).map_or("", |m| &m.notes);
                rest.insert("notes".into(), json!(notes));
                let alerts: Vec<Value> = if pl.is_some_and(crate::state::players::Player::alive) {
                    alert_items(g, p).iter().map(|a| a.to_json(g, false)).collect()
                } else {
                    Vec::new()
                };
                rest.insert("alerts".into(), Value::Array(alerts));
            }
            None => {
                let chron = g.chronicle();
                rest.insert("stats".into(), json!(chron.stats().last().map(stats_row_json)));
                let empires: Map<String, Value> = g
                    .majors(true)
                    .map(|p| (p.id().0.to_string(), empire_info(g, p.id())))
                    .collect();
                rest.insert("empires".into(), Value::Object(empires));
                let thoughts = chron.thoughts();
                let thoughts: Vec<Value> = thoughts
                    [thoughts.len().saturating_sub(SPECTATOR_FEED)..]
                    .iter()
                    .map(super::thought_json)
                    .collect();
                rest.insert("thoughts".into(), Value::Array(thoughts));
                let messages = chron.messages();
                let messages: Vec<Value> = messages
                    [messages.len().saturating_sub(SPECTATOR_FEED)..]
                    .iter()
                    .map(super::message_json)
                    .collect();
                rest.insert("messages".into(), Value::Array(messages));
                let negs = g.negotiations();
                let negs: Vec<Value> = negs[negs.len().saturating_sub(SPECTATOR_NEGOTIATIONS)..]
                    .iter()
                    .map(|n| super::stored_negotiation(g, n))
                    .collect();
                rest.insert("negotiations".into(), Value::Array(negs));
            }
        }
        let map = st.map();
        ClientView {
            turn: clock.turn,
            year: g.year_text(None),
            current_player: clock.current.0,
            phase: match clock.phase {
                Phase::Playing => "playing",
                Phase::Over => "over",
            },
            winner: clock.winner.map(|p| p.0),
            victory: victory::won_by(g).map(|w| w.name(r)),
            you: viewer.map(|p| p.0),
            width: map.width,
            height: map.height,
            wrap_x: map.wrap_x,
            wrap_y: map.wrap_y,
            tiles: tile_rows(g, r, viewer),
            units,
            cities,
            players: players_overview(g, viewer),
            turn_limit: g.total_turns(),
            config: config_json(g),
            events: events_json(g, viewer, event_limit),
            rest,
        }
    }

    /// The route a move order would take one of `pid`'s units to `(x, y)`, and its turns
    /// (`EngineGame.path_preview`); no path for a unit that is not theirs, a tile off the map,
    /// or a target out of reach.
    #[must_use]
    pub fn path_preview(&self, pid: PlayerId, unit: UnitId, x: i32, y: i32) -> PathPreview {
        let none = PathPreview { path: None, turns: None };
        let Some(u) = self.unit(unit).filter(|u| u.owner() == pid) else { return none };
        let Some(target) = self.grid().idx(x, y) else { return none };
        let path = match movement::find_path(self, u.id(), target, PATH_TURNS) {
            Some(p) if !p.is_empty() => p,
            _ => return none,
        };
        let turns = movement::path_turns(self, u.id(), &path);
        let tiles = path
            .iter()
            .map(|&t| {
                let (a, b) = self.xy(t);
                [a, b]
            })
            .collect();
        PathPreview { path: Some(tiles), turns: Some(turns) }
    }
}
