//! The uniques that apply in a city (`cities.py:48-111`).
//!
//! A city's uniques are, in Python's order, its own (its buildings' that hold in their city
//! alone, and the Marble decision's resources, DESIGN.md 5.12), its majority religion's
//! follower beliefs', then its owner's: three indexes, each a memo (`CityLocal`,
//! `FollowerIndex`, `CivIndexFull` in `game::derive::civ`), which `unique::query::city` walks in
//! turn. Python rebuilt the city's list of maps on every read that missed its cache
//! (`local_umaps`, `cities.py:48-66`), and its cache was dropped at every write.

use crate::base::ids::{BuildingId, CityId, UniqueId};
use crate::game::Game;
use crate::game::core::has_type;
use crate::unique::{Ctx, UniqueType, uq};

/// The context a unique is asked about a city in (`cities.city_ctx`, `cities.py:80-82`): its
/// owner's, on its tile.
#[must_use]
pub fn city_ctx(g: &Game, c: CityId) -> Ctx {
    Ctx::city(&g.view(), c)
}

/// The uniques of type `ty` that hold in the city alone: its own, then its religion's
/// (`cities.local_uniques`, `cities.py:69-77`), with their copies.
#[must_use]
pub fn local_uniques(g: &Game, c: CityId, ty: UniqueType) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    uq::local(&v, c, ty, &Ctx::city(&v, c)).map(|h| (h.id, h.n)).collect()
}

/// Every unique of type `ty` that holds in the city: its own, its religion's, then its owner's
/// (`cities.city_uniques`, `cities.py:85-89`, UnCiv's `City.forEachMatchingUnique`), with their
/// copies.
#[must_use]
pub fn city_uniques(g: &Game, c: CityId, ty: UniqueType) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    uq::city(&v, c, ty, &Ctx::city(&v, c)).map(|h| (h.id, h.n)).collect()
}

/// Whether any unique of type `ty` holds in the city (`cities.city_has`, `cities.py:92-94`).
#[must_use]
pub fn city_has(g: &Game, c: CityId, ty: UniqueType) -> bool {
    let v = g.view();
    uq::any(uq::city(&v, c, ty, &Ctx::city(&v, c)))
}

/// Whether one of the city's own buildings carries a unique of type `ty` that holds, rather than
/// any source of the city's uniques (`cities.building_unique_in_city`, `cities.py:97-104`): a
/// Courthouse removing annexation unhappiness asks this.
#[must_use]
pub fn building_unique_in_city(g: &Game, c: CityId, ty: UniqueType) -> bool {
    let Some(city) = g.city(c) else { return false };
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let buildings = g.rules().buildings();
    city.buildings.iter().any(|b| uq::any(uq::object(&v, &buildings[b].uniques, ty, &ctx)))
}

/// Whether the city has a building or its equivalent (`cities.contains_building`,
/// `cities.py:107-111`, UnCiv's `containsBuildingOrEquivalent`): the building itself, one that
/// replaces it, or one that carries its name as a tag.
#[must_use]
pub fn contains_building(g: &Game, c: CityId, b: BuildingId) -> bool {
    let Some(city) = g.city(c) else { return false };
    if city.buildings.contains(b) {
        return true;
    }
    let r = g.rules();
    let tag = r.uniques().tag_named(&r.buildings()[b].name);
    city.buildings.iter().any(|x| {
        let d = &r.buildings()[x];
        d.replaces == Some(b)
            || tag.is_some_and(|t| d.uniques.tags.contains(t) || d.uniques.cond_tags.contains(t))
    })
}

/// Whether a building's uniques include one of type `ty`, conditionals not evaluated
/// (`UniqueMap.has_tag` on the building's map).
#[must_use]
pub fn building_has_type(g: &Game, b: BuildingId, ty: UniqueType) -> bool {
    has_type(g.rules(), &g.rules().buildings()[b].uniques, ty)
}
