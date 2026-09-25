//! Paths (DESIGN.md 6.10): how a unit may move, what a step costs, and the searches over steps.
//! Ports the passability, cost and search half of `movement.py:22-510`; the steps themselves,
//! standing orders and the rest are `game::movement`'s.
//!
//! - [`class`]: what decides how a unit moves: its profile ([`Profile`], `movement.profile`), its
//!   civilization's movement rules ([`CivMove`]) and the ruleset's names ([`MoveRules`], resolved
//!   at load), taken once per search into a [`Mover`];
//! - `memo`: the game's memos those are taken from, each checked against what it reads;
//! - [`node`]: whether it may pass through a tile, end its move there, or route through it, with
//!   Python's reasons ([`Blocked`]);
//! - [`cost`]: what one step costs (`movement.enter_cost`), with routes, rivers and zones of
//!   control;
//! - [`astar`]: the best path (`find_path`), the turns a path takes and what a unit reaches this
//!   turn, over [`Label`]s;
//! - [`tree`]: one bounded search that answers paths to many tiles ([`PathTree`]).
//!
//! **What is cached, and what is not.** A mover takes a unit's profile, its civilization's rules,
//! whom it is at war with, whose land it may enter and the zones of control once, for one search,
//! from memos that are checked against the revisions of what they read (`memo`); the tiles
//! themselves are read as they are, each a handful of loads, so nothing per tile has to follow
//! the map's changes. The search's bound reads two facts of the map the game keeps up to date
//! from the tile log and the routes: whether a tile costs nothing to enter
//! (`TerrainFloorMemo`, looked at only when the ruleset has such a terrain), and how far each
//! tile is from the routes ([`RouteNet`]). The search's scratch arrays (`PathScratch`) are reused,
//! a generation stamp resetting them without clearing.

pub mod astar;
pub mod class;
pub mod cost;
pub(crate) mod memo;
pub mod node;
pub mod tree;

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

pub use self::astar::{Label, PathCache, PathKey, PathScratch, Start};
pub use self::class::{CivMove, Double, DoubleOn, MoveRules, Mover, OceanFor, Profile};
pub use self::node::{Blocked, air_capacity_ok, can_carry, first_unit, stack_reason};
pub use self::tree::PathTree;

use crate::base::hex::{HexGrid, NO_TILE};
use crate::base::ids::TileIdx;
use crate::base::sets::PlayerSet;
use crate::game::Game;
use crate::game::derive::rev::Rev;
use crate::rules::defs::Route;

/// Movement meaning "all that is left" (`movement.ALL`, `movement.py:19`).
pub const ALL: i32 = 1_000_000;

/// How far each tile is from the routes (DESIGN.md 6.10's `RouteLayer`, as built): steps to the
/// nearest tile that can start a road step, and to the nearest that can start a railroad step,
/// as `route_at` gives the routes (a city's tile by its owner's techs); [`RouteNet::NONE`] where
/// the map has no such tile. The search's bound reads it: a step can be a road step only between
/// two tiles with routes, and a railroad step only between two railroads, so a tile far from them
/// is far from their prices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteNet {
    pub road: Vec<u16>,
    pub rail: Vec<u16>,
}

impl RouteNet {
    /// No such route on the map.
    pub const NONE: u16 = u16::MAX;

    /// Makes each of `tiles` that can start a road or railroad step a source, and lowers every
    /// distance the new sources shorten: a breadth-first search from all of them at once, which
    /// visits a tile again only when it comes nearer. From an empty net and every tile, the cold
    /// build; after routes were only added, the distances a cold build finds, since adding a
    /// route only ever makes a tile a source.
    fn spread_from(&mut self, grid: &HexGrid, routes: &[Option<Route>], tiles: &[TileIdx]) {
        let (mut roads, mut rails) = (Vec::new(), Vec::new());
        for &t in tiles {
            let Some(route) = routes.get(t.0 as usize).copied().flatten() else { continue };
            // A step is a road step only between two tiles with routes, and a rail step only
            // between two railroads: a tile with no such neighbour starts none (a lone city; a
            // road's dead end is still one).
            let (mut any, mut railed) = (false, false);
            for m in grid.neighbor_table(t) {
                match routes.get(m as usize).copied().flatten() {
                    Some(Route::Railroad) => {
                        any = true;
                        railed = true;
                    }
                    Some(Route::Road) => any = true,
                    None => {}
                }
            }
            let i = t.0 as usize;
            if any && self.road[i] != 0 {
                self.road[i] = 0;
                roads.push(t);
            }
            if route == Route::Railroad && railed && self.rail[i] != 0 {
                self.rail[i] = 0;
                rails.push(t);
            }
        }
        spread(grid, &mut self.road, roads);
        spread(grid, &mut self.rail, rails);
    }
}

/// Where the routes run and how far each tile is from them, as [`RouteNet`] says: a cold look at
/// the map, which the cache oracle compares the game's `RouteNetMemo` with.
#[must_use]
pub fn route_net(g: &Game) -> RouteNet {
    let routes = routes_of(g);
    let n = routes.len();
    let mut net = RouteNet { road: vec![RouteNet::NONE; n], rail: vec![RouteNet::NONE; n] };
    let all: Vec<TileIdx> = (0..g.state().map().size()).map(TileIdx).collect();
    net.spread_from(g.grid(), &routes, &all);
    net
}

/// The route each tile gives, by tile (`movement.route_at`).
fn routes_of(g: &Game) -> Vec<Option<Route>> {
    let rules = &g.rules().derived().moves;
    (0..g.state().map().size()).map(|i| cost::route_at(g, rules, TileIdx(i))).collect()
}

/// Lowers the distances from the tiles of `queue`, each just made a source (distance 0), to every
/// tile they come nearer to, over neighbours, level by level.
fn spread(grid: &HexGrid, dist: &mut [u16], mut queue: Vec<TileIdx>) {
    let mut head = 0;
    while let Some(&t) = queue.get(head) {
        head += 1;
        let next = dist[t.0 as usize].saturating_add(1);
        for n in grid.neighbor_table(t) {
            if n != NO_TILE && dist[n as usize] > next {
                dist[n as usize] = next;
                queue.push(TileIdx(n));
            }
        }
    }
}

/// Whether any tile of the map is governed by a terrain that costs nothing to enter, kept up to
/// date from the tile log: what the search's bound assumes of a step off the routes, when the
/// ruleset has such a terrain ([`MoveRules::terrain_floor`] below one). A tile written is looked
/// at again alone; nothing else moves it.
#[derive(Clone, Debug, Default)]
pub(crate) struct TerrainFloorMemo {
    /// The revision it was last brought up to date at; [`Rev::NEVER`] before its first look.
    at: Rev,
    /// Per tile, whether its governing terrain costs nothing.
    free: Vec<bool>,
    /// How many tiles are.
    count: u32,
}

impl TerrainFloorMemo {
    /// The least movement points a step off the routes may cost: 0 if a tile costs nothing to
    /// enter, else 1.
    pub(crate) const fn floor(&self) -> i32 {
        if self.count > 0 { 0 } else { 1 }
    }

    /// Whether it was brought up to date at revision `now`, and needs no look.
    pub(crate) fn current(&self, now: Rev) -> bool {
        self.at == now && now != Rev::NEVER
    }

    /// Brings it up to date with game `g`, whose revision is `now`.
    pub(crate) fn update(&mut self, g: &Game, now: Rev) {
        let n = g.state().map().size() as usize;
        let changed = if self.at == Rev::NEVER || self.free.len() != n {
            None
        } else {
            g.derived().revs().tile_log.since(self.at)
        };
        match changed {
            Some(tiles) => {
                for t in tiles {
                    let now_free = costs_nothing(g, t);
                    if let Some(was) = self.free.get_mut(t.0 as usize)
                        && *was != now_free
                    {
                        *was = now_free;
                        self.count = if now_free { self.count + 1 } else { self.count - 1 };
                    }
                }
            }
            None => {
                self.free =
                    (0..g.state().map().size()).map(|i| costs_nothing(g, TileIdx(i))).collect();
                self.count = self.free.iter().map(|&f| u32::from(f)).sum();
            }
        }
        self.at = now;
    }
}

/// The least movement points a step off the routes may cost on game `g`'s map, as
/// `TerrainFloorMemo` says: a cold look at the map, which the cache oracle compares with.
#[must_use]
pub fn terrain_floor(g: &Game) -> i32 {
    let rules = &g.rules().derived().moves;
    if rules.terrain_floor >= 1
        || !(0..g.state().map().size()).any(|i| costs_nothing(g, TileIdx(i)))
    {
        return 1;
    }
    0
}

/// Whether tile `t`'s governing terrain costs nothing to enter.
fn costs_nothing(g: &Game, t: TileIdx) -> bool {
    g.tile(t).is_some_and(|x| g.rules().terrains()[node::governing(g, x)].movement_cost <= 0)
}

/// The game's [`RouteNet`], kept up to date as the routes change (DESIGN.md 6.10). The net reads
/// the routes, the cities (a city's tile is a route by its owner's techs) and which civilizations
/// know the road's and the railroad's techs, and nothing else a civilization learns or builds:
/// it is checked against the `routes` and `cities` revisions and those two sets of players, read
/// in one pass over the players. A route built or repaired is taken in where it was built: only
/// the tiles it brings nearer are visited. A route lost (removed or pillaged, or a railroad
/// become a road), a city founded, lost or taken, or a civilization learning either tech builds
/// the net afresh.
#[derive(Clone, Debug, Default)]
pub(crate) struct RouteNetMemo {
    net: RouteNet,
    /// The route each tile gave when the net was last brought up to date.
    routes: Vec<Option<Route>>,
    /// The revision it was last brought up to date at; [`Rev::NEVER`] before its first build.
    at: Rev,
    /// The players who knew the road's tech and the railroad's then.
    road: PlayerSet,
    rail: PlayerSet,
    /// How many times it was built afresh, for tests.
    builds: u32,
}

impl RouteNetMemo {
    /// The net, as of its last update.
    pub(crate) const fn net(&self) -> &RouteNet {
        &self.net
    }

    /// Whether it was brought up to date at revision `now`, and needs no look.
    pub(crate) fn current(&self, now: Rev) -> bool {
        self.at == now && now != Rev::NEVER
    }

    /// How many times the net was built afresh.
    #[cfg(test)]
    pub(crate) const fn builds(&self) -> u32 {
        self.builds
    }

    /// Brings the net up to date with game `g`, whose revision is `now`.
    pub(crate) fn update(&mut self, g: &Game, now: Rev) {
        let revs = g.derived().revs();
        let rules = &g.rules().derived().moves;
        let knows = |tech| -> PlayerSet {
            g.state().players().ids().filter(|&p| g.has_tech(p, tech)).collect()
        };
        let (road, rail) = (knows(rules.road_tech), knows(rules.rail_tech));
        let fresh = self.at == Rev::NEVER
            || revs.cities > self.at
            || (road, rail) != (self.road, self.rail)
            || self.routes.len() != g.state().map().size() as usize;
        if fresh || (revs.routes > self.at && !self.take_added(g, revs.tile_log.since(self.at))) {
            self.rebuild(g);
        }
        self.road = road;
        self.rail = rail;
        self.at = now;
    }

    /// Takes in the routes changed on the tiles `changed` lists, if every change added a route
    /// or made a road a railroad; `false`, with nothing written, if any took one away, or if the
    /// log no longer reaches back far enough to say.
    fn take_added(&mut self, g: &Game, changed: Option<impl Iterator<Item = TileIdx>>) -> bool {
        let Some(changed) = changed else { return false };
        let rules = &g.rules().derived().moves;
        let mut added: Vec<(TileIdx, Option<Route>)> = Vec::new();
        for t in changed {
            let Some(&was) = self.routes.get(t.0 as usize) else { return false };
            let now = cost::route_at(g, rules, t);
            if now == was || added.iter().any(|&(x, _)| x == t) {
                continue;
            }
            if matches!((was, now), (Some(_), None) | (Some(Route::Railroad), Some(Route::Road))) {
                return false;
            }
            added.push((t, now));
        }
        let grid = g.grid();
        let mut near: Vec<TileIdx> = Vec::with_capacity(added.len() * 7);
        for &(t, route) in &added {
            self.routes[t.0 as usize] = route;
            // A new route may make its tile a source, and its neighbours that have routes too.
            near.push(t);
            near.extend(grid.neighbors(t));
        }
        self.net.spread_from(grid, &self.routes, &near);
        true
    }

    /// Builds the net afresh.
    fn rebuild(&mut self, g: &Game) {
        self.routes = routes_of(g);
        let n = self.routes.len();
        self.net = RouteNet { road: vec![RouteNet::NONE; n], rail: vec![RouteNet::NONE; n] };
        let all: Vec<TileIdx> = (0..g.state().map().size()).map(TileIdx).collect();
        self.net.spread_from(g.grid(), &self.routes, &all);
        self.builds = self.builds.saturating_add(1);
    }
}
