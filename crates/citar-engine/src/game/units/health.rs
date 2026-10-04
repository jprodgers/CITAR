//! A unit's movement allowance, range and attacks, its healing and damage (`units.py:288-397`),
//! and the one-time effects on a unit (`triggers.py:339-367`).

use super::{type_has, unit_matches, unit_sum};
use crate::base::ids::{TileIdx, UnitId};
use crate::game::Game;
use crate::game::derive::rev::UnitTouch;
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::trigger::UnitEffect;
use crate::unique::{Ctx, FilterFacts, UniqueData, UniqueType, uq};

/// A unit's movement points per turn, whole (`units.max_movement`, `units.py:288-301`,
/// `MapUnit.getMaxMovement`): 2 embarked, else its base movement; its own and its owner's
/// `[n] Movement`; at least 1; and the movement of a unit on its tile that transfers its movement
/// to units like it (a Great General beside an army).
#[must_use]
pub fn max_movement(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let r = g.rules();
    let movement = |d: &UniqueData| match d {
        UniqueData::Movement(m) => Some(m.movement),
        _ => None,
    };
    let base = if g.view().unit_embarked(u) { 2 } else { r.base_units()[x.base].movement };
    let mut mv = base.saturating_add(unit_sum(g, u, UniqueType::Movement, true, movement)).max(1);
    let v = g.view();
    for other in g.units_at(x.tile()) {
        let o = other.id();
        if o == u {
            continue;
        }
        let octx = Ctx::unit(&v, o);
        let transfers =
            uq::unit(&v, o, UniqueType::TransferMovement, &octx).any(|h| match h.data() {
                UniqueData::TransferMovement(t) => unit_matches(g, u, t.units),
                _ => false,
            });
        if transfers {
            let theirs = r.base_units()[other.base].movement.saturating_add(unit_sum(
                g,
                o,
                UniqueType::Movement,
                true,
                movement,
            ));
            mv = mv.max(theirs);
        }
    }
    mv
}

/// A unit's movement allowance in move-scale units (`movement.max_moves`, `movement.py:110-116`):
/// an aircraft one point, anything else its movement points.
#[must_use]
pub fn max_moves(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let sc = g.rules().constants().move_scale;
    if g.rules().base_units()[x.base].domain == Domain::Air {
        return sc;
    }
    max_movement(g, u).saturating_mul(sc)
}

/// How far a unit attacks (`units.attack_range`, `units.py:313-318`): 1 for melee, else its range
/// with its own and its owner's `[n] Range`.
#[must_use]
pub fn attack_range(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let d = &g.rules().base_units()[x.base];
    if d.melee {
        return 1;
    }
    d.range.saturating_add(unit_sum(g, u, UniqueType::Range, true, |d| match d {
        UniqueData::Range(r) => Some(r.range),
        _ => None,
    }))
}

/// How often a unit attacks in a turn (`units.max_attacks`, `units.py:321-323`).
#[must_use]
pub fn max_attacks(g: &Game, u: UnitId) -> i32 {
    1i32.saturating_add(unit_sum(g, u, UniqueType::AdditionalAttacks, true, |d| match d {
        UniqueData::AdditionalAttacks(a) => Some(a.count),
        _ => None,
    }))
}

/// How much a unit would heal on tile `t` (`units.heal_for_tile`, `units.py:329-363`): 25 in a
/// city, 20 in friendly land (on friendly water only a ship or a carried unit), 10 in the open
/// and nothing at sea; then its `[n] HP when healing` asked on the tile, its own city's
/// `[units] Units adjacent to this city heal [n] HP` from the first city within one tile, and
/// the best `All adjacent units heal [n] HP` of its own units there or next to it.
#[must_use]
pub fn heal_for_tile(g: &Game, u: UnitId, t: TileIdx) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let owner = x.owner();
    let v = g.view();
    let friendly = v.tile_friendly_to(t, owner);
    let water = g.is_water(t);
    let naval = g.rules().base_units()[x.base].domain == Domain::Water;
    let mut h = if g.city_at(t).is_some() {
        25
    } else if water && friendly && (naval || x.carried_by().is_some()) {
        20
    } else if water {
        0
    } else if friendly {
        20
    } else {
        10
    };
    let heals_outside = || {
        let ctx = Ctx::unit(&v, u);
        uq::any(uq::unit_and_civ(&v, u, UniqueType::HealsOutsideFriendlyTerritory, &ctx))
    };
    if !(h > 0 || (water && heals_outside())) {
        return h;
    }
    let tctx = Ctx { civ: Some(owner), unit: Some(u), tile: Some(t), ..Ctx::default() }.resolve(&v);
    h = h.saturating_add(uq::sum_i32(
        uq::unit_and_civ(&v, u, UniqueType::Heal, &tctx),
        |d| match d {
            UniqueData::Heal(x) => Some(x.hp),
            _ => None,
        },
    ));
    // Only the first city within one tile counts, as Python stopped at it.
    if let Some(c) = g.grid().within(t, 1).into_iter().find_map(|n| g.city_at(n))
        && c.owner() == owner
    {
        let cid = c.id();
        let cctx = Ctx::city(&v, cid);
        h = h.saturating_add(uq::sum_i32(
            uq::city(&v, cid, UniqueType::CityHealingUnits, &cctx),
            |d| match d {
                UniqueData::CityHealingUnits(x) if unit_matches(g, u, x.units) => Some(x.hp),
                _ => None,
            },
        ));
    }
    let mut best = 0i32;
    for n in g.grid().neighbors(t).chain(core::iter::once(t)) {
        for o in g.units_at(n) {
            if o.owner() == owner && o.id() != u {
                best =
                    best.max(unit_sum(
                        g,
                        o.id(),
                        UniqueType::HealAdjacentUnits,
                        false,
                        |d| match d {
                            UniqueData::HealAdjacentUnits(x) => Some(x.hp),
                            _ => None,
                        },
                    ));
            }
        }
    }
    h.saturating_add(best)
}

/// How much a unit heals where it stands at the end of its turn (`units.heal_amount_here`,
/// `units.py:366-374`): nothing embarked or unhurt, nothing for one that heals only by
/// pillaging (the barbarians' by their nation), else what its tile gives.
#[must_use]
pub fn heal_amount_here(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    if g.view().unit_embarked(u) || x.hp >= 100 {
        return 0;
    }
    let owner = x.owner();
    if super::unit_has(g, u, UniqueType::HealOnlyByPillaging, true) {
        return 0;
    }
    if g.is_barbarian(owner) {
        let r = g.rules();
        let by_nation = g.player(owner).is_some_and(|p| {
            crate::game::core::has_type(
                r,
                &r.nations()[p.nation].uniques,
                UniqueType::HealOnlyByPillaging,
            )
        });
        if by_nation {
            return 0;
        }
    }
    heal_for_tile(g, u, x.tile())
}

/// Heals a unit by `amount`, doubled by `All healing effects doubled`, to full health at most
/// (`units.heal_by`, `units.py:377-381`).
pub fn heal_by(g: &mut Game, u: UnitId, amount: i32) {
    let doubled = super::unit_has(g, u, UniqueType::HealingEffectsDoubled, true);
    let amount = if doubled { amount.saturating_mul(2) } else { amount };
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        let hp = i32::from(x.hp).saturating_add(amount).min(100);
        x.hp = i16::try_from(hp).unwrap_or(100);
    }
}

/// Damages a unit; whether it died, which takes it out of the game (`units.take_damage`,
/// `units.py:384-392`), with `note` announced to its owner if given.
pub fn take_damage(g: &mut Game, u: UnitId, amount: i32, note: Option<&str>) -> bool {
    let Some((owner, tile, hp)) = g.unit(u).map(|x| (x.owner(), x.tile(), x.hp)) else {
        return false;
    };
    let left = i32::from(hp).saturating_sub(amount).clamp(0, 100);
    if left <= 0 {
        if g.despawn_unit(u).is_err() {
            return false;
        }
        if let Some(text) = note {
            let audience = core::iter::once(owner).collect();
            g.emit(
                EngineEvent::UnitKilled,
                text,
                Some(audience),
                Some(tile),
                EventData::default(),
                &[],
            );
        }
        return true;
    }
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.hp = i16::try_from(left).unwrap_or(1);
    }
    false
}

/// The damage a tile's terrain does to units standing on it (`units.terrain_damage`,
/// `units.py:395-397`): its terrains' `Units ending their turn on this terrain take [n] damage`,
/// whatever their conditionals.
#[must_use]
pub fn terrain_damage(g: &Game, t: TileIdx) -> i32 {
    let v = g.view();
    uq::sum_i32(
        uq::terrains(&v, t, UniqueType::DamagesContainingUnits, &Ctx::IGNORE),
        |d| match d {
            UniqueData::DamagesContainingUnits(x) => Some(x.damage),
            _ => None,
        },
    )
}

/// Whether [`apply_unit_effect`] would do anything to unit `u` now, without doing it: all but
/// healing a unit at full health and an upgrade with no target or no room do.
#[must_use]
pub fn unit_effect_would_apply(g: &Game, u: UnitId, e: UnitEffect) -> bool {
    let Some(hp) = g.unit(u).map(|x| x.hp) else { return false };
    match e {
        UnitEffect::Heal(_) => hp < 100,
        UnitEffect::Upgrade => super::upgrades::can_free_upgrade(g, u, false),
        UnitEffect::SpecialUpgrade => super::upgrades::can_free_upgrade(g, u, true),
        UnitEffect::Damage(_)
        | UnitEffect::GainXp(_)
        | UnitEffect::GainPromotion(_)
        | UnitEffect::Movement(_)
        | UnitEffect::Destroyed => true,
    }
}

/// A one-time effect on a unit (`triggers.py:339-367`): healing (no doubling), damage,
/// experience, a free upgrade, a promotion, movement gained or lost, or its end. Whether
/// anything happened. `note` is what caused it, for the announcement of experience gained.
pub fn apply_unit_effect(g: &mut Game, u: UnitId, e: UnitEffect, note: Option<&str>) -> bool {
    let Some((owner, base, tile, hp)) = g.unit(u).map(|x| (x.owner(), x.base, x.tile(), x.hp))
    else {
        return false;
    };
    match e {
        UnitEffect::Heal(n) => {
            if hp >= 100 {
                return false;
            }
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.hp =
                    i16::try_from(i32::from(x.hp).saturating_add(n).clamp(1, 100)).unwrap_or(100);
            }
            true
        }
        UnitEffect::Damage(n) => {
            take_damage(g, u, n, None);
            true
        }
        UnitEffect::GainXp(n) => {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.xp = x.xp.saturating_add(n).max(0);
            }
            let suffix = note.map(|n| format!(" ({n})")).unwrap_or_default();
            let who = g.player(owner).map(|p| p.name.to_string()).unwrap_or_default();
            let what = g.rules().name(base).unwrap_or("");
            let text = format!("{who}'s {what} gained {n} XP{suffix}.");
            let audience = core::iter::once(owner).collect();
            g.emit(
                EngineEvent::Ruins,
                &text,
                Some(audience),
                Some(tile),
                EventData::default(),
                &[],
            );
            true
        }
        UnitEffect::Upgrade => super::upgrades::free_upgrade(g, u, false),
        UnitEffect::SpecialUpgrade => super::upgrades::free_upgrade(g, u, true),
        UnitEffect::GainPromotion(p) => {
            super::promotions::add_promotion(g, u, p, true);
            true
        }
        UnitEffect::Movement(n) => {
            let delta = n.saturating_mul(g.rules().constants().move_scale);
            if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
                x.moves = x.moves.saturating_add(delta).max(0);
            }
            true
        }
        UnitEffect::Destroyed => g.despawn_unit(u).is_ok(),
    }
}

/// Whether a unit is of a religious type (`ReligiousUnit`, whatever its conditionals).
#[must_use]
pub fn is_religious(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| type_has(g, x.base, UniqueType::ReligiousUnit))
}
