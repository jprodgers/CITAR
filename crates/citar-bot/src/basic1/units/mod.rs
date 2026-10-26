//! The units (`manage_units`, basic.py:1771-1805): every unit of the seat its orders for the
//! turn, ranged units first, then by id, each by what it is, and a promotion taken after each.
//!
//! Package 2-01b ported the order, the dispatch and the units of the economy: settlers
//! (`settlers.rs`), workers and work boats (`workers.rs`) and scouts ([`scouts`]). Package 2-03
//! ports the rest:
//! - the garrisons, which the bot keeps in `memory.garrisons` (basic.py:1774-1775) for the
//!   production advisor to read as a `BotFact`;
//! - [`special`]: great people, religious units and spaceship parts (`handle_special`,
//!   1951-2052);
//! - [`air`] and [`naval`]: aircraft and ships (2065-2094);
//! - [`attack`]: the best attack in reach (`_attack_best`, 2096-2139);
//! - [`military`]: the army's orders (`handle_military`, 2141-2289);
//! - [`war_plan`]: the city a war is fought for and where the army gathers (2291-2358, 2389);
//! - [`promote`]: promotions (1806-1822).
//!
//! A unit that is gone, or a turn that is no longer the seat's, is skipped, as Python skipped
//! them; a refused order skips that unit's next step and never the rest of the turn (P2.3.9,
//! fix 9: Python caught an `ActionError` per unit).
//!
//! The context holds ids, read back from the game when a decision needs a unit or a city: an
//! enemy killed earlier in the turn no longer counts as a danger or a target, nor one of the
//! seat's units killed as part of its army, where Python's lists still held them.

pub(crate) mod air;
pub(crate) mod attack;
pub(crate) mod military;
pub(crate) mod naval;
pub(crate) mod promote;
pub(crate) mod scouts;
pub(crate) mod special;
pub(crate) mod war_plan;

use std::collections::BTreeMap;

use citar_engine::base::ids::{BaseUnitId, CityId, PlayerId, TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::cities::borders::within_order;
use citar_engine::game::units::type_has;
use citar_engine::rules::defs::Domain;
use citar_engine::unique::UniqueType;

use super::context::{Context, needs_garrison};
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
pub(crate) fn role(g: &Game, base: BaseUnitId) -> Role {
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

/// `manage_units` (basic.py:1771-1805): the garrisons of the turn, then the units in their
/// order, each dispatched by its role and then promoted.
pub(crate) fn manage_units(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    s.memory.garrisons = garrisons(t.game(), pid, ctx);
    war_plan::forget_stale(t.game(), pid, s, ctx);
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
            Role::Special => special::handle_special(t, s, ctx, u),
            Role::Scout => scouts::handle_scout(t, s, ctx, u),
            Role::Air => air::handle_air(t, u),
            Role::Naval => naval::handle_naval(t, s, u),
            Role::Military => military::handle_military(t, s, ctx, u),
            Role::Other => {}
        }
        if t.game().unit(u).is_some() {
            promote::promote(t, s, u);
        }
    }
}

/// The garrisons of the turn (`_garrisons`, basic.py:1774-1775): each city that wants one
/// (`needs_garrison`) and holds a land military unit of the seat's, by that unit.
fn garrisons(g: &Game, pid: PlayerId, ctx: &Context) -> BTreeMap<CityId, UnitId> {
    let r = g.rules();
    ctx.cities
        .iter()
        .filter(|&&c| needs_garrison(ctx, c))
        .filter_map(|&c| {
            let m = g.military_at(g.city(c)?.tile())?;
            let land = r.base_units()[m.base].domain == Domain::Land;
            (m.owner() == pid && land).then_some((c, m.id()))
        })
        .collect()
}

/// Whether unit `u` garrisons a city (`_is_garrison`, basic.py:1824-1826).
pub(crate) fn is_garrison(s: &Seat<'_>, u: UnitId) -> bool {
    s.memory.garrisons.values().any(|&x| x == u)
}

/// The tiles within `radius` of `at`, `at` first, in Python's order (`hexmap.within`): the
/// bot takes the first of equals in that order, as Python's loops and `min` did.
pub(crate) fn py_within(g: &Game, at: TileIdx, radius: u32) -> Vec<TileIdx> {
    let mut v = g.grid().within(at, radius);
    v.sort_by_cached_key(|&n| within_order(g, at, n));
    v
}

/// The tiles at exactly `radius` from `at` in Python's order (`hexmap.ring`).
pub(crate) fn py_ring(g: &Game, at: TileIdx, radius: u32) -> Vec<TileIdx> {
    let grid = g.grid();
    let mut v = py_within(g, at, radius);
    v.retain(|&n| grid.distance(n, at) == radius);
    v
}

/// A non-negative parameter as a radius.
pub(crate) fn radius(p: i32) -> u32 {
    u32::try_from(p.max(0)).unwrap_or(0)
}

/// Whether distance `d` is within `p` tiles (a parameter, which may be negative).
pub(crate) fn within(d: u32, p: i32) -> bool {
    i64::from(d) <= i64::from(p)
}

/// The first of `tiles` nearest to `from` (Python's `min` by distance).
pub(crate) fn nearest(
    g: &Game,
    tiles: impl IntoIterator<Item = TileIdx>,
    from: TileIdx,
) -> Option<TileIdx> {
    let grid = g.grid();
    let mut best: Option<(TileIdx, u32)> = None;
    for t in tiles {
        let d = grid.distance(t, from);
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((t, d));
        }
    }
    best.map(|(t, _)| t)
}

/// The tile of the first of `cities` nearest to `from`.
pub(crate) fn nearest_city(g: &Game, cities: &[CityId], from: TileIdx) -> Option<TileIdx> {
    nearest(g, cities.iter().filter_map(|&c| g.city(c)).map(|c| c.tile()), from)
}
