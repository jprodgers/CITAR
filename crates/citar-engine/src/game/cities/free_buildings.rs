//! Free buildings (`cities.py:1877-1961`, UnCiv's `CivConstructions`): the buildings a
//! civilization's uniques owe its cities, handed out whenever it may be owed one more: a policy
//! adopted, a city founded or conquered, a building built.
//!
//! - `Provides the cheapest [stat] building in your first [n] cities for free`
//!   ([`add_free_stat_buildings`]);
//! - `Provides a [building] in your first [n] cities for free` ([`add_free_specific_buildings`]);
//! - `Gain a free [building] [cities]` in each city that passes its filter.
//!
//! A free building costs no maintenance (`City::free_buildings`). The uniques that grant one upon
//! an event (`<upon ...>`) are package 1b-08's to fire; the index holds them at their trigger, so
//! they are not read here, as Python skipped them.

use smallvec::SmallVec;

use super::super::Game;
use super::super::derive::rev::{CityTouch, PlayerTouch};
use super::construction::{complete_construction, is_buildable};
use super::founding::equivalent_building;
use super::uniques::contains_building;
use crate::base::ids::{BuildingId, CityId, PlayerId};
use crate::base::stats::Stat;
use crate::state::cities::Constructible;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// Records a building as free in a city, so that it costs no maintenance (`cities._add_free`,
/// `cities.py:1880-1884`).
pub(crate) fn add_free(g: &mut Game, c: CityId, b: BuildingId) {
    if g.city(c).is_some_and(|x| !x.free_buildings.contains(b))
        && let Some(x) = g.city_mut(c, CityTouch::CORE)
    {
        x.free_buildings.insert(b);
    }
}

/// The cheapest building of a city that yields a stat (`cities.cheapest_stat_building`,
/// `cities.py:1887-1902`): not a wonder, one the city could build or has queued; the first of
/// the ruleset's order among equals.
#[must_use]
pub fn cheapest_stat_building(g: &Game, c: CityId, stat: Stat) -> Option<BuildingId> {
    let city = g.city(c)?;
    let mut best: Option<(BuildingId, i32)> = None;
    for (b, d) in g.rules().buildings().iter() {
        if d.any_wonder || !d.stat_related.contains(stat) {
            continue;
        }
        let item = Constructible::Building(b);
        if !city.queue.contains(&item) && !is_buildable(g, c, item) {
            continue;
        }
        if best.is_none_or(|(_, cost)| d.cost < cost) {
            best = Some((b, d.cost));
        }
    }
    best.map(|(b, _)| b)
}

/// Grants the cheapest building yielding `stat` in the civilization's first `amount` cities,
/// once each (`cities.add_free_stat_buildings`, `cities.py:1905-1916`).
pub fn add_free_stat_buildings(g: &mut Game, p: PlayerId, stat: Stat, amount: i32) {
    let n = usize::try_from(amount).unwrap_or(0);
    let cities: Vec<CityId> = g.state().cities().of(p).iter().copied().take(n).collect();
    for c in cities {
        let granted = g.player(p).is_some_and(|x| x.civ.free_stat_buildings.contains(&(stat, c)));
        if granted || g.city(c).is_none() {
            continue;
        }
        let Some(b) = cheapest_stat_building(g, c, stat) else { continue };
        if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
            let list = &mut x.civ.free_stat_buildings;
            if let Err(i) = list.binary_search(&(stat, c)) {
                list.insert(i, (stat, c));
            }
        }
        add_free(g, c, b);
        complete_construction(g, c, Constructible::Building(b), None);
    }
}

/// Grants a building (the civilization's own version of it) in its first `amount` cities that do
/// not have it, once each (`cities.add_free_specific_buildings`, `cities.py:1919-1928`).
pub fn add_free_specific_buildings(g: &mut Game, p: PlayerId, building: BuildingId, amount: i32) {
    let b = equivalent_building(g, p, building);
    let n = usize::try_from(amount).unwrap_or(0);
    let cities: Vec<CityId> = g.state().cities().of(p).iter().copied().take(n).collect();
    for c in cities {
        let granted = g.player(p).is_some_and(|x| x.civ.free_specific_buildings.contains(&(b, c)));
        if granted || g.city(c).is_none() || contains_building(g, c, building) {
            continue;
        }
        if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
            let list = &mut x.civ.free_specific_buildings;
            if let Err(i) = list.binary_search(&(b, c)) {
                list.insert(i, (b, c));
            }
        }
        add_free(g, c, b);
        complete_construction(g, c, Constructible::Building(b), None);
    }
}

/// Hands out every free building the civilization is owed and has not received
/// (`cities.try_add_free_buildings`, `cities.py:1931-1961`): the stat and specific grants summed
/// by what they grant, in the order they first appear, then `Gain a free [building] [cities]` in
/// each of its cities, its own uniques first, then the city's.
pub fn try_add_free_buildings(g: &mut Game, p: PlayerId) {
    let (stats, specific) = {
        let v = g.view();
        let ctx = Ctx::civ(p);
        let mut stats: SmallVec<[(Stat, i32); 2]> = SmallVec::new();
        for h in uq::civ(&v, p, UniqueType::FreeStatBuildings, &ctx) {
            if let UniqueData::FreeStatBuildings(x) = *h.data() {
                let n = x.cities.saturating_mul(i32::from(h.n));
                match stats.iter_mut().find(|(s, _)| *s == x.stat) {
                    Some((_, m)) => *m += n,
                    None => stats.push((x.stat, n)),
                }
            }
        }
        let mut specific: SmallVec<[(BuildingId, i32); 2]> = SmallVec::new();
        for h in uq::civ(&v, p, UniqueType::FreeSpecificBuildings, &ctx) {
            if let UniqueData::FreeSpecificBuildings(x) = *h.data() {
                let n = x.cities.saturating_mul(i32::from(h.n));
                match specific.iter_mut().find(|(b, _)| *b == x.building) {
                    Some((_, m)) => *m += n,
                    None => specific.push((x.building, n)),
                }
            }
        }
        (stats, specific)
    };
    for (stat, n) in stats {
        add_free_stat_buildings(g, p, stat, n);
    }
    for (b, n) in specific {
        add_free_specific_buildings(g, p, b, n);
    }
    let cities: Vec<CityId> = g.state().cities().of(p).to_vec();
    for c in cities {
        if g.city(c).is_none() {
            continue;
        }
        let grants: SmallVec<[BuildingId; 2]> = {
            let t = g.rules().uniques();
            let v = g.view();
            let ctx = Ctx::city(&v, c);
            let mut out: SmallVec<[BuildingId; 2]> = SmallVec::new();
            let hits = uq::civ(&v, p, UniqueType::GainFreeBuildings, &ctx).chain(uq::local(
                &v,
                c,
                UniqueType::GainFreeBuildings,
                &ctx,
            ));
            for h in hits {
                if let UniqueData::GainFreeBuildings(x) = *h.data()
                    && t.filters().city_matches(x.cities, &v, c, None)
                {
                    out.push(x.building);
                }
            }
            out
        };
        for b in grants {
            let b = equivalent_building(g, p, b);
            add_free(g, c, b);
            if !contains_building(g, c, b) {
                complete_construction(g, c, Constructible::Building(b), None);
            }
        }
    }
}
