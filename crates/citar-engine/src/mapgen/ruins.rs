//! Ancient ruins (`_ruins`, `mapgen.py:1650-1659`; UnCiv's `MapGenerator.spreadAncientRuins`):
//! one passable land tile in forty, spread out, none within two tiles of a start, none under a
//! feature but hills, forest or jungle.

use super::map::{GenMap, Kit};
use super::options::MapOptions;
use super::spread::spread_out;
use crate::base::hex::HexGrid;
use crate::base::ids::TileIdx;
use crate::base::num::{floor_i64, round_half_even};
use crate::base::rng::Rng;
use crate::base::sets::FeatureSet;
use crate::rules::Ruleset;
use crate::state::map::Tile;

/// Spreads the ruins; nothing if the ruleset has no `Ancient ruins` improvement.
pub(crate) fn ruins(m: &mut GenMap<'_>, rng: &mut Rng, starts: &[TileIdx], cs: &[TileIdx]) {
    let kit = m.kit;
    let Some(ruins) = kit.ruins else { return };
    let mut open = FeatureSet::EMPTY;
    open.insert(kit.hill);
    for f in [kit.names.forest, kit.names.jungle].into_iter().flatten() {
        if let Some(id) = kit.r.terrains()[f].feature {
            open.insert(id);
        }
    }
    let mut occupied = vec![false; m.tiles.len()];
    for &s in starts.iter().chain(cs) {
        for t in m.grid.within(s, 2) {
            occupied[t.0 as usize] = true;
        }
    }
    let ok: Vec<TileIdx> = m
        .all()
        .filter(|&t| {
            let tile = m.tile(t);
            let f = tile.features();
            m.land(t)
                && !m.impassable(t)
                && tile.improvement().is_none()
                && !occupied[t.0 as usize]
                && tile.wonder().is_none()
                && f.bits() & !open.bits() == 0
        })
        .collect();
    let count = usize::try_from(floor_i64(round_half_even(ok.len() as f64 * 0.025))).unwrap_or(0);
    for t in spread_out(m, rng, count, &ok) {
        let tile = m.tile(t).with_improvement(Some(ruins));
        m.set(t, tile);
    }
}

/// The ruins on a map from anywhere: `maps.prepare` spreads them on an editor document that has
/// none (`maps.py:342-343`; package 1c-09, drawing from `Purpose::MapPrepare`).
pub fn ruins_on(
    rules: &Ruleset,
    grid: &HexGrid,
    tiles: &mut [Tile],
    rng: &mut Rng,
    starts: &[TileIdx],
    cs: &[TileIdx],
) {
    let Some(kit) = Kit::new(rules) else { return };
    let opts = MapOptions::default();
    let mut m = GenMap::new(&kit, &opts, grid.clone(), tiles.to_vec(), vec![false; tiles.len()]);
    ruins(&mut m, rng, starts, cs);
    tiles.copy_from_slice(&m.tiles);
}
