//! The memos of yields, stats, happiness and connectivity (DESIGN.md 6.5): `CityMods` and
//! `TileYield` (tiles), `CityHappiness` and `CityStats` (cities), `Happiness`, `CivStats`,
//! `Connectivity` and the unit-supply deficit (civilizations).
//!
//! Replaces the entries of Python's yield cache (`tstats`, `chappy`, `cstats`, `happiness`,
//! `stat_map`, `civ_stats`, `connected`, `supply_deficit`; `game.py:565-609`), which every write
//! threw away: 98.7% of the tile yields Python recomputed were unchanged. Each memo validates
//! itself on read against the revisions of what it read and the memos upstream of it:
//! - a tile's yield, for its owner as its own city works it: its own inputs and its neighbours'
//!   (a river's fresh water, the coast, an improvement's adjacency), its owner's techs and stocks
//!   (resources it sees, a golden age), which cities exist, its city's modifiers (`CityMods`),
//!   and the conditionals and filters the computation evaluated, whose classes it records. A
//!   tile seen by another civilization, or worked by another city, has a table entry validated
//!   the same way;
//! - a city's modifiers: its own index, its owner's, its majority religion's, and the
//!   conditionals evaluated with the city in context;
//! - a city's parts (`CityHappiness`): the yields of the tiles it works, the city, its owner's
//!   index, seat and cities, and the classes the conditionals and filters of the city's stat
//!   uniques read in the ruleset ([`StatsDeps`]);
//! - a city's stats: its parts, its owner's connectivity, supply deficit and, in We Love The King
//!   Day, happiness; its capital;
//! - a civilization's happiness and stats: its cities', its supply, routes, units, deals and
//!   religion; its connectivity: routes, cities, borders, techs and harbours.
//!
//! [`verify`] is the cache oracle for them all.

use core::cell::{Cell, Ref, RefCell};

use super::civ;
use super::rev::{BitEq, CopyMemo, Memo, Rev};
use crate::base::collections::LookupMap;
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId};
use crate::base::sets::PlayerVec;
use crate::base::stats::Stats;
use crate::game::Game;
use crate::game::cities::connections::{self, Connectivity, Media};
use crate::game::cities::stats::{self as cstats, CityParts, CityStats, Work};
use crate::game::economy::{self, CivStats, Happiness};
use crate::game::tiles::{self, CityMods};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::params::Param;
use crate::unique::{CondDeps, Ctx, UniqueType};

// ---- What the stat uniques read (the ruleset's) -------------------------------------------------

/// The unique types a city's parts and stats read (`cities.py:139-700`).
const CITY_TYPES: [UniqueType; 27] = [
    UniqueType::Stats,
    UniqueType::StatsFromObject,
    UniqueType::StatsFromBuildings,
    UniqueType::StatPercentFromObject,
    UniqueType::AllStatsPercentFromObject,
    UniqueType::BuildingMaintenance,
    UniqueType::StatsFromTradeRoute,
    UniqueType::StatPercentFromTradeRoutes,
    UniqueType::BonusStatsFromCityStates,
    UniqueType::StatsPerCity,
    UniqueType::StatsPerPopulation,
    UniqueType::StatsFromCitiesOnSpecificTiles,
    UniqueType::StatPercentBonus,
    UniqueType::StatPercentBonusCities,
    UniqueType::PercentProductionUnits,
    UniqueType::PercentProductionWonders,
    UniqueType::PercentProductionBuildings,
    UniqueType::PercentProductionBuildingsInCapital,
    UniqueType::StatPercentFromReligionFollowers,
    UniqueType::FoodConsumptionBySpecialists,
    UniqueType::FoodConsumptionByPopulation,
    UniqueType::GrowthPercentBonus,
    UniqueType::NullifiesGrowth,
    UniqueType::UnhappinessFromCitiesPercentage,
    UniqueType::UnhappinessFromPopulationTypePercentageChange,
    UniqueType::StatsFromSpecialist,
    UniqueType::RemovesAnnexUnhappiness,
];

/// The unique types a civilization's happiness, stats and connectivity read besides
/// (`economy.py:404-708`, `cities.py:1967-2064`).
const CIV_TYPES: [UniqueType; 17] = [
    UniqueType::BonusHappinessFromLuxury,
    UniqueType::CityStateLuxuryHappiness,
    UniqueType::RetainHappinessFromLuxury,
    UniqueType::StatsFromGlobalCitiesFollowingReligion,
    UniqueType::StatsFromGlobalFollowers,
    UniqueType::StatsPerPolicies,
    UniqueType::StatsFromNaturalWonders,
    UniqueType::CityStateStatPercent,
    UniqueType::ExcessHappinessToGlobalStat,
    UniqueType::NoImprovementMaintenanceInSpecificTiles,
    UniqueType::ImprovementMaintenance,
    UniqueType::ImprovementAllMaintenance,
    UniqueType::RoadMaintenance,
    UniqueType::FreeUnits,
    UniqueType::UnitsInCitiesNoMaintenance,
    UniqueType::UnitMaintenanceDiscount,
    UniqueType::ForestsAndJunglesAreRoads,
];

/// The unique types the unit supply reads (`economy.py:570-584`).
const SUPPLY_TYPES: [UniqueType; 3] =
    [UniqueType::BaseUnitSupply, UniqueType::UnitSupplyPerCity, UniqueType::UnitSupplyPerPop];

/// The unique types a tile's yields read (`tiles.py:196-373`).
const TILE_TYPES: [UniqueType; 10] = [
    UniqueType::Stats,
    UniqueType::StatsFromTiles,
    UniqueType::StatsFromObject,
    UniqueType::StatsFromTilesWithout,
    UniqueType::StatPercentFromObject,
    UniqueType::AllStatsPercentFromObject,
    UniqueType::NullifyYields,
    UniqueType::ImprovementStatsForAdjacencies,
    UniqueType::ImprovementStatsOnTile,
    UniqueType::EnsureMinimumStats,
];

/// The unique types citizen ranking reads besides (`cities.py:706-790`, `great_people.py:19-32`).
const RANK_TYPES: [UniqueType; 2] =
    [UniqueType::GreatPersonPointPercentage, UniqueType::GreatPersonBoostWithFriendship];

/// What the conditionals and filters of the uniques the memos evaluate read, over the whole
/// ruleset: what each memo that does not record its own validates against, and what a write must
/// move for citizens to be looked at again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsDeps {
    /// A city's parts and stats.
    pub city: CondDeps,
    /// A civilization's happiness, stats and connectivity.
    pub civ: CondDeps,
    /// The unit supply.
    pub supply: CondDeps,
    /// Citizen ranking: tile yields, city stats and great person points.
    pub citizens: CondDeps,
}

/// What unique `id` reads when it is evaluated: its conditionals' classes and its parameters'
/// filters'.
#[must_use]
pub fn unique_deps(rules: &Ruleset, id: UniqueId) -> CondDeps {
    let t = rules.uniques();
    let filters = t.filters();
    let u = t.get(id);
    let mut d = u.deps();
    // Its tile filter is asked of the neighbours, not of the tile in context.
    let adjacent = matches!(u.data, crate::unique::UniqueData::ImprovementStatsForAdjacencies(_));
    for p in u.data.params() {
        d |= match p {
            Param::CityFilter(f) => filters.city(f).deps(),
            Param::UnitFilter(f) => filters.unit(f).deps(),
            Param::CivFilter(f) => filters.civ(f).deps(),
            Param::CombatantFilter(f) => {
                let c = filters.combatant(f);
                c.unit.deps() | c.city.deps()
            }
            Param::TileFilter(f) if adjacent => tiles::adjacency_deps(filters.tile(f)),
            Param::TileFilter(f) => tile_leaves(filters.tile(f)),
            Param::Object(o) => {
                t.object(o).tiles.map_or(CondDeps::empty(), |f| tile_leaves(filters.tile(f)))
            }
            Param::Countable(c) => c.deps(filters),
            // The city's citizens are the city in context's own; its followers, its religion's.
            Param::Population(
                crate::unique::params::PopulationFilter::FollowersOfThisReligion
                | crate::unique::params::PopulationFilter::FollowersOfTheMajorityReligion,
            ) => CondDeps::RELIGION_STATE,
            _ => CondDeps::empty(),
        };
    }
    d
}

/// What a tile filter reads beyond the tile it is asked about: its leaves' classes, and for
/// `worked` its city's worked tiles (`TILE`).
fn tile_leaves(f: &crate::unique::TileFilter) -> CondDeps {
    let mut d = f.full.deps();
    if f.full.leaves().iter().any(|l| matches!(l, crate::unique::TileLeaf::Worked)) {
        d |= CondDeps::TILE;
    }
    d
}

impl StatsDeps {
    /// The ruleset's.
    #[must_use]
    pub fn of(rules: &Ruleset) -> Self {
        let t = rules.uniques();
        let none = CondDeps::empty();
        let mut out = Self { city: none, civ: none, supply: none, citizens: none };
        for (id, _) in t.iter() {
            let Some(ty) = t.meta(id).ty else { continue };
            let (city, civ, supply, tile, rank) = (
                CITY_TYPES.contains(&ty),
                CIV_TYPES.contains(&ty),
                SUPPLY_TYPES.contains(&ty),
                TILE_TYPES.contains(&ty),
                RANK_TYPES.contains(&ty),
            );
            if !(city || civ || supply || tile || rank) {
                continue;
            }
            let d = unique_deps(rules, id);
            if city {
                out.city |= d;
            }
            if civ || city {
                out.civ |= d;
            }
            if supply {
                out.supply |= d;
            }
            if ranks(rules, id) {
                out.citizens |= d;
            }
        }
        out
    }
}

/// Whether unique `id` can change how a city's citizens rank its tiles and slots
/// (`cities.rank_stats_for_work`): what a tile or a specialist yields, the city's surplus food,
/// its growth, and great person points. Happiness, maintenance and the production bonuses of
/// what it builds cannot, since the ranking reads the happiness committed and the city's food.
fn ranks(rules: &Ruleset, id: UniqueId) -> bool {
    use crate::base::stats::Stat;
    use crate::unique::{Source, UniqueData as D};
    let t = rules.uniques();
    let food = |s| t.stats(s)[Stat::Food] != 0.0;
    // An object filter that names tiles, improvements or a specialist reaches tile yields and
    // specialists; one that names only buildings reaches the city's stats.
    let beyond_buildings = |o| {
        let of = t.object(o);
        of.tiles.is_some() || of.improvements.is_some() || of.specialist.is_some()
    };
    match t.get(id).data {
        D::StatsFromObject(x) => beyond_buildings(x.object) || food(x.stats),
        D::StatPercentFromObject(x) => beyond_buildings(x.object) || x.stat == Stat::Food,
        D::AllStatsPercentFromObject(x) => beyond_buildings(x.object) || x.percent != 0,
        D::Stats(x) => match t.meta(id).source {
            Source::Terrain(_) | Source::Improvement(_) => true,
            Source::Building(_) => food(x.stats),
            _ => false,
        },
        D::StatsPerCity(x) => food(x.stats),
        D::StatsPerPopulation(x) => food(x.stats),
        D::StatsFromCitiesOnSpecificTiles(x) => food(x.stats),
        D::StatsFromBuildings(x) => food(x.stats),
        D::StatsFromTradeRoute(x) => food(x.stats),
        D::StatPercentBonus(x) => x.stat == Stat::Food,
        D::StatPercentBonusCities(x) => x.stat == Stat::Food,
        D::StatPercentFromReligionFollowers(x) => x.stat == Stat::Food,
        D::StatPercentFromTradeRoutes(x) => x.stat == Stat::Food,
        D::BonusStatsFromCityStates(x) => x.stat == Stat::Food,
        D::PercentProductionUnits(_)
        | D::PercentProductionWonders(_)
        | D::PercentProductionBuildings(_)
        | D::PercentProductionBuildingsInCapital(_)
        | D::BuildingMaintenance(_)
        | D::UnhappinessFromCitiesPercentage(_)
        | D::UnhappinessFromPopulationTypePercentageChange(_)
        | D::RemovesAnnexUnhappiness => false,
        ref other => other.ty().is_some_and(|ty| {
            CITY_TYPES.contains(&ty) || TILE_TYPES.contains(&ty) || RANK_TYPES.contains(&ty)
        }),
    }
}

// ---- The memos ----------------------------------------------------------------------------------

/// A tile as a viewer sees it, worked by a city: the key of a yield's table entry.
type Viewing = (TileIdx, Option<PlayerId>, Option<CityId>);

/// A table entry: a tile's yield for a viewer and city other than its owner and its own city.
#[derive(Clone, Copy, Debug)]
struct Entry {
    verified: Rev,
    changed: Rev,
    value: Stats,
    deps: CondDeps,
}

/// The memos of one city.
#[derive(Clone, Debug)]
struct CityMemos {
    mods: Memo<CityMods>,
    /// What the last computation of `mods` read.
    mods_deps: Cell<CondDeps>,
    parts: Memo<CityParts>,
    stats: Memo<CityStats>,
}

impl Default for CityMemos {
    fn default() -> Self {
        Self {
            mods: Memo::new(),
            mods_deps: Cell::new(CondDeps::empty()),
            parts: Memo::new(),
            stats: Memo::new(),
        }
    }
}

/// The memos of one civilization.
#[derive(Clone, Debug, Default)]
struct CivMemos {
    happiness: Memo<Happiness>,
    stats: Memo<CivStats>,
    connectivity: Memo<Connectivity>,
    deficit: CopyMemo<i32>,
}

/// What a read of a city or civilization the game does not have lends.
#[derive(Clone, Debug, Default)]
struct Empty {
    parts: RefCell<CityParts>,
    stats: RefCell<CityStats>,
    happiness: RefCell<Happiness>,
    civ_stats: RefCell<CivStats>,
    connectivity: RefCell<Connectivity>,
}

/// Every memo of this module: part of `Derived`.
#[derive(Clone, Debug)]
pub struct StatsCaches {
    /// Each tile's yield for its owner as its own city works it.
    owned: Vec<CopyMemo<Stats>>,
    /// What each last computation of `owned` read.
    owned_deps: Vec<Cell<CondDeps>>,
    /// Yields for any other viewer and city, by (tile, viewer, city).
    other: RefCell<LookupMap<Viewing, Entry>>,
    cities: LookupMap<CityId, CityMemos>,
    civs: PlayerVec<CivMemos>,
    deps: StatsDeps,
    empty: Empty,
}

impl StatsCaches {
    /// The caches of `st`, cold.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let n = st.tiles().len();
        let mut cities = LookupMap::with_capacity(st.cities().len());
        for c in st.cities().iter() {
            cities.insert(c.id(), CityMemos::default());
        }
        Self {
            owned: (0..n).map(|_| CopyMemo::new()).collect(),
            owned_deps: (0..n).map(|_| Cell::new(CondDeps::empty())).collect(),
            other: RefCell::new(LookupMap::new()),
            cities,
            civs: st.players().ids().map(|_| CivMemos::default()).collect(),
            deps: StatsDeps::of(rules),
            empty: Empty::default(),
        }
    }

    /// What the ruleset's stat uniques read.
    #[must_use]
    pub const fn deps(&self) -> &StatsDeps {
        &self.deps
    }

    /// Keeps one set of memos per city of the state as cities come and go.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.cities.get_or_insert_with(c, CityMemos::default);
            }
            Change::CityRemoved { c, .. } => {
                self.cities.remove(&c);
            }
            _ => {}
        }
    }
}

// ---- Tiles --------------------------------------------------------------------------------------

/// The latest revision of what a tile's yield for `viewer`, worked by `city`, reads: its inputs
/// and its neighbours', which cities exist, the viewer's techs and stocks (and with no city its
/// percentages), the city's modifiers, and the classes the last computation recorded.
fn tile_inputs(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
    deps: CondDeps,
) -> Rev {
    let revs = &g.dv.revs;
    let mut r = revs.tile(t).max(revs.tile_owner(t)).max(revs.cities).max(revs.config);
    for nb in g.grid().neighbors(t) {
        r = r.max(revs.tile(nb));
    }
    if let Some(p) = viewer {
        let c = revs.civ(p);
        r = r.max(c.index).max(c.stocks);
        if city.is_none() {
            r = r.max(civ::civ_index_full_changed(g, p));
        }
    }
    if let Some(c) = city {
        r = r.max(mods_changed(g, c));
    }
    r.max(civ::cond(g, deps, &Ctx { civ: viewer, city, tile: Some(t), ..Ctx::default() }))
}

/// A tile's yield computed, and what it read.
fn compute_tile(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
) -> (Stats, CondDeps) {
    let mut d = CondDeps::empty();
    let mods = city.and_then(|c| city_mods(g, c));
    let s = tiles::compute_tile_yield(g, t, viewer, city, mods.as_deref(), &mut d);
    (s, d)
}

/// What tile `t` yields to `viewer`, worked by `city` (`tiles.tile_stats`, `tiles.py:259-262`),
/// and when that last changed.
fn tile_yield_full(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
) -> (Stats, Rev) {
    let Some(tile) = g.tile(t) else { return (Stats::ZERO, Rev::START) };
    let caches = &g.dv.stats;
    let now = g.dv.revs.now();
    // A city seen by another civilization than its owner: no rule asks, so no memo keeps it.
    if city.is_some_and(|c| g.city(c).map(crate::state::cities::City::owner) != viewer) {
        let mut d = CondDeps::empty();
        let mods = city.map(|c| tiles::city_mods(g, c));
        return (tiles::compute_tile_yield(g, t, viewer, city, mods.as_ref(), &mut d), now);
    }
    if (viewer, city) == (tile.owner(), tile.city())
        && let (Some(m), Some(deps)) =
            (caches.owned.get(t.0 as usize), caches.owned_deps.get(t.0 as usize))
    {
        let v = m.get(
            now,
            || tile_inputs(g, t, viewer, city, deps.get()),
            || {
                let (s, d) = compute_tile(g, t, viewer, city);
                deps.set(d);
                s
            },
        );
        return (v, m.changed());
    }
    let key = (t, viewer, city);
    let cur = caches.other.borrow().get(&key).copied();
    if let Some(e) = cur {
        if e.verified == now {
            return (e.value, e.changed);
        }
        if tile_inputs(g, t, viewer, city, e.deps) <= e.verified && e.verified != Rev::NEVER {
            caches.other.borrow_mut().insert(key, Entry { verified: now, ..e });
            return (e.value, e.changed);
        }
    }
    let (s, d) = compute_tile(g, t, viewer, city);
    let changed = match cur {
        Some(e) if e.value.bit_eq(&s) => e.changed,
        _ => now,
    };
    caches.other.borrow_mut().insert(key, Entry { verified: now, changed, value: s, deps: d });
    (s, changed)
}

/// What tile `t` yields to `viewer`, worked by `city` (`tiles.tile_stats`): from the tile's memo
/// for its owner and its own city, else from the table.
#[must_use]
pub fn tile_yield(g: &Game, t: TileIdx, viewer: Option<PlayerId>, city: Option<CityId>) -> Stats {
    tile_yield_full(g, t, viewer, city).0
}

// ---- Cities -------------------------------------------------------------------------------------

/// City `c`'s tile modifiers (`CityMods`); `None` for a city the game does not have.
pub(crate) fn city_mods(g: &Game, c: CityId) -> Option<Ref<'_, CityMods>> {
    let m = g.dv.stats.cities.get(&c)?;
    let city = g.city(c)?;
    let owner = city.owner();
    let revs = &g.dv.revs;
    let inputs = || {
        let cr = revs.city(c);
        civ::city_local_full_changed(g, c)
            .max(civ::civ_index_full_changed(g, owner))
            .max(cr.core)
            .max(cr.buildings)
            .max(cr.religion)
            .max(revs.religions)
            .max(revs.config)
            .max(civ::cond(g, m.mods_deps.get(), &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let x = tiles::city_mods(g, c);
        m.mods_deps.set(x.deps);
        x
    };
    Some(m.mods.get(revs.now(), inputs, compute))
}

/// When city `c`'s tile modifiers last changed, validated now.
fn mods_changed(g: &Game, c: CityId) -> Rev {
    drop(city_mods(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.mods.changed())
}

/// City `c`'s parts and happiness (`CityHappiness`, `cities._city_happiness`).
///
/// # Panics
/// Never for a city of the game; an empty value stands in for one it does not have.
pub(crate) fn city_parts(g: &Game, c: CityId) -> Ref<'_, CityParts> {
    let Some(m) = g.dv.stats.cities.get(&c) else { return g.dv.stats.empty.parts.borrow() };
    let revs = &g.dv.revs;
    let verified = m.parts.stamp().verified();
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let o = revs.civ(owner);
        let mut r = revs
            .city(c)
            .max()
            .max(civ::city_local_full_changed(g, c))
            .max(civ::civ_index_full_changed(g, owner))
            .max(o.index)
            .max(o.seat)
            .max(o.cities)
            .max(revs.cities)
            .max(revs.religions)
            .max(revs.config);
        for t in cstats::worked_or_free_tiles(g, c, &city.worked) {
            r = r.max(tile_yield_full(g, t, Some(owner), Some(c)).1);
        }
        // Whether a tile of the city yields without a citizen, on the tiles it owns.
        let Some(changed) = revs.tile_log.since(verified) else { return revs.now() };
        for t in changed {
            if g.tile(t).and_then(crate::state::map::Tile::city) == Some(c) {
                r = r.max(revs.tile(t));
            }
        }
        r.max(civ::cond(g, g.dv.stats.deps.city, &Ctx::city(&g.view(), c)))
    };
    let compute =
        || g.city(c).map(|city| cstats::city_parts(g, c, &Work::of(city))).unwrap_or_default();
    m.parts.get(revs.now(), inputs, compute)
}

/// When city `c`'s parts last changed, validated now.
fn parts_changed(g: &Game, c: CityId) -> Rev {
    drop(city_parts(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.parts.changed())
}

/// City `c`'s stats (`CityStats`, `cities.city_stats(g, city)`), as it builds what it builds.
pub(crate) fn city_stats(g: &Game, c: CityId) -> Ref<'_, CityStats> {
    let Some(m) = g.dv.stats.cities.get(&c) else { return g.dv.stats.empty.stats.borrow() };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let o = revs.civ(owner);
        let mut r = parts_changed(g, c)
            .max(revs.city(c).max())
            .max(o.index)
            .max(o.stocks)
            .max(o.seat)
            .max(revs.cities)
            .max(revs.religions)
            .max(revs.config)
            .max(civ::city_local_full_changed(g, c))
            .max(civ::civ_index_full_changed(g, owner))
            .max(connectivity_changed(g, owner))
            .max(deficit_changed(g, owner));
        if let Some(cap) = g.player(owner).and_then(|p| p.capital) {
            let cr = revs.city(cap);
            r = r.max(cr.core).max(cr.buildings);
        }
        if city.wltkd > 0 && g.player(owner).is_some_and(crate::state::players::Player::is_major) {
            r = r.max(happiness_changed(g, owner));
        }
        r.max(civ::cond(g, g.dv.stats.deps.city, &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let Some(city) = g.city(c) else { return CityStats::default() };
        let parts = city_parts(g, c);
        let work = Work::of(city);
        cstats::city_stats_from(g, c, &parts, &work, cstats::current_construction(city), None)
    };
    m.stats.get(revs.now(), inputs, compute)
}

/// When city `c`'s stats last changed, validated now.
fn stats_changed(g: &Game, c: CityId) -> Rev {
    drop(city_stats(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.stats.changed())
}

// ---- Civilizations ------------------------------------------------------------------------------

/// What every civilization-level memo reads of civilization `p` and the world, coarsely.
fn civ_inputs(g: &Game, p: PlayerId) -> Rev {
    let revs = &g.dv.revs;
    let mut r = revs
        .civ(p)
        .max()
        .max(revs.cities)
        .max(revs.routes)
        .max(revs.tile_log.rev())
        .max(revs.alliances)
        .max(revs.religions)
        .max(revs.turn)
        .max(revs.diplo)
        .max(revs.config);
    // Its religion's founder beliefs count the cities and followers of the world.
    if g.player(p).is_some_and(|x| x.religion.founded.is_some()) {
        r = r.max(revs.city_religion).max(revs.city_core);
    }
    r.max(civ::cond(g, g.dv.stats.deps.civ, &Ctx::civ(p)))
}

/// Civilization `p`'s happiness (`Happiness`, `economy.happiness`).
pub(crate) fn happiness(g: &Game, p: PlayerId) -> Ref<'_, Happiness> {
    let Some(m) = g.dv.stats.civs.get(p) else { return g.dv.stats.empty.happiness.borrow() };
    let inputs = || {
        let mut r = civ_inputs(g, p).max(civ::supply_changed(g, p));
        for &c in g.state().cities().of(p) {
            r = r.max(parts_changed(g, c));
        }
        r
    };
    m.happiness.get(g.dv.revs.now(), inputs, || economy::compute_happiness(g, p))
}

/// Civilization `p`'s happiness total.
#[must_use]
pub fn happiness_total(g: &Game, p: PlayerId) -> i32 {
    happiness(g, p).total
}

fn happiness_changed(g: &Game, p: PlayerId) -> Rev {
    drop(happiness(g, p));
    g.dv.stats.civs.get(p).map_or(Rev::START, |m| m.happiness.changed())
}

/// Civilization `p`'s stats for the next turn (`CivStats`, `economy.stat_map` and `civ_stats`).
pub(crate) fn civ_stats(g: &Game, p: PlayerId) -> Ref<'_, CivStats> {
    let Some(m) = g.dv.stats.civs.get(p) else { return g.dv.stats.empty.civ_stats.borrow() };
    let inputs = || {
        let revs = &g.dv.revs;
        let mut r = civ_inputs(g, p).max(happiness_changed(g, p)).max(revs.units_core);
        for &c in g.state().cities().of(p) {
            r = r.max(stats_changed(g, c));
        }
        for cs in economy::allied_city_states(g, p) {
            r = r.max(civ_stats_changed(g, cs));
        }
        r
    };
    m.stats.get(g.dv.revs.now(), inputs, || economy::compute_civ_stats(g, p))
}

fn civ_stats_changed(g: &Game, p: PlayerId) -> Rev {
    drop(civ_stats(g, p));
    g.dv.stats.civs.get(p).map_or(Rev::START, |m| m.stats.changed())
}

/// How civilization `p`'s cities are linked to its capital (`Connectivity`,
/// `cities.connected_cities`).
pub(crate) fn connectivity(g: &Game, p: PlayerId) -> Ref<'_, Connectivity> {
    let Some(m) = g.dv.stats.civs.get(p) else { return g.dv.stats.empty.connectivity.borrow() };
    let inputs = || {
        let revs = &g.dv.revs;
        revs.routes
            .max(revs.cities)
            .max(revs.diplo)
            .max(revs.turn)
            .max(revs.civ(p).index)
            .max(revs.city_buildings)
            .max(revs.owners)
            .max(revs.tile_log.rev())
            .max(revs.config)
            .max(civ::cond(g, g.dv.stats.deps.civ, &Ctx::civ(p)))
    };
    m.connectivity.get(g.dv.revs.now(), inputs, || connections::connected_cities(g, p))
}

fn connectivity_changed(g: &Game, p: PlayerId) -> Rev {
    drop(connectivity(g, p));
    g.dv.stats.civs.get(p).map_or(Rev::START, |m| m.connectivity.changed())
}

/// Whether a city has a trade route to its capital (`cities.connected_to_capital`,
/// `cities.py:2067-2076`): its owner has another city, and it is linked.
#[must_use]
pub fn connected_to_capital(g: &Game, c: CityId) -> bool {
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return false };
    if g.state().cities().of(owner).len() < 2 {
        return false;
    }
    connectivity(g, owner).media(c).is_some()
}

/// Whether a city is linked to its capital by railroad (`connected_to_capital(rail=True)`).
#[must_use]
pub fn connected_by_rail(g: &Game, c: CityId) -> bool {
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return false };
    if g.state().cities().of(owner).len() < 2 {
        return false;
    }
    connectivity(g, owner).media(c).is_some_and(|m| m.contains(Media::RAILROAD))
}

/// How far over its unit supply a civilization is (`economy.unit_supply_deficit`,
/// `economy.py:587-594`), as a memo, with when it last changed: its units, its cities and their
/// population, its seat, and the supply uniques.
fn deficit(g: &Game, p: PlayerId) -> (i32, Rev) {
    let Some(m) = g.dv.stats.civs.get(p) else { return (0, Rev::START) };
    let inputs = || {
        let revs = &g.dv.revs;
        let o = revs.civ(p);
        let mut r = o.roster.max(o.cities).max(o.index).max(o.seat).max(revs.config);
        for &c in g.state().cities().of(p) {
            r = r.max(revs.city(c).core);
        }
        r.max(civ::cond(g, g.dv.stats.deps.supply, &Ctx::civ(p)))
    };
    let v = m.deficit.get(g.dv.revs.now(), inputs, || economy::unit_supply_deficit(g, p));
    (v, m.deficit.changed())
}

/// How far over its unit supply a civilization is (`economy.unit_supply_deficit`), from its
/// memo.
#[must_use]
pub fn unit_supply_deficit(g: &Game, p: PlayerId) -> i32 {
    deficit(g, p).0
}

fn deficit_changed(g: &Game, p: PlayerId) -> Rev {
    deficit(g, p).1
}

/// The production penalty of units over the supply, in percent (`economy.unit_supply_penalty`).
#[must_use]
pub fn unit_supply_penalty(g: &Game, p: PlayerId) -> f64 {
    -(f64::from(unit_supply_deficit(g, p)) * 10.0).min(70.0)
}

// ---- The cache oracle (DESIGN.md 9.4) ----------------------------------------------------------

/// Every memo of this module, validated, against a cold rebuild from the same state: one line
/// for each that disagrees.
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
    for (t, tile) in g.st.tiles().iter() {
        let (viewer, city) = (tile.owner(), tile.city());
        let (a, b) = (tile_yield(g, t, viewer, city), tile_yield(&cold, t, viewer, city));
        if !a.bit_eq(&b) {
            out.push(format!("tile {t}: its yield {a:?} differs from a cold rebuild {b:?}"));
        }
    }
    for city in g.st.cities().iter() {
        let c = city.id();
        if !g.dv.stats.cities.contains_key(&c) {
            out.push(format!("city {}: no stats memos", c.get()));
            continue;
        }
        let mods = city_mods(g, c).map(|x| x.clone());
        if mods != city_mods(&cold, c).map(|x| x.clone()) {
            out.push(format!("city {}: its tile modifiers differ from a cold rebuild", c.get()));
        }
        if !city_parts(g, c).bit_eq(&city_parts(&cold, c)) {
            out.push(format!(
                "city {}: its happiness and parts differ from a cold rebuild",
                c.get()
            ));
        }
        if !city_stats(g, c).bit_eq(&city_stats(&cold, c)) {
            out.push(format!("city {}: its stats differ from a cold rebuild", c.get()));
        }
    }
    for p in g.st.players().ids() {
        if !happiness(g, p).bit_eq(&happiness(&cold, p)) {
            out.push(format!("player {}: its happiness differs from a cold rebuild", p.0));
        }
        if !civ_stats(g, p).bit_eq(&civ_stats(&cold, p)) {
            out.push(format!("player {}: its stats differ from a cold rebuild", p.0));
        }
        if *connectivity(g, p) != *connectivity(&cold, p) {
            out.push(format!("player {}: its connectivity differs from a cold rebuild", p.0));
        }
        if unit_supply_deficit(g, p) != unit_supply_deficit(&cold, p) {
            out.push(format!("player {}: its supply deficit differs from a cold rebuild", p.0));
        }
    }
    out
}
