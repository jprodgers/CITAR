//! Smooth noise: value noise on a coarse lattice, and octaves of it summed (`mapgen.py:108-192,
//! 527-540`: `ValueNoise`, `fractal_field`, `_value_noise`, `_normalize`, `_pos`, `_noise`).
//!
//! Value noise is cheaper than Perlin and good enough for deciding where land is. On an axis the
//! map wraps round, the lattice repeats with the map, so the two sides of the seam are the same
//! noise and no coastline is cut off at the edge.

use crate::base::hex::HexGrid;
use crate::base::ids::TileIdx;
use crate::base::num::{floor_i64, round_half_even};
use crate::base::rng::Rng;

/// The rows of a hex map are `sqrt(3) / 2` apart, as Python wrote it.
pub(crate) const ROW: f64 = 0.866;

/// Value noise on a lattice of cells, interpolated smoothly (`ValueNoise`).
#[derive(Clone, Debug)]
pub(crate) struct ValueNoise {
    /// Lattice points across and down.
    gw: usize,
    gh: usize,
    /// The period in cells on a wrapping axis, 0 on one that does not wrap.
    per_x: usize,
    per_y: usize,
    /// The cell size along each axis.
    cx: f64,
    cy: f64,
    vals: Vec<f64>,
}

/// `max(1, round(period / cell))`: a whole number of cells fits a wrapping axis.
fn cells(period: Option<f64>, cell: f64) -> usize {
    period.map_or(0, |p| usize::try_from(floor_i64(round_half_even(p / cell)).max(1)).unwrap_or(1))
}

impl ValueNoise {
    /// Noise over `width` by `height`, with lattice cells of `cell`, repeating along the axes
    /// that have a period.
    pub(crate) fn new(
        rng: &mut Rng,
        width: f64,
        height: f64,
        cell: f64,
        period_x: Option<f64>,
        period_y: Option<f64>,
    ) -> Self {
        let per_x = cells(period_x, cell);
        let per_y = cells(period_y, cell);
        // The cell stretches slightly so the period is a whole number of cells.
        let cx = match period_x {
            Some(p) if per_x > 0 => p / per_x as f64,
            _ => cell,
        };
        let cy = match period_y {
            Some(p) if per_y > 0 => p / per_y as f64,
            _ => cell,
        };
        let span = |side: f64| usize::try_from(floor_i64(side / cell)).unwrap_or(0) + 3;
        let gw = if per_x > 0 { per_x } else { span(width) };
        let gh = if per_y > 0 { per_y } else { span(height) };
        let vals = (0..gw * gh).map(|_| rng.unit()).collect();
        Self { gw, gh, per_x, per_y, cx, cy, vals }
    }

    /// The noise at a point, interpolated from the lattice round it (`ValueNoise.at`).
    pub(crate) fn at(&self, fx: f64, fy: f64) -> f64 {
        let (gx, gy) = (fx / self.cx, fy / self.cy);
        let (fx0, fy0) = (gx.floor(), gy.floor());
        let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
        let (tx, ty) = (smooth(gx - fx0), smooth(gy - fy0));
        let lattice = |f: f64, per: usize, len: usize| -> (usize, usize) {
            let i = floor_i64(f);
            if per > 0 {
                let p = i64::try_from(per).unwrap_or(i64::MAX);
                (
                    usize::try_from(i.rem_euclid(p)).unwrap_or(0),
                    usize::try_from((i + 1).rem_euclid(p)).unwrap_or(0),
                )
            } else {
                // Points lie on the map, which the lattice covers with room to spare.
                let last = len.saturating_sub(1);
                let at = usize::try_from(i.max(0)).unwrap_or(0).min(last);
                (at, (at + 1).min(last))
            }
        };
        let (x0, x1) = lattice(fx0, self.per_x, self.gw);
        let (y0, y1) = lattice(fy0, self.per_y, self.gh);
        let g = self.gw;
        let v = |x: usize, y: usize| self.vals[y * g + x];
        let a = v(x0, y0) + (v(x1, y0) - v(x0, y0)) * tx;
        let b = v(x0, y1) + (v(x1, y1) - v(x0, y1)) * tx;
        a + (b - a) * ty
    }
}

/// A tile's position in continuous space: odd rows shifted half a tile, rows `ROW` apart
/// (`_pos`).
pub(crate) fn pos(grid: &HexGrid, t: TileIdx) -> (f64, f64) {
    let (x, y) = grid.xy(t);
    (f64::from(x) + 0.5 * f64::from(y & 1), f64::from(y) * ROW)
}

/// Value noise for this grid, repeating along the axes the map wraps (`_value_noise`).
fn value_noise(rng: &mut Rng, grid: &HexGrid, cell: f64) -> ValueNoise {
    let (w, h) = (f64::from(grid.width()), f64::from(grid.height()));
    ValueNoise::new(
        rng,
        w + 2.0,
        h + 2.0,
        cell,
        grid.wrap_x().then_some(w),
        grid.wrap_y().then_some(h * ROW),
    )
}

/// Octaves of noise, each half the cell and `persistence` times the amplitude of the one before,
/// summed per tile and divided by the total amplitude.
fn octaves(
    rng: &mut Rng,
    grid: &HexGrid,
    base_cell: f64,
    count: u32,
    persistence: f64,
) -> Vec<f64> {
    let mut layers = Vec::new();
    let (mut cell, mut amp) = (base_cell, 1.0);
    for _ in 0..count {
        layers.push((value_noise(rng, grid, cell.max(1.0)), amp));
        cell /= 2.0;
        amp *= persistence;
    }
    let total: f64 = layers.iter().map(|(_, a)| a).sum();
    grid.tiles()
        .map(|t| {
            let (px, py) = pos(grid, t);
            layers.iter().map(|(n, a)| n.at(px, py) * a).sum::<f64>() / total
        })
        .collect()
}

/// Several octaves of noise, rescaled to 0-1 (`fractal_field`): continents and coastline detail
/// at once.
pub(crate) fn fractal_field(
    rng: &mut Rng,
    grid: &HexGrid,
    base_cell: f64,
    count: u32,
    persistence: f64,
) -> Vec<f64> {
    normalize(octaves(rng, grid, base_cell, count, persistence))
}

/// Smooth noise in about -1..1 with features `scale` tiles across (`_noise`, the stand-in for
/// UnCiv's Perlin noise).
pub(crate) fn noise(rng: &mut Rng, grid: &HexGrid, scale: f64, count: u32) -> Vec<f64> {
    octaves(rng, grid, scale.max(1.0), count, 0.5).into_iter().map(|v| 2.0 * v - 1.0).collect()
}

/// Values rescaled to 0-1, so thresholds mean the same on any map (`_normalize`).
pub(crate) fn normalize(mut vals: Vec<f64>) -> Vec<f64> {
    let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = if hi - lo == 0.0 { 1.0 } else { hi - lo };
    for v in &mut vals {
        *v = (*v - lo) / span;
    }
    vals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::rng::Purpose;

    fn rng() -> Rng {
        Rng::keyed(7, Purpose::MapLand, &[0])
    }

    #[test]
    fn noise_is_smooth_and_hits_the_lattice() {
        let n = ValueNoise::new(&mut rng(), 20.0, 20.0, 4.0, None, None);
        // At a lattice point the noise is the point's value.
        assert!((n.at(8.0, 4.0) - n.vals[n.gw + 2]).abs() < 1e-12);
        // Between neighbouring points it stays within their range.
        let v = n.at(9.0, 4.0);
        let (a, b) = (n.vals[n.gw + 2], n.vals[n.gw + 3]);
        assert!(v >= a.min(b) - 1e-12 && v <= a.max(b) + 1e-12);
    }

    #[test]
    fn a_wrapping_axis_repeats() {
        let n = ValueNoise::new(&mut rng(), 22.0, 22.0, 3.0, Some(20.0), None);
        for k in 0..20 {
            let x = f64::from(k) * 0.7;
            assert!((n.at(x, 5.0) - n.at(x + 20.0, 5.0)).abs() < 1e-12, "at {x}");
        }
    }

    #[test]
    fn a_fractal_field_spans_zero_to_one() {
        let grid = HexGrid::new(30, 20, true, false).expect("grid");
        let f = fractal_field(&mut rng(), &grid, 6.0, 4, 0.5);
        let lo = f.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = f.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(lo.abs() < 1e-12 && (hi - 1.0).abs() < 1e-12);
        let n = noise(&mut rng(), &grid, 6.0, 1);
        assert!(n.iter().all(|v| (-1.0..=1.0).contains(v)));
    }
}
