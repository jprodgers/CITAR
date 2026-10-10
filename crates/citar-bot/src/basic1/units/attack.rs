//! The best attack in reach (`_attack_best`, basic.py:2096-2139): predicted damage decides, and a
//! unit that would lose the exchange holds instead.
//!
//! Every tile in reach (the unit's range for a ranged unit, else its neighbours) is previewed
//! with the engine's typed preview (`combat::resolve::preview_of`, which refuses what the attack
//! would refuse). A city whose defences are down is taken at once by a melee unit and left alone
//! by a ranged one. Otherwise an attack is worth the damage it deals, a siege unit's against a
//! city `atk_siege_city_mult` times, a kill's `atk_kill_mult` times, less the damage it takes by
//! `atk_city_loss_weight` or `atk_unit_loss_weight`; the city a war plan advances on is worth
//! `siege_city_pref` times more; a melee unit that would be left under `atk_city_min_hp` or
//! `atk_unit_min_hp` holds. The best attack worth more than nothing is made, the first of equals
//! in Python's `within` order.

use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::combat::actions::Attack;
use citar_engine::game::combat::resolve::{can_attack_now, preview_of};
use citar_engine::game::units::health::attack_range;
use citar_engine::game::{Action, Game};
use citar_engine::unique::Combatant;

use super::py_within;
use crate::basic1::Seat;
use crate::driver::Turn;

/// `_attack_best` (basic.py:2096-2139): attacks the best target in reach, if one is worth it;
/// whether the attack was made.
pub(crate) fn attack_best(t: &mut Turn<'_>, s: &Seat<'_>, u: UnitId) -> bool {
    let Some(target) = best_target(t.game(), t.pid(), s, u) else { return false };
    let (x, y) = t.game().xy(target);
    t.act(Action::Attack(Attack { unit_id: i64::from(u.get()), x: i64::from(x), y: i64::from(y) }))
        .is_some()
}

/// The tile `_attack_best` would attack, if any: what [`attack_best`] attacks, and what the
/// reference checks ask (`decisions`).
pub(crate) fn best_target(g: &Game, pid: PlayerId, s: &Seat<'_>, u: UnitId) -> Option<TileIdx> {
    let p = s.params;
    let x = g.unit(u)?;
    if x.owner() != pid || x.moves <= 0 || can_attack_now(g, u).is_some() {
        return None;
    }
    let r = g.rules();
    let d = &r.base_units()[x.base];
    let siege = r.derived().advisor.siege == Some(d.unit_type);
    let siege_city = s.memory.war_plan.as_ref().filter(|w| w.advance).map(|w| w.city);
    let reach = if d.ranged { attack_range(g, u) } else { 1 };
    let hp = f64::from(x.hp);
    let mut best: Option<TileIdx> = None;
    let mut best_v = 0.0;
    for at in py_within(g, x.tile(), super::radius(reach)).into_iter().skip(1) {
        // Nothing there, nothing to preview: the preview would refuse it.
        if g.city_at(at).is_none() && g.units_at(at).next().is_none() {
            continue;
        }
        let Ok(pv) = preview_of(g, u, at) else { continue };
        let dd = f64::from(pv.damage_to_defender[0] + pv.damage_to_defender[1]) / 2.0;
        let da = f64::from(pv.damage_to_attacker[0] + pv.damage_to_attacker[1]) / 2.0;
        let v = if matches!(pv.setup.defender, Combatant::City(_)) {
            if pv.city_down {
                // Taken by a melee attack: nothing beats it (Python's value of 10,000).
                if !d.ranged {
                    return Some(at);
                }
                continue;
            }
            let mult = if siege { p.atk_siege_city_mult } else { 1.0 };
            let mut v = dd * mult - da * p.atk_city_loss_weight;
            if siege_city == Some(at) {
                v *= p.siege_city_pref;
            }
            if !d.ranged && hp - da < f64::from(p.atk_city_min_hp) {
                continue;
            }
            v
        } else {
            let kill = dd >= f64::from(pv.defender_hp);
            let v = dd * if kill { p.atk_kill_mult } else { 1.0 } - da * p.atk_unit_loss_weight;
            if !d.ranged && hp - da < f64::from(p.atk_unit_min_hp) {
                continue;
            }
            v
        };
        if v > best_v {
            best = Some(at);
            best_v = v;
        }
    }
    best
}
