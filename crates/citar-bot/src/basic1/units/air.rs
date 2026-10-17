//! Aircraft (`handle_air`, basic.py:2065-2083): strike the first enemy city or military unit in
//! range, in Python's `within` order, else sleep where based.
//!
//! The strike is checked before it is asked for (`combat::actions::plan_attack`, the check the
//! `attack` tool makes), where Python asked for each enemy tile in turn until one was accepted:
//! the same tile is struck, and the refusals before it are not made.

use citar_engine::base::ids::UnitId;
use citar_engine::game::Action;
use citar_engine::game::combat::actions::{Attack, plan_attack};
use citar_engine::game::units::health::attack_range;
use citar_engine::state::units::Activity;

use super::{py_within, radius};
use crate::basic1::workers::order;
use crate::driver::Turn;

/// `handle_air` (basic.py:2065-2083).
pub(crate) fn handle_air(t: &mut Turn<'_>, u: UnitId) {
    let pid = t.pid();
    let g = t.game();
    let Some(x) = g.unit(u) else { return };
    let at = x.tile();
    let target = py_within(g, at, radius(attack_range(g, u))).into_iter().skip(1).find(|&n| {
        let owner = g.city_at(n).map(|c| c.owner()).or_else(|| g.military_at(n).map(|m| m.owner()));
        owner.is_some_and(|o| g.at_war(pid, o)) && plan_attack(g, u, n).is_ok()
    });
    if let Some(n) = target {
        let (x, y) = g.xy(n);
        let strike = Attack { unit_id: i64::from(u.get()), x: i64::from(x), y: i64::from(y) };
        if t.act(Action::Attack(strike)).is_some() {
            return;
        }
    }
    if t.game().unit(u).is_some_and(|x| x.activity != Some(Activity::Sleep)) {
        order(t, u, "sleep");
    }
}
