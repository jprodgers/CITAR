//! Ships (`handle_naval`, basic.py:2085-2094): attack the best target in reach; a ship below
//! `naval_heal_hp` heals; otherwise it sleeps.

use citar_engine::base::ids::UnitId;
use citar_engine::state::units::Activity;

use super::attack::attack_best;
use crate::basic1::Seat;
use crate::basic1::workers::order;
use crate::driver::Turn;

/// `handle_naval` (basic.py:2085-2094).
pub(crate) fn handle_naval(t: &mut Turn<'_>, s: &Seat<'_>, u: UnitId) {
    if attack_best(t, s, u) {
        return;
    }
    let Some((hp, activity)) = t.game().unit(u).map(|x| (x.hp, x.activity)) else { return };
    if i32::from(hp) < s.params.naval_heal_hp {
        if activity != Some(Activity::Heal) {
            order(t, u, "heal");
        }
        return;
    }
    if activity.is_none() {
        order(t, u, "sleep");
    }
}
