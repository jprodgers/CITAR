//! The ruleset's names movement reads (`movement.py:90-96, 131-135, 288-299, 362-377` and
//! `units.py:758`), resolved once at load: the terrains, features and techs the movement rules
//! name, how each `Double movement in [...]` and `Units may enter ocean [...]` text reads a tile
//! or a unit, which `Can carry [n] extra [...] units` count toward a city's aircraft, and the
//! least a tile's governing terrain may cost to enter.
//!
//! Python compared these texts with names at every step (`"All"`, `"Embarked"`, `"Air"`, a
//! feature's or a base terrain's name); the comparisons happen here, once, and the game reads
//! ids (`game::path`).

use smallvec::SmallVec;

use super::Ruleset;
use super::defs::TerrainType;
use super::derived::Known;
use crate::base::collections::DetMap;
use crate::base::ids::{FeatureId, TechId, TerrainId, TileFilterId, UnitFilterId};
use crate::unique::{CondDeps, UniqueData, UniqueType};

/// Where a `Double movement in [terrainFilter]` unique counts (`movement.py:364-376`). Python
/// compared the filter's text with the tile's feature and terrain names in three passes around
/// the rough-terrain and hill rules; the text is read once here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleOn {
    /// The name of a feature: counts on a tile that has it, before the rough-terrain penalty.
    Feature(FeatureId),
    /// The name of a base terrain: counts on a tile of it, after the hill rule.
    Base(TerrainId),
    /// Anything else, read as a tile filter, last (a natural wonder's name among them).
    Filter(TileFilterId),
}

/// Who `Units may enter ocean` lets onto the ocean (`movement.ocean_permissions`,
/// `movement.py:90-96`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OceanFor {
    /// `[All]`: every unit.
    All,
    /// `[Embarked]`: embarked land units.
    Embarked,
    /// Units the filter matches.
    Units(UnitFilterId),
}

/// The unique types a unit's movement profile asks of it (`game::path::Profile`,
/// `movement.profile`, `movement.py:41-80`), with their conditionals; its double-movement uniques
/// are asked without.
pub const PROFILE_TYPES: [UniqueType; 14] = [
    UniqueType::AllTilesCost1Move,
    UniqueType::CanPassImpassable,
    UniqueType::IgnoresTerrainCost,
    UniqueType::IgnoresZOC,
    UniqueType::RoughTerrainPenalty,
    UniqueType::CanMoveOnWater,
    UniqueType::CannotEmbark,
    UniqueType::CannotEnterOcean,
    UniqueType::CanEnterForeignTiles,
    UniqueType::CanEnterForeignTilesButLosesReligiousStrength,
    UniqueType::CanTradeWithCityStateForGoldAndInfluence,
    UniqueType::CanEnterIceTiles,
    UniqueType::ReducedDisembarkCost,
    UniqueType::ReducedEmbarkCost,
];

/// The unique types a civilization's movement rules ask of it (`game::path::CivMove`,
/// `movement.py:83-96` and the `civ_has` reads of `movement.py:122-377`).
pub const CIV_TYPES: [UniqueType; 7] = [
    UniqueType::LandUnitEmbarkation,
    UniqueType::RoadMovementSpeed,
    UniqueType::RoadsConnectAcrossRivers,
    UniqueType::IgnoreHillMovementCost,
    UniqueType::ForestsAndJunglesAreRoads,
    UniqueType::UnitsMayEnterOcean,
    UniqueType::LandUnitsCrossTerrainAfterUnitGained,
];

/// The ruleset's objects and texts movement reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveRules {
    /// Movement points per tile of movement (`game.json` `move_scale`).
    pub scale: i32,
    pub ocean: Option<TerrainId>,
    pub mountain: Option<TerrainId>,
    pub ice: Option<FeatureId>,
    pub hill: FeatureId,
    pub forest: Option<FeatureId>,
    pub jungle: Option<FeatureId>,
    /// The techs that make a city centre a road and a railroad (`movement.route_at`,
    /// `movement.py:288-299`). A route that needs no tech is had by everyone.
    pub road_tech: Option<TechId>,
    pub rail_tech: Option<TechId>,
    /// The least movement points any terrain costs to enter, at most one (a city's tile costs
    /// one). At one, a step off the routes costs at least a point wherever it goes; below, the
    /// path search's bound asks the map whether a tile is governed by such a terrain (the shipped
    /// River, a feature that costs nothing, governs none unless an editor puts it on a tile).
    pub terrain_floor: i32,
    /// What the conditionals of the uniques of [`PROFILE_TYPES`] and of [`CIV_TYPES`] read,
    /// anywhere in the ruleset: what the memos of a unit's profile and a civilization's rules
    /// validate against (`game::path::memo`).
    pub profile_deps: CondDeps,
    pub civ_deps: CondDeps,
    double: DetMap<TileFilterId, DoubleOn>,
    ocean_for: DetMap<UnitFilterId, OceanFor>,
    /// The `Can carry [n] extra [Air] units` filters a city's air capacity counts: Python
    /// compared the parameter with `Air` (`units.py:758`).
    air: SmallVec<[UnitFilterId; 1]>,
}

impl Default for MoveRules {
    /// Nothing resolved: what the loader starts from.
    fn default() -> Self {
        Self {
            scale: 1,
            ocean: None,
            mountain: None,
            ice: None,
            hill: FeatureId(0),
            forest: None,
            jungle: None,
            road_tech: None,
            rail_tech: None,
            terrain_floor: 1,
            profile_deps: CondDeps::empty(),
            civ_deps: CondDeps::empty(),
            double: DetMap::default(),
            ocean_for: DetMap::default(),
            air: SmallVec::new(),
        }
    }
}

impl MoveRules {
    /// The movement names of `r`, whose uniques are compiled; `known` is the objects the engine
    /// names.
    pub(crate) fn new(r: &Ruleset, known: &Known) -> Self {
        let feature = |t: Option<TerrainId>| t.and_then(|t| r.terrains().get(t)?.feature);
        let t = r.uniques();
        let mut double = DetMap::default();
        let mut ocean_for = DetMap::default();
        let mut air = SmallVec::new();
        for (_, u) in t.iter() {
            match u.data {
                UniqueData::DoubleMovementOnTerrain(x) => {
                    double.entry(x.terrain).or_insert_with(|| double_on(r, x.terrain));
                }
                UniqueData::UnitsMayEnterOcean(x) => {
                    // Python compared the parameter's text (`movement.py:93-95`).
                    let on = match t.unit_filter(x.units) {
                        "All" | "all" => OceanFor::All,
                        "Embarked" => OceanFor::Embarked,
                        _ => OceanFor::Units(x.units),
                    };
                    ocean_for.insert(x.units, on);
                }
                UniqueData::CarryExtraAirUnits(x)
                    if t.unit_filter(x.units) == "Air" && !air.contains(&x.units) =>
                {
                    air.push(x.units);
                }
                _ => {}
            }
        }
        let deps = |types: &[UniqueType]| {
            t.iter()
                .filter(|&(id, _)| t.meta(id).ty.is_some_and(|ty| types.contains(&ty)))
                .fold(CondDeps::empty(), |d, (_, u)| d | u.deps())
        };
        let tech = |i| r.improvements().get(i).and_then(|d| d.tech_required);
        let terrain_floor =
            r.terrains().iter().map(|(_, d)| d.movement_cost).fold(1, i32::min).max(0);
        Self {
            scale: r.constants().move_scale,
            ocean: known.map.ocean,
            mountain: known.map.mountain,
            ice: feature(known.map.ice),
            hill: known.hill,
            forest: feature(known.map.forest),
            jungle: feature(known.map.jungle),
            road_tech: tech(known.road),
            rail_tech: tech(known.railroad),
            terrain_floor,
            profile_deps: deps(&PROFILE_TYPES),
            civ_deps: deps(&CIV_TYPES),
            double,
            ocean_for,
            air,
        }
    }

    /// Where a double-movement filter counts.
    #[must_use]
    pub fn double_on(&self, f: TileFilterId) -> DoubleOn {
        self.double.get(&f).copied().unwrap_or(DoubleOn::Filter(f))
    }

    /// Whether a city's `Can carry [n] extra [...] units` counts toward its air capacity.
    #[must_use]
    pub fn is_air_filter(&self, f: UnitFilterId) -> bool {
        self.air.contains(&f)
    }

    /// Whom an ocean permission lets in.
    #[must_use]
    pub fn ocean_for(&self, f: UnitFilterId) -> OceanFor {
        self.ocean_for.get(&f).copied().unwrap_or(OceanFor::Units(f))
    }
}

/// How the text of a double-movement filter reads a tile: a feature's name, a base terrain's, or
/// a filter (`movement.py:364-376`).
fn double_on(r: &Ruleset, f: TileFilterId) -> DoubleOn {
    let text = r.uniques().tile_filter(f);
    match r.lookup::<TerrainId>(text) {
        Some(id) => match (&r.terrains()[id].kind, r.terrains()[id].feature) {
            (TerrainType::TerrainFeature, Some(feat)) => DoubleOn::Feature(feat),
            (TerrainType::Land | TerrainType::Water, _) => DoubleOn::Base(id),
            _ => DoubleOn::Filter(f),
        },
        None => DoubleOn::Filter(f),
    }
}
