//! Ancient ruins (`ruins.py`, UnCiv's `RuinsManager` and `RuinReward`): a major civilization's
//! unit that enters ruins clears them and finds a reward.
//!
//! [`enter`] ports `ruins.enter` (`ruins.py:38-67`): the rewards the civilization may find here
//! (not the last two it found, not one the game's difficulty excludes, and those whose
//! `Unavailable` and `Only available` allow it) are shuffled, drawn from `Purpose::Ruins` keyed by
//! the tile and the civilization, and the first that does anything is the one found. Movement
//! (package 1c-02) calls it when a unit steps onto ruins.

use super::derive::rev::PlayerTouch;
use super::{Game, triggers};
use crate::base::ids::{PlayerId, RuinId, TileIdx, UnitId};
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::trigger::TriggerSite;
use crate::unique::{Ctx, UniqueType, applies};

/// Whether civilization `p` may find reward `ruin` with unit `u` on tile `t`
/// (`ruins._possible`, `ruins.py:25-35`).
fn possible(g: &Game, p: PlayerId, u: UnitId, t: TileIdx, ruin: RuinId) -> bool {
    let r = g.rules();
    let def = &r.ruins()[ruin];
    let last = g.player(p).map(|x| x.civ.last_ruins).unwrap_or_default();
    if last.contains(&Some(ruin)) || def.excluded_difficulties.contains(&g.difficulty(None)) {
        return false;
    }
    let tt = r.uniques();
    let v = g.view();
    let ctx = Ctx { civ: Some(p), unit: Some(u), tile: Some(t), ..Ctx::default() }.resolve(&v);
    def.uniques.ids().all(|id| match tt.meta(id).ty {
        Some(UniqueType::Unavailable) => !applies(id, &ctx, &v),
        Some(UniqueType::OnlyAvailable) => applies(id, &ctx, &v),
        _ => true,
    })
}

/// A unit of civilization `p` explores the ruins on tile `t` (`ruins.enter`, `ruins.py:38-67`):
/// they are cleared, and the first reward in a keyed shuffle of those possible that does anything
/// is found, remembered, and announced. Whether one was found.
pub fn enter(g: &mut Game, u: UnitId, t: TileIdx) -> bool {
    let Some((p, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return false };
    if let Err(e) = g.set_improvement(t, None) {
        debug_assert!(false, "a tile of the map lost no ruins: {e}");
    }
    let r = g.rules();
    let mut candidates: Vec<RuinId> =
        r.ruins().ids().filter(|&ruin| possible(g, p, u, t, ruin)).collect();
    Rng::keyed(g.state().seed(), Purpose::Ruins, &[t.key(), p.key()]).shuffle(&mut candidates);
    let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let what = r.base_units()[base].name.to_string();
    for ruin in candidates {
        let def = &r.ruins()[ruin];
        let note = format!("from the ruins ({})", def.name);
        let mut found = false;
        for &id in def.uniques.on_gain.iter() {
            let unit = Some(u).filter(|&x| g.unit(x).is_some());
            let site = TriggerSite { civ: p, city: None, unit, tile: Some(t) };
            let holds = {
                let v = g.view();
                let ctx = Ctx { civ: Some(p), unit, tile: Some(t), ..Ctx::default() }.resolve(&v);
                applies(id, &ctx, &v)
            };
            if holds && triggers::apply(g, id, &site, Some(&note)) {
                found = true;
            }
        }
        if found {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                x.civ.last_ruins = [x.civ.last_ruins[1], Some(ruin)];
            }
            let data = EventData { reward: Some(ruin), ..EventData::default() };
            let text = format!("{who}'s {what} explored ancient ruins and found {}.", def.name);
            g.emit(EngineEvent::Ruins, &text, Some(PlayerSet::single(p)), Some(t), data, &[]);
            return true;
        }
    }
    let text = format!("{who}'s {what} explored ancient ruins but found nothing of value.");
    g.emit(
        EngineEvent::Ruins,
        &text,
        Some(PlayerSet::single(p)),
        Some(t),
        EventData::default(),
        &[],
    );
    false
}
