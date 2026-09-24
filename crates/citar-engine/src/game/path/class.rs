//! What decides how a unit moves: its movement profile ([`Profile`], `movement.profile`,
//! `movement.py:41-80`), its civilization's movement rules ([`CivMove`], `movement.py:83-96` and
//! the `civ_has` reads of `movement.py:122-377`), and the ruleset's names they read
//! ([`MoveRules`]), resolved once per game.
//!
//! A search takes these once, into a [`Mover`], and then reads tiles alone. Python cached the
//! profile per unit, tile, promotion count, tech count and policy count and cleared it on every
//! write (`g._cache`); a mover lives for one search, or one step, so it is never stale.

use core::cell::{OnceCell, RefCell};

use smallvec::SmallVec;

use crate::base::collections::DetMap;
use crate::base::ids::{
    BaseUnitId, FeatureId, PlayerId, TechId, TerrainId, TileFilterId, UniqueId, UnitFilterId,
    UnitId,
};
use crate::base::sets::{BitSet, PlayerSet};
use crate::game::Game;
use crate::rules::Ruleset;
use crate::rules::defs::{BaseUnitDef, Domain, TerrainType};
use crate::unique::filter::UnitScope;
use crate::unique::{Ctx, FilterFacts, UniqueData, UniqueType, uq};

// ---- The ruleset's names ------------------------------------------------------------------------

/// Where a `Double movement in [terrainFilter]` unique counts (`movement.py:364-376`). Python
/// compared the filter's text with the tile's feature and terrain names in three passes around
/// the rough-terrain and hill rules; the text is read once here, at the start of the game.
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

/// The ruleset's objects and texts movement reads, resolved once per game.
#[derive(Clone, Debug)]
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
    double: DetMap<TileFilterId, DoubleOn>,
    ocean_for: DetMap<UnitFilterId, OceanFor>,
    /// The `Can carry [n] extra [Air] units` filters a city's air capacity counts: Python
    /// compared the parameter with `Air` (`units.py:758`).
    air: SmallVec<[UnitFilterId; 1]>,
}

impl MoveRules {
    /// The ruleset's movement names.
    #[must_use]
    pub fn new(r: &Ruleset) -> Self {
        let known = &r.derived().known;
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
        let tech = |i| r.improvements().get(i).and_then(|d| d.tech_required);
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

// ---- A unit's profile ---------------------------------------------------------------------------

/// One `Double movement in [...]` unique of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Double {
    pub on: DoubleOn,
    pub id: UniqueId,
    /// Whether it has conditionals, which are asked on the tile entered.
    pub conditional: bool,
}

/// The movement rules of one unit where it stands (`movement.Profile`, `movement.py:41-80`): its
/// profile's flags asked in its context (its owner's, on its tile), its double-movement uniques
/// with their conditionals left for the tile entered, and its embark and disembark costs with
/// its civilization's. A unit with no unit behind it (a type asked about a tile) has none of
/// them, as Python passed no profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub all_1: bool,
    pub impassable_ok: bool,
    pub ignores_terrain: bool,
    pub ignores_zoc: bool,
    pub rough_penalty: bool,
    pub on_water: bool,
    pub cannot_embark: bool,
    pub no_ocean: bool,
    pub foreign_ok: bool,
    pub cs_ok: bool,
    pub ice_ok: bool,
    /// What disembarking and embarking cost, in move-scale units; `None` is all that is left.
    pub disembark: Option<i32>,
    pub embark: Option<i32>,
    pub doubles: SmallVec<[Double; 2]>,
}

impl Profile {
    /// Unit `u`'s profile where it stands now.
    #[must_use]
    pub fn of(g: &Game, rules: &MoveRules, u: UnitId) -> Self {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        let has = |ty| uq::any(uq::unit(&v, u, ty, &ctx));
        let t = g.rules().uniques();
        let doubles = uq::unit(&v, u, UniqueType::DoubleMovementOnTerrain, &Ctx::IGNORE)
            .filter_map(|h| match h.data() {
                UniqueData::DoubleMovementOnTerrain(x) => Some(Double {
                    on: rules.double_on(x.terrain),
                    id: h.id,
                    conditional: !t.get(h.id).conds.is_empty(),
                }),
                _ => None,
            })
            .collect();
        // The least of the unit's and its civilization's, in movement points.
        let least = |ty| {
            uq::unit_and_civ(&v, u, ty, &ctx)
                .filter_map(|h| match h.data() {
                    UniqueData::ReducedDisembarkCost(x) => Some(x.movement),
                    UniqueData::ReducedEmbarkCost(x) => Some(x.movement),
                    _ => None,
                })
                .min()
                .map(|n| n.saturating_mul(rules.scale))
        };
        Self {
            all_1: has(UniqueType::AllTilesCost1Move),
            impassable_ok: has(UniqueType::CanPassImpassable),
            ignores_terrain: has(UniqueType::IgnoresTerrainCost),
            ignores_zoc: has(UniqueType::IgnoresZOC),
            rough_penalty: has(UniqueType::RoughTerrainPenalty),
            on_water: has(UniqueType::CanMoveOnWater),
            cannot_embark: has(UniqueType::CannotEmbark),
            no_ocean: has(UniqueType::CannotEnterOcean),
            foreign_ok: has(UniqueType::CanEnterForeignTiles)
                || has(UniqueType::CanEnterForeignTilesButLosesReligiousStrength),
            cs_ok: has(UniqueType::CanTradeWithCityStateForGoldAndInfluence),
            ice_ok: has(UniqueType::CanEnterIceTiles),
            disembark: least(UniqueType::ReducedDisembarkCost),
            embark: least(UniqueType::ReducedEmbarkCost),
            doubles,
        }
    }
}

// ---- A civilization's movement rules -----------------------------------------------------------

/// What a civilization's uniques say about moving, asked of the civilization
/// (`movement.py:83-96` and the `civ_has` reads of `movement.py:122-377`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CivMove {
    /// Land units may embark (`civ_can_embark`); never the barbarians'.
    pub can_embark: bool,
    /// `Units may enter ocean [All]`.
    pub ocean_all: bool,
    /// `[All]` or `[Embarked]`: embarked units may enter the ocean.
    pub ocean_embarked: bool,
    /// The other `Units may enter ocean` filters.
    pub ocean_units: SmallVec<[UnitFilterId; 2]>,
    /// Land units may cross mountains: `Land units may cross [Mountain] tiles after the first
    /// [unit] is earned`, and it was.
    pub cross_mountains: bool,
    pub road_speed: bool,
    pub rivers_ok: bool,
    pub hill_ignore: bool,
    pub forest_roads: bool,
}

impl CivMove {
    /// Civilization `p`'s movement rules.
    #[must_use]
    pub fn of(g: &Game, rules: &MoveRules, p: PlayerId) -> Self {
        let v = g.view();
        let ctx = Ctx::civ(p);
        let has = |ty| uq::any(uq::civ(&v, p, ty, &ctx));
        let mut out = Self {
            can_embark: !g.is_barbarian(p) && has(UniqueType::LandUnitEmbarkation),
            road_speed: has(UniqueType::RoadMovementSpeed),
            rivers_ok: has(UniqueType::RoadsConnectAcrossRivers),
            hill_ignore: has(UniqueType::IgnoreHillMovementCost),
            forest_roads: has(UniqueType::ForestsAndJunglesAreRoads),
            ..Self::default()
        };
        for h in uq::civ(&v, p, UniqueType::UnitsMayEnterOcean, &ctx) {
            if let UniqueData::UnitsMayEnterOcean(x) = h.data() {
                match rules.ocean_for(x.units) {
                    OceanFor::All => {
                        out.ocean_all = true;
                        out.ocean_embarked = true;
                    }
                    OceanFor::Embarked => out.ocean_embarked = true,
                    OceanFor::Units(f) => out.ocean_units.push(f),
                }
            }
        }
        // Carthage (`movement.py:131-135`), with the units gained Python never recorded.
        let gained = g.player(p).map(|pl| pl.civ.units_gained).unwrap_or_default();
        let t = g.rules().uniques();
        out.cross_mountains =
            uq::civ(&v, p, UniqueType::LandUnitsCrossTerrainAfterUnitGained, &ctx).any(|h| match h
                .data()
            {
                UniqueData::LandUnitsCrossTerrainAfterUnitGained(x) => {
                    Some(x.terrain) == rules.mountain && gained.iter().any(|b| t.in_set(x.units, b))
                }
                _ => false,
            });
        out
    }
}

// ---- The mover ----------------------------------------------------------------------------------

/// Tiles from which enemies exert a zone of control on a mover (`movement.zoc_between`,
/// `movement.py:312-327`): an enemy city; else, the first military unit on the tile, if it is an
/// enemy's and either a naval unit or, for a land mover, a land unit not embarked. Like Python it
/// counts every enemy, seen or not.
#[derive(Clone, Debug, Default)]
pub(crate) struct Zoc {
    pub tiles: BitSet,
    /// No tile exerts one: most searches, which then skip the look (a bit set's emptiness walks
    /// its words).
    pub none: bool,
}

/// One unit's movement, or one unit type's, for one search: everything that does not change while
/// it looks at tiles, taken once.
pub struct Mover<'g> {
    pub(crate) g: &'g Game,
    pub(crate) rules: &'g MoveRules,
    /// The unit, or `None` for a type asked about a tile.
    pub unit: Option<UnitId>,
    /// A unit the stacking rules leave out: the unit itself, or one about to be replaced (an
    /// upgrade places its successor as if it were gone).
    pub ignore: Option<UnitId>,
    pub pid: PlayerId,
    pub base: BaseUnitId,
    pub def: &'g BaseUnitDef,
    pub prof: Profile,
    pub civ: CivMove,
    pub barbarian: bool,
    /// Whose territory it may enter (`Game::can_enter_territory`, by owner).
    pub(crate) enter: PlayerSet,
    /// Who it is at war with.
    pub(crate) war: PlayerSet,
    pub(crate) city_states: PlayerSet,
    /// It matches a `Units may enter ocean` filter of its civilization's.
    pub(crate) ocean_unit_ok: bool,
    /// For a type, its own `Cannot enter ocean tiles` uniques, asked per tile
    /// (`movement.py:154-157`).
    pub(crate) no_ocean_uniques: SmallVec<[UniqueId; 1]>,
    /// What its owner has explored, and sees now.
    pub(crate) explored: Option<&'g BitSet>,
    pub(crate) visible: Option<&'g BitSet>,
    zoc: OnceCell<Zoc>,
    /// `Enemy units must spend extra movement` of each enemy owner, in move-scale units, asked on
    /// first need.
    extra: RefCell<SmallVec<[(PlayerId, i32); 4]>>,
}

impl<'g> Mover<'g> {
    /// Unit `u`'s movement where it stands; `None` for a unit the game does not have.
    #[must_use]
    pub fn unit(g: &'g Game, u: UnitId) -> Option<Self> {
        let x = g.unit(u)?;
        let rules = g.derived().move_rules();
        let prof = Profile::of(g, rules, u);
        let mut m = Self::new(g, rules, x.owner(), x.base, Some(u), prof)?;
        if !m.civ.ocean_units.is_empty() {
            let v = g.view();
            let f = g.rules().uniques().filters();
            m.ocean_unit_ok =
                m.civ.ocean_units.iter().any(|&id| f.unit_matches(id, &v, u, UnitScope::default()));
        }
        Some(m)
    }

    /// A unit of `base` that player `p` would have, with no unit behind it: what placing a new
    /// unit asks (`movement.can_stand` with no unit). `None` for a base unit the ruleset lacks.
    #[must_use]
    pub fn of_type(g: &'g Game, p: PlayerId, base: BaseUnitId) -> Option<Self> {
        let rules = g.derived().move_rules();
        let mut m = Self::new(g, rules, p, base, None, Profile::default())?;
        let t = g.rules().uniques();
        m.no_ocean_uniques = m
            .def
            .uniques
            .ids()
            .chain(g.rules().unit_types()[m.def.unit_type].uniques.ids())
            .filter(|&id| t.meta(id).ty == Some(UniqueType::CannotEnterOcean))
            .collect();
        Some(m)
    }

    fn new(
        g: &'g Game,
        rules: &'g MoveRules,
        p: PlayerId,
        base: BaseUnitId,
        unit: Option<UnitId>,
        prof: Profile,
    ) -> Option<Self> {
        let def = g.rules().base_units().get(base)?;
        let players = g.state().players();
        let enter = players.ids().filter(|&q| g.can_enter_owner(p, q)).collect();
        let city_states =
            players.iter().filter(|(_, x)| x.is_city_state()).map(|(q, _)| q).collect();
        Some(Self {
            g,
            rules,
            unit,
            ignore: unit,
            pid: p,
            base,
            def,
            prof,
            civ: CivMove::of(g, rules, p),
            barbarian: g.is_barbarian(p),
            enter,
            war: g.state().diplo().war_mask(p),
            city_states,
            ocean_unit_ok: false,
            no_ocean_uniques: SmallVec::new(),
            explored: g.player(p).map(|x| &x.explored),
            visible: g.derived().vis().visible(p),
            zoc: OnceCell::new(),
            extra: RefCell::new(SmallVec::new()),
        })
    }

    /// The game it moves in.
    #[must_use]
    pub const fn game(&self) -> &'g Game {
        self.g
    }

    /// Movement points per tile of movement.
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.rules.scale
    }

    /// Its domain.
    #[must_use]
    pub fn domain(&self) -> Domain {
        self.def.domain
    }

    /// Whether it is a military unit.
    #[must_use]
    pub fn military(&self) -> bool {
        self.def.military
    }

    /// Whether it is an aircraft, which never walks (`movement.is_air`).
    #[must_use]
    pub fn is_air(&self) -> bool {
        self.def.domain == Domain::Air
    }

    /// Whether it is at war with `q`.
    #[must_use]
    pub(crate) fn at_war(&self, q: PlayerId) -> bool {
        self.war.contains(q)
    }

    /// The tiles exerting a zone of control on it.
    pub(crate) fn zoc(&self) -> &Zoc {
        self.zoc.get_or_init(|| self.build_zoc())
    }

    fn build_zoc(&self) -> Zoc {
        let g = self.g;
        let mut tiles = BitSet::with_capacity(g.state().map().size());
        let land = self.def.domain == Domain::Land;
        let r = g.rules();
        let v = g.view();
        for q in self.war.iter() {
            for c in g.player_cities(q) {
                tiles.insert(c.tile().0);
            }
            for m in g.player_units(q) {
                let Some(d) = r.base_units().get(m.base) else { continue };
                if !d.military || d.domain == Domain::Air {
                    continue;
                }
                let t = m.tile();
                if g.city_at(t).is_some() || g.military_at(t).map(|x| x.id()) != Some(m.id()) {
                    continue;
                }
                if d.domain == Domain::Water || (land && !v.unit_embarked(m.id())) {
                    tiles.insert(t.0);
                }
            }
        }
        let none = tiles.is_empty();
        Zoc { tiles, none }
    }

    /// What entering a tile of `owner` costs it on top, at war (`movement.py:345-349`): the
    /// owner's `Enemy [units] units must spend [n] extra movement points` that match it.
    pub(crate) fn extra(&self, owner: PlayerId) -> i32 {
        if let Some(&(_, n)) = self.extra.borrow().iter().find(|(q, _)| *q == owner) {
            return n;
        }
        let n = self.unit.map_or(0, |u| {
            let g = self.g;
            let v = g.view();
            let f = g.rules().uniques().filters();
            let sc = self.rules.scale;
            uq::civ(&v, owner, UniqueType::EnemyUnitsSpendExtraMovement, &Ctx::civ(owner))
                .filter_map(|h| match h.data() {
                    UniqueData::EnemyUnitsSpendExtraMovement(x)
                        if f.unit_matches(x.units, &v, u, UnitScope::default()) =>
                    {
                        Some(x.movement.saturating_mul(sc).saturating_mul(i32::from(h.n)))
                    }
                    _ => None,
                })
                .fold(0i32, i32::saturating_add)
        });
        self.extra.borrow_mut().push((owner, n));
        n
    }
}
