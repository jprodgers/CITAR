//! Settlers (`handle_settler`, `_guarded`, `_assign_escort` and `_follow`, basic.py:1835-1926):
//! found the capital where the first settler stands; otherwise walk to the best expansion site in
//! reach and found the city on arriving.
//!
//! A settler an enemy could reach on its way (a hostile unit within its reach, plus
//! `settler_danger_margin`, of the settler or of the site) does not go alone: a free land unit of
//! the army within `escort_radius` (not a garrison, not escorting another, with
//! `escort_min_hp`) is assigned to it (`memory.escorts`) and comes onto its tile, and the two go
//! together, the escort following each step. With no escort it turns back to the nearest city,
//! or waits in the city it is in for one (`memory.need_escort`, which the production advisor
//! reads to build a defender there); a site it has turned back from `settler_max_retreats` times
//! (`memory.retreats`) is given up on for a while (`memory.bad_sites`).
//!
//! The sites are the production advisor's (`Advisor::sites`), found afresh each turn, where
//! Python kept them for `site_cache_turns` (P2.3.9, fix 5); a site no city can be founded on now,
//! or one the bot has given up on, is not gone to.
//!
//! What differs from Python, on purpose (P2.3.9, fix 2): a site no path reaches, or one given up
//! on as unsafe, is recorded in `memory.bad_sites` (blacklisted for `site_blacklist_turns`), and a
//! standing order to walk to a blacklisted site is ignored, where Python also cleared the unit's
//! `goto` by writing to the engine's state.

use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::actions::UnitAction;
use citar_engine::game::cities::founding::found_check;
use citar_engine::game::derive::danger::threat_reach;
use citar_engine::game::movement::find_path;
use citar_engine::game::units::actions::MoveUnit;
use citar_engine::game::{Action, Game, Outcome};
use citar_engine::rules::defs::Domain;

use super::Seat;
use super::context::Context;
use super::units::{is_garrison, nearest, nearest_city, within};
use super::workers::order;
use crate::driver::Turn;

/// The most turns a path to a site may take before it counts as unreachable (Python's
/// `find_path` default).
const PATH_TURNS: u32 = 40;

/// `handle_settler` (basic.py:1835-1888). `sites` are the expansion sites of the turn, best
/// first, asked of the advisor the first time a settler needs them.
pub(crate) fn handle_settler(
    t: &mut Turn<'_>,
    s: &mut Seat<'_>,
    ctx: &Context,
    u: UnitId,
    sites: &mut Option<Vec<TileIdx>>,
) {
    let pid = t.pid();
    let g = t.game();
    let Some(unit) = g.unit(u) else { return };
    let here = unit.tile();
    if ctx.cities.is_empty() && found_check(g, pid, here).is_none() {
        found(t, u);
        return;
    }
    if unit.moves <= 0 {
        // It has moved this turn already (a standing order runs as the turn starts).
        return;
    }
    let goto = unit.goto;
    let sites = sites.get_or_insert_with(|| super::advisor(g, pid, s).sites(g).to_vec());
    let open = |g: &Game, x: TileIdx| {
        found_check(g, pid, x).is_none() && !s.memory.bad_sites.contains_key(&x)
    };
    let open_sites: Vec<TileIdx> = sites.iter().copied().filter(|&x| open(g, x)).collect();
    let mut target = goto.filter(|&x| open(g, x));
    if target.is_none() {
        // The nearest, a lower rank costing `settler_rank_cost` a place; the first of equals.
        let mut best: Option<(TileIdx, f64)> = None;
        for (i, &x) in open_sites.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "a handful of sites")]
            let cost =
                f64::from(g.grid().distance(x, here)) + i as f64 * s.params.settler_rank_cost;
            if best.is_none_or(|(_, b)| cost < b) {
                best = Some((x, cost));
            }
        }
        target = best.map(|(x, _)| x);
    }
    let Some(target) = target else {
        // Nowhere to go: found here when near enough to a city of its own, else go home.
        let nearest = ctx
            .cities
            .iter()
            .filter_map(|&c| g.city(c))
            .map(|c| (c.tile(), g.grid().distance(c.tile(), here)))
            .min_by_key(|&(_, d)| d);
        let Some((home, d)) = nearest else { return };
        if found_check(g, pid, here).is_none()
            && i64::from(d) <= i64::from(s.params.settler_found_here_dist)
        {
            found(t, u);
        } else if g.city_at(here).is_none() {
            move_to(t, u, home);
        }
        return;
    };
    if here == target {
        found(t, u);
        return;
    }
    if in_danger(g, s, ctx, here, target) && !guarded(g, pid, here) {
        let escort = assign_escort(t, s, ctx, u);
        if escort.is_none() || !guarded(t.game(), pid, here) {
            retreat(t, s, ctx, u, target);
            return;
        }
    }
    let moved = move_to(t, u, target);
    follow(t, s, u);
    match moved {
        None => {
            let g = t.game();
            if g.unit(u).is_some() && find_path(g, u, target, PATH_TURNS).is_none() {
                // Unreachable: try elsewhere for a while.
                s.memory.bad_sites.insert(target, g.turn());
            }
        }
        Some(out) => {
            let arrived = out.get("arrived").and_then(serde_json::Value::as_bool) == Some(true);
            if arrived && t.game().unit(u).is_some_and(|x| x.moves > 0) {
                found(t, u);
            }
        }
    }
}

/// Whether a hostile unit could reach the settler at `here`, or the site, next turn (basic.py:
/// 1858-1862): an escort sharing its tile makes the trip safe, as UnCiv escorts settlers.
fn in_danger(g: &Game, s: &Seat<'_>, ctx: &Context, here: TileIdx, site: TileIdx) -> bool {
    let grid = g.grid();
    let margin = s.params.settler_danger_margin;
    ctx.hostile.iter().filter_map(|&e| g.unit(e)).any(|e| {
        let d = grid.distance(e.tile(), here).min(grid.distance(e.tile(), site));
        let reach = i32::try_from(threat_reach(g, e.id())).unwrap_or(i32::MAX);
        within(d, reach.saturating_add(margin))
    })
}

/// Whether a military unit of the seat's shares tile `at` (`_guarded`, basic.py:1896-1899).
fn guarded(g: &Game, pid: PlayerId, at: TileIdx) -> bool {
    g.military_at(at).is_some_and(|m| m.owner() == pid)
}

/// `_assign_escort` (basic.py:1901-1915): the settler's escort, assigned if it has none (the
/// nearest free land unit of the army within `escort_radius`, with `escort_min_hp`), brought
/// onto its tile if it can still move; `None` when there is nobody to send.
fn assign_escort(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context, u: UnitId) -> Option<UnitId> {
    let pid = t.pid();
    let g = t.game();
    let here = g.unit(u)?.tile();
    let ours = |e: UnitId| g.unit(e).is_some_and(|x| x.owner() == pid);
    let mut escort = s.memory.escorts.get(&u).copied().filter(|&e| ours(e));
    if escort.is_none() {
        let p = s.params;
        let r = g.rules();
        let free: Vec<(UnitId, TileIdx)> = ctx
            .military
            .iter()
            .filter(|&&m| !is_garrison(s, m) && !s.memory.escorts.values().any(|&e| e == m))
            .filter_map(|&m| g.unit(m))
            .filter(|m| {
                r.base_units()[m.base].domain == Domain::Land
                    && i32::from(m.hp) >= p.escort_min_hp
                    && within(g.grid().distance(m.tile(), here), p.escort_radius)
            })
            .map(|m| (m.id(), m.tile()))
            .collect();
        let to = nearest(g, free.iter().map(|&(_, at)| at), here)?;
        let chosen = free.iter().find(|&&(_, at)| at == to).map(|&(m, _)| m)?;
        s.memory.escorts.insert(u, chosen);
        escort = Some(chosen);
    }
    let e = escort?;
    if t.game().unit(e).is_some_and(|x| x.tile() != here && x.moves > 0) {
        move_to(t, e, here);
    }
    Some(e)
}

/// The settler turns back from `site` (basic.py:1866-1878): a site turned back from
/// `settler_max_retreats` times is given up on for a while; the settler goes to the nearest of
/// the seat's cities, or, in one already, waits there for an escort.
fn retreat(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context, u: UnitId, site: TileIdx) {
    let g = t.game();
    let n = s.memory.retreats.entry(site).or_insert(0);
    *n = n.saturating_add(1);
    if i32::from(*n) >= s.params.settler_max_retreats {
        // This site keeps being unsafe: pick another.
        s.memory.bad_sites.insert(site, g.turn());
        s.memory.retreats.remove(&site);
    }
    let Some(here) = g.unit(u).map(citar_engine::state::units::Unit::tile) else { return };
    if g.city_at(here).is_none()
        && let Some(home) = nearest_city(g, &ctx.cities, here)
    {
        move_to(t, u, home);
    } else {
        s.memory.need_escort = Some(here);
        order(t, u, "skip");
    }
}

/// `_follow` (basic.py:1917-1925): the settler moved, and its escort comes onto its tile; an
/// escort or a settler that is gone ends the escort.
fn follow(t: &mut Turn<'_>, s: &mut Seat<'_>, u: UnitId) {
    let g = t.game();
    let escort = s.memory.escorts.get(&u).copied().and_then(|e| g.unit(e));
    let (Some(e), Some(x)) = (escort, g.unit(u)) else {
        s.memory.escorts.remove(&u);
        return;
    };
    if e.tile() != x.tile() && e.moves > 0 {
        let (e, to) = (e.id(), x.tile());
        move_to(t, e, to);
    }
}

/// Founds a city where unit `u` stands (`unit_action found_city`).
fn found(t: &mut Turn<'_>, u: UnitId) {
    t.act(Action::UnitAction(UnitAction {
        unit_id: i64::from(u.get()),
        action: "found_city".to_owned(),
        name: None,
        beliefs: None,
        x: None,
        y: None,
    }));
}

/// Moves unit `u` toward `to` (`_move`, basic.py:1828-1833): what the move did, or `None` when it
/// was refused or the unit is there already.
pub(crate) fn move_to(t: &mut Turn<'_>, u: UnitId, to: TileIdx) -> Option<Outcome> {
    if t.game().unit(u).is_some_and(|x| x.tile() == to) {
        return None;
    }
    let (x, y) = t.game().xy(to);
    t.act(Action::MoveUnit(MoveUnit {
        unit_id: i64::from(u.get()),
        x: i64::from(x),
        y: i64::from(y),
    }))
}
