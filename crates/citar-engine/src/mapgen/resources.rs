//! Resources (`mapgen.py:1191-1647`: `_kind` to `_normalize_start`): strategic deposits,
//! luxuries round the starts and the city-states and scattered, bonus resources, the variety
//! every map keeps, the lobby's shares, and starts made playable.
//!
//! What differs from Python:
//! - the share of the luxury types a map should hold is a curve over the tile count alone
//!   (`LUXURY_VARIETY`), with the areas of the shipped lobby sizes, where Python read the
//!   areas of the ruleset's sizes by their keys (for the shipped sizes the two are the same);
//! - the variety top-ups draw from streams of their own, keyed apart from the rest, where
//!   Python seeded a side stream from the main one's state (`_side_rng`);
//! - the lobby's shares take no type's last deposit, which the top-ups after them put back as a
//!   fresh cluster, off the share (`mapgen-shares-keep-every-type`).

use super::map::{GenMap, only};
use super::spread::spread_out;
use crate::base::ids::{ResourceId, TerrainId, TileIdx};
use crate::base::num::{ceil_i64, floor_i64, round_half_even, trunc_i64};
use crate::base::rng::Rng;
use crate::base::sets::FeatureSet;
use crate::rules::defs::ResourceType;
use crate::rules::gen_tables::GenValue;
use crate::state::config::ResourceRule;

/// The share of the ruleset's luxury types a map of this many tiles should hold at least
/// (`LUXURY_VARIETY`, `luxury_variety`, `mapgen.py:1402-1420`): half on a duel or small map,
/// three quarters on a standard one, 90% on a large one and all of them from huge up, linear by
/// tile count between.
// refcheck: mapgen-luxury-variety-by-tile-count
#[must_use]
pub fn luxury_variety(tiles: u32) -> f64 {
    const POINTS: [(u32, f64); 6] = [
        (44 * 28, 0.5),
        (60 * 38, 0.5),
        (76 * 48, 0.75),
        (92 * 58, 0.9),
        (112 * 70, 1.0),
        (160 * 100, 1.0),
    ];
    if tiles <= POINTS[0].0 {
        return POINTS[0].1;
    }
    for pair in POINTS.windows(2) {
        let ((a0, v0), (a1, v1)) = (pair[0], pair[1]);
        if tiles <= a1 {
            return v0 + (v1 - v0) * f64::from(tiles - a0) / f64::from((a1 - a0).max(1));
        }
    }
    POINTS[POINTS.len() - 1].1
}

/// How many luxury types a map of this many tiles keeps, out of `types` that can go on it.
#[must_use]
pub fn luxury_types_wanted(tiles: u32, types: usize) -> usize {
    let want = ceil_i64(luxury_variety(tiles) * types as f64 - 1e-9);
    usize::try_from(want).unwrap_or(0).min(types)
}

fn kind(m: &GenMap<'_>, r: ResourceId) -> ResourceType {
    m.kit.r.resources()[r].kind
}

/// Whether the lobby's settings still let this resource be placed (`_allowed`): never when off
/// or its kind's density is 0, until its cap when capped. Shares are settled afterwards by
/// [`rebalance`], so here they count as normal.
pub(crate) fn allowed(m: &GenMap<'_>, r: ResourceId) -> bool {
    let k = kind(m, r);
    if m.opts.density(k) <= 0.0 {
        return false;
    }
    match m.opts.rule(k, r) {
        Some(ResourceRule::Off) => false,
        Some(ResourceRule::Cap(v)) => f64::from(m.placed[r]) < v.trunc(),
        _ => true,
    }
}

/// Whether the resource generates naturally on this tile, whatever is on it now and whatever
/// the lobby says (`_natural_on`, UnCiv's `TileResource.generatesNaturallyOn`).
pub(crate) fn natural_on(m: &GenMap<'_>, r: ResourceId, t: TileIdx) -> bool {
    let res = &m.kit.r.resources()[r];
    let g = m.kit.r.gen_tables();
    let gr = &g.resources[r];
    if m.tile(t).wonder().is_some() || !res.terrains_can_be_found_on.contains(&m.last(t)) {
        return false;
    }
    // `Doesn't generate naturally <in [Hill] tiles>`: only where its conditions hold.
    if gr.never || gr.not_where.iter().any(|c| m.holds(c, t)) {
        return false;
    }
    if m.all_terrains(t).any(|x| g.terrains[x].blocks_resources.iter().any(|c| m.holds(c, t))) {
        return false;
    }
    !m.grid.neighbors(t).any(|n| m.tile(n).wonder().is_some())
}

/// Whether the resource may be generated on this empty tile now (`_can_hold`).
pub(crate) fn can_hold(m: &GenMap<'_>, r: ResourceId, t: TileIdx) -> bool {
    let tile = m.tile(t);
    tile.resource().is_none() && tile.wonder().is_none() && allowed(m, r) && natural_on(m, r, t)
}

/// Whether a neighbour holds a resource.
fn resource_next_to(m: &GenMap<'_>, t: TileIdx) -> bool {
    m.grid.neighbors(t).any(|n| m.tile(n).resource().is_some())
}

/// Takes the resource off a tile, keeping the counts right (`_clear_resource`).
pub(crate) fn clear_resource(m: &mut GenMap<'_>, t: TileIdx) {
    if let Some(r) = m.tile(t).resource() {
        m.placed[r] = m.placed[r].saturating_sub(1);
    }
    let tile = m.tile(t).with_resource(None, 0);
    m.set(t, tile);
}

/// Puts a resource on a tile with the amount its definition calls for (`_set_resource`): a
/// strategic deposit takes `Deposits in [tiles] tiles always provide [n]` where it holds, else
/// its major or minor deposit (a coin toss when `major` is `None`).
pub(crate) fn set_resource(
    m: &mut GenMap<'_>,
    rng: &mut Rng,
    r: ResourceId,
    t: TileIdx,
    major: Option<bool>,
) {
    clear_resource(m, t);
    m.placed[r] += 1;
    let res = &m.kit.r.resources()[r];
    let mut amount = 0;
    if res.kind == ResourceType::Strategic {
        let fixed = m.kit.r.gen_tables().resources[r]
            .amounts
            .iter()
            .find(|a| m.matches(a.tiles, t))
            .map(|a| a.amount);
        amount = fixed.unwrap_or_else(|| {
            let major = major.unwrap_or_else(|| rng.unit() < 0.5);
            if major {
                res.major_deposit_amount.map_or(3, |d| d.default)
            } else {
                res.minor_deposit_amount.map_or(1, |d| d.default)
            }
        });
    }
    let amount = u8::try_from(amount.clamp(0, i32::from(u8::MAX))).unwrap_or(u8::MAX);
    let tile = m.tile(t).with_resource(Some(r), amount);
    m.set(t, tile);
}

/// A count scaled by the lobby's density for a kind, the fraction settled by a draw, so half
/// density on a count of one still places it half the time (`_scaled`).
pub(crate) fn scaled(m: &GenMap<'_>, rng: &mut Rng, count: f64, k: ResourceType) -> usize {
    let x = count * m.opts.density(k);
    let whole = trunc_i64(x);
    let frac = x - whole as f64;
    let extra = i64::from(rng.unit() < frac);
    usize::try_from(whole + extra).unwrap_or(0)
}

/// Which list of weights a pick reads.
#[derive(Clone, Copy)]
enum Weights {
    /// `Generated with weight [n]`: a major deposit.
    Major,
    /// `Minor deposits generated with weight [n]`.
    Minor,
}

/// A resource for this tile, weighted by how well each suits it (`_weighted`).
fn weighted(
    m: &GenMap<'_>,
    rng: &mut Rng,
    t: TileIdx,
    list: &[ResourceId],
    which: Weights,
) -> Option<ResourceId> {
    let g = m.kit.r.gen_tables();
    let mut opts = Vec::new();
    let mut weights = Vec::new();
    for &r in list {
        if !can_hold(m, r, t) {
            continue;
        }
        let values: &[GenValue] = match which {
            Weights::Major => &g.resources[r].weights,
            Weights::Minor => &g.resources[r].minor_weights,
        };
        for x in values {
            if m.holds(&x.cond, t) {
                opts.push(r);
                weights.push(f64::from(x.value));
            }
        }
    }
    rng.weighted(&weights).map(|i| opts[i])
}

/// Strategic deposits (`_strategic`): `Every [n] tiles with this terrain will receive a major
/// deposit` for each terrain, two tiles apart at normal density, then minor deposits spread
/// over three in a hundred land tiles.
pub(crate) fn strategic(m: &mut GenMap<'_>, rng: &mut Rng) {
    let density = m.opts.density(ResourceType::Strategic);
    if density <= 0.0 {
        return;
    }
    let kit = m.kit;
    let strat = &kit.strategic;
    // At high density the usual two-tile spacing between major deposits cannot fit them all.
    let spacing = if density <= 1.5 { 2 } else { 1 };
    let g = kit.r.gen_tables();
    for terrain in kit.r.terrains().ids() {
        for &freq in &g.terrains[terrain].major_deposits {
            let mut tiles: Vec<TileIdx> = m
                .all()
                .filter(|&t| {
                    m.tile(t).resource().is_none() && m.all_terrains(t).any(|x| x == terrain)
                })
                .collect();
            let count = scaled(
                m,
                rng,
                tiles.len() as f64 / f64::from(freq.max(1)),
                ResourceType::Strategic,
            );
            rng.shuffle(&mut tiles);
            let mut placed = 0;
            for t in tiles {
                if placed >= count {
                    break;
                }
                let Some(r) = weighted(m, rng, t, strat, Weights::Major) else { continue };
                if m.grid.any_within(t, spacing, |n| m.tile(n).resource().is_some()) {
                    continue;
                }
                set_resource(m, rng, r, t, Some(true));
                placed += 1;
            }
        }
    }
    let land: Vec<TileIdx> = m.all().filter(|&t| m.land(t) && !m.impassable(t)).collect();
    let minor: Vec<TileIdx> =
        land.iter().copied().filter(|&t| m.tile(t).resource().is_none()).collect();
    let count = scaled(m, rng, land.len() as f64 * 0.03, ResourceType::Strategic);
    for t in spread_out(m, rng, count, &minor) {
        if let Some(r) = weighted(m, rng, t, strat, Weights::Minor) {
            set_resource(m, rng, r, t, Some(false));
        }
    }
}

/// Bonus resources (`_bonus`): `Generated on every [n] tiles <in [filter] tiles>`, spread out,
/// none next to another resource. A frequency for a start region is skipped: there are none.
pub(crate) fn bonus(m: &mut GenMap<'_>, rng: &mut Rng) {
    if m.opts.density(ResourceType::Bonus) <= 0.0 {
        return;
    }
    let r = m.kit.r;
    let g = r.gen_tables();
    for (id, res) in r.resources().iter() {
        if res.kind != ResourceType::Bonus {
            continue;
        }
        for x in &g.resources[id].frequencies {
            if !x.cond.regions.is_empty() {
                continue;
            }
            let tiles: Vec<TileIdx> =
                m.all().filter(|&t| can_hold(m, id, t) && m.holds(&x.cond, t)).collect();
            let count =
                scaled(m, rng, tiles.len() as f64 / f64::from(x.value.max(1)), ResourceType::Bonus);
            for t in spread_out(m, rng, count, &tiles) {
                if !resource_next_to(m, t) && can_hold(m, id, t) {
                    set_resource(m, rng, id, t, None);
                }
            }
        }
    }
}

/// The luxuries a map places: those that generate naturally, less those only city-states make.
fn luxury_types(m: &GenMap<'_>) -> Vec<ResourceId> {
    m.kit.luxury.iter().copied().filter(|&r| !m.kit.city_state_only[r]).collect()
}

/// Luxuries (`_luxuries`): each civilization gets a regional luxury round its start, each
/// city-state one nearby, and more are scattered over two in a hundred land tiles.
pub(crate) fn luxuries(m: &mut GenMap<'_>, rng: &mut Rng, starts: &[TileIdx], cs: &[TileIdx]) {
    if m.opts.density(ResourceType::Luxury) <= 0.0 {
        return;
    }
    let mut lux = luxury_types(m);
    rng.shuffle(&mut lux);
    let mut free = lux.clone();
    for &s in starts {
        let area: Vec<TileIdx> = m.grid.within(s, 5).into_iter().filter(|&t| t != s).collect();
        let mut best = None;
        let mut best_n = 0;
        for &r in &free {
            let n = area.iter().filter(|&&t| can_hold(m, r, t)).count();
            if n > best_n {
                best = Some(r);
                best_n = n;
            }
        }
        let Some(best) = best else { continue };
        free.retain(|&r| r != best);
        let mut spots: Vec<TileIdx> =
            area.iter().copied().filter(|&t| can_hold(m, best, t)).collect();
        rng.shuffle(&mut spots);
        let base = (2 + rng.below(2)) as f64;
        let k = scaled(m, rng, base, ResourceType::Luxury);
        spots.sort_by_key(|&t| m.grid.distance(t, s));
        for t in spots.into_iter().take(k) {
            if can_hold(m, best, t) {
                set_resource(m, rng, best, t, None);
            }
        }
    }
    let g = m.kit.r.gen_tables();
    let near_cs: Vec<ResourceId> =
        lux.iter().copied().filter(|&r| g.resources[r].city_state_weight.is_some()).collect();
    for &c in cs {
        if scaled(m, rng, 1.0, ResourceType::Luxury) == 0 {
            continue;
        }
        let area: Vec<TileIdx> = m.grid.within(c, 3).into_iter().filter(|&t| t != c).collect();
        let pool = if near_cs.is_empty() { &lux } else { &near_cs };
        let opts: Vec<ResourceId> =
            pool.iter().copied().filter(|&r| area.iter().any(|&t| can_hold(m, r, t))).collect();
        let Some(&r) = rng.pick(&opts) else { continue };
        let spots: Vec<TileIdx> = area.iter().copied().filter(|&t| can_hold(m, r, t)).collect();
        if let Some(&t) = rng.pick(&spots) {
            set_resource(m, rng, r, t, None);
        }
    }
    let passable: Vec<TileIdx> = m.all().filter(|&t| !m.impassable(t)).collect();
    let random_lux = if free.is_empty() { lux.clone() } else { free };
    let land = passable.iter().filter(|&&t| m.land(t)).count();
    let count = scaled(m, rng, land as f64 * 0.02, ResourceType::Luxury);
    let spots: Vec<TileIdx> = passable
        .iter()
        .copied()
        .filter(|&t| {
            random_lux.iter().any(|&r| can_hold(m, r, t))
                && starts.iter().map(|&s| m.grid.distance(t, s)).min().unwrap_or(99) > 3
        })
        .collect();
    for t in spread_out(m, rng, count, &spots) {
        let opts: Vec<ResourceId> =
            random_lux.iter().copied().filter(|&r| can_hold(m, r, t)).collect();
        if opts.is_empty() || resource_next_to(m, t) {
            continue;
        }
        if let Some(&r) = rng.pick(&opts) {
            set_resource(m, rng, r, t, None);
        }
    }
}

/// A tile among the (shuffled) candidates with room round it for a cluster of this size, or the
/// one with the most (`_cluster_origin`).
fn cluster_origin(m: &GenMap<'_>, r: ResourceId, cands: &[TileIdx], size: usize) -> TileIdx {
    let mut best = cands[0];
    let mut best_n = None;
    for &t in cands.iter().take(200) {
        let n = m.grid.neighbors(t).filter(|&x| can_hold(m, r, x)).count();
        if n + 1 >= size {
            return t;
        }
        if best_n.is_none_or(|b| n > b) {
            best = t;
            best_n = Some(n);
        }
    }
    best
}

/// Puts a small cluster of a resource the map lacks where it can naturally go (`_guarantee`):
/// empty tiles away from `avoid` first, then any empty tile it may go on, then a tile held by
/// another resource of its kind that has more than one deposit. With `anywhere` (strategic
/// resources, which the game must never lack) the last resort is any empty passable tile of a
/// terrain it can be found on, though a feature or rule would normally keep it off, then any
/// passable land.
fn guarantee(
    m: &mut GenMap<'_>,
    rng: &mut Rng,
    r: ResourceId,
    avoid: &[TileIdx],
    size: usize,
    major: Option<bool>,
    anywhere: bool,
) -> bool {
    if !allowed(m, r) {
        return false;
    }
    let k = kind(m, r);
    let mut empty: Vec<TileIdx> = m.all().filter(|&t| can_hold(m, r, t)).collect();
    rng.shuffle(&mut empty);
    let far: Vec<TileIdx> = empty
        .iter()
        .copied()
        .filter(|&t| !resource_next_to(m, t) && avoid.iter().all(|&s| m.grid.distance(t, s) > 3))
        .collect();
    let cands = if far.is_empty() { &empty } else { &far };
    let origin = if cands.is_empty() {
        let mut spare: Vec<TileIdx> = m
            .all()
            .filter(|&t| {
                m.tile(t).resource().is_some_and(|x| {
                    x != r && kind(m, x) == k && m.placed[x] > 1 && natural_on(m, r, t)
                })
            })
            .collect();
        if spare.is_empty() && anywhere {
            let found_on = &m.kit.r.resources()[r].terrains_can_be_found_on;
            let free: Vec<TileIdx> = m
                .all()
                .filter(|&t| {
                    let tile = m.tile(t);
                    tile.resource().is_none() && tile.wonder().is_none() && !m.impassable(t)
                })
                .collect();
            let on = |f: &dyn Fn(TileIdx) -> bool| {
                free.iter().copied().filter(|&t| f(t)).collect::<Vec<_>>()
            };
            spare = on(&|t| found_on.contains(&m.last(t)));
            if spare.is_empty() {
                spare = on(&|t| found_on.contains(&m.tile(t).terrain()));
            }
            if spare.is_empty() {
                spare = on(&|t| m.land(t));
            }
        }
        match rng.pick(&spare) {
            Some(&t) => t,
            None => return false,
        }
    } else {
        cluster_origin(m, r, cands, size)
    };
    set_resource(m, rng, r, origin, major);
    let mut around: Vec<TileIdx> =
        m.grid.neighbors(origin).filter(|&n| can_hold(m, r, n)).collect();
    rng.shuffle(&mut around);
    for n in around.into_iter().take(size.saturating_sub(1)) {
        if can_hold(m, r, n) {
            set_resource(m, rng, r, n, major);
        }
    }
    true
}

/// Tops the map up to its share of the luxury types (`_luxury_variety`), in small clusters.
/// Only types that can go somewhere on this map count, and a type the lobby turned off stays
/// off.
pub(crate) fn luxury_variety_top_up(
    m: &mut GenMap<'_>,
    rng: &mut Rng,
    starts: &[TileIdx],
    cs: &[TileIdx],
) {
    if m.opts.density(ResourceType::Luxury) <= 0.0 {
        return;
    }
    let lux: Vec<ResourceId> = luxury_types(m)
        .into_iter()
        .filter(|&r| m.opts.rule(ResourceType::Luxury, r) != Some(ResourceRule::Off))
        .filter(|&r| m.placed[r] > 0 || m.all().any(|t| natural_on(m, r, t)))
        .collect();
    let want = luxury_types_wanted(m.grid.size(), lux.len());
    let mut missing: Vec<ResourceId> = lux.iter().copied().filter(|&r| m.placed[r] == 0).collect();
    let mut have = lux.len() - missing.len();
    if have >= want {
        return;
    }
    rng.shuffle(&mut missing);
    let avoid: Vec<TileIdx> = starts.iter().chain(cs).copied().collect();
    for r in missing {
        if have >= want {
            break;
        }
        let base = (2 + rng.below(2)) as f64;
        let size = scaled(m, rng, base, ResourceType::Luxury).max(1);
        if guarantee(m, rng, r, &avoid, size, None, false) {
            have += 1;
        }
    }
}

/// Every strategic type the lobby has not turned off is on the map at least once
/// (`_strategic_variety`).
pub(crate) fn strategic_variety(
    m: &mut GenMap<'_>,
    rng: &mut Rng,
    starts: &[TileIdx],
    cs: &[TileIdx],
) {
    if m.opts.density(ResourceType::Strategic) <= 0.0 {
        return;
    }
    let lacking: Vec<ResourceId> =
        m.kit.strategic.iter().copied().filter(|&r| m.placed[r] == 0).collect();
    let avoid: Vec<TileIdx> = starts.iter().chain(cs).copied().collect();
    for r in lacking {
        guarantee(m, rng, r, &avoid, 1, Some(true), true);
    }
}

/// Makes each `share` resource the lobby's percentage of every resource of its kind
/// (`_rebalance`).
///
/// The kind's total stays what the density made; only the mix changes. A resource short of its
/// share first takes over tiles of normal resources of its kind where its terrain allows, then
/// goes on fresh tiles, each paid for by removing a normal one elsewhere. One over its share
/// hands the surplus back to normal resources, or clears it if none fit. Tiles near a start
/// change last, so the starts stay as balanced as they were, and no normal resource loses its
/// last deposit, which Python took. Shares above 100% in all, or with no normal resource left
/// to make up the rest, are scaled to exactly 100%.
pub(crate) fn rebalance(m: &mut GenMap<'_>, rng: &mut Rng, k: ResourceType, starts: &[TileIdx]) {
    let shares: Vec<(ResourceId, f64)> = m
        .opts
        .rules_of(k)
        .filter_map(|(r, rule)| match rule {
            ResourceRule::Share(v) => Some((r, v)),
            _ => None,
        })
        .collect();
    if shares.is_empty() {
        return;
    }
    let total = m.all().filter(|&t| m.tile(t).resource().is_some_and(|x| kind(m, x) == k)).count();
    if total == 0 {
        return;
    }
    let g = m.kit.r.gen_tables();
    let normal: Vec<ResourceId> = m
        .kit
        .r
        .resources()
        .iter()
        .filter(|(id, x)| {
            x.kind == k
                && !shares.iter().any(|&(s, _)| s == *id)
                && !g.resources[*id].never
                && allowed(m, *id)
                && !m.kit.city_state_only[*id]
        })
        .map(|(id, _)| id)
        .collect();
    let sum: f64 = shares.iter().map(|&(_, v)| v).sum();
    let scale = if sum > 100.0 || (normal.is_empty() && sum > 0.0) { 100.0 / sum } else { 1.0 };
    let want: Vec<(ResourceId, u32)> = shares
        .iter()
        .map(|&(r, v)| {
            let n = floor_i64(round_half_even(total as f64 * v * scale / 100.0));
            (r, u32::try_from(n.max(0)).unwrap_or(0))
        })
        .collect();
    let remoteness = |m: &GenMap<'_>, t: TileIdx| {
        starts.iter().map(|&s| m.grid.distance(t, s)).min().unwrap_or(99)
    };
    let holding = |m: &GenMap<'_>, f: &dyn Fn(ResourceId) -> bool| -> Vec<TileIdx> {
        let mut v: Vec<TileIdx> =
            m.all().filter(|&t| m.tile(t).resource().is_some_and(f)).collect();
        v.sort_by_key(|&t| core::cmp::Reverse(remoteness(m, t)));
        v
    };
    let is_normal = |x: ResourceId| normal.contains(&x);
    for &(r, n) in &want {
        let surplus = m.placed[r].saturating_sub(n) as usize;
        for t in holding(m, &|x| x == r).into_iter().take(surplus) {
            clear_resource(m, t);
            let opts: Vec<ResourceId> =
                normal.iter().copied().filter(|&x| can_hold(m, x, t)).collect();
            if let Some(&x) = rng.pick(&opts) {
                set_resource(m, rng, x, t, None);
            }
        }
    }
    // A normal resource keeps its last deposit: taking it would lose the type, which the
    // variety top-ups after this put back as a fresh cluster, so the shares came out diluted.
    // refcheck: mapgen-shares-keep-every-type
    let spare = |m: &GenMap<'_>, t: TileIdx| m.tile(t).resource().is_some_and(|x| m.placed[x] > 1);
    for &(r, n) in &want {
        if m.placed[r] >= n {
            continue;
        }
        let swap: Vec<TileIdx> =
            holding(m, &is_normal).into_iter().filter(|&t| natural_on(m, r, t)).collect();
        for t in swap {
            if m.placed[r] >= n {
                break;
            }
            if spare(m, t) {
                set_resource(m, rng, r, t, None);
            }
        }
        let need = n.saturating_sub(m.placed[r]) as usize;
        if need == 0 {
            continue;
        }
        let empty: Vec<TileIdx> =
            m.all().filter(|&t| can_hold(m, r, t) && !resource_next_to(m, t)).collect();
        let mut victims = holding(m, &is_normal).into_iter();
        for t in spread_out(m, rng, need, &empty) {
            set_resource(m, rng, r, t, None);
            if let Some(v) = victims.by_ref().find(|&v| spare(m, v)) {
                clear_resource(m, v);
            }
        }
    }
}

/// Keeps a start playable (`_normalize_start`, after UnCiv's `MapRegions.normalizeStart`): no
/// mountains, snow or ice next door, two food bonuses near, horses and iron within reach, and
/// some rough ground for production.
pub(crate) fn normalize_start(m: &mut GenMap<'_>, rng: &mut Rng, s: TileIdx) {
    let kit = m.kit;
    let names = kit.names;
    let hill = only(kit.hill);
    let start = m.tile(s);
    let kept = if start.features().contains(kit.hill) { hill } else { FeatureSet::EMPTY };
    m.set_features(s, kept);
    clear_resource(m, s);
    let bare = m.tile(s).with_improvement(None);
    m.set(s, bare);

    let ring: Vec<TileIdx> = m.grid.within(s, 1).into_iter().filter(|&t| t != s).collect();
    for n in ring {
        if m.tile(n).wonder().is_some() {
            continue;
        }
        if m.mountain(n) {
            m.set_terrain(n, names.plains.unwrap_or(kit.land));
            m.set_features(n, hill);
        }
        if m.is(n, names.snow)
            && let Some(tundra) = names.tundra
        {
            m.set_terrain(n, tundra);
        }
        if let Some(ice) = names.ice.and_then(|i| kit.r.terrains()[i].feature) {
            let mut f = m.tile(n).features();
            if f.remove(ice) {
                m.set_features(n, f);
            }
        }
    }

    let near = |m: &GenMap<'_>, radius: u32| -> Vec<TileIdx> {
        m.grid.within(s, radius).into_iter().filter(|&t| t != s).collect()
    };
    let count_food = |m: &GenMap<'_>| {
        near(m, 2)
            .into_iter()
            .filter(|&t| {
                m.tile(t).resource().is_some_and(|r| {
                    kind(m, r) == ResourceType::Bonus && kit.food_bonus.contains(&r)
                })
            })
            .count()
    };
    // Adds one of `options` near the start, at `min_r` to `radius` from it.
    let add = |m: &mut GenMap<'_>,
               rng: &mut Rng,
               options: &[ResourceId],
               radius: u32,
               min_r: u32,
               major: Option<bool>| {
        let mut spots: Vec<TileIdx> = m
            .grid
            .within(s, radius)
            .into_iter()
            .filter(|&t| m.grid.distance(s, t) >= min_r)
            .collect();
        rng.shuffle(&mut spots);
        let mut opts = options.to_vec();
        rng.shuffle(&mut opts);
        for r in opts {
            if let Some(&t) = spots.iter().find(|&&t| can_hold(m, r, t)) {
                set_resource(m, rng, r, t, major);
                return true;
            }
        }
        false
    };
    let reps = if m.opts.density(ResourceType::Bonus) > 0.0 {
        2usize.saturating_sub(count_food(m))
    } else {
        0
    };
    for _ in 0..reps {
        if add(m, rng, &kit.food_bonus, 2, 1, None) {
            continue;
        }
        // No food bonus fits: make a plain tile grassland, with cattle if they may go there.
        let Some(grass) = names.grassland else { continue };
        let flats: [Option<TerrainId>; 3] = [names.plains, names.desert, names.tundra];
        for n in near(m, 2) {
            let tile = m.tile(n);
            if flats.contains(&Some(tile.terrain()))
                && tile.features().is_empty()
                && tile.resource().is_none()
                && tile.wonder().is_none()
            {
                m.set_terrain(n, grass);
                if let Some(cattle) = names.cattle
                    && can_hold(m, cattle, n)
                {
                    set_resource(m, rng, cattle, n, None);
                }
                break;
            }
        }
    }
    for strat in [names.horses, names.iron].into_iter().flatten() {
        if !m.grid.any_within(s, 5, |n| m.tile(n).resource() == Some(strat)) {
            add(m, rng, &[strat], 5, 2, Some(false));
        }
    }
    let forest = names.forest;
    let mut rough =
        near(m, 2).into_iter().filter(|&n| m.hill(n) || m.has_feature(n, forest)).count();
    let hill_on = &kit.r.terrains()[kit.hill_terrain].occurs_on;
    for n in near(m, 2) {
        if rough >= 2 {
            break;
        }
        let tile = m.tile(n);
        if m.land(n)
            && !m.impassable(n)
            && tile.features().is_empty()
            && tile.resource().is_none()
            && tile.wonder().is_none()
            && hill_on.contains(&tile.terrain())
        {
            m.set_features(n, hill);
            rough += 1;
        }
    }
}
