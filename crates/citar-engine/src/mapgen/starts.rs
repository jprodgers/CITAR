//! Start positions (`mapgen.py:945-1100`: `_fertility`, `_start_score`, `_bias_score`,
//! `_start_candidates`, `_choose_starts`, `_choose_cs_starts`), and the start filling
//! `maps.prepare` reuses for editor maps short of starts (`maps._fill_starts`, `maps.py:347-365`;
//! package 1c-09).
//!
//! A start's score is the fertility round it, weighted by distance, and a bonus for the coast
//! and a river. The major civilizations are placed in turn, those with a start bias first, each
//! at the best tile at least a distance from the others; a few trials keep the best spread.
//! City-states go on the best tiles left, away from the civilizations.

use super::map::{GenMap, Kit};
use super::options::MapOptions;
use crate::base::hex::HexGrid;
use crate::base::ids::{NationId, TileIdx};
use crate::base::num::trunc_i64;
use crate::base::rng::Rng;
use crate::rules::Ruleset;
use crate::rules::defs::StartBias;
use crate::state::map::Tile;

/// Start-location fertility (`_fertility`): `[+n] to Fertility for Map Generation` summed over
/// the tile's terrains, or the last `Always Fertility [n]`, plus one for a river and one for
/// fresh water; coast is 1 and other water 0.
pub(crate) fn fertility(m: &GenMap<'_>, t: TileIdx) -> f64 {
    if m.water(t) {
        return if m.is(t, m.kit.names.coast) { 1.0 } else { 0.0 };
    }
    let g = m.kit.r.gen_tables();
    let mut total = 0;
    let mut fixed = None;
    for x in m.all_terrains(t) {
        let f = g.terrains[x].fertility;
        if f.fixed.is_some() {
            fixed = f.fixed;
        }
        total += f.add;
    }
    let mut f = f64::from(fixed.unwrap_or(total));
    if m.tile(t).has_river() {
        f += 1.0;
    }
    if crate::unique::world::TileFacts::tile_fresh_water(m, t) {
        f += 1.0;
    }
    f
}

/// Every tile's start score (`_start_score`): the fertility within three tiles, full next door,
/// 0.7 two away and 0.35 three away, plus 2 on the coast and 2 on a river.
pub(crate) fn start_scores(m: &GenMap<'_>) -> Vec<f64> {
    let fert: Vec<f64> = m.all().map(|t| fertility(m, t)).collect();
    let mut area = Vec::new();
    m.all()
        .map(|t| {
            m.grid.within_into(t, 3, &mut area);
            let mut s = 0.0;
            for &n in &area {
                let w = match m.grid.distance(t, n) {
                    0 | 1 => 1.0,
                    2 => 0.7,
                    _ => 0.35,
                };
                s += fert[n.0 as usize] * w;
            }
            if m.coastal(t) {
                s += 2.0;
            }
            if m.tile(t).has_river() {
                s += 2.0;
            }
            s
        })
        .collect()
}

/// How well a tile suits a start bias (`_bias_score`): 4 for each tile within three that passes
/// a preferred filter, -3 for each that passes an avoided one, and 25 for a coastal start that
/// wants the coast (-25 for an inland one).
pub(crate) fn bias_score(m: &GenMap<'_>, t: TileIdx, bias: &[StartBias], area: &[TileIdx]) -> f64 {
    let mut s = 0.0;
    for b in bias {
        match *b {
            StartBias::Avoid(f) => {
                s -= 3.0 * area.iter().filter(|&&n| m.matches(f, n)).count() as f64;
            }
            StartBias::Coast => s += if m.coastal(t) { 25.0 } else { -25.0 },
            StartBias::Prefer(f) => {
                s += 4.0 * area.iter().filter(|&&n| m.matches(f, n)).count() as f64;
            }
        }
    }
    s
}

/// Every tile that could be a start at all (`_start_candidates`): passable land on a landmass of
/// 25 tiles or more, two tiles from an edge that does not wrap, not snow, not under marsh, an
/// oasis or ice, and no polar tundra.
pub(crate) fn start_candidates(m: &GenMap<'_>) -> Vec<TileIdx> {
    let mut sizes = vec![0u32; m.tiles.len()];
    for &c in &m.continent {
        if c != crate::state::map::WATER {
            sizes[usize::from(c)] += 1;
        }
    }
    let names = m.kit.names;
    let (w, h) = (i32::from(m.grid.width()), i32::from(m.grid.height()));
    let under = [names.marsh, names.oasis, names.ice];
    m.all()
        .filter(|&t| {
            let c = m.continent[t.0 as usize];
            if !m.land(t)
                || m.impassable(t)
                || c == crate::state::map::WATER
                || sizes[usize::from(c)] < 25
            {
                return false;
            }
            let (x, y) = m.grid.xy(t);
            if !m.grid.wrap_x() && (x < 2 || x > w - 3) {
                return false;
            }
            if !m.grid.wrap_y() && (y < 2 || y > h - 3) {
                return false;
            }
            if m.is(t, names.snow) || under.contains(&Some(m.last(t))) {
                return false;
            }
            !(m.is(t, names.tundra) && m.lat[t.0 as usize] > 0.8)
        })
        .collect()
}

/// Places the major civilizations (`_choose_starts`), honouring start biases and keeping them
/// apart; `None` when they do not fit, the caller's cue to make another map rather than squash
/// everyone together.
pub(crate) fn choose_starts(
    m: &GenMap<'_>,
    rng: &mut Rng,
    scores: &[f64],
    n: usize,
    nations: &[Option<NationId>],
) -> Option<Vec<TileIdx>> {
    let mut cand = start_candidates(m);
    if cand.len() < n || n == 0 {
        return (n == 0).then(Vec::new);
    }
    let score = |t: TileIdx| scores[t.0 as usize];
    cand.sort_by(|&a, &b| score(b).total_cmp(&score(a)));
    let land_total = m.all().filter(|&t| m.land(t) && !m.impassable(t)).count();
    let per = (land_total as f64 / n as f64).sqrt() * 0.8;
    let min_dist = i64::from(u32::try_from(trunc_i64(per)).unwrap_or(0)).max(7);
    let r = m.kit.r;
    let bias_of = |k: usize| -> &[StartBias] {
        nations.get(k).copied().flatten().map_or(&[], |x| &*r.nations()[x].start_bias)
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&k| core::cmp::Reverse(bias_of(k).len()));

    let top: Vec<TileIdx> = cand[..(n * 25).max(cand.len() / 2).min(cand.len())].to_vec();
    // Each seat's bias over the top tiles, once: the map does not change between trials.
    let mut area = Vec::new();
    let biases: Vec<Vec<f64>> = (0..n)
        .map(|k| {
            let b = bias_of(k);
            if b.is_empty() {
                return Vec::new();
            }
            top.iter()
                .map(|&t| {
                    m.grid.within_into(t, 3, &mut area);
                    bias_score(m, t, b, &area)
                })
                .collect()
        })
        .collect();

    let mut best: Option<(f64, Vec<TileIdx>)> = None;
    let mut nearest: Vec<Option<u32>> = vec![None; top.len()];
    for trial in 0..24i64 {
        let md = (min_dist - trial / 6).max(5) as f64;
        nearest.fill(None);
        let mut starts: Vec<Option<TileIdx>> = vec![None; n];
        let mut placed = 0;
        for &k in &order {
            let mut best_i = None;
            let mut best_v = -1e9;
            for (j, &t) in top.iter().enumerate() {
                let d = nearest[j];
                if d.is_some_and(|d| f64::from(d) < md) {
                    continue;
                }
                let spread = d.map_or(md * 2.0, f64::from);
                let bias = biases[k].get(j).copied().unwrap_or(0.0);
                let v = score(t) + bias + spread.min(md * 2.0) * 1.5 + rng.unit() * 6.0;
                if v > best_v {
                    best_v = v;
                    best_i = Some(t);
                }
            }
            let Some(chosen) = best_i else { break };
            starts[k] = Some(chosen);
            placed += 1;
            for (j, &t) in top.iter().enumerate() {
                let d = m.grid.distance(t, chosen);
                if nearest[j].is_none_or(|x| d < x) {
                    nearest[j] = Some(d);
                }
            }
        }
        if placed == n {
            let list: Vec<TileIdx> = starts.into_iter().flatten().collect();
            let mut spread = 99;
            for (i, &a) in list.iter().enumerate() {
                for &b in &list[i + 1..] {
                    spread = spread.min(m.grid.distance(a, b));
                }
            }
            let weakest = list.iter().map(|&t| score(t)).fold(f64::INFINITY, f64::min);
            let quality = f64::from(spread) * 3.0 + weakest;
            if best.as_ref().is_none_or(|(q, _)| quality > *q) {
                best = Some((quality, list));
            }
            if trial >= 6 {
                break;
            }
        }
    }
    best.map(|(_, list)| list)
}

/// Places the city-states away from the civilizations (`_choose_cs_starts`): the best tiles six
/// from every civilization, six apart, then five, then four.
pub(crate) fn choose_cs_starts(
    m: &GenMap<'_>,
    scores: &[f64],
    n: usize,
    starts: &[TileIdx],
) -> Vec<TileIdx> {
    let mut cand: Vec<TileIdx> = start_candidates(m)
        .into_iter()
        .filter(|&t| starts.iter().all(|&s| m.grid.distance(t, s) >= 6))
        .collect();
    cand.sort_by(|&a, &b| scores[b.0 as usize].total_cmp(&scores[a.0 as usize]));
    let mut out: Vec<TileIdx> = Vec::new();
    for md in [6, 5, 4] {
        for &t in &cand {
            if out.len() >= n {
                break;
            }
            if out.iter().any(|&s| m.grid.distance(t, s) < md) {
                continue;
            }
            out.push(t);
        }
        if out.len() >= n {
            break;
        }
    }
    out.truncate(n);
    out
}

/// Adds start positions to a map that has too few, spread out from those it has
/// (`maps._fill_starts`, for `maps.prepare`): the best candidates without a wonder or an
/// improvement, `min_gap` apart and `avoid_gap` from `avoid`, the gap shrinking to 2.
pub(crate) fn fill_starts(
    m: &GenMap<'_>,
    have: &[TileIdx],
    n: usize,
    min_gap: u32,
    avoid: &[TileIdx],
    avoid_gap: u32,
) -> Vec<TileIdx> {
    let scores = start_scores(m);
    let mut out = have.to_vec();
    let mut cand: Vec<TileIdx> = start_candidates(m)
        .into_iter()
        .filter(|&t| m.tile(t).wonder().is_none() && m.tile(t).improvement().is_none())
        .collect();
    cand.sort_by(|&a, &b| scores[b.0 as usize].total_cmp(&scores[a.0 as usize]));
    for gap in (2..=min_gap).rev() {
        for &t in &cand {
            if out.len() >= n {
                return out;
            }
            if out.contains(&t) || out.iter().any(|&s| m.grid.distance(t, s) < gap) {
                continue;
            }
            if !avoid.is_empty()
                && avoid.iter().any(|&s| m.grid.distance(t, s) < avoid_gap.max(gap))
            {
                continue;
            }
            out.push(t);
        }
        if out.len() >= n {
            break;
        }
    }
    out
}

/// Start filling on a map from anywhere, an editor document included: `maps.prepare` fills the
/// starts a document lacks with it (`maps.py:334-341`; package 1c-09). A ruleset without land
/// or water terrain adds nothing.
#[must_use]
#[allow(clippy::too_many_arguments, reason = "maps._fill_starts' parameters, and the map's")]
pub fn fill_starts_on(
    rules: &Ruleset,
    grid: &HexGrid,
    tiles: &[Tile],
    have: &[TileIdx],
    n: usize,
    min_gap: u32,
    avoid: &[TileIdx],
    avoid_gap: u32,
) -> Vec<TileIdx> {
    let Some(kit) = Kit::new(rules) else { return have.to_vec() };
    let opts = MapOptions::default();
    let mut m = GenMap::new(&kit, &opts, grid.clone(), tiles.to_vec(), vec![false; tiles.len()]);
    m.assign_continents();
    fill_starts(&m, have, n, min_gap, avoid, avoid_gap)
}
