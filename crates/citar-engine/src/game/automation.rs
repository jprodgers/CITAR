//! Standing orders (`automation.py`): multi-turn moves carried out at the start of each turn,
//! automated exploration, automated workers, and city-site scoring.
//!
//! Stage S8 runs [`run_unit_orders`] for every major, a settle after each unit's order: a move
//! order walks on (`movement::move_toward` along the route it was given), an explorer heads for
//! the next unexplored ground, an automated worker for its next job, a sleeper wakes when enemies
//! come near, and a builder whose tile is threatened stops.
//!
//! **Worker jobs** follow the idea of UnCiv's `WorkerAutomation`: on each tile of the
//! civilization's cities the improvement with the best yield gain ([`best_job`]), weighed by how
//! near and how worked the tile is ([`worker_jobs`]), and roads that link cities to the capital.
//! The best job on a tile is the job map's (`derive::jobs`, one per civilization and builder
//! class), where Python cached each tile's answer per unit type for the rest of a turn; a unit
//! with no builder class asks the tile directly. The tiles civilians keep out of are the danger
//! map's (`derive::danger`).
//!
//! What differs from Python, on purpose:
//! - a tile's job is judged for the builder class and its civilization, not for the unit asking:
//!   a unit's own promotions and conditionals do not change the job map (`jobs-by-builder-class`).
//!   Python judged it for the first unit of the type to ask in a turn, and kept that answer for
//!   every other;
//! - jobs of the same priority go to the higher tile, then to the improvement later in the
//!   ruleset (`jobs-tie-by-ruleset-order`), where Python compared the improvements' names.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::Game;
use super::derive::danger::{danger, hostile_military, threat_reach};
use super::derive::rev::{PlayerTouch, UnitTouch};
use super::movement::{self, Stop};
use super::path::Mover;
use super::workers::{self, Builder};
use crate::base::ids::{ImprovementId, PlayerId, ResourceId, TileIdx, UnitId};
use crate::base::sets::{ImprovementSet, PlayerSet};
use crate::base::stats::{Stat, Stats};
use crate::rules::defs::{Domain, ImprovementKind, ResourceType};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::map::Tile;
use crate::state::units::{Activity, Unit};

/// How much each yield is worth to an automated worker (`automation.YIELD_WEIGHTS`), in Python's
/// order, which is the order they are summed in.
pub const YIELD_WEIGHTS: [(Stat, f64); 7] = [
    (Stat::Food, 1.4),
    (Stat::Production, 1.2),
    (Stat::Gold, 0.9),
    (Stat::Science, 1.0),
    (Stat::Culture, 0.9),
    (Stat::Faith, 0.8),
    (Stat::Happiness, 1.0),
];

/// Stats as an automated worker weighs them.
fn weighted(s: &Stats) -> f64 {
    YIELD_WEIGHTS.iter().fold(0.0, |acc, &(k, w)| acc + s[k] * w)
}

// ---- Standing orders (automation.py:27-62) ----------------------------------------------------------

/// Stage S8, standing orders (`automation.run_unit_orders`, `automation.py:27-62`): each of the
/// civilization's units in id order carries out its order, a settle after each. A builder whose
/// tile a hostile unit threatens stops building; a move order walks on and is news only when it
/// ended short; an explorer explores, an automated worker works, a sleeper near enemies wakes. A
/// move order with no way on is dropped.
pub(crate) fn run_unit_orders(g: &mut Game, p: PlayerId) {
    let ids: Vec<UnitId> = {
        let mut v: Vec<UnitId> = g.player_units(p).map(Unit::id).collect();
        v.sort();
        v
    };
    let mut claimed: BTreeSet<TileIdx> = BTreeSet::new();
    for u in ids {
        let Some((t, activity, goto)) = g.unit(u).map(|x| (x.tile(), x.activity, x.goto)) else {
            continue;
        };
        let label = unit_label(g, u);
        if activity == Some(Activity::Build)
            && let Some(last) = g.state().tiles().builds(t).last().map(|s| s.improvement)
            && civilian_in_danger(g, u)
        {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.activity = None;
            }
            let what = g.rules().improvements()[last].name.clone();
            let text = format!("{label} stopped building {what}: enemies nearby.");
            let data = EventData { unit: Some(u), ..EventData::default() };
            g.emit(EngineEvent::UnitWoke, &text, Some(PlayerSet::single(p)), Some(t), data, &[]);
        }
        let activity = g.unit(u).and_then(|x| x.activity);
        match activity {
            Some(Activity::Goto) if goto.is_some() => {
                let dest = goto.unwrap_or(t);
                match movement::move_toward(g, u, dest, true, true) {
                    Ok(res) => {
                        if let Some(stop) = res.stopped
                            && !res.order_kept
                            && !res.arrived
                        {
                            let at = g.unit(u).map_or(t, Unit::tile);
                            let mut why = stop.text(g);
                            if let Some(n) = res.stalled {
                                why.push_str(&format!(", no progress for {n} turns"));
                            }
                            let text =
                                format!("{label} stopped its move to {}: {why}.", g.fmt_xy(dest));
                            let data = EventData { unit: Some(u), ..EventData::default() };
                            let audience = Some(PlayerSet::single(p));
                            g.emit(
                                EngineEvent::OrdersInterrupted,
                                &text,
                                audience,
                                Some(at),
                                data,
                                &[],
                            );
                        }
                    }
                    Err(_) => {
                        if let Some(x) = g.unit_mut(u, UnitTouch::CORE)
                            && x.activity == Some(Activity::Goto)
                        {
                            x.activity = None;
                            x.goto = None;
                            x.path.clear();
                            x.order_wait = 0;
                        }
                    }
                }
            }
            Some(Activity::Explore) => {
                explore(g, u);
            }
            Some(Activity::Automate) => {
                automate_worker(g, u, &mut claimed);
            }
            Some(Activity::Sleep) if enemy_near(g, u, 2) => {
                if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                    x.activity = None;
                }
                let at = g.unit(u).map_or(t, Unit::tile);
                let text = format!("{label} woke up: enemies nearby.");
                let data = EventData { unit: Some(u), ..EventData::default() };
                g.emit(
                    EngineEvent::UnitWoke,
                    &text,
                    Some(PlayerSet::single(p)),
                    Some(at),
                    data,
                    &[],
                );
            }
            _ => {}
        }
        g.settle();
    }
}

/// A unit as the announcements name it: `Warrior #12`.
fn unit_label(g: &Game, u: UnitId) -> String {
    g.unit(u).map_or_else(String::new, |x| {
        format!("{} #{}", g.rules().base_units()[x.base].name, u.get())
    })
}

/// Whether an unescorted civilian could be taken next turn (`automation._civilian_in_danger`,
/// `automation.py:76-89`): a civilian with no military unit beside it and no city under it, with
/// a hostile military land unit it sees within five tiles whose reach covers it.
#[must_use]
pub fn civilian_in_danger(g: &Game, u: UnitId) -> bool {
    let Some(x) = g.unit(u) else { return false };
    let r = g.rules();
    let at = x.tile();
    if r.base_units()[x.base].military || g.military_at(at).is_some() || g.city_at(at).is_some() {
        return false;
    }
    let p = x.owner();
    let Some(vis) = g.dv.vis.visible(p) else { return false };
    for t in g.grid().within(at, 5) {
        if !vis.contains(t.0) {
            continue;
        }
        for other in g.units_at(t) {
            if hostile_military(g, p, other)
                && r.base_units()[other.base].domain == Domain::Land
                && g.grid().distance(t, at) <= threat_reach(g, other.id())
            {
                return true;
            }
        }
    }
    false
}

/// Whether a hostile military unit stands within `radius` of unit `u`, seen or not
/// (`automation._enemy_near`, `automation.py:92-94`).
#[must_use]
pub fn enemy_near(g: &Game, u: UnitId, radius: u32) -> bool {
    let Some(x) = g.unit(u) else { return false };
    let p = x.owner();
    g.grid()
        .within(x.tile(), radius)
        .into_iter()
        .any(|t| g.units_at(t).any(|o| hostile_military(g, p, o)))
}

// ---- City sites (automation.py:100-143) -------------------------------------------------------------

/// How good a city site tile `t` is for civilization `p`, or `None` where no city can go
/// (`automation.city_site_score`, `automation.py:100-126`): the yields it sees within two tiles
/// (an unexplored tile counts one), its resources, less for others' land; more on the coast, a
/// river or a hill.
#[must_use]
pub fn city_site_score(g: &Game, p: PlayerId, t: TileIdx) -> Option<f64> {
    let pl = g.player(p)?;
    if !pl.explored.contains(t.0) || super::cities::founding::found_check(g, p, t).is_some() {
        return None;
    }
    let camp = g.rules().derived().known.barbarian_camp;
    let near = g.grid().within(t, 2);
    if camp.is_some() && near.iter().any(|&n| g.tile(n).and_then(Tile::improvement) == camp) {
        return None;
    }
    let r = g.rules();
    let mut v = 0.0;
    for &n in &near {
        if !pl.explored.contains(n.0) {
            v += 1.0;
            continue;
        }
        let y = super::query::tile_yield(g, n, Some(p), None);
        v += y[Stat::Food] * 1.4 + y[Stat::Production] + y[Stat::Gold] * 0.5;
        let Some(tile) = g.tile(n) else { continue };
        if let Some(res) = tile.resource()
            && workers::resource_visible(g, p, res)
        {
            v += if r.resources()[res].kind == ResourceType::Luxury { 3.0 } else { 2.0 };
        }
        if tile.owner().is_some_and(|o| o != p) {
            v -= 4.0;
        }
    }
    let tile = g.tile(t)?;
    if super::tiles::adjacent_to_coast(g, t) {
        v += 3.0;
    }
    if tile.river_mask() != 0 {
        v += 3.0;
    }
    if tile.features().contains(r.derived().known.hill) {
        v += 2.0;
    }
    Some(v)
}

/// The best city sites near a tile, at least three tiles apart, best first
/// (`automation.suggest_city_sites`, `automation.py:129-143`): each site's score less 2.2 a tile
/// of distance.
#[must_use]
pub fn suggest_city_sites(
    g: &Game,
    p: PlayerId,
    center: TileIdx,
    radius: u32,
    count: usize,
) -> Vec<(TileIdx, f64)> {
    let mut scored: Vec<(TileIdx, f64)> = g
        .grid()
        .within(center, radius)
        .into_iter()
        .filter_map(|t| {
            let s = city_site_score(g, p, t)?;
            Some((t, s - f64::from(g.grid().distance(center, t)) * 2.2))
        })
        .collect();
    // Python's sort was stable on the score alone: equal scores keep the `within` order.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<(TileIdx, f64)> = Vec::new();
    for (t, s) in scored {
        if out.iter().all(|&(o, _)| g.grid().distance(t, o) >= 3) {
            out.push((t, s));
        }
        if out.len() >= count {
            break;
        }
    }
    out
}

// ---- Exploration (automation.py:149-287) ------------------------------------------------------------

/// Whether automation may route unit `u` through tile `t` (`automation._passable`): its terrain
/// lets it.
fn passable(m: &Mover<'_>, t: TileIdx) -> bool {
    m.terrain_reason(t).is_none()
}

/// Whether automation should not send a unit of `p` to tile `t` at all
/// (`automation._off_limits`): land it may not enter, a barbarian camp, a foreign city.
fn off_limits(g: &Game, p: PlayerId, t: TileIdx) -> bool {
    let camp = g.rules().derived().known.barbarian_camp;
    !g.can_enter_territory(p, t)
        || (camp.is_some() && g.tile(t).and_then(Tile::improvement) == camp)
        || g.city_at(t).is_some_and(|c| c.owner() != p)
}

/// How much unexplored ground next to known land lies within two tiles of `t`
/// (`automation._unexplored_near`, `automation.py:227-231`).
#[must_use]
pub fn unexplored_near(g: &Game, p: PlayerId, t: TileIdx) -> u32 {
    let Some(pl) = g.player(p) else { return 0 };
    let grid = g.grid();
    let n = grid
        .within(t, 2)
        .into_iter()
        .filter(|&n| {
            !pl.explored.contains(n.0)
                && grid.neighbors(n).any(|m| pl.explored.contains(m.0) && g.is_land(m))
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Where exploring unit `u` should head next (`automation.explore_target`,
/// `automation.py:174-201`): of the explored tiles within `radius` it may reach and stand clear of
/// danger on (a healthy fighter that is no scout ignores danger), the one with the most unexplored
/// ground around it for its distance, ruins drawing a major; else the nearest frontier anywhere.
#[must_use]
pub fn explore_target(
    g: &Game,
    u: UnitId,
    radius: u32,
    exclude: &BTreeSet<TileIdx>,
) -> Option<TileIdx> {
    let x = g.unit(u)?;
    let p = x.owner();
    let pl = g.player(p)?;
    let r = g.rules();
    let def = &r.base_units()[x.base];
    let scout = r.derived().known.scout;
    let fighter = def.military && Some(def.unit_type) != scout && x.hp >= 60;
    let danger_tiles = if fighter { None } else { danger(g, p).map(|d| d.clone()) };
    let is_danger =
        |t: TileIdx| exclude.contains(&t) || danger_tiles.as_ref().is_some_and(|d| d.contains(t.0));
    let land_unit = def.domain == Domain::Land;
    let m = Mover::unit(g, u)?;
    let grid = g.grid();
    let interesting = |n: TileIdx| {
        !land_unit || grid.neighbors(n).any(|m| pl.explored.contains(m.0) && g.is_land(m))
    };
    let ruins = r.derived().known.ancient_ruins;
    let mut best: Option<TileIdx> = None;
    let mut best_v = 0.0;
    let at = x.tile();
    for t in grid.within(at, radius) {
        if t == at || !pl.explored.contains(t.0) || is_danger(t) {
            continue;
        }
        if !passable(&m, t) || off_limits(g, p, t) {
            continue;
        }
        let unexplored = grid
            .within(t, 2)
            .into_iter()
            .filter(|&n| !pl.explored.contains(n.0) && interesting(n))
            .count();
        if unexplored == 0 {
            continue;
        }
        let d = f64::from(grid.distance(at, t));
        #[allow(clippy::cast_precision_loss, reason = "a count of tiles within two")]
        let mut v = unexplored as f64 / (1.0 + d * 0.6);
        if ruins.is_some() && g.tile(t).and_then(Tile::improvement) == ruins && pl.is_major() {
            v += 6.0;
        }
        if v > best_v {
            best = Some(t);
            best_v = v;
        }
    }
    best.or_else(|| far_frontier(g, u, &m, &is_danger, 60))
}

/// The nearest explored tile anywhere that borders unexplored ground (land, for a land unit),
/// by breadth first over explored tiles it may pass, at most `limit` steps out
/// (`automation._far_frontier`, `automation.py:204-224`).
fn far_frontier(
    g: &Game,
    u: UnitId,
    m: &Mover<'_>,
    is_danger: &dyn Fn(TileIdx) -> bool,
    limit: u32,
) -> Option<TileIdx> {
    let x = g.unit(u)?;
    let p = x.owner();
    let pl = g.player(p)?;
    let land_unit = g.rules().base_units()[x.base].domain == Domain::Land;
    let grid = g.grid();
    let mut seen: BTreeSet<TileIdx> = BTreeSet::new();
    seen.insert(x.tile());
    let mut frontier = vec![x.tile()];
    let mut depth = 0;
    while !frontier.is_empty() && depth < limit {
        depth += 1;
        let mut next = Vec::new();
        for cur in frontier {
            for nb in grid.neighbors(cur) {
                if seen.contains(&nb) || !pl.explored.contains(nb.0) {
                    continue;
                }
                seen.insert(nb);
                if !passable(m, nb) || off_limits(g, p, nb) || is_danger(nb) {
                    continue;
                }
                if grid.neighbors(nb).any(|n| !pl.explored.contains(n.0))
                    && (!land_unit || g.is_land(nb))
                {
                    return Some(nb);
                }
                next.push(nb);
            }
        }
        frontier = next;
    }
    None
}

/// Unit `u` explores (`automation.explore`, `automation.py:234-287`): it heads for its target,
/// keeping it until it is reached so that it does not dither, and picks another (up to six times)
/// when the target turns out unreachable or its unexplored ground cannot be seen from there; a
/// unit back where it was two turns ago gives its target up. With nothing left to explore it stops
/// and says so. The targets given up are kept for the civilization (at most 300), and forgotten
/// every fifteenth turn.
pub fn explore(g: &mut Game, u: UnitId) -> Value {
    let Some(p) = g.unit(u).map(Unit::owner) else { return json!({"exploring": false}) };
    let mut skip: BTreeSet<TileIdx> =
        g.player(p).map(|x| x.civ.explore_skip.iter().copied().collect()).unwrap_or_default();
    let mut tries = 0;
    let mut moved_any = false;
    let set_target = |g: &mut Game, t: Option<TileIdx>| {
        if let Some(x) = g.unit_mut(u, UnitTouch::empty()) {
            x.explore.target = t;
        }
    };
    while tries < 6 && g.unit(u).is_some_and(|x| x.moves > 0) {
        tries += 1;
        let Some((at, mut target)) = g.unit(u).map(|x| (x.tile(), x.explore.target)) else { break };
        if let Some(t0) = target
            && (skip.contains(&t0) || t0 == at || unexplored_near(g, p, t0) == 0)
        {
            if t0 == at && unexplored_near(g, p, t0) > 0 {
                skip.insert(t0);
            }
            target = None;
        }
        if target.is_none() {
            target = explore_target(g, u, 14, &skip);
        }
        let Some(tgt) = target else {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.activity = None;
                x.explore.target = None;
            }
            let text = format!("{} has nothing left to explore nearby.", unit_label(g, u));
            let data = EventData { unit: Some(u), ..EventData::default() };
            g.emit(
                EngineEvent::ExploreDone,
                &text,
                Some(PlayerSet::single(p)),
                Some(at),
                data,
                &[],
            );
            return json!({"exploring": false});
        };
        set_target(g, Some(tgt));
        match movement::move_toward(g, u, tgt, false, false) {
            Err(_) => {
                skip.insert(tgt);
                set_target(g, None);
            }
            Ok(res) => {
                if res.to != Some(res.from) {
                    moved_any = true;
                }
                if let Some(stop) = res.stopped
                    && stop != Stop::OutOfMoves
                {
                    if res.to == Some(res.from) {
                        skip.insert(tgt);
                        set_target(g, None);
                        continue;
                    }
                    break;
                }
            }
        }
    }
    if let Some(at) = g.unit(u).map(Unit::tile) {
        let give_up = {
            let Some(x) = g.unit_mut(u, UnitTouch::CORE) else { return json!({}) };
            x.activity = Some(Activity::Explore);
            let recent = &mut x.explore.recent;
            recent.push(at);
            while recent.len() > 4 {
                recent.remove(0);
            }
            let n = recent.len();
            let back = n >= 3 && recent[n - 1] == recent[n - 3] && recent[n - 1] != recent[n - 2];
            if back { x.explore.target.take() } else { None }
        };
        if let Some(t) = give_up {
            skip.insert(t);
        }
    }
    if g.turn() % 15 == 0 {
        skip.clear();
    }
    let kept: Vec<TileIdx> = {
        let all: Vec<TileIdx> = skip.into_iter().collect();
        all[all.len().saturating_sub(300)..].to_vec()
    };
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.civ.explore_skip = kept;
    }
    json!({"exploring": true, "moved": moved_any})
}

// ---- Worker automation (automation.py:293-451) ------------------------------------------------------

/// Whether civilization `p` has some of luxury `res` for its happiness
/// (`economy.happiness(...)["luxury_types"]`): a major's supply of it is above nothing; no other
/// civilization counts luxuries.
#[must_use]
pub fn luxury_owned(g: &Game, p: PlayerId, res: ResourceId) -> bool {
    g.player(p).is_some_and(crate::state::players::Player::is_major)
        && super::derive::civ::supply(g, p)
            .is_some_and(|s| s.totals().iter().any(|&(r, a)| r == res && a > 0))
}

/// The rough yield gain of building `imp` on tile `t` for civilization `p`
/// (`automation._improvement_value`, `automation.py:293-310`): the improvement's yields, those of
/// the resource it improves (with a bonus by kind, and more for a luxury the civilization lacks),
/// less those of the improvement it replaces and one.
#[must_use]
pub fn improvement_value(g: &Game, p: PlayerId, t: TileIdx, imp: ImprovementId) -> f64 {
    let r = g.rules();
    let Some(tile) = g.tile(t) else { return 0.0 };
    let mut v = weighted(&r.improvements()[imp].stats);
    if let Some(res) = tile.resource()
        && workers::resource_visible(g, p, res)
        && super::tiles::resource_improved_by(g, res, imp)
    {
        let rd = &r.resources()[res];
        v += weighted(&rd.improvement_stats);
        v += match rd.kind {
            ResourceType::Luxury => 6.0,
            ResourceType::Strategic => 4.0,
            _ => 1.0,
        };
        if rd.kind == ResourceType::Luxury && !luxury_owned(g, p, res) {
            v += 8.0;
        }
    }
    if let Some(old) = tile.improvement()
        && old != imp
    {
        v -= weighted(&r.improvements()[old].stats) + 1.0;
    }
    v
}

/// The most valuable improvement `b` could build on tile `t`, and what it is worth
/// (`automation._best_job_uncached`, `automation.py:330-356`): a repair first, at 30; else the
/// best yield gain less 0.15 a turn above 0.5, never a route, a removal or a great improvement,
/// nor anything over a great improvement; else, with fallout on the tile, its removal at 8.
/// `only` limits the improvements looked at, which the job map uses to pass over those its
/// civilization cannot have.
#[must_use]
pub fn best_job(
    g: &Game,
    b: &Builder,
    t: TileIdx,
    only: Option<&ImprovementSet>,
) -> Option<(ImprovementId, f64)> {
    let r = g.rules();
    let tile = g.tile(t)?;
    let opts = workers::build_options(g, b, t, only);
    let mut best: Option<(ImprovementId, f64)> = None;
    let over_great = tile.improvement().is_some_and(|i| r.improvements()[i].great);
    for o in &opts {
        let def = &r.improvements()[o.imp];
        if o.repair {
            return Some((o.imp, 30.0));
        }
        if def.kind != ImprovementKind::Normal || def.great || over_great {
            continue;
        }
        let v = improvement_value(g, b.owner, t, o.imp) - f64::from(o.turns) * 0.15;
        if v > 0.5 && best.is_none_or(|(_, bv)| v > bv) {
            best = Some((o.imp, v));
        }
    }
    let known = &r.derived().known;
    if best.is_none() && tile.features().contains(known.fallout) {
        let removal = r.derived().removal_of.get(known.fallout).copied().flatten();
        if let Some(rem) = removal
            && opts.iter().any(|o| o.imp == rem)
        {
            return Some((rem, 8.0));
        }
    }
    best
}

/// The best job of builder `b` on tile `t`: its civilization's job map's for a builder class
/// (`derive::jobs`), else asked of the tile.
// refcheck: jobs-by-builder-class
fn job_on(g: &Game, b: &Builder, t: TileIdx) -> Option<(ImprovementId, f64)> {
    match b.class {
        Some(class) => super::derive::jobs::job(g, b.owner, class, t),
        None => best_job(g, b, t, None),
    }
}

/// A worker's candidate job: its priority, the tile, the improvement.
type Candidate = (f64, TileIdx, ImprovementId);

/// The best job for automated worker `u` away from the tiles other workers have claimed
/// (`automation.worker_jobs`, `automation.py:359-412`): on the land tiles of its civilization's
/// cities (beyond the work range only for a resource it sees), clear of danger and of other
/// civilians, `10 + 2 x value`, 12 more for a worked tile, less 1.5 a tile of distance; and once
/// it knows roads, the first tile along the line from each city not linked to the capital, within
/// fourteen tiles, not yet a road, at `18 - 1.2 x distance`.
#[must_use]
pub fn worker_jobs(
    g: &Game,
    u: UnitId,
    claimed: &BTreeSet<TileIdx>,
) -> Option<(TileIdx, ImprovementId)> {
    let b = Builder::unit(g, u)?;
    worker_jobs_with(g, u, claimed, |t| job_on(g, &b, t))
}

/// [`worker_jobs`] with the jobs of the tiles from `job`: the job map's, or, for the direct port
/// the tests compare the map with, asked of each tile for the unit itself.
pub fn worker_jobs_with(
    g: &Game,
    u: UnitId,
    claimed: &BTreeSet<TileIdx>,
    mut job: impl FnMut(TileIdx) -> Option<(ImprovementId, f64)>,
) -> Option<(TileIdx, ImprovementId)> {
    let x = g.unit(u)?;
    let p = x.owner();
    let here = x.tile();
    let r = g.rules();
    let grid = g.grid();
    let range = super::cities::stats::work_range(g);
    let danger_tiles = danger(g, p).map(|d| d.clone()).unwrap_or_default();
    let mut candidates: Vec<Candidate> = Vec::new();
    let cities: Vec<(crate::base::ids::CityId, TileIdx)> =
        g.player_cities(p).map(|c| (c.id(), c.tile())).collect();
    for &(c, centre) in &cities {
        let worked: Vec<TileIdx> = g.city(c).map(|x| x.worked.clone()).unwrap_or_default();
        for t in super::economy::city_tiles(g, c) {
            if t == centre || claimed.contains(&t) || danger_tiles.contains(t.0) {
                continue;
            }
            let Some(tile) = g.tile(t) else { continue };
            let visible_res =
                tile.resource().is_some_and(|res| workers::resource_visible(g, p, res));
            if grid.distance(t, centre) > range && !visible_res {
                continue;
            }
            if g.is_water(t) {
                continue;
            }
            if g.civilian_at(t).is_some_and(|o| o.id() != u) {
                continue;
            }
            let Some((imp, value)) = job(t) else { continue };
            let mut prio = 10.0 + value * 2.0;
            if worked.contains(&t) {
                prio += 12.0;
            }
            prio -= f64::from(grid.distance(here, t)) * 1.5;
            candidates.push((prio, t, imp));
        }
    }
    let known = &r.derived().known;
    let road = known.road;
    if g.has_tech(p, r.improvements()[road].tech_required) {
        let capital = g.player(p).and_then(|x| x.capital).and_then(|c| g.city(c));
        if let Some(cap) = capital {
            let connected = super::query::connectivity(g, p);
            for &(c, centre) in &cities {
                if connected.media(c).is_some()
                    || c == cap.id()
                    || grid.distance(centre, cap.tile()) > 14
                {
                    continue;
                }
                let line = grid.line(centre, cap.tile());
                let inner = if line.len() >= 2 { &line[1..line.len() - 1] } else { &[][..] };
                for &t in inner {
                    let Some(tile) = g.tile(t) else { break };
                    if g.is_water(t) || super::tiles::is_impassable(g, t) {
                        break;
                    }
                    if claimed.contains(&t)
                        || danger_tiles.contains(t.0)
                        || (tile.route().is_some() && !tile.route_pillaged())
                    {
                        continue;
                    }
                    if tile.owner().is_some_and(|o| o != p) {
                        break;
                    }
                    candidates.push((18.0 - f64::from(grid.distance(here, t)) * 1.2, t, road));
                    break;
                }
            }
        }
    }
    // refcheck: jobs-tie-by-ruleset-order
    candidates
        .into_iter()
        .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)))
        .map(|(_, t, imp)| (t, imp))
}

/// Sends automated worker `u` to its next job (`automation.automate_worker`,
/// `automation.py:415-451`): in danger, or in a city with enemies near, it retreats to the
/// nearest city; on a tile it is building, it keeps at it; otherwise it claims its best job, walks
/// there, and starts it once it arrives.
pub fn automate_worker(g: &mut Game, u: UnitId, claimed: &mut BTreeSet<TileIdx>) -> Value {
    let Some((p, at)) = g.unit(u).map(|x| (x.owner(), x.tile())) else { return Value::Null };
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.activity = Some(Activity::Automate);
    }
    if civilian_in_danger(g, u) || (g.city_at(at).is_some() && enemy_near(g, u, 2)) {
        let mut safe: Option<(u32, TileIdx)> = None;
        for c in g.player_cities(p) {
            let d = g.grid().distance(c.tile(), at);
            if safe.is_none_or(|(sd, _)| d < sd) {
                safe = Some((d, c.tile()));
            }
        }
        if let Some((_, to)) = safe
            && to != at
        {
            let _moved = movement::move_toward(g, u, to, false, false).is_ok();
        }
        return json!({"status": "retreating"});
    }
    if let (Some(b), Some(step)) =
        (Builder::unit(g, u), g.state().tiles().builds(at).first().copied())
        && step.turns_left >= 0
        && workers::unit_can_build(g, &b, step.improvement, at)
    {
        claimed.insert(at);
        let last = g.state().tiles().builds(at).last().map_or(step.improvement, |s| s.improvement);
        let (x, y) = g.xy(at);
        let job = g.rules().improvements()[last].name.to_string();
        return json!({"status": "working", "tile": {"x": x, "y": y}, "job": job});
    }
    let Some((t, imp)) = worker_jobs(g, u, claimed) else { return json!({"status": "idle"}) };
    claimed.insert(t);
    if at != t && movement::move_toward(g, u, t, false, false).is_err() {
        return json!({"status": "no path"});
    }
    if g.unit(u).is_some_and(|x| x.tile() == t) {
        let name = g.rules().improvements()[imp].name.to_string();
        if let Ok(plan) = workers::plan_build(g, u, &name) {
            workers::apply_build(g, plan);
        }
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            x.activity = Some(Activity::Automate);
        }
    }
    let (x, y) = g.xy(t);
    let job = g.rules().improvements()[imp].name.to_string();
    json!({"status": "working", "tile": {"x": x, "y": y}, "job": job})
}
