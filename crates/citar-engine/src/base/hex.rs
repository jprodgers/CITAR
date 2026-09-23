//! The hex grid: neighbours, distance, areas, rings and lines on a map that may wrap
//! (DESIGN.md 4.3).
//!
//! A port of `citar/engine/hexmap.py:1-214`. Tiles are addressed by "odd-r" offset coordinates
//! (x the column, y the row, odd rows shifted right by half a hex) and stored as a flat index
//! `y * width + x`. Cube coordinates do the arithmetic. A map may wrap east-west, north-south or
//! both; every method here accounts for it, so nothing that goes through [`HexGrid`] needs to.
//! North-south wrapping needs an even height, because odd rows are shifted: an odd height turns
//! it off, as in Python. Sides are 8 to 256 tiles, the bounds `maps.py:31` and `130-133` put on
//! every map, so a corrupt save or map cannot ask for an enormous grid, and no wrapped step lands
//! back on its own tile.
//!
//! Differences from Python:
//! - **`within` order.** Python sorted `within(idx, r)` by distance and kept its dq/dr
//!   enumeration order among ties, then cached up to 200k results (`hexmap.py:138-157`). Here the
//!   tiles come ring by ring, with no cache: each ring starts at the east neighbour's direction
//!   and goes counter-clockwise, following the direction table. The order is still sorted by
//!   distance; within one distance it differs. Rules that pick among ties use an explicit key
//!   (DESIGN.md 7.4), and where a Python rule took "the first tile in `within` order" the new
//!   order is a listed intended difference.
//! - **Tiles off the map.** Python indexed its lists with whatever it was given. Here every method
//!   takes any `TileIdx` without panicking: a tile off the map has no neighbours, is `u32::MAX`
//!   steps from everything, and has no line to anywhere.
//! - **Sizes.** Python built a grid of any size, and only the map loader checked the bounds; a
//!   game config could ask for a 3x3 map. Here the grid refuses sides outside 8 to 256.

use super::ids::TileIdx;
use super::num::round_half_even;
use super::sets::BitSet;

/// One of the six directions, in Python's order (`hexmap.py:14-16`): counter-clockwise from
/// east. River edges and the neighbour table use this order.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dir {
    E = 0,
    NE = 1,
    NW = 2,
    W = 3,
    SW = 4,
    SE = 5,
}

impl Dir {
    /// Every direction, in order.
    pub const ALL: [Dir; 6] = [Self::E, Self::NE, Self::NW, Self::W, Self::SW, Self::SE];

    /// The direction's position in the order.
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The direction at this position.
    #[must_use]
    pub const fn from_index(i: usize) -> Option<Self> {
        if i < 6 { Some(Self::ALL[i]) } else { None }
    }

    /// Python's name for it (`DIR_NAMES`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::E => "E",
            Self::NE => "NE",
            Self::NW => "NW",
            Self::W => "W",
            Self::SW => "SW",
            Self::SE => "SE",
        }
    }

    /// The direction pointing back.
    #[must_use]
    pub const fn opposite(self) -> Self {
        Self::ALL[(self as usize + 3) % 6]
    }
}

/// Offset steps on even rows, in direction order (`DIRS_EVEN`).
const DIRS_EVEN: [(i32, i32); 6] = [(1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1)];
/// Offset steps on odd rows, in direction order (`DIRS_ODD`).
const DIRS_ODD: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)];
/// The same steps in cube coordinates `(dq, dr)`, the same on every row.
const CUBE_DIRS: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)];

/// Marks a missing neighbour in [`HexGrid::neighbor_table`].
pub const NO_TILE: u32 = u32::MAX;

/// Cube coordinates: `q` and `r`, with `s = -q - r` implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cube {
    pub q: i32,
    pub r: i32,
}

impl Cube {
    /// The third coordinate.
    #[must_use]
    pub const fn s(self) -> i32 {
        -self.q - self.r
    }

    /// From odd-r offset coordinates (`offset_to_cube`).
    #[must_use]
    pub const fn from_offset(x: i32, y: i32) -> Self {
        // y - (y & 1) is even, so the division is exact and matches Python's `//` even for
        // negative y.
        Self { q: x - (y - (y & 1)) / 2, r: y }
    }

    /// To odd-r offset coordinates (`cube_to_offset`).
    #[must_use]
    pub const fn to_offset(self) -> (i32, i32) {
        (self.q + (self.r - (self.r & 1)) / 2, self.r)
    }
}

/// Twice the cube length of `(dq, dr)`.
#[inline]
const fn double_length(dq: i32, dr: i32) -> u32 {
    dq.unsigned_abs() + dr.unsigned_abs() + (dq + dr).unsigned_abs()
}

/// The shortest side a map may have (`maps.py:132`).
pub const MIN_SIDE: u16 = 8;

/// The longest side a map may have (`maps.py:31`, `MAX_SIDE`).
pub const MAX_SIDE: u16 = 256;

/// Why a grid cannot be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HexError {
    /// A side outside `MIN_SIDE..=MAX_SIDE`.
    #[error("a map needs sides of {MIN_SIDE} to {MAX_SIDE} tiles, not {width}x{height}")]
    Size { width: u16, height: u16 },
}

/// A hex map's geometry: size, wrapping, neighbours, distance, areas, rings and lines.
///
/// Pure geometry with no game state. It is derived from `MapInfo` and never saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexGrid {
    width: u16,
    height: u16,
    wrap_x: bool,
    wrap_y: bool,
    size: u32,
    /// The cube translations that land on the same tile again, in Python's order
    /// (`hexmap.py:54-58`); only the first `n_shifts` are used.
    shifts: [(i32, i32); 9],
    n_shifts: usize,
    /// The shortest of those translations (cube length): below half of it, nothing wraps round
    /// onto itself. `u32::MAX` on a map that does not wrap.
    min_period: u32,
    /// The neighbours in direction order, `NO_TILE` where there is none.
    neighbors: Vec<[u32; 6]>,
}

impl HexGrid {
    /// The grid for a `width` by `height` map, each side from [`MIN_SIDE`] to [`MAX_SIDE`].
    /// `wrap_y` is ignored on an odd height.
    pub fn new(width: u16, height: u16, wrap_x: bool, wrap_y: bool) -> Result<Self, HexError> {
        let sides = MIN_SIDE..=MAX_SIDE;
        if !sides.contains(&width) || !sides.contains(&height) {
            return Err(HexError::Size { width, height });
        }
        let wrap_y = wrap_y && height.is_multiple_of(2);
        let (w, h) = (i32::from(width), i32::from(height));
        let xs: &[i32] = if wrap_x { &[0, -w, w] } else { &[0] };
        let ys: &[i32] = if wrap_y { &[0, -1, 1] } else { &[0] };
        let mut shifts = [(0, 0); 9];
        let mut n_shifts = 0;
        for &sy in ys {
            for &sx in xs {
                shifts[n_shifts] = (sx - sy * (h / 2), sy * h);
                n_shifts += 1;
            }
        }
        // Every nonzero translation is m*(W, 0) + n*(-H/2, H) in cube terms, whose length is at
        // least H when n != 0 and at least W when n == 0.
        let min_period = match (wrap_x, wrap_y) {
            (false, false) => u32::MAX,
            (true, false) => u32::from(width),
            (false, true) => u32::from(height),
            (true, true) => u32::from(width.min(height)),
        };
        let size = u32::from(width) * u32::from(height);
        let mut grid = Self {
            width,
            height,
            wrap_x,
            wrap_y,
            size,
            shifts,
            n_shifts,
            min_period,
            neighbors: Vec::new(),
        };
        grid.neighbors = (0..size).map(|i| grid.compute_neighbors(i)).collect();
        Ok(grid)
    }

    fn compute_neighbors(&self, i: u32) -> [u32; 6] {
        let (x, y) = self.xy(TileIdx(i));
        let dirs = if y & 1 == 1 { &DIRS_ODD } else { &DIRS_EVEN };
        let mut out = [NO_TILE; 6];
        for (d, (dx, dy)) in dirs.iter().enumerate() {
            // With sides of at least 8 a step never wraps onto the tile or an earlier neighbour;
            // the checks keep the table a set of distinct neighbours regardless.
            if let Some(n) = self.wrap(x + dx, y + dy)
                && n.0 != i
                && !out.contains(&n.0)
            {
                out[d] = n.0;
            }
        }
        out
    }

    /// The width in tiles.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// The height in tiles.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Whether the map wraps east-west.
    #[must_use]
    pub const fn wrap_x(&self) -> bool {
        self.wrap_x
    }

    /// Whether the map wraps north-south (never on an odd height).
    #[must_use]
    pub const fn wrap_y(&self) -> bool {
        self.wrap_y
    }

    /// Whether the map wraps in either direction.
    #[must_use]
    pub const fn wraps(&self) -> bool {
        self.wrap_x || self.wrap_y
    }

    /// The number of tiles.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Every tile, in index order.
    pub fn tiles(&self) -> impl DoubleEndedIterator<Item = TileIdx> + use<> {
        (0..self.size).map(TileIdx)
    }

    /// Whether `idx` is a tile of this map.
    #[must_use]
    #[inline]
    pub const fn contains(&self, idx: TileIdx) -> bool {
        idx.0 < self.size
    }

    /// Whether the coordinates are on the map, without wrapping.
    #[must_use]
    #[inline]
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        (0..i32::from(self.width)).contains(&x) && (0..i32::from(self.height)).contains(&y)
    }

    /// The tile at these coordinates, if they are on the map (without wrapping).
    #[must_use]
    #[inline]
    pub fn idx(&self, x: i32, y: i32) -> Option<TileIdx> {
        if self.in_bounds(x, y) {
            // In bounds, so both are non-negative and the index fits.
            Some(TileIdx(y.unsigned_abs() * u32::from(self.width) + x.unsigned_abs()))
        } else {
            None
        }
    }

    /// The tile at coordinates that may lie past a wrapping edge, or `None` off the map.
    #[must_use]
    #[inline]
    pub fn wrap(&self, x: i32, y: i32) -> Option<TileIdx> {
        let x = if self.wrap_x { x.rem_euclid(i32::from(self.width)) } else { x };
        let y = if self.wrap_y { y.rem_euclid(i32::from(self.height)) } else { y };
        self.idx(x, y)
    }

    /// The coordinates of a tile. For an index off the map, the row is past the last one.
    #[must_use]
    #[inline]
    pub const fn xy(&self, idx: TileIdx) -> (i32, i32) {
        let w = self.width as u32;
        // x is below the width; y is below 2^32 / 8 = 2^29, since the width is at least 8, so
        // both fit an i32 even for an index off the map.
        ((idx.0 % w) as i32, (idx.0 / w) as i32)
    }

    /// The cube coordinates of a tile.
    #[must_use]
    #[inline]
    pub const fn cube(&self, idx: TileIdx) -> Cube {
        let (x, y) = self.xy(idx);
        Cube::from_offset(x, y)
    }

    /// The neighbours in direction order, [`NO_TILE`] where there is none (off the map edge).
    /// A tile outside the map has none.
    #[must_use]
    #[inline]
    pub fn neighbor_table(&self, idx: TileIdx) -> [u32; 6] {
        self.neighbors.get(idx.0 as usize).copied().unwrap_or([NO_TILE; 6])
    }

    /// The neighbours, in direction order (Python's `neighbors`).
    #[inline]
    pub fn neighbors(&self, idx: TileIdx) -> impl Iterator<Item = TileIdx> + use<> {
        self.neighbor_table(idx).into_iter().filter(|&n| n != NO_TILE).map(TileIdx)
    }

    /// The neighbour in direction `d`, or `None` at the map edge.
    #[must_use]
    #[inline]
    pub fn neighbor(&self, idx: TileIdx, d: Dir) -> Option<TileIdx> {
        let n = self.neighbor_table(idx)[d.index()];
        (n != NO_TILE).then_some(TileIdx(n))
    }

    fn shifts(&self) -> &[(i32, i32)] {
        &self.shifts[..self.n_shifts]
    }

    /// The cube translation that brings `b`'s copy closest to `a`: the first of equals in the
    /// shift order, as `_nearest_shift` chose it.
    fn nearest_shift(&self, a: TileIdx, b: TileIdx) -> (i32, i32) {
        let (ca, cb) = (self.cube(a), self.cube(b));
        let mut best = (0, 0);
        let mut best_d = u32::MAX;
        for &(sq, sr) in self.shifts() {
            let d = double_length(cb.q + sq - ca.q, cb.r + sr - ca.r);
            if d < best_d {
                best = (sq, sr);
                best_d = d;
            }
        }
        best
    }

    /// The number of steps between two tiles, the short way round on a wrapping map; `u32::MAX`
    /// if either is off the map.
    #[must_use]
    pub fn distance(&self, a: TileIdx, b: TileIdx) -> u32 {
        if !self.contains(a) || !self.contains(b) {
            return u32::MAX;
        }
        let (ca, cb) = (self.cube(a), self.cube(b));
        let (dq, dr) = (cb.q - ca.q, cb.r - ca.r);
        let mut best = u32::MAX;
        for &(sq, sr) in self.shifts() {
            best = best.min(double_length(dq + sq, dr + sr));
        }
        best / 2
    }

    /// No two tiles are further apart than this, so no radius beyond it adds a tile.
    fn reach(&self) -> u32 {
        u32::from(self.width) + u32::from(self.height)
    }

    /// Calls `f` with each on-map tile of the ring at `radius` (at least 1) round `c`, in ring
    /// order: from the east corner, counter-clockwise. On a wrapping map a tile can come more
    /// than once, or be nearer than `radius` the other way round.
    fn walk_ring(&self, c: Cube, radius: u32, mut f: impl FnMut(TileIdx)) {
        // radius <= reach <= 512, so it fits an i32.
        let k = radius as i32;
        let mut q = c.q + k;
        let mut r = c.r;
        for side in 0..6 {
            let (dq, dr) = CUBE_DIRS[(side + 2) % 6];
            for _ in 0..k {
                let (x, y) = Cube { q, r }.to_offset();
                if let Some(t) = self.wrap(x, y) {
                    f(t);
                }
                q += dq;
                r += dr;
            }
        }
    }

    /// Whether rings up to `radius` can meet themselves round a wrapping edge.
    fn wraps_within(&self, radius: u32) -> bool {
        self.wraps() && radius.saturating_mul(2) >= self.min_period
    }

    /// Every tile within `radius` of `center`, including it, nearest first (Python's `within`).
    ///
    /// The tiles come ring by ring; see the module doc for the order within a ring.
    #[must_use]
    pub fn within(&self, center: TileIdx, radius: u32) -> Vec<TileIdx> {
        let mut out = Vec::new();
        self.within_into(center, radius, &mut out);
        out
    }

    /// [`within`](Self::within) into a vector the caller reuses. It is cleared first.
    pub fn within_into(&self, center: TileIdx, radius: u32, out: &mut Vec<TileIdx>) {
        out.clear();
        if !self.contains(center) {
            return;
        }
        out.push(center);
        let radius = radius.min(self.reach());
        let c = self.cube(center);
        if !self.wraps_within(radius) {
            // No ring can reach round onto itself, so every tile comes once, at its distance.
            for k in 1..=radius {
                self.walk_ring(c, k, |t| out.push(t));
            }
            return;
        }
        let mut seen = BitSet::with_capacity(self.size);
        seen.insert(center.0);
        for k in 1..=radius {
            self.walk_ring(c, k, |t| {
                if self.distance(center, t) == k && seen.insert(t.0) {
                    out.push(t);
                }
            });
        }
    }

    /// Whether some tile within `radius` of `center`, including it, passes `f`: [`within`]
    /// without the vector, stopping at the first tile that passes. The tiles come nearest ring
    /// first; on a wrapping map a tile may be asked about twice, which cannot change the answer.
    ///
    /// [`within`]: Self::within
    pub fn any_within(
        &self,
        center: TileIdx,
        radius: u32,
        mut f: impl FnMut(TileIdx) -> bool,
    ) -> bool {
        if !self.contains(center) {
            return false;
        }
        if f(center) {
            return true;
        }
        let c = self.cube(center);
        let mut found = false;
        for k in 1..=radius.min(self.reach()) {
            // walk_ring visits a ring whole; the flag skips the tests after the first hit.
            self.walk_ring(c, k, |t| found = found || f(t));
            if found {
                return true;
            }
        }
        false
    }

    /// Every tile at exactly `radius` from `center` (Python's `ring`), in ring order.
    #[must_use]
    pub fn ring(&self, center: TileIdx, radius: u32) -> Vec<TileIdx> {
        let mut out = Vec::new();
        self.ring_into(center, radius, &mut out);
        out
    }

    /// [`ring`](Self::ring) into a vector the caller reuses. It is cleared first.
    pub fn ring_into(&self, center: TileIdx, radius: u32, out: &mut Vec<TileIdx>) {
        out.clear();
        if !self.contains(center) || radius > self.reach() {
            return;
        }
        if radius == 0 {
            out.push(center);
            return;
        }
        let c = self.cube(center);
        if !self.wraps_within(radius) {
            self.walk_ring(c, radius, |t| out.push(t));
            return;
        }
        let mut seen = BitSet::with_capacity(self.size);
        self.walk_ring(c, radius, |t| {
            if self.distance(center, t) == radius && seen.insert(t.0) {
                out.push(t);
            }
        });
    }

    /// The tiles on the straight line from `a` to `b`, both included, the short way round
    /// (Python's `line`). Empty if either is off the map.
    ///
    /// The same float steps as Python, with the same nudges off the exact midpoints, and
    /// Python's round-half-even, so the tiles are the same.
    #[must_use]
    pub fn line(&self, a: TileIdx, b: TileIdx) -> Vec<TileIdx> {
        if !self.contains(a) || !self.contains(b) {
            return Vec::new();
        }
        let n = self.distance(a, b);
        if n == 0 {
            return vec![a];
        }
        let ca = self.cube(a);
        let (sq, sr) = self.nearest_shift(a, b);
        let cb = self.cube(b);
        let cb = Cube { q: cb.q + sq, r: cb.r + sr };
        let (aq, ar, as_) = (f64::from(ca.q), f64::from(ca.r), f64::from(ca.s()));
        let (dq, dr, ds) =
            (f64::from(cb.q - ca.q), f64::from(cb.r - ca.r), f64::from(cb.s() - ca.s()));
        let mut out: Vec<TileIdx> = Vec::with_capacity(n as usize + 1);
        for i in 0..=n {
            let t = f64::from(i) / f64::from(n);
            let fq = aq + dq * t + 1e-6;
            let fr = ar + dr * t + 1e-6;
            let fs = as_ + ds * t - 2e-6;
            let (mut rq, mut rr, rs) =
                (round_half_even(fq), round_half_even(fr), round_half_even(fs));
            let (eq, er, es) = ((rq - fq).abs(), (rr - fr).abs(), (rs - fs).abs());
            if eq > er && eq > es {
                rq = -rr - rs;
            } else if er > es {
                rr = -rq - rs;
            }
            // Whole numbers of map scale, so the conversions are exact.
            let (x, y) = Cube { q: rq as i32, r: rr as i32 }.to_offset();
            if let Some(idx) = self.wrap(x, y)
                && out.last() != Some(&idx)
            {
                out.push(idx);
            }
        }
        out
    }

    /// `b`'s offset coordinates as seen from `a`: the copy of `b` nearest to `a`, which may lie
    /// off the map (Python's `unwrapped_xy`). On a map that does not wrap, or when either tile is
    /// off the map, just `b`'s.
    #[must_use]
    pub fn unwrapped_xy(&self, a: TileIdx, b: TileIdx) -> (i32, i32) {
        let (bx, by) = self.xy(b);
        if !self.contains(a) || !self.contains(b) {
            return (bx, by);
        }
        let (sq, sr) = self.nearest_shift(a, b);
        // A cube shift (sq, sr) is (sq + sr/2, sr) in offset terms; sr is a multiple of an even
        // height, so the halving is exact.
        (bx + sq + sr / 2, by + sr)
    }

    /// The rough compass direction from `a` to `b`, for text: `"N"`, `"SE"`, ... or `"here"`
    /// (Python's `direction_name`).
    #[must_use]
    pub fn direction_name(&self, a: TileIdx, b: TileIdx) -> &'static str {
        let (ax, ay) = self.xy(a);
        let (bx, by) = self.unwrapped_xy(a, b);
        let (ax, ay, bx, by) = (i64::from(ax), i64::from(ay), i64::from(bx), i64::from(by));
        // Python measures dx in half-hexes, with odd rows shifted by 0.5; doubling keeps it
        // in integers. In i64, so a row off the map (up to 2^29) cannot overflow.
        let dx2 = (2 * bx + (by & 1)) - (2 * ax + (ay & 1));
        let dy = by - ay;
        if dx2 == 0 && dy == 0 {
            return "here";
        }
        let mut ns = dy.signum();
        let mut ew = dx2.signum();
        // abs(dy) > 2 * abs(dx), and abs(dx) > 2 * abs(dy), with dx = dx2 / 2.
        if dy.abs() > dx2.abs() {
            ew = 0;
        }
        if dx2.abs() > 4 * dy.abs() {
            ns = 0;
        }
        match (ns, ew) {
            (-1, 1) => "NE",
            (-1, -1) => "NW",
            (-1, _) => "N",
            (1, 1) => "SE",
            (1, -1) => "SW",
            (1, _) => "S",
            (_, 1) => "E",
            (_, -1) => "W",
            // Unreachable: a nonzero offset keeps at least one of the two.
            _ => "here",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: u16, h: u16, wx: bool, wy: bool) -> HexGrid {
        HexGrid::new(w, h, wx, wy).expect("a valid size")
    }

    #[test]
    fn neighbours_follow_the_direction_table() {
        let g = grid(10, 10, false, false);
        let t = g.idx(4, 4).expect("on the map");
        let names: Vec<(i32, i32)> = g.neighbors(t).map(|n| g.xy(n)).collect();
        assert_eq!(names, [(5, 4), (4, 3), (3, 3), (3, 4), (3, 5), (4, 5)]);
        let corner = TileIdx(0);
        assert_eq!(g.neighbors(corner).count(), 2);
        assert_eq!(g.neighbor(corner, Dir::W), None);
        let wrapped = grid(10, 10, true, true);
        assert_eq!(wrapped.neighbors(corner).count(), 6);
        assert_eq!(wrapped.neighbor(corner, Dir::W), wrapped.idx(9, 0));
    }

    #[test]
    fn rings_start_east_and_turn_counter_clockwise() {
        let g = grid(20, 20, false, false);
        let c = g.idx(10, 10).expect("on the map");
        let ring1 = g.ring(c, 1);
        let dirs: Vec<TileIdx> = g.neighbors(c).collect();
        assert_eq!(ring1, dirs);
        let within = g.within(c, 3);
        assert_eq!(within.len(), 37);
        let dist: Vec<u32> = within.iter().map(|&t| g.distance(c, t)).collect();
        assert!(dist.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(g.ring(c, 3).len(), 18);
    }

    #[test]
    fn wrapping_takes_the_short_way() {
        let g = grid(20, 10, true, false);
        let a = g.idx(0, 5).expect("on the map");
        let b = g.idx(19, 5).expect("on the map");
        assert_eq!(g.distance(a, b), 1);
        assert_eq!(g.line(a, b), [a, b]);
        assert_eq!(g.unwrapped_xy(a, b), (-1, 5));
        assert_eq!(g.direction_name(a, b), "W");
        // A radius that wraps all the way round still lists each tile once.
        let all = g.within(a, 100);
        assert_eq!(all.len(), 200);
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 200);
    }

    /// `any_within` answers as a search of `within` would, flat or wrapping, and stops early.
    #[test]
    fn any_within_agrees_with_within() {
        for g in [grid(12, 10, false, false), grid(8, 8, true, true), grid(20, 10, true, false)] {
            for center in [TileIdx(0), TileIdx(g.size() / 2 + 3)] {
                for radius in [0, 1, 2, 5, 40] {
                    let near = g.within(center, radius);
                    for target in g.tiles() {
                        let got = g.any_within(center, radius, |t| t == target);
                        assert_eq!(got, near.contains(&target), "{radius} from {center:?}");
                    }
                }
            }
            let mut asked = 0;
            assert!(g.any_within(TileIdx(0), 5, |_| {
                asked += 1;
                true
            }));
            assert_eq!(asked, 1, "the centre passes, and nothing else is asked");
            assert!(!g.any_within(TileIdx(g.size()), 2, |_| true), "off the map");
        }
    }

    #[test]
    fn odd_height_cannot_wrap_north_south() {
        let g = grid(10, 9, false, true);
        assert!(!g.wrap_y());
    }

    #[test]
    fn sides_are_eight_to_256() {
        for (w, h) in [(0, 5), (7, 8), (8, 7), (257, 8), (8, 257), (u16::MAX, u16::MAX)] {
            assert_eq!(
                HexGrid::new(w, h, true, true),
                Err(HexError::Size { width: w, height: h }),
                "{w}x{h}"
            );
        }
        assert_eq!(grid(8, 8, true, true).size(), 64);
        assert_eq!(grid(256, 256, false, false).size(), 65_536);
    }

    /// The smallest wrapping map still has six distinct neighbours everywhere.
    #[test]
    fn smallest_wrapping_map_has_six_neighbours() {
        let g = grid(8, 8, true, true);
        for t in g.tiles() {
            let mut n: Vec<TileIdx> = g.neighbors(t).collect();
            n.sort();
            n.dedup();
            assert_eq!(n.len(), 6, "{:?}", g.xy(t));
            assert!(!n.contains(&t));
        }
    }

    /// Indices off the map, as a corrupt save could hold, never panic or allocate by their size.
    #[test]
    fn tiles_off_the_map_are_answered_without_panicking() {
        for g in [grid(8, 8, true, true), grid(256, 256, true, false), grid(8, 256, false, true)] {
            let on = TileIdx(3);
            for off in [TileIdx(g.size()), TileIdx(u32::MAX), TileIdx(u32::MAX / 2)] {
                assert_eq!(g.distance(on, off), u32::MAX);
                assert_eq!(g.distance(off, off), u32::MAX);
                assert!(g.line(on, off).is_empty());
                assert!(g.line(off, on).is_empty());
                assert!(g.within(off, 3).is_empty());
                assert!(g.ring(off, 2).is_empty());
                assert_eq!(g.neighbors(off).count(), 0);
                assert_eq!(g.unwrapped_xy(on, off), g.xy(off));
                assert_eq!(g.unwrapped_xy(off, on), g.xy(on));
                assert!(!g.direction_name(on, off).is_empty());
                assert!(!g.direction_name(off, on).is_empty());
            }
            assert_eq!(g.within(on, u32::MAX).len(), g.size() as usize);
        }
    }
}
