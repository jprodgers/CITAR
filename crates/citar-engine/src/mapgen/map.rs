//! The map being generated, and what the generator reads of the ruleset (`mapgen.py:380-525`:
//! `_Map` and its terrain helpers).
//!
//! [`Kit`] gathers, once per map, the terrain and resource lists the steps choose from and the
//! objects map generation names ([`KnownMap`]). [`GenMap`] holds the tiles and each tile's
//! climate, and answers map generation's filters through [`TileFacts`], from the terrain alone.

use crate::base::hex::HexGrid;
use crate::base::ids::{FeatureId, IdVec, ImprovementId, ResourceId, TerrainId, TileIdx};
use crate::base::sets::{FeatureSet, TerrainSet};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::{ResourceType, TerrainType};
use crate::rules::derived::KnownMap;
use crate::rules::gen_tables::{GenCond, TerrainGen};
use crate::state::map::{Tile, WATER};
use crate::unique::world::TileFacts;
use crate::unique::{GenFilter, UniqueType};

use super::options::MapOptions;

/// The set of one feature.
#[must_use]
pub(crate) fn only(f: FeatureId) -> FeatureSet {
    let mut s = FeatureSet::EMPTY;
    s.insert(f);
    s
}

/// A climate range as generation compares it (`_Map.occurs`, `mapgen.py:505-515`): the lower
/// bounds of -1 and 0 are widened a little, so the coldest and driest tiles still fit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Climate {
    t_lo: f64,
    t_hi: f64,
    h_lo: f64,
    h_hi: f64,
}

impl Climate {
    fn holds(&self, temp: f64, humid: f64) -> bool {
        self.t_lo < temp && temp <= self.t_hi && self.h_lo < humid && humid <= self.h_hi
    }
}

/// What map generation reads of the ruleset, gathered once per map.
#[derive(Debug)]
pub(crate) struct Kit<'r> {
    pub r: &'r Ruleset,
    pub names: KnownMap,
    /// The Hill feature and its terrain.
    pub hill: FeatureId,
    pub hill_terrain: TerrainId,
    /// The terrain land starts as: Plains, or the first base terrain of land.
    pub land: TerrainId,
    /// The terrain water starts as: Ocean, or the first base terrain of water.
    pub ocean: TerrainId,
    /// Each terrain's climate ranges; none means anywhere.
    climates: IdVec<TerrainId, Vec<Climate>>,
    /// The base terrains of water.
    pub water: TerrainSet,
    /// Terrains that block movement.
    pub impassable: TerrainSet,
    /// `Occurs in groups`: hills.
    pub groups: TerrainSet,
    /// Coast, and any water terrain marked `Coastal Water` (`_Map.coastal`).
    pub coastal_water: TerrainSet,
    /// Terrains that are a source of fresh water: lakes and oases.
    pub fresh: TerrainSet,
    /// Land a tile's climate chooses from (`_humidity_and_temperature`): passable, smooth, not
    /// rare, generated naturally.
    pub climate_lands: Vec<TerrainId>,
    /// Flat land a lowered mountain becomes (`_flat_for`): passable, smooth, with a climate.
    pub flats: Vec<TerrainId>,
    /// Forest, jungle and the like: features marked `Vegetation`, not rare.
    pub vegetation: Vec<TerrainId>,
    /// Features marked `Rare feature`.
    pub rare: Vec<TerrainId>,
    /// The natural wonders, in file order.
    pub wonders: Vec<TerrainId>,
    /// Strategic and luxury resources that generate naturally somewhere, in file order.
    pub strategic: Vec<ResourceId>,
    pub luxury: Vec<ResourceId>,
    /// Resources only a city-state makes (`Can only be created by Mercantile City-States`).
    pub city_state_only: IdVec<ResourceId, bool>,
    /// Bonus resources with food (`_normalize_start`).
    pub food_bonus: Vec<ResourceId>,
    pub ruins: Option<ImprovementId>,
}

impl<'r> Kit<'r> {
    /// The kit of `r`; `None` if the ruleset has no base terrain of land or of water, which no
    /// map can do without.
    pub(crate) fn new(r: &'r Ruleset) -> Option<Self> {
        let t = r.terrains();
        let g = r.gen_tables();
        let table = r.uniques();
        let known = &r.derived().known;
        let names = known.map;
        let hill = known.hill;
        let hill_terrain = *r.derived().features.get(hill)?;
        let first = |kind: TerrainType| t.iter().find(|(_, x)| x.kind == kind).map(|(id, _)| id);
        let land = names.plains.or_else(|| first(TerrainType::Land))?;
        let ocean = names.ocean.or_else(|| first(TerrainType::Water))?;
        let set = |f: &dyn Fn(TerrainId, &TerrainGen) -> bool| -> TerrainSet {
            t.ids().filter(|&id| f(id, &g.terrains[id])).collect()
        };
        let water = set(&|id, _| t[id].kind == TerrainType::Water);
        let mut coastal_water = set(&|id, x| t[id].kind == TerrainType::Water && x.coastal_water);
        if let Some(c) = names.coast {
            coastal_water.insert(c);
        }
        let climates = t
            .ids()
            .map(|id| {
                g.terrains[id]
                    .climates
                    .iter()
                    .map(|c| {
                        let (mut t_lo, t_hi) = c.temperature;
                        let (mut h_lo, h_hi) = c.humidity;
                        // Python compared the bounds with -1 and 0 exactly; they are read from
                        // whole numbers, so they are those exactly when they are.
                        if (t_lo + 1.0).abs() < 1e-12 {
                            t_lo -= 1e-6;
                        }
                        if h_lo.abs() < 1e-12 {
                            h_lo -= 1e-6;
                        }
                        Climate { t_lo, t_hi, h_lo, h_hi }
                    })
                    .collect()
            })
            .collect();
        let of_kind = |kind: TerrainType, f: &dyn Fn(TerrainId, &TerrainGen) -> bool| {
            t.ids().filter(|&id| t[id].kind == kind && f(id, &g.terrains[id])).collect::<Vec<_>>()
        };
        // Any `Doesn't generate naturally`, conditional or not, keeps a land out of the climate's
        // choice, as Python's `has_tag` did (`mapgen.py:552-554`).
        let climate_lands = of_kind(TerrainType::Land, &|id, x| {
            !t[id].impassable && !t[id].rough && !x.rare && !x.never && x.not_where.is_empty()
        });
        let flats = of_kind(TerrainType::Land, &|id, x| {
            !t[id].impassable && !t[id].rough && !x.climates.is_empty()
        });
        // A feature marked `Doesn't generate naturally` is never placed, which Python's lists did
        // not check (no shipped feature is both); one marked so under conditions is dropped
        // tile by tile, where they hold (`terrain::vegetation`, `terrain::rare_features`).
        // refcheck: mapgen-features-that-never-generate
        let vegetation =
            of_kind(TerrainType::TerrainFeature, &|_, x| x.vegetation && !x.rare && !x.never);
        let rare = of_kind(TerrainType::TerrainFeature, &|_, x| x.rare && !x.never);
        let wonders = of_kind(TerrainType::NaturalWonder, &|_, _| true);
        let resources = r.resources();
        let has = |id: ResourceId, ty: UniqueType| {
            resources[id].uniques.ids().any(|u| table.meta(u).ty == Some(ty))
        };
        let city_state_only: IdVec<ResourceId, bool> =
            resources.ids().map(|id| has(id, UniqueType::CityStateOnlyResource)).collect();
        let natural = |kind: ResourceType| {
            resources
                .iter()
                .filter(|(id, x)| x.kind == kind && !g.resources[*id].never)
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        let food_bonus = resources
            .iter()
            .filter(|(_, x)| x.kind == ResourceType::Bonus && x.stats[Stat::Food] != 0.0)
            .map(|(id, _)| id)
            .collect();
        Some(Self {
            r,
            names,
            hill,
            hill_terrain,
            land,
            ocean,
            climates,
            water,
            impassable: set(&|id, _| t[id].impassable),
            groups: set(&|_, x| x.groups),
            coastal_water,
            fresh: r.derived().fresh_water,
            climate_lands,
            flats,
            vegetation,
            rare,
            wonders,
            strategic: natural(ResourceType::Strategic),
            luxury: natural(ResourceType::Luxury),
            city_state_only,
            food_bonus,
            ruins: known.ancient_ruins,
        })
    }

    /// Whether a terrain has climate ranges at all.
    pub(crate) fn has_climate(&self, t: TerrainId) -> bool {
        !self.climates[t].is_empty()
    }

    /// Whether terrain `t` may occur at this temperature and humidity (`_Map.climate_ok`): any
    /// of its ranges holds, or it has none.
    pub(crate) fn climate_ok(&self, t: TerrainId, temp: f64, humid: f64) -> bool {
        let cs = &self.climates[t];
        cs.is_empty() || cs.iter().any(|c| c.holds(temp, humid))
    }

    /// The terrain a feature is.
    pub(crate) fn feature_terrain(&self, f: FeatureId) -> Option<TerrainId> {
        self.r.derived().features.get(f).copied()
    }

    /// Whether a terrain is a base terrain of water.
    pub(crate) fn is_water(&self, t: TerrainId) -> bool {
        self.water.contains(t)
    }
}

/// The map being generated: its tiles, and each tile's latitude, climate and landmass.
#[derive(Debug)]
pub(crate) struct GenMap<'r> {
    pub kit: &'r Kit<'r>,
    pub opts: &'r MapOptions,
    pub grid: HexGrid,
    pub tiles: Vec<Tile>,
    /// The polar ice band (`m.ice`).
    pub ice: Vec<bool>,
    /// 0 at the equator to 1 at the poles.
    pub lat: Vec<f64>,
    pub temp: Vec<f64>,
    pub humid: Vec<f64>,
    /// Each tile's landmass, numbered from the largest; [`WATER`] for water.
    pub continent: Vec<u16>,
    /// How many tiles hold each resource, for caps and shares (`m.placed`).
    pub placed: IdVec<ResourceId, u32>,
}

impl<'r> GenMap<'r> {
    pub(crate) fn new(
        kit: &'r Kit<'r>,
        opts: &'r MapOptions,
        grid: HexGrid,
        tiles: Vec<Tile>,
        ice: Vec<bool>,
    ) -> Self {
        let n = tiles.len();
        let h = f64::from(grid.height());
        let half = ((h - 1.0) / 2.0).max(1.0);
        let lat = grid
            .tiles()
            .map(|t| (f64::from(grid.xy(t).1) - (h - 1.0) / 2.0).abs() / half)
            .collect();
        Self {
            kit,
            opts,
            grid,
            tiles,
            ice,
            lat,
            temp: vec![0.0; n],
            humid: vec![0.0; n],
            continent: vec![WATER; n],
            placed: IdVec::from_elem(0, kit.r.resources().len()),
        }
    }

    #[inline]
    pub(crate) fn tile(&self, t: TileIdx) -> &Tile {
        &self.tiles[t.0 as usize]
    }

    #[inline]
    pub(crate) fn set(&mut self, t: TileIdx, tile: Tile) {
        self.tiles[t.0 as usize] = tile;
    }

    /// Every tile of the map.
    pub(crate) fn all(&self) -> impl DoubleEndedIterator<Item = TileIdx> + use<> {
        self.grid.tiles()
    }

    /// Whether a tile is water (`_Map.water`).
    #[inline]
    pub(crate) fn water(&self, t: TileIdx) -> bool {
        self.kit.is_water(self.tile(t).terrain())
    }

    /// Whether a tile is land.
    #[inline]
    pub(crate) fn land(&self, t: TileIdx) -> bool {
        !self.water(t)
    }

    /// The topmost terrain on a tile: its natural wonder, else its top feature, else its base
    /// (`_Map.last`).
    pub(crate) fn last(&self, t: TileIdx) -> TerrainId {
        let tile = self.tile(t);
        if let Some(w) = tile.wonder() {
            return w;
        }
        tile.features().top().and_then(|f| self.kit.feature_terrain(f)).unwrap_or(tile.terrain())
    }

    /// The base terrain, the features and the wonder, in that order (`_Map.all_terrains`).
    pub(crate) fn all_terrains(&self, t: TileIdx) -> impl Iterator<Item = TerrainId> + '_ {
        let tile = *self.tile(t);
        core::iter::once(tile.terrain())
            .chain(tile.features().iter().filter_map(|f| self.kit.feature_terrain(f)))
            .chain(tile.wonder())
    }

    /// Whether any terrain on the tile is in `set`.
    pub(crate) fn any_in(&self, t: TileIdx, set: &TerrainSet) -> bool {
        !self.tile_terrains(t).is_disjoint(set)
    }

    /// Whether the tile's base terrain is the named one.
    #[inline]
    pub(crate) fn is(&self, t: TileIdx, named: Option<TerrainId>) -> bool {
        named == Some(self.tile(t).terrain())
    }

    /// Whether the tile is a mountain (its base is the named Mountain).
    #[inline]
    pub(crate) fn mountain(&self, t: TileIdx) -> bool {
        self.is(t, self.kit.names.mountain)
    }

    /// Whether any terrain on the tile generates in groups: a hill (`_Map.hill`).
    pub(crate) fn hill(&self, t: TileIdx) -> bool {
        self.any_in(t, &self.kit.groups)
    }

    /// Whether anything on the tile blocks movement (`_Map.impassable`).
    pub(crate) fn impassable(&self, t: TileIdx) -> bool {
        self.any_in(t, &self.kit.impassable)
    }

    /// Whether the feature `f` is on the tile.
    pub(crate) fn has_feature(&self, t: TileIdx, f: Option<TerrainId>) -> bool {
        let Some(f) = f.and_then(|f| self.kit.r.terrains()[f].feature) else { return false };
        self.tile(t).features().contains(f)
    }

    /// Whether a tile is sea, the water a river must reach: water that is no source of fresh
    /// water, so lakes do not count (`_salt`).
    pub(crate) fn salt(&self, t: TileIdx) -> bool {
        let base = self.tile(t).terrain();
        self.kit.is_water(base) && !self.kit.fresh.contains(base)
    }

    /// Whether a neighbour is coastal water (`_Map.coastal`).
    pub(crate) fn coastal(&self, t: TileIdx) -> bool {
        self.grid.neighbors(t).any(|n| self.kit.coastal_water.contains(self.tile(n).terrain()))
    }

    /// Whether the conditions of a map-generation unique hold on a tile (`_Map.cond`).
    pub(crate) fn holds(&self, c: &GenCond, t: TileIdx) -> bool {
        c.holds(self.kit.r.uniques().filters(), self, t)
    }

    /// Whether a tile passes a map-generation filter (`_Map.matches`).
    pub(crate) fn matches(&self, f: GenFilter, t: TileIdx) -> bool {
        self.kit.r.uniques().filters().gen_matches(f, self, t)
    }

    /// Whether terrain `terrain` may occur on tile `t`, by the tile's climate.
    pub(crate) fn climate_ok(&self, terrain: TerrainId, t: TileIdx) -> bool {
        let i = t.0 as usize;
        self.kit.climate_ok(terrain, self.temp[i], self.humid[i])
    }

    /// Whether terrain `terrain` may be generated on tile `t` as it is now: not where one of
    /// its `Doesn't generate naturally <...>` holds.
    pub(crate) fn may_generate(&self, terrain: TerrainId, t: TileIdx) -> bool {
        // refcheck: mapgen-features-that-never-generate
        !self.kit.r.gen_tables().terrains[terrain].not_where.iter().any(|c| self.holds(c, t))
    }

    /// The tile with its features replaced.
    pub(crate) fn set_features(&mut self, t: TileIdx, f: FeatureSet) {
        let tile = self.tile(t).with_features(f);
        self.set(t, tile);
    }

    /// The tile with its base terrain replaced; a tile that becomes water loses its rivers,
    /// on both sides of each edge, since no river runs along water.
    pub(crate) fn set_terrain(&mut self, t: TileIdx, terrain: TerrainId) {
        let tile = self.tile(t).with_terrain(terrain);
        self.set(t, tile);
        // refcheck: mapgen-no-river-along-new-water
        if self.kit.is_water(terrain) && tile.river_mask() != 0 {
            self.clear_rivers(t);
        }
    }

    /// Takes every river off a tile's edges, and off its neighbours' matching edges.
    pub(crate) fn clear_rivers(&mut self, t: TileIdx) {
        for d in crate::base::hex::Dir::ALL {
            if !self.tile(t).river(d) {
                continue;
            }
            if let Some(n) = self.grid.neighbor(t, d) {
                let back = self.tile(n).river_mask() & !(1 << d.opposite().index());
                let nt = self.tile(n).with_river(back);
                self.set(n, nt);
            }
        }
        let tile = self.tile(t).with_river(0);
        self.set(t, tile);
    }

    /// Numbers the landmasses from the largest (`_assign_continents`).
    pub(crate) fn assign_continents(&mut self) {
        self.continent = super::continents::assign(self.kit.r, &self.grid, &self.tiles);
    }
}

impl TileFacts for GenMap<'_> {
    fn tile_terrains(&self, t: TileIdx) -> TerrainSet {
        let mut out = TerrainSet::new();
        let Some(tile) = self.tiles.get(t.0 as usize) else { return out };
        out.insert(tile.terrain());
        for f in tile.features().iter() {
            if let Some(x) = self.kit.feature_terrain(f) {
                out.insert(x);
            }
        }
        if let Some(w) = tile.wonder() {
            out.insert(w);
        }
        out
    }

    fn tile_river(&self, t: TileIdx) -> bool {
        self.tiles.get(t.0 as usize).is_some_and(Tile::has_river)
    }

    /// A river, or a source of fresh water on the tile or next to it (`_Map.fresh_water`).
    fn tile_fresh_water(&self, t: TileIdx) -> bool {
        self.tile_river(t)
            || self.any_in(t, &self.kit.fresh)
            || self.grid.neighbors(t).any(|n| self.any_in(n, &self.kit.fresh))
    }

    fn tile_next_to_coast(&self, t: TileIdx) -> bool {
        self.coastal(t)
    }
}
