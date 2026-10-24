//! The land's shape: the polar ice, how land-like each tile is, and the cut that makes a share of
//! the map land (`mapgen.py:195-356, 1680-1688`: `_land_scores`, `_distance_from`, `_ice_band`,
//! `_ice_profile`, `_threshold`, and the land mask of `generate_map`).
//!
//! The map types differ only here: the same noise is masked differently, pushed away from the
//! edges, split down the middle or broken up, and everything after is the same. On a wrapping
//! axis every distance is measured the short way round, so a continent can straddle the seam.

use super::noise::{ROW, fractal_field, pos};
use super::options::{IceSides, MapType};
use crate::base::hex::HexGrid;
use crate::base::ids::TileIdx;
use crate::base::num::{ceil_i64, hypot, round_half_even, trunc_i64};
use crate::base::order::TotalF64;
use crate::base::rng::Rng;

/// A score no threshold reaches: the ice is always sea.
const SEA: f64 = -1e9;

/// Steps from the nearest source tile, for every tile within `limit` of one; `u32::MAX`
/// elsewhere (`_distance_from`).
pub(crate) fn distance_from(grid: &HexGrid, sources: &[bool], limit: u32) -> Vec<u32> {
    let mut dist = vec![u32::MAX; sources.len()];
    let mut frontier: Vec<TileIdx> = Vec::new();
    for t in grid.tiles() {
        if sources[t.0 as usize] {
            dist[t.0 as usize] = 0;
            frontier.push(t);
        }
    }
    let mut next = Vec::new();
    for d in 1..=limit {
        next.clear();
        for &c in &frontier {
            for n in grid.neighbors(c) {
                if dist[n.0 as usize] == u32::MAX {
                    dist[n.0 as usize] = d;
                    next.push(n);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        core::mem::swap(&mut frontier, &mut next);
    }
    dist
}

/// The polar ice: a band one to four tiles deep along each capped side (`_ice_band`).
///
/// The depth drifts slowly along the edge, a smooth profile rather than noise, so the ice reads
/// as a ragged but essentially straight shelf. On a small map the band is thinner, so it never
/// takes more than about a quarter of the height.
pub(crate) fn ice_band(rng: &mut Rng, grid: &HexGrid, sides: IceSides) -> Vec<bool> {
    let (w, h) = (u32::from(grid.width()), u32::from(grid.height()));
    let mut band = vec![false; (w * h) as usize];
    let each = [
        (sides.north, Side::North),
        (sides.south, Side::South),
        (sides.west, Side::West),
        (sides.east, Side::East),
    ];
    for (on, side) in each {
        if !on {
            continue;
        }
        let north_south = matches!(side, Side::North | Side::South);
        let (along, across) = if north_south { (w, h) } else { (h, w) };
        let periodic = if north_south { grid.wrap_x() } else { grid.wrap_y() };
        let hi = max_depth(across);
        for (s, depth) in ice_profile(rng, along, periodic, 1, hi).into_iter().enumerate() {
            let s = u32::try_from(s).unwrap_or(0);
            for d in 0..depth {
                let (x, y) = match side {
                    Side::North => (s, d),
                    Side::South => (s, h - 1 - d),
                    Side::West => (d, s),
                    Side::East => (w - 1 - d, s),
                };
                band[(y * w + x) as usize] = true;
            }
        }
    }
    band
}

/// The deepest the ice reaches from a side `across` tiles long: 1 to 4 (`_ice_band`).
#[must_use]
pub fn max_depth(across: u32) -> u32 {
    (across / 8).clamp(1, 4)
}

#[derive(Clone, Copy)]
enum Side {
    North,
    South,
    West,
    East,
}

/// Depths `lo..=hi` along an edge of `n` tiles: smoothed random control points about seven tiles
/// apart, periodic along a wrapping edge so the shelf meets itself at the seam (`_ice_profile`).
fn ice_profile(rng: &mut Rng, n: u32, periodic: bool, lo: u32, hi: u32) -> Vec<u32> {
    if hi <= lo {
        return vec![lo; n as usize];
    }
    let k = trunc_i64(round_half_even(f64::from(n) / 7.0)).max(1);
    let k = usize::try_from(k).unwrap_or(1);
    let step = f64::from(n) / k as f64;
    let count = if periodic { k } else { k + 1 };
    let pts: Vec<f64> = (0..count).map(|_| rng.unit()).collect();
    (0..n)
        .map(|s| {
            let f = f64::from(s) / step;
            let i0 = usize::try_from(trunc_i64(f)).unwrap_or(0);
            let t = f - i0 as f64;
            let t = t * t * (3.0 - 2.0 * t);
            let a = pts[i0 % pts.len()];
            let b =
                if periodic { pts[(i0 + 1) % pts.len()] } else { pts[(i0 + 1).min(pts.len() - 1)] };
            let v = a + (b - a) * t;
            let span = f64::from(hi - lo + 1);
            let step_up = u32::try_from(trunc_i64(v * span).max(0)).unwrap_or(0);
            lo + (hi - lo).min(step_up)
        })
        .collect()
}

/// Every tile scored for how land-like it is, shaped by the map type, and the share of the map
/// that should be land (`_land_scores`).
#[allow(clippy::too_many_lines, reason = "one arm per map type, as Python wrote it")]
pub(crate) fn land_scores(
    rng: &mut Rng,
    grid: &HexGrid,
    map_type: MapType,
    players: usize,
    ice: &[bool],
) -> (Vec<f64>, f64) {
    let (w, h) = (f64::from(grid.width()), f64::from(grid.height()));
    let (cw, ch) = (w / 2.0, h * ROW / 2.0);
    let ph = h * ROW;
    let (wrap_x, wrap_y) = (grid.wrap_x(), grid.wrap_y());
    // Separations the short way round on a wrapping axis.
    let dx = |a: f64, b: f64| {
        let d = a - b;
        if wrap_x { (d + w / 2.0).rem_euclid(w) - w / 2.0 } else { d }
    };
    let dy = |a: f64, b: f64| {
        let d = a - b;
        if wrap_y { (d + ph / 2.0).rem_euclid(ph) - ph / 2.0 } else { d }
    };
    let noise = fractal_field(rng, grid, w.max(h) / 5.0, 5, 0.55);
    let mut scores = vec![0.0; noise.len()];
    let uniform = |rng: &mut Rng, a: f64, b: f64| a + (b - a) * rng.unit();
    let fraction = match map_type {
        MapType::Pangaea => {
            for t in grid.tiles() {
                let (px, py) = pos(grid, t);
                let d = hypot(dx(px, cw) / (w * 0.42), dy(py, ch) / (h * ROW * 0.40));
                scores[t.0 as usize] = noise[t.0 as usize] * 0.9 - d * 0.9;
            }
            0.42
        }
        MapType::Continents => {
            let half = ceil_i64(players as f64 / 2.0);
            let n = usize::try_from(half.clamp(2, 4)).unwrap_or(2);
            // Continent centres spread east to west, with some jitter north to south.
            let mut centres = Vec::with_capacity(n);
            for k in 0..n {
                let kf = k as f64;
                if n == 2 {
                    let cx = w * (0.27 + 0.46 * kf);
                    let cy = h * ROW * uniform(rng, 0.42, 0.58);
                    centres.push((cx, cy));
                } else {
                    let cols = ceil_i64(n as f64 / 2.0) as f64;
                    let (row, col) = ((k % 2) as f64, (k / 2) as f64);
                    let cx = w * (col + 0.5) / cols + uniform(rng, -w * 0.04, w * 0.04);
                    let cy = h * ROW * (0.3 + 0.4 * row);
                    centres.push((cx, cy));
                }
            }
            let mut ds = Vec::with_capacity(n);
            for t in grid.tiles() {
                let (px, py) = pos(grid, t);
                ds.clear();
                ds.extend(
                    centres.iter().map(|&(cx, cy)| hypot(dx(px, cx) / w, dy(py, cy) / (h * ROW))),
                );
                ds.sort_by(|a, b| a.total_cmp(b));
                let gap = if ds.len() > 1 { ds[1] - ds[0] } else { 1.0 };
                let gap_penalty = (0.07 - gap).max(0.0) * 9.0;
                scores[t.0 as usize] = noise[t.0 as usize] * 0.85 - ds[0] * 1.6 - gap_penalty;
            }
            0.40
        }
        MapType::Archipelago => {
            let noise2 = fractal_field(rng, grid, w.max(h) / 12.0, 4, 0.5);
            let n = (players * 5).max(8);
            let centres: Vec<(f64, f64)> = (0..n)
                .map(|_| {
                    let cx = uniform(rng, w * 0.08, w * 0.92);
                    let cy = uniform(rng, h * ROW * 0.1, h * ROW * 0.9);
                    (cx, cy)
                })
                .collect();
            for t in grid.tiles() {
                let (px, py) = pos(grid, t);
                let near = centres
                    .iter()
                    .map(|&(cx, cy)| hypot(dx(px, cx), dy(py, cy)))
                    .fold(f64::INFINITY, f64::min);
                let d = near / w.max(h);
                let i = t.0 as usize;
                scores[i] = noise2[i] * 0.8 + noise[i] * 0.2 - d * 3.2;
            }
            0.30
        }
        MapType::InlandSea => {
            for t in grid.tiles() {
                let (px, py) = pos(grid, t);
                let d = hypot(dx(px, cw) / (w * 0.5), dy(py, ch) / (h * ROW * 0.5));
                let inner = (0.45 - d).max(0.0) * 2.2;
                scores[t.0 as usize] = noise[t.0 as usize] * 0.7 - inner;
            }
            0.58
        }
        MapType::Fractal => {
            scores.copy_from_slice(&noise);
            0.40
        }
    };

    // Push land away from the edges that do not wrap, and from the ice. The ice itself is always
    // sea; the three rows inside it are discouraged as a bare map edge is, so land meets the ice
    // at a coastline rather than being cut off by it.
    let near_ice = distance_from(grid, ice, 3);
    let (gw, gh) = (i32::from(grid.width()), i32::from(grid.height()));
    for t in grid.tiles() {
        let i = t.0 as usize;
        if ice[i] {
            scores[i] = SEA;
            continue;
        }
        let (x, y) = grid.xy(t);
        let mut edge = 99;
        if !wrap_x {
            edge = edge.min(x).min(gw - 1 - x);
        }
        if !wrap_y {
            edge = edge.min(y).min(gh - 1 - y);
        }
        if near_ice[i] != u32::MAX {
            edge = edge.min(i32::try_from(near_ice[i]).unwrap_or(99) - 1);
        }
        if edge < 3 {
            scores[i] -= f64::from(3 - edge) * 0.35;
        }
    }
    (scores, fraction)
}

/// The score at or above which `fraction` of the map is land (`_threshold`).
pub(crate) fn threshold(scores: &[f64], fraction: f64) -> f64 {
    let mut s: Vec<TotalF64> = scores.iter().copied().map(TotalF64).collect();
    s.sort_by(|a, b| b.cmp(a));
    let n = s.len();
    let k = usize::try_from(trunc_i64(n as f64 * fraction)).unwrap_or(0);
    let k = k.min(n.saturating_sub(1)).max(1).min(n.saturating_sub(1));
    s.get(k).map_or(0.0, |x| x.0)
}

/// Which tiles are land: those at or above the threshold, less any with no land beside them, so
/// no island is a single tile (`generate_map`, `mapgen.py:1682-1687`).
pub(crate) fn land_mask(grid: &HexGrid, scores: &[f64], fraction: f64) -> Vec<bool> {
    let thr = threshold(scores, fraction);
    let mut land: Vec<bool> = scores.iter().map(|&s| s >= thr).collect();
    for t in grid.tiles() {
        let i = t.0 as usize;
        if land[i] && !grid.neighbors(t).any(|n| land[n.0 as usize]) {
            land[i] = false;
        }
    }
    land
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::rng::Purpose;
    use crate::state::config::MapEdges;

    #[test]
    fn the_ice_hugs_the_capped_sides() {
        let grid = HexGrid::new(40, 30, false, false).expect("grid");
        let mut rng = Rng::keyed(3, Purpose::MapIce, &[0]);
        let band = ice_band(&mut rng, &grid, IceSides::of(MapEdges::Boxed));
        let deepest = max_depth(30);
        for t in grid.tiles() {
            let (x, y) = grid.xy(t);
            let from_edge = x.min(y).min(39 - x).min(29 - y);
            if band[t.0 as usize] {
                assert!(from_edge < i32::try_from(deepest.max(max_depth(40))).unwrap_or(4));
            }
            if from_edge == 0 {
                assert!(band[t.0 as usize], "every edge tile of a boxed map is ice");
            }
        }
        let none = ice_band(&mut rng, &grid, IceSides::of(MapEdges::WrapBoth));
        assert!(none.iter().all(|&b| !b));
    }

    #[test]
    fn the_threshold_makes_the_share_of_land_asked_for() {
        let scores: Vec<f64> = (0..100).map(f64::from).collect();
        assert!((threshold(&scores, 0.4) - 59.0).abs() < 1e-12);
        assert!((threshold(&scores, 0.0) - 98.0).abs() < 1e-12, "at least one tile");
    }

    #[test]
    fn every_map_type_makes_some_land_and_some_sea() {
        let grid = HexGrid::new(44, 28, true, false).expect("grid");
        let ice = vec![false; grid.size() as usize];
        for t in super::super::options::MAP_TYPES {
            let mut rng = Rng::keyed(11, Purpose::MapLand, &[0]);
            let (scores, frac) = land_scores(&mut rng, &grid, t, 4, &ice);
            let land = land_mask(&grid, &scores, frac);
            let n = land.iter().filter(|&&l| l).count();
            assert!(n > 100 && n < 1000, "{t:?}: {n} land tiles");
        }
    }
}
