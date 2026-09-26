//! The religious units' actions (`religion.py:709-775`): a missionary or great prophet spreading
//! its religion into a city, and an inquisitor removing the other religions from one. Each is a
//! check that only reads (`plan_*`) and an apply that cannot fail, which the unit action of
//! [`crate::game::actions`] runs.

use serde_json::{Value, json};

use super::{
    add_pressure, display_name, followers, is_major, majority_religion, protected_by_inquisitor,
    remove_all_except,
};
use crate::base::ids::{CityId, ReligionId, UniqueId, UnitId};
use crate::base::num;
use crate::base::sets::PlayerSet;
use crate::base::stats::Stat;
use crate::game::Game;
use crate::game::derive::rev::WorldTouch;
use crate::game::error::ActionError;
use crate::game::units::abilities::{consume_action, usable_action};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::map::Tile;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// How much pressure one use of a missionary or prophet applies (`religion.spread_pressure`,
/// `religion.py:709-715`): its religious strength, raised by its own and its owner's `[n]%
/// Spread religion strength`.
#[must_use]
pub fn spread_pressure(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let mut pressure = f64::from(g.rules().base_units()[x.base].religious_strength);
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    for h in uq::unit_and_civ(&v, u, UniqueType::SpreadReligionStrength, &ctx) {
        if let UniqueData::SpreadReligionStrength(s) = *h.data() {
            for _ in 0..h.n {
                pressure *= 1.0 + f64::from(s.percent) / 100.0;
            }
        }
    }
    num::trunc_i32(pressure)
}

/// A spread [`plan_spread`] allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpreadPlan {
    pub unit: UnitId,
    pub city: CityId,
    pub religion: ReligionId,
    /// The use of `Can Spread Religion` it spends.
    pub action: UniqueId,
}

/// Whether unit `u` may spread its religion into the city whose land it stands in now
/// (`religion.spread_religion`'s checks, `religion.py:718-736`). Reads only.
///
/// # Errors
/// Religion out of play, a unit with no major religion, no use or movement left, no city's land,
/// a city that follows it already, or one an inquisitor protects.
pub fn plan_spread(g: &Game, u: UnitId) -> Result<SpreadPlan, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    let p = x.owner();
    if !g.player(p).is_some_and(crate::state::players::Player::is_major) || !g.religion_enabled() {
        return Err(ActionError::rule("Religion cannot be spread."));
    }
    let Some(r) = x.religion.filter(|&r| is_major(g, r)) else {
        return Err(ActionError::rule("This unit carries no religion."));
    };
    let Some(action) = usable_action(g, u, UniqueType::CanSpreadReligion) else {
        return Err(ActionError::rule(
            "This unit cannot spread religion (no uses or movement left).",
        ));
    };
    let city = g
        .tile(x.tile())
        .and_then(Tile::city)
        .and_then(|c| g.city(c))
        .ok_or_else(|| ActionError::rule("Move into a city's territory to spread religion."))?;
    let c = city.id();
    if majority_religion(g, c) == Some(r) {
        return Err(ActionError::rule(format!(
            "{} already follows {}.",
            city.name,
            display_name(g, r)
        )));
    }
    if protected_by_inquisitor(g, c, Some(r)) {
        return Err(ActionError::rule(format!("An inquisitor protects {}.", city.name)));
    }
    Ok(SpreadPlan { unit: u, city: c, religion: r, action })
}

/// The spread [`plan_spread`] allowed (`religion.py:737-750`): the unit's `When spreading
/// religion to a city, gain [n] times the amount of followers of other religions as [stat]`
/// pays out, its pressure arrives (and with `Removes other religions when spreading religion`
/// the other religions leave), a conversion of another's city is told to both, and the use is
/// spent.
pub fn apply_spread(g: &mut Game, plan: SpreadPlan) -> Value {
    let SpreadPlan { unit: u, city: c, religion: r, action } = plan;
    let Some((p, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return Value::Null };
    let others: i32 = g
        .city(c)
        .map_or(0, |x| followers(x).iter().filter(|&&(k, _)| k != r).map(|&(_, n)| n).sum());
    let gains: Vec<(Stat, f64)> = {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        uq::unit_and_civ(&v, u, UniqueType::StatsWhenSpreading, &ctx)
            .filter_map(|h| match *h.data() {
                UniqueData::StatsWhenSpreading(s) => {
                    Some((s.stat, f64::from(others) * f64::from(s.percent) * f64::from(h.n)))
                }
                _ => None,
            })
            .collect()
    };
    for (stat, amount) in gains {
        g.add_stat(p, stat, amount);
    }
    let before = majority_religion(g, c);
    add_pressure(g, c, Some(r), spread_pressure(g, u));
    let rules = g.rules();
    if crate::game::cities::construction::unit_has_type(
        rules,
        base,
        UniqueType::RemoveOtherReligions,
    ) {
        remove_all_except(g, c, r);
    }
    let after = majority_religion(g, c);
    let Some((city_name, owner, at)) = g.city(c).map(|x| (x.name.to_string(), x.owner(), x.tile()))
    else {
        return Value::Null;
    };
    if after != before
        && let Some(now) = after
        && owner != p
    {
        let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
        let text = format!(
            "{who}'s {} converted {city_name} to {}!",
            rules.base_units()[base].name,
            display_name(g, now)
        );
        let mut audience = PlayerSet::single(owner);
        audience.insert(p);
        let data = EventData { religion: Some(now), ..EventData::default() };
        g.emit(EngineEvent::Religion, &text, Some(audience), Some(at), data, &[]);
    }
    let spread = display_name(g, r);
    let majority = after.map(|m| display_name(g, m));
    if g.unit(u).is_some() {
        consume_action(g, u, action);
    }
    json!({ "spread": spread, "city": city_name, "majority": majority })
}

/// An inquisition [`plan_remove_heresy`] allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeresyPlan {
    pub unit: UnitId,
    pub city: CityId,
    pub religion: ReligionId,
    pub action: UniqueId,
}

/// Whether unit `u` may clear the other religions out of the city of its owner whose land it
/// stands in (`religion.remove_heresy`'s checks, `religion.py:753-765`). Reads only.
///
/// # Errors
/// A unit with no major religion, not in its own cities' land, a city with no other religion,
/// or a unit that cannot remove heresy now.
pub fn plan_remove_heresy(g: &Game, u: UnitId) -> Result<HeresyPlan, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    let Some(r) = x.religion.filter(|&r| is_major(g, r)) else {
        return Err(ActionError::rule("This unit carries no religion."));
    };
    let city = g
        .tile(x.tile())
        .and_then(Tile::city)
        .and_then(|c| g.city(c))
        .filter(|c| c.owner() == x.owner())
        .ok_or_else(|| {
            ActionError::rule("Inquisitors remove heresy in your own cities' territory.")
        })?;
    if !city.pressures.iter().any(|&(k, _)| k.is_some_and(|k| k != r)) {
        return Err(ActionError::rule(format!("There is no other religion in {}.", city.name)));
    }
    let Some(action) = usable_action(g, u, UniqueType::CanRemoveHeresy) else {
        return Err(ActionError::rule("This unit cannot remove heresy."));
    };
    Ok(HeresyPlan { unit: u, city: city.id(), religion: r, action })
}

/// The inquisition [`plan_remove_heresy`] allowed (`religion.py:766-775`): every other religion
/// leaves the city; the holy city of another religion is blocked, and the unit's own holy city
/// unblocked; the use is spent.
pub fn apply_remove_heresy(g: &mut Game, plan: HeresyPlan) -> Value {
    let HeresyPlan { unit: u, city: c, religion: r, action } = plan;
    remove_all_except(g, c, r);
    if let Some(holy) = g.city(c).and_then(|x| x.holy_city_of) {
        // Another religion's holy city is blocked; the unit's own is freed.
        let blocked = super::religion(g, holy).is_some_and(|x| x.blocked_holy);
        let want = holy != r;
        if blocked != want
            && let Some(x) =
                g.edit_world(WorldTouch::RELIGIONS).religions.get_mut(usize::from(holy.0))
        {
            x.blocked_holy = want;
        }
    }
    let name = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
    if g.unit(u).is_some() {
        consume_action(g, u, action);
    }
    json!({ "removed_heresy": name })
}
