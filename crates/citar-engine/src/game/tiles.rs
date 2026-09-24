//! Tiles: what a tile is (water, rough, next to the coast), and what it yields (`tiles.py:1-391`).
//!
//! A tile's yields are its terrain's and its features', a river's, its resource's where the
//! viewer can see it, its improvement's and its route's, and the modifiers a city applies to the
//! tiles it works (`tiles._tile_stats`, `tiles.py:265-349`). Python recomputed them on every call
//! and threw them away on every write, 43 µs each; here they are memos (`derive::tile`): one per
//! tile for its owner as its own city works it, and a table for any other viewer or city.
//!
//! A city's tile modifiers (`StatsFromTiles`, `StatsFromObject`, `StatsFromTilesWithout`,
//! `StatPercentFromObject`, `AllStatsPercentFromObject`) are gathered once per city, with every
//! conditional that does not read the tile evaluated there ([`CityMods`], DESIGN.md 6.11), so a
//! tile evaluates only its own filters and the conditionals that read it.
//!
//! Every function that evaluates uniques adds the classes of what it read (their conditionals'
//! and their filters') to a [`CondDeps`], which the memo that keeps the answer validates against.

use super::Game;
use super::eval::EvalView;
use crate::base::ids::{
    CityId, ImprovementId, ObjectFilterId, PlayerId, ResourceId, StatsId, TerrainId, TileFilterId,
    TileIdx, UniqueId,
};
use crate::base::stats::{Stat, StatMask, Stats};
use crate::rules::defs::Route;
use crate::unique::{CondDeps, Ctx, FilterFacts, TileLeaf, UniqueData, UniqueType, applies, uq};

/// The yields a city's own tile has at least (`tiles.CITY_CENTER_MIN`, `tiles.py:13`).
const CITY_CENTER_MIN: [(Stat, f64); 2] = [(Stat::Food, 2.0), (Stat::Production, 1.0)];

// ---- What a tile is (tiles.py:36-189) ------------------------------------------------------------

/// Every terrain on a tile, in Python's order: its base terrain, its natural wonder, then its
/// features lowest layer first (`tiles.all_terrains`, `tiles.py:36-43`).
pub fn all_terrains(g: &Game, t: TileIdx) -> impl Iterator<Item = TerrainId> + '_ {
    let tile = g.tile(t);
    let features = &g.rules().derived().features;
    let base = tile.map(crate::state::map::Tile::terrain);
    let wonder = tile.and_then(crate::state::map::Tile::wonder);
    let feats = tile.map(|x| x.features()).unwrap_or_default();
    base.into_iter().chain(wonder).chain(feats.iter().filter_map(move |f| features.get(f).copied()))
}

/// The terrain that governs a tile: its top feature, else its natural wonder, else its base
/// (`tiles.last_terrain`, `tiles.py:46-52`).
#[must_use]
pub fn last_terrain(g: &Game, t: TileIdx) -> Option<TerrainId> {
    let tile = g.tile(t)?;
    if let Some(f) = tile.features().top() {
        return g.rules().derived().features.get(f).copied();
    }
    Some(tile.wonder().unwrap_or_else(|| tile.terrain()))
}

/// Whether nothing can enter a tile (`tiles.is_impassable`, `tiles.py:75-77`).
#[must_use]
pub fn is_impassable(g: &Game, t: TileIdx) -> bool {
    last_terrain(g, t).is_some_and(|x| g.rules().terrains()[x].impassable)
}

/// Whether a tile's terrain is the ruleset's Coast (`tiles.adjacent_to_coast` compared the name,
/// `tiles.py:127-134`).
fn is_coast(g: &Game, t: TileIdx) -> bool {
    let coast = g.rules().derived().known.map.coast;
    coast.is_some() && g.tile(t).map(crate::state::map::Tile::terrain) == coast
}

/// Whether a neighbour of a tile is coast (`tiles.adjacent_to_coast`, `tiles.py:127-134`).
#[must_use]
pub fn adjacent_to_coast(g: &Game, t: TileIdx) -> bool {
    g.grid().neighbors(t).any(|n| is_coast(g, n))
}

/// Whether a tile is land next to the coast (`tiles.is_coastal_land`, `tiles.py:137-140`).
#[must_use]
pub fn is_coastal_land(g: &Game, t: TileIdx) -> bool {
    g.is_land(t) && adjacent_to_coast(g, t)
}

/// A tile's improvement, unless it is pillaged (`tiles.unpillaged_improvement`,
/// `tiles.py:173-180`).
#[must_use]
pub fn unpillaged_improvement(g: &Game, t: TileIdx) -> Option<ImprovementId> {
    g.tile(t).filter(|x| !x.improvement_pillaged()).and_then(crate::state::map::Tile::improvement)
}

/// A tile's route as the improvement that builds it, unless it is pillaged
/// (`tiles.unpillaged_route`, `tiles.py:183-185`).
#[must_use]
pub fn unpillaged_route(g: &Game, t: TileIdx) -> Option<ImprovementId> {
    let tile = g.tile(t).filter(|x| !x.route_pillaged())?;
    let known = &g.rules().derived().known;
    Some(match tile.route()? {
        Route::Road => known.road,
        Route::Railroad => known.railroad,
    })
}

/// Whether an improvement is one that makes a resource available (`tiles.resource_improved_by`,
/// `tiles.py:188-193`).
#[must_use]
pub fn resource_improved_by(g: &Game, res: ResourceId, imp: ImprovementId) -> bool {
    let rd = &g.rules().resources()[res];
    rd.improvement == Some(imp) || rd.improved_by.contains(&imp)
}

// ---- A city's tile modifiers (CityMods, DESIGN.md 6.11) -------------------------------------------

/// What a tile modifier's filter names (`add_stats`' and `addp`'s `f`, `tiles.py:291-301,
/// 355-363`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// `[stats] from [tileFilter] tiles [cityFilter]`.
    Tiles(TileFilterId),
    /// `from every [tileFilter/specialist/buildingFilter]`, `from every [tileFilter/buildingFilter]`.
    Object(ObjectFilterId),
    /// `[stats] from [tileFilter] tiles without [tileFilter] [cityFilter]`: the second filter
    /// must not match.
    Without { tiles: TileFilterId, without: TileFilterId },
}

/// What a tile modifier adds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModKind {
    /// Flat stats.
    Flat(StatsId),
    /// A percentage of one stat, or of every stat for `None`.
    Percent(Option<Stat>, i32),
}

/// One of a city's tile modifiers, as the city's uniques give it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileMod {
    pub id: UniqueId,
    /// Its copies.
    pub n: u16,
    pub kind: ModKind,
    pub target: Target,
    /// Its conditionals read the tile, so they are evaluated tile by tile.
    pub per_tile: bool,
    /// What evaluating it on a tile reads: its filters', and its conditionals' where they are
    /// evaluated per tile.
    pub deps: CondDeps,
}

/// A city's tile modifiers, prefiltered (DESIGN.md 6.11): each unique of the five types that
/// holds for the city, its city filter tested, its conditionals evaluated unless they read the
/// tile, in the order Python read them.
#[derive(Clone, Debug, PartialEq)]
pub struct CityMods {
    pub mods: Vec<TileMod>,
    /// What gathering them read: the conditionals evaluated here and the city filters.
    pub deps: CondDeps,
}

impl Default for CityMods {
    fn default() -> Self {
        Self { mods: Vec::new(), deps: CondDeps::empty() }
    }
}

impl super::derive::rev::BitEq for CityMods {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// What evaluating a tile filter on the tile in context reads: its leaves' classes, and the tile's
/// territory city's worked tiles for `worked` (`TILE`).
#[must_use]
pub fn tile_filter_deps(g: &Game, f: TileFilterId) -> CondDeps {
    let e = &g.rules().uniques().filters().tile(f).full;
    let mut d = e.deps();
    if e.leaves().iter().any(|l| matches!(l, TileLeaf::Worked)) {
        d |= CondDeps::TILE;
    }
    d
}

/// What a tile filter asked of a tile's neighbours reads (`[stats] for each adjacent [tileFilter]`,
/// `tiles.py:241-248`). A memo of a tile's yield reads its neighbours' own facts already: their
/// terrain, river, resource and improvement. A filter that reads more of a neighbour (its owner,
/// whether a city works it, fresh water or the coast beside it, which are two tiles away) reads
/// the map (`MAP`), as do the local classes of its leaves, which are about the neighbour and not
/// the tile in context.
#[must_use]
pub fn adjacency_deps(f: &crate::unique::TileFilter) -> CondDeps {
    let e = &f.full;
    let own = |l: &TileLeaf| {
        matches!(
            l,
            TileLeaf::Terrains(_)
                | TileLeaf::River
                | TileLeaf::AnyResource
                | TileLeaf::Resource(_)
                | TileLeaf::Unimproved
                | TileLeaf::Improved
                | TileLeaf::Pillaged
                | TileLeaf::Improvement(_)
        )
    };
    let d = e.deps();
    if e.leaves().into_iter().all(own) && !d.intersects(CondDeps::LOCAL) {
        d
    } else {
        d.difference(CondDeps::LOCAL) | CondDeps::MAP
    }
}

fn target_deps(g: &Game, t: Target) -> CondDeps {
    match t {
        Target::Tiles(f) => tile_filter_deps(g, f),
        Target::Object(o) => g
            .rules()
            .uniques()
            .object(o)
            .tiles
            .map_or(CondDeps::empty(), |f| tile_filter_deps(g, f)),
        Target::Without { tiles, without } => {
            tile_filter_deps(g, tiles) | tile_filter_deps(g, without)
        }
    }
}

/// City `c`'s tile modifiers (`cities.city_uniques` of the five types, as `_tile_stats` and
/// `_tile_percentages` read them, `tiles.py:291-311, 352-373`).
#[must_use]
pub fn city_mods(g: &Game, c: CityId) -> CityMods {
    let v = g.view();
    let mut out = CityMods::default();
    let Some(city) = g.city(c) else { return out };
    let t = g.rules().uniques();
    let filters = t.filters();
    let ctx =
        Ctx { civ: Some(city.owner()), city: Some(c), tile: Some(city.tile()), ..Ctx::default() };
    let types = [
        UniqueType::StatsFromTiles,
        UniqueType::StatsFromObject,
        UniqueType::StatsFromTilesWithout,
        UniqueType::StatPercentFromObject,
        UniqueType::AllStatsPercentFromObject,
    ];
    for ty in types {
        for h in uq::city(&v, c, ty, &Ctx::IGNORE) {
            let (kind, target, city_filter) = match *h.data() {
                UniqueData::StatsFromTiles(x) => {
                    (ModKind::Flat(x.stats), Target::Tiles(x.tiles), Some(x.cities))
                }
                UniqueData::StatsFromObject(x) => {
                    (ModKind::Flat(x.stats), Target::Object(x.object), None)
                }
                UniqueData::StatsFromTilesWithout(x) => (
                    ModKind::Flat(x.stats),
                    Target::Without { tiles: x.tiles, without: x.without },
                    Some(x.cities),
                ),
                UniqueData::StatPercentFromObject(x) => {
                    (ModKind::Percent(Some(x.stat), x.percent), Target::Object(x.object), None)
                }
                UniqueData::AllStatsPercentFromObject(x) => {
                    (ModKind::Percent(None, x.percent), Target::Object(x.object), None)
                }
                _ => continue,
            };
            let conds = h.unique.deps();
            let per_tile = conds.intersects(CondDeps::TILE);
            if !per_tile {
                out.deps |= conds;
                if !applies(h.id, &ctx, &v) {
                    continue;
                }
            }
            if let Some(cf) = city_filter {
                out.deps |= filters.city(cf).deps();
                if !filters.city_matches(cf, &v, c, None) {
                    continue;
                }
            }
            let mut deps = target_deps(g, target);
            if per_tile {
                deps |= conds;
            }
            out.mods.push(TileMod { id: h.id, n: h.n, kind, target, per_tile, deps });
        }
    }
    out
}

// ---- Yields (tiles.py:196-391) --------------------------------------------------------------------

/// A terrain's own yields and its `Stats` uniques that hold (`tiles._single_terrain_stats`,
/// `tiles.py:199-205`).
fn single_terrain_stats(
    v: &EvalView<'_>,
    terrain: TerrainId,
    ctx: &Ctx,
    deps: &mut CondDeps,
) -> Stats {
    let r = v.game().rules();
    let td = &r.terrains()[terrain];
    let mut s = td.stats;
    for h in uq::object(v, &td.uniques, UniqueType::Stats, &Ctx::IGNORE) {
        *deps |= h.unique.deps();
        if let UniqueData::Stats(x) = h.data()
            && applies(h.id, ctx, v)
        {
            s += *r.uniques().stats(x.stats);
        }
    }
    s
}

/// Whether a terrain nullifies the yields beneath it here.
fn nullifies(v: &EvalView<'_>, terrain: TerrainId, ctx: &Ctx, deps: &mut CondDeps) -> bool {
    let td = &v.game().rules().terrains()[terrain];
    uq::object(v, &td.uniques, UniqueType::NullifyYields, &Ctx::IGNORE).any(|h| {
        *deps |= h.unique.deps();
        applies(h.id, ctx, v)
    })
}

/// The combined yields of a tile's terrains, before improvements (`tiles.terrain_stats`,
/// `tiles.py:208-223`): a terrain that nullifies the yields gives only its own, and one that
/// overrides them replaces those below it.
fn terrain_stats(v: &EvalView<'_>, t: TileIdx, ctx: &Ctx, deps: &mut CondDeps) -> Stats {
    let g = v.game();
    let mut total = Stats::ZERO;
    for terrain in all_terrains(g, t) {
        let s = single_terrain_stats(v, terrain, ctx, deps);
        if nullifies(v, terrain, ctx, deps) {
            return s;
        }
        if g.rules().terrains()[terrain].override_stats {
            total = s;
        } else {
            total += s;
        }
    }
    total
}

/// Whether a terrain of the tile suppresses the yields beneath it (`tiles.nullified`,
/// `tiles.py:226-229`).
fn nullified(v: &EvalView<'_>, t: TileIdx, ctx: &Ctx, deps: &mut CondDeps) -> bool {
    let g = v.game();
    let mut any = false;
    for terrain in all_terrains(g, t) {
        any |= nullifies(v, terrain, ctx, deps);
    }
    any
}

/// The nonzero stats of a `Stats` value: the keys a unique's stats name, as Python's dicts held
/// only those.
fn keys(s: &Stats) -> StatMask {
    s.nonzero().map(|(k, _)| k).collect()
}

/// What an improvement adds from techs, policies and its neighbours
/// (`tiles._extra_improvement_stats`, `tiles.py:232-256`).
fn extra_improvement_stats(
    v: &EvalView<'_>,
    t: TileIdx,
    imp: ImprovementId,
    viewer: PlayerId,
    ctx: &Ctx,
    deps: &mut CondDeps,
) -> Stats {
    let g = v.game();
    let r = g.rules();
    let table = r.uniques();
    let filters = table.filters();
    let mut s = Stats::ZERO;
    if let Some(res) = g.tile(t).and_then(crate::state::map::Tile::resource)
        && v.resource_visible(viewer, res)
        && resource_improved_by(g, res, imp)
    {
        s += r.resources()[res].improvement_stats;
    }
    let def = &r.improvements()[imp];
    for h in uq::object(v, &def.uniques, UniqueType::Stats, &Ctx::IGNORE) {
        *deps |= h.unique.deps();
        if let UniqueData::Stats(x) = h.data()
            && applies(h.id, ctx, v)
        {
            s += *table.stats(x.stats);
        }
    }
    for h in uq::object(v, &def.uniques, UniqueType::ImprovementStatsForAdjacencies, &Ctx::IGNORE) {
        *deps |= h.unique.deps();
        let UniqueData::ImprovementStatsForAdjacencies(x) = h.data() else { continue };
        *deps |= adjacency_deps(filters.tile(x.tiles));
        if applies(h.id, ctx, v) {
            let n = g
                .grid()
                .neighbors(t)
                .filter(|&nb| filters.tile_matches(x.tiles, v, nb, Some(viewer)))
                .count();
            #[allow(clippy::cast_precision_loss, reason = "at most six neighbours")]
            s.add_scaled(table.stats(x.stats), n as f64);
        }
    }
    for h in uq::object(v, &def.uniques, UniqueType::ImprovementStatsOnTile, &Ctx::IGNORE) {
        if let UniqueData::ImprovementStatsOnTile(x) = h.data() {
            *deps |= h.unique.deps() | tile_filter_deps(g, x.tiles);
            if applies(h.id, ctx, v) && filters.tile_matches(x.tiles, v, t, Some(viewer)) {
                s += *table.stats(x.stats);
            }
        }
    }
    s
}

/// Where a tile modifier lands on this tile: the improvement, the tile, or the route
/// (`add_stats` and `addp`, `tiles.py:291-301, 355-363`), or nowhere.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lands {
    Improvement,
    Tile,
    Route,
}

fn lands(
    v: &EvalView<'_>,
    target: Target,
    t: TileIdx,
    viewer: Option<PlayerId>,
    imp: Option<ImprovementId>,
    road: Option<ImprovementId>,
) -> Option<Lands> {
    let table = v.game().rules().uniques();
    let filters = table.filters();
    let (tiles, object) = match target {
        Target::Tiles(f) | Target::Without { tiles: f, .. } => (Some(f), None),
        Target::Object(o) => (table.object(o).tiles, Some(o)),
    };
    let improvement_matches = |i: Option<ImprovementId>| {
        let Some(i) = i else { return false };
        match object {
            None => tiles.is_some_and(|f| filters.tile(f).improvements.contains(i)),
            Some(o) => table.object(o).improvements.is_some_and(|s| table.in_set(s, i)),
        }
    };
    if improvement_matches(imp) {
        return Some(Lands::Improvement);
    }
    if tiles.is_some_and(|f| filters.tile_matches(f, v, t, viewer)) {
        return Some(Lands::Tile);
    }
    if improvement_matches(road) {
        return Some(Lands::Route);
    }
    None
}

/// Everything a tile yields to `viewer`, as `city` works it: `tiles._tile_stats`
/// (`tiles.py:265-349`). `mods` are the city's tile modifiers (its memo); with no city, a
/// viewer's own percentages apply, and with no viewer neither a resource nor an improvement's
/// extras do. What the evaluation read is added to `deps`.
#[must_use]
pub fn compute_tile_yield(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
    mods: Option<&CityMods>,
    deps: &mut CondDeps,
) -> Stats {
    let v = g.view();
    let r = g.rules();
    let table = r.uniques();
    let Some(tile) = g.tile(t) else { return Stats::ZERO };
    let ctx = Ctx { civ: viewer, city, tile: Some(t), ..Ctx::default() };
    let base = terrain_stats(&v, t, &ctx, deps);
    let ignored = nullified(&v, t, &ctx, deps);
    let imp = if ignored { None } else { unpillaged_improvement(g, t) };
    let road = if ignored { None } else { unpillaged_route(g, t) };
    let mut imp_s = imp.map_or(Stats::ZERO, |i| r.improvements()[i].stats);
    let mut road_s = road.map_or(Stats::ZERO, |i| r.improvements()[i].stats);
    let mut extra = Stats::ZERO;
    if let (Some(_), Some(m)) = (city, mods) {
        for x in &m.mods {
            let ModKind::Flat(stats) = x.kind else { continue };
            *deps |= x.deps;
            if x.per_tile && !applies(x.id, &ctx, &v) {
                continue;
            }
            if let Target::Without { without, .. } = x.target
                && table.filters().tile_matches(without, &v, t, viewer)
            {
                continue;
            }
            let add = *table.stats(stats) * f64::from(x.n);
            match lands(&v, x.target, t, viewer, imp, road) {
                Some(Lands::Improvement) => imp_s += add,
                Some(Lands::Tile) => extra += add,
                Some(Lands::Route) => road_s += add,
                None => {}
            }
        }
    }
    let mut total = base + extra;
    if tile.has_river()
        && let Some(river) = r.derived().known.river
    {
        total += single_terrain_stats(&v, river, &ctx, deps);
    }
    let mut minimum: Option<Stats> = None;
    if g.state().city_at(t).is_some() {
        let mut m = Stats::ZERO;
        for (k, x) in CITY_CENTER_MIN {
            m[k] = x;
        }
        minimum = Some(m);
    }
    if let Some(p) = viewer {
        if let Some(res) = tile.resource()
            && v.resource_visible(p, res)
        {
            total += r.resources()[res].stats;
        }
        if let Some(i) = imp {
            imp_s += extra_improvement_stats(&v, t, i, p, &ctx, deps);
            let def = &r.improvements()[i];
            for h in uq::object(&v, &def.uniques, UniqueType::EnsureMinimumStats, &Ctx::IGNORE) {
                *deps |= h.unique.deps();
            }
            if let Some(h) =
                uq::object(&v, &def.uniques, UniqueType::EnsureMinimumStats, &ctx).next()
                && let UniqueData::EnsureMinimumStats(x) = h.data()
            {
                minimum = Some(*table.stats(x.stats));
            }
        }
        if let Some(i) = road {
            road_s += extra_improvement_stats(&v, t, i, p, &ctx, deps);
        }
    }
    let (pt, pi, pr) = percentages(&v, t, viewer, city, mods, imp, road, &ctx, deps);
    for k in Stat::ALL {
        total[k] *= 1.0 + pt[k] / 100.0;
        imp_s[k] *= 1.0 + pi[k] / 100.0;
        road_s[k] *= 1.0 + pr[k] / 100.0;
    }
    total += imp_s;
    total += road_s;
    if let Some(m) = minimum {
        for k in keys(&m).iter() {
            if total[k] < m[k] {
                total[k] = m[k];
            }
        }
    }
    if let Some(p) = viewer
        && total[Stat::Gold] != 0.0
        && g.player(p).is_some_and(|x| x.econ.golden_age_turns > 0)
    {
        total[Stat::Gold] += 1.0;
    }
    total
}

/// The percentage modifiers of a tile's yields, of its terrain, improvement and route
/// (`tiles._tile_percentages`, `tiles.py:352-373`): the city's, or with no city the viewer's own.
#[allow(clippy::too_many_arguments, reason = "the parts of one tile's yield, in Python's order")]
fn percentages(
    v: &EvalView<'_>,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
    mods: Option<&CityMods>,
    imp: Option<ImprovementId>,
    road: Option<ImprovementId>,
    ctx: &Ctx,
    deps: &mut CondDeps,
) -> (Stats, Stats, Stats) {
    let g = v.game();
    let (mut pt, mut pi, mut pr) = (Stats::ZERO, Stats::ZERO, Stats::ZERO);
    let mut addp = |target: Target, stat: Option<Stat>, amount: f64| {
        let slot = match lands(v, target, t, viewer, imp, road) {
            Some(Lands::Improvement) => &mut pi,
            Some(Lands::Tile) => &mut pt,
            Some(Lands::Route) => &mut pr,
            None => return,
        };
        match stat {
            Some(k) => slot[k] += amount,
            None => {
                for k in Stat::ALL {
                    slot[k] += amount;
                }
            }
        }
    };
    if let (Some(_), Some(m)) = (city, mods) {
        for x in &m.mods {
            let ModKind::Percent(stat, pct) = x.kind else { continue };
            *deps |= x.deps;
            if x.per_tile && !applies(x.id, ctx, v) {
                continue;
            }
            for _ in 0..x.n {
                addp(x.target, stat, f64::from(pct));
            }
        }
    } else if let Some(p) = viewer
        && city.is_none()
    {
        for ty in [UniqueType::StatPercentFromObject, UniqueType::AllStatsPercentFromObject] {
            for h in uq::civ(v, p, ty, &Ctx::IGNORE) {
                *deps |= h.unique.deps();
                let (stat, pct, object) = match *h.data() {
                    UniqueData::StatPercentFromObject(x) => (Some(x.stat), x.percent, x.object),
                    UniqueData::AllStatsPercentFromObject(x) => (None, x.percent, x.object),
                    _ => continue,
                };
                *deps |= target_deps(g, Target::Object(object));
                if !applies(h.id, ctx, v) {
                    continue;
                }
                for _ in 0..h.n {
                    addp(Target::Object(object), stat, f64::from(pct));
                }
            }
        }
    }
    (pt, pi, pr)
}

/// Food, production and gold of a bare tile, for scoring starts (`tiles.start_yield`,
/// `tiles.py:376-391`): its terrains and its resource, lifted to `minimum`.
#[must_use]
pub fn start_yield(g: &Game, t: TileIdx, minimum: Option<&Stats>) -> f64 {
    let v = g.view();
    let mut deps = CondDeps::empty();
    let ctx = Ctx::tile(None, t);
    let mut s = terrain_stats(&v, t, &ctx, &mut deps);
    if let Some(res) = g.tile(t).and_then(crate::state::map::Tile::resource) {
        s += g.rules().resources()[res].stats;
    }
    if let Some(m) = minimum {
        for k in keys(m).iter() {
            if s[k] < m[k] {
                s[k] = m[k];
            }
        }
    }
    s[Stat::Food] + s[Stat::Production] + s[Stat::Gold]
}
