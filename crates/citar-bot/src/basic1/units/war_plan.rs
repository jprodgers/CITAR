//! The city a war is fought for, and where the army gathers before it goes (`_war_target`,
//! `_rally_point` and `military_power`, basic.py:2291-2358, 2389-2392), kept in
//! `memory.war_plan`.
//!
//! At war with a major civilization, the bot aims at the enemy city it has seen that is nearest
//! its own, smaller ones and those damaged since the last turn preferred, within
//! `war_target_max_dist` of one of its cities. Once a turn the plan is brought up to date: the
//! rally point, near the target but out of its reach, on the target's landmass, unowned or the
//! bot's, cover preferred; whether the army advances (enough of the field army gathered, or a
//! weak target and enough of an army) or falls back to gather again; and whether the siege is
//! ready for the melee units to close in (a ranged unit near the city, or the city worn down).
//!
//! A fix: Python kept a plan whose city was gone, taken, or no longer an enemy's until a land
//! unit asked for a target again, and kept it for good when there was no other target. Such a
//! plan still preferred its city for siege fire and kept the army "campaigning", which raised the
//! threat a city had to face before the army came home to defend it. A plan that no longer holds
//! is forgotten as the units' turn starts ([`forget_stale`]), and when a new target is asked for.

use citar_engine::base::ids::{PlayerId, TileIdx};
use citar_engine::base::num;
use citar_engine::game::Game;
use citar_engine::game::cities::stats::max_health;
use citar_engine::game::victory::score::military_strength;
use citar_engine::rules::defs::Domain;

use super::{is_garrison, py_within, radius, within};
use crate::basic1::Seat;
use crate::basic1::context::Context;
use crate::memory::WarPlan;

/// This civilization's military strength, for comparing against another's (`military_power`,
/// basic.py:2389-2392): one more than the engine's, so that no ratio divides by nothing.
pub(crate) fn military_power(g: &Game, pid: PlayerId) -> f64 {
    f64::from(military_strength(g, pid)) + 1.0
}

/// Whether a war plan still holds: its city stands, is not the seat's, and belongs to a
/// civilization it is at war with; and the seat has a city to fight it from (basic.py:2296-2299).
fn holds(g: &Game, pid: PlayerId, plan: &WarPlan, ctx: &Context) -> bool {
    g.city_at(plan.city).is_some_and(|c| c.owner() != pid && g.at_war(pid, c.owner()))
        && !ctx.cities.is_empty()
}

/// Forgets a war plan that no longer holds (the module's fix).
pub(crate) fn forget_stale(g: &Game, pid: PlayerId, s: &mut Seat<'_>, ctx: &Context) {
    if s.memory.war_plan.as_ref().is_some_and(|w| !holds(g, pid, w, ctx)) {
        s.memory.war_plan = None;
    }
}

/// `_war_target` (basic.py:2291-2339): the plan of the seat's war, made when there is none and
/// brought up to date once a turn; `None` when no enemy city is in reach.
pub(crate) fn war_target(
    g: &Game,
    pid: PlayerId,
    s: &mut Seat<'_>,
    ctx: &Context,
) -> Option<WarPlan> {
    let p = s.params;
    let turn = g.turn();
    let grid = g.grid();
    forget_stale(g, pid, s, ctx);
    if s.memory.war_plan.is_none() {
        let pl = g.player(pid)?;
        let mut best: Option<(TileIdx, f64)> = None;
        for c in g.state().cities().iter() {
            if !ctx.wars.contains(&c.owner()) || !pl.explored.contains(c.tile().0) {
                continue;
            }
            let d = ctx
                .cities
                .iter()
                .filter_map(|&x| g.city(x))
                .map(|x| grid.distance(c.tile(), x.tile()))
                .min()
                .unwrap_or(99);
            if !within(d, p.war_target_max_dist) {
                // Out of reach: this war is fought at home.
                continue;
            }
            let damaged = if c.damaged_turn >= turn - 1 { p.target_damaged_bonus } else { 0 };
            let score = f64::from(d) + f64::from(c.pop) * p.target_pop_weight - f64::from(damaged);
            if score < best.map_or(999.0, |(_, b)| b) {
                best = Some((c.tile(), score));
            }
        }
        let (city, _) = best?;
        s.memory.war_plan = Some(WarPlan {
            city,
            since: turn,
            advance: false,
            checked: None,
            rally: None,
            siege_ready: false,
        });
    }
    let mut plan = s.memory.war_plan.clone()?;
    if plan.checked != Some(turn) {
        plan.checked = Some(turn);
        let target = plan.city;
        // Where the army stages: the seat's city nearest the target.
        let stage = super::nearest_city(g, &ctx.cities, target)?;
        let rally = *plan.rally.get_or_insert_with(|| rally_point(g, pid, s, target, stage));
        let r = g.rules();
        let field: Vec<(TileIdx, bool)> = ctx
            .military
            .iter()
            .filter(|&&u| !is_garrison(s, u))
            .filter_map(|&u| g.unit(u))
            .filter(|x| r.base_units()[x.base].domain == Domain::Land)
            .map(|x| (x.tile(), r.base_units()[x.base].ranged))
            .collect();
        let rally_d = grid.distance(rally, target);
        let gathered = field
            .iter()
            .filter(|&&(at, _)| {
                let near_target = within(grid.distance(at, target), p.gather_radius);
                if plan.advance {
                    near_target
                } else {
                    within(grid.distance(at, rally), p.gather_radius)
                        || grid.distance(at, target) <= rally_d
                }
            })
            .count();
        let (gathered, n) = (count(gathered), count(field.len()));
        #[allow(clippy::cast_precision_loss, reason = "an army's size")]
        let share = num::trunc_i64(n as f64 * p.advance_share);
        let need = i64::from(p.advance_min).max(i64::from(p.advance_max).min(share));
        let city = g.city_at(target);
        let weak = city.is_some_and(|c| {
            f64::from(c.health) <= f64::from(max_health(g, c.id())) * p.weak_city_health
                || military_power(g, c.owner()) * p.weak_power_ratio < military_power(g, pid)
        });
        if !plan.advance && (gathered >= need || (weak && n >= i64::from(p.weak_min_units))) {
            plan.advance = true;
        } else if plan.advance && gathered < i64::from(p.advance_hold_min) && !weak {
            plan.advance = false;
            plan.rally = Some(rally_point(g, pid, s, target, stage));
        }
        let in_range = field
            .iter()
            .filter(|&&(at, ranged)| {
                ranged && within(grid.distance(at, target), p.siege_ready_dist)
            })
            .count();
        plan.siege_ready = city.is_some_and(|c| {
            in_range >= 1
                || f64::from(c.health) <= f64::from(max_health(g, c.id())) * p.siege_ready_health
        });
        s.memory.war_plan = Some(plan.clone());
    }
    Some(plan)
}

/// A count as Python's `len`.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// `_rally_point` (basic.py:2341-2358): where to gather an army before attacking `target` from
/// `stage`, one of the seat's cities: of the tiles from `rally_min_dist` to `rally_max_dist` of
/// the target, on its landmass, with no city, unowned or the seat's, the nearest the stage,
/// `rally_cover_bonus` nearer on hills, in forest or in jungle; the first of equals in Python's
/// `within` order, else the stage itself.
///
/// Gathering first is what makes the bot's wars work at all: units sent one at a time arrive
/// one at a time and die one at a time.
pub(crate) fn rally_point(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    target: TileIdx,
    stage: TileIdx,
) -> TileIdx {
    let p = s.params;
    let grid = g.grid();
    let known = &g.rules().derived().known;
    let features = &g.rules().derived().features;
    let cover = |t: &citar_engine::state::map::Tile| {
        let f = t.features();
        if f.contains(known.hill) {
            return true;
        }
        let mut rest = f;
        rest.remove(known.hill);
        let top = rest.top().and_then(|x| features.get(x).copied());
        top.is_some() && (top == known.map.forest || top == known.map.jungle)
    };
    let land = g.continent(target);
    let mut best = stage;
    let mut best_v = 1e9;
    for at in py_within(g, target, radius(p.rally_max_dist)) {
        let d = grid.distance(at, target);
        let Some(tile) = g.tile(at) else { continue };
        if i64::from(d) < i64::from(p.rally_min_dist)
            || g.continent(at) != land
            || g.city_at(at).is_some()
            || tile.owner().is_some_and(|o| o != pid)
        {
            continue;
        }
        let bonus = if cover(tile) { p.rally_cover_bonus } else { 0 };
        let v = f64::from(grid.distance(at, stage)) - f64::from(bonus);
        if v < best_v {
            best = at;
            best_v = v;
        }
    }
    best
}
