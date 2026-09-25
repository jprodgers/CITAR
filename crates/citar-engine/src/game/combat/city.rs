//! Cities in combat (`combat.py:874-931`): a city's bombardment, and a city brought to the brink
//! taken by the melee unit that beat it.

use serde_json::{Map, Value};

use super::combatant::{self, combatant_at};
use super::resolve::{contains_attackable_enemy, resolve};
use crate::base::ids::{CityId, TileIdx};
use crate::game::error::ActionError;
use crate::game::units::unit_has;
use crate::game::{Game, Porting, conquest, pending};
use crate::unique::{Combatant, UniqueType};

/// How far a city bombards (`game.json` `base_city_bombard_range`).
fn bombard_range(g: &Game) -> u32 {
    u32::try_from(g.rules().constants().formulas.base_city_bombard_range).unwrap_or(0)
}

/// Why a city cannot bombard now, or `None` if it can (`combat.can_bombard`,
/// `combat.py:893-899`): once a turn, and never in resistance.
#[must_use]
pub fn can_bombard(g: &Game, c: CityId) -> Option<String> {
    let city = g.city(c)?;
    if city.attacked {
        return Some(format!("{} has already attacked this turn.", city.name));
    }
    if city.resistance > 0 {
        return Some(format!("{} is in resistance.", city.name));
    }
    None
}

/// Every tile a city could shoot at now (`combat.bombard_targets`, `combat.py:902-913`): within
/// its range, seen by its owner, holding an enemy it may attack; in the grid's order.
#[must_use]
pub fn bombard_targets(g: &Game, c: CityId) -> Vec<TileIdx> {
    let Some(city) = g.city(c) else { return Vec::new() };
    let (owner, centre) = (city.owner(), city.tile());
    let a = Combatant::City(c);
    g.grid()
        .within(centre, bombard_range(g))
        .into_iter()
        .filter(|&t| t != centre && g.derived().vis().sees(owner, t))
        .filter(|&t| contains_attackable_enemy(g, t, a).is_none())
        .collect()
}

/// Checks a city's bombardment of tile `t` and returns what it would hit (`combat.city_bombard`'s
/// checks, `combat.py:916-930`).
///
/// # Errors
/// The city has attacked this turn or is in resistance, the tile is out of its range or sight,
/// or holds nothing it may attack.
pub fn plan_bombard(g: &Game, c: CityId, t: TileIdx) -> Result<Combatant, ActionError> {
    if let Some(why) = can_bombard(g, c) {
        return Err(ActionError::rule(why));
    }
    let city = g.city(c).ok_or_else(|| ActionError::rule("No such city."))?;
    let range = bombard_range(g);
    if g.grid().distance(city.tile(), t) > range {
        return Err(ActionError::rule(format!("Target is out of the city's range ({range}).")));
    }
    if !g.derived().vis().sees(city.owner(), t) {
        return Err(ActionError::rule("You cannot see that tile."));
    }
    if let Some(why) = contains_attackable_enemy(g, t, Combatant::City(c)) {
        return Err(ActionError::rule(why));
    }
    combatant_at(g, t).ok_or_else(|| ActionError::rule("There is nothing to attack there."))
}

/// A city fires at what [`plan_bombard`] found (`combat.city_bombard`).
pub fn city_bombard(g: &mut Game, c: CityId, d: Combatant) -> Value {
    resolve(g, Combatant::City(c), d)
}

/// A city brought to the brink is taken by the melee unit that beat it
/// (`combat._handle_city_defeated`, `combat.py:874-887`): the barbarians sack it instead
/// (package 1c-06), and a unit that `Cannot capture cities` leaves it. What the capture reports,
/// if one happened.
pub(crate) fn handle_city_defeated(
    g: &mut Game,
    a: Combatant,
    d: Combatant,
) -> Option<Map<String, Value>> {
    let (Combatant::City(c), Combatant::Unit(u)) = (d, a) else { return None };
    if !combatant::defeated(g, d) || !combatant::is_melee(g, a) || g.unit(u).is_none() {
        return None;
    }
    if g.is_barbarian(combatant::owner(g, a)) {
        // barbarians.sack_city: the barbarians sack a city rather than take it.
        pending(Porting::Pending("1c-06"));
        return None;
    }
    if unit_has(g, u, UniqueType::CannotCaptureCities, true) {
        return None;
    }
    Some(conquest::conquer(g, c, u))
}
