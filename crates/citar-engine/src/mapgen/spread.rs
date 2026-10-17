//! Choosing tiles far apart (`_spread_out`, `mapgen.py:902-942`; UnCiv's
//! `MapGenerationRandomness.chooseSpreadOutLocations`), which rivers, resources and ruins use.
//!
//! The tiles are grouped by base terrain, and each pick comes from the group picked least so far,
//! so a spread mixes the terrains it is given. Every pick removes the tiles within a distance of
//! it; the distance shrinks until enough fit, so a crowded map still places everything.

use super::map::GenMap;
use crate::base::ids::{TerrainId, TileIdx};
use crate::base::num::{floor_i64, pow, round_half_even, trunc_i64};
use crate::base::rng::Rng;

/// `number` tiles from `tiles`, as far apart as they reasonably can be; fewer only when the
/// tiles run out at distance 1.
pub(crate) fn spread_out(
    m: &GenMap<'_>,
    rng: &mut Rng,
    number: usize,
    tiles: &[TileIdx],
) -> Vec<TileIdx> {
    if number == 0 || tiles.is_empty() {
        return Vec::new();
    }
    let size = f64::from(m.grid.size());
    let radius = (trunc_i64(size.sqrt() / 2.0)).max(10) as f64;
    let hex_radius = |area: f64| (area.max(1.0) / 3.0).sqrt();
    let sparsity = pow(hex_radius(tiles.len() as f64) / radius, 0.333);
    let initial = if number == 1 || number * 5 >= tiles.len() * 3 {
        1
    } else {
        let d = radius * 0.666 / pow(hex_radius(number as f64), 0.9) * sparsity;
        u32::try_from(floor_i64(round_half_even(d)).max(1)).unwrap_or(1)
    };

    // The groups in the order their terrain first comes, each sorted by tile.
    let mut keys: Vec<TerrainId> = Vec::new();
    let mut groups: Vec<Vec<TileIdx>> = Vec::new();
    let mut group_of = vec![u32::MAX; m.tiles.len()];
    for &t in tiles {
        let terrain = m.tile(t).terrain();
        let g = match keys.iter().position(|&k| k == terrain) {
            Some(g) => g,
            None => {
                keys.push(terrain);
                groups.push(Vec::new());
                keys.len() - 1
            }
        };
        let slot = &mut group_of[t.0 as usize];
        if *slot == u32::MAX {
            *slot = u32::try_from(g).unwrap_or(u32::MAX);
            groups[g].push(t);
        }
    }
    for g in &mut groups {
        g.sort();
    }

    let mut close = Vec::new();
    for dist in (1..=initial).rev() {
        let mut avail = groups.clone();
        let mut gone = vec![false; m.tiles.len()];
        let mut alive: Vec<usize> = avail.iter().map(Vec::len).collect();
        let mut counts = vec![0usize; avail.len()];
        let mut chosen = Vec::with_capacity(number);
        for _ in 0..number {
            // The group picked least so far, the first of equals, that has a tile left.
            let Some(g) =
                (0..avail.len()).filter(|&g| alive[g] > 0).min_by_key(|&g| (counts[g], g))
            else {
                break;
            };
            if alive[g] * 2 < avail[g].len() {
                avail[g].retain(|t| !gone[t.0 as usize]);
            }
            let k = usize::try_from(rng.below(alive[g] as u64)).unwrap_or(0);
            let Some(c) = avail[g].iter().copied().filter(|t| !gone[t.0 as usize]).nth(k) else {
                break;
            };
            m.grid.within_into(c, dist, &mut close);
            for &x in &close {
                let xi = x.0 as usize;
                let xg = group_of[xi];
                if xg != u32::MAX && !gone[xi] {
                    gone[xi] = true;
                    alive[xg as usize] -= 1;
                }
            }
            chosen.push(c);
            counts[g] += 1;
        }
        if chosen.len() == number || dist == 1 {
            return chosen;
        }
    }
    Vec::new()
}
