//! Workers and work boats (`manage_units`' workers, basic.py:1784-1786, and `handle_work_boat`,
//! 1927-1949): a worker is automated, and the engine's automation (`game::automation`) chooses
//! its work; a work boat makes for the nearest sea resource its civilization owns and has not
//! improved, improves it, and is disbanded when there is none in reach.
//!
//! An action a unit could take is read by its kind (`ActionKind::Create`), where Python matched
//! its id's text (`create:`).

use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::actions::{ActionKind, UnitAction, unit_actions};
use citar_engine::game::cities::borders::within_order;
use citar_engine::game::units::actions::UnitOrder;
use citar_engine::game::{Action, Game};
use citar_engine::state::units::Activity;

use super::Seat;
use super::settlers::move_to;
use crate::driver::Turn;

/// A worker is automated unless it is automated or building already (basic.py:1784-1786).
pub(crate) fn handle_worker(t: &mut Turn<'_>, u: UnitId) {
    let busy = t
        .game()
        .unit(u)
        .is_some_and(|x| matches!(x.activity, Some(Activity::Automate | Activity::Build)));
    if !busy {
        order(t, u, "automate");
    }
}

/// `handle_work_boat` (basic.py:1927-1949).
pub(crate) fn handle_work_boat(t: &mut Turn<'_>, s: &Seat<'_>, u: UnitId) {
    if create(t, u) {
        return;
    }
    let pid = t.pid();
    let g = t.game();
    let Some(at) = g.unit(u).map(citar_engine::state::units::Unit::tile) else { return };
    let radius = u32::try_from(s.params.boat_seek_radius.max(0)).unwrap_or(0);
    let Some(best) = nearest_sea_resource(g, pid, at, radius) else {
        order(t, u, "disband");
        return;
    };
    move_to(t, u, best);
    if t.game().unit(u).is_some_and(|x| x.tile() == best) {
        create(t, u);
    }
}

/// The first tile within `radius` of `at`, in Python's `within` order, holding a sea resource
/// `pid` owns, sees and has not improved.
fn nearest_sea_resource(g: &Game, pid: PlayerId, at: TileIdx, radius: u32) -> Option<TileIdx> {
    let mut near = g.grid().within(at, radius);
    near.sort_by_cached_key(|&n| within_order(g, at, n));
    near.into_iter().find(|&n| {
        g.tile(n).is_some_and(|tile| {
            tile.owner() == Some(pid)
                && tile.improvement().is_none()
                && g.is_water(n)
                && tile
                    .resource()
                    .is_some_and(|r| g.has_tech(pid, g.rules().resources()[r].revealed_by))
        })
    })
}

/// Takes the first improvement the unit can make where it stands: whether there was one.
fn create(t: &mut Turn<'_>, u: UnitId) -> bool {
    let Some(id) = unit_actions(t.game(), u)
        .into_iter()
        .find(|a| matches!(a.kind, ActionKind::Create(_)) && a.available())
        .map(|a| a.id)
    else {
        return false;
    };
    t.act(Action::UnitAction(UnitAction {
        unit_id: i64::from(u.get()),
        action: id,
        name: None,
        beliefs: None,
        x: None,
        y: None,
    }));
    true
}

/// Gives unit `u` a standing order (`unit_order`); what it did, or `None` when refused.
pub(crate) fn order(t: &mut Turn<'_>, u: UnitId, what: &str) -> Option<serde_json::Value> {
    t.act(Action::UnitOrder(UnitOrder { unit_id: i64::from(u.get()), order: what.to_owned() }))
}
