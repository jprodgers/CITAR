//! Wars the bot starts (basic.py:2445-2492): a war prepared on a weaker neighbour whose city it
//! can reach, kept in `memory.war_prep`; the army gathered at a rally point near that city; the
//! war declared once enough of it has gathered, with a war plan for the army
//! (`memory.war_plan`, which `units::war_plan` keeps up to date); and the preparation given up
//! when the target is gone, the neighbour has grown too strong, a treaty or friendship binds
//! them, or `war_prep_timeout` turns have passed.
//!
//! Only with war the bot's (`Owners`): a war prepared is forgotten when the seat's model takes
//! war over (`consider_diplomacy`).
//!
//! A fix: a `war_need_city_div` of 0 counts no cities toward the army a war needs, where
//! Python's floor division by 0 raised.

use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::base::num;
use citar_engine::base::rng::KeyPart;
use citar_engine::game::diplomacy::actions::DeclareWar;
use citar_engine::game::diplomacy::category::Category;
use citar_engine::game::diplomacy::relations::{civ_has, is_friends};
use citar_engine::game::{Action, Game};
use citar_engine::rules::defs::Domain;
use citar_engine::state::diplo::Relation;
use citar_engine::unique::UniqueType;
use serde_json::json;

use super::total_threat;
use crate::basic1::Seat;
use crate::basic1::context::Context;
use crate::basic1::units::war_plan::{military_power, rally_point};
use crate::basic1::units::{is_garrison, nearest_city, within};
use crate::driver::Turn;
use crate::memory::{WarPlan, WarPrep};
use crate::params::{Params, WarTargetPick};
use crate::stream::Stream;

/// Whether `pid`'s land units can cross water yet (`_can_embark`, `movement.civ_can_embark`):
/// never the barbarians'.
fn can_embark(g: &Game, pid: PlayerId) -> bool {
    !g.is_barbarian(pid) && civ_has(g, pid, UniqueType::LandUnitEmbarkation)
}

/// `_reachable_city` (basic.py:2478-2492): a city of `q`'s the seat has explored and could
/// attack given the map: on a landmass one of its cities stands on, or anywhere once its land
/// units can embark with `war_overseas`; within `war_target_max_dist` of its nearest city. Of
/// those, the nearest (the smaller of equals) with `war_target_pick = nearest`, else the
/// smallest (the nearer of equals); the first of equals in the game's order of cities.
pub(crate) fn reachable_city(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    ctx: &Context,
    q: PlayerId,
) -> Option<CityId> {
    let p = s.params;
    let pl = g.player(pid)?;
    let grid = g.grid();
    let homes: Vec<TileIdx> =
        ctx.cities.iter().filter_map(|&c| g.city(c)).map(|c| c.tile()).collect();
    let ours: Vec<Option<u16>> = homes.iter().map(|&t| g.continent(t)).collect();
    let overseas = p.war_overseas && !homes.is_empty() && can_embark(g, pid);
    let mut best: Option<(CityId, (u32, u32))> = None;
    for c in g.state().cities().iter() {
        let at = c.tile();
        if c.owner() != q
            || !pl.explored.contains(at.0)
            || !(overseas || ours.contains(&g.continent(at)))
        {
            continue;
        }
        let Some(dist) = homes.iter().map(|&h| grid.distance(at, h)).min() else { continue };
        if !within(dist, p.war_target_max_dist) {
            continue;
        }
        let key = match p.war_target_pick {
            WarTargetPick::Nearest => (dist, u32::from(c.pop)),
            WarTargetPick::Smallest => (u32::from(c.pop), dist),
        };
        if best.is_none_or(|(_, b)| key < b) {
            best = Some((c.id(), key));
        }
    }
    best.map(|(c, _)| c)
}

/// Whether to start preparing a war on `q`, in peace with it (basic.py:2445-2451): with war the
/// bot's, past `war_min_turn`, at war with nobody, preparing none, no treaty with them and no
/// friendship, its cities' threats light against its strength; then if it is enough stronger,
/// the `WarPrep` stream draws for it, and it can reach one of their cities.
#[allow(clippy::too_many_arguments, reason = "one decision, with what Python's loop held")]
pub(crate) fn consider_war(
    g: &Game,
    pid: PlayerId,
    s: &mut Seat<'_>,
    ctx: &Context,
    q: PlayerId,
    rel: &Relation,
    mine: f64,
    theirs: f64,
) {
    let p = s.params;
    let a = s.spec.aggression;
    let turn = g.turn();
    if s.spec.owners.llm(Category::War)
        || turn <= p.war_min_turn
        || !ctx.wars.is_empty()
        || s.memory.war_prep.is_some()
        || rel.treaty_until >= turn
        || total_threat(ctx) >= mine * p.war_max_threat
        || is_friends(g, pid, q)
    {
        return;
    }
    if mine > theirs * (p.war_power_ratio - p.war_power_ratio_aggr * a)
        && Stream::WarPrep.rng(g, pid, &[q.key()]).unit()
            < (p.war_chance + p.war_chance_aggr * a) * p.war_prep_rate
        && reachable_city(g, pid, s, ctx, q).is_some()
    {
        s.memory.war_prep = Some(WarPrep { player: q, since: turn, target: None, rally: None });
    }
}

/// The army a war being prepared needs before it is declared (basic.py:2465-2466): one per
/// `war_need_city_div` cities plus `war_need_base`, held to `war_need_min..=war_need_max` as
/// Python's `max(min, min(max, n))` held it.
pub(crate) fn army_needed(p: &Params, cities: usize) -> i64 {
    let n = i64::try_from(cities).unwrap_or(i64::MAX);
    let div = i64::from(p.war_need_city_div);
    let per = if div == 0 { 0 } else { num::floor_div(n, div) };
    i64::from(p.war_need_min).max(i64::from(p.war_need_max).min(per + i64::from(p.war_need_base)))
}

/// The land units of the field army: military units standing on their own, not garrisoning a
/// city (basic.py:2456-2457).
fn field_army(g: &Game, s: &Seat<'_>, ctx: &Context) -> Vec<(UnitId, TileIdx)> {
    let r = g.rules();
    ctx.military
        .iter()
        .filter_map(|&u| g.unit(u))
        .filter(|x| {
            let at = x.tile();
            r.base_units()[x.base].domain == Domain::Land
                && g.military_at(at).is_some_and(|m| m.id() == x.id())
                && !(g.city_at(at).is_some() && is_garrison(s, x.id()))
        })
        .map(|x| (x.id(), x.tile()))
        .collect()
}

/// The war being prepared (basic.py:2452-2476): given up when it no longer makes sense;
/// otherwise the army gathers at the rally point near the target (with `prep_gather`), and once
/// enough of it has gathered, or an overwhelming army has, and the seat is strong enough, war is
/// declared with a plan to take the target. A war already being fought ends any preparation.
pub(crate) fn prepare(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context, mine: f64) {
    let Some(mut prep) = s.memory.war_prep.clone() else { return };
    if !ctx.wars.is_empty() {
        s.memory.war_prep = None;
        return;
    }
    let g = t.game();
    let pid = t.pid();
    let p = s.params;
    let a = s.spec.aggression;
    let turn = g.turn();
    let q = prep.player;
    let theirs = military_power(g, q);
    let field = field_army(g, s, ctx);
    let alive = g.player(q).is_some_and(|x| x.alive());
    let target = if alive { reachable_city(g, pid, s, ctx, q) } else { None };
    let treaty = g.relation(pid, q).map_or(0, |r| r.treaty_until) >= turn;
    let Some(target) = target.and_then(|c| g.city(c)).map(|c| c.tile()) else {
        s.memory.war_prep = None;
        return;
    };
    if i64::from(turn) - i64::from(prep.since) > i64::from(p.war_prep_timeout)
        || mine < theirs * (p.war_abort_ratio - p.war_abort_ratio_aggr * a)
        || treaty
        || is_friends(g, pid, q)
    {
        s.memory.war_prep = None;
        return;
    }
    let need = army_needed(p, ctx.cities.len());
    let ready = if p.prep_gather {
        if prep.target != Some(target) {
            // Where the army stages: the seat's city nearest the target.
            let Some(stage) = nearest_city(g, &ctx.cities, target) else {
                s.memory.war_prep = None;
                return;
            };
            prep.target = Some(target);
            prep.rally = Some(rally_point(g, pid, s, target, stage));
        }
        let rally = prep.rally.unwrap_or(target);
        field
            .iter()
            .filter(|&&(_, at)| within(g.grid().distance(at, rally), p.prep_gather_radius))
            .count()
    } else {
        field.len()
    };
    let ready = i64::try_from(ready).unwrap_or(i64::MAX);
    let overwhelming = ready >= i64::from(p.overwhelm_units) && mine > theirs * p.overwhelm_ratio;
    if (ready >= need || overwhelming)
        && mine > theirs * (p.declare_ratio - p.declare_ratio_aggr * a)
    {
        let declared = t
            .act(Action::DeclareWar(DeclareWar {
                player_id: i64::from(q.0),
                message: Some(json!("Your lands will be ours.")),
            }))
            .is_some();
        if declared {
            s.memory.war_plan = Some(WarPlan {
                city: target,
                since: turn,
                advance: p.prep_gather,
                checked: None,
                rally: prep.rally,
                siege_ready: false,
            });
        }
        s.memory.war_prep = None;
    } else {
        s.memory.war_prep = Some(prep);
    }
}
