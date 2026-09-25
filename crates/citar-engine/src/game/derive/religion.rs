//! What religious pressure reads (DESIGN.md 6.5, 6.11): `CityNeighbours`, the cities by where
//! they stand, and each city's spread, which Python worked out again for every pair of cities each
//! turn (`religion.pressures_from_surroundings`, `religion.py:266-300`).
//!
//! - **The grid** ([`cities_within`]) puts the cities in square buckets of ten columns and ten rows
//!   of the map (the base reach of a religion), valid while no city is founded, taken or lost
//!   (`revs.cities`). A city's surroundings are the buckets its reach overlaps, wrapping as the
//!   map wraps, sorted by city id, as Python walked the cities.
//! - **A city's spread** ([`spread_source`]) is its majority religion when that is a major one,
//!   how far it reaches (`religion._spread_range`) and the multipliers of its natural pressure
//!   (`religion._pressure_to`), with the cities each holds for. It is a memo per city, with the
//!   classes its computation recorded (`unique::record`), valid while the city's pressures,
//!   population and buildings, its owner's and its majority's founder's indexes, the religions and
//!   the settings stand. So a round asks each city's uniques about once, and applying pressure to
//!   one city recomputes that city's spread alone.
//! - **The reach** ([`reach`]): how far any city's religion could reach, which the grid is asked
//!   for: ten tiles, and the most `Religion naturally spreads to cities [n] tiles away` could add,
//!   whatever its conditionals (the most any civilization's index and any religion's followers
//!   hold, twice the first for the city's owner and its religion's founder, and every building's
//!   and resource's own). A ruleset without the unique never asks.
//!
//! [`verify`] is the cache oracle for both.

use core::cell::{Cell, Ref};

use smallvec::SmallVec;

use super::civ;
use super::rev::{BitEq, Memo};
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

/// One city's spread, with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct SourceMemo {
    source: Memo<Option<SpreadSource>>,
    deps: Cell<CondDeps>,
}

impl Default for SourceMemo {
    fn default() -> Self {
        Self { source: Memo::new(), deps: Cell::new(CondDeps::empty()) }
    }
}

/// The grid and every city's spread: part of `Derived`.
#[derive(Clone, Debug)]
pub struct ReligionCaches {
    grid: Memo<CityGrid>,
    sources: LookupMap<CityId, SourceMemo>,
    /// The most a building's or a resource's own `Religion naturally spreads to cities [n] tiles
    /// away` adds, and whether the ruleset has the unique at all.
    local_bonus: i32,
    has_distance: bool,
}

impl ReligionCaches {
    /// The caches of `st`, cold.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let mut sources = LookupMap::with_capacity(st.cities().len());
        for c in st.cities().iter() {
            sources.insert(c.id(), SourceMemo::default());
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
        Self { grid: Memo::new(), sources, local_bonus, has_distance }
    }

    /// Keeps one spread per city of the state as cities come and go.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.sources.get_or_insert_with(c, SourceMemo::default);
            }
            Change::CityRemoved { c, .. } => {
                self.sources.remove(&c);
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

/// The cities within `radius` tiles of `at`, by id (`CityNeighbours`).
#[must_use]
pub fn cities_within(g: &Game, at: TileIdx, radius: i32) -> SmallVec<[CityId; 16]> {
    let grid_ = grid(g);
    let hex = g.grid();
    let (x, y) = g.xy(at);
    let r = radius.max(0);
    let (w, h) = (i32::from(hex.width()), i32::from(hex.height()));
    let cols = bucket_span(x - r, x + r, grid_.cols, w, hex.wrap_x());
    let rows = bucket_span(y - r, y + r, grid_.rows, h, hex.wrap_y());
    let limit = u32::try_from(r).unwrap_or(0);
    let mut out: SmallVec<[CityId; 16]> = SmallVec::new();
    for &by in &rows {
        for &bx in &cols {
            let Some(b) =
                usize::try_from(by * grid_.cols + bx).ok().and_then(|i| grid_.buckets.get(i))
            else {
                continue;
            };
            out.extend(b.iter().filter(|&&(_, t)| hex.distance(t, at) <= limit).map(|&(c, _)| c));
        }
    }
    out.sort_by_key(|c| c.get());
    out
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
    let civ_most =
        g.st.players()
            .ids()
            .map(|p: PlayerId| run_bonus(civ::civ_index_full(g, p)))
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
}

/// How city `c`'s religion spreads, computed afresh.
fn compute_source(g: &Game, c: CityId) -> Option<SpreadSource> {
    let r = religion::majority_religion(g, c)?;
    if !religion::is_major(g, r) {
        return None;
    }
    let tile = g.city(c)?.tile();
    Some(SpreadSource {
        religion: r,
        tile,
        range: religion::spread_range(g, c),
        factors: religion::spread_factors(g, c),
    })
}

/// How city `c`'s religion spreads (see the module's doc); `None` if it has no major religion as
/// its majority.
pub fn spread_source(g: &Game, c: CityId) -> Option<SpreadSource> {
    let caches = &g.dv.religion;
    let Some(m) = caches.sources.get(&c) else { return compute_source(g, c) };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let cr = revs.city(c);
        let v = g.view();
        let deps = m.deps.get();
        let mut r = cr
            .core
            .max(cr.religion)
            .max(cr.buildings)
            .max(revs.civ(owner).index)
            .max(civ::civ_index_full_changed(g, owner))
            .max(civ::city_local_full_changed(g, c))
            .max(revs.religions)
            .max(revs.config)
            .max(civ::cond(g, deps, &Ctx::city(&v, c)));
        if let Some(founder) = religion::majority_religion(g, c)
            .and_then(|x| religion::religion(g, x))
            .map(|x| x.founder)
        {
            r = r
                .max(revs.civ(founder).index)
                .max(civ::civ_index_full_changed(g, founder))
                .max(civ::cond(g, deps, &Ctx::civ(founder)));
        }
        r
    };
    let compute = || {
        let (v, d) = record::recorded(|| compute_source(g, c));
        m.deps.set(d);
        v
    };
    m.source.get(revs.now(), inputs, compute).clone()
}

/// The grid and every city's spread, validated, against a cold rebuild from the same state: one
/// line for each that disagrees (the cache oracle, DESIGN.md 9.4).
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
    if *grid(g) != *grid(&cold) {
        out.push("the religious grid of cities differs from a cold rebuild".to_owned());
    }
    for city in g.st.cities().iter() {
        let c = city.id();
        if !g.dv.religion.sources.contains_key(&c) {
            out.push(format!("city {}: no religious spread", c.get()));
            continue;
        }
        let (warm, fresh) = (spread_source(g, c), spread_source(&cold, c));
        if !warm.bit_eq(&fresh) {
            out.push(format!("city {}: its religious spread differs from a cold rebuild", c.get()));
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
