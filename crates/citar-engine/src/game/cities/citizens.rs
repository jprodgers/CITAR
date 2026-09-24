//! Where a city's citizens work (`cities.py:706-926`, UnCiv's `CityPopulationManager` and
//! `Automation.rankStatsForCityWork`), and the tools that tell them (`tools.py:679-757`).
//!
//! Citizens are always reassigned in full, greedily from the tiles the player locked
//! (`assign_citizens`, `auto_assign_population`, `_unassign_extra`): each free citizen takes the
//! best of the workable tiles and the specialist slots by [`rank_stats_for_work`], ties going to
//! the larger column, then row. Assignment happens only in settle (DESIGN.md 6.7): a write flags
//! the cities it concerns, and the settle's passes reassign them in id order, to a fixed point. A
//! city whose assignment takes or releases a tile flags the cities of its owner that could work
//! it. The citizen oracle ([`verify`]) checks every city this engine has assigned against a fresh
//! assignment.
//!
//! The ranking is hoisted per city ([`RankCtx`], DESIGN.md 6.11): the focus, the growth terms,
//! the construction, the bands of happiness and the sign of the gold rate.
//!
//! What differs from Python, on purpose:
//! - the ranking reads the happiness committed at the civilization's fixed stages
//!   (`happiness-seen-committed`), not the live figure, and the gold rate written at stage E2
//!   (`last-gold-rate-written`), where Python read one nothing wrote;
//! - a city's food with the citizens placed so far reads We Love The King Day's food by the
//!   happiness committed too;
//! - a city's worked and locked tiles are sets, so a city whose citizens are all locked refuses
//!   another lock (`work-tile-refuses-a-lock-past-the-citizens`), where Python dropped its oldest;
//! - the tools list a city's tiles by column and then row, as `inspect` does
//!   (`citizen-tools-list-tiles-sorted`), where Python listed them in the order they were taken.

use smallvec::SmallVec;

use super::super::Game;
use super::super::action::{OutcomeSpec, Rule};
use super::super::derive::rev::CityTouch;
use super::super::derive::stats as memo;
use super::super::error::{ActionError, ErrCode};
use super::stats::{
    self as cstats, Work, converts_food, current_construction, food_to_next_pop, growth_bonus,
    max_specialists, production_from_excess_food, slots, specialist_stats,
};
use crate::base::ids::{CityId, PlayerId, SpecialistId, TileIdx};
use crate::base::py;
use crate::base::sets::MAX_SPECIALISTS;
use crate::base::stats::{Stat, Stats};
use crate::state::cities::{City, CityFocus, Constructible};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};
use serde_json::{Map, Value, json};

/// The weight of each stat in a citizen's rank (`cities._WEIGHTS`, `cities.py:706`), in stat
/// order.
const WEIGHTS: [f64; Stat::COUNT] = [14.0, 12.01, 6.0, 9.01, 8.0, 10.0, 7.0];

/// What a focus multiplies (`cities.FOCUSES`, `cities.py:25-31`).
fn focus_weights(f: CityFocus) -> &'static [(Stat, f64)] {
    match f {
        CityFocus::Balanced | CityFocus::Manual => &[],
        CityFocus::Food => &[(Stat::Food, 3.05)],
        CityFocus::Production => &[(Stat::Production, 3.05)],
        CityFocus::Gold => &[(Stat::Gold, 3.05)],
        CityFocus::Science => &[(Stat::Science, 3.05)],
        CityFocus::Culture => &[(Stat::Culture, 3.05)],
        CityFocus::Faith => &[(Stat::Faith, 3.05)],
        CityFocus::Happiness => &[(Stat::Happiness, 3.05)],
        CityFocus::GoldGrowth => &[(Stat::Gold, 2.0), (Stat::Food, 1.5)],
        CityFocus::ProductionGrowth => &[(Stat::Production, 2.0), (Stat::Food, 1.5)],
    }
}

/// Everything a city's ranking of its tiles and specialists reads that does not change while it
/// assigns (DESIGN.md 6.11).
#[derive(Clone, Debug)]
pub struct RankCtx {
    focus: CityFocus,
    /// The percentages of `[n]% Food consumption by specialists` that hold, summed with copies.
    specialist_food_pct: f64,
    construction: Option<Constructible>,
    /// What it builds turns surplus food into production.
    converts: bool,
    avoid_growth: bool,
    nullifies_growth: bool,
    /// The percentages of `[n]% growth` that hold, summed with copies.
    growth_pct: f64,
    wltkd: bool,
    /// The happiness committed: citizen ranking's bands (DESIGN.md 6.6).
    happiness: i32,
    bot_managed: bool,
    pop: u16,
    /// A balanced city's growth is worth less above this food.
    balanced_threshold: f64,
    /// The gold rate committed at E2 is negative.
    broke: bool,
}

impl RankCtx {
    /// The context of city `c`'s ranking.
    #[must_use]
    pub fn of(g: &Game, c: CityId) -> Option<Self> {
        let city = g.city(c)?;
        let owner = city.owner();
        let p = g.player(owner)?;
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        let filters = g.rules().uniques().filters();
        let mut specialist_food_pct = 0.0;
        for h in uq::city(&v, c, UniqueType::FoodConsumptionBySpecialists, &ctx) {
            if let UniqueData::FoodConsumptionBySpecialists(x) = h.data()
                && filters.city_matches(x.cities, &v, c, None)
            {
                specialist_food_pct += f64::from(x.percent) * f64::from(h.n);
            }
        }
        let growth_pct: f64 = growth_bonus(g, c, 100.0).iter().map(|&(_, a)| a).sum();
        // refcheck: happiness-seen-committed
        let happiness = if p.is_major() { p.econ.happiness_seen } else { 0 };
        Some(Self {
            focus: city.focus,
            specialist_food_pct,
            construction: current_construction(city),
            converts: converts_food(g, current_construction(city)),
            avoid_growth: city.avoid_growth,
            nullifies_growth: uq::any(uq::city(&v, c, UniqueType::NullifiesGrowth, &ctx)),
            growth_pct,
            wltkd: city.wltkd > 0,
            happiness,
            bot_managed: p.seat().controller().is_bot_managed(),
            pop: city.pop,
            balanced_threshold: f64::from(food_to_next_pop(g, c)) / (10.0 * g.speed().modifier),
            // refcheck: last-gold-rate-written
            broke: p.econ.last_gold_rate < 0.0,
        })
    }

    fn perpetual(&self) -> bool {
        matches!(self.construction, Some(Constructible::Perpetual(_)))
    }
}

/// A set of yields scored for a citizen (`cities.rank_stats_for_work`, `cities.py:709-773`):
/// food that ends starvation counts eightfold, growth counts unless it is not wanted, a small
/// city's science is halved, gold counts double when the treasury shrinks and happiness when the
/// empire is unhappy, and the focus weighs last.
///
/// The rank sums its food's worth (`rank_food`) and the rest (`rank_rest`); only the first reads
/// the city's food, so citizen assignment weighs the rest of each tile once.
#[must_use]
pub fn rank_stats_for_work(rc: &RankCtx, stats: &Stats, specialist: bool, surplus: f64) -> f64 {
    let mut y = *stats;
    if specialist {
        y[Stat::Food] -= rc.specialist_food_pct / 100.0 * 2.0;
        #[allow(clippy::float_cmp, reason = "Python compared the yield with 3 exactly")]
        let three = y[Stat::Science] == 3.0;
        if three || y[Stat::Science] >= 5.0 {
            y[Stat::Science] *= 1.3;
        }
    }
    // `cities.can_convert_food`.
    if surplus > 0.0 && rc.converts {
        y[Stat::Production] += production_from_excess_food(surplus + y[Stat::Food])
            - production_from_excess_food(surplus);
        y[Stat::Food] = 0.0;
    }
    rank_rest(rc, &y) + rank_food(rc, y[Stat::Food], surplus)
}

/// What a tile's or slot's food is worth to a city with `surplus` food: eightfold what ends
/// starvation, and its growth unless the city avoids growing, less when the empire is very
/// unhappy or a balanced city already grows well; times the focus on food.
fn rank_food(rc: &RankCtx, food: f64, surplus: f64) -> f64 {
    let feed = if surplus < 0.0 { food.min(-surplus) } else { 0.0 }.max(0.0);
    let growth = if rc.avoid_growth { 0.0 } else { food - feed };
    let food_w = WEIGHTS[Stat::Food.index()];
    let mut v = feed * food_w * 8.0;
    let hap = rc.happiness;
    if !rc.nullifies_growth {
        let mut ng = if growth > 0.0 { growth + rc.growth_pct / 100.0 * growth } else { growth };
        if rc.wltkd && hap >= 0 {
            ng += growth / 4.0;
        }
        let ng = ng.max(0.0);
        let fmod = if hap < -8 {
            0.0
        } else if rc.bot_managed {
            1.5
        } else if rc.focus == CityFocus::Balanced {
            if rc.pop < 5 {
                2.0
            } else if surplus > rc.balanced_threshold {
                0.75
            } else {
                1.0
            }
        } else {
            1.0
        };
        v += ng * food_w * fmod;
    }
    for &(k, m) in focus_weights(rc.focus) {
        if k == Stat::Food {
            v *= m;
        }
    }
    v
}

/// What every yield but food is worth: each by its weight, a small city's science halved, gold
/// doubled when the treasury shrinks and happiness when the empire is unhappy, production of a
/// city that builds nothing a sixth, and the focus.
fn rank_rest(rc: &RankCtx, y: &Stats) -> f64 {
    let mut w = WEIGHTS;
    w[Stat::Food.index()] = 0.0;
    if rc.pop < 10 {
        w[Stat::Science.index()] /= 2.0;
    }
    if rc.broke {
        w[Stat::Gold.index()] *= 2.0;
    }
    if rc.happiness < 0 {
        w[Stat::Happiness.index()] *= 2.0;
    }
    if rc.perpetual() {
        w[Stat::Production.index()] /= 6.0;
    }
    for &(k, m) in focus_weights(rc.focus) {
        if k != Stat::Food {
            w[k.index()] *= m;
        }
    }
    let mut v = 0.0;
    for k in Stat::ALL {
        if k != Stat::Food {
            v += y[k] * w[k.index()];
        }
    }
    v
}

/// The percentage bonus of great person points in a city (`great_people.city_gpp_bonus`,
/// `great_people.py:19-32`): `[n]% Great Person generation [cities]`, and for each friendship in
/// force either side's `[n]% Great Person generation with declared friendships`... bonuses.
#[must_use]
pub fn city_gpp_bonus(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let filters = g.rules().uniques().filters();
    let mut total = 0i32;
    for h in uq::city(&v, c, UniqueType::GreatPersonPointPercentage, &ctx) {
        if let UniqueData::GreatPersonPointPercentage(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            total = total.saturating_add(x.percent.saturating_mul(i32::from(h.n)));
        }
    }
    let p = city.owner();
    let boost = |q: PlayerId| -> i32 {
        uq::civ(&v, q, UniqueType::GreatPersonBoostWithFriendship, &Ctx::civ(q))
            .map(|h| match h.data() {
                UniqueData::GreatPersonBoostWithFriendship(x) => {
                    x.percent.saturating_mul(i32::from(h.n))
                }
                _ => 0,
            })
            .sum()
    };
    for q in g.state().players().ids() {
        if q == p {
            continue;
        }
        if g.relation(p, q).is_some_and(|r| r.friendship_until >= g.turn()) {
            total = total.saturating_add(boost(p)).saturating_add(boost(q));
        }
    }
    total
}

/// A specialist slot scored, with the great person points it earns
/// (`cities.rank_specialist`, `cities.py:791-800`); `gpp` is the city's [`city_gpp_bonus`],
/// the same for every slot.
fn rank_specialist(
    g: &Game,
    rc: &RankCtx,
    c: CityId,
    s: SpecialistId,
    surplus: f64,
    gpp: i32,
) -> f64 {
    let stats = specialist_stats(g, c, s);
    let mut r = rank_stats_for_work(rc, &stats, true, surplus);
    if let Some(sp) = g.rules().specialists().get(s) {
        let points: i32 = sp.great_person_points.iter().map(|&(_, n)| n).sum();
        r += f64::from(points) * f64::from(100 + gpp) / 100.0;
    }
    r
}

/// Where a city's citizens are: its worked and locked tiles, sorted, and its specialists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub worked: Vec<TileIdx>,
    pub locked: Vec<TileIdx>,
    pub specialists: [u8; MAX_SPECIALISTS],
}

impl Assignment {
    /// A city's own.
    #[must_use]
    pub fn of(city: &City) -> Self {
        Self {
            worked: city.worked.clone(),
            locked: city.locked.clone(),
            specialists: city.specialists,
        }
    }

    fn free(&self, pop: u16) -> i32 {
        Work { worked: &self.worked, specialists: self.specialists }.free(pop)
    }
}

/// Where city `c`'s citizens would be put now (`cities.assign_citizens`, `cities.py:803-815`):
/// its locks on tiles it can still work, then every free citizen to the best tile or specialist
/// slot; with `reset`, its locks dropped first. Reads only.
#[must_use]
pub fn assign(g: &Game, c: CityId, reset: bool) -> Option<Assignment> {
    let city = g.city(c)?;
    let avail = cstats::workable_tiles(g, c);
    let mut a =
        Assignment { worked: Vec::new(), locked: Vec::new(), specialists: [0; MAX_SPECIALISTS] };
    if !reset {
        a.locked = city.locked.iter().copied().filter(|t| avail.contains(t)).collect();
    }
    a.worked = a.locked.iter().copied().take(usize::from(city.pop)).collect();
    let maxs = max_specialists(g, c);
    if city.manual_specialists {
        for (i, &n) in city.specialists.iter().enumerate() {
            let cap = u8::try_from(i).ok().map_or(0, |s| slots(&maxs, SpecialistId(s)));
            a.specialists[i] = u8::try_from(i32::from(n).min(cap).max(0)).unwrap_or(0);
        }
    }
    auto_assign(g, c, city, &avail, &maxs, &mut a);
    a.worked.sort();
    Some(a)
}

/// A tile a citizen could take: where it is, what it yields, its coordinates for ties, and what
/// its yields but food are worth ([`rank_rest`]).
type Candidate = (TileIdx, Stats, (i32, i32), f64);

/// Puts every free citizen somewhere, best first (`cities.auto_assign_population`,
/// `cities.py:818-872`), or takes the extra ones off (`_unassign_extra`).
fn auto_assign(
    g: &Game,
    c: CityId,
    city: &City,
    avail: &[TileIdx],
    maxs: &[(SpecialistId, i32)],
    a: &mut Assignment,
) {
    let free = a.free(city.pop);
    let Some(rc) = RankCtx::of(g, c) else { return };
    if free <= 0 {
        unassign_extra(g, c, city, avail, maxs, &rc, a);
        return;
    }
    let owner = city.owner();
    let work = Work { worked: &a.worked, specialists: a.specialists };
    // refcheck: happiness-seen-committed
    let happy = rc.happiness >= 0;
    let mut surplus = cstats::food_surplus(g, c, &work, rc.construction, happy);
    let tiles: SmallVec<[Candidate; 32]> = avail
        .iter()
        .copied()
        .filter(|t| !a.worked.contains(t) && !cstats::provides_yield_without_pop(g, *t))
        .map(|t| {
            let s = memo::tile_yield(g, t, Some(owner), Some(c));
            (t, s, g.xy(t), rank_rest(&rc, &s))
        })
        .collect();
    let spec_food_bonus = specialist_food_bonus(g, c);
    let gpp = if city.manual_specialists || maxs.is_empty() { 0 } else { city_gpp_bonus(g, c) };
    let mut spec_cache: SmallVec<[(SpecialistId, f64); 4]> = SmallVec::new();
    let mut taken: SmallVec<[bool; 32]> = tiles.iter().map(|_| false).collect();
    for _ in 0..free {
        let mut best: Option<(usize, f64)> = None;
        for (i, &(_, s, (x, y), rest)) in tiles.iter().enumerate() {
            if taken[i] {
                continue;
            }
            // Converting food to production makes production read the food: rank it whole.
            let v = if surplus > 0.0 && rc.converts {
                rank_stats_for_work(&rc, &s, false, surplus)
            } else {
                rest + rank_food(&rc, s[Stat::Food], surplus)
            };
            let better = match best {
                None => true,
                Some((j, bv)) => {
                    let (bx, by) = tiles[j].2;
                    (v, x, y) > (bv, bx, by)
                }
            };
            if better {
                best = Some((i, v));
            }
        }
        let mut best_job: Option<(SpecialistId, f64)> = None;
        if !city.manual_specialists {
            for &(s, mx) in maxs {
                if i32::from(a.specialists[usize::from(s.0)]) >= mx {
                    continue;
                }
                let v = match spec_cache.iter().find(|(x, _)| *x == s) {
                    Some(&(_, v)) => v,
                    None => {
                        let v = rank_specialist(g, &rc, c, s, surplus, gpp);
                        spec_cache.push((s, v));
                        v
                    }
                };
                if best_job.is_none_or(|(_, bv)| v > bv) {
                    best_job = Some((s, v));
                }
            }
        }
        match (best, best_job) {
            (Some((i, v)), job) if job.is_none_or(|(_, jv)| v > jv) => {
                taken[i] = true;
                a.worked.push(tiles[i].0);
                surplus += tiles[i].1[Stat::Food];
            }
            (_, Some((s, _))) => {
                let n = &mut a.specialists[usize::from(s.0)];
                *n = n.saturating_add(1);
                surplus += spec_food_bonus;
            }
            _ => break,
        }
    }
}

/// What a specialist adds to a city's food against a worker's two eaten
/// (`auto_assign_population`'s `spec_food_bonus`, `cities.py:838-843`): each
/// `[n]% Food consumption by specialists` multiplies the two a specialist eats.
fn specialist_food_bonus(g: &Game, c: CityId) -> f64 {
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let filters = g.rules().uniques().filters();
    let mut eats = 2.0;
    for h in uq::city(&v, c, UniqueType::FoodConsumptionBySpecialists, &ctx) {
        if let UniqueData::FoodConsumptionBySpecialists(x) = h.data()
            && filters.city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                eats *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    2.0 - eats
}

/// Takes citizens off tiles and out of slots when there are more of them placed than there are
/// citizens (`cities._unassign_extra`, `cities.py:875-903`): the worst unlocked tile first, then
/// the specialists in slot order.
fn unassign_extra(
    g: &Game,
    c: CityId,
    city: &City,
    avail: &[TileIdx],
    maxs: &[(SpecialistId, i32)],
    rc: &RankCtx,
    a: &mut Assignment,
) {
    a.worked.retain(|t| avail.contains(t));
    for (i, n) in a.specialists.iter_mut().enumerate() {
        let cap = u8::try_from(i).ok().map_or(0, |s| slots(maxs, SpecialistId(s)));
        *n = u8::try_from(i32::from(*n).min(cap).max(0)).unwrap_or(0);
    }
    let owner = city.owner();
    while a.free(city.pop) < 0 {
        if !a.worked.is_empty() {
            let mut worst: Option<(usize, (bool, f64))> = None;
            for (i, &t) in a.worked.iter().enumerate() {
                let key = (
                    a.locked.contains(&t),
                    rank_stats_for_work(
                        rc,
                        &memo::tile_yield(g, t, Some(owner), Some(c)),
                        false,
                        0.0,
                    ),
                );
                if worst.is_none_or(|(_, w)| key < w) {
                    worst = Some((i, key));
                }
            }
            if let Some((i, _)) = worst {
                let t = a.worked.remove(i);
                a.locked.retain(|&x| x != t);
            }
        } else if let Some(n) = a.specialists.iter_mut().find(|n| **n > 0) {
            *n -= 1;
        } else {
            break;
        }
    }
}

// ---- Settle and the oracle (DESIGN.md 6.7, 6.8) ---------------------------------------------------

impl Game {
    /// Reassigns city `c`'s citizens, as the settle does for a flagged city: writes the new
    /// assignment if it differs (or if the engine never assigned the city), marks the city
    /// settled, and flags the cities of its owner that could work a tile it took or released.
    pub(crate) fn reassign(&mut self, c: CityId) {
        let Some(a) = assign(self, c, false) else { return };
        let Some(city) = self.city(c) else { return };
        let before = Assignment::of(city);
        if before == a && city.citizens_settled {
            return;
        }
        let owner = city.owner();
        let moved: SmallVec<[TileIdx; 8]> = before
            .worked
            .iter()
            .filter(|t| !a.worked.contains(t))
            .chain(a.worked.iter().filter(|t| !before.worked.contains(t)))
            .copied()
            .collect();
        let changed = before != a;
        self.set_citizens(c, a);
        let deps = self.dv.stats.deps().citizens;
        if !moved.is_empty() && deps.contains(crate::unique::CondDeps::MAP) {
            // What a tile yields reads whether a city works the tiles around it (a filter asked
            // of every tile, or of the neighbours): any city in reach of a tile taken or
            // released may rank its tiles differently.
            let reach = cstats::work_range(self).saturating_add(1);
            let near: SmallVec<[CityId; 8]> = self
                .state()
                .cities()
                .iter()
                .filter(|y| y.id() != c)
                .filter(|y| moved.iter().any(|&t| self.grid().distance(y.tile(), t) <= reach))
                .map(City::id)
                .collect();
            for x in near {
                self.pending.flag_city(x);
            }
        }
        if !moved.is_empty() {
            let range = cstats::work_range(self);
            let siblings: SmallVec<[CityId; 8]> = self
                .state()
                .cities()
                .of(owner)
                .iter()
                .copied()
                .filter(|&x| x != c)
                .filter(|&x| {
                    self.city(x).is_some_and(|y| {
                        moved.iter().any(|&t| self.grid().distance(y.tile(), t) <= range)
                    })
                })
                .collect();
            for x in siblings {
                self.pending.flag_city(x);
            }
        }
        // A city whose own citizens change what its uniques' conditionals and filters read
        // (the tiles it works, its specialists) looks again, until its assignment stands. Those
        // read the tile class besides the city's (a population filter's conditional, a worked
        // tile's filter), or the map; the city class alone is its capital status, stocks and
        // size, which no reassignment moves.
        let own = crate::unique::CondDeps::TILE | crate::unique::CondDeps::MAP;
        if changed && self.dv.stats.deps().citizens.intersects(own) {
            self.pending.flag_city(c);
        }
    }
}

/// The citizen oracle (DESIGN.md 6.8): every city this engine has assigned has its citizens where
/// a fresh assignment would put them. One line for each that does not.
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let mut out = Vec::new();
    for city in g.state().cities().iter().filter(|c| c.citizens_settled) {
        let c = city.id();
        let Some(a) = assign(g, c, false) else { continue };
        if a != Assignment::of(city) {
            out.push(format!(
                "city {}: its citizens are at {:?} with specialists {:?}, a fresh assignment at \
                 {:?} with {:?}",
                c.get(),
                city.worked,
                city.specialists,
                a.worked,
                a.specialists
            ));
        }
    }
    out
}

// ---- The tools (tools.py:679-757) ---------------------------------------------------------------

/// One of the player's cities (`tools._own_city`, `tools.py:169-175`), or the refusal that lists
/// the real ones.
fn own_city(g: &Game, pid: PlayerId, city_id: i64) -> Result<CityId, ActionError> {
    let found = u32::try_from(city_id)
        .ok()
        .and_then(CityId::new)
        .filter(|&c| g.city(c).is_some_and(|x| x.owner() == pid));
    found.ok_or_else(|| {
        let ids: Vec<String> =
            g.player_cities(pid).map(|x| format!("#{} {}", x.id().get(), x.name)).collect();
        let ids = if ids.is_empty() { "none".to_owned() } else { ids.join(", ") };
        ActionError::new(
            ErrCode::NoSuchCity,
            format!("You have no city with id {city_id}. Your cities: {ids}."),
        )
    })
}

/// The refusal of a city that is gone, which [`own_city`] has already ruled out.
fn no_city(c: CityId) -> ActionError {
    ActionError::new(ErrCode::NoSuchCity, format!("No city {}.", c.get()))
}

/// A tile by its coordinates (`tools._idx`, `tools.py:143-151`), or the refusal that names the
/// map's size.
fn tile_at(g: &Game, x: i64, y: i64) -> Result<TileIdx, ActionError> {
    let fits = |n: i64| i32::try_from(n).ok();
    fits(x).zip(fits(y)).and_then(|(x, y)| g.grid().idx(x, y)).ok_or_else(|| {
        let m = g.state().map();
        ActionError::new(
            ErrCode::OffMap,
            format!("({x},{y}) is off the map (map is {}x{}).", m.width, m.height),
        )
    })
}

/// Tiles as `[x, y]` pairs, by column and then row, as `inspect` lists a city's: a tool's result
/// is what the city shows after the call.
// refcheck: citizen-tools-list-tiles-sorted
fn xys(g: &Game, tiles: &[TileIdx]) -> Value {
    let mut v: SmallVec<[(i32, i32); 16]> = tiles.iter().map(|&t| g.xy(t)).collect();
    v.sort();
    Value::Array(v.into_iter().map(|(x, y)| json!([x, y])).collect())
}

/// A city's specialists as the tools report them: `{name: count}` of those it has.
fn specialists_json(g: &Game, c: CityId) -> Value {
    let mut out = Map::new();
    if let Some(city) = g.city(c) {
        for (i, &n) in city.specialists.iter().enumerate() {
            if n > 0
                && let Some(name) =
                    u8::try_from(i).ok().and_then(|s| g.rules().specialists().get(SpecialistId(s)))
            {
                out.insert(name.name.to_string(), json!(n));
            }
        }
    }
    Value::Object(out)
}

/// `set_city_focus`: what a city's citizens favour, and whether it avoids growing
/// (`tools.set_city_focus`, `tools.py:679-700`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetCityFocus {
    pub city_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avoid_growth: Option<Value>,
}

impl Rule for SetCityFocus {
    type Plan = (CityId, Option<CityFocus>);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let focus = match &self.focus {
            None => None,
            Some(f) => Some(f.as_str().and_then(CityFocus::from_name).ok_or_else(|| {
                let all: Vec<&str> = CityFocus::ALL.iter().map(|f| f.name()).collect();
                ActionError::new(
                    ErrCode::BadParam,
                    format!("Focus must be one of {}.", all.join(", ")),
                )
            })?),
        };
        Ok((c, focus))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, focus): Self::Plan) -> OutcomeSpec {
        let avoid = self.avoid_growth.as_ref().map(py::truthy);
        if let Some(x) = g.city_mut(c, CityTouch::WORK) {
            if let Some(f) = focus {
                x.focus = f;
                x.manual_specialists = f == CityFocus::Manual && x.manual_specialists;
            }
            if let Some(a) = avoid {
                x.avoid_growth = a;
            }
        }
        OutcomeSpec::render(move |g| {
            let Some(x) = g.city(c) else { return Value::Null };
            json!({
                "focus": x.focus.name(),
                "avoid_growth": x.avoid_growth,
                "worked_tiles": xys(g, &x.worked),
                "specialists": specialists_json(g, c),
            })
        })
    }
}

/// `work_tile`: locks a citizen onto a tile of a city, or releases it (`tools.work_tile`,
/// `tools.py:703-724`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkTile {
    pub city_id: i64,
    pub x: i64,
    pub y: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<Value>,
}

impl Rule for WorkTile {
    type Plan = (CityId, TileIdx, bool);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let t = tile_at(g, self.x, self.y)?;
        let lock = self.locked.as_ref().is_none_or(py::truthy);
        if lock {
            if !cstats::workable_tiles(g, c).contains(&t) {
                return Err(ActionError::new(
                    ErrCode::Rule,
                    "That tile is not workable by this city.",
                ));
            }
            let Some(city) = g.city(c) else { return Err(no_city(c)) };
            // refcheck: work-tile-refuses-a-lock-past-the-citizens
            if !city.locked.contains(&t) && city.locked.len() >= usize::from(city.pop) {
                return Err(ActionError::new(
                    ErrCode::Rule,
                    format!(
                        "All {} citizens of {} are locked to tiles: unlock one first.",
                        city.pop, city.name
                    ),
                ));
            }
        }
        Ok((c, t, lock))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, t, lock): Self::Plan) -> OutcomeSpec {
        if let Some(x) = g.city_mut(c, CityTouch::WORK) {
            if lock {
                if let Err(i) = x.locked.binary_search(&t) {
                    x.locked.insert(i, t);
                }
            } else {
                x.locked.retain(|&l| l != t);
            }
        }
        OutcomeSpec::render(move |g| {
            let Some(x) = g.city(c) else { return Value::Null };
            json!({"locked_tiles": xys(g, &x.locked), "worked_tiles": xys(g, &x.worked)})
        })
    }
}

/// `set_specialists`: specialists by hand, or back to automatic with `{}`
/// (`tools.set_specialists`, `tools.py:727-757`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetSpecialists {
    pub city_id: i64,
    pub specialists: Value,
}

impl Rule for SetSpecialists {
    type Plan = (CityId, [u8; MAX_SPECIALISTS]);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let Some(city) = g.city(c) else { return Err(no_city(c)) };
        let Value::Object(asked) = &self.specialists else {
            return Err(ActionError::new(
                ErrCode::BadParam,
                "specialists must be an object like {\"Scientist\": 1}.",
            ));
        };
        let r = g.rules();
        let maxs = max_specialists(g, c);
        let available = || {
            if maxs.is_empty() {
                return "none".to_owned();
            }
            let parts: Vec<String> = maxs
                .iter()
                .map(|&(s, n)| {
                    let name = r.specialists().get(s).map_or("?", |d| &*d.name);
                    format!("{}: {n}", py::repr(&Value::from(name)))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        };
        let mut clean = [0u8; MAX_SPECIALISTS];
        for (k, v) in asked {
            let s = r.resolve::<SpecialistId>(k).filter(|&s| maxs.iter().any(|&(x, _)| x == s));
            let Some(s) = s else {
                return Err(ActionError::new(
                    ErrCode::BadParam,
                    format!("{} has no slots for {k}. Available: {}.", city.name, available()),
                ));
            };
            let cap = slots(&maxs, s);
            let name = r.specialists().get(s).map_or("?", |d| &*d.name);
            // refcheck: specialist-counts-must-be-numbers
            let n = py::int_of(v).ok_or_else(|| {
                ActionError::new(
                    ErrCode::BadParam,
                    format!("The number of {name} specialists must be a whole number."),
                )
            })?;
            if n < 0 || n > i64::from(cap) {
                return Err(ActionError::new(
                    ErrCode::BadParam,
                    format!("{} has {cap} {name} slot(s).", city.name),
                ));
            }
            clean[usize::from(s.0)] = u8::try_from(n).unwrap_or(u8::MAX);
        }
        let total: u32 = clean.iter().map(|&n| u32::from(n)).sum();
        if total > u32::from(city.pop) {
            return Err(ActionError::new(
                ErrCode::BadParam,
                format!("{} has only {} citizens.", city.name, city.pop),
            ));
        }
        Ok((c, clean))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, clean): Self::Plan) -> OutcomeSpec {
        if let Some(x) = g.city_mut(c, CityTouch::WORK) {
            x.specialists = clean;
            x.manual_specialists = clean.iter().any(|&n| n > 0);
        }
        OutcomeSpec::render(move |g| {
            let Some(x) = g.city(c) else { return Value::Null };
            json!({"specialists": specialists_json(g, c), "worked_tiles": xys(g, &x.worked)})
        })
    }
}
