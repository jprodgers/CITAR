//! Terrain on the land's shape: climate, mountains and hills, lakes and coasts, vegetation, rare
//! features, the polar ice and the conversions near rivers (UnCiv's `MapGenerator` steps,
//! `mapgen.py:546-718, 883-899`).
//!
//! Mountains and hills are ranked from an elevation noise and cut at quantiles, where UnCiv cut
//! Perlin noise at fixed heights, so every map has the same share of them.

use super::map::{GenMap, only};
use super::noise::noise;
use crate::base::ids::{TerrainId, TileIdx};
use crate::base::num::trunc_i64;
use crate::base::rng::Rng;
use crate::base::sets::FeatureSet;
use crate::rules::defs::TerrainType;
use crate::rules::gen_tables::Near;

/// Climate: each tile's humidity and temperature, and the land terrain its climate suits
/// (`MapGenerator.applyHumidityAndTemperature`, `_humidity_and_temperature`).
pub(crate) fn humidity_and_temperature(m: &mut GenMap<'_>, rng: &mut Rng) {
    let hum = noise(rng, &m.grid, 6.0, 1);
    let tmp = noise(rng, &m.grid, 6.0, 1);
    let intensity = 0.6;
    let kit = m.kit;
    let mut ok: Vec<TerrainId> = Vec::new();
    for t in m.all() {
        let i = t.0 as usize;
        m.humid[i] = ((hum[i] + 1.0) / 2.0).clamp(0.0, 1.0);
        let expected = 1.0 - 2.0 * m.lat[i];
        let v = (5.0 * expected + tmp[i]) / 6.0;
        let v = crate::base::num::pow(v.abs(), 1.0 - intensity) * if v >= 0.0 { 1.0 } else { -1.0 };
        m.temp[i] = v.clamp(-1.0, 1.0);
        if m.land(t) {
            ok.clear();
            ok.extend(
                kit.climate_lands
                    .iter()
                    .copied()
                    .filter(|&x| kit.has_climate(x) && m.climate_ok(x, t)),
            );
            let from = if ok.is_empty() { &kit.climate_lands } else { &ok };
            if let Some(&x) = rng.pick(from) {
                m.set_terrain(t, x);
            }
        }
    }
}

/// The flat terrain that belongs at this tile's latitude (`_flat_for`); `None` if the ruleset has
/// no flat land with a climate.
fn flat_for(m: &GenMap<'_>, rng: &mut Rng, t: TileIdx) -> Option<TerrainId> {
    let flats = &m.kit.flats;
    let ok: Vec<TerrainId> = flats.iter().copied().filter(|&x| m.climate_ok(x, t)).collect();
    rng.pick(if ok.is_empty() { flats } else { &ok }).copied()
}

/// Mountains and hills (`MapElevationGenerator.raiseMountainsAndHills`, `_mountains_and_hills`):
/// the highest 6% of the land by an elevation noise become mountains and the next 14% hills;
/// then five rounds pull lone mountains down and grow chains, and five more do the same for
/// hills round them.
#[allow(clippy::too_many_lines, reason = "the two cellular passes, as Python wrote them")]
pub(crate) fn mountains_and_hills(m: &mut GenMap<'_>, rng: &mut Rng) {
    let elev = noise(rng, &m.grid, 2.5, 4);
    let land: Vec<TileIdx> = m.all().filter(|&t| m.land(t)).collect();
    if land.is_empty() {
        return;
    }
    let mut ranked = land.clone();
    ranked.sort_by(|a, b| elev[b.0 as usize].total_cmp(&elev[a.0 as usize]));
    let n = land.len() as f64;
    let n_mtn = usize::try_from(trunc_i64(n * 0.06)).unwrap_or(0);
    let n_hill = usize::try_from(trunc_i64(n * 0.14)).unwrap_or(0);
    let mountain = m.kit.names.mountain;
    let hill = only(m.kit.hill);
    let mut rank = vec![usize::MAX; m.tiles.len()];
    for (k, &t) in ranked.iter().enumerate() {
        rank[t.0 as usize] = k;
    }
    for &t in &land {
        let k = rank[t.0 as usize];
        if k < n_mtn {
            if let Some(mt) = mountain {
                m.set_terrain(t, mt);
                m.set_features(t, FeatureSet::EMPTY);
            }
        } else if k < n_mtn + n_hill {
            m.set_features(t, hill);
        }
    }

    let count_mountains = |m: &GenMap<'_>| land.iter().filter(|&&t| m.mountain(t)).count();
    if mountain.is_some() {
        let target = count_mountains(m) * 2;
        for _ in 0..5 {
            let mut total = count_mountains(m) * 2;
            let (mut rise, mut lower) = (Vec::new(), Vec::new());
            for &t in &land {
                let is_m = m.mountain(t);
                let adj = m.grid.neighbors(t).filter(|&x| m.mountain(x)).count();
                match adj {
                    0 => {
                        if is_m && rng.below(4) == 0 {
                            lower.push(t);
                        }
                    }
                    1 => {
                        if !is_m && rng.below(10) == 0 {
                            rise.push(t);
                        }
                    }
                    3 => {
                        if is_m && rng.below(2) == 0 {
                            lower.push(t);
                        }
                    }
                    a if is_m && a > 3 => lower.push(t),
                    _ => {}
                }
            }
            for &t in &rise {
                if total >= target {
                    break;
                }
                total += 1;
                if let Some(mt) = mountain {
                    m.set_terrain(t, mt);
                    m.set_features(t, FeatureSet::EMPTY);
                }
            }
            for &t in &lower {
                if total * 2 <= target {
                    break;
                }
                total -= 1;
                if let Some(flat) = flat_for(m, rng, t) {
                    m.set_terrain(t, flat);
                }
            }
        }
    }

    let count_hills = |m: &GenMap<'_>| land.iter().filter(|&&t| m.hill(t)).count();
    let target = count_hills(m) as f64;
    for it in 1..=5 {
        let mut total = count_hills(m) as f64;
        let (mut rise, mut lower) = (Vec::new(), Vec::new());
        for &t in &land {
            if m.mountain(t) {
                continue;
            }
            let is_h = m.hill(t);
            let am = m.grid.neighbors(t).filter(|&x| m.mountain(x)).count();
            let ah = m.grid.neighbors(t).filter(|&x| m.hill(x)).count();
            // A lone hill or one crowded by hills, on no mountain's flank, may go.
            if (ah <= 1 || ah > 3) && am == 0 && is_h && rng.below(2) == 0 {
                lower.push(t);
            } else if (2..=3).contains(&(ah + am)) && !is_h && rng.below(2) == 0 {
                rise.push(t);
            }
        }
        for &t in &rise {
            if total > target && it != 1 {
                continue;
            }
            total += 1.0;
            m.set_features(t, hill);
        }
        for &t in &lower {
            if total >= target * 0.9 || it == 1 {
                total -= 1.0;
                m.set_features(t, FeatureSet::EMPTY);
            }
        }
    }
}

/// Lakes and coasts (`MapGenerator.spawnLakesAndCoasts`, `_lakes_and_coasts`): water bodies of
/// ten tiles or fewer become lakes, the rest ocean, and three rounds spread coast from the land,
/// each ocean tile next to land always and one next to coast half the time.
pub(crate) fn lakes_and_coasts(m: &mut GenMap<'_>, rng: &mut Rng) {
    let kit = m.kit;
    let water_mask: Vec<bool> = m.all().map(|t| m.water(t)).collect();
    let mut lake = vec![false; water_mask.len()];
    if kit.names.lakes.is_some() {
        for comp in super::continents::components(&m.grid, &water_mask) {
            if comp.len() <= 10 {
                for t in comp {
                    lake[t.0 as usize] = true;
                }
            }
        }
    }
    for t in m.all() {
        if !water_mask[t.0 as usize] {
            continue;
        }
        match (lake[t.0 as usize], kit.names.lakes) {
            (true, Some(l)) => m.set_terrain(t, l),
            _ => m.set_terrain(t, kit.ocean),
        }
    }
    let Some(coast) = kit.names.coast else { return };
    let mut to_coast = Vec::new();
    for _ in 0..3 {
        to_coast.clear();
        for t in m.all() {
            if m.tile(t).terrain() != kit.ocean {
                continue;
            }
            for n in m.grid.neighbors(t) {
                if m.land(n) {
                    to_coast.push(t);
                    break;
                }
                if m.tile(n).terrain() == coast {
                    if rng.unit() < 0.5 {
                        to_coast.push(t);
                    }
                    break;
                }
            }
        }
        for &t in &to_coast {
            m.set_terrain(t, coast);
        }
    }
}

/// Forest and jungle (`MapGenerator.spawnVegetation`, `_vegetation`): where a smooth noise is
/// low, a vegetation feature that may lie on the tile, jungle where it is warm and forest where
/// it is cool.
pub(crate) fn vegetation(m: &mut GenMap<'_>, rng: &mut Rng) {
    let veg = noise(rng, &m.grid, 3.0, 1);
    let kit = m.kit;
    let terrains = kit.r.terrains();
    let bases: crate::base::sets::TerrainSet =
        kit.vegetation.iter().flat_map(|&f| terrains[f].occurs_on.iter().copied()).collect();
    let (forest, jungle) = (kit.names.forest, kit.names.jungle);
    let mut options: Vec<TerrainId> = Vec::new();
    for t in m.all() {
        let i = t.0 as usize;
        let last = m.last(t);
        if !bases.contains(m.tile(t).terrain()) || !bases.contains(last) {
            continue;
        }
        if (veg[i] + 1.0) / 2.0 > 0.4 {
            continue;
        }
        options.clear();
        options.extend(kit.vegetation.iter().copied().filter(|&f| {
            terrains[f].occurs_on.contains(&last) && m.climate_ok(f, t) && m.may_generate(f, t)
        }));
        if let (Some(fo), Some(ju)) = (forest, jungle)
            && options.contains(&fo)
            && options.contains(&ju)
        {
            if m.temp[i] > 0.45 {
                options.clear();
                options.push(ju);
            } else if m.temp[i] < 0.15 {
                options.clear();
                options.push(fo);
            }
        }
        if let Some(&f) = rng.pick(&options) {
            add_feature(m, t, f);
        }
    }
}

/// Puts a feature on a tile, over those it has.
pub(crate) fn add_feature(m: &mut GenMap<'_>, t: TileIdx, f: TerrainId) {
    if let Some(id) = m.kit.r.terrains()[f].feature {
        let mut set = m.tile(t).features();
        set.insert(id);
        m.set_features(t, set);
    }
}

/// Rare features (`MapGenerator.spawnRareFeatures`, `_rare_features`): one tile in twenty
/// without features gets one that may lie on its base.
pub(crate) fn rare_features(m: &mut GenMap<'_>, rng: &mut Rng) {
    let kit = m.kit;
    let terrains = kit.r.terrains();
    let mut options: Vec<TerrainId> = Vec::new();
    for t in m.all() {
        if !m.tile(t).features().is_empty() || rng.unit() > 0.05 {
            continue;
        }
        let base = m.tile(t).terrain();
        options.clear();
        options.extend(kit.rare.iter().copied().filter(|&f| {
            terrains[f].occurs_on.contains(&base) && m.climate_ok(f, t) && m.may_generate(f, t)
        }));
        if let Some(&f) = rng.pick(&options) {
            add_feature(m, t, f);
        }
    }
}

/// Freezes the polar band (`_ice`): ice on each of its tiles whose base the ice may lie on. There
/// is no other sea ice.
pub(crate) fn ice(m: &mut GenMap<'_>) {
    let kit = m.kit;
    let Some(ice) = kit.names.ice else { return };
    let Some(id) = kit.r.terrains()[ice].feature else { return };
    let occurs = &kit.r.terrains()[ice].occurs_on;
    let default: Vec<TerrainId> =
        [kit.names.ocean, kit.names.coast].into_iter().flatten().collect();
    let on: &[TerrainId] = if occurs.is_empty() { &default } else { occurs };
    for t in m.all() {
        if m.ice[t.0 as usize] && on.contains(&m.tile(t).terrain()) {
            m.set_features(t, only(id));
        }
    }
}

/// `Becomes [terrain] when adjacent to [filter]` (`Helpers.convertTerrains`, `_convert_terrains`):
/// tundra by a river turns to plains, desert by a river gains flood plains. A base terrain is
/// replaced; a feature is added where it may lie on the top terrain. The first change that
/// applies is the only one.
pub(crate) fn convert_terrains(m: &mut GenMap<'_>) {
    let kit = m.kit;
    let g = kit.r.gen_tables();
    let terrains = kit.r.terrains();
    for t in m.all() {
        let base = m.tile(t).terrain();
        for c in &g.terrains[base].changes {
            let near = match c.near {
                Near::River => m.tile(t).has_river(),
                Near::Tiles(f) => m.grid.neighbors(t).any(|n| m.matches(f, n)),
            };
            if !near {
                continue;
            }
            let into = &terrains[c.into];
            match into.kind {
                TerrainType::Land | TerrainType::Water => m.set_terrain(t, c.into),
                TerrainType::TerrainFeature if into.occurs_on.contains(&m.last(t)) => {
                    add_feature(m, t, c.into);
                }
                // A natural wonder is placed by its own step, never by a conversion.
                _ => {}
            }
            break;
        }
    }
}
