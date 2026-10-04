//! The army's orders (`handle_military`, `_camp_target`, `_ruin_target` and `_approach_tile`,
//! basic.py:2141-2289, 2360-2384): a land unit, in order of what comes first,
//! - escorts the settler it is assigned to, standing beside it in a city a garrison holds;
//! - in an advancing siege, a ranged unit first moves to a firing position on the target city
//!   (`siege_move_first`);
//! - attacks the best target in reach ([`attack_best`]), and a garrison then stays;
//! - a garrison fortifies;
//! - a wounded unit heals in a city with no unit in it, or where it stands;
//! - fills the nearest city that wants a garrison and has none, within `garrison_radius`;
//! - defends the most threatened city in reach, closing on the nearest enemy near it;
//! - at war, follows the war plan ([`war_target`]): gathers at the rally point, then advances on
//!   the target city, ranged units and a siege that is ready closing in, melee units otherwise
//!   standing off `melee_standoff` tiles;
//! - while a war is prepared, gathers at its rally point (`memory.war_prep`, which
//!   `diplomacy::war` keeps);
//! - clears a barbarian camp near the seat's cities, walks to ancient ruins near it;
//! - after `seek_after_turn`, explores to find a rival's city when none is known and no other
//!   unit of the army explores;
//! - goes home, and fortifies.
//!
//! Ties go to the first of equals in Python's `within`, `ring` and `neighbors` orders, and among
//! the camps in the game's order.

use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::advisor::knows_rival_city;
use citar_engine::game::movement::can_stand;
use citar_engine::game::units::unit_has;
use citar_engine::game::vis::sight::has_los;
use citar_engine::rules::defs::Domain;
use citar_engine::state::units::Activity;
use citar_engine::unique::UniqueType;
use serde_json::Value;

use super::attack::attack_best;
use super::war_plan::war_target;
use super::{is_garrison, nearest, nearest_city, py_ring, py_within, radius, within};
use crate::basic1::Seat;
use crate::basic1::context::{Context, city_defense, needs_garrison};
use crate::basic1::settlers::{escort_spot, move_to};
use crate::basic1::workers::order;
use crate::driver::Turn;

/// Where a unit stands and what it is, read afresh after each of its moves.
#[derive(Clone, Copy)]
struct Here {
    at: TileIdx,
    moves: i32,
    hp: i16,
    activity: Option<Activity>,
    base: citar_engine::base::ids::BaseUnitId,
}

fn here(g: &Game, u: UnitId) -> Option<Here> {
    g.unit(u).map(|x| Here {
        at: x.tile(),
        moves: x.moves,
        hp: x.hp,
        activity: x.activity,
        base: x.base,
    })
}

/// `handle_military` (basic.py:2141-2266).
#[allow(clippy::too_many_lines, reason = "one decision list, in Python's order")]
pub(crate) fn handle_military(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context, u: UnitId) {
    let pid = t.pid();
    let p = s.params;
    let Some(h) = here(t.game(), u) else { return };
    if h.moves <= 0 {
        return;
    }
    let (ranged, land) = {
        let d = &t.game().rules().base_units()[h.base];
        (d.ranged, d.domain == Domain::Land)
    };
    let garrison = is_garrison(s, u);
    // The settler it escorts, if any: onto its tile, or beside it in a city a garrison holds.
    let ward = s.memory.escorts.iter().find(|&(_, &e)| e == u).map(|(&w, _)| w);
    if let Some(w) = ward {
        let g = t.game();
        if let Some(at) = g.unit(w).map(citar_engine::state::units::Unit::tile) {
            if let Some(to) = escort_spot(g, pid, s, u, at)
                && h.at != to
            {
                move_to(t, u, to);
            }
            return;
        }
        s.memory.escorts.remove(&w);
    }
    // In an advancing siege, a ranged unit first moves to a firing position on the city.
    let siege = if ctx.wars.is_empty() { None } else { s.memory.war_plan.clone() };
    if let Some(plan) = siege.filter(|w| w.advance)
        && ranged
        && !garrison
        && p.siege_move_first
    {
        let g = t.game();
        let d = g.grid().distance(h.at, plan.city);
        let reach = citar_engine::game::units::health::attack_range(g, u);
        if d > 1 && within(d, reach.saturating_add(p.siege_approach_extra)) {
            let out_of_reach = !within(d, reach) || !has_los(g, h.at, plan.city);
            if out_of_reach
                && let Some(dest) = approach_tile(g, pid, s, u, plan.city, true, 0)
                && dest != h.at
            {
                move_to(t, u, dest);
                if here(t.game(), u).is_none_or(|x| x.moves <= 0) {
                    return;
                }
            }
        }
    }
    if attack_best(t, s, u) && (here(t.game(), u).is_none_or(|x| x.moves <= 0) || garrison) {
        return;
    }
    let Some(h) = here(t.game(), u) else { return };
    if garrison {
        if h.activity.is_none() {
            order(t, u, "fortify");
        }
        return;
    }
    let g = t.game();
    let grid = g.grid();
    if i32::from(h.hp) < p.heal_below_hp {
        let empty: Vec<_> = ctx
            .cities
            .iter()
            .filter_map(|&c| g.city(c))
            .filter(|c| g.military_at(c.tile()).is_none())
            .map(|c| (c.id(), c.tile()))
            .collect();
        if g.city_at(h.at).is_none()
            && let Some(to) = nearest(g, empty.iter().map(|&(_, x)| x), h.at)
            && within(grid.distance(to, h.at), p.heal_city_radius)
        {
            move_to(t, u, to);
            if t.game().unit(u).is_some_and(|x| x.tile() == to)
                && let Some(&(c, _)) = empty.iter().find(|&&(_, x)| x == to)
            {
                s.memory.garrisons.insert(c, u);
            }
            return;
        }
        if !matches!(h.activity, Some(Activity::Heal | Activity::Fortify)) {
            order(t, u, "heal");
        }
        return;
    }
    // The nearest city that wants a garrison and has none.
    let empty: Vec<_> = ctx
        .cities
        .iter()
        .filter(|&&c| !s.memory.garrisons.contains_key(&c) && needs_garrison(ctx, c))
        .filter_map(|&c| g.city(c).map(|x| (c, x.tile())))
        .collect();
    if let Some(to) = nearest(g, empty.iter().map(|&(_, x)| x), h.at)
        && within(grid.distance(to, h.at), p.garrison_radius)
        && let Some(&(c, _)) = empty.iter().find(|&&(_, x)| x == to)
    {
        s.memory.garrisons.insert(c, u);
        if h.at != to {
            move_to(t, u, to);
        }
        if t.game().unit(u).is_some_and(|x| x.tile() == to) {
            order(t, u, "fortify");
        }
        return;
    }
    // The most threatened city in reach, against the enemy nearest it.
    let campaigning = !ctx.wars.is_empty() && s.memory.war_plan.is_some();
    let mut best: Option<(usize, f64)> = None;
    for (i, &c) in ctx.cities.iter().enumerate() {
        let Some(at) = g.city(c).map(citar_engine::state::cities::City::tile) else { continue };
        let threat = ctx.threat[i];
        let floor = if campaigning { city_defense(g, c) * p.campaign_defend_ratio } else { 0.0 };
        if threat <= floor {
            continue;
        }
        let v = threat - f64::from(grid.distance(at, h.at)) * f64::from(p.defend_distance_cost);
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((i, v));
        }
    }
    if let Some((i, _)) = best {
        let at = g.city(ctx.cities[i]).map_or(h.at, citar_engine::state::cities::City::tile);
        let reach = if campaigning { p.defend_radius_campaign } else { p.defend_radius };
        if within(grid.distance(at, h.at), reach) {
            let enemies = ctx.near_enemies[i].iter().filter_map(|&e| g.unit(e)).map(|e| e.tile());
            let target = nearest(g, enemies, h.at).unwrap_or(at);
            if let Some(dest) = approach_tile(g, pid, s, u, target, false, 0) {
                move_to(t, u, dest);
                attack_best(t, s, u);
            }
            return;
        }
    }
    // At war: the plan's rally point, then its city.
    if !ctx.wars.is_empty()
        && land
        && let Some(plan) = war_target(t.game(), pid, s, ctx)
    {
        let g = t.game();
        let rally = plan.rally.unwrap_or(plan.city);
        let dest = if plan.advance {
            let close_in = ranged || plan.siege_ready;
            let ring = if close_in { 0 } else { radius(p.melee_standoff) };
            approach_tile(g, pid, s, u, plan.city, ranged, ring)
        } else {
            let far = !within(g.grid().distance(h.at, rally), p.rally_radius);
            let to = if far { rally } else { h.at };
            if to != h.at && !can_stand(g, pid, h.base, to, Some(u)) {
                approach_tile(g, pid, s, u, rally, false, 1)
            } else {
                Some(to)
            }
        };
        match dest {
            Some(d) if d != h.at => {
                move_to(t, u, d);
                attack_best(t, s, u);
                return;
            }
            Some(_) => {
                if h.activity.is_none() && !plan.advance {
                    order(t, u, "fortify");
                }
                return;
            }
            None => {}
        }
    }
    // A war being prepared: gather at its rally point.
    let prep = s.memory.war_prep.as_ref().and_then(|w| w.rally);
    if ctx.wars.is_empty()
        && land
        && let Some(rally) = prep
    {
        let g = t.game();
        if !within(g.grid().distance(h.at, rally), p.rally_radius) {
            let dest = if can_stand(g, pid, h.base, rally, Some(u)) {
                Some(rally)
            } else {
                approach_tile(g, pid, s, u, rally, false, 1)
            };
            if let Some(d) = dest.filter(|&d| d != h.at) {
                move_to(t, u, d);
            }
        } else if h.activity.is_none() {
            order(t, u, "fortify");
        }
        return;
    }
    // A barbarian camp near home.
    if let Some(camp) = camp_target(t.game(), pid, s, ctx, h.at) {
        let g = t.game();
        let dest = if g.military_at(camp).is_none() {
            Some(camp)
        } else {
            approach_tile(g, pid, s, u, camp, false, 0)
        };
        if let Some(d) = dest.filter(|&d| d != h.at)
            && move_to(t, u, d).is_some()
        {
            attack_best(t, s, u);
            return;
        }
    }
    if let Some(ruins) = ruin_target(t.game(), pid, s, h.at)
        && move_to(t, u, ruins).is_some()
    {
        return;
    }
    // Late and alone: look for a rival.
    let g = t.game();
    let exploring = ctx.units.iter().filter_map(|&x| g.unit(x)).any(|x| {
        x.activity == Some(Activity::Explore) && citar_engine::game::advisor::is_army(g, x.base)
    });
    if g.turn() > p.seek_after_turn && !knows_rival_city(g, pid) && !exploring {
        let r = order(t, u, "explore");
        if r.as_ref().and_then(|x| x.get("exploring")).and_then(Value::as_bool) == Some(true) {
            return;
        }
    }
    // Home.
    let g = t.game();
    let Some(h) = here(g, u) else { return };
    if g.city_at(h.at).is_none()
        && let Some(home) = nearest_city(g, &ctx.cities, h.at)
    {
        let spot = if g.military_at(home).is_none() {
            Some(home)
        } else {
            approach_tile(g, pid, s, u, home, false, 0)
        };
        if let Some(spot) = spot
            && spot != h.at
            && g.grid().distance(spot, h.at) > 1
            && move_to(t, u, spot).is_some()
        {
            return;
        }
    }
    if t.game().unit(u).is_some_and(|x| x.activity.is_none()) {
        order(t, u, "fortify");
    }
}

/// `_camp_target` (basic.py:2268-2281): the nearest barbarian camp the seat has seen within
/// `camp_city_radius` of one of its cities and `camp_unit_radius` of the unit at `at`.
fn camp_target(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    ctx: &Context,
    at: TileIdx,
) -> Option<TileIdx> {
    let p = s.params;
    let pl = g.player(pid)?;
    let grid = g.grid();
    let mut best: Option<(TileIdx, u32)> = None;
    for camp in g.state().world().camps.values() {
        let c = camp.tile;
        if camp.destroyed || !pl.explored.contains(c.0) {
            continue;
        }
        let d_city = ctx
            .cities
            .iter()
            .filter_map(|&x| g.city(x))
            .map(|x| grid.distance(x.tile(), c))
            .min()
            .unwrap_or(99);
        let d = grid.distance(at, c);
        if within(d_city, p.camp_city_radius)
            && within(d, p.camp_unit_radius)
            && d < best.map_or(99, |(_, b)| b)
        {
            best = Some((c, d));
        }
    }
    best.map(|(c, _)| c)
}

/// `_ruin_target` (basic.py:2283-2289): the first ancient ruins within `ruin_radius` of `at`, in
/// Python's `within` order, on a tile the seat has seen and no unit stands on.
fn ruin_target(g: &Game, pid: PlayerId, s: &Seat<'_>, at: TileIdx) -> Option<TileIdx> {
    let ruins = g.rules().derived().known.ancient_ruins?;
    let pl = g.player(pid)?;
    py_within(g, at, radius(s.params.ruin_radius)).into_iter().skip(1).find(|&t| {
        pl.explored.contains(t.0)
            && g.tile(t).and_then(citar_engine::state::map::Tile::improvement) == Some(ruins)
            && g.units_at(t).next().is_none()
    })
}

/// `_approach_tile` (basic.py:2360-2384): the tile unit `u` should move to next to close on
/// `target`. It wants to stand `ring` tiles from it, or by default one, two for a unit of range
/// two or more besieging; a ranged unit that besieges without indirect fire needs a line of sight
/// from beyond one tile. Where it stands will do if it is near enough; else the nearest tile of
/// the ring it may stand on (a siege falls back to the tiles beside the target), else of those
/// within `approach_fallback_radius`, hills first among equals; `None` when there is none.
pub(crate) fn approach_tile(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    u: UnitId,
    target: TileIdx,
    siege: bool,
    ring: u32,
) -> Option<TileIdx> {
    let x = g.unit(u)?;
    let at = x.tile();
    let grid = g.grid();
    let d = &g.rules().base_units()[x.base];
    let want = if ring > 0 {
        ring
    } else if siege && d.range >= 2 {
        2
    } else {
        1
    };
    let needs_los = siege && d.ranged && !unit_has(g, u, UniqueType::IndirectFire, true);
    let sees = |n: TileIdx| !needs_los || grid.distance(n, target) <= 1 || has_los(g, n, target);
    if grid.distance(at, target) <= want && sees(at) {
        return Some(at);
    }
    let stands = |n: TileIdx| can_stand(g, pid, x.base, n, Some(u));
    let rings: &[u32] = if want > 1 && ring == 0 { &[want, 1] } else { &[want] };
    let mut opts: Vec<TileIdx> = Vec::new();
    for &r in rings {
        opts = py_ring(g, target, r)
            .into_iter()
            .filter(|&n| (n == at || stands(n)) && sees(n))
            .collect();
        if !opts.is_empty() {
            break;
        }
    }
    if opts.is_empty() {
        let fallback = radius(s.params.approach_fallback_radius);
        opts = py_within(g, target, fallback).into_iter().skip(1).filter(|&n| stands(n)).collect();
    }
    let hill = g.rules().derived().known.hill;
    let on_hill = |n: TileIdx| g.tile(n).is_some_and(|t| t.features().contains(hill));
    // The nearest, hills first among equals; the first of equals after that.
    let mut best: Option<(TileIdx, (u32, bool))> = None;
    for n in opts {
        let key = (grid.distance(n, at), !on_hill(n));
        if best.is_none_or(|(_, b)| key < b) {
            best = Some((n, key));
        }
    }
    best.map(|(n, _)| n)
}
