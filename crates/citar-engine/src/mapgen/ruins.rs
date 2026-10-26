//! Ancient ruins (`_ruins`, `mapgen.py:1650-1659`; UnCiv's `MapGenerator.spreadAncientRuins`):
//! one passable land tile in forty, spread out, none within two tiles of a start, none under a
//! feature but hills, forest or jungle.

use super::document::{MapDocument, MapError};
use super::map::{GenMap, Kit};
use super::options::MapOptions;
use super::spread::spread_out;
use crate::base::ids::TileIdx;
use crate::base::num::{floor_i64, round_half_even};
use crate::base::rng::Rng;
use crate::base::sets::FeatureSet;
use crate::rules::Ruleset;

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

/// The ruins on an editor map: `maps.prepare` spreads them on a document that has none
/// (`maps.py:342-343`; package 1c-09, drawing from `Purpose::MapPrepare`), none within two tiles
/// of `starts` or `cs`. A ruleset without land or water terrain, or without ruins, adds none.
///
/// # Errors
/// A document whose tiles do not fill its size, or a start off the map; the document is left
/// as it was.
pub fn ruins_on(
    rules: &Ruleset,
    doc: &mut MapDocument,
    rng: &mut Rng,
    starts: &[TileIdx],
    cs: &[TileIdx],
) -> Result<(), MapError> {
    let grid = doc.checked_grid(&[starts, cs])?;
    let Some(kit) = Kit::new(rules) else { return Ok(()) };
    let opts = MapOptions::default();
    let n = doc.tiles.len();
    let mut m = GenMap::new(&kit, &opts, grid, core::mem::take(&mut doc.tiles), vec![false; n]);
    ruins(&mut m, rng, starts, cs);
    doc.tiles = m.tiles;
    Ok(())
}
