//! What rule scripts read of a game (DESIGN.md 9.3): small documented shapes, the same from
//! both engines, with every set sorted. Feature `test-ops`; `citar/engine/inspect.py` is the
//! Python side, and `tests/rules/README.md` documents each shape.
//!
//! A query is `{"what": ..., ...}`:
//! - `game`: the turn, whose turn it is, the phase and winner, the map's size, and the players by
//!   kind;
//! - `player` (`player`): a civilization's seat, stocks, counters, techs, research, policies,
//!   contacts, cities and units, and a city-state's type, ally and influence;
//! - `tile` (`x`, `y`): terrain, features, resource, improvement, route, river, owner, city and
//!   units;
//! - `relation` (`a`, `b`): contact, war and every treaty term, with the two-sided ones as
//!   `[a's, b's]`;
//! - `unit` (`unit`), `units` (optionally `player`, `x` and `y`), `city` (`city`);
//! - `events` (optionally `since`, `type` and `player`, the last keeping what that player hears);
//! - `find_tiles`: the tiles that pass the filters given, nearest first (see [`find_tiles`]);
//! - `ops`: the scenario and test operations with their parameters;
//! - `pending`: what is not ported yet, as the queries, operations, test operations, turn stages
//!   and setup stages that wait for a package.
//!
//! `negotiation`, `view` and `briefing` wait for the packages that port what they read (1c-05,
//! 1d-02 and 1d-03), and are refused as not ported until then.
//!
//! Reads only: a query never changes the game or its digest.

use serde_json::{Map, Value, json};

use super::scenario::{self, pid, resolve};
use super::testops;
use crate::base::ids::{CityId, ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx, UnitId};
use crate::base::py;
use crate::game::diplomacy::relations::{has_pact, is_friends, opinion};
use crate::game::error::{ActionError, ErrCode};
use crate::game::turn::stages;
use crate::game::{Game, Porting, setup};
use crate::rules::Named;
use crate::rules::defs::{Route, TerrainType};
use crate::state::Phase;
use crate::state::diplo::side;
use crate::state::players::{AutoDecision, Player, PlayerKind};

/// The queries, by `what`, sorted, with whether what each reads is ported yet.
const QUERIES: [(&str, Porting); 14] = [
    ("briefing", Porting::Pending("1d-03")),
    ("city", Porting::Ported),
    ("events", Porting::Ported),
    ("find_tiles", Porting::Ported),
    ("game", Porting::Ported),
    ("negotiation", Porting::Pending("1c-05")),
    ("ops", Porting::Ported),
    ("pending", Porting::Ported),
    ("player", Porting::Ported),
    ("relation", Porting::Ported),
    ("tile", Porting::Ported),
    ("unit", Porting::Ported),
    ("units", Porting::Ported),
    ("view", Porting::Pending("1d-02")),
];

/// Answers one query.
pub fn inspect(g: &Game, q: &Value) -> Result<Value, ActionError> {
    let empty = Map::new();
    let o = q.as_object().unwrap_or(&empty);
    let what = o.get("what").and_then(Value::as_str).unwrap_or("");
    match what {
        "game" => Ok(game(g)),
        "player" => Ok(player(g, any_player(g, o.get("player"))?)),
        "tile" => Ok(tile(g, scenario::tile(g, o)?)),
        "relation" => {
            let a = pid(g, o.get("a"), false)?;
            let b = pid(g, o.get("b"), false)?;
            if a == b {
                return Err(bad("A relation needs two different players."));
            }
            Ok(relation(g, a, b))
        }
        "unit" => {
            let id = py::int_of(o.get("unit").unwrap_or(&Value::Null))
                .and_then(|n| u32::try_from(n).ok())
                .and_then(UnitId::new)
                .filter(|&u| g.unit(u).is_some())
                .ok_or_else(|| bad("No such unit."))?;
            Ok(unit(g, id))
        }
        "units" => units(g, o),
        "city" => {
            let id = py::int_of(o.get("city").unwrap_or(&Value::Null))
                .and_then(|n| u32::try_from(n).ok())
                .and_then(CityId::new)
                .filter(|&c| g.city(c).is_some())
                .ok_or_else(|| bad("No such city."))?;
            Ok(city(g, id))
        }
        "events" => events(g, o),
        "find_tiles" => find_tiles(g, o),
        "ops" => Ok(json!({"scenario": scenario::ops_help(), "test": testops::help()})),
        "pending" => Ok(pending()),
        "negotiation" => Err(not_ported("game::diplomacy::negotiation")),
        "view" => Err(not_ported("api::views")),
        "briefing" => Err(not_ported("api::briefing")),
        _ => {
            let known: Vec<&str> = QUERIES.iter().map(|&(name, _)| name).collect();
            Err(bad(format!(
                "Unknown inspect query {}. Known: {}.",
                py::repr(&Value::from(what)),
                known.join(", ")
            )))
        }
    }
}

fn bad(message: impl Into<String>) -> ActionError {
    ActionError::new(ErrCode::BadParam, message)
}

/// The refusal of a query whose system is not ported yet: `path` names the system, and
/// `cargo xtask check` counts the calls (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str) -> ActionError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("This inspect query is not ported to the new engine yet ({path})."),
    )
}

/// A player by id, the barbarians included, which scenario operations may not name but a script
/// may read.
fn any_player(g: &Game, v: Option<&Value>) -> Result<PlayerId, ActionError> {
    let raw = v.unwrap_or(&Value::Null);
    let barbarians = py::int_of(raw)
        .and_then(|n| u8::try_from(n).ok())
        .map(PlayerId)
        .filter(|&p| g.is_barbarian(p));
    barbarians.map_or_else(|| pid(g, v, false), Ok)
}

/// A rule object's name, or null.
fn name<I: Named>(g: &Game, id: Option<I>) -> Value {
    id.and_then(|i| g.rules().name(i)).map_or(Value::Null, Value::from)
}

/// Names, sorted.
fn sorted_names<I: Named>(g: &Game, ids: impl IntoIterator<Item = I>) -> Value {
    let mut names: Vec<&str> = ids.into_iter().filter_map(|i| g.rules().name(i)).collect();
    names.sort();
    json!(names)
}

fn kind_name(k: PlayerKind) -> &'static str {
    match k {
        PlayerKind::Major => "major",
        PlayerKind::CityState => "city_state",
        PlayerKind::Barbarian => "barbarian",
    }
}

/// `game`: where the game is, and its players by kind.
fn game(g: &Game) -> Value {
    let st = g.state();
    let clock = st.clock();
    let by = |f: fn(&Player) -> bool| -> Vec<u8> {
        st.players().iter().filter(|(_, p)| f(p)).map(|(id, _)| id.0).collect()
    };
    json!({
        "turn": clock.turn,
        "current": clock.current.0,
        "phase": match clock.phase { Phase::Playing => "playing", Phase::Over => "over" },
        "winner": clock.winner.map(|p| p.0),
        "victory": clock.victory.and_then(|v| g.rules().name(v)),
        "width": st.map().width,
        "height": st.map().height,
        "players": st.players().len(),
        "majors": by(Player::is_major),
        "city_states": by(Player::is_city_state),
        "barbarians": by(Player::is_barbarian).first(),
    })
}

/// `player`: one civilization, city-state or the barbarians.
fn player(g: &Game, p: PlayerId) -> Value {
    let Some(pl) = g.player(p) else { return Value::Null };
    let seat = pl.seat();
    let auto = seat.auto();
    let over = seat.overrides();
    let mut overrides = Map::new();
    if let Some(h) = over.handicap {
        overrides.insert("handicap".into(), json!(h.name()));
    }
    if !over.auto.is_empty() {
        let set: Map<String, Value> = AutoDecision::ALL
            .into_iter()
            .filter_map(|d| over.auto.get(d).map(|on| (d.name().to_owned(), json!(on))))
            .collect();
        overrides.insert("auto".into(), Value::Object(set));
    }
    let difficulty =
        seat.difficulty().or_else(|| pl.is_major().then(|| g.state().config().difficulty));
    let st = g.state();
    let met: Vec<u8> = st.diplo().met_mask(p).iter().filter(|&q| q != p).map(|q| q.0).collect();
    let city_state = pl.city_state.as_deref().map(|cs| {
        let influence: Map<String, Value> = g
            .majors(false)
            .map(|m| (m.id().0.to_string(), json!(cs.influence_of(m.id()))))
            .collect();
        json!({
            "type": cs.cs_type.and_then(|t| g.rules().city_state_types().get(t)).map(|d| &*d.name),
            "ally": cs.ally().map(|a| a.0),
            "influence": influence,
        })
    });
    json!({
        "id": p.0,
        "kind": kind_name(pl.kind),
        "name": &*pl.name,
        "leader": &*pl.leader,
        "nation": name(g, Some(pl.nation)),
        "alive": pl.alive(),
        "controller": seat.controller().name(),
        "handicap": seat.handicap().name(),
        "auto": {
            "un_vote": auto.un_vote,
            "conquest": auto.conquest,
            "free_picks": auto.free_picks,
        },
        "overrides": overrides,
        "difficulty": name(g, difficulty),
        "gold": pl.econ.gold,
        "culture": pl.econ.culture,
        "faith": pl.econ.faith,
        "golden_age_turns": pl.econ.golden_age_turns,
        "free_policies": pl.policy.free_policies,
        "free_techs": pl.tech.free_techs,
        "future_techs": pl.tech.future_techs,
        "techs": sorted_names(g, pl.tech.known.iter()),
        "research": {
            "queue": pl.tech.queue.iter().filter_map(|&t| g.rules().name(t)).collect::<Vec<_>>(),
            "goal": name(g, pl.tech.goal),
        },
        "policies": sorted_names(g, pl.policy.adopted.iter()),
        "met": met,
        "capital": pl.capital.map(CityId::get),
        "cities": st.cities().of(p).iter().map(|c| c.get()).collect::<Vec<_>>(),
        "units": st.units().of(p).iter().map(|u| u.get()).collect::<Vec<_>>(),
        "explored": pl.explored.len(),
        "natural_wonders": sorted_names(g, pl.civ.natural_wonders.iter()),
        "notes": pl.major.as_deref().map_or("", |m| &*m.notes),
        "city_state": city_state,
    })
}

/// `tile`: one tile.
fn tile(g: &Game, t: TileIdx) -> Value {
    let Some(x) = g.tile(t) else { return Value::Null };
    let r = g.rules();
    let features = x.features().iter().filter_map(|f| r.derived().features.get(f).copied());
    let (tx, ty) = g.xy(t);
    let mut units: Vec<u32> = g.units_at(t).map(|u| u.id().get()).collect();
    units.sort();
    json!({
        "x": tx,
        "y": ty,
        "terrain": name(g, Some(x.terrain())),
        "features": sorted_names(g, features),
        "wonder": name(g, x.wonder()),
        "resource": name(g, x.resource()),
        "resource_amount": x.resource_amount(),
        "improvement": name(g, x.improvement()),
        "pillaged": x.improvement_pillaged(),
        "route": x.route().map(|r| match r { Route::Road => "Road", Route::Railroad => "Railroad" }),
        "route_pillaged": x.route_pillaged(),
        "river": x.river_mask(),
        "owner": x.owner().map(|p| p.0),
        "city": x.city().map(CityId::get),
        "units": units,
        "visible": g.derived().vis().seers(t).map(|p| p.0).collect::<Vec<_>>(),
    })
}

/// `relation`: two players' relation, the two-sided terms as `[a's, b's]`.
fn relation(g: &Game, a: PlayerId, b: PlayerId) -> Value {
    let r = g.relation(a, b).copied().unwrap_or_default();
    let (sa, sb) = (side(a, b), side(b, a));
    json!({
        "a": a.0,
        "b": b.0,
        "met": g.has_met(a, b),
        "war": r.war,
        "war_declared_by": r.war_declared_by.map(|p| p.0),
        "since": r.since,
        "treaty_until": r.treaty_until,
        "friendship_until": r.friendship_until,
        "pact_until": r.pact_until,
        "ra_until": r.ra_until,
        "embassy": [r.embassy[sa], r.embassy[sb]],
        "open_borders_until": [r.open_borders_until[sa], r.open_borders_until[sb]],
        "opinion": [opinion(g, a, b), opinion(g, b, a)],
        "friends": is_friends(g, a, b),
        "pact": has_pact(g, a, b),
    })
}

/// `unit`: one unit.
fn unit(g: &Game, u: UnitId) -> Value {
    let Some(x) = g.unit(u) else { return Value::Null };
    let (tx, ty) = g.xy(x.tile());
    json!({
        "id": u.get(),
        "owner": x.owner().0,
        "type": name(g, Some(x.base)),
        "x": tx,
        "y": ty,
        "hp": x.hp,
        "xp": x.xp,
        "promotions": sorted_names(g, x.promotions.iter()),
    })
}

/// `units`: every unit, or a player's, or those on a tile, by id.
fn units(g: &Game, o: &Map<String, Value>) -> Result<Value, ActionError> {
    let owner = match o.get("player") {
        None | Some(Value::Null) => None,
        Some(v) => Some(any_player(g, Some(v))?),
    };
    let at =
        if o.contains_key("x") || o.contains_key("y") { Some(scenario::tile(g, o)?) } else { None };
    let list: Vec<Value> = g
        .state()
        .units()
        .iter()
        .filter(|u| owner.is_none_or(|p| u.owner() == p) && at.is_none_or(|t| u.tile() == t))
        .map(|u| unit(g, u.id()))
        .collect();
    Ok(Value::Array(list))
}

/// `city`: one city.
fn city(g: &Game, c: CityId) -> Value {
    let Some(x) = g.city(c) else { return Value::Null };
    let (tx, ty) = g.xy(x.tile());
    json!({
        "id": c.get(),
        "name": &*x.name,
        "owner": x.owner().0,
        "x": tx,
        "y": ty,
        "pop": x.pop,
        "buildings": sorted_names(g, x.buildings.iter()),
    })
}

/// `events`: the events after id `since`, oldest first; only those of a `type`, and only those
/// `player` hears of, if given.
fn events(g: &Game, o: &Map<String, Value>) -> Result<Value, ActionError> {
    let since = match o.get("since") {
        None | Some(Value::Null) => 0,
        Some(v) => py::int_of(v)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad("since must be an event id."))?,
    };
    let kind = o.get("type").and_then(Value::as_str);
    let hearer = match o.get("player") {
        None | Some(Value::Null) => None,
        Some(v) => Some(pid(g, Some(v), false)?),
    };
    let list: Vec<Value> = g
        .chronicle()
        .events_since(since)
        .iter()
        .filter(|e| kind.is_none_or(|k| e.kind.name() == k))
        .filter(|e| hearer.is_none_or(|p| e.audience.is_none_or(|a| a.contains(p))))
        .map(|e| {
            json!({
                "id": e.id.get(),
                "turn": e.turn,
                "type": e.kind.name(),
                "text": &*e.text,
                "audience": e.audience.map(|a| a.iter().map(|p| p.0).collect::<Vec<_>>()),
            })
        })
        .collect();
    Ok(Value::Array(list))
}

/// `find_tiles`: the tiles that pass every filter given, sorted by distance from `x`, `y` (0
/// for all without them), then row, then column; the first `limit`, if given.
///
/// Filters: `radius` (at most this far); `terrain`, `feature`, `resource` and `improvement`
/// (by name: the tile has it); `owner` (a player id, or `none` for unowned tiles); `land`,
/// `river`, `city` and `units` (true or false: whether the tile is land, has a river, a city,
/// units); `bare` (true: no feature, resource, improvement or natural wonder).
pub fn find_tiles(g: &Game, o: &Map<String, Value>) -> Result<Value, ActionError> {
    const KNOWN: [&str; 15] = [
        "what",
        "x",
        "y",
        "radius",
        "terrain",
        "feature",
        "resource",
        "improvement",
        "owner",
        "land",
        "river",
        "city",
        "units",
        "bare",
        "limit",
    ];
    if let Some(k) = o.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(bad(format!(
            "find_tiles has no filter '{k}'. Filters: {}.",
            KNOWN[1..].join(", ")
        )));
    }
    let origin =
        if o.contains_key("x") || o.contains_key("y") { Some(scenario::tile(g, o)?) } else { None };
    let whole = |k: &str| -> Result<Option<i64>, ActionError> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                py::int_of(v).map(Some).ok_or_else(|| bad(format!("{k} must be a whole number.")))
            }
        }
    };
    let flag = |k: &str| o.get(k).filter(|v| !v.is_null()).map(py::truthy);
    let radius = whole("radius")?;
    let limit = whole("limit")?.map(|n| usize::try_from(n).unwrap_or(0));
    let terrain: Option<TerrainId> = o.get("terrain").map(|v| resolve(g, Some(v))).transpose()?;
    let feature: Option<TerrainId> = o.get("feature").map(|v| resolve(g, Some(v))).transpose()?;
    let feature = match feature {
        Some(f) => Some(g.rules().terrains()[f].feature.ok_or_else(|| {
            bad(format!("{} is not a terrain feature.", g.rules().terrains()[f].name))
        })?),
        None => None,
    };
    let resource: Option<ResourceId> =
        o.get("resource").map(|v| resolve(g, Some(v))).transpose()?;
    let improvement: Option<ImprovementId> =
        o.get("improvement").map(|v| resolve(g, Some(v))).transpose()?;
    let owner = match o.get("owner") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "none" => Some(None),
        Some(v) => Some(Some(any_player(g, Some(v))?)),
    };
    let (land, river, city, units, bare) =
        (flag("land"), flag("river"), flag("city"), flag("units"), flag("bare"));
    let r = g.rules();
    let mut found: Vec<(u32, i32, i32)> = Vec::new();
    for (t, x) in g.state().tiles().iter() {
        let distance = origin.map_or(0, |o| g.grid().distance(o, t));
        let is_land = r.terrains().get(x.terrain()).is_some_and(|d| d.kind == TerrainType::Land);
        let keep = radius.is_none_or(|n| i64::from(distance) <= n)
            && terrain.is_none_or(|id| x.terrain() == id)
            && feature.is_none_or(|f| x.features().contains(f))
            && resource.is_none_or(|id| x.resource() == Some(id))
            && improvement.is_none_or(|id| x.improvement() == Some(id))
            && owner.is_none_or(|p| x.owner() == p)
            && land.is_none_or(|on| is_land == on)
            && river.is_none_or(|on| x.has_river() == on)
            && city.is_none_or(|on| g.city_at(t).is_some() == on)
            && units.is_none_or(|on| g.units_at(t).next().is_some() == on)
            && bare.is_none_or(|on| {
                let empty = x.features().is_empty()
                    && x.resource().is_none()
                    && x.improvement().is_none()
                    && x.wonder().is_none();
                empty == on
            });
        if keep {
            let (tx, ty) = g.xy(t);
            found.push((distance, ty, tx));
        }
    }
    found.sort();
    found.truncate(limit.unwrap_or(usize::MAX));
    Ok(Value::Array(
        found.into_iter().map(|(d, y, x)| json!({"x": x, "y": y, "distance": d})).collect(),
    ))
}

/// `pending`: every query, operation, test operation and stage still waiting for its package.
fn pending() -> Value {
    let mut out = Vec::new();
    for (name, porting) in QUERIES {
        if let Porting::Pending(pkg) = porting {
            out.push(json!({"kind": "inspect", "name": name, "package": pkg}));
        }
    }
    for o in scenario::OPS {
        if let Porting::Pending(pkg) = o.porting {
            out.push(json!({"kind": "scenario_op", "name": o.name, "package": pkg}));
        }
    }
    for (name, pkg) in testops::pending() {
        out.push(json!({"kind": "test_op", "name": name, "package": pkg}));
    }
    for (table, s, pkg) in stages::waiting() {
        let name = format!("{table} {}: {}", s.id, s.name);
        out.push(json!({"kind": "turn_stage", "name": name, "package": pkg}));
    }
    for (s, pkg) in setup::waiting() {
        out.push(json!({"kind": "setup_stage", "name": s.name, "package": pkg}));
    }
    Value::Array(out)
}
