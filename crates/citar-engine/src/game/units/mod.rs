//! Units (`units.py`): their uniques, how they are made and placed, promotions and experience,
//! health, upgrades, limited abilities, capture, and what happens to them as turns start and end.
//!
//! Package 1b-05 ports `units.py:21-59`: the uniques that apply to a unit, which are its
//! profile's (its base unit's, its unit type's and its promotions', one shared index per
//! combination of base unit and promotions, DESIGN.md 5.12), with or without its owner's; the
//! context they are asked in; and which units garrison a city. Python cached each profile's map
//! by its type and sorted promotions (`g._static`); here the profile table in
//! `game::derive::civ` does, and never drops one.
//!
//! Package 1c-02 ports the rest, module by module:
//! - here: making and placing units (`units.py:70-180`: `on_created`, `place_unit_near`,
//!   `add_unit_in_city`, `add_construction_bonuses`, `starting_units`), and what they are;
//! - [`promotions`]: experience and promotions (`units.py:186-282`);
//! - [`health`]: movement allowance, range, attacks, healing and damage (`units.py:288-397`);
//! - [`abilities`]: limited-use actions (`units.py:403-482`);
//! - [`upgrades`]: upgrades and their costs (`units.py:488-602`), and disbanding
//!   (`units.py:763-776`);
//! - [`capture`]: capturing a civilian (`units.py:608-640`);
//! - [`turn`]: a unit's start and end of turn (`units.py:679-751`);
//! - [`actions`]: the tools `move_unit`, `unit_order`, `upgrade_unit` and `promote_unit`
//!   (`tools.py:412-630`).

pub mod abilities;
pub mod actions;
pub mod capture;
pub mod health;
pub mod promotions;
pub mod turn;
pub mod upgrades;

use smallvec::SmallVec;

use super::Game;
use super::derive::rev::{PlayerTouch, UnitTouch};
use super::path::Mover;
use crate::base::ids::{BaseUnitId, CityId, EraId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::rules::defs::{BaseUnitDef, Domain, StartingUnit};
use crate::unique::filter::UnitScope;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The context a unique is asked about a unit in (`units.unit_ctx`, `units.py:36-38`): its
/// owner's, on its tile.
#[must_use]
pub fn unit_ctx(g: &Game, u: UnitId) -> Ctx {
    Ctx::unit(&g.view(), u)
}

/// The unit's uniques of type `ty` that hold in its context, and with `with_civ` its owner's
/// after them (`units.unit_uniques`, `units.py:41-48`), with their copies.
#[must_use]
pub fn unit_uniques(g: &Game, u: UnitId, ty: UniqueType, with_civ: bool) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if with_civ {
        uq::unit_and_civ(&v, u, ty, &ctx).map(|h| (h.id, h.n)).collect()
    } else {
        uq::unit(&v, u, ty, &ctx).map(|h| (h.id, h.n)).collect()
    }
}

/// Whether any unique of type `ty` holds for the unit, or with `with_civ` for it or its owner
/// (`units.unit_has`, `units.py:51-53`).
#[must_use]
pub fn unit_has(g: &Game, u: UnitId, ty: UniqueType, with_civ: bool) -> bool {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if with_civ {
        uq::any(uq::unit_and_civ(&v, u, ty, &ctx))
    } else {
        uq::any(uq::unit(&v, u, ty, &ctx))
    }
}

/// The sum of an amount over the unit's uniques of type `ty` that hold, each as many times as
/// its copies, with its owner's when `with_civ`.
pub(crate) fn unit_sum(
    g: &Game,
    u: UnitId,
    ty: UniqueType,
    with_civ: bool,
    f: impl FnMut(&UniqueData) -> Option<i32>,
) -> i32 {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if with_civ {
        uq::sum_i32(uq::unit_and_civ(&v, u, ty, &ctx), f)
    } else {
        uq::sum_i32(uq::unit(&v, u, ty, &ctx), f)
    }
}

/// Whether a unit counts as a city's garrison, for its strength and its happiness
/// (`units.can_garrison`, `units.py:56-59`): a military land unit.
#[must_use]
pub fn can_garrison(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| {
        let d = &g.rules.base_units()[x.base];
        d.military && d.domain == Domain::Land
    })
}

/// Takes a unit out of the game (`Game.remove_unit`, `game.py:751-764`); the units it carried
/// stay, no longer carried. Nothing for a unit the game does not have.
pub fn remove_unit(g: &mut Game, u: UnitId) {
    if g.unit(u).is_some() {
        let gone = g.despawn_unit(u);
        debug_assert!(gone.is_ok(), "a unit the game has is removed: {gone:?}");
    }
}

/// Whether a unit is a military unit (`units.is_military`).
#[must_use]
pub fn is_military(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| g.rules.base_units()[x.base].military)
}

/// Whether a base unit, with its type, carries a unique of type `ty`, whatever its conditionals:
/// Python's `ud["_umap"].has_tag` and `.get` (`rules.py:116-118`).
#[must_use]
pub fn type_has(g: &Game, base: BaseUnitId, ty: UniqueType) -> bool {
    let r = g.rules();
    let d = &r.base_units()[base];
    super::core::has_type(r, &d.uniques, ty)
        || super::core::has_type(r, &r.unit_types()[d.unit_type].uniques, ty)
}

/// The uniques of a base unit and its type, in Python's order: the unit's own, then its type's.
pub(crate) fn type_uniques(g: &Game, base: BaseUnitId) -> impl Iterator<Item = UniqueId> + '_ {
    let r = g.rules();
    let d: &BaseUnitDef = &r.base_units()[base];
    d.uniques.ids().chain(r.unit_types()[d.unit_type].uniques.ids())
}

/// Whether unit `u` passes a unit filter, asked with nothing in context (`unit_matches(g, u,
/// f)`).
#[must_use]
pub fn unit_matches(g: &Game, u: UnitId, f: crate::base::ids::UnitFilterId) -> bool {
    g.rules().uniques().filters().unit_matches(f, &g.view(), u, UnitScope::default())
}

/// The unit a civilization builds in place of `base`: its nation's unique unit, if it has one
/// (`cities.equivalent_unit`, `cities.py:1132-1135`).
#[must_use]
pub fn equivalent_unit(g: &Game, p: PlayerId, base: BaseUnitId) -> BaseUnitId {
    let Some(nation) = g.player(p).map(|x| x.nation) else { return base };
    g.rules()
        .derived()
        .nation_uniques
        .get(nation)
        .and_then(|n| n.units.iter().find(|&&(replaced, _)| replaced == base))
        .map_or(base, |&(_, own)| own)
}

// ---- Making and placing units (units.py:70-180) --------------------------------------------------

/// What a new unit gets (`units.on_created`, `units.py:70-83`): its original owner, its base
/// unit's promotions, and those its civilization grants units like it (the barbarians' from
/// their nation). The unit is also recorded among the units its civilization has gained, which
/// `Land units may cross [terrain] tiles after the first [unit] is earned` reads: Python read a
/// flag it never wrote.
pub(crate) fn on_created(g: &mut Game, u: UnitId) {
    let Some((owner, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return };
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE)
        && x.original_owner.is_none()
    {
        x.original_owner = Some(owner);
    }
    // refcheck: units-gained-recorded
    // The unit placed moved its owner's roster, which the movement memos read for this set.
    if let Some(p) = g.player_mut(owner, PlayerTouch::OTHER) {
        p.civ.units_gained.insert(base);
    }
    let r = g.rules();
    for &pr in &r.base_units()[base].promotions {
        promotions::add_promotion(g, u, pr, true);
    }
    let t = r.uniques();
    let grants: SmallVec<[_; 4]> = if g.is_barbarian(owner) {
        // The barbarians' nation, whatever its conditionals (`units.py:80-83`).
        let nation = g.player(owner).map(|x| x.nation);
        nation
            .into_iter()
            .flat_map(|n| r.nations()[n].uniques.ids())
            .filter_map(|id| match t.get(id).data {
                UniqueData::UnitsGainPromotion(x) => Some((x.units, x.promotion)),
                _ => None,
            })
            .collect()
    } else {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        uq::civ(&v, owner, UniqueType::UnitsGainPromotion, &ctx)
            .filter_map(|h| match *h.data() {
                UniqueData::UnitsGainPromotion(x) => Some((x.units, x.promotion)),
                _ => None,
            })
            .collect()
    };
    for (units, promotion) in grants {
        if unit_matches(g, u, units) {
            promotions::add_promotion(g, u, promotion, true);
        }
    }
}

/// Where a new unit of `base` for `p` could appear on or near `t` (`units.place_unit_near`,
/// `units.py:86-110`): the tile itself if it may stand there, else the nearest ring it can walk
/// to, land before water for a land unit, at most `max_tries` rings out. `ignore` is a unit
/// taken as gone, as an upgrade takes the unit it replaces.
#[must_use]
pub fn spawn_spot(
    g: &Game,
    p: PlayerId,
    base: BaseUnitId,
    t: TileIdx,
    max_tries: u32,
    ignore: Option<UnitId>,
) -> Option<TileIdx> {
    let mut m = Mover::of_type(g, p, base)?;
    m.ignore = ignore;
    if m.can_stand(t) {
        return Some(t);
    }
    let grid = g.grid();
    let mut checked = vec![t];
    let mut frontier: Vec<TileIdx> =
        grid.neighbors(t).filter(|&n| m.pass_reason(n).is_none()).collect();
    let restricted = m.domain() == Domain::Land;
    for _ in 0..max_tries {
        let primary = frontier.iter().filter(|&&x| !restricted || g.is_land(x));
        let secondary = frontier.iter().filter(|&&x| restricted && !g.is_land(x));
        if let Some(&x) = primary.chain(secondary).find(|&&x| m.can_stand(x)) {
            return Some(x);
        }
        checked.extend_from_slice(&frontier);
        let mut next: Vec<TileIdx> = Vec::new();
        for &x in &frontier {
            for n in grid.neighbors(x) {
                if !checked.contains(&n) && !next.contains(&n) && m.pass_reason(n).is_none() {
                    next.push(n);
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    None
}

/// Makes a unit of `base` for `p` on or near `t` (`units.place_unit_near`), if there is room.
pub fn place_unit_near(g: &mut Game, p: PlayerId, base: BaseUnitId, t: TileIdx) -> Option<UnitId> {
    let spot = spawn_spot(g, p, base, t, 10, None)?;
    g.create_unit(p, base, spot, 0).ok()
}

/// A new unit from a city (`units.add_unit_in_city`, `units.py:113-136`): a naval unit goes to
/// the owner's first coastal city if this one is not; placed on or near the city; the city
/// recorded as its origin; and `upon gaining a [unit]` fires.
pub fn add_unit_in_city(g: &mut Game, c: CityId, base: BaseUnitId) -> Option<UnitId> {
    let (owner, tile) = g.city(c).map(|x| (x.owner(), x.tile()))?;
    let mut target = (c, tile);
    let naval = g.rules().base_units().get(base)?.domain == Domain::Water;
    if naval && !(g.is_water(tile) || super::path::node::next_to_coast(g, tile)) {
        target = g
            .player_cities(owner)
            .find(|x| super::path::node::next_to_coast(g, x.tile()))
            .map(|x| (x.id(), x.tile()))?;
    }
    let u = place_unit_near(g, owner, base, target.1)?;
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.origin_city = Some(target.0);
    }
    // The religion a religious unit carries: its city's majority, or its founder's own
    // (`units.py:127-133`).
    super::religion::on_unit_made(g, u, target.0);
    let site = TriggerSite { civ: owner, city: None, unit: Some(u), tile: None };
    super::triggers::fire(g, &site, &TriggerEvent::GainingUnit(base), true, None);
    Some(u)
}

/// The experience and promotions a city gives the units it trains (`units.add_construction_bonuses`,
/// `units.py:139-155`).
pub fn add_construction_bonuses(g: &mut Game, u: UnitId, c: CityId) {
    let Some(base) = g.unit(u).map(|x| x.base) else { return };
    let r = g.rules();
    let t = r.uniques();
    let f = t.filters();
    let (xp, promos) = {
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        let xp =
            uq::sum_i32(uq::city(&v, c, UniqueType::UnitStartingExperience, &ctx), |d| match d {
                UniqueData::UnitStartingExperience(x)
                    if t.in_set(x.units, base) && f.city_matches(x.cities, &v, c, None) =>
                {
                    Some(x.xp)
                }
                _ => None,
            });
        let promos: SmallVec<[_; 2]> = uq::city(&v, c, UniqueType::UnitStartingPromotions, &ctx)
            .filter_map(|h| match *h.data() {
                UniqueData::UnitStartingPromotions(x)
                    if f.city_matches(x.cities, &v, c, None) && t.in_set(x.units, base) =>
                {
                    Some(x.promotion)
                }
                _ => None,
            })
            .collect();
        (xp, promos)
    };
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.xp = xp;
    }
    for pr in promos {
        promotions::add_promotion(g, u, pr, true);
    }
}

/// The units a civilization starts with in the era the game starts in (`units.starting_units`,
/// `units.py:158-180`): its settlers, workers and military units, and its difficulty's bonus
/// units; a city-state starts with one settler. Each is its nation's own unit where it has one;
/// a name the ruleset lacks is skipped.
#[must_use]
pub fn starting_units(g: &Game, p: PlayerId, era: EraId) -> Vec<BaseUnitId> {
    let r = g.rules();
    let Some(e) = r.eras().get(era) else { return Vec::new() };
    let Some(pl) = g.player(p) else { return Vec::new() };
    let known = &r.derived().known;
    let settler = known.settler;
    let count = |n: i32| usize::try_from(n).unwrap_or(0);
    let mut out: Vec<Option<StartingUnit>> = Vec::new();
    out.extend(core::iter::repeat_n(
        settler.map(StartingUnit::Unit),
        count(e.starting_settler_count),
    ));
    out.extend(core::iter::repeat_n(
        known.worker.map(StartingUnit::Unit),
        count(e.starting_worker_count),
    ));
    out.extend(core::iter::repeat_n(
        Some(StartingUnit::EraStartingUnit),
        count(e.starting_military_unit_count),
    ));
    let d = &r.difficulties()[g.seat_difficulty(Some(p))];
    let bonus = if pl.is_major() {
        if g.is_humanlike(p) {
            &d.player_bonus_starting_units
        } else {
            &d.ai_major_civ_bonus_starting_units
        }
    } else {
        &d.ai_city_state_bonus_starting_units
    };
    out.extend(bonus.iter().copied().map(Some));
    if pl.is_city_state() {
        out = vec![settler.map(StartingUnit::Unit)];
    }
    out.into_iter()
        .flatten()
        .map(|s| match s {
            StartingUnit::Unit(b) => b,
            StartingUnit::EraStartingUnit => e.starting_military_unit,
        })
        .map(|b| equivalent_unit(g, p, b))
        .collect()
}

/// A tile near `near` where a new unit of `base` for `p` could appear (`Game.find_spawn_tile`,
/// `game.py:786-793`): the first within three tiles it could stand on, ring by ring, the tiles
/// of a ring in the grid's ring order.
#[must_use]
pub fn find_spawn_tile(g: &Game, near: TileIdx, base: BaseUnitId, p: PlayerId) -> Option<TileIdx> {
    let m = Mover::of_type(g, p, base)?;
    g.grid().within(near, 3).into_iter().find(|&t| m.can_stand(t))
}
