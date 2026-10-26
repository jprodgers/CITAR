//! What a briefing offers for the decisions left this turn: each idle unit's useful orders, what
//! each idle city can build, and the techs to research (`briefing._unit_options` and
//! `_city_options`, `briefing.py:332-406`, and `briefing.py:613-621`).

use serde_json::Value;

use super::map::where_from;
use crate::api::views::tiles::Known;
use crate::base::ids::{PlayerId, TechId, UnitId};
use crate::base::py;
use crate::game::cities::borders::within_order;
use crate::game::cities::construction::{buildable_items, item_name};
use crate::game::cities::stats::{production_cost, turns_to_build};
use crate::game::combat::resolve;
use crate::game::units::{self, health, promotions};
use crate::game::{Game, actions, automation, research, workers};
use crate::rules::defs::Domain;
use crate::state::cities::{Constructible, Perpetual};
use crate::unique::UniqueType;

/// How many idle units a briefing gives options for (`briefing.py:341`).
const UNITS_SHOWN: usize = 14;
/// How many idle cities it lists what they can build for (`briefing.py:396`).
const CITIES_SHOWN: usize = 6;
/// How many items of each kind an idle city lists (`briefing.py:404`).
const ITEMS_SHOWN: usize = 14;
/// How many techs to choose from it lists (`briefing.py:617`).
const TECHS_SHOWN: usize = 14;

/// Whether a unit still wants orders this turn: it has none and can move.
pub(super) fn needs_orders(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| x.activity.is_none() && x.moves > 0)
}

/// The options of each unit of `units` that needs orders, the first [`UNITS_SHOWN`] of them
/// (`briefing._unit_options`): the actions it may take, where it could found a city, what it
/// could build where it stands, what it could attack and at what cost, its promotions, the ruins
/// and camps near it that the reader knows of, and, for a military unit with nothing to attack,
/// how much is unexplored around it.
pub(super) fn unit_options(g: &Game, pid: PlayerId, units: &[UnitId]) -> Vec<String> {
    let idle: Vec<UnitId> = units.iter().copied().filter(|&u| needs_orders(g, u)).collect();
    if idle.is_empty() {
        return Vec::new();
    }
    let Some(pl) = g.player(pid) else { return Vec::new() };
    let r = g.rules();
    let known = &r.derived().known;
    let explored = |t: crate::base::ids::TileIdx| pl.explored.contains(t.0);
    let vis = g.derived().vis();
    let mut out = vec!["\nOPTIONS FOR UNITS NEEDING ORDERS:".to_owned()];
    for u in idle.into_iter().take(UNITS_SHOWN) {
        let Some(x) = g.unit(u) else { continue };
        let d = &r.base_units()[x.base];
        let at = x.tile();
        let mut bits: Vec<String> = Vec::new();
        let acts: Vec<String> = actions::unit_actions(g, u)
            .into_iter()
            .filter(actions::UnitActionEntry::available)
            .take(6)
            .map(|a| format!("{} ({})", a.id, a.name))
            .collect();
        if !acts.is_empty() {
            bits.push(format!("unit_action: {}", acts.join(", ")));
        }
        if units::type_has(g, x.base, UniqueType::FoundCity) {
            let sites: Vec<String> = automation::suggest_city_sites(g, pid, at, 6, 3)
                .into_iter()
                .map(|(t, _)| where_from(g, at, t))
                .collect();
            if !sites.is_empty() {
                bits.push(format!("good city sites: {}", sites.join(", ")));
            }
        }
        if let Some(b) = workers::Builder::unit(g, u) {
            let opts: Vec<String> = workers::build_options(g, &b, at, None)
                .into_iter()
                .take(8)
                .map(|o| format!("{} ({}t)", r.improvements()[o.imp].name, o.turns))
                .collect();
            if !opts.is_empty() {
                bits.push(format!("build here: {}; or unit_order automate", opts.join(", ")));
            }
        }
        if d.military && d.domain != Domain::Air && resolve::can_attack_now(g, u).is_none() {
            let radius = if d.ranged { health::attack_range(g, u) } else { 1 };
            let mut near = g.grid().within(at, u32::try_from(radius).unwrap_or(0));
            near.sort_by_key(|&t| within_order(g, at, t));
            let targets: Vec<String> = near
                .into_iter()
                .skip(1)
                .filter_map(|t| {
                    let pv = resolve::preview(g, u, t).ok()?;
                    let field = |k: &str| pv.get(k).cloned().unwrap_or(Value::Null);
                    Some(format!(
                        "{} {}: deal {}, take {}",
                        py::str_of(&field("defender")),
                        where_from(g, at, t),
                        py::str_of(&field("damage_to_defender")),
                        py::str_of(&field("damage_to_attacker"))
                    ))
                })
                .take(4)
                .collect();
            if !targets.is_empty() {
                bits.push(format!("attack targets: {}", targets.join("; ")));
            }
        }
        if promotions::can_promote(g, u) {
            let mut names: Vec<&str> = promotions::available_promotions(g, u)
                .into_iter()
                .filter_map(|p| r.name(p))
                .collect();
            names.sort();
            names.truncate(8);
            bits.push(format!("promotions: {}", names.join(", ")));
        }
        let mut around = g.grid().within(at, 6);
        around.sort_by_key(|&t| within_order(g, at, t));
        // The ruins and camps as the reader knows them, as its map and points of interest show
        // them: a camp raised in the fog since it looked is not there for it, and one it saw is
        // until it looks again. Python read what stood there now.
        // refcheck: briefing-nearby-reads-what-it-knows
        let near: Vec<String> = around
            .iter()
            .skip(1)
            .filter(|&&t| explored(t))
            .filter_map(|&t| {
                let tile = g.tile(t)?;
                let imp = Known::of(g, t, tile, Some(pid), vis.sees(pid, t)).improvement?;
                if Some(imp) == known.ancient_ruins {
                    Some(format!("ancient ruins {}", where_from(g, at, t)))
                } else if Some(imp) == known.barbarian_camp {
                    Some(format!("barbarian camp {}", where_from(g, at, t)))
                } else {
                    None
                }
            })
            .take(3)
            .collect();
        if !near.is_empty() {
            bits.push(format!("nearby: {}", near.join("; ")));
        }
        if d.military && !bits.iter().any(|b| b.starts_with("attack")) {
            let unexplored = g.grid().within(at, 4).into_iter().filter(|&t| !explored(t)).count();
            if unexplored > 0 {
                bits.push(format!("{unexplored} unexplored tiles within 4 (unit_order explore)"));
            }
        }
        let (ux, uy) = g.xy(at);
        let what = if bits.is_empty() {
            "move_unit, fortify, sleep or skip".to_owned()
        } else {
            bits.join(" | ")
        };
        out.push(format!("  [#{}] {} ({ux},{uy}): {what}", u.get(), d.name));
    }
    out
}

/// What each of the first [`CITIES_SHOWN`] idle cities could build, with each item's cost and
/// turns (`briefing._city_options`).
pub(super) fn city_options(g: &Game, pid: PlayerId) -> Vec<String> {
    let idle: Vec<_> = g.player_cities(pid).filter(|c| c.queue.is_empty() && !c.puppet).collect();
    if idle.is_empty() {
        return Vec::new();
    }
    let r = g.rules();
    let mut out = vec!["\nIDLE CITIES — what they can build (item cost/turns):".to_owned()];
    for c in idle.into_iter().take(CITIES_SHOWN) {
        let id = c.id();
        let items = buildable_items(g, id);
        let costed = |item: Constructible| {
            format!(
                "{} {}/{}t",
                item_name(r, item),
                production_cost(g, pid, item, Some(id)),
                turns_to_build(g, id, item)
            )
        };
        let mut parts: Vec<String> = Vec::new();
        let mut kind = |name: &str, list: Vec<String>| {
            if !list.is_empty() {
                parts.push(format!("{name}: {}", list.join(", ")));
            }
        };
        kind(
            "units",
            items.units.iter().take(ITEMS_SHOWN).map(|u| costed(Constructible::Unit(u))).collect(),
        );
        kind(
            "buildings",
            items
                .buildings
                .iter()
                .take(ITEMS_SHOWN)
                .map(|b| costed(Constructible::Building(b)))
                .collect(),
        );
        kind(
            "wonders",
            items
                .wonders
                .iter()
                .take(ITEMS_SHOWN)
                .map(|b| costed(Constructible::Building(b)))
                .collect(),
        );
        let other = [(items.gold, Perpetual::Gold), (items.science, Perpetual::Science)];
        kind(
            "other",
            other.iter().filter(|&&(on, _)| on).map(|&(_, p)| p.name().to_owned()).collect(),
        );
        out.push(format!("  [#{}] {}: {}", id.get(), c.name, parts.join(" | ")));
    }
    out
}

/// The techs `pid` could research, cheapest first, with what each unlocks
/// (`briefing.py:613-621`); nothing when there are none.
pub(super) fn available_techs(g: &Game, pid: PlayerId) -> Vec<String> {
    let mut avail: Vec<(i32, TechId)> = research::available_techs(g, pid)
        .into_iter()
        .map(|t| (research::tech_cost(g, pid, t), t))
        .collect();
    if avail.is_empty() {
        return Vec::new();
    }
    // Stable, so equal costs keep the tree's order, as Python's `sorted` kept it.
    avail.sort_by_key(|&(cost, _)| cost);
    let r = g.rules();
    let mut out = vec!["\nAVAILABLE TECHS (name: cost → unlocks):".to_owned()];
    for (cost, t) in avail.into_iter().take(TECHS_SHOWN) {
        let mut what: Vec<String> = Vec::new();
        if let Some(u) = r.derived().unlocks.get(t) {
            what.extend(u.units.iter().map(|&x| r.base_units()[x].name.to_string()));
            what.extend(u.buildings.iter().map(|&x| r.buildings()[x].name.to_string()));
            what.extend(u.improvements.iter().map(|&x| r.improvements()[x].name.to_string()));
            what.extend(u.reveals.iter().map(|&x| format!("reveals {}", r.resources()[x].name)));
        }
        let what = if what.is_empty() {
            "prerequisite for later techs".to_owned()
        } else {
            what.join(", ")
        };
        out.push(format!("  {}: {cost} → {what}", r.techs()[t].name));
    }
    out
}
