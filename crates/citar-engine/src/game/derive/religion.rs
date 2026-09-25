//! What religious pressure reads (DESIGN.md 6.5, 6.11): `CityNeighbours`, the cities by where
//! they stand, and each city's spread, which Python worked out again for every pair of cities each
//! turn (`religion.pressures_from_surroundings`, `religion.py:266-300`).
//!
//! - **The grid** ([`cities_within`]) puts the cities in square buckets of ten columns and ten rows
//!   of the map (the base reach of a religion), valid while no city is founded, taken or lost
//!   (`revs.cities`). A city's surroundings are the buckets its reach overlaps, wrapping as the
//!   map wraps, sorted by city id, as Python walked the cities.
//! - **A city's neighbours** ([`with_near`]): the other cities within the farthest any religion
//!   reaches of it, by id, each with its distance. A memo per city, valid while `revs.cities`
//!   stands and the reach is what it was found for, so a turn's pressure reads a list, not the
//!   grid.
//! - **The major religion a city follows** ([`major_religion`]): its majority, when that is a
//!   full religion and religion is in play. A memo per city on its `religion` and `core`
//!   revisions (its pressures and population), the religions and the settings. Pressure arrives
//!   in nearly every city each turn, so this is recomputed once per city a turn, and read by each
//!   neighbour the city presses.
//! - **A city's spread** ([`with_spread`]) is that religion, how far it reaches
//!   (`religion._spread_range`) and the multipliers of its natural pressure
//!   (`religion._pressure_to`), with the cities each holds for. It is a memo per city, with the
//!   classes its computation recorded (`unique::record`), valid while the major religion the city
//!   follows (the stamp of the memo above), its buildings, its owner's and that religion's
//!   founder's indexes, the religions and the settings stand. Pressure rarely changes which
//!   religion a city follows, so the pressures and the population are not read beside that
//!   religion (their conditionals are, through the recorded classes). The indexes' resource
//!   layers are read only for a ruleset whose resources carry a unique a spread reads; without
//!   them the indexes validate on the civilization's `index` and the city's `buildings`
//!   revisions alone.
//! - **The reach** ([`reach`]): how far any city's religion could reach, which the grid is asked
//!   for: ten tiles, and the most `Religion naturally spreads to cities [n] tiles away` could add,
//!   whatever its conditionals (the most any civilization's index and any religion's followers
//!   hold, twice the first for the city's owner and its religion's founder, and every building's
//!   and resource's own). A ruleset without the unique never asks; otherwise it is a memo on the
//!   civilizations' `index` revisions and the religions.
//!
//! [`verify`] is the cache oracle for all of them.

use core::cell::{Cell, Ref};

use smallvec::SmallVec;

use super::civ;
use super::rev::{BitEq, CopyMemo, Memo};
use crate::base::collections::LookupMap;
use crate::base::ids::{CityFilterId, CityId, PlayerId, ReligionId, TileIdx};
use crate::game::{Game, religion};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::{CondDeps, Ctx, IndexRef, UniqueData, UniqueType, record};

/// A religion's base reach, and the size of the grid's buckets (`religion.py:244`).
pub const BASE_REACH: i32 = 10;

/// How a city's majority religion spreads from it: `None` when it has no majority, or one that
/// is only a pantheon.
#[derive(Clone, Debug, PartialEq)]
pub struct SpreadSource {
    pub religion: ReligionId,
    pub tile: TileIdx,
    /// How far it reaches ([`religion::spread_range`]).
    pub range: i32,
    /// The multipliers of its natural pressure, with the cities each holds for
    /// ([`religion::spread_factors`]).
    pub factors: SmallVec<[(f64, CityFilterId); 2]>,
}

impl BitEq for SpreadSource {
    fn bit_eq(&self, other: &Self) -> bool {
        self.religion == other.religion
            && self.tile == other.tile
            && self.range == other.range
            && self.factors.len() == other.factors.len()
            && self
                .factors
                .iter()
                .zip(&other.factors)
                .all(|(a, b)| a.0.to_bits() == b.0.to_bits() && a.1 == b.1)
    }
}

/// The cities by bucket of the map, each with its tile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CityGrid {
    cols: i32,
    rows: i32,
    buckets: Vec<Vec<(CityId, TileIdx)>>,
}

impl BitEq for CityGrid {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// Cities, by id, each with its distance from some tile.
pub type Near = SmallVec<[(CityId, u32); 16]>;

/// A city's neighbours: the other cities within the reach, by id, with their distances.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NearCities(Near);

impl BitEq for NearCities {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// One city's memos.
#[derive(Clone, Debug)]
struct CityMemo {
    /// The major religion it follows.
    major: CopyMemo<Option<ReligionId>>,
    /// Its spread, with the classes its last computation recorded.
    source: Memo<Option<SpreadSource>>,
    deps: Cell<CondDeps>,
    /// Its neighbours, and the reach they were found within.
    near: Memo<NearCities>,
    near_radius: Cell<i32>,
}

impl Default for CityMemo {
    fn default() -> Self {
        Self {
            major: CopyMemo::new(),
            source: Memo::new(),
            deps: Cell::new(CondDeps::empty()),
            near: Memo::new(),
            near_radius: Cell::new(-1),
        }
    }
}

/// The grid and every city's memos: part of `Derived`.
#[derive(Clone, Debug)]
pub struct ReligionCaches {
    grid: Memo<CityGrid>,
    /// How far any city's religion could reach, for a ruleset with `Religion naturally spreads to
    /// cities [n] tiles away` and none on a resource.
    reach: CopyMemo<i32>,
    cities: LookupMap<CityId, CityMemo>,
    /// The most a building's or a resource's own `Religion naturally spreads to cities [n] tiles
    /// away` adds, and whether the ruleset has the unique at all.
    local_bonus: i32,
    has_distance: bool,
    /// Whether a resource carries a unique a spread reads, so that its layer must be validated.
    resource_spread: bool,
}

impl ReligionCaches {
    /// The caches of `st`, cold.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let mut cities = LookupMap::with_capacity(st.cities().len());
        for c in st.cities().iter() {
            cities.insert(c.id(), CityMemo::default());
        }
        let t = rules.uniques();
        let positive = |id| match t.get(id).data {
            UniqueData::ReligionSpreadDistance(x) => x.distance.max(0),
            _ => 0,
        };
        let has_distance =
            t.iter().any(|(_, u)| matches!(u.data, UniqueData::ReligionSpreadDistance(_)));
        let local_bonus = rules
            .buildings()
            .iter()
            .map(|(_, b)| &b.uniques)
            .chain(rules.resources().iter().map(|(_, r)| &r.uniques))
            .flat_map(|s| s.local.iter().copied())
            .map(positive)
            .fold(0i32, i32::saturating_add);
        let spread = |id| {
            matches!(
                t.get(id).data,
                UniqueData::ReligionSpreadDistance(_)
                    | UniqueData::NaturalReligionSpreadStrength(_)
            )
        };
        let resource_spread = rules
            .resources()
            .iter()
            .any(|(_, r)| r.uniques.civ.iter().chain(r.uniques.local.iter()).any(|&id| spread(id)));
        Self {
            grid: Memo::new(),
            reach: CopyMemo::new(),
            cities,
            local_bonus,
            has_distance,
            resource_spread,
        }
    }

    /// Keeps one set of memos per city of the state as cities come and go.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.cities.get_or_insert_with(c, CityMemo::default);
            }
            Change::CityRemoved { c, .. } => {
                self.cities.remove(&c);
            }
            _ => {}
        }
    }
}

/// The grid of the state's cities.
fn build_grid(g: &Game) -> CityGrid {
    let (w, h) = (i32::from(g.grid().width()), i32::from(g.grid().height()));
    let cols = (w + BASE_REACH - 1) / BASE_REACH;
    let rows = (h + BASE_REACH - 1) / BASE_REACH;
    let mut buckets = vec![Vec::new(); usize::try_from(cols * rows).unwrap_or(0)];
    for c in g.st.cities().iter() {
        let (x, y) = g.xy(c.tile());
        let i = usize::try_from((y / BASE_REACH) * cols + x / BASE_REACH).unwrap_or(0);
        if let Some(b) = buckets.get_mut(i) {
            b.push((c.id(), c.tile()));
        }
    }
    CityGrid { cols, rows, buckets }
}

/// The grid, valid while no city is founded, taken or lost.
fn grid(g: &Game) -> Ref<'_, CityGrid> {
    let revs = &g.dv.revs;
    g.dv.religion.grid.get(revs.now(), || revs.cities, || build_grid(g))
}

/// The buckets a range of rows or columns overlaps: `n` buckets over a side of `side` tiles,
/// wrapping if the map does.
fn bucket_span(from: i32, to: i32, n: i32, side: i32, wraps: bool) -> SmallVec<[i32; 8]> {
    let mut out: SmallVec<[i32; 8]> = SmallVec::new();
    if to - from + 1 >= side {
        out.extend(0..n);
        return out;
    }
    for v in from..=to {
        let v = if wraps { v.rem_euclid(side) } else { v };
        if !(0..side).contains(&v) {
            continue;
        }
        let b = v / BASE_REACH;
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

/// The cities within `radius` tiles of `at`, by id, each with its distance.
fn cities_near(g: &Game, at: TileIdx, radius: i32) -> Near {
    let grid_ = grid(g);
    let hex = g.grid();
    let (x, y) = g.xy(at);
    let (w, h) = (i32::from(hex.width()), i32::from(hex.height()));
    // No tile is farther than the map's width and height together: a reach a ruleset makes
    // larger reaches no further, and the spans below cannot overflow.
    let r = radius.clamp(0, w + h);
    let cols = bucket_span(x - r, x + r, grid_.cols, w, hex.wrap_x());
    let rows = bucket_span(y - r, y + r, grid_.rows, h, hex.wrap_y());
    let limit = u32::try_from(r).unwrap_or(0);
    let mut out = Near::new();
    for &by in &rows {
        for &bx in &cols {
            let Some(b) =
                usize::try_from(by * grid_.cols + bx).ok().and_then(|i| grid_.buckets.get(i))
            else {
                continue;
            };
            for &(c, t) in b {
                let d = hex.distance(t, at);
                if d <= limit {
                    out.push((c, d));
                }
            }
        }
    }
    out.sort_by_key(|&(c, _)| c.get());
    out
}

/// The cities within `radius` tiles of `at`, by id (`CityNeighbours`).
#[must_use]
pub fn cities_within(g: &Game, at: TileIdx, radius: i32) -> SmallVec<[CityId; 16]> {
    cities_near(g, at, radius).into_iter().map(|(c, _)| c).collect()
}

/// City `c`'s neighbours, found afresh.
fn compute_near(g: &Game, c: CityId, radius: i32) -> NearCities {
    let Some(at) = g.city(c).map(crate::state::cities::City::tile) else {
        return NearCities::default();
    };
    let mut near = cities_near(g, at, radius);
    near.retain(|&mut (o, _)| o != c);
    NearCities(near)
}

/// Calls `f` with the other cities within the farthest any religion reaches of city `c`
/// ([`reach`]), by id, each with its distance (see the module's doc).
pub fn with_near<R>(g: &Game, c: CityId, f: impl FnOnce(&[(CityId, u32)]) -> R) -> R {
    let radius = reach(g);
    let Some(m) = g.dv.religion.cities.get(&c) else { return f(&compute_near(g, c, radius).0) };
    let revs = &g.dv.revs;
    let inputs = || if m.near_radius.get() == radius { revs.cities } else { revs.now() };
    let compute = || {
        m.near_radius.set(radius);
        compute_near(g, c, radius)
    };
    let near = m.near.get(revs.now(), inputs, compute);
    f(&near.0)
}

/// How far any city's religion could reach now (see the module's doc).
#[must_use]
pub fn reach(g: &Game) -> i32 {
    let caches = &g.dv.religion;
    if !caches.has_distance {
        return BASE_REACH;
    }
    let t = g.rules().uniques();
    let run_bonus = |ix: IndexRef<'_>| -> i32 {
        ix.get(UniqueType::ReligionSpreadDistance)
            .iter()
            .map(|e| match t.get(e.id).data {
                UniqueData::ReligionSpreadDistance(x) => {
                    x.distance.max(0).saturating_mul(i32::from(e.n))
                }
                _ => 0,
            })
            .fold(0, i32::saturating_add)
    };
    // Without the unique on a resource, the indexes without their resource layers hold every
    // copy of it, and validate on the civilizations' `index` alone.
    let full = caches.resource_spread;
    let compute = || {
        let civ_most =
            g.st.players()
                .ids()
                .map(|p: PlayerId| {
                    run_bonus(if full { civ::civ_index_full(g, p) } else { civ::civ_index(g, p) })
                })
                .max()
                .unwrap_or(0);
        let follower_most = (0..g.st.world().religions.len())
            .filter_map(|i| u8::try_from(i).ok())
            .map(|i| run_bonus(civ::follower(g, ReligionId(i))))
            .max()
            .unwrap_or(0);
        BASE_REACH
            .saturating_add(civ_most.saturating_mul(2))
            .saturating_add(follower_most)
            .saturating_add(caches.local_bonus)
    };
    if full {
        return compute();
    }
    let revs = &g.dv.revs;
    let inputs = || {
        g.st.players()
            .ids()
            .map(|p| revs.civ(p).index)
            .fold(revs.religions.max(revs.config), |a, b| a.max(b))
    };
    caches.reach.get(revs.now(), inputs, compute)
}

/// The major religion city `c` follows, computed afresh.
fn compute_major(g: &Game, c: CityId) -> Option<ReligionId> {
    religion::majority_religion(g, c).filter(|&r| religion::is_major(g, r))
}

/// The major religion city `c` follows: its majority, if that is a full religion and religion is
/// in play (see the module's doc).
#[must_use]
pub fn major_religion(g: &Game, c: CityId) -> Option<ReligionId> {
    let Some(m) = g.dv.religion.cities.get(&c) else { return compute_major(g, c) };
    let revs = &g.dv.revs;
    let inputs = || {
        let cr = revs.city(c);
        cr.religion.max(cr.core).max(revs.religions).max(revs.config)
    };
    m.major.get(revs.now(), inputs, || compute_major(g, c))
}

/// How city `c`'s religion spreads, computed afresh.
fn compute_source(g: &Game, c: CityId) -> Option<SpreadSource> {
    let r = compute_major(g, c)?;
    let tile = g.city(c)?.tile();
    Some(SpreadSource {
        religion: r,
        tile,
        range: religion::spread_range(g, c),
        factors: religion::spread_factors(g, c),
    })
}

/// Calls `f` with how city `c`'s religion spreads (see the module's doc): `None` if it follows no
/// major religion.
pub fn with_spread<R>(g: &Game, c: CityId, f: impl FnOnce(Option<&SpreadSource>) -> R) -> R {
    let caches = &g.dv.religion;
    let Some(m) = caches.cities.get(&c) else { return f(compute_source(g, c).as_ref()) };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let cr = revs.city(c);
        let deps = m.deps.get();
        // Pressure arrives in a city every turn and moves its religion's revision, but its spread
        // depends on the major religion it follows, which it rarely changes: that memo's stamp
        // is read, and the city's pressures and population are not.
        let major = major_religion(g, c);
        let mut r = m
            .major
            .changed()
            .max(cr.buildings)
            .max(revs.civ(owner).index)
            .max(revs.religions)
            .max(revs.config);
        // The indexes without their resource layers validate on the civilization's `index` and
        // the city's `buildings` alone; a ruleset whose resources carry no unique a spread reads
        // leaves the layers out, since the runs a spread reads are the same without them.
        if caches.resource_spread {
            r = r
                .max(civ::civ_index_full_changed(g, owner))
                .max(civ::city_local_full_changed(g, c));
        }
        if !deps.is_empty() {
            r = r.max(civ::cond(g, deps, &Ctx::city(&g.view(), c)));
        }
        if let Some(founder) = major.and_then(|x| religion::religion(g, x)).map(|x| x.founder) {
            r = r.max(revs.civ(founder).index);
            if caches.resource_spread {
                r = r.max(civ::civ_index_full_changed(g, founder));
            }
            if !deps.is_empty() {
                r = r.max(civ::cond(g, deps, &Ctx::civ(founder)));
            }
        }
        r
    };
    let compute = || {
        let (v, d) = record::recorded(|| compute_source(g, c));
        m.deps.set(d);
        v
    };
    let source = m.source.get(revs.now(), inputs, compute);
    f(source.as_ref())
}

/// How city `c`'s religion spreads ([`with_spread`]), as a copy.
#[must_use]
pub fn spread_source(g: &Game, c: CityId) -> Option<SpreadSource> {
    with_spread(g, c, |s| s.cloned())
}

/// Every city's memos and the grid and reach, validated, against a cold rebuild from the same
/// state: one line for each that disagrees (the cache oracle, DESIGN.md 9.4).
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
    if *grid(g) != *grid(&cold) {
        out.push("the religious grid of cities differs from a cold rebuild".to_owned());
    }
    // The reach bounds every city's search: one too small would miss pressure without a trace.
    let (warm, fresh) = (reach(g), reach(&cold));
    if warm != fresh {
        out.push(format!("the religious reach is {warm}, a cold rebuild's {fresh}"));
    }
    for city in g.st.cities().iter() {
        let c = city.id();
        if !g.dv.religion.cities.contains_key(&c) {
            out.push(format!("city {}: no religious memos", c.get()));
            continue;
        }
        if major_religion(g, c) != major_religion(&cold, c) {
            out.push(format!("city {}: its major religion differs from a cold rebuild", c.get()));
        }
        let (warm, fresh) = (spread_source(g, c), spread_source(&cold, c));
        if !warm.bit_eq(&fresh) {
            out.push(format!("city {}: its religious spread differs from a cold rebuild", c.get()));
        }
        let same = with_near(g, c, |a| with_near(&cold, c, |b| a == b));
        if !same {
            out.push(format!("city {}: its neighbours differ from a cold rebuild", c.get()));
        }
    }
    out
}

/// Every city's surroundings asked once, as a round of city turns asks them: the pressure they
/// add up to. The bench's round (feature `test-ops`).
#[cfg(any(test, feature = "test-ops"))]
#[must_use]
pub fn surroundings_for_bench(g: &Game) -> i64 {
    let ids: Vec<CityId> = g.st.cities().iter().map(crate::state::cities::City::id).collect();
    ids.into_iter()
        .flat_map(|c| religion::pressures_from_surroundings(g, c))
        .map(|(_, n)| i64::from(n))
        .sum()
}
