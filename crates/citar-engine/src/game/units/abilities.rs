//! Limited-use unit actions (`units.py:403-482`, UnCiv's `UnitActionModifiers`): how many uses of
//! an action a unit has left, which of its action uniques it can use now, spending a use, and a
//! unit used up by its action.
//!
//! An action unique's modifiers are compiled ([`crate::unique::table::ActionMods`]): `<once>`,
//! `<[n] times>`, `<[n] additional time(s)>` (more uses of the same action from another unique,
//! a promotion's), `<by consuming this unit>`, `<after which this unit is consumed>` and `<for [n]
//! movement>`. Uses are counted under the action's ability key, Python's `ph|params`.

use crate::base::ids::{UniqueId, UnitId};
use crate::game::derive::rev::UnitTouch;
use crate::game::{Game, triggers};
use crate::unique::filter::UnitFacts;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueType, applies, uq};

/// How many uses of the action unique `id` a unit has left (`units._usages_left`,
/// `units.py:403-418`), or `None` if its uses are not limited: its own count, with every
/// `[n] additional time(s)` of the unit's uniques of the same type, less those it used.
#[must_use]
pub fn usages_left(g: &Game, u: UnitId, id: UniqueId) -> Option<i32> {
    let t = g.rules().uniques();
    let meta = t.meta(id);
    let total = i32::from(meta.actions.uses()?);
    let ty = meta.ty?;
    let v = g.view();
    let extra: i32 = uq::unit(&v, u, ty, &Ctx::IGNORE)
        .filter_map(|h| t.meta(h.id).actions.extra_times)
        .map(i32::from)
        .sum();
    let used = match (meta.ability, g.unit(u)) {
        (Some(k), Some(x)) => i32::from(x.ability_uses(k)),
        _ => 0,
    };
    Some(total.saturating_add(extra).saturating_sub(used))
}

/// The unit's action uniques of type `ty` it can use now, in order
/// (`units.usable_action_uniques`, `units.py:440-451`): not an extra-uses modifier, conditionals
/// holding, uses left.
#[must_use]
pub fn usable_action_uniques(g: &Game, u: UnitId, ty: UniqueType) -> Vec<UniqueId> {
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    uq::unit(&v, u, ty, &Ctx::IGNORE)
        .map(|h| h.id)
        .filter(|&id| t.meta(id).actions.extra_times.is_none() && applies(id, &ctx, &v))
        .filter(|&id| usages_left(g, u, id).is_none_or(|n| n > 0))
        .collect()
}

/// The unit's first action unique of type `ty` it can use now (`units.usable_action`,
/// `units.py:421-437`): as [`usable_action_uniques`], and it needs movement left unless the
/// action consumes the unit.
#[must_use]
pub fn usable_action(g: &Game, u: UnitId, ty: UniqueType) -> Option<UniqueId> {
    let t = g.rules().uniques();
    let moves = g.unit(u)?.moves;
    usable_action_uniques(g, u, ty).into_iter().find(|&id| moves > 0 || t.meta(id).actions.consume)
}

/// Spends one use of an action (`units.consume_action`, `units.py:454-474`): its movement (1
/// point, or `<for [n] movement>`); then the unit is used up if the action consumes it, or if
/// this was the last of its uses and it is consumed after; otherwise the use is counted.
pub fn consume_action(g: &mut Game, u: UnitId, id: UniqueId) {
    let t = g.rules().uniques();
    let meta = *t.meta(id);
    let sc = g.rules().constants().move_scale;
    let cost = meta.actions.movement.unwrap_or(1).saturating_mul(sc);
    if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
        x.moves = x.moves.saturating_sub(cost).max(0);
    }
    if meta.actions.consume {
        consume(g, u);
        return;
    }
    if meta.actions.uses().is_some() {
        if usages_left(g, u, id) == Some(1) && meta.actions.consumed_after {
            consume(g, u);
            return;
        }
        if let (Some(k), Some(x)) = (meta.ability, g.unit_mut(u, UnitTouch::CORE)) {
            x.use_ability(k);
        }
    }
}

/// Uses a unit up (`units.consume`, `units.py:477-482`): `upon expending a [unit]` fires for its
/// owner, then it leaves the game.
pub fn consume(g: &mut Game, u: UnitId) {
    let Some(owner) = g.unit(u).map(crate::state::units::Unit::owner) else { return };
    let facts = UnitFacts::of(&g.view(), u);
    let site = TriggerSite::civ(owner);
    triggers::fire(g, &site, &TriggerEvent::ExpendingUnit(facts), false);
    if g.unit(u).is_some() {
        let _removed = g.despawn_unit(u);
    }
}
