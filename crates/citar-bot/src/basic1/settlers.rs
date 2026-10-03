//! Settlers (`handle_settler`, basic.py:1835-1857 and 1879-1888): found the capital where the
//! first settler stands; otherwise walk to the best expansion site in reach and found the city
//! on arriving. Danger, escorts and retreats (1858-1878, `_follow`) are package 2-03's.
//!
//! The sites are the production advisor's (`Advisor::sites`), found afresh each turn, where
//! Python kept them for `site_cache_turns` (P2.3.9, fix 5); a site no city can be founded on now,
//! or one the bot has given up on, is not gone to.
//!
//! What differs from Python, on purpose (P2.3.9, fix 2): a site no path reaches is recorded in
//! `memory.bad_sites` (blacklisted for `site_blacklist_turns`), and a standing order to walk to
//! a blacklisted site is ignored, where Python also cleared the unit's `goto` by writing to the
//! engine's state.

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::actions::UnitAction;
use citar_engine::game::cities::founding::found_check;
use citar_engine::game::movement::find_path;
use citar_engine::game::units::actions::MoveUnit;
use citar_engine::game::{Action, Game, Outcome};

use super::Seat;
use super::context::Context;
use crate::driver::Turn;

/// The most turns a path to a site may take before it counts as unreachable (Python's
/// `find_path` default).
const PATH_TURNS: u32 = 40;

/// `handle_settler` without danger or escorts (basic.py:1835-1857, 1879-1888). `sites` are the
/// expansion sites of the turn, best first, asked of the advisor the first time a settler needs
/// them.
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
    match move_to(t, u, target) {
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
