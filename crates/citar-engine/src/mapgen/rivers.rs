//! Rivers from high ground down to the sea, along the edges between tiles (`mapgen.py:754-880`:
//! `_dir`, `_paint_river`, `_common`, `_salt`, `_rivers`; UnCiv's `RiverGenerator`).
//!
//! Rivers run between tiles, so the walk is over hex corners, each where three tiles meet;
//! stepping to the next corner runs the river along the edge the two corners share. Every corner
//! first learns its way to the sea: a shortest-path search outward from the river mouths
//! (corners where two land tiles meet the sea), over edges between two land tiles, with a random
//! cost per edge so the channels meander and a penalty between two mountains. That gives a
//! drainage tree, and every river follows it down: a river always reaches the sea (a source
//! that cannot drain is never used), and two rivers that meet merge into one channel instead of
//! crossing, as tributaries do.
//!
//! Each edge's cost is drawn once, in edge order, where Python drew it the first time the search
//! reached the edge; ties in the search go to the corner pushed first, where Python drew a
//! random tie-break for each push.

use super::landmass::distance_from;
use super::map::GenMap;
use super::spread::spread_out;
use crate::base::collections::MinHeap;
use crate::base::hex::{Dir, HexGrid};
use crate::base::ids::TileIdx;
use crate::base::num::{floor_i64, round_half_even};
use crate::base::order::{TotalF64, argmax_first};
use crate::base::rng::Rng;

/// No corner, no edge.
const NONE: u32 = u32::MAX;

/// The corners of a grid: every three tiles that touch one another, each once, sorted.
#[derive(Debug)]
pub(crate) struct Corners {
    /// Each corner's three tiles, ascending.
    pub tiles: Vec<[u32; 3]>,
    /// For each corner, its three edges as `(the other corner of the edge, edge tile a, edge
    /// tile b)`; the other corner is [`NONE`] at the map's edge.
    pub steps: Vec<[(u32, u32, u32); 3]>,
    /// Each tile's corners, in direction order (the corner between direction d and d + 1).
    pub of_tile: Vec<[u32; 6]>,
}

/// The direction from `a` to its neighbour `b` (`_dir`).
pub(crate) fn dir(grid: &HexGrid, a: TileIdx, b: TileIdx) -> Option<Dir> {
    Dir::ALL.into_iter().find(|&d| grid.neighbor(a, d) == Some(b))
}

impl Corners {
    pub(crate) fn new(grid: &HexGrid) -> Self {
        let n = grid.size() as usize;
        let mut tiles: Vec<[u32; 3]> = Vec::with_capacity(n * 2);
        for a in grid.tiles() {
            let nb = grid.neighbor_table(a);
            for d in 0..6 {
                let (b, c) = (nb[d], nb[(d + 1) % 6]);
                if b != NONE && c != NONE && a.0 < b && a.0 < c {
                    let mut k = [a.0, b, c];
                    k.sort();
                    tiles.push(k);
                }
            }
        }
        tiles.sort();
        tiles.dedup();
        let find = |k: [u32; 3]| -> u32 {
            tiles.binary_search(&k).map_or(NONE, |i| u32::try_from(i).unwrap_or(NONE))
        };
        let mut of_tile = vec![[NONE; 6]; n];
        for a in grid.tiles() {
            let nb = grid.neighbor_table(a);
            for d in 0..6 {
                let (b, c) = (nb[d], nb[(d + 1) % 6]);
                if b != NONE && c != NONE {
                    let mut k = [a.0, b, c];
                    k.sort();
                    of_tile[a.0 as usize][d] = find(k);
                }
            }
        }
        let steps = tiles
            .iter()
            .map(|&v| {
                let pairs = [(v[0], v[1], v[2]), (v[0], v[2], v[1]), (v[1], v[2], v[0])];
                pairs.map(|(a, b, not)| {
                    // The other common neighbour of a and b is the edge's other corner.
                    let other = common(grid, TileIdx(a), TileIdx(b))
                        .into_iter()
                        .flatten()
                        .find(|c| c.0 != not)
                        .map_or(NONE, |c| {
                            let mut k = [a, b, c.0];
                            k.sort();
                            find(k)
                        });
                    (other, a, b)
                })
            })
            .collect();
        Self { tiles, steps, of_tile }
    }

    /// The number of corners.
    pub(crate) fn len(&self) -> usize {
        self.tiles.len()
    }
}

/// The tiles next to both `a` and `b` (`_common`): the two corners of their edge.
fn common(grid: &HexGrid, a: TileIdx, b: TileIdx) -> [Option<TileIdx>; 2] {
    let mut out = [None; 2];
    let mut k = 0;
    for c in grid.neighbors(a) {
        if k < 2 && grid.neighbors(b).any(|x| x == c) {
            out[k] = Some(c);
            k += 1;
        }
    }
    out
}

/// Draws a river along the edge between two neighbours, on both tiles (`_paint_river`).
pub(crate) fn paint(m: &mut GenMap<'_>, a: TileIdx, b: TileIdx) {
    let Some(d) = dir(&m.grid, a, b) else { return };
    let ta = m.tile(a).with_river(m.tile(a).river_mask() | (1 << d.index()));
    m.set(a, ta);
    let tb = m.tile(b).with_river(m.tile(b).river_mask() | (1 << d.opposite().index()));
    m.set(b, tb);
}

/// The key of the edge between neighbours `a` and `b`: the lower tile and the direction to the
/// other.
fn edge_key(grid: &HexGrid, a: u32, b: u32) -> Option<usize> {
    let (lo, hi) = (a.min(b), a.max(b));
    dir(grid, TileIdx(lo), TileIdx(hi)).map(|d| lo as usize * 6 + d.index())
}

/// Traces the rivers (`_rivers`): about one per hundred land tiles, times the lobby's river
/// density.
pub(crate) fn rivers(m: &mut GenMap<'_>, rng: &mut Rng) {
    let grid = m.grid.clone();
    let land: Vec<TileIdx> = m.all().filter(|&t| m.land(t)).collect();
    let n = floor_i64(round_half_even(land.len() as f64 * 0.01 * m.opts.rivers));
    let n = usize::try_from(n).unwrap_or(0);
    if land.is_empty() || land.len() == grid.size() as usize || n == 0 {
        return;
    }
    let corners = Corners::new(&grid);
    let is_land: Vec<bool> = m.all().map(|t| m.land(t)).collect();
    let is_salt: Vec<bool> = m.all().map(|t| m.salt(t)).collect();
    let mountain: Vec<bool> = m.all().map(|t| m.mountain(t)).collect();

    // Each edge between two land tiles gets its cost, in edge order.
    let mut cost = vec![f64::NAN; grid.size() as usize * 6];
    for a in grid.tiles() {
        for d in Dir::ALL {
            let Some(b) = grid.neighbor(a, d) else { continue };
            if b.0 < a.0 || !is_land[a.0 as usize] || !is_land[b.0 as usize] {
                continue;
            }
            let both = mountain[a.0 as usize] && mountain[b.0 as usize];
            if let Some(k) = edge_key(&grid, a.0, b.0) {
                cost[k] = 1.0 + rng.unit() * 1.5 + if both { 4.0 } else { 0.0 };
            }
        }
    }

    // The drainage: from the mouths, two land tiles and one of sea, so a river's last edge runs
    // straight into the sea.
    let mut dist = vec![f64::INFINITY; corners.len()];
    let mut down: Vec<(u32, u32, u32)> = vec![(NONE, NONE, NONE); corners.len()];
    let mut heap: MinHeap<TotalF64, u32> = MinHeap::with_capacity(corners.len());
    for (v, k) in corners.tiles.iter().enumerate() {
        let salt = k.iter().filter(|&&t| is_salt[t as usize]).count();
        let lands = k.iter().filter(|&&t| is_land[t as usize]).count();
        if salt == 1 && lands == 2 {
            dist[v] = 0.0;
            heap.push(TotalF64(0.0), u32::try_from(v).unwrap_or(NONE));
        }
    }
    while let Some((TotalF64(d), v)) = heap.pop() {
        let vi = v as usize;
        if d > dist[vi] {
            continue;
        }
        for &(u, a, b) in &corners.steps[vi] {
            if u == NONE || !is_land[a as usize] || !is_land[b as usize] {
                continue;
            }
            let Some(c) = edge_key(&grid, a, b).map(|k| cost[k]) else { continue };
            let nd = d + c;
            if nd < dist[u as usize] {
                dist[u as usize] = nd;
                down[u as usize] = (v, a, b);
                heap.push(TotalF64(nd), u);
            }
        }
    }
    let drains = |v: u32| v != NONE && down[v as usize].0 != NONE;

    // Sources: high inland tiles, spread out. A source corner lies wholly on land and drains.
    let to_sea = distance_from(&grid, &is_salt, u32::from(grid.width()) + u32::from(grid.height()));
    let reach = |t: TileIdx| {
        let d = to_sea[t.0 as usize];
        if d == u32::MAX { 0 } else { d }
    };
    let far: Vec<TileIdx> = land.iter().copied().filter(|&t| reach(t) >= 3).collect();
    let mut opts: Vec<TileIdx> = far.iter().copied().filter(|&t| mountain[t.0 as usize]).collect();
    if opts.len() < n {
        // The options so far are the far mountains: the mask says which, without a search.
        opts.extend(far.iter().copied().filter(|&t| m.hill(t) && !mountain[t.0 as usize]));
    }
    if opts.len() < n {
        opts = if far.is_empty() {
            land.iter().copied().filter(|&t| reach(t) >= 2).collect()
        } else {
            far.clone()
        };
    }
    let mut on_river = vec![false; corners.len()];
    let mut path: Vec<(u32, u32)> = Vec::new();
    let mut visited: Vec<u32> = Vec::new();
    for s in spread_out(m, rng, n, &opts) {
        let heads = corners.of_tile[s.0 as usize].into_iter().filter(|&v| {
            drains(v) && corners.tiles[v as usize].iter().all(|&t| is_land[t as usize])
        });
        let Some(mut v) = argmax_first(heads, |&v| TotalF64(dist[v as usize])) else { continue };
        if on_river[v as usize] {
            continue;
        }
        path.clear();
        visited.clear();
        while drains(v) && !on_river[v as usize] {
            let (next, a, b) = down[v as usize];
            path.push((a, b));
            visited.push(v);
            v = next;
        }
        if path.len() < 2 {
            continue;
        }
        // v is a mouth, or a corner of an earlier river this one flows into.
        for &x in &visited {
            on_river[x as usize] = true;
        }
        on_river[v as usize] = true;
        for &(a, b) in &path {
            paint(m, TileIdx(a), TileIdx(b));
        }
    }
}
