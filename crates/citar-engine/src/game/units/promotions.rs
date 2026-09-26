//! Experience and promotions (`units.py:186-282`): what the next promotion costs, which a unit
//! may take, taking one with what follows from it, and combat experience.

use crate::base::ids::{PromotionId, UnitId};
use crate::base::text::echo;
use crate::game::derive::rev::UnitTouch;
use crate::game::error::ActionError;
use crate::game::lookup::with_list;
use crate::game::{Game, triggers};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::trigger::{OneTimeEffect, TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, applies, uq};

/// The experience a unit needs for its next promotion (`units.xp_for_next`, `units.py:186-191`):
/// ten for each promotion it will then have bought, times its owner's `[n]% XP required for
/// promotions`.
#[must_use]
pub fn xp_for_next(g: &Game, u: UnitId) -> i32 {
    let Some(x) = g.unit(u) else { return 0 };
    let v = g.view();
    let owner = x.owner();
    let mut m = 1.0f64;
    for h in uq::civ(&v, owner, UniqueType::XPForPromotionModifier, &Ctx::civ(owner)) {
        if let UniqueData::XPForPromotionModifier(y) = h.data() {
            for _ in 0..h.n {
                m *= 1.0 + f64::from(y.percent) / 100.0;
            }
        }
    }
    let raw = f64::from((i32::from(x.promotion_count) + 1) * 10) * m;
    #[allow(clippy::cast_possible_truncation, reason = "Python's int(): toward zero, in range")]
    let n = raw as i32;
    n
}

/// Whether a promotion is free: `This Promotion is free`, whatever its conditionals.
fn is_free(g: &Game, pr: PromotionId) -> bool {
    let r = g.rules();
    crate::game::core::has_type(r, &r.promotions()[pr].uniques, UniqueType::FreePromotion)
}

/// The promotions a unit could take now (`units.available_promotions`, `units.py:194-210`), in
/// the ruleset's order: those its type may take, it has not, whose prerequisites it has one of,
/// that are not `Unavailable` and whose `Only available` conditions hold for it.
#[must_use]
pub fn available_promotions(g: &Game, u: UnitId) -> Vec<PromotionId> {
    let Some(x) = g.unit(u) else { return Vec::new() };
    let r = g.rules();
    let t = r.uniques();
    let unit_type = r.base_units()[x.base].unit_type;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    r.promotions()
        .iter()
        .filter(|&(id, pr)| {
            if x.promotions.contains(id) || !pr.unit_types.contains(&unit_type) {
                return false;
            }
            if !pr.prerequisites.is_empty()
                && !pr.prerequisites.iter().any(|&p| x.promotions.contains(p))
            {
                return false;
            }
            if uq::any(uq::object(&v, &pr.uniques, UniqueType::Unavailable, &ctx)) {
                return false;
            }
            pr.uniques
                .ids()
                .all(|id| t.meta(id).ty != Some(UniqueType::OnlyAvailable) || applies(id, &ctx, &v))
        })
        .map(|(id, _)| id)
        .collect()
}

/// Whether a unit has the experience, or a free pick, for a promotion it could take
/// (`units.can_promote`, `units.py:213-220`); a free promotion needs neither.
#[must_use]
pub fn can_promote(g: &Game, u: UnitId) -> bool {
    let Some(x) = g.unit(u) else { return false };
    let av = available_promotions(g, u);
    if av.is_empty() {
        return false;
    }
    if x.xp >= xp_for_next(g, u) || x.pending_promotions > 0 {
        return true;
    }
    av.iter().any(|&p| is_free(g, p))
}

/// Gives a unit a promotion with everything that follows (`units.add_promotion`,
/// `units.py:223-246`). Bought (`free` false), it spends a free pick or the experience of the
/// next promotion, unless the promotion is free, and fires `upon being promoted`. A promotion
/// that `consume[s] this opportunity` (Heal Instantly) is not kept. Last, what the promotion
/// does once happens to the unit (`[This Unit] heals [50] HP`). Nothing for one it has.
pub fn add_promotion(g: &mut Game, u: UnitId, pr: PromotionId, free: bool) {
    let r = g.rules();
    let Some(def) = r.promotions().get(pr) else { return };
    let Some(owner) = g.unit(u).filter(|x| !x.promotions.contains(pr)).map(|x| x.owner()) else {
        return;
    };
    if !free {
        if !is_free(g, pr) {
            let cost = xp_for_next(g, u);
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                if x.pending_promotions > 0 {
                    x.pending_promotions -= 1;
                } else {
                    // refcheck: promotion-needs-its-experience (never below zero)
                    x.xp = x.xp.saturating_sub(cost).max(0);
                    x.promotion_count = x.promotion_count.saturating_add(1);
                }
            }
        }
        let site = TriggerSite { civ: owner, city: None, unit: Some(u), tile: None };
        triggers::fire(g, &site, &TriggerEvent::Promotion, true, None);
        if g.unit(u).is_none() {
            return;
        }
    }
    if !crate::game::core::has_type(r, &def.uniques, UniqueType::SkipPromotion)
        && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
    {
        x.promotions.insert(pr);
    }
    // What the promotion does once, where its conditionals hold for the unit (`units.py:240-245`).
    let t = r.uniques();
    let once: Vec<_> = {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        def.uniques
            .ids()
            .filter(|&id| {
                t.meta(id).trigger.is_none()
                    && OneTimeEffect::decode(r, id).is_some()
                    && applies(id, &ctx, &v)
            })
            .collect()
    };
    let site = TriggerSite { civ: owner, city: None, unit: Some(u), tile: None };
    for id in once {
        if g.unit(u).is_none() {
            break;
        }
        triggers::apply(g, id, &site, None);
    }
}

/// Resolves and checks a promotion a player picks (`units.promote`, `units.py:249-258`, with
/// the tool's resolving of the name, `tools.py:626`): the promotion, or Python's refusal.
pub fn plan_promotion(g: &Game, u: UnitId, name: &str) -> Result<PromotionId, ActionError> {
    let r = g.rules();
    let found = r.resolve::<PromotionId>(name);
    let av = available_promotions(g, u);
    let Some(pr) = found.filter(|p| av.contains(p)) else {
        let shown = found.and_then(|p| r.name(p)).unwrap_or(name);
        let names: Vec<String> = av.iter().filter_map(|&p| r.name(p)).map(str::to_owned).collect();
        // refcheck: refusals-end-as-sentences
        let head = format!("{} is not available for this unit. Available: ", echo(shown));
        return Err(ActionError::rule(with_list(&head, &names, ".", "get_unit")));
    };
    // A paid promotion needs the experience or a free pick of its own: Python let any promotion
    // through while a free one was available, and took the experience the unit did not have.
    // refcheck: promotion-needs-its-experience
    let (xp, picks) = g.unit(u).map_or((0, 0), |x| (x.xp, x.pending_promotions));
    let next = xp_for_next(g, u);
    if !(is_free(g, pr) || xp >= next || picks > 0) {
        return Err(ActionError::rule(format!("Not enough XP ({xp}/{next}).")));
    }
    Ok(pr)
}

/// Combat experience (`units.add_xp`, `units.py:261-282`, `Battle.addXp`): the amount times the
/// unit's `[n]% XP gained from combat`, capped against the barbarians; it feeds great general
/// points, and a unit that can newly be promoted is announced.
pub fn add_xp(g: &mut Game, u: UnitId, amount: i32, vs_barbarian: bool) {
    if amount <= 0 {
        return;
    }
    let Some((owner, base, xp, count)) =
        g.unit(u).map(|x| (x.owner(), x.base, x.xp, x.promotion_count))
    else {
        return;
    };
    let mut m = 1.0f64;
    {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        for h in uq::unit_and_civ(&v, u, UniqueType::PercentageXPGain, &ctx) {
            if let UniqueData::PercentageXPGain(x) = h.data() {
                for _ in 0..h.n {
                    m += f64::from(x.percent) / 100.0;
                }
            }
        }
    }
    #[allow(clippy::cast_possible_truncation, reason = "Python's int(): toward zero, in range")]
    let mut gain = (f64::from(amount) * m) as i32;
    if vs_barbarian {
        let cap = g.rules().constants().formulas.max_xp_from_barbarians;
        let c = i32::from(count);
        let prior = xp.saturating_add(c * (c + 1) * 5);
        gain = gain.min(cap.saturating_sub(prior)).max(0);
    }
    if gain <= 0 {
        return;
    }
    let before = can_promote(g, u);
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.xp = x.xp.saturating_add(gain);
    }
    if g.player(owner).is_some_and(crate::state::players::Player::is_major) && !vs_barbarian {
        // Not capped against the barbarians, so the gain is the whole amount (`units.py:279-280`).
        crate::game::great_people::add_combat_points(g, owner, base, gain);
    }
    if !before && can_promote(g, u) {
        let name = unit_label(g, u);
        let tile = g.unit(u).map(crate::state::units::Unit::tile);
        let data = EventData { unit: Some(u), ..EventData::default() };
        g.emit(
            EngineEvent::PromotionReady,
            &format!("{name} can be promoted."),
            Some(core::iter::once(owner).collect()),
            tile,
            data,
            &[],
        );
    }
}

/// A unit as Python named it in messages: `Warrior #12`.
#[must_use]
pub fn unit_label(g: &Game, u: UnitId) -> String {
    let base = g.unit(u).map(|x| x.base);
    let name = base.and_then(|b| g.rules().name(b)).unwrap_or("Unit");
    format!("{name} #{}", u.get())
}

/// The promotions a unit has, by name, in the ruleset's order.
#[must_use]
pub fn promotion_names(g: &Game, u: UnitId) -> Vec<String> {
    let r = g.rules();
    g.unit(u)
        .map(|x| x.promotions.iter().filter_map(|p| r.name(p)).map(str::to_owned).collect())
        .unwrap_or_default()
}
