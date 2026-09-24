//! What decides how a unit moves: its movement profile ([`Profile`], `movement.profile`,
//! `movement.py:41-80`) and its civilization's movement rules ([`CivMove`], `movement.py:83-96`
//! and the `civ_has` reads of `movement.py:122-377`), with the ruleset's names they read
//! ([`MoveRules`], resolved at load).
//!
//! A search takes these once, into a [`Mover`], and then reads tiles alone. Python cached the
//! profile per unit, tile, promotion count, tech count and policy count and cleared it on every
//! write (`g._cache`); a mover lives for one search, or one step, so it is never stale.

use core::cell::{OnceCell, RefCell};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::base::ids::{BaseUnitId, PlayerId, UniqueId, UnitFilterId, UnitId};
use crate::base::sets::{BitSet, PlayerSet};
use crate::game::Game;
use crate::rules::defs::{BaseUnitDef, Domain};
pub use crate::rules::moves::{DoubleOn, MoveRules, OceanFor};
use crate::unique::filter::UnitScope;
use crate::unique::{Ctx, FilterFacts, UniqueData, UniqueType, uq};

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
        // refcheck: units-gained-recorded
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
    /// Shared, so that a mover takes the game's at the cost of a count.
    pub tiles: Arc<BitSet>,
    /// No tile exerts one: most searches, which then skip the look (a bit set's emptiness walks
    /// its words).
    pub none: bool,
}

impl Zoc {
    /// The tiles from which player `p`'s enemies exert a zone of control on its units, its land
    /// units when `land`: a land unit's is not exerted by an embarked unit.
    pub(crate) fn build(g: &Game, p: PlayerId, land: bool) -> Self {
        let war = g.state().diplo().war_mask(p);
        if war.is_empty() {
            return Self { tiles: Arc::default(), none: true };
        }
        let mut tiles = BitSet::with_capacity(g.state().map().size());
        let r = g.rules();
        let v = g.view();
        for q in war.iter() {
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
        Self { tiles: Arc::new(tiles), none }
    }
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
    /// The game's zones of control for its owner's units of its kind, taken on first need.
    zoc: OnceCell<Option<Zoc>>,
    /// `Enemy units must spend extra movement` of each enemy owner, in move-scale units, asked on
    /// first need.
    extra: RefCell<SmallVec<[(PlayerId, i32); 4]>>,
}

impl<'g> Mover<'g> {
    /// Unit `u`'s movement where it stands; `None` for a unit the game does not have.
    #[must_use]
    pub fn unit(g: &'g Game, u: UnitId) -> Option<Self> {
        let x = g.unit(u)?;
        let rules = &g.rules().derived().moves;
        let mut m = Self::new(g, rules, x.owner(), x.base, Some(u), super::memo::profile(g, u))?;
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
        let rules = &g.rules().derived().moves;
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
        let parts = super::memo::civ_parts(g, p)?;
        Some(Self {
            g,
            rules,
            unit,
            ignore: unit,
            pid: p,
            base,
            def,
            prof,
            civ: parts.civ.clone(),
            barbarian: g.is_barbarian(p),
            enter: parts.enter,
            war: g.state().diplo().war_mask(p),
            city_states: parts.city_states,
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

    /// The tiles exerting a zone of control on it; `None` when none does.
    pub(crate) fn zoc(&self) -> Option<&Zoc> {
        let land = self.def.domain == Domain::Land;
        self.zoc
            .get_or_init(|| {
                if self.war.is_empty() {
                    return None;
                }
                super::memo::zoc(self.g, self.pid, land).filter(|z| !z.none).map(|z| z.clone())
            })
            .as_ref()
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
