//! Landmasses: each tile's continent (`mapgen._components` and `_assign_continents`,
//! `mapgen.py:359-377, 743-751`).
//!
//! The landmasses are numbered from the largest, 0 first, so later rules can ask whether two
//! tiles are on the same one; water has none. Of two landmasses of one size, the one holding the
//! lower tile index comes first, as Python's stable sort left them.

use crate::base::hex::HexGrid;
use crate::base::ids::TileIdx;
use crate::rules::Ruleset;
use crate::rules::defs::TerrainType;
use crate::state::map::{Tile, WATER};

/// The connected regions of `mask`, each in the order a search from its lowest tile found it, the
/// regions in the order of their lowest tile (`_components`).
#[must_use]
pub fn components(grid: &HexGrid, mask: &[bool]) -> Vec<Vec<TileIdx>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in grid.tiles() {
        let i = start.0 as usize;
        if !mask.get(i).copied().unwrap_or(false) || seen[i] {
            continue;
        }
        seen[i] = true;
        let mut stack = vec![start];
        let mut comp = Vec::new();
        while let Some(c) = stack.pop() {
            comp.push(c);
            for n in grid.neighbors(c) {
                let j = n.0 as usize;
                if mask.get(j).copied().unwrap_or(false) && !seen[j] {
                    seen[j] = true;
                    stack.push(n);
                }
            }
        }
        out.push(comp);
    }
    out
}

/// Each tile's continent, [`WATER`] for water: the landmasses numbered from the largest.
#[must_use]
pub fn assign(rules: &Ruleset, grid: &HexGrid, tiles: &[Tile]) -> Vec<u16> {
    let land: Vec<bool> =
        tiles.iter().map(|t| rules.terrains()[t.terrain()].kind != TerrainType::Water).collect();
    let mut comps = components(grid, &land);
    // A stable sort, as Python's `sort(key=len, reverse=True)` is.
    comps.sort_by_key(|c| core::cmp::Reverse(c.len()));
    let mut out = vec![WATER; tiles.len()];
    for (k, comp) in comps.iter().enumerate() {
        // A grid holds at most 65,536 tiles, and a continent has at least one, so there are
        // fewer continents than WATER.
        let id = u16::try_from(k).unwrap_or(WATER - 1);
        for t in comp {
            out[t.0 as usize] = id;
        }
    }
    out
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::base::ids::TerrainId;

    #[test]
    fn landmasses_are_numbered_from_the_largest() {
        let r = Ruleset::shared();
        let grid = HexGrid::new(8, 8, false, false).expect("a grid");
        let land = r.lookup::<TerrainId>("Grassland").expect("grassland");
        let sea = r.lookup::<TerrainId>("Ocean").expect("ocean");
        let mut tiles = vec![Tile::new(sea); 64];
        // A one-tile island at (0,0), a three-tile one at (5,5)-(7,5).
        tiles[0] = Tile::new(land);
        for x in 5..8 {
            tiles[5 * 8 + x] = Tile::new(land);
        }
        let c = assign(r, &grid, &tiles);
        assert_eq!(c[45], 0);
        assert_eq!(c[0], 1);
        assert_eq!(c[1], WATER);
        assert_eq!(c.iter().filter(|&&x| x == 0).count(), 3);
    }
}
