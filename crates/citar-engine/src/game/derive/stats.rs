//! The memos of yields, stats, happiness and connectivity (DESIGN.md 6.5): `CityMods` and
//! `TileYield` (tiles), `CityHappiness` and `CityStats` (cities), `Happiness`, `CivStats`, unit
//! upkeep, `Connectivity` and the unit-supply deficit (civilizations).
//!
//! Replaces the entries of Python's yield cache (`tstats`, `chappy`, `cstats`, `happiness`,
//! `stat_map`, `civ_stats`, `connected`, `supply_deficit`; `game.py:565-609`), which every write
//! threw away: 98.7% of the tile yields Python recomputed were unchanged. Each memo validates
//! itself on read against the revisions of what it read, the memos upstream of it, and the
//! classes of the conditionals and filters its last computation evaluated, which it records
//! (`unique::record`): a memo whose uniques did not read the units is not recomputed when one
//! moves, whatever other uniques of the ruleset read.
//! - a tile's yield, for its owner as its own city works it: its own inputs and its neighbours'
//!   (a river's fresh water, the coast, an improvement's adjacency), its owner's techs and whether
//!   it is in a golden age, which cities exist, its city's modifiers (`CityMods`), and its
//!   classes. A tile seen by another civilization, or worked by another city, has a table entry
//!   validated the same way, dropped with its city and at each turn;
//! - a city's modifiers: its own index, its owner's, its majority religion's, and its classes;
//! - a city's parts (`CityHappiness`): the yields of the tiles its last computation added (its
//!   centre, the tiles it works, those that yield without a citizen), the city and the tiles it
//!   owns, its owner's index, seat and cities, and its classes;
//! - a city's stats: its parts, its owner's connectivity, supply deficit, golden age and, in We
//!   Love The King Day, happiness; its capital; its classes;
//! - a civilization's happiness and stats: its cities', its supply and index, its routes and
//!   tiles, deals and religion, its unit upkeep (a memo of its own), and its classes, the local
//!   ones over its cities, tiles and units; its connectivity: routes, cities, borders, techs,
//!   harbours and the classes of its `Forests and Jungles are roads`.
//!
//! So a unit's move or heal, a city working other tiles, a city growing or a treasury filling
//! recomputes none of them unless a unique they evaluated reads it. [`verify`] is the cache
//! oracle for them all.

use core::cell::{Cell, Ref, RefCell};

use smallvec::SmallVec;

use super::civ;
use super::rev::{BitEq, CopyMemo, Memo, Rev};
use crate::base::collections::{DetMap, LookupMap};
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId};
use crate::base::sets::PlayerVec;
use crate::base::stats::Stats;
use crate::game::Game;
use crate::game::cities::connections::{self, Connectivity, Media};
use crate::game::cities::stats::{self as cstats, CityBase, CityParts, CityStats, Work};
use crate::game::economy::{self, CivStats, Happiness};
use crate::game::tiles::{self, CityMods};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::{CondDeps, Ctx, UniqueType, record};

// ---- What citizen ranking reads (the ruleset's) ---------------------------------------------------

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

/// What citizen ranking reads over the whole ruleset (DESIGN.md 6.7): what a write must move for
/// a city's citizens to be looked at again. The memos record what they read themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsDeps {
    /// The classes the conditionals and filters of the uniques that can change a ranking read:
    /// tile yields, a city's food and growth, great person points. With `CONNECTED`, what the
    /// trade network reads besides.
    pub citizens: CondDeps,
    /// Whether a unique gives more great person points for a declared friendship, which runs
    /// out on a turn (`great_people.py:24-31`).
    pub friendship: bool,
}

/// What the trade network to a capital reads besides its `Forests and Jungles are roads`
/// (`cities.py:1967-2067`): the routes and tiles, the cities and their harbours, borders and war,
/// techs, and the turn that open borders run out on.
const NETWORK: CondDeps = CondDeps::MAP
    .union(CondDeps::WAR)
    .union(CondDeps::TURN)
    .union(CondDeps::TECHS)
    .union(CondDeps::GLOBAL_BUILDINGS)
    .union(CondDeps::CITY_COUNT)
    .union(CondDeps::CONFIG);

impl StatsDeps {
    /// The ruleset's.
    #[must_use]
    pub fn of(rules: &Ruleset) -> Self {
        let t = rules.uniques();
        let mut citizens = CondDeps::empty();
        let mut network = NETWORK;
        let mut friendship = false;
        for (id, _) in t.iter() {
            let Some(ty) = t.meta(id).ty else { continue };
            friendship |= ty == UniqueType::GreatPersonBoostWithFriendship;
            if ty == UniqueType::ForestsAndJunglesAreRoads {
                network |= t.reads(id);
            }
            let read =
                CITY_TYPES.contains(&ty) || TILE_TYPES.contains(&ty) || RANK_TYPES.contains(&ty);
            if read && ranks(rules, id) {
                citizens |= t.reads(id);
            }
        }
        if citizens.contains(CondDeps::CONNECTED) {
            citizens |= network.difference(CondDeps::CONNECTED);
        }
        Self { citizens, friendship }
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

/// The memos of one city, each with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct CityMemos {
    mods: Memo<CityMods>,
    mods_deps: Cell<CondDeps>,
    base: Memo<CityBase>,
    base_deps: Cell<CondDeps>,
    parts: Memo<CityParts>,
    parts_deps: Cell<CondDeps>,
    /// The tiles whose yields the last computation of `parts` added: its centre, the tiles it
    /// worked and those of its own that yield without a citizen.
    parts_tiles: RefCell<SmallVec<[TileIdx; 24]>>,
    stats: Memo<CityStats>,
    stats_deps: Cell<CondDeps>,
}

impl Default for CityMemos {
    fn default() -> Self {
        let none = || Cell::new(CondDeps::empty());
        Self {
            mods: Memo::new(),
            mods_deps: none(),
            base: Memo::new(),
            base_deps: none(),
            parts: Memo::new(),
            parts_deps: none(),
            parts_tiles: RefCell::new(SmallVec::new()),
            stats: Memo::new(),
            stats_deps: none(),
        }
    }
}

/// The memos of one civilization, each with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct CivMemos {
    happiness: Memo<Happiness>,
    happiness_deps: Cell<CondDeps>,
    stats: Memo<CivStats>,
    stats_deps: Cell<CondDeps>,
    upkeep: CopyMemo<i32>,
    upkeep_deps: Cell<CondDeps>,
    connectivity: Memo<Connectivity>,
    connectivity_deps: Cell<CondDeps>,
    deficit: CopyMemo<i32>,
    deficit_deps: Cell<CondDeps>,
}

impl Default for CivMemos {
    fn default() -> Self {
        let none = || Cell::new(CondDeps::empty());
        Self {
            happiness: Memo::new(),
            happiness_deps: none(),
            stats: Memo::new(),
            stats_deps: none(),
            upkeep: CopyMemo::new(),
            upkeep_deps: none(),
            connectivity: Memo::new(),
            connectivity_deps: none(),
            deficit: CopyMemo::new(),
            deficit_deps: none(),
        }
    }
}

/// What a read of a city or civilization the game does not have lends.
#[derive(Clone, Debug, Default)]
struct Empty {
    base: RefCell<CityBase>,
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
    /// Yields for any other viewer and city, by (tile, viewer, city), in the order they were
    /// first asked: the oracle walks them, and a city's go with it.
    other: RefCell<DetMap<Viewing, Entry>>,
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
            other: RefCell::new(DetMap::default()),
            cities,
            civs: st.players().ids().map(|_| CivMemos::default()).collect(),
            deps: StatsDeps::of(rules),
            empty: Empty::default(),
        }
    }

    /// What citizen ranking reads in the ruleset.
    #[must_use]
    pub const fn deps(&self) -> &StatsDeps {
        &self.deps
    }

    /// Keeps one set of memos per city of the state as cities come and go. The table of yields
    /// for other viewers and cities loses a city's entries with it, and is emptied at each turn
    /// (DESIGN.md 6.5), so it holds what one turn asked.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.cities.get_or_insert_with(c, CityMemos::default);
            }
            Change::CityRemoved { c, .. } => {
                self.cities.remove(&c);
                self.other.get_mut().retain(|k, _| k.2 != Some(c));
            }
            Change::Turn => self.other.get_mut().clear(),
            _ => {}
        }
    }
}

// ---- Tiles --------------------------------------------------------------------------------------

/// The latest revision of what a tile's yield for `viewer`, worked by `city`, reads: its inputs
/// and its neighbours', which cities exist, the viewer's techs and golden age (and with no city
/// its percentages), the city's modifiers, and the classes the last computation recorded.
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
        r = r.max(c.index).max(c.golden_age);
        if city.is_none() {
            r = r.max(civ::civ_index_full_changed(g, p));
        }
    }
    if let Some(c) = city {
        r = r.max(mods_changed(g, c));
    }
    r.max(civ::cond(g, deps, &Ctx { civ: viewer, city, tile: Some(t), ..Ctx::default() }))
}

/// A tile's yield computed, and what it read. What it read is its own (`tiles` records it by
/// hand), so none of it reaches a computation that records what it reads.
fn compute_tile(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
) -> (Stats, CondDeps) {
    record::isolated(|| {
        let mut d = CondDeps::empty();
        let mods = city.and_then(|c| city_mods(g, c));
        let s = tiles::compute_tile_yield(g, t, viewer, city, mods.as_deref(), &mut d);
        (s, d)
    })
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
        let s = record::isolated(|| {
            let mut d = CondDeps::empty();
            let mods = city.map(|c| tiles::city_mods(g, c));
            tiles::compute_tile_yield(g, t, viewer, city, mods.as_ref(), &mut d)
        });
        return (s, now);
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
        let input = record::isolated(|| tile_inputs(g, t, viewer, city, e.deps));
        if input <= e.verified && e.verified != Rev::NEVER {
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

/// [`tile_yield`], and the classes its last computation read: what a what-if of one more
/// building asks before it reuses the yield (`cities::what_if`).
#[must_use]
pub(crate) fn tile_yield_read(
    g: &Game,
    t: TileIdx,
    viewer: Option<PlayerId>,
    city: Option<CityId>,
) -> (Stats, CondDeps) {
    let (s, _) = tile_yield_full(g, t, viewer, city);
    let caches = &g.dv.stats;
    let Some(tile) = g.tile(t) else { return (s, CondDeps::empty()) };
    if city.is_some_and(|c| g.city(c).map(crate::state::cities::City::owner) != viewer) {
        return compute_tile(g, t, viewer, city);
    }
    if (viewer, city) == (tile.owner(), tile.city())
        && let Some(deps) = caches.owned_deps.get(t.0 as usize)
    {
        return (s, deps.get());
    }
    let deps = caches.other.borrow().get(&(t, viewer, city)).map_or(CondDeps::all(), |e| e.deps);
    (s, deps)
}

// ---- Cities -------------------------------------------------------------------------------------

/// What a city's yields read of the city itself: its buildings, population, status and queue,
/// where its citizens work, and its religion. Not its stored food, culture and production, which
/// only a conditional about the city reads (the city class).
fn city_itself(cr: &super::rev::CityRevs) -> Rev {
    cr.core.max(cr.buildings).max(cr.work).max(cr.religion)
}

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

/// The classes the last computation of city `c`'s tile modifiers read, validated now.
pub(crate) fn city_mods_deps(g: &Game, c: CityId) -> CondDeps {
    drop(city_mods(g, c));
    g.dv.stats.cities.get(&c).map_or(CondDeps::all(), |m| m.mods_deps.get())
}

/// Every class the memos of city `c`'s yields and happiness read, validated now: its tile
/// modifiers', base's, parts' and stats' own, and those of the tiles its parts added. A what-if of
/// a building elsewhere reuses its parts when none of these moves.
pub(crate) fn city_reads(g: &Game, c: CityId) -> CondDeps {
    drop(city_stats(g, c));
    let Some(m) = g.dv.stats.cities.get(&c) else { return CondDeps::all() };
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else {
        return CondDeps::all();
    };
    let mut out =
        city_mods_deps(g, c) | m.base_deps.get() | m.parts_deps.get() | m.stats_deps.get();
    let tiles = m.parts_tiles.borrow().clone();
    for t in tiles {
        out |= tile_yield_read(g, t, Some(owner), Some(c)).1;
    }
    out
}

/// When city `c`'s tile modifiers last changed, validated now.
fn mods_changed(g: &Game, c: CityId) -> Rev {
    drop(city_mods(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.mods.changed())
}

/// What city `c`'s yields are made of that does not depend on where its citizens work
/// ([`CityBase`]): valid while its buildings, size, status and religion, its territory, its
/// indexes and what its classes read stand, so a reassignment leaves it.
pub(crate) fn city_base(g: &Game, c: CityId) -> Ref<'_, CityBase> {
    let Some(m) = g.dv.stats.cities.get(&c) else { return g.dv.stats.empty.base.borrow() };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let o = revs.civ(owner);
        let cr = revs.city(c);
        cr.core
            .max(cr.buildings)
            .max(cr.religion)
            .max(cr.tiles)
            .max(civ::city_local_full_changed(g, c))
            .max(civ::civ_index_full_changed(g, owner))
            .max(o.index)
            .max(o.seat)
            .max(revs.cities)
            .max(revs.religions)
            .max(revs.config)
            .max(civ::cond(g, m.base_deps.get(), &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let (base, d) = record::recorded(|| cstats::city_base(g, c));
        m.base_deps.set(d);
        base
    };
    m.base.get(revs.now(), inputs, compute)
}

fn base_changed(g: &Game, c: CityId) -> Rev {
    drop(city_base(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.base.changed())
}

/// City `c`'s parts and happiness (`CityHappiness`, `cities._city_happiness`).
///
/// # Panics
/// Never for a city of the game; an empty value stands in for one it does not have.
pub(crate) fn city_parts(g: &Game, c: CityId) -> Ref<'_, CityParts> {
    let Some(m) = g.dv.stats.cities.get(&c) else { return g.dv.stats.empty.parts.borrow() };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let o = revs.civ(owner);
        let cr = revs.city(c);
        // The tiles that yield without a citizen are its own (`tiles`) and change with a tile
        // bought or lost (`work`), so the list the last computation added still holds.
        let mut r = city_itself(&cr)
            .max(cr.tiles)
            .max(base_changed(g, c))
            .max(civ::city_local_full_changed(g, c))
            .max(civ::civ_index_full_changed(g, owner))
            .max(o.index)
            .max(o.seat)
            .max(o.cities)
            .max(revs.cities)
            .max(revs.religions)
            .max(revs.config);
        for &t in m.parts_tiles.borrow().iter() {
            r = r.max(tile_yield_full(g, t, Some(owner), Some(c)).1);
        }
        r.max(civ::cond(g, m.parts_deps.get(), &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let Some(city) = g.city(c) else { return CityParts::default() };
        // Its free tiles are its base's, which the validation reads first.
        let tiles = cstats::worked_or_free_tiles(g, c, &city.worked);
        let (parts, d) = record::recorded(|| cstats::city_parts_on(g, c, &Work::of(city), &tiles));
        m.parts_deps.set(d);
        *m.parts_tiles.borrow_mut() = tiles;
        parts
    };
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
            .max(city_itself(&revs.city(c)))
            .max(o.index)
            .max(o.golden_age)
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
        r.max(civ::cond(g, m.stats_deps.get(), &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let Some(city) = g.city(c) else { return CityStats::default() };
        let parts = city_parts(g, c);
        let work = Work::of(city);
        let (stats, d) = record::recorded(|| {
            cstats::city_stats_from(g, c, &parts, &work, cstats::current_construction(city), None)
        });
        m.stats_deps.set(d);
        stats
    };
    m.stats.get(revs.now(), inputs, compute)
}

/// When city `c`'s stats last changed, validated now.
fn stats_changed(g: &Game, c: CityId) -> Rev {
    drop(city_stats(g, c));
    g.dv.stats.cities.get(&c).map_or(Rev::START, |m| m.stats.changed())
}

// ---- Civilizations ------------------------------------------------------------------------------

/// What the classes `deps` a civilization-level computation of `p` recorded read: the
/// civilization's classes as [`civ::cond`] maps them, and the local ones over what of `p` such a
/// computation evaluates them with (a filter asked of its cities, route upkeep asked of its tiles,
/// unit upkeep of its units): its cities' own revisions (`CITY`), its tiles' inputs and owners and
/// the tiles its cities work (`TILE`), its units (`UNIT`), and all of them for a fight.
fn civ_level_cond(g: &Game, deps: CondDeps, p: PlayerId) -> Rev {
    let mut r = civ::cond(g, deps, &Ctx::civ(p));
    let local = deps.intersection(CondDeps::LOCAL);
    if local.is_empty() {
        return r;
    }
    let revs = &g.dv.revs;
    let fight = local.contains(CondDeps::COMBAT);
    let city = fight || local.contains(CondDeps::CITY);
    let tile = fight || local.contains(CondDeps::TILE);
    if city || tile {
        r = r.max(revs.cities).max(revs.civ(p).cities);
        for &c in g.state().cities().of(p) {
            let cr = revs.city(c);
            r = r.max(if city { cr.max() } else { cr.work });
        }
    }
    if tile {
        r = r.max(revs.tile_log.rev()).max(revs.owners);
    }
    if fight || local.contains(CondDeps::UNIT) {
        let o = revs.civ(p);
        r = r.max(o.units).max(o.roster);
        for &u in g.state().units().of(p) {
            r = r.max(revs.unit(u).max());
        }
    }
    r
}

/// What every civilization-level memo of `p` last verified at `verified` reads of it and the
/// world, besides its cities' memos and the classes it recorded: its index with the resource
/// layer, its stocks (and the natural wonders it found), golden age, seat, cities and tiles,
/// buildings and religion; the tiles of `owners` changed since (route upkeep,
/// and an allied city-state's luxuries); which cities exist, alliances, religions, the turn,
/// relations and deals, the settings; and the cities and followers of the world when its
/// religion's founder beliefs count them. A tile that changed hands moved both sides' `cities`.
fn civ_inputs(g: &Game, p: PlayerId, owners: &[PlayerId], verified: Rev) -> Rev {
    let revs = &g.dv.revs;
    let o = revs.civ(p);
    let mut r = o
        .index
        .max(o.stocks)
        .max(o.golden_age)
        .max(o.seat)
        .max(o.cities)
        .max(o.buildings)
        .max(o.religion)
        .max(civ::civ_index_full_changed(g, p))
        .max(revs.cities)
        .max(revs.alliances)
        .max(revs.religions)
        .max(revs.turn)
        .max(revs.diplo)
        .max(revs.config);
    if g.player(p).is_some_and(|x| x.religion.founded.is_some()) {
        r = r.max(revs.city_religion).max(revs.city_core);
    }
    let Some(changed) = revs.tile_log.since(verified) else { return revs.now() };
    for t in changed {
        if g.tile(t).and_then(crate::state::map::Tile::owner).is_some_and(|x| owners.contains(&x)) {
            r = r.max(revs.tile(t));
        }
    }
    r
}

/// Civilization `p`'s happiness (`Happiness`, `economy.happiness`).
pub(crate) fn happiness(g: &Game, p: PlayerId) -> Ref<'_, Happiness> {
    let Some(m) = g.dv.stats.civs.get(p) else { return g.dv.stats.empty.happiness.borrow() };
    let inputs = || {
        let revs = &g.dv.revs;
        // The luxuries of its allied city-states, read as their supply reads them.
        let mut owners: SmallVec<[PlayerId; 4]> = SmallVec::from_elem(p, 1);
        owners.extend(economy::allied_city_states(g, p));
        let verified = m.happiness.stamp().verified();
        let mut r = civ_inputs(g, p, &owners, verified).max(civ::supply_changed(g, p));
        for &cs in &owners[1..] {
            let x = revs.civ(cs);
            r = r.max(x.index).max(x.cities).max(x.buildings).max(x.city_state).max(x.roster);
        }
        for &c in g.state().cities().of(p) {
            r = r.max(parts_changed(g, c));
        }
        r.max(civ_level_cond(g, m.happiness_deps.get(), p))
    };
    let compute = || {
        let (h, d) = record::recorded(|| economy::compute_happiness(g, p));
        m.happiness_deps.set(d);
        h
    };
    m.happiness.get(g.dv.revs.now(), inputs, compute)
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
        let verified = m.stats.stamp().verified();
        let mut r =
            civ_inputs(g, p, &[p], verified).max(happiness_changed(g, p)).max(upkeep_changed(g, p));
        for &c in g.state().cities().of(p) {
            r = r.max(stats_changed(g, c));
        }
        for cs in economy::allied_city_states(g, p) {
            r = r.max(civ_stats_changed(g, cs));
        }
        r.max(civ_level_cond(g, m.stats_deps.get(), p))
    };
    let compute = || {
        let (s, d) = record::recorded(|| economy::compute_civ_stats(g, p));
        m.stats_deps.set(d);
        s
    };
    m.stats.get(g.dv.revs.now(), inputs, compute)
}

fn civ_stats_changed(g: &Game, p: PlayerId) -> Rev {
    drop(civ_stats(g, p));
    g.dv.stats.civs.get(p).map_or(Rev::START, |m| m.stats.changed())
}

/// The gold civilization `p` pays for its units each turn (`economy.unit_maintenance`), as a memo
/// with when it last changed: which units it has and of which kind, its index, seat and the turn
/// (the game's progress), the settings, and the classes its last computation recorded (a unit's
/// own conditionals, and where its units stand when those in cities are free).
fn upkeep(g: &Game, p: PlayerId) -> (i32, Rev) {
    let Some(m) = g.dv.stats.civs.get(p) else { return (0, Rev::START) };
    let inputs = || {
        let revs = &g.dv.revs;
        let o = revs.civ(p);
        // Which cities exist: whether a unit stands in one, where those in cities are free.
        o.roster
            .max(o.index)
            .max(o.seat)
            .max(civ::civ_index_full_changed(g, p))
            .max(revs.cities)
            .max(revs.turn)
            .max(revs.config)
            .max(civ_level_cond(g, m.upkeep_deps.get(), p))
    };
    let compute = || {
        let (v, d) = record::recorded(|| economy::unit_maintenance(g, p));
        m.upkeep_deps.set(d);
        v
    };
    let v = m.upkeep.get(g.dv.revs.now(), inputs, compute);
    (v, m.upkeep.changed())
}

/// The gold civilization `p` pays for its units each turn, from its memo.
#[must_use]
pub fn unit_upkeep(g: &Game, p: PlayerId) -> i32 {
    upkeep(g, p).0
}

fn upkeep_changed(g: &Game, p: PlayerId) -> Rev {
    upkeep(g, p).1
}

/// How civilization `p`'s cities are linked to its capital (`Connectivity`,
/// `cities.connected_cities`).
pub(crate) fn connectivity(g: &Game, p: PlayerId) -> Ref<'_, Connectivity> {
    let Some(m) = g.dv.stats.civs.get(p) else { return g.dv.stats.empty.connectivity.borrow() };
    let inputs = || {
        let revs = &g.dv.revs;
        // A `Forests and Jungles are roads` whose conditional asked for the network itself would
        // be a cycle in the rules: the network it reads is this one.
        let deps = m.connectivity_deps.get().difference(CondDeps::CONNECTED);
        revs.routes
            .max(revs.cities)
            .max(revs.diplo)
            .max(revs.turn)
            .max(revs.civ(p).index)
            .max(civ::civ_index_full_changed(g, p))
            .max(revs.city_buildings)
            .max(revs.owners)
            .max(revs.tile_log.rev())
            .max(revs.config)
            .max(civ::cond(g, deps, &Ctx::civ(p)))
    };
    let compute = || {
        let (x, d) = record::recorded(|| connections::connected_cities(g, p));
        m.connectivity_deps.set(d);
        x
    };
    m.connectivity.get(g.dv.revs.now(), inputs, compute)
}

/// The classes the last computation of civilization `p`'s connectivity read, validated now.
pub(crate) fn connectivity_deps(g: &Game, p: PlayerId) -> CondDeps {
    drop(connectivity(g, p));
    g.dv.stats.civs.get(p).map_or(CondDeps::all(), |m| m.connectivity_deps.get())
}

/// The classes the last computation of civilization `p`'s unit supply deficit read, validated
/// now.
pub(crate) fn deficit_deps(g: &Game, p: PlayerId) -> CondDeps {
    let (_, _) = deficit(g, p);
    g.dv.stats.civs.get(p).map_or(CondDeps::all(), |m| m.deficit_deps.get())
}

/// When civilization `p`'s connectivity last changed, validated now: what the class `CONNECTED`
/// reads ([`civ::cond`]).
pub(crate) fn connectivity_changed(g: &Game, p: PlayerId) -> Rev {
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
/// population, its seat and index, and the classes its last computation recorded.
fn deficit(g: &Game, p: PlayerId) -> (i32, Rev) {
    let Some(m) = g.dv.stats.civs.get(p) else { return (0, Rev::START) };
    let inputs = || {
        let revs = &g.dv.revs;
        let o = revs.civ(p);
        let mut r = o
            .roster
            .max(o.cities)
            .max(o.index)
            .max(o.seat)
            .max(civ::civ_index_full_changed(g, p))
            .max(revs.config);
        for &c in g.state().cities().of(p) {
            r = r.max(revs.city(c).core);
        }
        r.max(civ_level_cond(g, m.deficit_deps.get(), p))
    };
    let compute = || {
        let (v, d) = record::recorded(|| economy::unit_supply_deficit(g, p));
        m.deficit_deps.set(d);
        v
    };
    let v = m.deficit.get(g.dv.revs.now(), inputs, compute);
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
/// for each that disagrees. The table of yields for other viewers and cities is walked entry by
/// entry.
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
    let seen: Vec<Viewing> = g.dv.stats.other.borrow().keys().copied().collect();
    for (t, viewer, city) in seen {
        let (a, b) = (tile_yield(g, t, viewer, city), tile_yield(&cold, t, viewer, city));
        if !a.bit_eq(&b) {
            out.push(format!(
                "tile {t} seen by {viewer:?} worked by {city:?}: its yield {a:?} differs from a \
                 cold rebuild {b:?}"
            ));
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
        if !city_base(g, c).bit_eq(&city_base(&cold, c)) {
            out.push(format!("city {}: its base yields differ from a cold rebuild", c.get()));
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
        if unit_upkeep(g, p) != unit_upkeep(&cold, p) {
            out.push(format!("player {}: its unit upkeep differs from a cold rebuild", p.0));
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

#[cfg(all(test, feature = "embedded-ruleset", feature = "stats"))]
mod tests {
    use super::*;
    use crate::game::core::testing;
    use crate::game::derive::rev::{PlayerTouch, UnitTouch};

    /// Every memo of this module read, with how often each has recomputed, by name.
    fn recomputes(g: &Game) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for city in g.st.cities().iter() {
            let c = city.id();
            drop((city_base(g, c), city_parts(g, c), city_stats(g, c)));
            let m = g.dv.stats.cities.get(&c).expect("a city's memos");
            let n = c.get();
            out.push((format!("base {n}"), m.base.stamp().counts().2));
            out.push((format!("parts {n}"), m.parts.stamp().counts().2));
            out.push((format!("stats {n}"), m.stats.stamp().counts().2));
        }
        for p in g.st.players().ids() {
            drop((happiness(g, p), civ_stats(g, p), connectivity(g, p)));
            let (_, _) = (unit_upkeep(g, p), unit_supply_deficit(g, p));
            let m = g.dv.stats.civs.get(p).expect("a civilization's memos");
            let n = p.0;
            out.push((format!("happiness {n}"), m.happiness.stamp().counts().2));
            out.push((format!("civ_stats {n}"), m.stats.stamp().counts().2));
            out.push((format!("upkeep {n}"), m.upkeep.stamp().counts().2));
            out.push((format!("connectivity {n}"), m.connectivity.stamp().counts().2));
            out.push((format!("deficit {n}"), m.deficit.stamp().counts().2));
        }
        out
    }

    /// The memos that recomputed between two readings.
    fn moved(before: &[(String, u64)], after: &[(String, u64)]) -> Vec<String> {
        before.iter().zip(after).filter(|(a, b)| a.1 != b.1).map(|(a, _)| a.0.clone()).collect()
    }

    #[test]
    fn a_move_a_heal_growth_elsewhere_and_influence_recompute_none() {
        let mut g = testing::duel();
        let (rome, greece, geneva) = (PlayerId(0), PlayerId(1), PlayerId(2));
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        let _antium = testing::city(&mut g, rome, TileIdx(26), "Antium");
        let athens = testing::city(&mut g, greece, TileIdx(57), "Athens");
        let hoplite = testing::unit(&mut g, greece, "Warrior", TileIdx(60));
        let legion = testing::unit(&mut g, rome, "Warrior", TileIdx(24));
        for t in [TileIdx(12), TileIdx(21), TileIdx(23)] {
            g.set_tile_owner(t, crate::state::TileClaim::city(rome, roma)).expect("a tile");
        }
        g.settle();
        let before = recomputes(&g);
        g.relocate_unit(hoplite, TileIdx(61)).expect("a move");
        g.relocate_unit(legion, TileIdx(33)).expect("a move");
        if let Some(u) = g.unit_mut(hoplite, UnitTouch::CORE) {
            u.hp = 100;
        }
        g.set_influence(geneva, rome, 10.0).expect("a city-state");
        if let Some(x) = g.city_mut(athens, crate::game::derive::rev::CityTouch::STOCKS) {
            x.food += 3.0;
        }
        g.settle();
        assert_eq!(moved(&before, &recomputes(&g)), Vec::<String>::new());
        // A treasury: the civilization's stats read it, and happiness the natural wonders
        // written with it; no city's memo does.
        let before = recomputes(&g);
        if let Some(x) = g.player_mut(rome, PlayerTouch::STOCKS) {
            x.econ.gold += 10.0;
        }
        g.settle();
        assert_eq!(moved(&before, &recomputes(&g)), ["happiness 0", "civ_stats 0"]);
        // Where Roma's citizens work: its own parts and stats, and whatever their value moves;
        // never its base, the trade network, or another civilization's.
        let before = recomputes(&g);
        let t = crate::game::cities::stats::workable_tiles(&g, roma)[0];
        if let Some(x) = g.city_mut(roma, crate::game::derive::rev::CityTouch::WORK) {
            x.locked = vec![t];
        }
        g.settle();
        let moved = moved(&before, &recomputes(&g));
        assert!(moved.contains(&format!("parts {}", roma.get())), "{moved:?}");
        for name in ["base", "connectivity", "happiness 1", "civ_stats 1"] {
            assert!(!moved.iter().any(|m| m.starts_with(name)), "{name} in {moved:?}");
        }
        assert!(g.take_violations().is_empty());
    }
}
