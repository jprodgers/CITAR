//! The `civs` group (DESIGN.md 9.2): per civilization, what the engine derives for it.
//!
//! Package 1b-05 answers the paths of the civilization-level data it ports:
//! - `resource_supply` and `detailed_resources`: the `ResourceSupply` memo, each line as
//!   `[resource, origin, amount]` with Python's names for the origins;
//! - `unique_index`: the placeholders of the uniques the civilization's sources give it, counted
//!   as Python's `civ_index` counted them (`game::query::unique_index_counts`: the entries of the
//!   `CivIndexFull` memo with their copies, and what the index leaves out by design);
//! - `unit_maintenance` and `unit_supply`;
//! - `era`, which the unique index is built with.
//!
//! Package 1b-06 answers `happiness`, `civ_stats`, `stat_map` and `gold_per_turn`: the memos
//! `Happiness` and `CivStats`, with Python's keys.
//!
//! The group's other paths (costs, score, victory, the world) are answered by the packages that
//! port them; until then they are missing here, and counted in the ratchet.

use citar_engine::base::ids::PlayerId;
use citar_engine::base::num;
use citar_engine::game::economy::ResourceItem;
use citar_engine::game::{Game, economy, query};
use citar_engine::rules::Ruleset;
use citar_engine::state::players::PlayerKind;
use serde_json::{Map, Value, json};

use super::city_stats::named;
use super::tile_yields::yields;
use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `civs` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Civs;

impl AnswerModule for Civs {
    fn group(&self) -> Group {
        Group::Civs
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        Ok(json!({ "civs": civs(g) }))
    }
}

/// Every living civilization but the barbarians, by id (`queries._civs`).
fn civs(g: &Game) -> Vec<Value> {
    let r = g.rules();
    let mut out = Vec::new();
    for (p, player) in g.state().players().iter() {
        if !player.alive() || player.is_barbarian() {
            continue;
        }
        let kind = match player.kind {
            PlayerKind::Major => "major",
            PlayerKind::CityState => "city_state",
            PlayerKind::Barbarian => "barbarian",
        };
        let supply: Map<String, Value> = query::resource_supply(g, p)
            .into_iter()
            .map(|(res, n)| (r.resources()[res].name.to_string(), Value::from(n)))
            .collect();
        let detailed: Vec<Value> =
            query::detailed_resources(g, p).iter().map(|it| line(r, it)).collect();
        let index: Map<String, Value> = query::unique_index_counts(g, p)
            .into_iter()
            .map(|(ph, n)| (ph, Value::from(n)))
            .collect();
        let cs = query::civ_stats(g, p);
        let stat_map: Map<String, Value> =
            cs.map.iter().map(|(src, y)| ((*src).to_owned(), named(y))).collect();
        let gpt = economy::gold_per_turn(g, p);
        out.push(json!({
            "pid": p.0,
            "kind": kind,
            "happiness": happiness(g, p),
            "civ_stats": yields(&cs.total),
            "stat_map": stat_map,
            "gold_per_turn": {
                "income": gpt.income,
                "building_maintenance": gpt.building_maintenance,
                "unit_upkeep": gpt.unit_upkeep,
                "route_maintenance": gpt.route_maintenance,
                "trade": gpt.trade,
                "net": gpt.net,
            },
            "resource_supply": supply,
            "detailed_resources": detailed,
            "unique_index": index,
            "unit_maintenance": economy::unit_maintenance(g, p),
            "unit_supply": economy::unit_supply(g, p),
            "era": query::era(g, p).0,
        }));
    }
    out
}

/// A civilization's happiness as Python answered it (`economy.happiness`): the total, the
/// sources that are not zero rounded to two places, the luxuries it has (a major's), and its
/// mood.
fn happiness(g: &Game, p: PlayerId) -> Value {
    let h = query::happiness(g, p);
    let breakdown: Map<String, Value> = h
        .breakdown
        .iter()
        .filter(|&&(_, x)| x != 0.0)
        .map(|&(k, x)| (k.to_owned(), json!(num::round_ndigits(x, 2))))
        .collect();
    let mut out = json!({"total": h.total, "breakdown": breakdown, "status": h.status()});
    if h.major {
        let r = g.rules();
        let mut lux: Vec<&str> = h.luxury_types.iter().map(|&x| &*r.resources()[x].name).collect();
        lux.sort_unstable();
        out["luxury_types"] = json!(lux);
    }
    out
}

/// A line of the supply as Python wrote it: `[resource, origin, amount]`.
fn line(r: &Ruleset, it: &ResourceItem) -> Value {
    json!([&*r.resources()[it.resource].name, it.origin.name(r), it.amount])
}
