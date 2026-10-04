//! `PathEquivalent`: when a different route is as good as Python's (DESIGN.md 9.2 and 6.10).
//!
//! Rust's A* breaks ties between equal-cost routes its own way, so a route is not compared tile by
//! tile. A different route is accepted when it starts and ends on Python's tiles, steps between
//! adjacent tiles, and takes the same turns for the same summed step cost. A route with fewer
//! turns is `Better`, which needs an intended entry with `rule = "rust_le_python"`.
//!
//! Adjacency is worked out here from the map's size and wrapping, following `hexmap.py:15-63`,
//! rather than asked of the engine: a check that trusted the engine's own neighbours would pass
//! whatever the engine got wrong.

use serde_json::Value;

/// A hex map's geometry: odd-r offset coordinates, tile index `y * width + x`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    width: u32,
    height: u32,
    wrap_x: bool,
    wrap_y: bool,
}

/// The six directions on even and odd rows, in `hexmap.py`'s order (E, NE, NW, W, SW, SE).
const DIRS_EVEN: [(i64, i64); 6] = [(1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1)];
const DIRS_ODD: [(i64, i64); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)];

impl Grid {
    /// North-south wrapping needs an even height, as in `HexGrid.__init__` (`hexmap.py:47`).
    pub fn new(width: u32, height: u32, wrap_x: bool, wrap_y: bool) -> Grid {
        Grid { width, height, wrap_x, wrap_y: wrap_y && height.is_multiple_of(2) }
    }

    pub fn size(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    fn wrap(&self, x: i64, y: i64) -> Option<u64> {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let x = if self.wrap_x { x.rem_euclid(w) } else { x };
        let y = if self.wrap_y { y.rem_euclid(h) } else { y };
        if (0..w).contains(&x) && (0..h).contains(&y) {
            u64::try_from(y * w + x).ok()
        } else {
            None
        }
    }

    /// The tiles adjacent to `idx`, as `HexGrid.neighbors` lists them.
    pub fn neighbors(&self, idx: u64) -> Vec<u64> {
        let mut out = Vec::with_capacity(6);
        if self.width == 0 || idx >= self.size() {
            return out;
        }
        let w = u64::from(self.width);
        let (x, y) = ((idx % w) as i64, (idx / w) as i64);
        let dirs = if y & 1 == 1 { DIRS_ODD } else { DIRS_EVEN };
        for (dx, dy) in dirs {
            if let Some(n) = self.wrap(x + dx, y + dy)
                && n != idx
                && !out.contains(&n)
            {
                out.push(n);
            }
        }
        out
    }

    pub fn adjacent(&self, a: u64, b: u64) -> bool {
        self.neighbors(a).contains(&b)
    }
}

/// Why a route of Rust's cannot stand in for Python's, if it cannot. `python` and `rust` are the
/// two tile lists, and `rust_costs` Rust's step costs.
pub fn invalid_route(
    grid: Option<&Grid>,
    python: &[Value],
    rust: &[Value],
    rust_costs: Option<&Value>,
) -> Option<String> {
    let Some(grid) = grid else {
        return Some("no map to check the steps against".into());
    };
    let tiles: Vec<u64> = match rust.iter().map(Value::as_u64).collect::<Option<Vec<u64>>>() {
        Some(t) if !t.is_empty() => t,
        Some(_) => return Some("Rust's route is empty".into()),
        None => return Some("Rust's route holds something that is not a tile index".into()),
    };
    let (first, last) = (tiles[0], tiles[tiles.len() - 1]);
    if python.first().and_then(Value::as_u64) != Some(first) {
        return Some(format!("Rust's route starts at tile {first}, not Python's start"));
    }
    if python.last().and_then(Value::as_u64) != Some(last) {
        return Some(format!("Rust's route ends at tile {last}, not Python's end"));
    }
    for (k, pair) in tiles.windows(2).enumerate() {
        if !grid.adjacent(pair[0], pair[1]) {
            return Some(format!(
                "Rust's steps {k} and {} (tiles {} and {}) are not adjacent",
                k + 1,
                pair[0],
                pair[1]
            ));
        }
    }
    match rust_costs.and_then(Value::as_array) {
        Some(costs) if costs.len() + 1 == tiles.len() && costs.iter().all(Value::is_number) => None,
        Some(costs) => Some(format!("{} step costs for {} steps", costs.len(), tiles.len() - 1)),
        None => Some("Rust gave no step costs".into()),
    }
}

/// The sum of a list of step costs, if it is one.
pub fn summed_cost(costs: Option<&Value>) -> Option<f64> {
    costs?.as_array()?.iter().map(Value::as_f64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn neighbours_follow_hexmap_py() {
        // A 4x4 map without wrapping. Tile 5 is (1, 1), an odd row.
        let g = Grid::new(4, 4, false, false);
        assert_eq!(g.neighbors(5), [6, 2, 1, 4, 9, 10]);
        // Tile 0 is (0, 0), a corner on an even row.
        assert_eq!(g.neighbors(0), [1, 4]);
        assert!(g.adjacent(5, 9) && g.adjacent(9, 5));
        assert!(!g.adjacent(0, 5));
    }

    #[test]
    fn wrapping_joins_the_edges() {
        let g = Grid::new(4, 4, true, false);
        // (0, 0) wraps west to (3, 0) and south-west to (3, 1); north is still off the map.
        assert_eq!(g.neighbors(0), [1, 3, 7, 4]);
        let both = Grid::new(4, 4, true, true);
        assert!(both.adjacent(0, 12), "north wraps to the bottom row");
        // An odd height cannot wrap north-south.
        let odd = Grid::new(4, 5, false, true);
        assert!(!odd.adjacent(0, 16));
    }

    #[test]
    fn a_route_must_join_the_same_ends_through_adjacent_tiles() {
        let g = Grid::new(4, 4, false, false);
        // 1 is (1, 0) and 6 is (2, 1): through 5 below or through 2 beside.
        let python = [json!(1), json!(5), json!(6)];
        let good = [json!(1), json!(2), json!(6)];
        assert_eq!(invalid_route(Some(&g), &python, &good, Some(&json!([60, 60]))), None);
        let jump = [json!(1), json!(6)];
        assert!(invalid_route(Some(&g), &python, &jump, Some(&json!([60]))).is_some());
        let wrong_end = [json!(1), json!(2)];
        assert!(invalid_route(Some(&g), &python, &wrong_end, Some(&json!([60]))).is_some());
        assert!(invalid_route(Some(&g), &python, &good, Some(&json!([60]))).is_some());
        assert!(invalid_route(None, &python, &good, Some(&json!([60, 60]))).is_some());
        assert_eq!(summed_cost(Some(&json!([60, 30.5]))), Some(90.5));
        assert_eq!(summed_cost(Some(&json!([60, "x"]))), None);
    }
}
