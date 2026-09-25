//! A city's yields and happiness with one more building, for the production advisor
//! (`bots/basic.py:1491-1505`, `BasicBot._simulate`, UnCiv's `getStatDifferenceFromBuilding`;
//! DESIGN.md 6.11).
//!
//! Python appended the building to the city, threw every cache away, asked the city's stats and
//! happiness afresh, and took the building out again. Here nothing is written, neither the state
//! nor a memo: the game is read through an [`Overlay`] (`EvalView::what_if`), which adds the
//! building to the city's set and to the indexes it reaches (the city's own, `CityLocal`, and its
//! owner's, `CivIndex`, each with the building's uniques added, which is what a rebuild with it
//! would give), and holds what the building changes further out: its owner's resources (a
//! building needs one, or provides one), and with them the resource layer of its owner's index
//! and cities, its trade network (a harbour), and how far over its unit supply it is. The city's
//! tile modifiers, base, parts and stats are computed afresh in that view (the functions the
//! memos compute them with, `*_in`); a tile's yield, and anything else the building cannot reach,
//! is read from its memo, which the classes it recorded (`unique::record`) show it did not read
//! the building's city, its owner's buildings or the world's. A city in We Love The King Day
//! asks whether its owner would be happy with the building, which reads every city of its owner
//! the same way.
//!
//! The what-ifs of one city share what reading its memos gives them ([`CityWhatIf`]): its stats,
//! tile modifiers and each tile's yield with what it read.
//!
//! What the what-if gives equals what adding the building and reading the city's memos gives,
//! bit for bit: a property over the reference states holds it (package 1c-07, gate 1).

use std::borrow::Cow;

use super::super::derive::{civ, stats as memo};
use super::super::{EvalView, Game, economy, tiles};
use super::connections::{self, Connectivity};
use super::stats::{self as cstats, CityParts, Work};
use crate::base::ids::{BuildingId, CityId, PlayerId, TileIdx};
use crate::base::sets::{BuildingSet, ResourceSet};
use crate::base::stats::Stats;
use crate::game::core::has_type;
use crate::game::economy::ResourceSupply;
use crate::game::tiles::CityMods;
use crate::unique::index::Extra;
use crate::unique::{CondDeps, Csr, UniqueType, index, record};

/// A city's stats with and without one more building (`_simulate`'s answer beside the city's
/// own): each is the total of its stats (`city_stats(g, c)["total"]`), with its happiness the
/// sum of the city's happiness list (`sum(city_happiness(g, c).values())`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatsDelta {
    pub before: Stats,
    pub after: Stats,
}

impl StatsDelta {
    /// What the building adds to each stat, happiness included.
    #[must_use]
    pub fn delta(&self) -> Stats {
        self.after - self.before
    }
}

/// The game with one building added to one city, as the what-if reads it through
/// `EvalView::what_if`. A field left `None` (or empty) reads as the game has it.
#[derive(Clone, Debug)]
pub(crate) struct Overlay {
    pub(crate) city: CityId,
    pub(crate) owner: PlayerId,
    /// The city's buildings with the new one.
    pub(crate) buildings: BuildingSet,
    /// The city's own index with the building's uniques that hold in it alone (`CityLocal`),
    /// when it has some or its owner's resources changed.
    pub(crate) local: Option<Csr>,
    /// The same with its resources' (`CityLocalFull`).
    pub(crate) local_full: Option<Csr>,
    /// Its owner's index with the building's other uniques (`CivIndex`), when it has some.
    pub(crate) civ: Option<Csr>,
    /// The same with the resource layer (`CivIndexFull`), when either changed.
    pub(crate) civ_full: Option<Csr>,
    /// Its owner's other cities' `CityLocalFull`, when the resources its owner has some of
    /// changed.
    pub(crate) others: Vec<(CityId, Csr)>,
    /// Its owner's resources, when they changed.
    pub(crate) supply: Option<ResourceSupply>,
    /// Whether the resources its owner has some of changed, and with them the resource layer.
    pub(crate) layer: bool,
    /// Its owner's trade network, when it changed.
    pub(crate) connectivity: Option<Connectivity>,
    /// How far over its unit supply its owner is, when the building could change it.
    pub(crate) deficit: Option<i32>,
    /// What the building adds to the city's own index, and to its owner's.
    adds_local: &'static Extra,
    adds_civ: &'static Extra,
}

/// The classes a building added to a city moves whatever it is: what reads a city's buildings
/// (`CITY`, the city in context), a civilization's and the world's.
const MOVED: CondDeps =
    CondDeps::CITY.union(CondDeps::CIV_BUILDINGS).union(CondDeps::GLOBAL_BUILDINGS);

/// The unique types a city's tile modifiers are gathered from (`tiles::city_mods`).
const TILE_MOD_TYPES: [UniqueType; 5] = [
    UniqueType::StatsFromTiles,
    UniqueType::StatsFromObject,
    UniqueType::StatsFromTilesWithout,
    UniqueType::StatPercentFromObject,
    UniqueType::AllStatsPercentFromObject,
];

/// The unique types of a building the resource supply reads (`economy.city_resources`,
/// `economy.detailed_resources`), besides the resource it needs.
const SUPPLY_TYPES: [UniqueType; 4] = [
    UniqueType::ProvidesResources,
    UniqueType::ProvidesExtraLuxuryFromCityResources,
    UniqueType::PercentResourceProduction,
    UniqueType::CityStateResources,
];

/// The unique types unit supply reads (`economy.unit_supply`).
const UNIT_SUPPLY_TYPES: [UniqueType; 3] =
    [UniqueType::BaseUnitSupply, UniqueType::UnitSupplyPerCity, UniqueType::UnitSupplyPerPop];

/// Whether index `a` and `b` hold the same entries of each of `types`.
fn same_of(a: &Csr, b: &Csr, types: &[UniqueType]) -> bool {
    types.iter().all(|&ty| a.get(ty) == b.get(ty))
}

impl Overlay {
    /// City `c` with building `b` added: `None` for a city the game does not have, or one that
    /// has the building already.
    fn new(g: &Game, c: CityId, b: BuildingId) -> Option<Self> {
        let city = g.city(c)?;
        if city.buildings.contains(b) {
            return None;
        }
        let owner = city.owner();
        let r: &'static crate::rules::Ruleset = g.rules;
        let a = &r.derived().advisor;
        let mut buildings = city.buildings;
        buildings.insert(b);
        let mut o = Self {
            city: c,
            owner,
            buildings,
            local: None,
            local_full: None,
            civ: None,
            civ_full: None,
            others: Vec::new(),
            supply: None,
            layer: false,
            connectivity: None,
            deficit: None,
            adds_local: a.adds_local.get(b)?,
            adds_civ: a.adds_civ.get(b)?,
        };
        if !o.adds_local.is_empty() {
            o.local = Some(civ::city_local(g, c).plus(o.adds_local));
            o.local_full = Some(civ::city_local_full(g, c).plus(o.adds_local));
        }
        if !o.adds_civ.is_empty() {
            o.civ = Some(civ::civ_index(g, owner).plus(o.adds_civ));
            o.civ_full = Some(civ::civ_index_full(g, owner).plus(o.adds_civ));
        }
        o.resources(g, b);
        o.network(g, b);
        o.unit_supply(g);
        Some(o)
    }

    /// Whether the owner's index with its resource layer holds different entries of any of
    /// `types` with the building: it adds some, or the resource layer changed and gives others.
    fn civ_moves(&self, g: &Game, types: &[UniqueType]) -> bool {
        if !self.layer {
            return types.iter().any(|&ty| self.adds_civ.has(ty));
        }
        self.civ_full
            .as_ref()
            .is_some_and(|x| !same_of(x, &civ::civ_index_full(g, self.owner), types))
    }

    /// Its owner's resources with the building, when the building could change them: it needs a
    /// resource, carries a unique the supply reads, or the conditionals of those the supply
    /// evaluates read buildings. When the resources it has some of change, so do the resource
    /// layer of its owner's index and its cities' own indexes.
    fn resources(&mut self, g: &Game, b: BuildingId) {
        let r = g.rules();
        let bd = &r.buildings()[b];
        let t = r.uniques();
        let may = bd.required_resource.is_some()
            || bd
                .uniques
                .ids()
                .any(|id| t.meta(id).ty.is_some_and(|ty| SUPPLY_TYPES.contains(&ty)))
            || civ::supply_reads(g).intersects(MOVED);
        if !may {
            return;
        }
        let after = economy::compute_supply_in(&EvalView::what_if(g, self), self.owner);
        let before = civ::supply(g, self.owner);
        if before.as_deref() == Some(&after) {
            return;
        }
        let had = before.map(|s| s.positive()).unwrap_or_default();
        let has = after.positive();
        self.supply = Some(after);
        if had == has {
            return;
        }
        self.layer = true;
        // The resource layer, as `civ_index_full` and `city_local_full` build it.
        let base = self.civ.clone().unwrap_or_else(|| civ::civ_index(g, self.owner).clone());
        self.civ_full = Some(base.merged(&index::resource_layer(r, &has)));
        let none = BuildingSet::new();
        let layer = |c: CityId| -> Csr {
            let given: ResourceSet = economy::provided_resources(g, c) & has;
            index::city_local(r, &none, &given)
        };
        let own = self.local.clone().unwrap_or_else(|| civ::city_local(g, self.city).clone());
        self.local_full = Some(own.merged(&layer(self.city)));
        for &c in g.state().cities().of(self.owner) {
            if c != self.city {
                let x = civ::city_local(g, c).merged(&layer(c));
                self.others.push((c, x));
            }
        }
    }

    /// Its owner's trade network with the building, when the building could change it: a
    /// harbour, a `Forests and Jungles are roads` it adds, or what the network's conditionals
    /// read.
    fn network(&mut self, g: &Game, b: BuildingId) {
        let r = g.rules();
        let mut reads = MOVED;
        if self.supply.is_some() {
            reads |= CondDeps::RESOURCES;
        }
        let may = has_type(r, &r.buildings()[b].uniques, UniqueType::ConnectTradeRoutes)
            || self.civ_moves(g, &[UniqueType::ForestsAndJunglesAreRoads])
            || memo::connectivity_deps(g, self.owner).intersects(reads);
        if !may {
            return;
        }
        let after = connections::connected_cities_in(&EvalView::what_if(g, self), self.owner);
        if after != *memo::connectivity(g, self.owner) {
            self.connectivity = Some(after);
        }
    }

    /// How far over its unit supply its owner is with the building, when the building could
    /// change it: it adds to the unit supply's uniques, or what their conditionals read.
    fn unit_supply(&mut self, g: &Game) {
        let may = self.civ_moves(g, &UNIT_SUPPLY_TYPES)
            || memo::deficit_deps(g, self.owner).intersects(self.moved());
        if may {
            self.deficit =
                Some(economy::unit_supply_deficit_in(&EvalView::what_if(g, self), self.owner));
        }
    }

    /// The classes whose answers the building may change: [`MOVED`], and those of the owner's
    /// resources and trade network where they changed.
    fn moved(&self) -> CondDeps {
        let mut out = MOVED;
        if self.supply.is_some() {
            out |= CondDeps::RESOURCES;
        }
        if self.connectivity.is_some() {
            out |= CondDeps::CONNECTED;
        }
        out
    }

    /// Whether the building could change the tile modifiers of city `x`, whose last computation
    /// read `read`: it adds a tile modifier to an index the city reads, the resource layer
    /// changed, or what the modifiers' conditionals and city filters read moved.
    fn mods_move(&self, g: &Game, x: CityId, read: CondDeps) -> bool {
        let local = x == self.city && TILE_MOD_TYPES.iter().any(|&ty| self.adds_local.has(ty));
        let layer = self.layer
            && (self.others.iter().any(|(y, _)| *y == x)
                || (x == self.city
                    && self.local_full.as_ref().is_some_and(|l| {
                        !same_of(l, &civ::city_local_full(g, x), &TILE_MOD_TYPES)
                    })));
        local || layer || self.civ_moves(g, &TILE_MOD_TYPES) || read.intersects(self.moved())
    }
}

/// What reading a city's memos gives each what-if of it: its tile modifiers and what they read,
/// and each tile that adds to its yields with its yield and what that read.
struct CityRead {
    mods: CityMods,
    mods_read: CondDeps,
    tiles: Vec<(TileIdx, Stats, CondDeps)>,
}

impl CityRead {
    fn new(g: &Game, x: CityId) -> Option<Self> {
        let city = g.city(x)?;
        let owner = city.owner();
        let tiles = cstats::worked_or_free_tiles(g, x, &city.worked)
            .into_iter()
            .map(|t| {
                let (s, read) = memo::tile_yield_read(g, t, Some(owner), Some(x));
                (t, s, read)
            })
            .collect();
        Some(Self {
            mods: memo::city_mods(g, x).map(|m| m.clone()).unwrap_or_default(),
            mods_read: memo::city_mods_deps(g, x),
            tiles,
        })
    }
}

/// City `x`'s parts in view `v`, with the building (`_city_happiness` and the first lines of
/// `_compute_city_stats`): its tile modifiers, its tiles' yields and its base computed in the
/// view where the building could change them, else as its memos have them (`read`).
fn parts_in(v: &EvalView<'_>, o: &Overlay, x: CityId, read: &CityRead) -> Option<CityParts> {
    let g = v.game();
    let city = g.city(x)?;
    let owner = city.owner();
    let moved = o.moved();
    let fresh = o.mods_move(g, x, read.mods_read).then(|| tiles::city_mods_in(v, x));
    // A tile's memo read the modifiers the city has now: its yield holds while they are the
    // same and nothing else it read moved.
    let mods = fresh.as_ref().unwrap_or(&read.mods);
    let same = fresh.as_ref().is_none_or(|m| *m == read.mods);
    let mut sum = Stats::ZERO;
    for &(t, s, deps) in &read.tiles {
        sum += if same && !deps.intersects(moved) {
            s
        } else {
            let mut d = CondDeps::empty();
            tiles::compute_tile_yield_in(v, t, Some(owner), Some(x), Some(mods), &mut d)
        };
    }
    let base = cstats::city_base_in(v, x);
    Some(cstats::city_parts_from(v, x, &Work::of(city), sum, base.buildings, base.by_source))
}

/// The what-ifs of one city (`BasicBot._simulate` for each building the advisor weighs): what
/// reading the city's memos gives, read once for them all.
pub struct CityWhatIf<'g> {
    g: &'g Game,
    c: CityId,
    /// The city's stats now.
    before: Stats,
    read: CityRead,
}

impl<'g> CityWhatIf<'g> {
    /// The what-ifs of city `c`; `None` for a city the game does not have.
    #[must_use]
    pub fn new(g: &'g Game, c: CityId) -> Option<Self> {
        record::isolated(|| {
            let read = CityRead::new(g, c)?;
            let before = memo::city_stats(g, c).total;
            Some(Self { g, c, before, read })
        })
    }

    /// The city's stats with building `b` added, beside its stats now: `None` for one it has.
    /// Reads only: the state, the memos and the digest are as they were.
    #[must_use]
    pub fn with(&self, b: BuildingId) -> Option<StatsDelta> {
        let (g, c) = (self.g, self.c);
        record::isolated(|| {
            let o = Overlay::new(g, c, b)?;
            let v = EvalView::what_if(g, &o);
            let city = g.city(c)?;
            let parts = parts_in(&v, &o, c, &self.read)?;
            let major = g.player(o.owner).is_some_and(crate::state::players::Player::is_major);
            // Only a city in We Love The King Day reads whether its owner is happy.
            let happy = (city.wltkd > 0 && major).then(|| happiness_total(&v, &o, &parts) >= 0);
            let work = Work::of(city);
            let construction = cstats::current_construction(city);
            let after = cstats::city_stats_from_in(&v, c, &parts, &work, construction, happy).total;
            Some(StatsDelta { before: self.before, after })
        })
    }
}

/// City `c`'s stats with building `b` added, beside its stats now (`BasicBot._simulate`,
/// `bots/basic.py:1491-1505`): `None` for a city the game does not have, or one that has the
/// building. Reads only: the state, the memos and the digest are as they were. The what-ifs of
/// several buildings in one city share more through [`CityWhatIf`].
#[must_use]
pub fn what_if_building(g: &Game, c: CityId, b: BuildingId) -> Option<StatsDelta> {
    CityWhatIf::new(g, c)?.with(b)
}

/// The owner's happiness with the building (`economy.happiness`): each of its cities' parts in
/// the view, the what-if's city's given, and another's read from its memo when the building
/// reaches nothing it read.
fn happiness_total(v: &EvalView<'_>, o: &Overlay, parts: &CityParts) -> i32 {
    let g = v.game();
    let wide = o.civ_full.is_some() || o.supply.is_some();
    // Another city's own buildings are as they were.
    let others = o.moved().difference(CondDeps::CITY);
    economy::compute_happiness_in(v, o.owner, |x| -> Cow<'_, CityParts> {
        if x == o.city {
            return Cow::Borrowed(parts);
        }
        if !wide && !memo::city_reads(g, x).intersects(others) {
            return Cow::Owned(memo::city_parts(g, x).clone());
        }
        let read = CityRead::new(g, x);
        Cow::Owned(read.and_then(|r| parts_in(v, o, x, &r)).unwrap_or_default())
    })
    .total
}

/// How far a what-if of building `b` in city `c` reaches: which of what the overlay can hold it
/// holds (feature `test-ops`), so that a test can show the reference states reach each.
#[cfg(any(test, feature = "test-ops"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    /// The building adds to the city's own index.
    pub local: bool,
    /// It adds to its owner's index.
    pub civ: bool,
    /// It changes its owner's resources, and the resources it has some of.
    pub supply: bool,
    pub resource_layer: bool,
    /// It changes its owner's trade network.
    pub network: bool,
    /// The unit supply is asked again.
    pub deficit: bool,
    /// The city is in We Love The King Day, and its owner's happiness is asked.
    pub happiness: bool,
}

/// The [`Reach`] of a what-if of building `b` in city `c` (feature `test-ops`).
#[cfg(any(test, feature = "test-ops"))]
#[must_use]
pub fn reach_for_test(g: &Game, c: CityId, b: BuildingId) -> Option<Reach> {
    let o = Overlay::new(g, c, b)?;
    let city = g.city(c)?;
    let major = g.player(o.owner).is_some_and(crate::state::players::Player::is_major);
    Some(Reach {
        local: o.local.is_some(),
        civ: o.civ.is_some(),
        supply: o.supply.is_some(),
        resource_layer: o.layer,
        network: o.connectivity.is_some(),
        deficit: o.deficit.is_some(),
        happiness: city.wltkd > 0 && major,
    })
}

/// Adds building `b` to city `c` as `_simulate` did, with nothing else a building brings (no
/// free buildings, no triggers, no health), or takes it out again: the reference gate 1 holds the
/// what-if to (feature `test-ops`).
#[cfg(any(test, feature = "test-ops"))]
pub fn toggle_building_for_test(g: &mut Game, c: CityId, b: BuildingId, on: bool) {
    use super::super::derive::rev::CityTouch;
    if let Some(x) = g.city_mut(c, CityTouch::BUILDINGS) {
        if on {
            x.buildings.insert(b);
        } else {
            x.buildings.remove(b);
        }
    }
}
