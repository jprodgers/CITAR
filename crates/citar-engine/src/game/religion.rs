//! Religion (`religion.py`, UnCiv's `ReligionManager`, `CityReligionManager` and `Religion`).
//!
//! This module holds a city's side (`religion.py:98-315`): which religions its citizens follow and
//! which most of them do (package 1b-06's reads, which a city's stats and its follower uniques
//! build on), the pressure religions exert on it and its conversions, and holy cities. The rest
//! is package 1b-08's too:
//! - [`found`]: pantheons, founding and enhancing religions, the beliefs to choose and their
//!   triggers, and the tool `found_pantheon` (`religion.py:451-706, 778-796`);
//! - [`prophets`]: what pantheons and great prophets cost, how many religions a game allows, and
//!   the prophets a civilization's faith brings (`religion.py:331-445`).
//!
//! The unit actions that spread religion and remove heresy (`religion.py:709-775`) are package
//! 1c-04's, built on [`add_pressure`], [`remove_all_except`] and [`protected_by_inquisitor`].
//!
//! Pressure from the surroundings (`religion.pressures_from_surroundings`, `religion.py:266-284`)
//! looked at every city of the world for every city, each turn. Here it asks the cities the
//! spatial grid (`derive::religion`, `CityNeighbours` in DESIGN.md 6.11) puts within the farthest
//! any city's religion reaches, and each city's spread (its majority, how far it reaches and how
//! strongly) is a memo of its own, so a round costs the cities times their neighbours.
//!
//! A city's pressures are kept in the order each religion first reached it, as Python's dict kept
//! them, and ties go by that order: which religion a citizen left over after the division goes to,
//! and which of two religions with as many followers is the majority.

pub mod found;
pub mod prophets;

use smallvec::SmallVec;

use super::Game;
use super::derive::rev::{CityTouch, PlayerTouch};
use crate::base::ids::{BeliefId, CityFilterId, CityId, PlayerId, ReligionId, UnitId};
use crate::base::num;
use crate::base::sets::{BeliefSet, PlayerSet};
use crate::base::stats::Stat;
use crate::rules::defs::{BeliefKind, BeliefType};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::City;
use crate::state::world::{Religion, ReligionName};
use crate::unique::table::UFlags;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

// ---- The religions ---------------------------------------------------------------------------

/// A founded religion or pantheon (`religion.rel`, `religion.py:26-28`).
#[must_use]
pub fn religion(g: &Game, r: ReligionId) -> Option<&Religion> {
    g.state().world().religion(r)
}

/// Whether a religion has a belief of this type among its founder beliefs.
fn has_founder_belief(g: &Game, r: ReligionId, kind: BeliefType) -> bool {
    let beliefs = g.rules().beliefs();
    religion(g, r).is_some_and(|rel| {
        rel.founder_beliefs.iter().any(|b| beliefs.get(b).is_some_and(|d| d.kind == kind))
    })
}

/// Whether a religion is a full one rather than a pantheon: it has a founder belief
/// (`religion.is_major`, `religion.py:31-34`).
#[must_use]
pub fn is_major(g: &Game, r: ReligionId) -> bool {
    has_founder_belief(g, r, BeliefType::Founder)
}

/// Whether a religion is enhanced (`religion.is_enhanced`, `religion.py:37-40`).
#[must_use]
pub fn is_enhanced(g: &Game, r: ReligionId) -> bool {
    has_founder_belief(g, r, BeliefType::Enhancer)
}

/// What Python called a religion: a pantheon by its belief, a religion by its row of the
/// ruleset's names.
#[must_use]
pub fn key_name(g: &Game, r: ReligionId) -> String {
    let rules = g.rules();
    match religion(g, r).map(|x| x.name) {
        Some(ReligionName::Pantheon(b)) => rules.beliefs()[b].name.to_string(),
        Some(ReligionName::Religion(x)) => {
            rules.religions().get(x).map(ToString::to_string).unwrap_or_default()
        }
        None => String::new(),
    }
}

/// The name a religion is shown under, which its founder may have chosen
/// (`religion.display_name`, `religion.py:53-56`).
#[must_use]
pub fn display_name(g: &Game, r: ReligionId) -> String {
    religion(g, r).map(|x| x.display.to_string()).unwrap_or_default()
}

/// The order Python sorted beliefs in (`religion.py:49`).
const fn type_order(t: BeliefType) -> u8 {
    match t {
        BeliefType::Pantheon => 0,
        BeliefType::Founder => 1,
        BeliefType::Follower => 2,
        BeliefType::Enhancer => 3,
    }
}

/// Every belief of a religion, by type: pantheon, founder, follower, enhancer
/// (`religion.all_beliefs`, `religion.py:43-50`).
#[must_use]
pub fn all_beliefs(g: &Game, r: ReligionId) -> Vec<BeliefId> {
    let Some(rel) = religion(g, r) else { return Vec::new() };
    let defs = g.rules().beliefs();
    let mut out: Vec<BeliefId> =
        rel.founder_beliefs.iter().chain(rel.follower_beliefs.iter()).collect();
    out.sort_by_key(|&b| type_order(defs[b].kind));
    out
}

/// Every belief some religion or pantheon has taken (`religion.beliefs_taken`,
/// `religion.py:91-93`).
#[must_use]
pub fn beliefs_taken(g: &Game) -> BeliefSet {
    let mut out = BeliefSet::new();
    for r in &g.state().world().religions {
        out |= r.founder_beliefs;
        out |= r.follower_beliefs;
    }
    out
}

/// The beliefs of a kind nobody has taken, in the ruleset's order (`religion.beliefs_available`,
/// `religion.py:96-99`).
#[must_use]
pub fn beliefs_available(g: &Game, kind: BeliefKind) -> Vec<BeliefId> {
    let taken = beliefs_taken(g);
    g.rules()
        .beliefs()
        .iter()
        .filter(|&(b, d)| {
            !taken.contains(b)
                && match kind {
                    BeliefKind::Any => true,
                    BeliefKind::Type(t) => d.kind == t,
                }
        })
        .map(|(b, _)| b)
        .collect()
}

// ---- A city's followers ------------------------------------------------------------------------

/// How many of a city's citizens follow each religion (`religion.followers`, `religion.py:98-127`):
/// the population shared out in proportion to the pressures, the citizens left over going one at
/// a time to the largest remainders. No religion is left out, as is a religion with no follower.
#[must_use]
pub fn followers(city: &City) -> SmallVec<[(ReligionId, i32); 4]> {
    let mut out: SmallVec<[(Option<ReligionId>, i32); 4]> = SmallVec::new();
    let pop = i32::from(city.pop);
    if pop <= 0 {
        return SmallVec::new();
    }
    let total: f64 = city.pressures.iter().map(|&(_, v)| f64::from(v)).sum();
    let per = total / f64::from(pop);
    let mut rem: SmallVec<[f64; 4]> = SmallVec::new();
    for &(r, v) in &city.pressures {
        let v = f64::from(v);
        let n = if per == 0.0 { 0 } else { num::trunc_i32(v / per) };
        out.push((r, n));
        rem.push(v - f64::from(n) * per);
    }
    let mut left = pop - out.iter().map(|&(_, n)| n).sum::<i32>();
    while left > 0 {
        if rem.is_empty() {
            out.push((None, left));
            break;
        }
        // The first of the largest remainders, in the pressures' order.
        let mut best = 0;
        for (i, &x) in rem.iter().enumerate() {
            if x > rem[best] {
                best = i;
            }
        }
        out[best].1 += 1;
        rem[best] = 0.0;
        left -= 1;
    }
    let mut res: SmallVec<[(ReligionId, i32); 4]> = SmallVec::new();
    for (r, n) in out {
        if let Some(r) = r
            && n > 0
        {
            match res.iter_mut().find(|(x, _)| *x == r) {
                Some((_, m)) => *m += n,
                None => res.push((r, n)),
            }
        }
    }
    res
}

/// The religion most of a city's citizens follow, if at least half of them do and religion is in
/// play (`religion.majority_religion`, `religion.py:136-149`).
#[must_use]
pub fn majority_religion(g: &Game, c: CityId) -> Option<ReligionId> {
    if !g.religion_enabled() {
        return None;
    }
    let city = g.city(c)?;
    majority_of(city)
}

/// The majority of a city's own followers, whether or not religion is in play: of two religions
/// with as many followers, the one that reached the city first.
fn majority_of(city: &City) -> Option<ReligionId> {
    let f = followers(city);
    let mut best: Option<(ReligionId, i32)> = None;
    for &(r, n) in &f {
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((r, n));
        }
    }
    let (r, n) = best?;
    (f64::from(n) >= f64::from(city.pop) / 2.0).then_some(r)
}

/// How many of a city's citizens follow its majority religion (`religion.followers_of_majority`,
/// `religion.py:152-156`).
#[must_use]
pub fn followers_of_majority(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let Some(m) = majority_religion(g, c) else { return 0 };
    followers(city).iter().find(|&&(r, _)| r == m).map_or(0, |&(_, n)| n)
}

/// How many cities of the world follow religion `r` in the majority (`religion.cities_following`,
/// `religion.py:321-323`).
#[must_use]
pub fn cities_following(g: &Game, r: ReligionId) -> i32 {
    let n = g.state().cities().iter().filter(|c| majority_religion(g, c.id()) == Some(r)).count();
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// How many citizens of the world's cities that pass a city filter, seen by `viewer`, follow
/// religion `r` (`religion.followers_of`, `religion.py:326-328`).
#[must_use]
pub fn followers_of(g: &Game, r: ReligionId, filter: CityFilterId, viewer: PlayerId) -> i32 {
    let v = g.view();
    let filters = g.rules().uniques().filters();
    g.state()
        .cities()
        .iter()
        .filter(|c| filters.city_matches(filter, &v, c.id(), Some(viewer)))
        .map(|c| followers(c).iter().find(|&&(x, _)| x == r).map_or(0, |&(_, n)| n))
        .sum()
}

/// Whether a city's majority can depend on its population: some pressure is toward a religion.
/// A city whose pressures are all toward no religion follows none, whatever its size.
#[must_use]
pub fn has_religious_pressure(city: &City) -> bool {
    city.pressures.iter().any(|&(r, v)| r.is_some() && v != 0)
}

// ---- Pressure ----------------------------------------------------------------------------------

/// Adds religious pressure to a city (`None`: toward no religion), and, when its majority moves,
/// announces the conversion and what follows from it (`religion.add_pressure`,
/// `religion.py:165-177`).
pub fn add_pressure(g: &mut Game, c: CityId, r: Option<ReligionId>, amount: i32) {
    if !g.religion_enabled() {
        return;
    }
    let before = majority_religion(g, c);
    push_pressure(g, c, r, amount);
    after_update(g, c, before);
}

/// Adds religious pressure to a city with nothing else (`add_pressure(update=False)`).
fn push_pressure(g: &mut Game, c: CityId, r: Option<ReligionId>, amount: i32) {
    if let Some(x) = g.city_mut(c, CityTouch::RELIGION) {
        x.add_pressure(r, amount);
    }
}

/// What follows a change of a city's pressures (`religion._after_update`, `religion.py:180-186`):
/// a new majority is announced, and pays its founder the first time the city adopts it.
fn after_update(g: &mut Game, c: CityId, before: Option<ReligionId>) {
    let after = majority_religion(g, c);
    if after != before
        && let Some(r) = after
    {
        on_adoption(g, c, r);
    }
}

/// A city adopts a religion (`religion._on_adoption`, `religion.py:189-203`): it is announced to
/// the city's owner, and the first time the city adopts it, the religion's founder has the stats
/// of its `[stats] when a city adopts this religion for the first time`.
fn on_adoption(g: &mut Game, c: CityId, r: ReligionId) {
    let Some((owner, name, at, adopted)) = g
        .city(c)
        .map(|x| (x.owner(), x.name.to_string(), x.tile(), x.religions_adopted.contains(&r)))
    else {
        return;
    };
    let text = format!("{name} converted to {}.", display_name(g, r));
    let data = EventData { religion: Some(r), ..EventData::default() };
    g.emit(EngineEvent::Religion, &text, Some(PlayerSet::single(owner)), Some(at), data, &[]);
    let Some(founder) = religion(g, r).map(|x| x.founder) else { return };
    if adopted {
        return;
    }
    let total = {
        let v = g.view();
        let t = g.rules().uniques();
        let mut total = crate::base::stats::Stats::default();
        for h in uq::civ(&v, founder, UniqueType::StatsWhenAdoptingReligion, &Ctx::civ(founder)) {
            if let UniqueData::StatsWhenAdoptingReligion(x) = *h.data() {
                let mult =
                    if h.unique.flags().contains(UFlags::SPEED) { g.speed().modifier } else { 1.0 };
                for _ in 0..h.n {
                    total.add_scaled(t.stats(x.stats), mult);
                }
            }
        }
        total
    };
    for s in Stat::ALL {
        if total[s] != 0.0 {
            g.add_stat(founder, s, total[s].trunc());
        }
    }
    if let Some(x) = g.city_mut(c, CityTouch::RELIGION) {
        x.religions_adopted.push(r);
    }
}

/// Every religion's pressure in a city goes but that of `r` (and of no religion), as an
/// inquisitor makes it go (`religion.remove_all_except`, `religion.py:206-216`).
pub fn remove_all_except(g: &mut Game, c: CityId, r: ReligionId) {
    let before = majority_religion(g, c);
    if let Some(x) = g.city_mut(c, CityTouch::RELIGION) {
        let keep = x.pressure(Some(r));
        let none = x.pressure(None);
        x.pressures.clear();
        x.pressures.push((
            None,
            if none != 0 { none } else { crate::state::cities::NO_RELIGION_PRESSURE },
        ));
        x.pressures.push((Some(r), keep));
    }
    after_update(g, c, before);
}

/// A city grows: its new citizens follow its majority, or no religion
/// (`religion.on_population_change`, `religion.py:219-227`).
pub fn on_population_change(g: &mut Game, c: CityId, delta: i32) {
    if delta > 0 {
        let m = majority_religion(g, c);
        add_pressure(g, c, m, 100 * delta);
    }
}

/// A city that changed hands forgets the pantheons of civilizations other than its new owner's
/// (`religion.remove_unknown_pantheons`, `religion.py:230-238`).
pub fn remove_unknown_pantheons(g: &mut Game, c: CityId) {
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    let gone: SmallVec<[ReligionId; 2]> = city
        .pressures
        .iter()
        .filter_map(|&(r, _)| r)
        .filter(|&r| religion(g, r).is_some_and(|x| x.founder != owner) && !is_major(g, r))
        .collect();
    if gone.is_empty() {
        return;
    }
    if let Some(x) = g.city_mut(c, CityTouch::RELIGION) {
        x.pressures.retain(|(r, _)| r.is_none_or(|r| !gone.contains(&r)));
    }
}

/// How far a city's religion reaches (`religion._spread_range`, `religion.py:241-248`): 10 tiles,
/// and more with `Religion naturally spreads to cities [n] tiles away` of the city and of its
/// majority religion's founder.
#[must_use]
pub fn spread_range(g: &Game, c: CityId) -> i32 {
    let v = g.view();
    let distance = |d: &UniqueData| match *d {
        UniqueData::ReligionSpreadDistance(x) => Some(x.distance),
        _ => None,
    };
    let mut range = 10
        + uq::sum_i32(
            uq::city(&v, c, UniqueType::ReligionSpreadDistance, &Ctx::city(&v, c)),
            distance,
        );
    if let Some(founder) = majority_religion(g, c).and_then(|m| religion(g, m)).map(|x| x.founder) {
        range += uq::sum_i32(
            uq::civ(&v, founder, UniqueType::ReligionSpreadDistance, &Ctx::civ(founder)),
            distance,
        );
    }
    range
}

/// The multipliers of a city's natural religious pressure, each with the cities it holds for
/// (`[n]% Natural religion spread [cities]`), the city's own then its majority's founder's, in the
/// order `religion._pressure_to` applied them (`religion.py:255-262`).
#[must_use]
pub fn spread_factors(g: &Game, c: CityId) -> SmallVec<[(f64, CityFilterId); 2]> {
    let v = g.view();
    let mut out = SmallVec::new();
    let mut take = |hits: crate::unique::query::Hits<'_, crate::game::EvalView<'_>>| {
        for h in hits {
            if let UniqueData::NaturalReligionSpreadStrength(x) = *h.data() {
                for _ in 0..h.n {
                    out.push((1.0 + f64::from(x.percent) / 100.0, x.cities));
                }
            }
        }
    };
    take(uq::city(&v, c, UniqueType::NaturalReligionSpreadStrength, &Ctx::city(&v, c)));
    if let Some(founder) = majority_religion(g, c).and_then(|m| religion(g, m)).map(|x| x.founder) {
        take(uq::civ(&v, founder, UniqueType::NaturalReligionSpreadStrength, &Ctx::civ(founder)));
    }
    out
}

/// The pressure one city's religion puts on another each turn (`religion._pressure_to`,
/// `religion.py:251-263`): the speed's pressure for an adjacent city, times the multipliers that
/// hold for the city it reaches.
#[must_use]
pub fn pressure_to(g: &Game, src: CityId, target: CityId) -> i32 {
    pressure_with(g, &spread_factors(g, src), target)
}

/// The pressure a city with these multipliers puts on `target`.
fn pressure_with(g: &Game, factors: &[(f64, CityFilterId)], target: CityId) -> i32 {
    let mut pressure = f64::from(g.speed().religious_pressure_adjacent_city);
    if !factors.is_empty() {
        let v = g.view();
        let filters = g.rules().uniques().filters();
        for &(f, cities) in factors {
            if filters.city_matches(cities, &v, target, None) {
                pressure *= f;
            }
        }
    }
    num::trunc_i32(pressure)
}

/// The pressure arriving at a city this turn, by religion, in the order each first arrived
/// (`religion.pressures_from_surroundings`, `religion.py:266-284`): its own religion's five times
/// the speed's pressure if it is a holy city whose pressure no inquisitor blocks, and each major
/// religion that is the majority of a city within that city's reach, with its pressure; from the
/// cities in id order, as Python walked them.
#[must_use]
pub fn pressures_from_surroundings(g: &Game, c: CityId) -> SmallVec<[(ReligionId, i32); 4]> {
    let mut out: SmallVec<[(ReligionId, i32); 4]> = SmallVec::new();
    let Some(city) = g.city(c) else { return out };
    let mut add = |r: ReligionId, n: i32| match out.iter_mut().find(|(x, _)| *x == r) {
        Some((_, m)) => *m += n,
        None => out.push((r, n)),
    };
    if let Some(r) = city.holy_city_of
        && !blocked(g, c)
    {
        add(r, 5 * g.speed().religious_pressure_adjacent_city);
    }
    let at = city.tile();
    let reach = super::derive::religion::reach(g);
    for other in super::derive::religion::cities_within(g, at, reach) {
        if other == c {
            continue;
        }
        let Some(src) = super::derive::religion::spread_source(g, other) else { continue };
        if g.grid().distance(src.tile, at) > u32::try_from(src.range).unwrap_or(0) {
            continue;
        }
        add(src.religion, pressure_with(g, &src.factors, c));
    }
    out
}

/// Stage E4's religion for a city (`religion.city_end_turn`, `religion.py:293-300`): the
/// pressure of its surroundings is added, and a new majority takes the city.
pub fn city_end_turn(g: &mut Game, c: CityId) {
    if !g.religion_enabled() {
        return;
    }
    let before = majority_religion(g, c);
    for (r, n) in pressures_from_surroundings(g, c) {
        push_pressure(g, c, Some(r), n);
    }
    after_update(g, c, before);
}

/// Whether an inquisitor has blocked the pressure of the holy city `c` (`religion._blocked`,
/// `religion.py:287-290`).
#[must_use]
pub fn blocked(g: &Game, c: CityId) -> bool {
    g.city(c)
        .and_then(|x| x.holy_city_of)
        .and_then(|r| religion(g, r))
        .is_some_and(|r| r.blocked_holy)
}

/// Whether a city is a religion's holy city, still working as one (`religion.is_holy_city`,
/// `religion.py:303-305`).
#[must_use]
pub fn is_holy_city(g: &Game, c: CityId) -> bool {
    g.city(c).is_some_and(|x| x.holy_city_of.is_some()) && !blocked(g, c)
}

/// A religion's holy city, if it still works as one (`religion.holy_city`, `religion.py:433-440`).
#[must_use]
pub fn holy_city(g: &Game, r: ReligionId) -> Option<CityId> {
    g.state()
        .cities()
        .iter()
        .find(|x| x.holy_city_of == Some(r) && !blocked(g, x.id()))
        .map(City::id)
}

/// Whether an inquisitor of another religion than `from` stands in or next to a city
/// (`religion.protected_by_inquisitor`, `religion.py:308-315`).
#[must_use]
pub fn protected_by_inquisitor(g: &Game, c: CityId, from: Option<ReligionId>) -> bool {
    let Some(at) = g.city(c).map(City::tile) else { return false };
    let r = g.rules();
    g.grid().within(at, 1).into_iter().any(|t| {
        g.units_at(t).any(|u| {
            u.religion.is_some_and(|x| from.is_none_or(|f| x != f))
                && super::cities::construction::unit_has_type(
                    r,
                    u.base,
                    UniqueType::PreventSpreadingReligion,
                )
        })
    })
}

/// The religion a new religious unit made in city `c` carries (`units.py:127-133`): the city's
/// majority, or its owner's own religion for a unit that takes it over its birth city's.
pub(crate) fn on_unit_made(g: &mut Game, u: UnitId, c: CityId) {
    let r = g.rules();
    let Some((base, owner)) = g.unit(u).map(|x| (x.base, x.owner())) else { return };
    let has = |ty| super::cities::construction::unit_has_type(r, base, ty);
    if !has(UniqueType::ReligiousUnit) || !g.religion_enabled() {
        return;
    }
    let own = g.player(owner).and_then(|x| x.religion.founded);
    let carried =
        if has(UniqueType::TakeReligionOverBirthCity) && own.is_some_and(|x| is_major(g, x)) {
            own
        } else {
            majority_religion(g, c)
        };
    if let Some(x) = g.unit_mut(u, super::derive::rev::UnitTouch::CORE) {
        x.religion = carried;
    }
}

/// Stage S2's religion (`religion.start_turn`, `religion.py:400-403`): a great prophet, if the
/// civilization's faith brings one.
pub(crate) fn start_turn_stage(g: &mut Game, p: PlayerId) {
    prophets::start_turn(g, p);
}

/// Stage E3's faith (`religion.end_turn`, `religion.py:443-445`): the turn's faith is banked.
pub(crate) fn end_turn(g: &mut Game, p: PlayerId, faith: f64) {
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        x.econ.faith += faith.trunc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ids::TileIdx;

    fn city(pop: u16, pressures: &[(Option<u8>, i32)]) -> City {
        let mut c = City::new(CityId::FIRST, "Roma".into(), PlayerId(0), TileIdx(0), 1);
        c.pop = pop;
        c.pressures = pressures.iter().map(|&(r, v)| (r.map(ReligionId), v)).collect();
        c
    }

    #[test]
    fn citizens_follow_in_proportion_and_the_rest_by_remainder() {
        // 100 toward none and 300 toward religion 0, over 4 citizens: 1 and 3.
        assert_eq!(
            followers(&city(4, &[(None, 100), (Some(0), 300)])).to_vec(),
            [(ReligionId(0), 3)]
        );
        // Over 3: 0.75 and 2.25 give 0 and 2; the citizen left goes to the larger remainder.
        let f = followers(&city(3, &[(None, 100), (Some(0), 300)]));
        assert_eq!(f.to_vec(), [(ReligionId(0), 2)]);
        let c = city(2, &[(None, 100), (Some(0), 100), (Some(1), 200)]);
        assert_eq!(followers(&c).to_vec(), [(ReligionId(1), 1)]);
        assert_eq!(majority_of(&c), Some(ReligionId(1)), "one of two is half");
        assert_eq!(majority_of(&city(3, &[(None, 200), (Some(0), 100)])), None);
        assert!(followers(&city(1, &[])).is_empty(), "nobody follows nothing");
    }

    #[test]
    fn a_tie_goes_to_the_religion_that_arrived_first() {
        // Two religions with two followers each: the one first in the city's list wins, whatever
        // their ids.
        let c = city(4, &[(None, 0), (Some(3), 200), (Some(1), 200)]);
        assert_eq!(majority_of(&c), Some(ReligionId(3)));
        let c = city(4, &[(None, 0), (Some(1), 200), (Some(3), 200)]);
        assert_eq!(majority_of(&c), Some(ReligionId(1)));
        // A citizen left over goes to the first of the largest remainders.
        let c = city(3, &[(Some(2), 100), (None, 100), (Some(0), 100)]);
        assert_eq!(followers(&c).to_vec(), [(ReligionId(2), 1), (ReligionId(0), 1)]);
    }
}
