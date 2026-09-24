//! Religion, as the rest of the game reads it: which religions a city's citizens follow, and
//! which religion most of them do (`religion.py:98-156, 321-328`).
//!
//! Package 1b-06 ports these reads, since a city's stats and its citizens depend on them: the
//! uniques of a city's majority religion's follower beliefs are the city's own
//! (`cities.local_umaps`, `cities.py:48-66`), and so is `[n]% [stat] from every follower`.
//! Package 1b-08 ports the rest of the system: pressure, spreading, founding and enhancing.
//!
//! What differs from Python: a city's pressures are kept sorted, no religion first, then by
//! religion in the order they were founded, where Python kept them in the order each religion
//! first reached the city, and broke two ties by that order: which religion a citizen left over
//! after the division goes to, and which of two religions with as many followers is the
//! majority. Both go by the order of founding here (`religion-ties-by-founding-order`).

use smallvec::SmallVec;

use super::Game;
use crate::base::ids::{CityFilterId, CityId, PlayerId, ReligionId};
use crate::state::cities::City;

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
        let n = if per == 0.0 { 0 } else { crate::base::num::trunc_i32(v / per) };
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

/// The majority of a city's own followers, whether or not religion is in play. Religions with as
/// many followers are told apart by the order they were founded in
/// (`religion-ties-by-founding-order`).
fn majority_of(city: &City) -> Option<ReligionId> {
    let f = followers(city);
    // refcheck: religion-ties-by-founding-order
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
}
