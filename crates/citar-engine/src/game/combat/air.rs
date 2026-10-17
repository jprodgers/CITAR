//! Aircraft (`combat.py:934-1073`): interception, air strikes, air sweeps and rebasing.
//!
//! **Interception** (`try_intercept`) is a combat event of its own: it takes the next
//! `combat_seq` and draws its chance and its roll from `Purpose::Intercept`. **An air sweep**
//! orders the interceptors it meets from `Purpose::InterceptOrder` where Python shuffled them with
//! `g.rng`, then fights the first as one combat event.

use serde_json::{Map, Value, json};
use smallvec::SmallVec;

use super::combatant::{self, combatant_at};
use super::resolve::{
    add_xp, blows, can_attack_now, contains_attackable_enemy, kill_unit, player_name, resolve,
    stream,
};
use super::strength;
use crate::base::ids::{PlayerId, TileIdx, UnitId};
use crate::base::rng::Purpose;
use crate::game::Game;
use crate::game::derive::rev::UnitTouch;
use crate::game::error::ActionError;
use crate::game::path::{can_carry, stack_reason};
use crate::game::units::{health, unit_has};
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::{Combatant, Ctx, UniqueData, UniqueType, uq};

/// The sum of an amount over a unit's own uniques of one type that hold for it, with copies.
fn own_sum(g: &Game, u: UnitId, ty: UniqueType, with_civ: bool) -> i32 {
    crate::game::units::unit_sum(g, u, ty, with_civ, |d| match *d {
        UniqueData::ChanceInterceptAirAttacks(x) => Some(x.percent),
        UniqueData::ExtraInterceptionsPerTurn(x) => Some(x.count),
        UniqueData::AirInterceptionRange(x) => Some(x.range),
        UniqueData::DamageWhenIntercepting(x) => Some(x.percent),
        _ => None,
    })
}

/// A unit's chance in percent to intercept an air attack (`combat.intercept_chance`,
/// `combat.py:937-940`).
#[must_use]
pub fn intercept_chance(g: &Game, u: UnitId) -> i32 {
    own_sum(g, u, UniqueType::ChanceInterceptAirAttacks, false)
}

/// Whether a unit is placed to intercept over tile `t` now (`combat.can_intercept`,
/// `combat.py:943-956`): it has a chance to, an aircraft has movement left, it has interceptions
/// left this turn, and the tile is within its interception range with its own and its
/// civilization's `[n] Air Interception Range`.
///
/// Asked of every unit of a side, or of the game for an air sweep, so the unique queries come
/// last: a unit whose profile carries no chance to intercept (`CombatRules::intercept`, nearly
/// all of them) is out at once.
#[must_use]
pub fn can_intercept(g: &Game, u: UnitId, t: TileIdx) -> bool {
    let Some(x) = g.unit(u) else { return false };
    let r = g.rules();
    if !r.derived().combat.intercept.may(x.base, &x.promotions) {
        return false;
    }
    let def = &r.base_units()[x.base];
    if def.domain == Domain::Air && x.moves <= 0 {
        return false;
    }
    if intercept_chance(g, u) == 0 {
        return false;
    }
    let most = 1i32.saturating_add(own_sum(g, u, UniqueType::ExtraInterceptionsPerTurn, false));
    if i32::from(x.interceptions) >= most {
        return false;
    }
    let range =
        def.intercept_range.saturating_add(own_sum(g, u, UniqueType::AirInterceptionRange, true));
    i64::from(g.grid().distance(x.tile(), t)) <= i64::from(range)
}

/// What multiplies an interceptor's damage to an aircraft (`combat.py:974-976`): the
/// interceptor's `[n]% Damage when intercepting`, then each of the aircraft's `Damage taken from
/// interception reduced by [n]%`.
#[must_use]
pub fn interception_factor(g: &Game, interceptor: UnitId, aircraft: UnitId) -> f64 {
    let mut factor =
        1.0 + f64::from(own_sum(g, interceptor, UniqueType::DamageWhenIntercepting, false)) / 100.0;
    let v = g.view();
    let ctx = Ctx::unit(&v, aircraft);
    for h in uq::unit(&v, aircraft, UniqueType::DamageFromInterceptionReduced, &ctx) {
        if let UniqueData::DamageFromInterceptionReduced(x) = h.data() {
            for _ in 0..h.n {
                factor *= 1.0 - f64::from(x.percent) / 100.0;
            }
        }
    }
    factor
}

/// Whether an aircraft `Cannot be intercepted`.
#[must_use]
pub fn cannot_be_intercepted(g: &Game, u: UnitId) -> bool {
    unit_has(g, u, UniqueType::CannotBeIntercepted, false)
}

/// The units of `civ` that could intercept an air attack on `t`, the defender itself left out, the
/// likeliest first and in id order among equals (`combat.try_intercept`'s candidates,
/// `combat.py:964-967`).
#[must_use]
pub fn interceptors(
    g: &Game,
    civ: PlayerId,
    t: TileIdx,
    defender: Option<Combatant>,
) -> Vec<UnitId> {
    let mut out: Vec<(i32, UnitId)> = g
        .player_units(civ)
        .map(crate::state::units::Unit::id)
        .filter(|&u| defender != Some(Combatant::Unit(u)) && can_intercept(g, u, t))
        .map(|u| (intercept_chance(g, u), u))
        .collect();
    out.sort_by_key(|&(chance, _)| core::cmp::Reverse(chance));
    out.into_iter().map(|(_, u)| u).collect()
}

/// Gives the defending civilization's interceptors a chance at an air attack on `t`, and returns
/// the damage done to the aircraft (`combat.try_intercept`, `combat.py:959-983`): the likeliest
/// interceptor spends an interception, then, if its chance comes up, deals its damage at a roll
/// times the factor, at most what the aircraft has left.
pub fn try_intercept(
    g: &mut Game,
    aircraft: UnitId,
    t: TileIdx,
    civ: PlayerId,
    defender: Option<Combatant>,
) -> i32 {
    if cannot_be_intercepted(g, aircraft) {
        return 0;
    }
    let Some(&icpt) = interceptors(g, civ, t, defender).first() else { return 0 };
    let chance = intercept_chance(g, icpt);
    let mut rng = stream(g, Purpose::Intercept, u64::from(aircraft.get()), u64::from(icpt.get()));
    if let Some(x) = g.unit_mut(icpt, UnitTouch::CORE) {
        x.interceptions = x.interceptions.saturating_add(1);
    }
    if rng.unit() > f64::from(chance) / 100.0 {
        return 0;
    }
    let ic = Combatant::Unit(icpt);
    let a = Combatant::Unit(aircraft);
    let from = combatant::tile(g, ic);
    let roll = rng.unit();
    let s = strength::setup(g, ic, from, a, false);
    let factor = interception_factor(g, icpt, aircraft);
    let hp = combatant::hp(g, a);
    let dmg = crate::base::num::trunc_i32(f64::from(s.damage_to_defender(roll)) * factor).min(hp);
    if let Some(x) = g.unit_mut(aircraft, UnitTouch::CORE) {
        x.hp = i16::try_from(hp - dmg).unwrap_or(0);
    }
    let a_owner = combatant::owner(g, a);
    if dmg > 0 {
        add_xp(g, ic, 2, a_owner);
    }
    let text = format!(
        "{}'s {} intercepted {}'s {} (-{dmg} HP).",
        player_name(g, civ),
        combatant::name(g, ic),
        player_name(g, a_owner),
        combatant::name(g, a)
    );
    let audience = [civ, a_owner].into_iter().collect();
    g.emit(EngineEvent::Combat, &text, Some(audience), Some(t), EventData::default(), &[]);
    dmg
}

/// What an aircraft's attack on `t` would hit, checked (`combat.air_strike`'s checks,
/// `combat.py:986-1006`): it can attack now, the tile is in its range and sight, and holds an
/// enemy it may attack.
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_air_strike(g: &Game, u: UnitId, t: TileIdx) -> Result<Combatant, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    if g.rules().base_units()[x.base].domain != Domain::Air {
        return Err(ActionError::rule("Only aircraft make air strikes."));
    }
    if let Some(why) = can_attack_now(g, u) {
        return Err(ActionError::rule(why));
    }
    let range = health::attack_range(g, u);
    if i64::from(g.grid().distance(x.tile(), t)) > i64::from(range) {
        return Err(ActionError::rule(format!("Target out of range ({range}).")));
    }
    if !g.derived().vis().sees(x.owner(), t) {
        return Err(ActionError::rule("You cannot see that tile."));
    }
    if let Some(why) = contains_attackable_enemy(g, t, Combatant::Unit(u)) {
        return Err(ActionError::rule(why));
    }
    combatant_at(g, t).ok_or_else(|| ActionError::rule("There is nothing to attack there."))
}

/// An aircraft attacks what [`plan_air_strike`] found, after any interception
/// (`combat.air_strike`).
pub fn air_strike(g: &mut Game, u: UnitId, d: Combatant) -> Value {
    resolve(g, Combatant::Unit(u), d)
}

/// Checks a fighter's sweep of tile `t` (`combat.air_sweep`'s checks, `combat.py:1010-1019`):
/// it may sweep, attack now, and reach the tile.
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_air_sweep(g: &Game, u: UnitId, t: TileIdx) -> Result<(), ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    if !unit_has(g, u, UniqueType::CanAirsweep, false) {
        return Err(ActionError::rule("This unit cannot perform air sweeps."));
    }
    if let Some(why) = can_attack_now(g, u) {
        return Err(ActionError::rule(why));
    }
    let range = health::attack_range(g, u);
    if i64::from(g.grid().distance(x.tile(), t)) > i64::from(range) {
        return Err(ActionError::rule(format!("Target out of range ({range}).")));
    }
    Ok(())
}

/// A fighter clears the interceptors over tile `t` so that bombers can follow
/// (`combat.air_sweep`, `combat.py:1010-1048`): the sweep spends an attack; every enemy unit that
/// could intercept there is a candidate, the aircraft only if there are any; the likeliest,
/// drawn at random among equals, spends an interception, and an aircraft among them fights the
/// sweeper. A ground interceptor fires and misses.
pub fn air_sweep(g: &mut Game, u: UnitId, t: TileIdx) -> Value {
    let Some(owner) = g.unit(u).map(crate::state::units::Unit::owner) else {
        return Value::Null;
    };
    let keeps = unit_has(g, u, UniqueType::CanMoveAfterAttacking, false);
    let attacks = g.unit(u).map_or(0, |x| x.attacks).saturating_add(1);
    let again = health::max_attacks(g, u) > i32::from(attacks);
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        x.attacks = attacks;
        if !(keeps || again) {
            x.moves = 0;
        }
    }
    let r = g.rules();
    let air = |g: &Game, x: UnitId| {
        g.unit(x).is_some_and(|x| r.base_units()[x.base].domain == Domain::Air)
    };
    let mut cands: Vec<UnitId> = g
        .state()
        .units()
        .iter()
        .filter(|x| x.owner() != owner && g.at_war(owner, x.owner()))
        .map(crate::state::units::Unit::id)
        .filter(|&x| can_intercept(g, x, t))
        .collect();
    if cands.iter().any(|&x| air(g, x)) {
        cands.retain(|&x| air(g, x));
    }
    let mut out = Map::new();
    if cands.is_empty() {
        out.insert("air_sweep".into(), json!("Nothing tried to intercept."));
        return Value::Object(out);
    }
    let mut rng = stream(g, Purpose::InterceptOrder, u64::from(u.get()), u64::from(t.0));
    rng.shuffle(&mut cands);
    let mut ranked: SmallVec<[(i32, UnitId); 8]> =
        cands.iter().map(|&x| (intercept_chance(g, x), x)).collect();
    ranked.sort_by_key(|&(chance, _)| core::cmp::Reverse(chance));
    let icpt = ranked[0].1;
    if let Some(x) = g.unit_mut(icpt, UnitTouch::CORE) {
        x.interceptions = x.interceptions.saturating_add(1);
    }
    let icpt_type = combatant::name(g, Combatant::Unit(icpt));
    if !air(g, icpt) {
        out.insert(
            "air_sweep".into(),
            json!(format!(
                "Ground interceptor {icpt_type} fired and missed; it is spent for this turn."
            )),
        );
        return Value::Object(out);
    }
    let (a, d) = (Combatant::Unit(u), Combatant::Unit(icpt));
    let icpt_owner = combatant::owner(g, d);
    let from = combatant::tile(g, a);
    let (dd, da) = blows(g, &mut rng, a, d, from, true);
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.activity = None;
    }
    add_xp(g, d, 5, owner);
    add_xp(g, a, 5, icpt_owner);
    out.insert("air_sweep".into(), json!(icpt_type));
    out.insert("damage_to_interceptor".into(), json!(dd));
    out.insert("damage_to_attacker".into(), json!(da));
    let sweeper = combatant::name(g, a);
    if g.unit(icpt).is_some_and(|x| x.hp <= 0) {
        let text =
            format!("{}'s {sweeper} shot down an intercepting {icpt_type}.", player_name(g, owner));
        kill_unit(g, icpt, Some(owner), &text);
        out.insert("interceptor_killed".into(), json!(true));
    }
    if g.unit(u).is_some_and(|x| x.hp <= 0) {
        let text =
            format!("{}'s {icpt_type} shot down a sweeping {sweeper}.", player_name(g, icpt_owner));
        kill_unit(g, u, Some(icpt_owner), &text);
        out.insert("attacker_killed".into(), json!(true));
    }
    Value::Object(out)
}

/// Where an aircraft would rebase to, checked (`combat.rebase`'s checks, `combat.py:1051-1068`):
/// it has not acted this turn, the tile is within twice its range, and a city of its owner's or
/// a carrier there has room; with the carrier it would board.
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_rebase(g: &Game, u: UnitId, t: TileIdx) -> Result<Option<UnitId>, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    if g.rules().base_units()[x.base].domain != Domain::Air {
        return Err(ActionError::rule("Only aircraft can rebase."));
    }
    if x.moves <= 0 || x.attacks > 0 {
        return Err(ActionError::rule("This aircraft has already acted this turn."));
    }
    let reach = health::attack_range(g, u).saturating_mul(2);
    if i64::from(g.grid().distance(x.tile(), t)) > i64::from(reach) {
        return Err(ActionError::rule(format!("Rebase range is {reach} tiles.")));
    }
    if let Some(why) = stack_reason(g, x.owner(), x.base, t, Some(u)) {
        return Err(ActionError::rule(why.text(g)));
    }
    if g.city_at(t).is_some() {
        return Ok(None);
    }
    Ok(g.units_at(t)
        .find(|o| o.owner() == x.owner() && can_carry(g, o.id(), x.base, Some(u)))
        .map(crate::state::units::Unit::id))
}

/// Moves an aircraft to a city or a carrier (`combat.rebase`, `combat.py:1069-1073`): it boards
/// the carrier, and has no movement left.
pub fn rebase(g: &mut Game, u: UnitId, t: TileIdx, carrier: Option<UnitId>) -> Value {
    let moved = g.relocate_unit(u, t);
    debug_assert!(moved.is_ok(), "a checked rebase lands: {moved:?}");
    if g.unit(u).is_some_and(|x| x.carried_by().is_some()) {
        let off = g.unboard_unit(u);
        debug_assert!(off.is_ok(), "a carried aircraft leaves its carrier: {off:?}");
    }
    if let Some(c) = carrier {
        let on = g.board_unit(u, c);
        debug_assert!(on.is_ok(), "a checked carrier takes the aircraft: {on:?}");
    }
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        x.moves = 0;
        x.acted = true;
    }
    let (x, y) = g.xy(t);
    let carrier_type = carrier.map(|c| combatant::name(g, Combatant::Unit(c)));
    json!({ "rebased_to": { "x": x, "y": y }, "carrier": carrier_type })
}
