//! The `city_stats` group (DESIGN.md 9.2): everything the engine derives for each city.
//!
//! Package 1b-06 answers a city's stats breakdown (`cities.city_stats`: each source's stats with
//! the keys Python's dicts held, the total, the happiness list, the tiles' yields and the
//! percentages), the food to its next citizen, its building maintenance, its most health, its
//! combat strength, the tiles it could work, its trade route to the capital, and the turns to
//! build what it builds. Its followers and majority religion are the reads package 1b-08's
//! religion builds on; the pressure on it from its neighbours (`pressure_in`) is 1b-08's, and
//! missing here until then.

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
        "workable": cstats::workable_tiles(g, c).iter().map(|t| t.0).collect::<Vec<_>>(),
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
    }
    e
}
