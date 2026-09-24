//! Line of sight: the ring-by-ring elevation walk (`visibility.py:14-71`, UnCiv's
//! `TileMap.getViewableTiles`), and the cache of its answers (DESIGN.md 6.5, 6.9).
//!
//! A tile has two heights: the one a viewer stands at, the sum of its terrains'
//! `Has an elevation of [n] for visibility calculations` (hills 1, mountains 4 in the shipped
//! ruleset), and the one it blocks sight at, one more when a terrain on it
//! `Blocks line-of-sight from tiles at same elevation` (forest, jungle). Python kept both lists in
//! `g._static`, cleared with every other terrain cache; here [`Heights`] keeps them per tile,
//! and a change of one tile's terrains updates that tile alone.
//!
//! [`LosCache`] replaces the `("los", idx, sight, for_attack)` entries of `g._static`. An answer
//! reads the heights of the tiles within its radius plus one of its centre, so a change of heights
//! at a tile evicts exactly the answers whose centre is that near ([`LosCache::evict_near`]);
//! Python dropped them all.

use std::sync::Arc;

use crate::base::collections::DetMap;
use crate::base::hex::HexGrid;
use crate::base::ids::{Id, TerrainId, TileIdx};
use crate::base::sets::TerrainSet;
use crate::rules::Ruleset;
use crate::state::map::{Tile, Tiles};
use crate::unique::UniqueData;

/// What each terrain adds to the heights of a tile it is on, read once from the ruleset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerrainHeights {
    /// The sum of the terrain's `Has an elevation of [n]`, whatever their conditionals, as
    /// Python's `UniqueMap.get` read them.
    elevation: Vec<i32>,
    /// The terrains that `Block line-of-sight from tiles at same elevation`.
    blocks: TerrainSet,
}

impl TerrainHeights {
    fn new(rules: &Ruleset) -> Self {
        let t = rules.uniques();
        let mut elevation = Vec::with_capacity(rules.terrains().len());
        let mut blocks = TerrainSet::new();
        for (id, def) in rules.terrains().iter() {
            let mut e = 0i32;
            for u in def.uniques.ids() {
                match t.get(u).data {
                    UniqueData::VisibilityElevation(x) => e = e.saturating_add(x.elevation),
                    UniqueData::BlocksLineOfSightAtSameElevation => {
                        blocks.insert(id);
                    }
                    _ => {}
                }
            }
            elevation.push(e);
        }
        Self { elevation, blocks }
    }

    /// The heights of a tile: `(where a viewer stands, what it blocks at)` (`_heights`).
    fn of(&self, rules: &Ruleset, tile: &Tile) -> (i32, i32) {
        let mut stand = 0i32;
        let mut blocks = false;
        let mut add = |t: TerrainId| {
            stand = stand.saturating_add(self.elevation.get(t.index()).copied().unwrap_or(0));
            blocks |= self.blocks.contains(t);
        };
        // `all_terrains` (tiles.py:38-44): the base terrain, the natural wonder, the features.
        add(tile.terrain());
        if let Some(w) = tile.wonder() {
            add(w);
        }
        let features = &rules.derived().features;
        for f in tile.features().iter() {
            if let Some(&t) = features.get(f) {
                add(t);
            }
        }
        (stand, if blocks { stand.saturating_add(1) } else { stand })
    }
}

/// Every tile's two heights for sight (`visibility._heights`, `visibility.py:14-31`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heights {
    terrains: TerrainHeights,
    /// `(where a viewer stands, what the tile blocks at)` per tile.
    tiles: Vec<(i32, i32)>,
}

impl Heights {
    /// The heights of every tile of `tiles`.
    #[must_use]
    pub fn new(rules: &Ruleset, tiles: &Tiles) -> Self {
        let terrains = TerrainHeights::new(rules);
        let tiles = tiles.iter().map(|(_, x)| terrains.of(rules, x)).collect();
        Self { terrains, tiles }
    }

    /// Reads tile `t`'s heights again after its terrains changed.
    pub fn update(&mut self, rules: &Ruleset, t: TileIdx, tile: &Tile) {
        let h = self.terrains.of(rules, tile);
        if let Some(slot) = self.tiles.get_mut(t.0 as usize) {
            *slot = h;
        }
    }

    /// The height a viewer on `t` stands at (`uh`).
    #[must_use]
    #[inline]
    pub fn stand(&self, t: TileIdx) -> i32 {
        self.tiles.get(t.0 as usize).map_or(0, |h| h.0)
    }

    /// The height tile `t` blocks sight at (`th`).
    #[must_use]
    #[inline]
    pub fn block(&self, t: TileIdx) -> i32 {
        self.tiles.get(t.0 as usize).map_or(0, |h| h.1)
    }
}

/// Buffers [`viewable_into`] reuses between calls: the ring, and per tile of the map the layer
/// that last reached it (a generation number) with the highest height seen on the way to it.
#[derive(Clone, Debug, Default)]
pub struct LosScratch {
    ring: Vec<TileIdx>,
    reached: Vec<TileIdx>,
    /// The generation of the layer that last reached each tile.
    layer: Vec<u32>,
    /// The highest height seen on the way to each tile, when its layer reached it.
    height: Vec<i32>,
    /// The generation of the last layer handed out.
    generation: u32,
}

impl LosScratch {
    /// A fresh generation for a layer, the per-tile marks grown to the map and cleared when the
    /// count would run out.
    fn next(&mut self, tiles: usize) -> u32 {
        if self.layer.len() < tiles {
            self.layer.resize(tiles, 0);
            self.height.resize(tiles, 0);
        }
        if self.generation == u32::MAX {
            self.layer.fill(0);
            self.generation = 0;
        }
        self.generation += 1;
        self.generation
    }

    fn mark(&mut self, t: TileIdx, generation: u32, h: i32) {
        if let Some(x) = self.layer.get_mut(t.0 as usize) {
            *x = generation;
        }
        if let Some(x) = self.height.get_mut(t.0 as usize) {
            *x = h;
        }
    }

    /// The height seen on the way to `t` if the layer of `generation` reached it.
    fn seen(&self, t: TileIdx, generation: u32) -> Option<i32> {
        let i = t.0 as usize;
        (self.layer.get(i) == Some(&generation)).then(|| self.height.get(i).copied().unwrap_or(0))
    }
}

/// Every tile visible from `idx` at `sight`, sorted (`visibility.viewable_from`,
/// `visibility.py:34-66`); with `for_attack`, what a ranged attack may target
/// (`TileMap.getViewableTiles(forAttack = true)`).
///
/// The walk goes ring by ring. A tile of ring `i` is reached from its lowest neighbour on ring
/// `i - 1` that was reached, and remembers the highest height seen on the way to it; it is seen
/// when the viewer stands at least that high, or when it rises above it. The ring beyond the
/// sight radius adds only the tiles higher than the viewer (mountains in the distance), and none
/// for an attack.
#[must_use]
pub fn viewable_from(
    grid: &HexGrid,
    h: &Heights,
    idx: TileIdx,
    sight: u32,
    for_attack: bool,
) -> Vec<TileIdx> {
    let mut out = Vec::new();
    viewable_into(grid, h, idx, sight, for_attack, &mut LosScratch::default(), &mut out);
    out
}

/// [`viewable_from`] into `out`, with buffers the caller keeps.
pub fn viewable_into(
    grid: &HexGrid,
    h: &Heights,
    idx: TileIdx,
    sight: u32,
    for_attack: bool,
    s: &mut LosScratch,
    out: &mut Vec<TileIdx>,
) {
    out.clear();
    if !grid.contains(idx) {
        return;
    }
    let tiles = grid.size() as usize;
    let a = h.stand(idx);
    out.push(idx);
    let mut prev = s.next(tiles);
    s.mark(idx, prev, a);
    // No ring past the width plus the height has a tile, and each ring is reached only through
    // the one before it.
    let last = sight.saturating_add(1).min(u32::from(grid.width()) + u32::from(grid.height()));
    let mut ring = core::mem::take(&mut s.ring);
    let mut reached = core::mem::take(&mut s.reached);
    for i in 1..=last {
        grid.ring_into(idx, i, &mut ring);
        reached.clear();
        let cur = s.next(tiles);
        for &c in &ring {
            let ch = h.block(c);
            if i == sight.saturating_add(1) && (ch <= a || for_attack) {
                continue;
            }
            // The lowest of the tiles of the ring before that led here.
            let mut b: Option<i32> = None;
            for n in grid.neighbors(c) {
                if let Some(seen) = s.seen(n, prev) {
                    b = Some(b.map_or(seen, |x| x.min(seen)));
                }
            }
            let Some(b) = b else { continue };
            reached.push(c);
            // Marked with this ring's generation, which no neighbour lookup of this ring reads.
            s.mark(c, cur, ch.max(b));
            let seen = if for_attack { a >= b || h.stand(c) > b } else { a >= b || ch > b };
            if seen {
                out.push(c);
            }
        }
        if reached.is_empty() {
            break;
        }
        prev = cur;
    }
    s.ring = ring;
    s.reached = reached;
    out.sort();
    out.dedup();
}

/// What a line-of-sight answer is for: its centre, its radius and whether it is an attack's.
pub type LosKey = (TileIdx, u32, bool);

/// The answers of [`viewable_from`] already worked out, until a height they read changes
/// (DESIGN.md 6.5). Never saved, and nothing but speed depends on what it holds.
#[derive(Clone, Debug, Default)]
pub struct LosCache {
    map: DetMap<LosKey, Arc<[TileIdx]>>,
    scratch: LosScratch,
    out: Vec<TileIdx>,
}

impl LosCache {
    /// Past this many answers the cache starts again: a bound on its memory, never on a result.
    pub const CAP: usize = 1 << 16;

    /// The tiles visible from `idx` at `sight`, sorted, from the cache or worked out now.
    pub fn get(
        &mut self,
        grid: &HexGrid,
        h: &Heights,
        idx: TileIdx,
        sight: u32,
        for_attack: bool,
    ) -> Arc<[TileIdx]> {
        let key = (idx, sight, for_attack);
        if let Some(v) = self.map.get(&key) {
            return Arc::clone(v);
        }
        viewable_into(grid, h, idx, sight, for_attack, &mut self.scratch, &mut self.out);
        let v: Arc<[TileIdx]> = Arc::from(self.out.as_slice());
        if self.map.len() >= Self::CAP {
            self.map.clear();
        }
        self.map.insert(key, Arc::clone(&v));
        v
    }

    /// Drops every answer that read the heights of tile `t`: those whose centre is within their
    /// radius plus one of it.
    pub fn evict_near(&mut self, grid: &HexGrid, t: TileIdx) {
        self.map.retain(|&(c, r, _), _| grid.distance(c, t) > r.saturating_add(1));
    }

    /// How many answers it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Python's walk, as written: `seen` a map from tile to height, rings from `within`.
    fn python(grid: &HexGrid, h: &Heights, idx: TileIdx, sight: u32, attack: bool) -> Vec<TileIdx> {
        let a = h.stand(idx);
        let mut seen: std::collections::BTreeMap<u32, i32> = [(idx.0, a)].into();
        let mut visible = vec![idx];
        for i in 1..=sight + 1 {
            let mut layer = Vec::new();
            for c in grid.within(idx, i).into_iter().filter(|&c| grid.distance(idx, c) == i) {
                let ch = h.block(c);
                if i == sight + 1 && (ch <= a || attack) {
                    continue;
                }
                let prev: Vec<i32> = grid
                    .neighbors(c)
                    .filter(|n| seen.contains_key(&n.0) && grid.distance(idx, *n) == i - 1)
                    .map(|n| seen[&n.0])
                    .collect();
                let Some(&b) = prev.iter().min() else { continue };
                layer.push((c, ch.max(b)));
                let ok = if attack { a >= b || h.stand(c) > b } else { a >= b || ch > b };
                if ok {
                    visible.push(c);
                }
            }
            for (c, m) in layer {
                seen.insert(c.0, m);
            }
        }
        visible.sort();
        visible
    }

    fn heights(grid: &HexGrid, f: impl FnMut(TileIdx) -> (i32, i32)) -> Heights {
        Heights { terrains: TerrainHeights::default(), tiles: grid.tiles().map(f).collect() }
    }

    #[test]
    fn flat_land_is_a_disc_and_a_mountain_blocks_what_lies_behind_it() {
        let grid = HexGrid::new(12, 12, false, false).expect("a grid");
        let flat = heights(&grid, |_| (0, 0));
        let c = grid.idx(5, 5).expect("a tile");
        let mut disc = grid.within(c, 2);
        disc.sort();
        assert_eq!(viewable_from(&grid, &flat, c, 2, false), disc);
        // A mountain east of the centre hides the tile beyond it, and shows over the rim.
        let m = grid.idx(6, 5).expect("a tile");
        let behind = grid.idx(7, 5).expect("a tile");
        let hilly = heights(&grid, |t| if t == m { (4, 4) } else { (0, 0) });
        let seen = viewable_from(&grid, &hilly, c, 2, false);
        assert!(seen.contains(&m) && !seen.contains(&behind));
        let far =
            heights(&grid, |t| if t == grid.idx(8, 5).unwrap_or(t) { (4, 4) } else { (0, 0) });
        assert!(viewable_from(&grid, &far, c, 2, false).contains(&grid.idx(8, 5).expect("a tile")));
        assert!(!viewable_from(&grid, &far, c, 2, true).contains(&grid.idx(8, 5).expect("a tile")));
    }

    #[test]
    fn the_walk_is_pythons_on_random_heights() {
        for (w, wrap) in [(10u16, false), (12, true), (16, true)] {
            let grid = HexGrid::new(w, 10, wrap, false).expect("a grid");
            let mut state = 0x9E37_79B9_u32;
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state
            };
            for _ in 0..20 {
                let h = heights(&grid, |_| {
                    let r = next() % 10;
                    let stand = match r {
                        0..=5 => 0,
                        6..=7 => 1,
                        _ => 4,
                    };
                    (stand, stand + i32::from(next() % 4 == 0))
                });
                for idx in grid.tiles().step_by(7) {
                    for sight in 1..=4 {
                        for attack in [false, true] {
                            assert_eq!(
                                viewable_from(&grid, &h, idx, sight, attack),
                                python(&grid, &h, idx, sight, attack),
                                "{idx:?} sight {sight} attack {attack}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_cache_forgets_the_answers_a_height_change_reaches() {
        let grid = HexGrid::new(20, 12, false, false).expect("a grid");
        let h = heights(&grid, |_| (0, 0));
        let mut cache = LosCache::default();
        let near = grid.idx(4, 4).expect("a tile");
        let far = grid.idx(15, 4).expect("a tile");
        let a = cache.get(&grid, &h, near, 2, false);
        let _b = cache.get(&grid, &h, far, 2, false);
        assert_eq!(cache.len(), 2);
        assert!(Arc::ptr_eq(&a, &cache.get(&grid, &h, near, 2, false)), "a hit");
        // Three tiles away: the answer of radius 2 read it (its rim of higher tiles).
        cache.evict_near(&grid, grid.idx(7, 4).expect("a tile"));
        assert_eq!(cache.len(), 1);
        cache.evict_near(&grid, grid.idx(8, 4).expect("a tile"));
        assert_eq!(cache.len(), 1, "four away is out of reach");
    }
}
