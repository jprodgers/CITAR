//! Upgrades and their costs (`units.py:488-602`, UnCiv's `UnitUpgradeManager`), and disbanding
//! (`units.py:763-776`).
//!
//! An upgrade replaces the unit with a new one of the better type, as Python did: the new unit
//! keeps its health, experience, promotions, bought-promotion count, name, religion and original
//! owner, and has no movement left. Where the new type may stand is worked out before the old unit
//! goes, as if it were gone already, so an upgrade that cannot be placed changes nothing: Python
//! removed the unit first and put a copy of it back.

use serde_json::{Value, json};

use super::{equivalent_unit, remove_unit, spawn_spot, type_uniques};
use crate::base::ids::{BaseUnitId, UnitId};
use crate::base::num;
use crate::game::Game;
use crate::game::cities::purchase::base_gold_cost;
use crate::game::derive::rev::{PlayerTouch, UnitTouch};
use crate::game::error::ActionError;
use crate::rules::defs::Domain;
use crate::state::cities::Constructible;
use crate::state::map::Tile;
use crate::unique::{Ctx, FilterFacts, UniqueData, UniqueType, applies, uq};

/// What a unit could upgrade into (`units.upgrade_targets`, `units.py:488-501`): its type's
/// `Can upgrade to [unit]` that hold for it, then the type it upgrades to; each its owner's
/// nation's own version. With `special`, its `May upgrade to [unit] through ruins-like effects`
/// first, alone.
#[must_use]
pub fn upgrade_targets(g: &Game, u: UnitId, special: bool) -> Vec<BaseUnitId> {
    let Some(x) = g.unit(u) else { return Vec::new() };
    let r = g.rules();
    let t = r.uniques();
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    let holding = |ty: UniqueType| {
        type_uniques(g, x.base)
            .filter(move |&id| t.meta(id).ty == Some(ty) && applies(id, &ctx, &v))
            .filter_map(|id| match t.get(id).data {
                UniqueData::RuinsUpgrade(y) => Some(y.unit),
                UniqueData::CanUpgrade(y) => Some(y.unit),
                _ => None,
            })
    };
    if special && let Some(b) = holding(UniqueType::RuinsUpgrade).next() {
        return vec![equivalent_unit(g, x.owner(), b)];
    }
    let mut out: Vec<BaseUnitId> = holding(UniqueType::CanUpgrade).collect();
    out.extend(r.base_units()[x.base].upgrades_to);
    out.into_iter().map(|b| equivalent_unit(g, x.owner(), b)).collect()
}

/// The gold an upgrade costs (`units.upgrade_cost`, `units.py:504-515`): a base plus the
/// difference in production cost, grown by the target's era, times the owner's `[n]% Gold cost of
/// upgrading`, raised to an exponent, scaled by game speed, rounded down to a multiple.
#[must_use]
pub fn upgrade_cost(g: &Game, u: UnitId, target: BaseUnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let r = g.rules();
    let c = &r.constants().formulas.unit_upgrade_cost;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    let mut civ_mod = 1.0f64;
    for h in uq::civ(&v, x.owner(), UniqueType::UnitUpgradeCost, &ctx) {
        if let UniqueData::UnitUpgradeCost(y) = h.data() {
            for _ in 0..h.n {
                civ_mod *= 1.0 + f64::from(y.percent) / 100.0;
            }
        }
    }
    let (from, to) = (&r.base_units()[x.base], &r.base_units()[target]);
    let mut cost = c.base + (c.per_production * f64::from(to.cost - from.cost)).max(0.0);
    cost *= 1.0 + f64::from(to.era.0) * c.era_multiplier;
    cost = crate::base::num::pow(cost * civ_mod, c.exponent);
    cost *= g.speed().modifier;
    let step = c.round_to.max(1);
    #[allow(clippy::cast_possible_truncation, reason = "Python's int(): toward zero, in range")]
    let steps = (cost / f64::from(step)) as i32;
    steps.saturating_mul(step)
}

/// Why the owner may not have the target type (`units._upgrade_blockers`, `units.py:518-535`):
/// its tech, a nation's own unit, its resource (the unit's own counts), nuclear weapons off.
fn blockers(
    g: &Game,
    u: UnitId,
    target: BaseUnitId,
    ignore_requirements: bool,
    ignore_resources: bool,
) -> Option<String> {
    let x = g.unit(u)?;
    let r = g.rules();
    let td = &r.base_units()[target];
    let name = &*td.name;
    let owner = x.owner();
    if !ignore_requirements && td.required_tech.is_some() && !g.has_tech(owner, td.required_tech) {
        let tech = td.required_tech.and_then(|t| r.name(t)).unwrap_or("");
        return Some(format!("{name} requires {tech}."));
    }
    if let Some(n) = td.unique_to
        && g.player(owner).is_some_and(|p| p.nation != n)
    {
        return Some(format!("{name} is unique to {}.", r.nations()[n].name));
    }
    if !ignore_resources && let Some(res) = td.required_resource {
        let have = crate::game::economy::resource_amount(g, owner, res);
        let own = i32::from(r.base_units()[x.base].required_resource == Some(res));
        if have + own < 1 {
            return Some(format!("{name} needs {}.", r.resources()[res].name));
        }
    }
    if !g.nukes_enabled() && super::type_has(g, target, UniqueType::NuclearWeapon) {
        return Some("Nuclear weapons are disabled.".to_owned());
    }
    None
}

/// An upgrade a player asked for, checked (`units.check_upgrade`, `units.py:538-560`): the
/// target, and why it cannot happen now, and its cost. The first target the owner may have is
/// the one; without one, the last target's blocker.
pub struct UpgradeCheck {
    pub target: Option<BaseUnitId>,
    pub refusal: Option<String>,
    pub cost: i32,
}

/// Checks an upgrade (`units.check_upgrade`): the unit must stand in its owner's land, not
/// embarked, with movement left, and the owner must have the gold.
#[must_use]
pub fn check_upgrade(g: &Game, u: UnitId) -> UpgradeCheck {
    let Some(x) = g.unit(u) else {
        return UpgradeCheck { target: None, refusal: Some("No such unit.".into()), cost: 0 };
    };
    let r = g.rules();
    let targets = upgrade_targets(g, u, false);
    let Some(&first) = targets.first() else {
        let name = r.name(x.base).unwrap_or("");
        return UpgradeCheck {
            target: None,
            refusal: Some(format!("{name} has no upgrade.")),
            cost: 0,
        };
    };
    let mut last = None;
    for &t in &targets {
        let cost = upgrade_cost(g, u, t);
        if let Some(b) = blockers(g, u, t, false, false) {
            last = Some(b);
            continue;
        }
        let refusal = if g.tile(x.tile()).and_then(Tile::owner) != Some(x.owner()) {
            Some("Units can only upgrade inside your own territory.".to_owned())
        } else if g.view().unit_embarked(u) {
            Some("Embarked units cannot upgrade.".to_owned())
        } else if x.moves <= 0 {
            Some("The unit has no movement left.".to_owned())
        } else {
            let gold = g.player(x.owner()).map_or(0.0, |p| p.econ.gold);
            (gold < f64::from(cost)).then(|| {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "Python's int(), for the message"
                )]
                let have = gold as i64;
                format!(
                    "Upgrading to {} costs {cost} gold; you have {have}.",
                    r.base_units()[t].name
                )
            })
        };
        return UpgradeCheck { target: Some(t), refusal, cost };
    }
    UpgradeCheck { target: Some(first), refusal: last, cost: upgrade_cost(g, u, first) }
}

/// A checked upgrade, ready to happen: the unit, its new type, where the new unit stands, and
/// the gold it costs.
#[derive(Clone, Copy, Debug)]
pub struct UpgradePlan {
    pub unit: UnitId,
    pub target: BaseUnitId,
    pub spot: crate::base::ids::TileIdx,
    pub cost: i32,
}

/// Plans an upgrade a player asked for (`units.upgrade`, `units.py:584-594`): the check's refusal,
/// or Python's when the new unit has nowhere to stand.
pub fn plan_upgrade(g: &Game, u: UnitId) -> Result<UpgradePlan, ActionError> {
    let c = check_upgrade(g, u);
    if let Some(why) = c.refusal {
        return Err(ActionError::rule(why));
    }
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    let target = c.target.ok_or_else(|| ActionError::rule("No upgrade."))?;
    // refcheck: upgrade-places-before-removing
    let spot = spawn_spot(g, x.owner(), target, x.tile(), 10, Some(u))
        .ok_or_else(|| ActionError::rule("The upgraded unit could not be placed."))?;
    Ok(UpgradePlan { unit: u, target, spot, cost: c.cost })
}

/// Carries out a planned upgrade and charges for it; what the tool reports.
pub fn apply_upgrade(g: &mut Game, p: UpgradePlan) -> Value {
    let old = g.unit(p.unit).map(|x| (x.owner(), x.base));
    let Some((owner, from)) = old else { return Value::Null };
    let Some(nu) = replace(g, p.unit, p.target, p.spot) else { return Value::Null };
    if let Some(pl) = g.player_mut(owner, PlayerTouch::STOCKS) {
        pl.econ.gold -= f64::from(p.cost);
    }
    let r = g.rules();
    json!({
        "upgraded": r.name(from).unwrap_or(""),
        "to": r.name(p.target).unwrap_or(""),
        "unit_id": nu.get(),
        "gold_spent": p.cost,
    })
}

/// Replaces a unit with a new one of `target` on `spot`, which the caller found before, keeping
/// what carries over (`units._perform_upgrade`, `units.py:563-581`).
// refcheck: upgrade-places-before-removing (Python removed the unit, then looked for a spot)
fn replace(
    g: &mut Game,
    u: UnitId,
    target: BaseUnitId,
    spot: crate::base::ids::TileIdx,
) -> Option<UnitId> {
    let x = g.unit(u)?.clone();
    remove_unit(g, u);
    let nu = g.create_unit(x.owner(), target, spot, 0).ok()?;
    if let Some(y) = g.unit_mut(nu, UnitTouch::CORE | UnitTouch::MOVES) {
        y.hp = x.hp;
        y.xp = x.xp;
        y.promotion_count = x.promotion_count;
        y.name.clone_from(&x.name);
        y.religion = x.religion;
        y.promotions |= x.promotions;
        y.original_owner = x.original_owner;
        y.moves = 0;
    }
    Some(nu)
}

/// Upgrades a unit for nothing, where a unique grants it (`units.free_upgrade`,
/// `units.py:597-602`): the first target the owner may have, tech and resources aside; with
/// `special`, along its ruins path. Whether it upgraded.
pub fn free_upgrade(g: &mut Game, u: UnitId, special: bool) -> bool {
    let Some((owner, tile)) = g.unit(u).map(|x| (x.owner(), x.tile())) else { return false };
    for t in upgrade_targets(g, u, special) {
        if blockers(g, u, t, true, true).is_some() {
            continue;
        }
        // refcheck: upgrade-places-before-removing
        return match spawn_spot(g, owner, t, tile, 10, Some(u)) {
            Some(spot) => replace(g, u, t, spot).is_some(),
            None => false,
        };
    }
    false
}

/// Whether [`free_upgrade`] would upgrade unit `u` now: its first target it may have, and a
/// spot for the new unit.
#[must_use]
pub fn can_free_upgrade(g: &Game, u: UnitId, special: bool) -> bool {
    let Some((owner, tile)) = g.unit(u).map(|x| (x.owner(), x.tile())) else { return false };
    upgrade_targets(g, u, special)
        .into_iter()
        .find(|&t| blockers(g, u, t, true, true).is_none())
        .is_some_and(|t| spawn_spot(g, owner, t, tile, 10, Some(u)).is_some())
}

/// The gold a disbanded unit refunds inside its owner's borders (`units.disband_gold`,
/// `units.py:763-766`): a twentieth of its purchase cost.
#[must_use]
pub fn disband_gold(g: &Game, u: UnitId) -> i32 {
    let Some((owner, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return 0 };
    let cost = base_gold_cost(g, owner, Constructible::Unit(base), None);
    num::trunc_i32(cost).div_euclid(20)
}

/// Disbands a unit (`units.disband`, `units.py:769-776`, `MapUnit.disband`): the refund inside
/// its owner's borders, and the units it carried go with it. What the tool reports.
pub fn disband(g: &mut Game, u: UnitId) -> Value {
    let Some((owner, base, tile)) = g.unit(u).map(|x| (x.owner(), x.base, x.tile())) else {
        return Value::Null;
    };
    let gold =
        if g.tile(tile).and_then(Tile::owner) == Some(owner) { disband_gold(g, u) } else { 0 };
    if gold != 0
        && let Some(pl) = g.player_mut(owner, PlayerTouch::STOCKS)
    {
        pl.econ.gold += f64::from(gold);
    }
    let cargo: Vec<UnitId> = g.state().units().carried_by(u).collect();
    for c in cargo {
        remove_unit(g, c);
    }
    remove_unit(g, u);
    json!({"disbanded": g.rules().name(base).unwrap_or(""), "gold": gold})
}

/// Whether a unit is an aircraft.
#[must_use]
pub fn is_air(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| g.rules().base_units()[x.base].domain == Domain::Air)
}
