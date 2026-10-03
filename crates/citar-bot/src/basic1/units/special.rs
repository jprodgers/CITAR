//! Great people, religious units and spaceship parts (`handle_special` and
//! `_clear_civilian_slot`, basic.py:1951-2052): each uses the best action it has, in this order.
//! - A spaceship part is added in the capital, walking there first, and a civilian of the seat's
//!   parked in the capital (a tile holds one) steps aside for it.
//! - A great prophet founds a religion in one of the seat's cities with the bot's beliefs
//!   (`faith::choose_beliefs`), walking to the nearest city with no other civilian first; or
//!   enhances the religion, walking into a city first.
//! - A great scientist hurries research, a great merchant goes on a trade mission, a great
//!   engineer hurries what its city builds, a great writer writes a political treatise.
//! - A unit with an ability that triggers once (a great artist's golden age) uses it.
//! - A missionary with a religion spreads it where the majority follows another, else walks
//!   beside the nearest city it has seen, at peace with it, whose majority does; one with none
//!   sleeps. An inquisitor removes the heresy in the seat's city whose land it stands in, else
//!   walks beside the nearest of the seat's cities that has one: another founded religion's
//!   pressure ([`heresy`]).
//! - A civilian great person builds its great improvement on the seat's land, walking to a free
//!   tile within two of the capital first.
//! - A great general or admiral follows the army: the unit of its domain with the most enemies
//!   within `general_front_radius`; with no army it sleeps.
//!
//! An action is read by its kind (`ActionKind`), not by its id's text. Where Python took "the
//! first" of a set of ids (the triggers, the improvements), the first in the order the engine
//! lists them is taken (P2.3.9, fix 8).
//!
//! Fixes:
//! - A great writer's political treatise was in no list, so a great writer followed the army for
//!   the rest of the game; it is used as the great scientist's and engineer's abilities are.
//! - An inquisitor walked beside cities whose majority followed another religion and never
//!   removed anything; it removes the heresy in the seat's own city when the engine allows it.
//!   It goes only to the seat's cities where another founded religion has pressure, which the
//!   engine's inquisition clears (`religion::spread::has_heresy`): a city with no majority, a
//!   new one or one with no religion at all, is no heresy, and an inquisitor that went beside one
//!   would have stood there for good. A pantheon's pressure, the seat's own among them, is no
//!   heresy either, though the engine would clear it: that is no use of an inquisitor's one
//!   action.

use citar_engine::base::ids::{CityId, ReligionId, TileIdx, UnitId};
use citar_engine::game::actions::{ActionKind, UnitAction, UnitActionEntry, unit_actions};
use citar_engine::game::movement::can_stand;
use citar_engine::game::religion::found::{beliefs_to_choose, can_found_religion};
use citar_engine::game::religion::spread::{has_heresy, plan_remove_heresy};
use citar_engine::game::religion::{is_major, majority_religion};
use citar_engine::game::{Action, Game};
use citar_engine::rules::defs::ReligionProgress;
use serde_json::{Value, json};

use super::{nearest, nearest_city, py_within, radius};
use crate::basic1::Seat;
use crate::basic1::context::Context;
use crate::basic1::faith::choose_beliefs;
use crate::basic1::settlers::move_to;
use crate::basic1::workers::order;
use crate::driver::Turn;

/// The abilities spent where the unit stands, in Python's order, then the treatise (a fix).
const SPENT: [fn(&ActionKind) -> bool; 4] = [
    |k| matches!(k, ActionKind::HurryResearch),
    |k| matches!(k, ActionKind::TradeMission),
    |k| matches!(k, ActionKind::HurryConstruction),
    |k| matches!(k, ActionKind::PoliticalTreatise(_)),
];

/// The first action of a kind `is` picks, available or not.
fn find(acts: &[UnitActionEntry], is: impl Fn(&ActionKind) -> bool) -> Option<&UnitActionEntry> {
    acts.iter().find(|a| is(&a.kind))
}

/// The first available action of a kind `is` picks.
fn ready(acts: &[UnitActionEntry], is: impl Fn(&ActionKind) -> bool) -> Option<&UnitActionEntry> {
    acts.iter().find(|a| is(&a.kind) && a.available())
}

/// Takes action `id` with unit `u` (`unit_action`), with a name and beliefs where it needs them.
fn act(t: &mut Turn<'_>, u: UnitId, id: String, name: Option<Value>, beliefs: Option<Value>) {
    t.act(Action::UnitAction(UnitAction {
        unit_id: i64::from(u.get()),
        action: id,
        name,
        beliefs,
        x: None,
        y: None,
    }));
}

/// The names of the beliefs the bot founds or enhances its religion with.
fn beliefs(
    g: &Game,
    s: &Seat<'_>,
    enhancing: bool,
    pid: citar_engine::base::ids::PlayerId,
) -> Value {
    let need = beliefs_to_choose(g, pid, enhancing);
    let names: Vec<&str> =
        choose_beliefs(g, pid, s, &need).into_iter().filter_map(|b| g.rules().name(b)).collect();
    json!(names)
}

/// `handle_special` (basic.py:1951-2037).
#[allow(clippy::too_many_lines, reason = "one decision list, in Python's order")]
pub(crate) fn handle_special(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context, u: UnitId) {
    let pid = t.pid();
    let g = t.game();
    let Some(x) = g.unit(u) else { return };
    let (here, carries, activity, base) = (x.tile(), x.religion, x.activity, x.base);
    let Some(pl) = g.player(pid) else { return };
    let acts = unit_actions(g, u);
    let capital = pl.capital.and_then(|c| g.city(c)).map(citar_engine::state::cities::City::tile);
    if let Some(a) = find(&acts, |k| matches!(k, ActionKind::AddToSpaceship)) {
        if a.available() {
            let id = a.id.clone();
            act(t, u, id, None, None);
        } else if let Some(cap) = capital {
            clear_civilian_slot(t, cap, u);
            move_to(t, u, cap);
        }
        return;
    }
    if let Some(a) = find(&acts, |k| matches!(k, ActionKind::FoundReligion(_))) {
        if a.available() {
            let (id, name) = (a.id.clone(), json!(format!("Faith of {}", pl.name)));
            let beliefs = beliefs(g, s, false, pid);
            act(t, u, id, Some(name), Some(beliefs));
            return;
        }
        if can_found_religion(g, pid).is_none() {
            // Any city of its own will do (UnCiv): the nearest with no other civilian in it.
            let r = g.rules();
            let free =
                ctx.cities.iter().filter_map(|&c| g.city(c)).map(|c| c.tile()).filter(|&at| {
                    !g.units_at(at).any(|y| y.id() != u && !r.base_units()[y.base].military)
                });
            if let Some(to) = nearest(g, free, here) {
                move_to(t, u, to);
                return;
            }
        }
    }
    if let Some(a) = ready(&acts, |k| matches!(k, ActionKind::EnhanceReligion(_))) {
        let id = a.id.clone();
        let beliefs = beliefs(g, s, true, pid);
        act(t, u, id, None, Some(beliefs));
        return;
    }
    if find(&acts, |k| matches!(k, ActionKind::EnhanceReligion(_))).is_some()
        && pl.religion.progress == ReligionProgress::Religion
        && g.city_at(here).is_none()
        && let Some(to) = nearest_city(g, &ctx.cities, here)
    {
        move_to(t, u, to);
        return;
    }
    for is in SPENT {
        let Some(a) = ready(&acts, is) else { continue };
        let busy = g.city_at(here).is_some_and(|c| !c.queue.is_empty());
        if !matches!(a.kind, ActionKind::HurryConstruction) || busy {
            let id = a.id.clone();
            act(t, u, id, None, None);
            return;
        }
    }
    if let Some(a) = ready(&acts, |k| matches!(k, ActionKind::Trigger(_))) {
        let id = a.id.clone();
        act(t, u, id, None, None);
        return;
    }
    let spreads = find(&acts, |k| matches!(k, ActionKind::SpreadReligion)).is_some();
    let inquisitor = find(&acts, |k| matches!(k, ActionKind::RemoveHeresy)).is_some();
    if spreads && carries.is_none() && !inquisitor {
        if activity.is_none() {
            order(t, u, "sleep");
        }
        return;
    }
    if spreads || inquisitor {
        let land_of = g.tile(here).and_then(citar_engine::state::map::Tile::city);
        let other = |c| majority_religion(g, c) != carries;
        if let Some(a) = ready(&acts, |k| matches!(k, ActionKind::SpreadReligion))
            && land_of.is_some_and(other)
        {
            let id = a.id.clone();
            act(t, u, id, None, None);
            return;
        }
        let heretic = |c| carries.is_some_and(|r| heresy(g, c, r));
        if let Some(a) = ready(&acts, |k| matches!(k, ActionKind::RemoveHeresy))
            && land_of.is_some_and(heretic)
            && plan_remove_heresy(g, u).is_ok()
        {
            let id = a.id.clone();
            act(t, u, id, None, None);
            return;
        }
        // The nearest city it has seen, at peace, whose majority follows another religion; for
        // an inquisitor, the nearest of the seat's own with a heresy.
        let grid = g.grid();
        let mut cities: Vec<_> = g
            .state()
            .cities()
            .iter()
            .filter(|c| {
                let owner = c.owner();
                let fits = if inquisitor && !spreads {
                    owner == pid && heretic(c.id())
                } else {
                    (owner == pid || !g.is_barbarian(owner)) && other(c.id())
                };
                fits && !g.at_war(pid, owner) && pl.explored.contains(c.tile().0)
            })
            .map(|c| (grid.distance(c.tile(), here), c.tile()))
            .collect();
        cities.sort_by_key(|&(d, _)| d);
        if let Some(&(d, to)) = cities.first()
            && d > 1
        {
            let near =
                grid.neighbors(to).filter(|&n| g.units_at(n).next().is_none() && !g.is_water(n));
            if let Some(step) = nearest(g, near, here) {
                move_to(t, u, step);
            }
        }
        return;
    }
    let r = g.rules();
    if r.derived().advisor.civilian == Some(r.base_units()[base].unit_type) {
        let owned = g.tile(here).and_then(citar_engine::state::map::Tile::owner) == Some(pid);
        if let Some(a) = ready(&acts, |k| matches!(k, ActionKind::Create(_)))
            && owned
            && g.city_at(here).is_none()
        {
            let id = a.id.clone();
            act(t, u, id, None, None);
            return;
        }
        // Walk to a free tile next to the capital to build the great improvement there.
        if let Some(cap) = capital {
            let spots = py_within(g, cap, 2).into_iter().skip(1).filter(|&n| {
                g.tile(n)
                    .is_some_and(|tile| tile.owner() == Some(pid) && tile.improvement().is_none())
                    && !g.is_water(n)
                    && g.units_at(n).next().is_none()
            });
            if let Some(to) = nearest(g, spots, here) {
                move_to(t, u, to);
                return;
            }
        }
    }
    // Great generals and admirals follow the army.
    let domain = r.base_units()[base].domain;
    let hostile: Vec<TileIdx> =
        ctx.hostile.iter().filter_map(|&e| g.unit(e)).map(|e| e.tile()).collect();
    let near = radius(s.params.general_front_radius);
    let grid = g.grid();
    let mut front: Option<(TileIdx, usize)> = None;
    for m in ctx.military.iter().filter_map(|&m| g.unit(m)) {
        if r.base_units()[m.base].domain != domain {
            continue;
        }
        let n = hostile.iter().filter(|&&e| grid.distance(e, m.tile()) <= near).count();
        if front.is_none_or(|(_, b)| n > b) {
            front = Some((m.tile(), n));
        }
    }
    match front {
        Some((to, _)) => {
            if grid.distance(to, here) > 1 {
                move_to(t, u, to);
            }
        }
        None => {
            if activity.is_none() {
                order(t, u, "sleep");
            }
        }
    }
}

/// Whether city `c` has a heresy for an inquisitor of `r` to clear (the module's fix): another
/// founded religion's pressure, which the engine's inquisition clears (`has_heresy`, checked
/// too, so that the bot never sends one where the engine would refuse it).
fn heresy(g: &Game, c: CityId, r: ReligionId) -> bool {
    let founded = |k: Option<ReligionId>| k.is_some_and(|k| k != r && is_major(g, k));
    has_heresy(g, c, r) && g.city(c).is_some_and(|x| x.pressures.iter().any(|&(k, _)| founded(k)))
}

/// `_clear_civilian_slot` (basic.py:2039-2052): a tile holds one civilian, so the seat's other
/// civilians on `at` (a parked general, a missionary) step aside onto the first land neighbour
/// they may stand on, letting `keep` in; nothing while `keep` is more than two tiles away. One
/// sleeping unit in the capital used to block every spaceship part for the rest of the game.
fn clear_civilian_slot(t: &mut Turn<'_>, at: TileIdx, keep: UnitId) {
    let pid = t.pid();
    let g = t.game();
    if g.unit(keep).is_none_or(|k| g.grid().distance(k.tile(), at) > 2) {
        return;
    }
    let r = g.rules();
    let parts = &r.derived().advisor.parts;
    let movers: Vec<_> = g
        .units_at(at)
        .filter(|x| {
            x.owner() == pid
                && x.id() != keep
                && !r.base_units()[x.base].military
                && !parts.contains(x.base)
                && x.moves > 0
        })
        .map(|x| (x.id(), x.base))
        .collect();
    for (x, base) in movers {
        let g = t.game();
        let spot =
            g.grid().neighbors(at).find(|&n| !g.is_water(n) && can_stand(g, pid, base, n, Some(x)));
        if let Some(n) = spot {
            move_to(t, x, n);
        }
    }
}
