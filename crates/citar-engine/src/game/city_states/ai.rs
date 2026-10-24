//! A city-state's own turn (`city_states.take_turn` and `_found_with_settlers`,
//! `city_states.py:1220-1320`): it founds its city with its settler, each city bombards what it
//! can and picks what to build when its queue runs out, and its military units attack what is in
//! reach, else hold the capital or come home.
//!
//! Production reads the cached `Buildable` list (`construction::buildable_items`): Gold when it is
//! short of gold and losing it, a defender while it has fewer than two plus a unit per city (the
//! strongest land unit it can build), else a building whose upkeep it can meet (its favourites
//! first, `Known::city_state_builds`), else Gold. A building whose upkeep it can no longer meet
//! is dropped from the front of the queue. City-states never asked the production advisor.

use smallvec::SmallVec;

use crate::base::ids::{BuildingId, CityId, PlayerId, TileIdx, UnitId};
use crate::base::stats::Stat;
use crate::game::cities::construction::buildable_items;
use crate::game::cities::founding::{found_check, found_city_by};
use crate::game::cities::queue::{plan_production, write_queue};
use crate::game::combat::{city as city_combat, resolve};
use crate::game::derive::rev::UnitTouch;
use crate::game::units::type_has;
use crate::game::{Game, movement, query};
use crate::rules::defs::Domain;
use crate::state::cities::{Constructible, Perpetual};
use crate::state::units::Activity;
use crate::unique::UniqueType;

/// Stage S8: a city-state's turn (`take_turn`, `city_states.py:1220-1297`).
pub fn take_turn(g: &mut Game, cs: PlayerId) {
    found_with_settlers(g, cs);
    let cities: Vec<CityId> = g.player_cities(cs).map(crate::state::cities::City::id).collect();
    for c in cities {
        bombard(g, c);
        produce(g, cs, c);
    }
    let cap = g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c)).map(|c| c.tile());
    let units: Vec<UnitId> = g.player_units(cs).map(crate::state::units::Unit::id).collect();
    let r = g.rules();
    for u in units {
        let Some(base) = g.unit(u).map(|x| x.base) else { continue };
        if !r.base_units()[base].military {
            continue;
        }
        if attack_in_reach(g, u) || g.unit(u).is_none() {
            continue;
        }
        let Some(cap) = cap else { continue };
        let Some(at) = g.unit(u).map(crate::state::units::Unit::tile) else { continue };
        if (g.military_at(cap).is_none() && at != cap) || g.grid().distance(at, cap) > 3 {
            let _gone = movement::move_toward(g, u, cap, false, false);
        } else if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            x.activity = Some(Activity::Fortify);
        }
    }
}

/// Stage S8's row for a city-state.
pub(crate) fn take_turn_stage(g: &mut Game, cs: PlayerId) {
    take_turn(g, cs);
}

/// A city fires at the first target it may (`city_states.py:1231-1236`).
fn bombard(g: &mut Game, c: CityId) {
    for t in city_combat::bombard_targets(g, c) {
        if let Ok(d) = city_combat::plan_bombard(g, c, t) {
            city_combat::city_bombard(g, c, d);
            break;
        }
    }
}

/// A unit attacks the first tile in its range it may attack (`city_states.py:1269-1283`).
fn attack_in_reach(g: &mut Game, u: UnitId) -> bool {
    let Some(at) = g.unit(u).map(crate::state::units::Unit::tile) else { return false };
    let range = u32::try_from(crate::game::units::health::attack_range(g, u)).unwrap_or(0);
    for t in g.grid().within(at, range) {
        if t == at {
            continue;
        }
        if let Ok(d) = resolve::validate_attack(g, u, t) {
            resolve::attack(g, u, d);
            return true;
        }
    }
    false
}

/// What a city builds (`city_states.py:1237-1260`).
fn produce(g: &mut Game, cs: PlayerId, c: CityId) {
    let r = g.rules();
    let gpt = query::civ_stats(g, cs).total[Stat::Gold];
    let gold = g.player(cs).map_or(0.0, |p| p.econ.gold);
    let broke = gpt < 0.0 && gold < 50.0;
    let front = g.city(c).and_then(|x| x.queue.first().copied());
    if let Some(Constructible::Building(b)) = front
        && broke
        && r.buildings()[b].maintenance != 0
    {
        write_queue(g, c, SmallVec::new());
    }
    if g.city(c).is_none_or(|x| !x.queue.is_empty()) {
        return;
    }
    let items = buildable_items(g, c);
    let army = g.player_units(cs).filter(|u| r.base_units()[u.base].military).count();
    let cities = g.player_cities(cs).count();
    let choice = if broke {
        items.gold.then_some(Constructible::Perpetual(Perpetual::Gold))
    } else if army < 2 + cities
        && let Some(u) = strongest_defender(g, &items)
    {
        Some(Constructible::Unit(u))
    } else if let Some(b) = affordable_building(g, &items, gpt) {
        Some(Constructible::Building(b))
    } else {
        items.gold.then_some(Constructible::Perpetual(Perpetual::Gold))
    };
    if let Some(item) = choice
        && let Ok(q) = plan_production(g, c, item, false)
    {
        write_queue(g, c, q);
    }
}

/// The strongest military land unit on the list, the first in the ruleset among equals.
fn strongest_defender(
    g: &Game,
    items: &crate::game::cities::construction::Buildable,
) -> Option<crate::base::ids::BaseUnitId> {
    let r = g.rules();
    let mut best: Option<(i32, crate::base::ids::BaseUnitId)> = None;
    for u in items.units.iter() {
        let d = &r.base_units()[u];
        if !d.military || d.domain != Domain::Land {
            continue;
        }
        let s = d.strength + d.ranged_strength;
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, u));
        }
    }
    best.map(|(_, u)| u)
}

/// A building on the list whose upkeep the city-state can meet: its favourite first, else the
/// first in the ruleset.
fn affordable_building(
    g: &Game,
    items: &crate::game::cities::construction::Buildable,
    gpt: f64,
) -> Option<BuildingId> {
    let r = g.rules();
    let ok = |b: BuildingId| gpt - f64::from(r.buildings()[b].maintenance) >= 0.0;
    let affordable: Vec<BuildingId> = items.buildings.iter().filter(|&b| ok(b)).collect();
    r.derived()
        .known
        .city_state_builds
        .iter()
        .flatten()
        .copied()
        .find(|b| affordable.contains(b))
        .or_else(|| affordable.first().copied())
}

/// A city-state with no city founds one with its settler, where it stands or on the first tile
/// within two where it may (`_found_with_settlers`, `city_states.py:1306-1320`).
fn found_with_settlers(g: &mut Game, cs: PlayerId) {
    let units: Vec<(UnitId, TileIdx, crate::base::ids::BaseUnitId)> =
        g.player_units(cs).map(|u| (u.id(), u.tile(), u.base)).collect();
    for (u, at, base) in units {
        if !type_has(g, base, UniqueType::FoundCity) || g.player_cities(cs).next().is_some() {
            continue;
        }
        if g.unit(u).is_none() {
            continue;
        }
        let spots: Vec<TileIdx> = g
            .grid()
            .within(at, 2)
            .into_iter()
            .filter(|&t| found_check(g, cs, t).is_none())
            .collect();
        let Some(&first) = spots.first() else { continue };
        let t = if spots.contains(&at) { at } else { first };
        let _founded = found_city_by(g, cs, t, None, Some(u));
    }
}
