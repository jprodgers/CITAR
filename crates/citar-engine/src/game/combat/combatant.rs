//! One side of a fight (`combat.Combatant`, `combat.py:29-130`): a unit or a city, never both.
//!
//! Python wrapped the unit or the city in an object; here the side is the evaluator's own
//! [`Combatant`], and these functions read what the rules ask of it from the game.

use crate::base::ids::{PlayerId, TileIdx};
use crate::game::Game;
use crate::rules::defs::{BaseUnitDef, Domain};
use crate::unique::filter::Combatant;

/// The base unit behind a unit side, or `None` for a city (`Combatant.ud`).
fn def(g: &Game, c: Combatant) -> Option<&'static BaseUnitDef> {
    match c {
        Combatant::Unit(u) => g.unit(u).and_then(|x| g.rules().base_units().get(x.base)),
        Combatant::City(_) => None,
    }
}

/// Whose side it is. A side that is gone answers the first player.
#[must_use]
pub fn owner(g: &Game, c: Combatant) -> PlayerId {
    match c {
        Combatant::Unit(u) => g.unit(u).map_or(PlayerId(0), crate::state::units::Unit::owner),
        Combatant::City(x) => g.city(x).map_or(PlayerId(0), crate::state::cities::City::owner),
    }
}

/// The tile it occupies (`Combatant.idx`).
#[must_use]
pub fn tile(g: &Game, c: Combatant) -> TileIdx {
    match c {
        Combatant::Unit(u) => g.unit(u).map_or(TileIdx(0), crate::state::units::Unit::tile),
        Combatant::City(x) => g.city(x).map_or(TileIdx(0), crate::state::cities::City::tile),
    }
}

/// Whether it attacks from a distance: a city always does (`Combatant.is_ranged`).
#[must_use]
pub fn is_ranged(g: &Game, c: Combatant) -> bool {
    matches!(c, Combatant::City(_)) || def(g, c).is_some_and(|d| d.ranged)
}

/// Whether it must be next to its target, and so moves in when it wins (`Combatant.is_melee`).
#[must_use]
pub fn is_melee(g: &Game, c: Combatant) -> bool {
    def(g, c).is_some_and(|d| d.melee)
}

/// Whether it is an aircraft (`Combatant.is_air`).
#[must_use]
pub fn is_air(g: &Game, c: Combatant) -> bool {
    def(g, c).is_some_and(|d| d.domain == Domain::Air)
}

/// Whether it is a civilian, which is captured rather than killed (`Combatant.is_civilian`).
#[must_use]
pub fn is_civilian(g: &Game, c: Combatant) -> bool {
    def(g, c).is_some_and(|d| !d.military)
}

/// Whether it is a land unit (`Combatant.is_land`).
#[must_use]
pub fn is_land(g: &Game, c: Combatant) -> bool {
    def(g, c).is_some_and(|d| d.domain == Domain::Land)
}

/// Its hit points: a unit's health, or a city's (`Combatant.hp`).
#[must_use]
pub fn hp(g: &Game, c: Combatant) -> i32 {
    match c {
        Combatant::Unit(u) => g.unit(u).map_or(0, |x| i32::from(x.hp)),
        Combatant::City(x) => g.city(x).map_or(0, |c| c.health),
    }
}

/// Whether it has lost (`Combatant.defeated`): a unit at no health or gone, a city at 1 health,
/// since a city is never destroyed by damage but taken by a melee unit moving in.
#[must_use]
pub fn defeated(g: &Game, c: Combatant) -> bool {
    match c {
        Combatant::Unit(u) => g.unit(u).is_none_or(|x| x.hp <= 0),
        Combatant::City(x) => g.city(x).is_none_or(|c| c.health <= 1),
    }
}

/// What a notification calls it: a city's name, or a unit's type (`Combatant.name`).
#[must_use]
pub fn name(g: &Game, c: Combatant) -> String {
    match c {
        Combatant::Unit(u) => {
            g.unit(u).and_then(|x| g.rules().name(x.base)).unwrap_or_default().to_owned()
        }
        Combatant::City(x) => g.city(x).map(|c| c.name.to_string()).unwrap_or_default(),
    }
}

/// Whatever would defend a tile (`combat.combatant_at`, `combat.py:115-130`): a city first, then
/// a military unit, then a civilian. A city with a garrison is defended by the city.
#[must_use]
pub fn combatant_at(g: &Game, t: TileIdx) -> Option<Combatant> {
    if let Some(c) = g.city_at(t) {
        return Some(Combatant::City(c.id()));
    }
    if let Some(m) = g.military_at(t) {
        return Some(Combatant::Unit(m.id()));
    }
    g.civilian_at(t).map(|u| Combatant::Unit(u.id()))
}

/// A side as one part of a random draw's key: a unit by its id, a city by its id above every
/// unit's, so that a unit and a city with the same number never share a draw.
#[must_use]
pub fn key(c: Combatant) -> u64 {
    match c {
        Combatant::Unit(u) => u64::from(u.get()),
        Combatant::City(x) => (1u64 << 32) | u64::from(x.get()),
    }
}
