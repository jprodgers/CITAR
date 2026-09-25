//! Firing triggers and applying one-time effects (`triggers.py:38-374`).
//!
//! [`fire`] finds the uniques that fire for an event at a site (`unique::trigger::fire`, the
//! matching half of `triggers.fire`, `triggers.py:38-65`) and applies each. Package 1c-02 applies
//! the effects on the unit in context (`triggers.py:339-367`: heal, damage, experience, upgrades,
//! promotions, movement, destruction), which promotions and the unit triggers need; the rest of
//! `triggers.trigger` (`triggers.py:75-338`) is package 1b-08's `apply_one_time`.

use super::units;
use super::{Game, Porting, pending};
use crate::base::ids::UniqueId;
use crate::unique::trigger::{OneTimeEffect, TriggerEvent, TriggerSite};

/// Fires the uniques that wait for `event` at `site` (`triggers.fire`): the civilization's, the
/// city's local ones and, with `include_unit`, the unit's, each applied in turn. `note` is what
/// caused them, for their announcements (`due to expending our Great Prophet`).
pub fn fire(
    g: &mut Game,
    site: &TriggerSite,
    event: &TriggerEvent,
    include_unit: bool,
    note: Option<&str>,
) {
    let found = {
        let v = g.view();
        crate::unique::trigger::fire(&v, site, event, include_unit)
    };
    for id in found {
        apply(g, id, site, note);
    }
}

/// Applies the one-time effect of the unique `id` at `site` (`triggers.trigger`); `note` is what
/// its announcement says caused it. Whether anything happened.
pub fn apply(g: &mut Game, id: UniqueId, site: &TriggerSite, note: Option<&str>) -> bool {
    match OneTimeEffect::decode(g.rules(), id) {
        Some(OneTimeEffect::Unit(e)) => {
            site.unit.is_some_and(|u| units::health::apply_unit_effect(g, u, e, note))
        }
        Some(_) => {
            // Everything else a one-time unique does (`triggers.py:75-338`).
            pending(Porting::Pending("1b-08"));
            false
        }
        None => false,
    }
}
