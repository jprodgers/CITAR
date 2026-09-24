//! Natural wonders (`mapgen.py:720-740, 1103-1188`: `_fits`, `_natural_wonders`, `_try_wonder`,
//! `_place_wonder`; UnCiv's `NaturalWonderGenerator`).
//!
//! A map of this size gets its number of wonders, drawn by weight; each goes where its terrain,
//! its neighbours, its latitude and its landmass allow, away from the starts and from the other
//! wonders. The wonders that fit fewest tiles are placed first; if some cannot be placed, the
//! ones not drawn fill in.
//!
//! What differs from Python: a neighbour turned by `Neighboring tiles will convert to` is held
//! to all of the unique's conditions, where Python read only `<in tiles without [...]>`; and a
//! neighbour that becomes water loses its rivers, which Python left running along the sea.

use super::map::GenMap;
use crate::base::ids::{TerrainId, TileIdx};
use crate::base::num::{floor_i64, round_half_even};
use crate::base::rng::Rng;
use crate::base::sets::FeatureSet;
use crate::rules::defs::TerrainType;
use crate::rules::gen_tables::NaturalWonderGen;

/// Whether wonder `w`'s own conditions allow tile `t` (`_fits`): how many neighbours pass each
/// filter, the latitude, and whether the tile's landmass is among the largest.
pub(crate) fn fits(m: &GenMap<'_>, w: TerrainId, t: TileIdx) -> bool {
    fits_gen(&m.kit.r.gen_tables().wonders[w], m, t)
}

/// [`fits`] for a wonder's table entry.
fn fits_gen(g: &NaturalWonderGen, m: &GenMap<'_>, t: TileIdx) -> bool {
    for c in &g.neighbours {
        let n = m.grid.neighbors(t).filter(|&x| m.matches(c.tiles, x)).count();
        let n = i32::try_from(n).unwrap_or(i32::MAX);
        if n < c.min || n > c.max {
            return false;
        }
    }
    // Landmasses are numbered from the largest, so the n largest are those below n; a count
    // beyond the number of landmasses takes them all, as Python's slice did.
    let largest = |n: i32| {
        let c = m.continent[t.0 as usize];
        c != crate::state::map::WATER && i64::from(c) < i64::from(n)
    };
    if g.not_on_largest.iter().any(|&n| largest(n)) {
        return false;
    }
    if !g.on_largest.iter().all(|&n| largest(n)) {
        return false;
    }
    let lat = m.lat[t.0 as usize];
    g.latitudes.iter().all(|&(lo, hi)| f64::from(lo) / 100.0 <= lat && lat <= f64::from(hi) / 100.0)
}

/// Places the natural wonders, away from `avoid` (`_natural_wonders`).
pub(crate) fn natural_wonders(m: &mut GenMap<'_>, rng: &mut Rng, radius: i32, avoid: &[TileIdx]) {
    let r = m.kit.r;
    let number =
        usize::try_from(floor_i64(round_half_even(f64::from(radius) * 0.124 + 0.1))).unwrap_or(0);
    let weight = |w: TerrainId| f64::from(r.terrains()[w].weight.unwrap_or(10).max(0));
    let mut pool: Vec<TerrainId> = m.kit.wonders.clone();
    let mut chosen = Vec::new();
    while !pool.is_empty() && chosen.len() < number {
        let total: f64 = pool.iter().map(|&w| weight(w)).sum();
        let x = rng.unit() * total;
        let mut acc = 0.0;
        let mut pick = pool.len() - 1;
        for (k, &w) in pool.iter().enumerate() {
            acc += weight(w);
            if x <= acc {
                pick = k;
                break;
            }
        }
        chosen.push(pool.remove(pick));
    }
    let mut too_close = vec![false; m.tiles.len()];
    for &s in avoid {
        for j in m.grid.within(s, 5) {
            too_close[j.0 as usize] = true;
        }
    }
    let mut blocked = vec![false; m.tiles.len()];
    let candidates = |m: &GenMap<'_>, w: TerrainId, blocked: &[bool]| -> Vec<TileIdx> {
        let occurs = &r.terrains()[w].occurs_on;
        m.all()
            .filter(|&t| {
                let tile = m.tile(t);
                let i = t.0 as usize;
                tile.resource().is_none()
                    && !too_close[i]
                    && !blocked[i]
                    && tile.wonder().is_none()
                    && occurs.contains(&m.last(t))
                    && fits(m, w, t)
            })
            .collect()
    };
    let mut spawned = 0;
    // The wonders with the fewest candidate tiles first; the blocked tiles do not count, as
    // Python's sort key did not remove them.
    let unblocked = vec![false; m.tiles.len()];
    let order = |m: &GenMap<'_>, list: Vec<TerrainId>| {
        let mut keyed: Vec<(usize, TerrainId)> =
            list.into_iter().map(|w| (candidates(m, w, &unblocked).len(), w)).collect();
        keyed.sort_by_key(|&(n, _)| n);
        keyed.into_iter().map(|(_, w)| w).collect::<Vec<_>>()
    };
    for w in order(m, chosen) {
        let spots = candidates(m, w, &blocked);
        if try_wonder(m, rng, w, &spots, &mut blocked) {
            spawned += 1;
        }
    }
    if spawned < number {
        for w in order(m, pool) {
            if spawned >= number {
                break;
            }
            let spots = candidates(m, w, &blocked);
            if try_wonder(m, rng, w, &spots, &mut blocked) {
                spawned += 1;
            }
        }
    }
}

/// Places one wonder, a group of tiles if it comes in groups (`_try_wonder`).
fn try_wonder(
    m: &mut GenMap<'_>,
    rng: &mut Rng,
    w: TerrainId,
    spots: &[TileIdx],
    blocked: &mut [bool],
) -> bool {
    let (lo, hi) = m.kit.r.gen_tables().wonders[w].group.unwrap_or((1, 1));
    let size = if lo == hi {
        hi
    } else {
        i32::try_from(rng.range(i64::from(lo), i64::from(hi))).unwrap_or(lo)
    };
    let (lo, size) = (usize::try_from(lo).unwrap_or(0), usize::try_from(size).unwrap_or(0));
    if spots.len() < lo {
        return false;
    }
    let Some(&first) = rng.pick(spots) else { return false };
    let mut group = vec![first];
    while group.len() < size {
        let next: Vec<TileIdx> = spots
            .iter()
            .copied()
            .filter(|&s| !group.contains(&s) && group.iter().any(|&g| m.grid.distance(g, s) == 1))
            .collect();
        let Some(&c) = rng.pick(&next) else { break };
        group.push(c);
    }
    if group.len() < lo {
        return false;
    }
    let reach = u32::from(m.grid.height() / 5).max(2);
    for &t in &group {
        place_wonder(m, w, t);
        for j in m.grid.within(t, reach) {
            blocked[j.0 as usize] = true;
        }
    }
    true
}

/// Puts a natural wonder on a tile, with the terrain changes it brings (`_place_wonder`): the
/// tile turns into its base terrain, loses its features and resource, and neighbours convert.
pub(crate) fn place_wonder(m: &mut GenMap<'_>, w: TerrainId, t: TileIdx) {
    let r = m.kit.r;
    let def = &r.terrains()[w];
    let tile =
        m.tile(t).with_wonder(Some(w)).with_resource(None, 0).with_features(FeatureSet::EMPTY);
    m.set(t, tile);
    if let Some(into) = def.turns_into {
        m.set_terrain(t, into);
    }
    for (to, cond) in &r.gen_tables().wonders[w].converts {
        let to = *to;
        if !matches!(r.terrains()[to].kind, TerrainType::Land | TerrainType::Water) {
            continue;
        }
        let around: Vec<TileIdx> = m.grid.neighbors(t).collect();
        for n in around {
            let nt = m.tile(n);
            // refcheck: mapgen-wonder-conversions-read-every-condition
            if nt.wonder().is_some() || nt.terrain() == to || !m.holds(cond, n) {
                continue;
            }
            m.set_terrain(n, to);
            let cleared = m.tile(n).with_features(FeatureSet::EMPTY).with_resource(None, 0);
            m.set(n, cleared);
        }
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::base::hex::HexGrid;
    use crate::mapgen::map::Kit;
    use crate::mapgen::options::MapOptions;
    use crate::rules::Ruleset;
    use crate::state::map::Tile;

    /// `Must be on [n] largest landmasses` and `Must not be on [n] largest landmasses` (the
    /// kitchen sink's extra, and the shipped one): the landmasses are numbered from the largest,
    /// and a count beyond their number takes them all.
    #[test]
    fn a_wonder_can_want_the_largest_landmasses_or_shun_them() {
        let r = Ruleset::shared();
        let kit = Kit::new(r).expect("a kit");
        let opts = MapOptions::default();
        let grid = HexGrid::new(12, 8, false, false).expect("grid");
        // A five-tile landmass in the west and a two-tile one in the east.
        let mut tiles = vec![Tile::new(kit.ocean); 96];
        for x in 1..6 {
            tiles[2 * 12 + x] = Tile::new(kit.land);
        }
        tiles[5 * 12 + 9] = Tile::new(kit.land);
        tiles[5 * 12 + 10] = Tile::new(kit.land);
        let mut m = GenMap::new(&kit, &opts, grid, tiles, vec![false; 96]);
        m.assign_continents();
        let (big, small, sea) = (TileIdx(2 * 12 + 3), TileIdx(5 * 12 + 9), TileIdx(0));
        let on = |n: i32| NaturalWonderGen { on_largest: vec![n], ..NaturalWonderGen::default() };
        let off =
            |n: i32| NaturalWonderGen { not_on_largest: vec![n], ..NaturalWonderGen::default() };
        assert!(fits_gen(&on(1), &m, big));
        assert!(!fits_gen(&on(1), &m, small));
        assert!(!fits_gen(&on(1), &m, sea), "the sea is on no landmass");
        assert!(fits_gen(&on(5), &m, small), "five largest of two: both");
        assert!(!fits_gen(&on(0), &m, big), "none of them");
        assert!(!fits_gen(&off(1), &m, big));
        assert!(fits_gen(&off(1), &m, small));
        assert!(fits_gen(&off(9), &m, sea));
        assert!(!fits_gen(&off(9), &m, small));
        let polar = NaturalWonderGen { latitudes: vec![(90, 100)], ..NaturalWonderGen::default() };
        assert!(fits_gen(&polar, &m, TileIdx(0)) && !fits_gen(&polar, &m, TileIdx(3 * 12)));
    }
}
