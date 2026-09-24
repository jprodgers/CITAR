//! Whether a unit may be on a tile, pass through it, or end its move there (`movement.py:122-271`
//! and `passable_for_path`, `movement.py:383-417`), with Python's reasons.
//!
//! Each check reads the tile, its city and its units as they are; what does not change while a
//! search runs (the unit's profile, its civilization's rules, whom it is at war with, whose land it
//! may enter) is the [`Mover`]'s.

use super::class::Mover;
use crate::base::ids::{BaseUnitId, CityId, PlayerId, TileIdx, UnitId};
use crate::game::Game;
use crate::rules::defs::Domain;
use crate::state::map::Tile;
use crate::state::units::Unit;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// Why a unit may not be somewhere, or take a step: each of Python's refusals, rendered by
/// [`Blocked::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Blocked {
    /// The tile's governing terrain is impassable.
    Impassable(TileIdx),
    NavalOnLand,
    NoEmbarkation,
    CannotEmbark,
    EmbarkedOcean,
    /// A unit of this type may not enter the ocean yet.
    NoOcean(BaseUnitId),
    /// The owner's land, with no right to enter it.
    Territory(PlayerId),
    EnemyCity,
    EnemyUnit,
    NoAirRoom,
    AirNoBase,
    ForeignUnit,
    CivilianThere,
    MilitaryThere,
    NoMoves,
    NotAdjacent,
    ForeignCity,
}

impl Blocked {
    /// What Python said.
    #[must_use]
    pub fn text(self, g: &Game) -> String {
        let r = g.rules();
        match self {
            Self::Impassable(t) => {
                let name = g.tile(t).map_or("", |x| &*r.terrains()[governing(g, x)].name);
                format!("{name} is impassable.")
            }
            Self::NavalOnLand => "Naval units cannot move onto land.".into(),
            Self::NoEmbarkation => {
                "Your civilization cannot embark land units yet (requires Optics).".into()
            }
            Self::CannotEmbark => "This unit cannot embark.".into(),
            Self::EmbarkedOcean => {
                "Embarked units cannot enter ocean yet (requires Astronomy).".into()
            }
            Self::NoOcean(b) => format!("{} cannot enter ocean tiles yet.", r.base_units()[b].name),
            Self::Territory(p) => format!(
                "Cannot enter {}'s territory without open borders (or war).",
                g.player(p).map_or("", |x| &*x.name)
            ),
            Self::EnemyCity => "Enemy city: attack it instead.".into(),
            Self::EnemyUnit => "Enemy unit in the way: attack it instead.".into(),
            Self::NoAirRoom => "That city has no room for more aircraft.".into(),
            Self::AirNoBase => {
                "Aircraft can only be based in your own cities or on carriers.".into()
            }
            Self::ForeignUnit => "Tile is occupied by a foreign unit.".into(),
            Self::CivilianThere => "Another civilian unit is already there.".into(),
            Self::MilitaryThere => "Another military unit is already there.".into(),
            Self::NoMoves => "no moves left".into(),
            Self::NotAdjacent => "not adjacent".into(),
            Self::ForeignCity => "Foreign city: you cannot enter it.".into(),
        }
    }
}

/// The terrain that governs a tile (`tiles.last_terrain`, `tiles.py:45-52`): its top feature,
/// else its natural wonder, else its base.
#[must_use]
pub fn governing(g: &Game, t: &Tile) -> crate::base::ids::TerrainId {
    let r = g.rules();
    match t.features().top() {
        Some(f) => r.derived().features.get(f).copied().unwrap_or(t.terrain()),
        None => t.wonder().unwrap_or(t.terrain()),
    }
}

/// Whether nothing may enter the tile (`tiles.is_impassable`, `tiles.py:77-79`).
#[must_use]
pub fn impassable(g: &Game, t: TileIdx) -> bool {
    g.tile(t).is_some_and(|x| g.rules().terrains()[governing(g, x)].impassable)
}

/// Whether a neighbour of the tile is coast (`tiles.adjacent_to_coast`, `tiles.py:127-134`).
#[must_use]
pub fn next_to_coast(g: &Game, t: TileIdx) -> bool {
    let Some(coast) = g.rules().derived().known.map.coast else { return false };
    g.grid().neighbors(t).any(|n| g.tile(n).is_some_and(|x| x.terrain() == coast))
}

/// The unit that meets a newcomer on a tile (`movement._first_unit`): its military unit, else its
/// civilian, else its first aircraft.
#[must_use]
pub fn first_unit(g: &Game, t: TileIdx) -> Option<&Unit> {
    g.military_at(t).or_else(|| g.civilian_at(t)).or_else(|| g.air_units_at(t).next())
}

impl Mover<'_> {
    /// Whether the tile's owner lets it in: its own land, land it may enter, or land its
    /// profile opens (`movement.py:178-181, 403-405`).
    pub(crate) fn may_enter(&self, owner: PlayerId) -> bool {
        owner == self.pid
            || self.enter.contains(owner)
            || self.prof.foreign_ok
            || (self.prof.cs_ok && self.city_states.contains(owner))
    }

    /// Why its type cannot pass through the tile's terrain, if it cannot
    /// (`movement.terrain_reason`, `movement.py:122-158`).
    #[must_use]
    pub fn terrain_reason(&self, t: TileIdx) -> Option<Blocked> {
        let tile = self.g.tile(t)?;
        self.terrain_reason_on(t, tile, self.g.state().city_at(t).is_some())
    }

    /// [`terrain_reason`](Self::terrain_reason) of tile `t`, given what is on it: whether a
    /// city stands there.
    pub(crate) fn terrain_reason_on(&self, t: TileIdx, tile: &Tile, city: bool) -> Option<Blocked> {
        let g = self.g;
        if self.is_air() {
            return None;
        }
        let terrains = g.rules().terrains();
        let land_unit = self.def.domain == Domain::Land;
        if !city && terrains[governing(g, tile)].impassable {
            let ice = self.rules.ice.is_some_and(|i| tile.features().contains(i));
            let ok = self.prof.impassable_ok
                || (self.prof.ice_ok && ice)
                || (land_unit
                    && Some(tile.terrain()) == self.rules.mountain
                    && self.civ.cross_mountains);
            if !ok {
                return Some(Blocked::Impassable(t));
            }
        }
        let water = terrains[tile.terrain()].kind == crate::rules::defs::TerrainType::Water;
        if !water && self.def.domain == Domain::Water && !city {
            return Some(Blocked::NavalOnLand);
        }
        let ocean = Some(tile.terrain()) == self.rules.ocean;
        let spec_ok = self.ocean_unit_ok;
        if water && land_unit && !self.prof.on_water && !city {
            if !self.civ.can_embark {
                return Some(Blocked::NoEmbarkation);
            }
            if self.prof.cannot_embark {
                return Some(Blocked::CannotEmbark);
            }
            if ocean && !self.civ.ocean_embarked && !spec_ok {
                return Some(Blocked::EmbarkedOcean);
            }
        }
        if ocean && !self.civ.ocean_all && !spec_ok {
            let barred = match self.unit {
                Some(_) => self.prof.no_ocean,
                None => {
                    let v = g.view();
                    let ctx = Ctx::tile(Some(self.pid), t);
                    self.no_ocean_uniques.iter().any(|&id| crate::unique::applies(id, &ctx, &v))
                }
            };
            if barred {
                return Some(Blocked::NoOcean(self.base));
            }
        }
        None
    }

    /// Why it cannot move through the tile, if it cannot (`movement.pass_reason`,
    /// `movement.py:166-193`): its terrain, its owner's borders, an enemy city, an enemy unit
    /// (unless a civilian it would capture).
    #[must_use]
    pub fn pass_reason(&self, t: TileIdx) -> Option<Blocked> {
        if let Some(r) = self.terrain_reason(t) {
            return Some(r);
        }
        let g = self.g;
        let owner = g.tile(t).and_then(Tile::owner);
        if let Some(o) = owner.filter(|&o| o != self.pid) {
            if !self.may_enter(o) {
                return Some(Blocked::Territory(o));
            }
            if g.state().city_at(t).is_some() && self.at_war(o) {
                return Some(Blocked::EnemyCity);
            }
        }
        let first = first_unit(g, t)?;
        if first.owner() != self.pid {
            let fd = &g.rules().base_units()[first.base];
            let water_embarked =
                self.def.domain == Domain::Land && g.is_water(t) && !self.prof.on_water;
            let war = self.at_war(first.owner());
            if !water_embarked && !fd.military && war && self.def.military {
                return None;
            }
            if war {
                return Some(Blocked::EnemyUnit);
            }
        }
        None
    }

    /// Whether it could end its move on the tile, which is stricter than passing through
    /// (`movement.can_stand`, `movement.py:260-271`).
    #[must_use]
    pub fn can_stand(&self, t: TileIdx) -> bool {
        let g = self.g;
        if self.is_air() {
            return stack_reason(g, self.pid, self.base, t, None).is_none();
        }
        if self.pass_reason(t).is_some() {
            return false;
        }
        let city = g.city_at(t);
        if city.is_some_and(|c| c.owner() != self.pid) {
            return false;
        }
        if self.def.domain == Domain::Water
            && city.is_some()
            && !(g.is_water(t) || next_to_coast(g, t))
        {
            return false;
        }
        stack_reason(g, self.pid, self.base, t, self.ignore).is_none()
    }

    /// Whether a search may route through the tile (`movement.passable_for_path`,
    /// `movement.py:388-417`): optimistic about tiles it has not explored, which are passable
    /// at their true cost, since that is how exploring happens; foreign units block only where
    /// it sees them, but for a civilian it would capture on the target.
    #[must_use]
    pub fn passable(&self, t: TileIdx, target: Option<TileIdx>) -> bool {
        let g = self.g;
        let Some(tile) = g.tile(t) else { return false };
        self.passable_on(t, tile, g.city_at(t).map(crate::state::cities::City::owner), target)
    }

    /// [`passable`](Self::passable) of tile `t`, given what is on it: the owner of the city
    /// standing there, if one does.
    pub(crate) fn passable_on(
        &self,
        t: TileIdx,
        tile: &Tile,
        city: Option<PlayerId>,
        target: Option<TileIdx>,
    ) -> bool {
        let g = self.g;
        let explored = self.barbarian || self.explored.is_some_and(|e| e.contains(t.0));
        if !explored {
            return true;
        }
        if self.terrain_reason_on(t, tile, city.is_some()).is_some() {
            return false;
        }
        if let Some(o) = tile.owner()
            && o != self.pid
            && !self.may_enter(o)
        {
            return false;
        }
        if city.is_some_and(|c| c != self.pid) {
            return false;
        }
        let seen = self.barbarian || self.visible.is_some_and(|v| v.contains(t.0));
        if seen {
            let r = g.rules();
            for other in g.units_at(t) {
                let od = &r.base_units()[other.base];
                if other.owner() == self.pid || od.domain == Domain::Air {
                    continue;
                }
                let capture = target == Some(t)
                    && !od.military
                    && self.def.military
                    && self.at_war(other.owner())
                    && g.military_at(t).is_none();
                if !capture {
                    return false;
                }
            }
        }
        true
    }

    /// Whether another of its owner's units stands on the tile, which a search checks before
    /// the stacking rules.
    pub(crate) fn own_unit_at(&self, t: TileIdx) -> bool {
        self.g.units_at(t).any(|o| o.owner() == self.pid && Some(o.id()) != self.ignore)
    }
}

/// Why a unit of `base` cannot end its move on the tile because of the units there, if it
/// cannot (`movement.stack_reason`, `movement.py:208-236`), leaving `ignore` out: an aircraft
/// needs its own city with room or a carrier with room; anything else may not share the tile
/// with a foreign unit (but an enemy civilian it would capture) or with one of its own of its
/// kind, military or civilian.
#[must_use]
pub fn stack_reason(
    g: &Game,
    p: PlayerId,
    base: BaseUnitId,
    t: TileIdx,
    ignore: Option<UnitId>,
) -> Option<Blocked> {
    let r = g.rules();
    let def = r.base_units().get(base)?;
    if def.domain == Domain::Air {
        if let Some(c) = g.city_at(t).filter(|c| c.owner() == p) {
            return (!air_capacity_ok(g, c.id(), ignore)).then_some(Blocked::NoAirRoom);
        }
        let carried = g.units_at(t).any(|o| o.owner() == p && can_carry(g, o.id(), base, ignore));
        return (!carried).then_some(Blocked::AirNoBase);
    }
    for other in g.units_at(t) {
        if Some(other.id()) == ignore {
            continue;
        }
        let od = &r.base_units()[other.base];
        if od.domain == Domain::Air {
            continue;
        }
        if other.owner() != p {
            if !od.military && def.military && g.at_war(p, other.owner()) {
                continue;
            }
            return Some(Blocked::ForeignUnit);
        }
        if od.military != def.military {
            continue;
        }
        return Some(if def.military { Blocked::MilitaryThere } else { Blocked::CivilianThere });
    }
    None
}

/// Whether a city has room for another aircraft (`units.air_capacity_ok`, `units.py:754-760`):
/// the base capacity and the city's `Can carry [n] extra [Air] units`, against the aircraft
/// based there and not carried, leaving `ignore` out.
#[must_use]
pub fn air_capacity_ok(g: &Game, c: CityId, ignore: Option<UnitId>) -> bool {
    let Some(city) = g.city(c) else { return false };
    let rules = g.derived().move_rules();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let extra = uq::sum_i32(uq::city(&v, c, UniqueType::CarryExtraAirUnits, &ctx), |d| match d {
        UniqueData::CarryExtraAirUnits(x) if rules.is_air_filter(x.units) => Some(x.count),
        _ => None,
    });
    let cap = g.rules().constants().formulas.city_air_unit_capacity.saturating_add(extra);
    let here = g
        .air_units_at(city.tile())
        .filter(|a| a.carried_by().is_none() && Some(a.id()) != ignore)
        .count();
    i32::try_from(here).unwrap_or(i32::MAX) < cap
}

/// Whether a carrier has room for an aircraft of `base` (`movement._can_carry`,
/// `movement.py:239-257`): its `Can carry` uniques for that type, unless the type cannot be
/// carried by it, against the units it carries, leaving `ignore` out.
#[must_use]
pub fn can_carry(g: &Game, carrier: UnitId, base: BaseUnitId, ignore: Option<UnitId>) -> bool {
    let r = g.rules();
    let Some(c) = g.unit(carrier) else { return false };
    let f = r.uniques().filters();
    let v = g.view();
    let ctx = Ctx::unit(&v, carrier);
    let fits = |x| f.base_unit_matches(x, base);
    let cap = uq::sum_i32(uq::unit(&v, carrier, UniqueType::CarryAirUnits, &ctx), |d| match d {
        UniqueData::CarryAirUnits(x) if fits(x.units) => Some(x.count),
        _ => None,
    })
    .saturating_add(uq::sum_i32(
        uq::unit(&v, carrier, UniqueType::CarryExtraAirUnits, &ctx),
        |d| match d {
            UniqueData::CarryExtraAirUnits(x) if fits(x.units) => Some(x.count),
            _ => None,
        },
    ));
    if cap <= 0 {
        return false;
    }
    let def = &r.base_units()[base];
    let t = r.uniques();
    let unit_type = &r.unit_types()[def.unit_type];
    let refused = def.uniques.ids().chain(unit_type.uniques.ids()).any(|id| {
        matches!(t.get(id).data, UniqueData::CannotBeCarriedBy(x) if f.base_unit_matches(x.units, c.base))
    });
    if refused {
        return false;
    }
    let carried = g.state().units().carried_by(carrier).filter(|&o| Some(o) != ignore).count();
    i32::try_from(carried).unwrap_or(i32::MAX) < cap
}
