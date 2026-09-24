//! A city's yields, with where each comes from, and its happiness (`cities.py:139-700`).
//!
//! Ports `cities.py:139-246` (the tiles a city works, specialists), `246-315` (buildings' stats
//! and percentages, maintenance), `318-587` (trade routes, the uniques by source, the percentage
//! bonuses, food eaten, growth, city happiness) and `587-700` (the stats breakdown, food to the
//! next citizen); and the city's other numbers the `city_stats` refcheck group reads beside them:
//! `max_health` (`cities.py:1872-1874`), `combat.city_strength` (`combat.py:149-174`), and the
//! production cost and turns of what it builds (`cities.py:1091-1128, 1596-1618`), which package
//! 1b-07's construction reads too.
//!
//! The breakdown keeps Python's keys: a source's stats hold the stats Python's dict held, a zero
//! included where it was named ([`Yields`]), since the answers are compared key by key. The
//! city's happiness with its parts ([`CityParts`], the memo `CityHappiness`) and its full stats
//! ([`CityStats`], the memo `CityStats`) are memos (`derive::stats`); what they hold is computed
//! here from a [`Work`], which the citizen assignment also passes, to know a city's food with the
//! citizens it has placed so far.
//!
//! What differs from Python, on purpose:
//! - a tile another city of the same owner works is not workable (`cities-never-share-a-tile`);
//!   Python refused only the tiles the tile's own city worked, so two cities could work one;
//! - a city's pressures are kept sorted (see `game::religion`), so a tie between religions is
//!   broken by that order.

use smallvec::SmallVec;

use super::super::Game;
use super::super::derive::stats as memo;
use super::super::religion;
use crate::base::ids::{BuildingId, CityId, PlayerId, SpecialistId, TileIdx};
use crate::base::num;
use crate::base::sets::MAX_SPECIALISTS;
use crate::base::stats::{Stat, StatMask, Stats};
use crate::game::core::has_type;
use crate::game::economy;
use crate::state::cities::{City, Constructible, Perpetual};
use crate::unique::world::CombatAction;
use crate::unique::{CombatCtx, Combatant, Ctx, Source, UniqueData, UniqueType, uq};

// ---- Stats as Python's dicts held them ----------------------------------------------------------

/// A source's stats as Python's dict held them: the values, and which stats it named. Python
/// added a unique's stats key by key (`add_into`), so a stat the unique named is there even at 0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Yields {
    pub stats: Stats,
    pub keys: StatMask,
}

impl Yields {
    /// Every stat named, as a dict made with `zero()` names them.
    #[must_use]
    pub const fn all(stats: Stats) -> Self {
        Self { stats, keys: StatMask::ALL }
    }

    /// `add_into(self, s, mult)`: the stats `s` names, scaled; each is named here afterwards.
    pub fn add(&mut self, s: &Stats, mult: f64) {
        self.stats.add_scaled(s, mult);
        for (k, _) in s.nonzero() {
            self.keys.insert(k);
        }
    }

    /// `self[k] = self.get(k, 0) + x`.
    pub fn add_to(&mut self, k: Stat, x: f64) {
        self.stats[k] += x;
        self.keys.insert(k);
    }

    /// `self[k] = x`.
    pub fn put(&mut self, k: Stat, x: f64) {
        self.stats[k] = x;
        self.keys.insert(k);
    }

    /// `self.pop(k)`.
    pub fn remove(&mut self, k: Stat) {
        self.stats[k] = 0.0;
        self.keys.remove(k);
    }

    /// Whether it names `k`.
    #[must_use]
    pub const fn has(&self, k: Stat) -> bool {
        self.keys.contains(k)
    }

    /// `self.get(k, 0)`.
    #[must_use]
    pub fn get(&self, k: Stat) -> f64 {
        self.stats[k]
    }

    /// Multiplies `k` by `m` if it is named (`if k in s: s[k] *= m`).
    fn scale(&mut self, k: Stat, m: f64) {
        if self.has(k) {
            self.stats[k] *= m;
        }
    }
}

/// The kind of object a unique came from, as Python named it (`Unique.src_type`,
/// `rules.py:82-164`): a stat breakdown lists a unique's stats under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    Era,
    Tech,
    UnitType,
    Unit,
    Promotion,
    Terrain,
    Resource,
    Improvement,
    Building,
    Belief,
    Policy,
    Nation,
    CityStateType,
    /// A city-state's friend or ally bonus.
    CityState,
    Ruins,
    Temporary,
    Global,
}

impl SourceKind {
    /// The kind of `s`.
    #[must_use]
    pub const fn of(s: Source) -> Self {
        match s {
            Source::Nation(_) => Self::Nation,
            Source::Building(_) => Self::Building,
            Source::Policy(_) => Self::Policy,
            Source::Tech(_) => Self::Tech,
            Source::Temporary(_) => Self::Temporary,
            Source::Era(_) => Self::Era,
            Source::CityStateFriend(_) | Source::CityStateAlly(_) => Self::CityState,
            Source::CityStateType(_) => Self::CityStateType,
            Source::Belief(_) => Self::Belief,
            Source::Resource(_) => Self::Resource,
            Source::Global => Self::Global,
            Source::Terrain(_) => Self::Terrain,
            Source::Improvement(_) => Self::Improvement,
            Source::UnitType(_) => Self::UnitType,
            Source::Unit(_) => Self::Unit,
            Source::Promotion(_) => Self::Promotion,
            Source::Ruins(_) => Self::Ruins,
        }
    }

    /// Python's name for it: `"Policy"`, `"CityState"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Era => "Era",
            Self::Tech => "Tech",
            Self::UnitType => "UnitType",
            Self::Unit => "Unit",
            Self::Promotion => "Promotion",
            Self::Terrain => "Terrain",
            Self::Resource => "Resource",
            Self::Improvement => "Improvement",
            Self::Building => "Building",
            Self::Belief => "Belief",
            Self::Policy => "Policy",
            Self::Nation => "Nation",
            Self::CityStateType => "CityStateType",
            Self::CityState => "CityState",
            Self::Ruins => "Ruins",
            Self::Temporary => "Temporary",
            Self::Global => "Global",
        }
    }

    /// The key of its growth in a city's breakdown: `"Policy (growth)"`.
    #[must_use]
    pub const fn growth_name(self) -> &'static str {
        match self {
            Self::Era => "Era (growth)",
            Self::Tech => "Tech (growth)",
            Self::UnitType => "UnitType (growth)",
            Self::Unit => "Unit (growth)",
            Self::Promotion => "Promotion (growth)",
            Self::Terrain => "Terrain (growth)",
            Self::Resource => "Resource (growth)",
            Self::Improvement => "Improvement (growth)",
            Self::Building => "Building (growth)",
            Self::Belief => "Belief (growth)",
            Self::Policy => "Policy (growth)",
            Self::Nation => "Nation (growth)",
            Self::CityStateType => "CityStateType (growth)",
            Self::CityState => "CityState (growth)",
            Self::Ruins => "Ruins (growth)",
            Self::Temporary => "Temporary (growth)",
            Self::Global => "Global (growth)",
        }
    }
}

/// The kind of the object unique `id` came from.
fn kind_of(g: &Game, id: crate::base::ids::UniqueId) -> SourceKind {
    SourceKind::of(g.rules().uniques().meta(id).source)
}

/// A source of a city's stats: the keys of Python's breakdown (`final`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatSource {
    Population,
    TileYields,
    Specialists,
    TradeRoutes,
    Buildings,
    /// A city or civilization unique's stats, by the kind of its source.
    Uniques(SourceKind),
    /// Production turned into gold or science.
    Construction,
    /// `[n]% growth`, by the kind of its source: `"Policy (growth)"`.
    Growth(SourceKind),
    WeLoveTheKingDay,
    Maintenance,
    ExcessFood,
    Unhappiness,
    /// The floor of one production.
    Production,
}

impl StatSource {
    /// Python's key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Population => "Population",
            Self::TileYields => "Tile yields",
            Self::Specialists => "Specialists",
            Self::TradeRoutes => "Trade routes",
            Self::Buildings => "Buildings",
            Self::Uniques(k) => k.name(),
            Self::Construction => "Construction",
            Self::Growth(k) => k.growth_name(),
            Self::WeLoveTheKingDay => "We Love The King Day",
            Self::Maintenance => "Maintenance",
            Self::ExcessFood => "Excess food to production",
            Self::Unhappiness => "Unhappiness",
            Self::Production => "Production",
        }
    }
}

/// A source of a city's happiness: the keys of Python's `happiness_list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HappinessSource {
    Cities,
    Population,
    OccupiedCity,
    Specialists,
    Buildings,
    TileYields,
    /// A unique's happiness, by the kind of its source.
    Uniques(SourceKind),
}

impl HappinessSource {
    /// Python's key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cities => "Cities",
            Self::Population => "Population",
            Self::OccupiedCity => "Occupied City",
            Self::Specialists => "Specialists",
            Self::Buildings => "Buildings",
            Self::TileYields => "Tile yields",
            Self::Uniques(k) => k.name(),
        }
    }
}

// ---- Work and tiles (cities.py:139-222) ----------------------------------------------------------

/// Where a city's citizens are: the tiles they work and its specialists by kind. A city's own, or
/// the one citizen assignment is building.
#[derive(Clone, Copy, Debug)]
pub struct Work<'a> {
    pub worked: &'a [TileIdx],
    pub specialists: [u8; MAX_SPECIALISTS],
}

impl<'a> Work<'a> {
    /// The city's own.
    #[must_use]
    pub fn of(city: &'a City) -> Self {
        Self { worked: &city.worked, specialists: city.specialists }
    }

    /// How many specialists there are.
    #[must_use]
    pub fn specialists_total(&self) -> i32 {
        self.specialists.iter().map(|&n| i32::from(n)).sum()
    }

    /// Citizens working no tile and no specialist slot (`cities.free_population`,
    /// `cities.py:142-144`).
    #[must_use]
    pub fn free(&self, pop: u16) -> i32 {
        i32::from(pop)
            - i32::try_from(self.worked.len()).unwrap_or(i32::MAX)
            - self.specialists_total()
    }
}

/// How far from its centre a city may work tiles (`cities.work_range`, `cities.py:150-152`).
#[must_use]
pub fn work_range(g: &Game) -> u32 {
    u32::try_from(g.rules().constants().formulas.city_work_range).unwrap_or(0)
}

/// Every tile within a city's working distance (`cities.tiles_in_range`, `cities.py:165-167`).
#[must_use]
pub fn tiles_in_range(g: &Game, c: CityId) -> Vec<TileIdx> {
    g.city(c).map(|x| g.grid().within(x.tile(), work_range(g))).unwrap_or_default()
}

/// The tiles a city's citizens could be put to work on (`cities.workable_tiles`,
/// `cities.py:170-193`): its owner's, within its working distance, no city on them, no enemy
/// military unit on them (the blockade), and worked by no other city.
// refcheck: cities-never-share-a-tile
#[must_use]
pub fn workable_tiles(g: &Game, c: CityId) -> Vec<TileIdx> {
    let Some(city) = g.city(c) else { return Vec::new() };
    let owner = city.owner();
    let st = g.state();
    let range = work_range(g);
    // A sibling works only tiles within its own range, so only one within twice the range of
    // this city can work one of its tiles.
    let others: SmallVec<[TileIdx; 32]> = st
        .cities()
        .of(owner)
        .iter()
        .filter(|&&x| x != c)
        .filter_map(|&x| st.cities().get(x))
        .filter(|x| g.grid().distance(x.tile(), city.tile()) <= 2 * range)
        .flat_map(|x| x.worked.iter().copied())
        .collect();
    let mut out = Vec::new();
    for t in g.grid().within(city.tile(), range) {
        if t == city.tile() || g.tile(t).and_then(crate::state::map::Tile::owner) != Some(owner) {
            continue;
        }
        if others.contains(&t) || st.city_at(t).is_some() {
            continue;
        }
        if g.military_at(t).is_some_and(|m| g.at_war(owner, m.owner())) {
            continue;
        }
        out.push(t);
    }
    out
}

/// Whether a tile yields with no citizen on it, as a Citadel does
/// (`cities.provides_yield_without_pop`, `cities.py:196-202`).
#[must_use]
pub fn provides_yield_without_pop(g: &Game, t: TileIdx) -> bool {
    let r = g.rules();
    let ty = UniqueType::TileProvidesYieldWithoutPopulation;
    if let Some(i) = crate::game::tiles::unpillaged_improvement(g, t)
        && has_type(r, &r.improvements()[i].uniques, ty)
    {
        return true;
    }
    crate::game::tiles::all_terrains(g, t).any(|x| has_type(r, &r.terrains()[x].uniques, ty))
}

/// Every tile that adds to a city's yields: its centre, its worked tiles, and its own tiles that
/// yield without a citizen (`cities.worked_or_free_tiles`, `cities.py:205-213`).
#[must_use]
pub fn worked_or_free_tiles(g: &Game, c: CityId, worked: &[TileIdx]) -> SmallVec<[TileIdx; 24]> {
    let mut out = SmallVec::new();
    let Some(city) = g.city(c) else { return out };
    out.push(city.tile());
    out.extend(worked.iter().copied());
    for t in economy::city_tiles(g, c) {
        if t != city.tile() && !worked.contains(&t) && provides_yield_without_pop(g, t) {
            out.push(t);
        }
    }
    out
}

// ---- Specialists and buildings (cities.py:216-315) ------------------------------------------------

/// Specialist slots by kind, summed over the city's buildings, in the order they first appear
/// (`cities.max_specialists`, `cities.py:219-225`).
#[must_use]
pub fn max_specialists(g: &Game, c: CityId) -> SmallVec<[(SpecialistId, i32); 4]> {
    let mut out: SmallVec<[(SpecialistId, i32); 4]> = SmallVec::new();
    let Some(city) = g.city(c) else { return out };
    for b in city.buildings.iter() {
        for &(s, n) in &g.rules().buildings()[b].specialist_slots {
            match out.iter_mut().find(|(x, _)| *x == s) {
                Some((_, m)) => *m += n,
                None => out.push((s, n)),
            }
        }
    }
    out
}

/// The slots of one kind (`max_specialists(...).get(name, 0)`).
#[must_use]
pub fn slots(max: &[(SpecialistId, i32)], s: SpecialistId) -> i32 {
    max.iter().find(|(x, _)| *x == s).map_or(0, |&(_, n)| n)
}

/// What one specialist of a kind yields in a city (`cities.specialist_stats`,
/// `cities.py:228-240`): its own stats, `[stats] from every specialist [cities]`, and
/// `[stats] from every [specialist]` naming it.
#[must_use]
pub fn specialist_stats(g: &Game, c: CityId, s: SpecialistId) -> Stats {
    let r = g.rules();
    let Some(sp) = r.specialists().get(s) else { return Stats::ZERO };
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let t = r.uniques();
    let filters = t.filters();
    let mut out = sp.stats;
    for h in uq::city(&v, c, UniqueType::StatsFromSpecialist, &ctx) {
        if let UniqueData::StatsFromSpecialist(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            out.add_scaled(t.stats(x.stats), f64::from(h.n));
        }
    }
    for h in uq::city(&v, c, UniqueType::StatsFromObject, &ctx) {
        if let UniqueData::StatsFromObject(x) = h.data()
            && t.object(x.object).specialist == Some(s)
        {
            out.add_scaled(t.stats(x.stats), f64::from(h.n));
        }
    }
    out
}

/// Whether building `b` passes an object filter's building part (`building_matches`).
fn object_names_building(g: &Game, o: crate::base::ids::ObjectFilterId, b: BuildingId) -> bool {
    let t = g.rules().uniques();
    t.object(o).buildings.is_some_and(|s| t.in_set(s, b))
}

/// The uniques of a city that add to what each of its buildings gives, gathered once for all of
/// them, in the order the city's queries give them: `[stats] from every [object]` and `[stats]
/// from all [buildings] buildings` for its yields, `[n]% [stat] from every [object]` and `[n]%
/// Yield from every [object]` for its percentages.
#[derive(Clone, Debug, Default)]
struct BuildingUniques {
    stats_from_object: SmallVec<[(UniqueData, u16); 4]>,
    stats_from_buildings: SmallVec<[(UniqueData, u16); 4]>,
    pct_from_object: SmallVec<[(UniqueData, u16); 4]>,
    all_pct_from_object: SmallVec<[(UniqueData, u16); 4]>,
}

impl BuildingUniques {
    /// Those of the yields.
    fn stats(v: &crate::game::EvalView<'_>, c: CityId, ctx: &Ctx) -> Self {
        let of = |ty| uq::city(v, c, ty, ctx).map(|h| (*h.data(), h.n)).collect();
        Self {
            stats_from_object: of(UniqueType::StatsFromObject),
            stats_from_buildings: of(UniqueType::StatsFromBuildings),
            ..Self::default()
        }
    }

    /// Those of the percentages.
    fn pct(v: &crate::game::EvalView<'_>, c: CityId, ctx: &Ctx) -> Self {
        let of = |ty| uq::city(v, c, ty, ctx).map(|h| (*h.data(), h.n)).collect();
        Self {
            pct_from_object: of(UniqueType::StatPercentFromObject),
            all_pct_from_object: of(UniqueType::AllStatsPercentFromObject),
            ..Self::default()
        }
    }
}

/// The flat yields a building gives in this city (`cities.building_stats`, `cities.py:246-266`):
/// its own, `[stats] from every [building]`, its `Stats` uniques that hold, and, unless it is a
/// wonder, `[stats] from all [buildings] buildings`.
#[must_use]
pub fn building_stats(g: &Game, c: CityId, b: BuildingId) -> Stats {
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    building_stats_with(g, &v, b, &ctx, &BuildingUniques::stats(&v, c, &ctx))
}

/// [`building_stats`] with the city's uniques gathered.
fn building_stats_with(
    g: &Game,
    v: &crate::game::EvalView<'_>,
    b: BuildingId,
    ctx: &Ctx,
    bu: &BuildingUniques,
) -> Stats {
    let r = g.rules();
    let t = r.uniques();
    let bd = &r.buildings()[b];
    let mut s = bd.stats;
    for &(d, n) in &bu.stats_from_object {
        if let UniqueData::StatsFromObject(x) = d
            && object_names_building(g, x.object, b)
        {
            s.add_scaled(t.stats(x.stats), f64::from(n));
        }
    }
    for h in uq::object(v, &bd.uniques, UniqueType::Stats, ctx) {
        if let UniqueData::Stats(x) = h.data() {
            s += *t.stats(x.stats);
        }
    }
    if !bd.is_wonder {
        for &(d, n) in &bu.stats_from_buildings {
            if let UniqueData::StatsFromBuildings(x) = d
                && t.in_set(x.buildings, b)
            {
                s.add_scaled(t.stats(x.stats), f64::from(n));
            }
        }
    }
    s
}

/// The percentage bonuses a building gives in this city (`cities.building_pct`,
/// `cities.py:269-282`): its own, and `[n]% [stat] from every [building]`, `[n]% Yield from
/// every [building]`, from the city's uniques gathered.
fn building_pct(g: &Game, b: BuildingId, bu: &BuildingUniques) -> Yields {
    let r = g.rules();
    let mut s = Yields::default();
    s.add(&r.buildings()[b].percent_stat_bonus, 1.0);
    for &(d, n) in &bu.pct_from_object {
        if let UniqueData::StatPercentFromObject(x) = d
            && object_names_building(g, x.object, b)
        {
            for _ in 0..n {
                s.add_to(x.stat, f64::from(x.percent));
            }
        }
    }
    for &(d, n) in &bu.all_pct_from_object {
        if let UniqueData::AllStatsPercentFromObject(x) = d
            && object_names_building(g, x.object, b)
        {
            for _ in 0..n {
                for k in Stat::ALL {
                    s.add_to(k, f64::from(x.percent));
                }
            }
        }
    }
    s
}

/// The gold a city's buildings cost each turn (`cities.maintenance`, `cities.py:295-315`): each
/// building's that was not free, times the `[n]% maintenance cost for [buildings] buildings
/// [cities]` that name it; an AI major's total times its seat's
/// `aiBuildingMaintenanceModifier`.
#[must_use]
pub fn maintenance(g: &Game, c: CityId) -> f64 {
    let Some(city) = g.city(c) else { return 0.0 };
    let r = g.rules();
    let t = r.uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let filters = t.filters();
    let mus: SmallVec<[(crate::base::ids::SetRef, i32, u16); 4]> =
        uq::city(&v, c, UniqueType::BuildingMaintenance, &ctx)
            .filter_map(|h| match *h.data() {
                UniqueData::BuildingMaintenance(x)
                    if filters.city_matches(x.cities, &v, c, None) =>
                {
                    Some((x.buildings, x.percent, h.n))
                }
                _ => None,
            })
            .collect();
    let mut total = 0.0;
    for b in city.buildings.iter() {
        if city.free_buildings.contains(b) {
            continue;
        }
        let mut m = f64::from(r.buildings()[b].maintenance);
        for &(set, pct, n) in &mus {
            if t.in_set(set, b) {
                for _ in 0..n {
                    m *= 1.0 + f64::from(pct) / 100.0;
                }
            }
        }
        total += m;
    }
    let owner = city.owner();
    if !g.is_humanlike(owner)
        && g.player(owner).is_some_and(crate::state::players::Player::is_major)
    {
        total *= r.difficulties()[g.seat_difficulty(Some(owner))].ai_building_maintenance_modifier;
    }
    total
}

// ---- The breakdown (cities.py:318-700) ------------------------------------------------------------

/// Whether a city is its owner's capital (`cities.is_capital`, `cities.py:114-116`).
#[must_use]
pub fn is_capital(g: &Game, c: CityId) -> bool {
    g.city(c).is_some_and(|x| g.player(x.owner()).is_some_and(|p| p.capital == Some(c)))
}

/// The yields of a city's trade route to its capital, if it has one (`cities.trade_route_stats`,
/// `cities.py:332-351`).
fn trade_route_stats(g: &Game, c: CityId) -> Yields {
    let mut s = Yields::default();
    let Some(city) = g.city(c) else { return s };
    let Some(cap) = g.player(city.owner()).and_then(|p| p.capital).and_then(|x| g.city(x)) else {
        return s;
    };
    if cap.id() == c || !memo::connected_to_capital(g, c) {
        return s;
    }
    s.put(Stat::Gold, f64::from(cap.pop) * 0.15 + f64::from(city.pop) * 1.1 - 1.0);
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    for h in uq::city(&v, c, UniqueType::StatsFromTradeRoute, &ctx) {
        if let UniqueData::StatsFromTradeRoute(x) = h.data() {
            s.add(t.stats(x.stats), f64::from(h.n));
        }
    }
    let mut pct = Stats::ZERO;
    for h in uq::city(&v, c, UniqueType::StatPercentFromTradeRoutes, &ctx) {
        if let UniqueData::StatPercentFromTradeRoutes(x) = h.data() {
            pct[x.stat] += f64::from(x.percent) * f64::from(h.n);
        }
    }
    for k in Stat::ALL {
        s.scale(k, 1.0 + pct[k] / 100.0);
    }
    s
}

/// The population a `[populationFilter]` counts in a city, with its citizens where `work` puts
/// them (`uniques.population_amount`, `uniques.py:720-732`).
fn population_amount(
    g: &Game,
    city: &City,
    work: &Work<'_>,
    f: crate::unique::params::PopulationFilter,
) -> i32 {
    use crate::unique::params::PopulationFilter as P;
    match f {
        P::Specialists => work.specialists_total(),
        P::Population => i32::from(city.pop),
        P::FollowersOfThisReligion | P::FollowersOfTheMajorityReligion => {
            religion::followers_of_majority(g, city.id())
        }
        P::Unemployed => work.free(city.pop),
        P::Specialist(s) => work.specialists.get(usize::from(s.0)).map_or(0, |&n| i32::from(n)),
    }
}

/// A city's flat stats from its and its owner's uniques, by the kind of their source
/// (`cities._uniques_by_source`, `cities.py:354-384`): `[stats] [cities]`, `[stats] per [n]
/// population [cities]`, `[stats] in cities on [terrain] tiles`; a city-state's bonus times its
/// owner's `[n]% [stat] from City-States`.
fn uniques_by_source(g: &Game, c: CityId) -> SmallVec<[(SourceKind, Yields); 4]> {
    let mut out: SmallVec<[(SourceKind, Yields); 4]> = SmallVec::new();
    let Some(city) = g.city(c) else { return out };
    let r = g.rules();
    let t = r.uniques();
    let filters = t.filters();
    let v = g.view();
    let owner = city.owner();
    let ctx = Ctx::city(&v, c);
    let cs_mults: SmallVec<[(Stat, i32, u16); 2]> =
        uq::civ(&v, owner, UniqueType::BonusStatsFromCityStates, &Ctx::civ(owner))
            .filter_map(|h| match *h.data() {
                UniqueData::BonusStatsFromCityStates(x) => Some((x.stat, x.percent, h.n)),
                _ => None,
            })
            .collect();
    let mut add = |id, stats: &Stats, mult: f64| {
        let kind = kind_of(g, id);
        let mut st = *stats;
        if kind == SourceKind::CityState {
            for &(k, pct, n) in &cs_mults {
                if st[k] != 0.0 {
                    for _ in 0..n {
                        st[k] *= 1.0 + f64::from(pct) / 100.0;
                    }
                }
            }
        }
        let slot = match out.iter().position(|(k, _)| *k == kind) {
            Some(i) => i,
            None => {
                out.push((kind, Yields::default()));
                out.len() - 1
            }
        };
        out[slot].1.add(&st, mult);
        // A multiplier that scaled a named stat to zero leaves it named, as Python's dict did.
        for (k, _) in stats.nonzero() {
            out[slot].1.keys.insert(k);
        }
    };
    for h in uq::city(&v, c, UniqueType::StatsPerCity, &ctx) {
        if let UniqueData::StatsPerCity(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                add(h.id, t.stats(x.stats), 1.0);
            }
        }
    }
    for h in uq::city(&v, c, UniqueType::StatsPerPopulation, &ctx) {
        if let UniqueData::StatsPerPopulation(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            let per = i32::from(city.pop).div_euclid(x.per.max(1));
            for _ in 0..h.n {
                add(h.id, t.stats(x.stats), f64::from(per));
            }
        }
    }
    for h in uq::city(&v, c, UniqueType::StatsFromCitiesOnSpecificTiles, &ctx) {
        if let UniqueData::StatsFromCitiesOnSpecificTiles(x) = h.data()
            && filters.tile_terrain_matches(x.terrain, &v, city.tile(), Some(owner))
        {
            for _ in 0..h.n {
                add(h.id, t.stats(x.stats), 1.0);
            }
        }
    }
    out
}

/// Every percentage modifier of a city's yields (`cities._pct_bonuses`, `cities.py:387-438`): a
/// golden age, the railroad to the capital, a puppet's penalty, the production penalty of units
/// over the supply, the `[n]% [stat]` uniques, those of what it builds now, its religion's
/// followers, and its buildings'.
fn pct_bonuses(g: &Game, c: CityId, construction: Option<Constructible>) -> Yields {
    let mut pct = Yields::default();
    let Some(city) = g.city(c) else { return pct };
    let r = g.rules();
    let t = r.uniques();
    let filters = t.filters();
    let v = g.view();
    let owner = city.owner();
    let Some(p) = g.player(owner) else { return pct };
    if p.econ.golden_age_turns > 0 {
        pct.add_to(Stat::Production, 20.0);
        pct.add_to(Stat::Culture, 20.0);
    }
    let rail_tech = r.improvements()[r.derived().known.railroad].tech_required;
    if g.has_tech(owner, rail_tech) && (is_capital(g, c) || memo::connected_by_rail(g, c)) {
        pct.add_to(Stat::Production, 25.0);
    }
    if city.puppet {
        pct.add_to(Stat::Science, -25.0);
        pct.add_to(Stat::Culture, -25.0);
    }
    if p.is_major() && memo::unit_supply_deficit(g, owner) > 0 {
        pct.add_to(Stat::Production, memo::unit_supply_penalty(g, owner));
    }
    let ctx = Ctx::city(&v, c);
    for h in uq::city(&v, c, UniqueType::StatPercentBonus, &ctx) {
        if let UniqueData::StatPercentBonus(x) = h.data() {
            for _ in 0..h.n {
                pct.add_to(x.stat, f64::from(x.percent));
            }
        }
    }
    for h in uq::city(&v, c, UniqueType::StatPercentBonusCities, &ctx) {
        if let UniqueData::StatPercentBonusCities(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                pct.add_to(x.stat, f64::from(x.percent));
            }
        }
    }
    match construction {
        Some(Constructible::Unit(u)) => {
            for h in uq::city(&v, c, UniqueType::PercentProductionUnits, &ctx) {
                if let UniqueData::PercentProductionUnits(x) = h.data()
                    && t.in_set(x.units, u)
                    && filters.city_matches(x.cities, &v, c, None)
                {
                    for _ in 0..h.n {
                        pct.add_to(Stat::Production, f64::from(x.percent));
                    }
                }
            }
        }
        Some(Constructible::Building(b)) => {
            let wonder = r.buildings()[b].any_wonder;
            let ty = if wonder {
                UniqueType::PercentProductionWonders
            } else {
                UniqueType::PercentProductionBuildings
            };
            for h in uq::city(&v, c, ty, &ctx) {
                let (set, percent, cities) = match *h.data() {
                    UniqueData::PercentProductionWonders(x) => (x.buildings, x.percent, x.cities),
                    UniqueData::PercentProductionBuildings(x) => (x.buildings, x.percent, x.cities),
                    _ => continue,
                };
                if t.in_set(set, b) && filters.city_matches(cities, &v, c, None) {
                    for _ in 0..h.n {
                        pct.add_to(Stat::Production, f64::from(percent));
                    }
                }
            }
            let cap = p.capital.and_then(|x| g.city(x));
            if cap.is_some_and(|x| x.buildings.contains(b)) {
                for h in uq::city(&v, c, UniqueType::PercentProductionBuildingsInCapital, &ctx) {
                    if let UniqueData::PercentProductionBuildingsInCapital(x) = h.data() {
                        for _ in 0..h.n {
                            pct.add_to(Stat::Production, f64::from(x.percent));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    for h in uq::city(&v, c, UniqueType::StatPercentFromReligionFollowers, &ctx) {
        if let UniqueData::StatPercentFromReligionFollowers(x) = h.data() {
            let followers = f64::from(religion::followers_of_majority(g, c));
            for _ in 0..h.n {
                pct.add_to(x.stat, (f64::from(x.percent) * followers).min(f64::from(x.cap)));
            }
        }
    }
    let bu = BuildingUniques::pct(&v, c, &ctx);
    for b in city.buildings.iter() {
        let bp = building_pct(g, b, &bu);
        pct.add(&bp.stats, 1.0);
        pct.keys |= bp.keys;
    }
    pct
}

/// The food a city's citizens eat each turn (`cities.food_eaten`, `cities.py:441-459`): two
/// each, the specialists' share times `[n]% Food consumption by specialists [cities]`, less
/// `[n]% Food consumption by [population] [cities]` of those it counts.
#[must_use]
pub fn food_eaten(g: &Game, c: CityId, work: &Work<'_>) -> f64 {
    let Some(city) = g.city(c) else { return 0.0 };
    let t = g.rules().uniques();
    let filters = t.filters();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let specs = f64::from(work.specialists_total());
    let mut by_specialists = 2.0 * specs;
    let mut eaten = f64::from(city.pop) * 2.0 - by_specialists;
    for h in uq::city(&v, c, UniqueType::FoodConsumptionBySpecialists, &ctx) {
        if let UniqueData::FoodConsumptionBySpecialists(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                by_specialists *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    eaten += by_specialists;
    for h in uq::city(&v, c, UniqueType::FoodConsumptionByPopulation, &ctx) {
        if let UniqueData::FoodConsumptionByPopulation(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            let amount = 2.0 * f64::from(population_amount(g, city, work, x.population));
            for _ in 0..h.n {
                eaten -= amount * (1.0 - (1.0 + f64::from(x.percent) / 100.0));
            }
        }
    }
    eaten
}

/// What `[n]% growth [cities]` adds to a city's food, by the kind of each source
/// (`cities.growth_bonus`, `cities.py:462-468`).
#[must_use]
pub fn growth_bonus(g: &Game, c: CityId, total_food: f64) -> SmallVec<[(SourceKind, f64); 2]> {
    let mut out: SmallVec<[(SourceKind, f64); 2]> = SmallVec::new();
    let t = g.rules().uniques();
    let filters = t.filters();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    for h in uq::city(&v, c, UniqueType::GrowthPercentBonus, &ctx) {
        if let UniqueData::GrowthPercentBonus(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            let kind = kind_of(g, h.id);
            let amount = f64::from(x.percent) / 100.0 * total_food;
            for _ in 0..h.n {
                match out.iter_mut().find(|(k, _)| *k == kind) {
                    Some((_, a)) => *a += amount,
                    None => out.push((kind, amount)),
                }
            }
        }
    }
    out
}

/// The production surplus food turns into, for the things that convert it
/// (`cities.production_from_excess_food`, `cities.py:471-483`).
#[must_use]
pub fn production_from_excess_food(food: f64) -> f64 {
    if food >= 4.0 {
        2.0 + (food / 4.0).trunc()
    } else if food >= 2.0 {
        2.0
    } else if food >= 1.0 {
        1.0
    } else {
        0.0
    }
}

/// Whether what a city builds turns its surplus food into production
/// (`cities.can_convert_food`, `cities.py:486-492`).
#[must_use]
pub fn can_convert_food(g: &Game, food: f64, construction: Option<Constructible>) -> bool {
    if food <= 0.0 {
        return false;
    }
    let r = g.rules();
    let ty = UniqueType::ConvertFoodToProductionWhenConstructed;
    match construction {
        Some(Constructible::Unit(u)) => has_type(r, &r.base_units()[u].uniques, ty),
        Some(Constructible::Building(b)) => has_type(r, &r.buildings()[b].uniques, ty),
        _ => false,
    }
}

/// What a city builds now (`cities.current_construction`, `cities.py:318-320`).
#[must_use]
pub fn current_construction(city: &City) -> Option<Constructible> {
    city.queue.first().copied()
}

/// A city's happiness and the parts of its yields its stats reuse (`cities._city_happiness`,
/// `cities.py:534-587`, and the first lines of `_compute_city_stats`): what its tiles, buildings
/// and specialists yield, its uniques by source, and its happiness list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CityParts {
    /// What its centre, worked tiles and free tiles yield.
    pub tiles: Stats,
    /// What its buildings yield.
    pub buildings: Stats,
    /// What its specialists yield.
    pub specialists: Stats,
    /// Its uniques' flat stats by source.
    pub by_source: SmallVec<[(SourceKind, Yields); 4]>,
    /// Its happiness by source; empty for a city that is not a major's.
    pub happiness: SmallVec<[(HappinessSource, f64); 8]>,
}

impl super::super::derive::rev::BitEq for CityParts {
    fn bit_eq(&self, other: &Self) -> bool {
        self.tiles.bit_eq(&other.tiles)
            && self.buildings.bit_eq(&other.buildings)
            && self.specialists.bit_eq(&other.specialists)
            && self.by_source.len() == other.by_source.len()
            && self
                .by_source
                .iter()
                .zip(&other.by_source)
                .all(|(a, b)| a.0 == b.0 && a.1.keys == b.1.keys && a.1.stats.bit_eq(&b.1.stats))
            && self.happiness.len() == other.happiness.len()
            && self
                .happiness
                .iter()
                .zip(&other.happiness)
                .all(|(a, b)| a.0 == b.0 && a.1.to_bits() == b.1.to_bits())
    }
}

impl CityParts {
    /// The city's happiness: the sum of its list.
    #[must_use]
    pub fn happiness_total(&self) -> f64 {
        self.happiness.iter().map(|&(_, x)| x).sum()
    }
}

/// A city's parts, with its citizens where `work` puts them.
#[must_use]
pub fn city_parts(g: &Game, c: CityId, work: &Work<'_>) -> CityParts {
    let mut out = CityParts::default();
    let Some(city) = g.city(c) else { return out };
    let owner = city.owner();
    let r = g.rules();
    for t in worked_or_free_tiles(g, c, work.worked) {
        out.tiles += memo::tile_yield(g, t, Some(owner), Some(c));
    }
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let bu = BuildingUniques::stats(&v, c, &ctx);
    for b in city.buildings.iter() {
        out.buildings += building_stats_with(g, &v, b, &ctx, &bu);
    }
    for (i, &n) in work.specialists.iter().enumerate() {
        if n > 0
            && let Some(s) = u8::try_from(i).ok().map(SpecialistId)
        {
            out.specialists.add_scaled(&specialist_stats(g, c, s), f64::from(n));
        }
    }
    out.by_source = uniques_by_source(g, c);
    let Some(p) = g.player(owner) else { return out };
    if !p.is_major() {
        return out;
    }
    // _city_happiness (cities.py:534-587).
    let t = r.uniques();
    let filters = t.filters();
    let mut unhap = r.difficulties()[g.difficulty(Some(owner))].unhappiness_modifier;
    if !g.is_humanlike(owner) {
        unhap *= r.difficulties()[g.seat_difficulty(Some(owner))].ai_unhappiness_modifier;
    }
    let annex = v_annexed(g, c);
    let from_city = -3.0 - if annex { 2.0 } else { 0.0 };
    let umod: f64 =
        uq::civ(&v, owner, UniqueType::UnhappinessFromCitiesPercentage, &Ctx::civ(owner))
            .map(|h| match h.data() {
                UniqueData::UnhappinessFromCitiesPercentage(x) => {
                    f64::from(x.percent) * f64::from(h.n)
                }
                _ => 0.0,
            })
            .sum();
    let mut hl: SmallVec<[(HappinessSource, f64); 8]> = SmallVec::new();
    hl.push((HappinessSource::Cities, from_city * unhap * (1.0 + umod / 100.0)));
    let mut citizens = f64::from(city.pop);
    for h in uq::city(&v, c, UniqueType::UnhappinessFromPopulationTypePercentageChange, &ctx) {
        if let UniqueData::UnhappinessFromPopulationTypePercentageChange(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            let amount = f64::from(population_amount(g, city, work, x.population));
            for _ in 0..h.n {
                citizens += f64::from(x.percent) / 100.0 * amount;
            }
        }
    }
    if annex {
        citizens *= 2.0;
    }
    let citizens = citizens.max(0.0);
    hl.push((HappinessSource::Population, -citizens * unhap));
    if annex {
        hl.push((HappinessSource::OccupiedCity, -2.0));
    }
    let spec_hap = out.specialists[Stat::Happiness].trunc();
    if spec_hap > 0.0 {
        hl.push((HappinessSource::Specialists, spec_hap));
    }
    hl.push((HappinessSource::Buildings, out.buildings[Stat::Happiness].trunc()));
    hl.push((HappinessSource::TileYields, out.tiles[Stat::Happiness]));
    for (kind, s) in &out.by_source {
        let x = s.get(Stat::Happiness);
        if x != 0.0 {
            let key = HappinessSource::Uniques(*kind);
            match hl.iter_mut().find(|(k, _)| *k == key) {
                Some((_, y)) => *y += x,
                None => hl.push((key, x)),
            }
        }
    }
    out.happiness = hl;
    out
}

/// Whether the city carries a conquered city's unhappiness (`cities.has_annex_unhappiness`).
fn v_annexed(g: &Game, c: CityId) -> bool {
    use crate::unique::FilterFacts as _;
    g.view().city_annex_unhappiness(c)
}

/// A city's full stats (`cities._compute_city_stats`, `cities.py:589-678`): the breakdown by
/// source, the total, and the percentages applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CityStats {
    /// `final`, in Python's order.
    pub breakdown: SmallVec<[(StatSource, Yields); 12]>,
    /// `total`, happiness included.
    pub total: Stats,
    /// `pct`.
    pub pct: Yields,
}

impl super::super::derive::rev::BitEq for CityStats {
    fn bit_eq(&self, other: &Self) -> bool {
        self.total.bit_eq(&other.total)
            && self.pct.keys == other.pct.keys
            && self.pct.stats.bit_eq(&other.pct.stats)
            && self.breakdown.len() == other.breakdown.len()
            && self
                .breakdown
                .iter()
                .zip(&other.breakdown)
                .all(|(a, b)| a.0 == b.0 && a.1.keys == b.1.keys && a.1.stats.bit_eq(&b.1.stats))
    }
}

impl CityStats {
    /// The surplus food (`food_surplus`).
    #[must_use]
    pub fn food(&self) -> f64 {
        self.total[Stat::Food]
    }

    /// The production (`production`).
    #[must_use]
    pub fn production(&self) -> f64 {
        self.total[Stat::Production]
    }
}

/// A city's stats from its parts, as it builds `construction`. `happy` is whether its owner is
/// happy enough for We Love The King Day's food; `None` reads the owner's happiness now, as the
/// stats do, where the citizen assignment passes the happiness committed (DESIGN.md 6.6).
#[must_use]
pub fn city_stats_from(
    g: &Game,
    c: CityId,
    parts: &CityParts,
    work: &Work<'_>,
    construction: Option<Constructible>,
    happy: Option<bool>,
) -> CityStats {
    let mut out = CityStats::default();
    let Some(city) = g.city(c) else { return out };
    let owner = city.owner();
    let major = g.player(owner).is_some_and(crate::state::players::Player::is_major);
    let mut fin: SmallVec<[(StatSource, Yields); 12]> = SmallVec::new();
    let mut pop = Yields::default();
    pop.put(Stat::Science, f64::from(city.pop));
    pop.put(Stat::Production, f64::from(work.free(city.pop).max(0)));
    fin.push((StatSource::Population, pop));
    fin.push((StatSource::TileYields, Yields::all(parts.tiles)));
    fin.push((StatSource::Specialists, Yields::all(parts.specialists)));
    fin.push((StatSource::TradeRoutes, trade_route_stats(g, c)));
    fin.push((StatSource::Buildings, Yields::all(parts.buildings)));
    for (kind, s) in &parts.by_source {
        let key = StatSource::Uniques(*kind);
        match fin.iter_mut().find(|(k, _)| *k == key) {
            Some((_, d)) => {
                d.stats += s.stats;
                d.keys |= s.keys;
            }
            None => fin.push((key, *s)),
        }
    }
    for (_, s) in &mut fin {
        s.remove(Stat::Happiness);
    }
    let pct = pct_bonuses(g, c, construction);
    for (_, s) in &mut fin {
        s.scale(Stat::Production, 1.0 + pct.get(Stat::Production) / 100.0);
    }
    let prod_total: f64 = fin.iter().map(|(_, s)| s.get(Stat::Production)).sum();
    if let Some(Constructible::Perpetual(k @ (Perpetual::Gold | Perpetual::Science))) = construction
    {
        let stat = if k == Perpetual::Gold { Stat::Gold } else { Stat::Science };
        let mut y = Yields::default();
        y.put(stat, prod_total * 0.25);
        fin.push((StatSource::Construction, y));
    }
    for (_, s) in &mut fin {
        for k in [Stat::Gold, Stat::Culture, Stat::Food, Stat::Faith] {
            s.scale(k, 1.0 + pct.get(k) / 100.0);
        }
    }
    for (_, s) in &mut fin {
        s.scale(Stat::Science, 1.0 + pct.get(Stat::Science) / 100.0);
    }
    let eaten = food_eaten(g, c, work);
    fin[0].1.add_to(Stat::Food, -eaten);
    let mut total_food: f64 = fin.iter().map(|(_, s)| s.get(Stat::Food)).sum();
    if total_food > 0.0 {
        for (kind, amount) in growth_bonus(g, c, total_food) {
            let key = StatSource::Growth(kind);
            match fin.iter_mut().find(|(k, _)| *k == key) {
                Some((_, d)) => d.add_to(Stat::Food, amount),
                None => {
                    let mut y = Yields::default();
                    y.add_to(Stat::Food, amount);
                    fin.push((key, y));
                }
            }
        }
        if city.wltkd > 0 && major {
            let happy = happy.unwrap_or_else(|| memo::happiness_total(g, owner) >= 0);
            if happy {
                let mut y = Yields::default();
                y.put(Stat::Food, total_food / 4.0);
                fin.push((StatSource::WeLoveTheKingDay, y));
            }
        }
        total_food = fin.iter().map(|(_, s)| s.get(Stat::Food)).sum();
    }
    let mut m = Yields::default();
    m.put(Stat::Gold, -maintenance(g, c).trunc());
    fin.push((StatSource::Maintenance, m));
    if can_convert_food(g, total_food, construction) {
        let mut y = Yields::default();
        y.put(Stat::Production, production_from_excess_food(total_food));
        y.put(Stat::Food, -total_food);
        fin.push((StatSource::ExcessFood, y));
    }
    let v = g.view();
    if uq::any(uq::city(&v, c, UniqueType::NullifiesGrowth, &Ctx::city(&v, c))) {
        let cur: f64 = fin.iter().map(|(_, s)| s.get(Stat::Food)).sum();
        if cur > 0.0 {
            let mut y = Yields::default();
            y.put(Stat::Food, -cur);
            fin.push((StatSource::Unhappiness, y));
        }
    }
    if city.resistance > 0 {
        fin.clear();
    }
    if fin.iter().map(|(_, s)| s.get(Stat::Production)).sum::<f64>() < 1.0 {
        let mut y = Yields::default();
        y.put(Stat::Production, 1.0);
        fin.push((StatSource::Production, y));
    }
    let mut total = Stats::ZERO;
    for (_, s) in &fin {
        total += s.stats;
    }
    total[Stat::Happiness] = parts.happiness_total();
    out.breakdown = fin;
    out.total = total;
    out.pct = pct;
    out
}

/// The food a city needs for its next citizen (`cities.food_to_next_pop`, `cities.py:686-701`):
/// more with each citizen, scaled by speed, half again for a city-state, and an AI major's by its
/// seat's `aiCityGrowthModifier`.
#[must_use]
pub fn food_to_next_pop(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let n = f64::from(city.pop) - 1.0;
    let mut req = 15.0 + 8.0 * n + num::pow(n, 1.5).floor();
    req *= g.speed().modifier;
    let owner = city.owner();
    let Some(p) = g.player(owner) else { return 0 };
    if p.is_city_state() {
        req *= 1.5;
    }
    if p.is_major() && !g.is_humanlike(owner) {
        req *= g.rules().difficulties()[g.seat_difficulty(Some(owner))].ai_city_growth_modifier;
    }
    num::trunc_i32(req)
}

// ---- Health, strength and production (cities.py:1091-1128, 1596-1618, 1872-1874) ----------------

/// A city's most hit points (`cities.max_health`, `cities.py:1872-1874`).
#[must_use]
pub fn max_health(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    200 + city.buildings.iter().map(|b| g.rules().buildings()[b].city_health).sum::<i32>()
}

/// A city's combat strength as it defends (`combat.city_strength`, `combat.py:149-174`): its
/// base, its population, its terrain, its owner's share of the techs, its garrison, its
/// buildings (times the owner's `[n]% Strength for cities`) and `[n] Strength`.
#[must_use]
pub fn city_strength(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let r = g.rules();
    let k = &r.constants().formulas;
    let v = g.view();
    let mut s = k.city_strength_base + f64::from(city.pop) * k.city_strength_per_pop;
    for h in uq::terrains(&v, city.tile(), UniqueType::GrantsCityStrength, &Ctx::IGNORE) {
        if let UniqueData::GrantsCityStrength(x) = h.data() {
            s += f64::from(x.strength);
        }
    }
    let n_techs = r.techs().len();
    #[allow(clippy::cast_precision_loss, reason = "tech counts are far below 2^52")]
    let pct = if n_techs == 0 {
        0.5
    } else {
        g.player(city.owner()).map_or(0, |p| p.tech.known.len()) as f64 / n_techs as f64
    };
    s += num::pow(pct * k.city_strength_from_techs_multiplier, k.city_strength_from_techs_exponent)
        * k.city_strength_from_techs_full_multiplier;
    if let Some(m) = g.military_at(city.tile()) {
        s += f64::from(r.base_units()[m.base].strength)
            * (f64::from(m.hp) / 100.0)
            * k.city_strength_from_garrison;
    }
    let mut bs: f64 = city.buildings.iter().map(|b| r.buildings()[b].city_strength).sum();
    let ctx = Ctx {
        civ: Some(city.owner()),
        city: Some(c),
        tile: Some(city.tile()),
        combat: Some(CombatCtx {
            our: Combatant::City(c),
            their: None,
            attacked_tile: None,
            action: Some(CombatAction::Defend),
        }),
        ..Ctx::default()
    };
    for h in uq::civ(&v, city.owner(), UniqueType::BetterDefensiveBuildings, &ctx) {
        if let UniqueData::BetterDefensiveBuildings(x) = h.data() {
            for _ in 0..h.n {
                bs *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    s += bs;
    for h in uq::city(&v, c, UniqueType::StrengthAmount, &ctx) {
        if let UniqueData::StrengthAmount(x) = h.data() {
            s += f64::from(x.strength) * f64::from(h.n);
        }
    }
    num::trunc_i32(num::round_half_even(s))
}

/// What something costs to build, in production (`cities.production_cost`,
/// `cities.py:1091-1128`): its cost, plus `Cost increases by [n] when built` for each one built
/// and `Cost increases by [n] per owned city`, times `[n]% production cost`; a city-state's half
/// again, a humanlike seat's difficulty's unit or building modifier, an AI major's its seat's; and
/// the speed's.
#[must_use]
pub fn production_cost(g: &Game, p: PlayerId, item: Constructible, city: Option<CityId>) -> i32 {
    let r = g.rules();
    let Some(pl) = g.player(p) else { return 0 };
    let (base_cost, uniques, is_unit, is_wonder) = match item {
        Constructible::Unit(u) => {
            let d = &r.base_units()[u];
            (d.cost, &d.uniques, true, false)
        }
        Constructible::Building(b) => {
            let d = &r.buildings()[b];
            (d.cost, &d.uniques, false, d.is_wonder)
        }
        Constructible::Perpetual(_) => return 0,
    };
    let mut cost = f64::from(base_cost);
    let v = g.view();
    let ctx = match city {
        Some(c) => Ctx::city(&v, c),
        None => Ctx::civ(p),
    };
    for h in uq::object(&v, uniques, UniqueType::CostIncreasesWhenBuilt, &ctx) {
        if let UniqueData::CostIncreasesWhenBuilt(x) = h.data() {
            cost += f64::from(built_increasing(g, p, item)) * f64::from(x.cost);
        }
    }
    for h in uq::object(&v, uniques, UniqueType::CostIncreasesPerCity, &ctx) {
        if let UniqueData::CostIncreasesPerCity(x) = h.data() {
            let n = i32::try_from(g.state().cities().of(p).len()).unwrap_or(i32::MAX);
            cost += f64::from(n) * f64::from(x.cost);
        }
    }
    for h in uq::object(&v, uniques, UniqueType::CostPercentageChange, &ctx) {
        if let UniqueData::CostPercentageChange(x) = h.data() {
            cost *= 1.0 + f64::from(x.percent) / 100.0;
        }
    }
    if pl.is_city_state() {
        cost *= 1.5;
    } else if g.is_humanlike(p) {
        let d = &r.difficulties()[g.difficulty(Some(p))];
        if is_unit {
            cost *= d.unit_cost_modifier;
        } else if !is_wonder {
            cost *= d.building_cost_modifier;
        }
    } else if pl.is_major() {
        let d = &r.difficulties()[g.seat_difficulty(Some(p))];
        cost *= if is_unit {
            d.ai_unit_cost_modifier
        } else if is_wonder {
            d.ai_wonder_cost_modifier
        } else {
            d.ai_building_cost_modifier
        };
    }
    cost *= g.speed().production_cost_modifier;
    num::trunc_i32(cost)
}

/// How many of an item a civilization has built of those that cost more each time
/// (`Player.built_increasing`).
fn built_increasing(g: &Game, p: PlayerId, item: Constructible) -> i32 {
    g.player(p).and_then(|pl| pl.civ.built_increasing.get(&item)).map_or(0, |&n| i32::from(n))
}

/// The production a city still needs to finish an item (`cities.remaining_work`,
/// `cities.py:1601-1605`).
#[must_use]
pub fn remaining_work(g: &Game, c: CityId, item: Constructible) -> f64 {
    let Some(city) = g.city(c) else { return 0.0 };
    if matches!(item, Constructible::Perpetual(_)) {
        return 0.0;
    }
    f64::from(production_cost(g, city.owner(), item, Some(c)))
        - city.progress.get(&item).copied().unwrap_or(0.0)
}

/// The turns a city takes to finish an item at its production now (`cities.turns_to_build`,
/// `cities.py:1608-1618`).
#[must_use]
pub fn turns_to_build(g: &Game, c: CityId, item: Constructible) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let left = remaining_work(g, c, item);
    if left <= 0.0 {
        return 0;
    }
    if left <= city.overflow {
        return 1;
    }
    let prod = if current_construction(city) == Some(item) {
        memo::city_stats(g, c).production()
    } else {
        let work = Work::of(city);
        let parts = memo::city_parts(g, c);
        city_stats_from(g, c, &parts, &work, Some(item), None).production()
    };
    let prod = num::round_half_even(prod).max(1.0);
    num::trunc_i32(((left - city.overflow) / prod).ceil())
}
