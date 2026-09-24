//! Paths (DESIGN.md 6.10): how a unit may move, what a step costs, and the searches over steps.
//! Ports the passability, cost and search half of `movement.py:22-510`; the steps themselves,
//! standing orders and the rest are `game::movement`'s.
//!
//! - [`class`]: what decides how a unit moves: its profile ([`Profile`], `movement.profile`), its
//!   civilization's movement rules ([`CivMove`]) and the ruleset's names ([`MoveRules`]), taken
//!   once per search into a [`Mover`];
//! - [`node`]: whether it may pass through a tile, end its move there, or route through it, with
//!   Python's reasons ([`Blocked`]);
//! - [`cost`]: what one step costs (`movement.enter_cost`), with routes, rivers and zones of
//!   control;
//! - [`astar`]: the best path (`find_path`), the turns a path takes and what a unit reaches this
//!   turn, over [`Label`]s;
//! - [`tree`]: one bounded search that answers paths to many tiles ([`PathTree`]).
//!
//! **What is cached, and what is not.** A mover takes a unit's profile, its civilization's rules,
//! whom it is at war with, whose land it may enter and the zones of control once, for one search;
//! the tiles themselves are read as they are, each a handful of loads, so nothing per tile has to
//! follow the map's changes. The game keeps the ruleset's names ([`MoveRules`]), two memos the
//! search's bound reads (the cheapest terrain on the map, [`TerrainFloor`], and how far each tile
//! is from the routes, [`RouteNet`]), and the search's scratch arrays (`PathScratch`), which a
//! generation stamp resets without clearing.

pub mod astar;
pub mod class;
pub mod cost;
pub mod node;
pub mod tree;

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

pub use self::astar::{Label, PathCache, PathKey, PathScratch, Start};
pub use self::class::{CivMove, Double, DoubleOn, MoveRules, Mover, OceanFor, Profile};
pub use self::node::{Blocked, air_capacity_ok, can_carry, first_unit, stack_reason};
pub use self::tree::PathTree;

/// Movement meaning "all that is left" (`movement.ALL`, `movement.py:19`).
pub const ALL: i32 = 1_000_000;

/// The least movement cost of any tile's governing terrain, in movement points (a city's tile
/// costs one): what the search's bound assumes of a step off the routes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainFloor {
    pub terrain: i32,
}

impl crate::game::derive::rev::BitEq for TerrainFloor {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// The least movement cost of any tile's governing terrain, as [`TerrainFloor`] says.
#[must_use]
pub fn terrain_floor(g: &crate::game::Game) -> TerrainFloor {
    let terrains = g.rules().terrains();
    let mut terrain = if g.state().cities().iter().next().is_some() { 1 } else { i32::MAX };
    for (_, t) in g.state().tiles().iter() {
        terrain = terrain.min(terrains[node::governing(g, t)].movement_cost);
    }
    if terrain == i32::MAX {
        terrain = 1;
    }
    TerrainFloor { terrain: terrain.max(0) }
}

/// How far each tile is from the routes (DESIGN.md 6.10's `RouteLayer`, as built): steps to the
/// nearest tile with a road or railroad, and to the nearest railroad, as `route_at` gives them (a
/// city's tile by its owner's techs); [`RouteNet::NONE`] where the map has no such tile. The
/// search's bound reads it: a step can be a road step only between two tiles with routes, and a
/// railroad step only between two railroads, so a tile far from them is far from their prices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteNet {
    pub road: Vec<u16>,
    pub rail: Vec<u16>,
}

impl RouteNet {
    /// No such route on the map.
    pub const NONE: u16 = u16::MAX;
}

impl crate::game::derive::rev::BitEq for RouteNet {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// Where the routes run and how far each tile is from them, as [`RouteNet`] says.
#[must_use]
pub fn route_net(g: &crate::game::Game) -> RouteNet {
    use crate::rules::defs::Route;
    let rules = g.derived().move_rules();
    let n = g.state().map().size();
    let mut road = vec![RouteNet::NONE; n as usize];
    let mut rail = vec![RouteNet::NONE; n as usize];
    let grid = g.grid();
    let routes: Vec<Option<Route>> =
        (0..n).map(|i| cost::route_at(g, rules, crate::base::ids::TileIdx(i))).collect();
    // A step is a road step only between two tiles with routes, and a rail step only between two
    // railroads: a tile with no such neighbour starts none (a lone city, a road's dead end is
    // still one).
    let (mut roads, mut rails) = (Vec::new(), Vec::new());
    for i in 0..n {
        let t = crate::base::ids::TileIdx(i);
        let Some(route) = routes[i as usize] else { continue };
        let mut near =
            grid.neighbor_table(t).into_iter().filter(|&m| m != crate::base::hex::NO_TILE);
        let (mut any, mut railed) = (false, false);
        for m in near.by_ref() {
            match routes[m as usize] {
                Some(Route::Railroad) => {
                    any = true;
                    railed = true;
                }
                Some(Route::Road) => any = true,
                None => {}
            }
        }
        if any {
            road[i as usize] = 0;
            roads.push(t);
        }
        if route == Route::Railroad && railed {
            rail[i as usize] = 0;
            rails.push(t);
        }
    }
    spread(grid, &mut road, roads);
    spread(grid, &mut rail, rails);
    RouteNet { road, rail }
}

/// Steps from the nearest tile of `queue` to every tile, over neighbours: a breadth-first search
/// from all of them at once. `dist` holds 0 at each of them and [`RouteNet::NONE`] elsewhere.
fn spread(
    grid: &crate::base::hex::HexGrid,
    dist: &mut [u16],
    mut queue: Vec<crate::base::ids::TileIdx>,
) {
    let mut head = 0;
    while let Some(&t) = queue.get(head) {
        head += 1;
        let next = dist[t.0 as usize].saturating_add(1);
        for n in grid.neighbor_table(t) {
            if n != crate::base::hex::NO_TILE && dist[n as usize] == RouteNet::NONE {
                dist[n as usize] = next;
                queue.push(crate::base::ids::TileIdx(n));
            }
        }
    }
}
