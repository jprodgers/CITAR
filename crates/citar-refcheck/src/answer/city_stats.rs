//! The `city_stats` group (DESIGN.md 9.2): everything the engine derives for each city.
//!
//! Package 1b-06 answers a city's stats breakdown (`cities.city_stats`: each source's stats with
//! the keys Python's dicts held, the total, the happiness list, the tiles' yields and the
//! percentages), the food to its next citizen, its building maintenance, its most health, its
//! combat strength, the tiles it could work, its trade route to the capital, and the turns to
//! build what it builds. Package 1b-08 adds its religion: its followers, its majority religion,
//! and the pressure its surroundings put on it this turn (`pressure_in`,
//! `religion.pressures_from_surroundings`).

use citar_engine::base::stats::{Stat, StatMask};
use citar_engine::game::cities::stats::{self as cstats, Yields};
use citar_engine::game::{Game, query, religion};
use citar_engine::rules::Ruleset;
use citar_engine::state::cities::Constructible;
use citar_engine::state::world::ReligionName;
use serde_json::{Map, Value, json};

use super::tile_yields::yields;
use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `city_stats` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct CityStats;

impl AnswerModule for CityStats {
    fn group(&self) -> Group {
        Group::CityStats
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let cities: Vec<Value> = g.state().cities().iter().map(|c| city(g, c.id())).collect();
        Ok(json!({ "cities": cities }))
    }
}

/// The tiles a city could work, as the group compares them: the engine's, and those it leaves out
/// only because another city of the owner works them where Python let the city take them too.
/// Python refused a tile only when the city of the tile's territory worked it, so two cities
/// could work one tile; the engine never lets them (`cities-never-share-a-tile`, a rule script's
/// difference). Adding those back leaves every other way the engine could leave a tile out to the
/// comparison.
fn workable(g: &Game, c: citar_engine::base::ids::CityId) -> Vec<u32> {
    let mut out = cstats::workable_tiles(g, c);
    if let Some(city) = g.city(c) {
        let owner = city.owner();
        let st = g.state();
        let works = |x: citar_engine::base::ids::CityId, t| {
            g.city(x).is_some_and(|y| y.worked.contains(&t))
        };
        for t in cstats::tiles_in_range(g, c) {
            let Some(tile) = g.tile(t) else { continue };
            let blocked = g.military_at(t).is_some_and(|m| g.at_war(owner, m.owner()));
            if out.contains(&t)
                || t == city.tile()
                || tile.owner() != Some(owner)
                || st.city_at(t).is_some()
                || blocked
            {
                continue;
            }
            let sibling = st.cities().of(owner).iter().any(|&x| x != c && works(x, t));
            let refused = tile.city().is_some_and(|x| x != c && works(x, t));
            if sibling && !refused {
                out.push(t);
            }
        }
    }
    let mut ids: Vec<u32> = out.iter().map(|t| t.0).collect();
    ids.sort();
    ids
}

/// A source's stats as Python's dict held them: the keys it named.
pub fn named(y: &Yields) -> Value {
    keyed(&y.stats, y.keys)
}

fn keyed(s: &citar_engine::base::stats::Stats, keys: StatMask) -> Value {
    Value::Object(keys.iter().map(|k| (k.key().to_owned(), json!(s[k]))).collect::<Map<_, _>>())
}

/// The name of something a city builds.
fn item(r: &Ruleset, c: Constructible) -> String {
    match c {
        Constructible::Building(b) => r.buildings()[b].name.to_string(),
        Constructible::Unit(u) => r.base_units()[u].name.to_string(),
        Constructible::Perpetual(p) => p.name().to_owned(),
    }
}

/// A religion's name, as Python keyed religions.
fn religion_name(g: &Game, id: citar_engine::base::ids::ReligionId) -> Value {
    let r = g.rules();
    match g.state().world().religion(id).map(|x| x.name) {
        Some(ReligionName::Pantheon(b)) => json!(&*r.beliefs()[b].name),
        Some(ReligionName::Religion(x)) => {
            r.religions().get(x).map_or(Value::Null, |n| json!(&**n))
        }
        None => Value::Null,
    }
}

fn city(g: &Game, c: citar_engine::base::ids::CityId) -> Value {
    let r = g.rules();
    let Some(x) = g.city(c) else { return Value::Null };
    let parts = query::city_parts(g, c);
    let stats = query::city_stats(g, c);
    let fin: Map<String, Value> =
        stats.breakdown.iter().map(|(src, y)| (src.name().to_owned(), named(y))).collect();
    let hl: Map<String, Value> =
        parts.happiness.iter().map(|&(k, v)| (k.name().to_owned(), json!(v))).collect();
    let mut e = json!({
        "id": c.get(),
        "owner": x.owner().0,
        "name": &*x.name,
        "stats": {
            "final": fin,
            "total": yields(&stats.total),
            "happiness_list": hl,
            "tiles": yields(&parts.tiles),
            "food_surplus": stats.total[Stat::Food],
            "production": stats.total[Stat::Production],
            "pct": named(&stats.pct),
        },
        "food_to_next_pop": cstats::food_to_next_pop(g, c),
        "maintenance": cstats::maintenance(g, c),
        "max_health": cstats::max_health(g, c),
        "strength": cstats::city_strength(g, c),
        "workable": workable(g, c),
        "connected_to_capital": query::connected_to_capital(g, c),
    });
    if let Some(cur) = cstats::current_construction(x) {
        e["current"] = json!({"item": item(r, cur), "turns": cstats::turns_to_build(g, c, cur)});
    }
    if g.religion_enabled() {
        let followers: Map<String, Value> = religion::followers(x)
            .iter()
            .filter_map(|&(rel, n)| {
                religion_name(g, rel).as_str().map(|k| (k.to_owned(), json!(n)))
            })
            .collect();
        e["followers"] = Value::Object(followers);
        e["majority"] =
            religion::majority_religion(g, c).map_or(Value::Null, |m| religion_name(g, m));
        let pressure_in: Map<String, Value> = religion::pressures_from_surroundings(g, c)
            .iter()
            .filter_map(|&(rel, n)| {
                religion_name(g, rel).as_str().map(|k| (k.to_owned(), json!(n)))
            })
            .collect();
        e["pressure_in"] = Value::Object(pressure_in);
    }
    e
}
