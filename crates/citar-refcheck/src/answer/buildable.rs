//! The `buildable` group (DESIGN.md 9.2): for each living major's city, what it can build
//! (`cities.buildable_items`), and for each unit, building and wonder in that list its production
//! cost, the turns it would take, and whether and for how much it can be bought with gold and with
//! faith (`cities.purchase_check`, as `[refusal or null, cost or null]`).
//!
//! Package 1b-07 answers it from the engine's `Buildable` memo, `production_cost`,
//! `turns_to_build` and `purchase_check`.

use citar_engine::base::ids::CityId;
use citar_engine::base::stats::Stat;
use citar_engine::game::Game;
use citar_engine::game::cities::construction::{self, item_name};
use citar_engine::game::cities::purchase::purchase_check;
use citar_engine::game::cities::stats as cstats;
use citar_engine::state::cities::Constructible;
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `buildable` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Buildable;

impl AnswerModule for Buildable {
    fn group(&self) -> Group {
        Group::Buildable
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let cities: Vec<Value> = g
            .state()
            .cities()
            .iter()
            .filter(|c| g.player(c.owner()).is_some_and(|p| p.is_major() && p.alive()))
            .map(|c| city(g, c.id()))
            .collect();
        Ok(json!({ "cities": cities }))
    }
}

/// One city's list and costs (`queries.buildable`).
fn city(g: &Game, c: CityId) -> Value {
    let r = g.rules();
    let items = construction::buildable_items(g, c);
    let owner = g.city(c).map(citar_engine::state::cities::City::owner);
    let units: Vec<Constructible> = items.units.iter().map(Constructible::Unit).collect();
    let buildings: Vec<Constructible> =
        items.buildings.iter().map(Constructible::Building).collect();
    let wonders: Vec<Constructible> = items.wonders.iter().map(Constructible::Building).collect();
    let names = |v: &[Constructible]| -> Vec<&str> { v.iter().map(|&i| item_name(r, i)).collect() };
    let mut other = Vec::new();
    if items.gold {
        other.push("Gold");
    }
    if items.science {
        other.push("Science");
    }
    let mut costs = Map::new();
    for &item in units.iter().chain(&buildings).chain(&wonders) {
        let check = |stat| {
            let (reason, cost) = purchase_check(g, c, item, stat);
            json!([reason, cost])
        };
        let production = owner.map_or(0, |p| cstats::production_cost(g, p, item, Some(c)));
        costs.insert(
            item_name(r, item).to_owned(),
            json!({
                "production": production,
                "turns": cstats::turns_to_build(g, c, item),
                "gold": check(Stat::Gold),
                "faith": check(Stat::Faith),
            }),
        );
    }
    json!({
        "city": c.get(),
        "owner": owner.map(|p| p.0),
        "items": {
            "units": names(&units),
            "buildings": names(&buildings),
            "wonders": names(&wonders),
            "other": other,
        },
        "costs": costs,
    })
}
