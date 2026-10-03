//! The units (`manage_units`, basic.py:1771-1805): every unit of the seat its orders for the
//! turn, ranged units first, then by id, each by what it is.
//!
//! Package 2-01b ports the order, the dispatch and the units of the economy: settlers
//! (`settlers.rs`), workers and work boats (`workers.rs`) and scouts ([`scouts`]). Great people,
//! religious units and spaceship parts (`handle_special`), aircraft, ships and the army, the
//! garrisons kept in memory and promotions are package 2-03's; until then those units stand.
//!
//! A unit that is gone, or a turn that is no longer the seat's, is skipped, as Python skipped
//! them; a refused order skips that unit's next step and never the rest of the turn (P2.3.9,
//! fix 9: Python caught an `ActionError` per unit).

pub(crate) mod scouts;

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::units::type_has;
use citar_engine::rules::defs::Domain;
use citar_engine::unique::UniqueType;

use super::context::Context;
use super::{Seat, settlers, workers};
use crate::driver::Turn;

/// What a unit is to `manage_units`' dispatch (basic.py:1782-1799), in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Settler,
    Worker,
    WorkBoat,
    /// A great person, a religious unit or what the capital adds (`handle_special`).
    Special,
    Scout,
    Air,
    Naval,
    Military,
    /// Nothing the bot orders.
    Other,
}

/// What base unit `u` is to the dispatch.
pub(crate) fn role(g: &Game, base: citar_engine::base::ids::BaseUnitId) -> Role {
    let r = g.rules();
    let a = &r.derived().advisor;
    let d = &r.base_units()[base];
    if a.founders.contains(base) {
        Role::Settler
    } else if a.workers.contains(base) {
        Role::Worker
    } else if a.boats.contains(base) {
        Role::WorkBoat
    } else if d.great_person
        || type_has(g, base, UniqueType::CanSpreadReligion)
        || type_has(g, base, UniqueType::AddInCapital)
        || type_has(g, base, UniqueType::CanRemoveHeresy)
    {
        Role::Special
    } else if a.scout == Some(d.unit_type) {
        Role::Scout
    } else if d.domain == Domain::Air {
        Role::Air
    } else if d.domain == Domain::Water {
        Role::Naval
    } else if d.military {
        Role::Military
    } else {
        Role::Other
    }
}

/// `manage_units` (basic.py:1771-1805): the units in their order, each dispatched by its role.
pub(crate) fn manage_units(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    let g = t.game();
    let mut order: Vec<(bool, UnitId)> =
        g.player_units(pid).map(|u| (!g.rules().base_units()[u.base].ranged, u.id())).collect();
    order.sort();
    // The expansion sites, asked of the advisor once, the first time a settler needs them.
    let mut sites: Option<Vec<TileIdx>> = None;
    for (_, u) in order {
        let g = t.game();
        let Some(base) = g.unit(u).map(|x| x.base) else { continue };
        if g.current() != pid {
            continue;
        }
        match role(g, base) {
            Role::Settler => settlers::handle_settler(t, s, ctx, u, &mut sites),
            Role::Worker => workers::handle_worker(t, u),
            Role::WorkBoat => workers::handle_work_boat(t, s, u),
            Role::Scout => scouts::handle_scout(t, s, ctx, u),
            // Package 2-03: special units, aircraft, ships, the army and promotions.
            Role::Special | Role::Air | Role::Naval | Role::Military | Role::Other => {}
        }
    }
}
