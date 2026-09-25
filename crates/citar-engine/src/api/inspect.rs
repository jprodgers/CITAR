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
//! - `preview` (`unit`, `x`, `y`): the attack preview (`combat.preview`), or its refusal as
//!   `{"error": ...}`;
//! - `buildable` (`city`): what a city can build now and what each costs in production;
//! - `costs` (`player`): what the techs a civilization could research cost it, its next policy's
//!   culture, and the policies it could adopt;
//! - `religion` (`player` or `city`): a civilization's pantheon or religion, its beliefs and what
//!   the next pantheon and prophet cost it; a city's majority, followers, pressures and holiness;
//! - `great_people` (`player`): great person points, free great people, golden ages and the
//!   uniques a civilization holds for some turns;
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
use crate::game::cities::{construction, stats as cstats};
use crate::game::diplomacy::relations::{has_pact, is_friends, opinion};
use crate::game::error::{ActionError, ErrCode};
use crate::game::turn::stages;
use crate::game::{Game, Porting, setup};
use crate::game::{policies, research};
use crate::rules::Named;
use crate::rules::defs::{Route, TerrainType};
use crate::state::Phase;
use crate::state::cities::Constructible;
use crate::state::diplo::side;
use crate::state::players::{AutoDecision, Player, PlayerKind};

/// The queries, by `what`, sorted, with whether what each reads is ported yet.
const QUERIES: [(&str, Porting); 19] = [
    ("briefing", Porting::Pending("1d-03")),
    ("buildable", Porting::Ported),
    ("city", Porting::Ported),
    ("costs", Porting::Ported),
    ("events", Porting::Ported),
    ("find_tiles", Porting::Ported),
    ("game", Porting::Ported),
    ("great_people", Porting::Ported),
    ("negotiation", Porting::Pending("1c-05")),
    ("ops", Porting::Ported),
    ("pending", Porting::Ported),
    ("player", Porting::Ported),
    ("preview", Porting::Ported),
    ("relation", Porting::Ported),
    ("religion", Porting::Ported),
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
        "preview" => {
            let id = py::int_of(o.get("unit").unwrap_or(&Value::Null))
                .and_then(|n| u32::try_from(n).ok())
                .and_then(UnitId::new)
                .filter(|&u| g.unit(u).is_some())
                .ok_or_else(|| bad("No such unit."))?;
            let t = scenario::tile(g, o)?;
            Ok(crate::game::combat::resolve::preview(g, id, t)
                .unwrap_or_else(|e| json!({"error": e.message})))
        }
        "city" => {
            let id = py::int_of(o.get("city").unwrap_or(&Value::Null))
                .and_then(|n| u32::try_from(n).ok())
                .and_then(CityId::new)
                .filter(|&c| g.city(c).is_some())
                .ok_or_else(|| bad("No such city."))?;
            Ok(city(g, id))
        }
        "buildable" => {
            let id = py::int_of(o.get("city").unwrap_or(&Value::Null))
                .and_then(|n| u32::try_from(n).ok())
                .and_then(CityId::new)
                .filter(|&c| g.city(c).is_some())
                .ok_or_else(|| bad("No such city."))?;
            Ok(buildable(g, id))
        }
        "costs" => Ok(costs(g, pid(g, o.get("player"), false)?)),
        "religion" => match o.get("city") {
            Some(v) if !v.is_null() => {
                let id = py::int_of(v)
                    .and_then(|n| u32::try_from(n).ok())
                    .and_then(CityId::new)
                    .filter(|&c| g.city(c).is_some())
                    .ok_or_else(|| bad("No such city."))?;
                Ok(city_religion(g, id))
            }
            _ => Ok(civ_religion(g, pid(g, o.get("player"), false)?)),
        },
        "great_people" => Ok(great_people(g, pid(g, o.get("player"), false)?)),
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
            "progress": sorted_map(pl.tech.progress.iter().filter_map(|(&t, &v)| {
                g.rules().name(t).map(|n| (n, json!(v)))
            })),
            "overflow": pl.tech.overflow,
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
        "happiness": crate::game::query::happiness(g, p).total,
        "happiness_seen": pl.econ.happiness_seen,
        "gold_rate": pl.econ.last_gold_rate,
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
        "moves": x.moves,
        "max_moves": crate::game::movement::max_moves(g, u),
        "activity": x.activity.map(|a| a.name()),
        "goto": x.goto.map(|t| { let (gx, gy) = g.xy(t); json!({"x": gx, "y": gy}) }),
        "fortify": x.fortify,
        "embarked": crate::game::movement::is_embarked(g, u),
        "carried_by": x.carried_by().map(UnitId::get),
        "set_up": x.set_up,
        "original_owner": x.original_owner.map(|p| p.0),
        "return_offer": x.return_offer.map(|p| p.0),
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
    let xys = |tiles: &[TileIdx]| {
        let mut v: Vec<(i32, i32)> = tiles.iter().map(|&t| g.xy(t)).collect();
        v.sort();
        v.into_iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>()
    };
    let r = g.rules();
    let mut specialists: Vec<(&str, u8)> = x
        .specialists
        .iter()
        .enumerate()
        .filter(|&(_, &n)| n > 0)
        .filter_map(|(i, &n)| {
            let s = crate::base::ids::SpecialistId(u8::try_from(i).ok()?);
            Some((&*r.specialists().get(s)?.name, n))
        })
        .collect();
    specialists.sort();
    let specialists: Map<String, Value> =
        specialists.into_iter().map(|(k, n)| (k.to_owned(), json!(n))).collect();
    let total = crate::game::query::city_stats(g, c).total;
    let yields: Map<String, Value> = crate::base::stats::Stat::ALL
        .into_iter()
        .map(|k| (k.key().to_owned(), json!(total[k])))
        .collect();
    json!({
        "id": c.get(),
        "name": &*x.name,
        "owner": x.owner().0,
        "x": tx,
        "y": ty,
        "pop": x.pop,
        "buildings": sorted_names(g, x.buildings.iter()),
        "worked": xys(&x.worked),
        "locked": xys(&x.locked),
        "workable": xys(&crate::game::cities::stats::workable_tiles(g, c)),
        "specialists": specialists,
        "focus": x.focus.name(),
        "avoid_growth": x.avoid_growth,
        "food": x.food,
        "yields": yields,
        "queue": x.queue.iter().map(|&i| construction::item_name(r, i)).collect::<Vec<_>>(),
        "progress": sorted_map(
            x.progress.iter().map(|(&i, &v)| (construction::item_name(r, i), json!(v))),
        ),
        "overflow": x.overflow,
        "culture": x.culture,
        "health": x.health,
        "max_health": crate::game::cities::stats::max_health(g, c),
        "tiles": crate::game::economy::city_tiles(g, c).len(),
        "founder": x.founder.0,
        "previous_owner": x.previous_owner.map(|p| p.0),
        "original_capital": x.original_capital,
        "puppet": x.puppet,
        "razing": x.razing,
        "resistance": x.resistance,
        "attacked": x.attacked,
    })
}

/// `religion` with `player`: a civilization's pantheon or religion, its beliefs and free beliefs,
/// and what the next pantheon and great prophet cost it.
fn civ_religion(g: &Game, p: PlayerId) -> Value {
    use crate::game::religion::{self as rel, prophets};
    let Some(pl) = g.player(p) else { return Value::Null };
    let founded = pl.religion.founded;
    let beliefs = founded.map(|r| rel::all_beliefs(g, r)).unwrap_or_default();
    let free: Map<String, Value> = crate::rules::defs::BeliefKind::ALL
        .into_iter()
        .filter(|&k| pl.religion.free(k) > 0)
        .map(|k| (k.name().to_owned(), json!(pl.religion.free(k))))
        .collect();
    json!({
        "state": pl.religion.progress.name(),
        "religion": founded.map(|r| rel::key_name(g, r)),
        "display": founded.map(|r| rel::display_name(g, r)),
        "beliefs": sorted_names(g, beliefs),
        "free_beliefs": free,
        "pantheon_cost": prophets::faith_for_pantheon(g, 0),
        "prophet_cost": prophets::faith_for_next_prophet(g, p),
        "prophets_earned": prophets::prophets_earned(g, p),
        "holy_city": founded.and_then(|r| rel::holy_city(g, r)).map(CityId::get),
    })
}

/// `religion` with `city`: its majority religion, its followers and pressures by religion, and
/// whose holy city it is.
fn city_religion(g: &Game, c: CityId) -> Value {
    use crate::game::religion as rel;
    let Some(x) = g.city(c) else { return Value::Null };
    let named = |r: Option<crate::base::ids::ReligionId>| {
        r.map_or_else(|| "None".to_owned(), |r| rel::key_name(g, r))
    };
    let followers: Map<String, Value> =
        rel::followers(x).iter().map(|&(r, n)| (named(Some(r)), json!(n))).collect();
    let pressures: Map<String, Value> =
        x.pressures.iter().map(|&(r, v)| (named(r), json!(v))).collect();
    json!({
        "majority": rel::majority_religion(g, c).map(|r| rel::key_name(g, r)),
        "followers": followers,
        "pressures": pressures,
        "holy_city_of": x.holy_city_of.map(|r| rel::key_name(g, r)),
    })
}

/// `great_people`: a civilization's great person points, its free great people, its golden ages
/// and the uniques it holds for some turns (by the timed unique's text).
fn great_people(g: &Game, p: PlayerId) -> Value {
    let Some(pl) = g.player(p) else { return Value::Null };
    let t = g.rules().uniques();
    let points = sorted_map(
        pl.gp.points.iter().filter_map(|(&u, &v)| g.rules().name(u).map(|n| (n, json!(v)))),
    );
    let temp: Vec<Value> = pl
        .civ
        .temp_uniques
        .iter()
        .map(|x| {
            let timed = match t.meta(x.unique).source {
                crate::unique::table::Source::Temporary(orig) => orig,
                _ => x.unique,
            };
            json!({"text": t.text_of(timed), "turns": x.turns})
        })
        .collect();
    json!({
        "points": points,
        "free": pl.gp.free,
        "earned": pl.gp.earned,
        "golden_age_points": pl.econ.golden_age_points,
        "golden_ages": pl.econ.golden_ages,
        "golden_age_turns": pl.econ.golden_age_turns,
        "golden_age_needed": crate::game::great_people::happiness_for_golden_age(g, p),
        "temp_uniques": temp,
    })
}

/// A map from names, sorted by name.
fn sorted_map<'a>(entries: impl Iterator<Item = (&'a str, Value)>) -> Value {
    let mut v: Vec<(&str, Value)> = entries.collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    Value::Object(v.into_iter().map(|(k, x)| (k.to_owned(), x)).collect())
}

/// `buildable`: what a city can build now, by kind, each list sorted, and what each unit,
/// building and wonder costs it in production.
fn buildable(g: &Game, c: CityId) -> Value {
    let r = g.rules();
    let items = construction::buildable_items(g, c);
    let owner = g.city(c).map(crate::state::cities::City::owner).unwrap_or(PlayerId(0));
    let units = sorted_names(g, items.units.iter());
    let buildings = sorted_names(g, items.buildings.iter());
    let wonders = sorted_names(g, items.wonders.iter());
    let mut other = Vec::new();
    if items.gold {
        other.push("Gold");
    }
    if items.science {
        other.push("Science");
    }
    let things = items
        .units
        .iter()
        .map(Constructible::Unit)
        .chain(items.buildings.iter().chain(items.wonders.iter()).map(Constructible::Building));
    let production = sorted_map(things.map(|i| {
        (construction::item_name(r, i), json!(cstats::production_cost(g, owner, i, Some(c))))
    }));
    json!({
        "units": units,
        "buildings": buildings,
        "wonders": wonders,
        "other": other,
        "production": production,
    })
}

/// `costs`: what each tech a civilization could research now costs it, its next policy's
/// culture, and (a major's) the policies and branches it could adopt, sorted.
fn costs(g: &Game, p: PlayerId) -> Value {
    let r = g.rules();
    let techs = sorted_map(
        research::available_techs(g, p)
            .into_iter()
            .filter_map(|t| r.name(t).map(|n| (n, json!(research::tech_cost(g, p, t))))),
    );
    let major = g.player(p).is_some_and(Player::is_major);
    let adoptable =
        if major { sorted_names(g, policies::adoptable_policies(g, p)) } else { json!([]) };
    json!({"tech": techs, "policy": policies::culture_cost(g, p, None), "adoptable": adoptable})
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
