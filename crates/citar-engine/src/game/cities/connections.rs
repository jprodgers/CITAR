//! Which cities a trade route links to their capital, and by what (`cities.py:1967-2072`,
//! UnCiv's `CapitalConnectionsFinder`).
//!
//! Python walked outward from the capital city by city: from each city it had reached, one
//! search per medium (a harbour's water, the railroad, the road), each a fresh depth-first search
//! over the map, 831 searches a round on a small map. Here each medium is one flood fill whose
//! visited set only grows (DESIGN.md 6.11): the railroad floods from the capital; the road floods
//! from every city reached so far, and a harbour's water from every harbour reached so far, and
//! the two alternate until neither reaches a new city. Python's walk reached the same cities by
//! the same media, since a city reached by any medium was searched from by every medium it
//! could use, and the railroad reached nothing a flood from the capital does not:
//! - the capital is `start`; every city reached is searched from by road, so each is `road` once
//!   the road is known, itself included;
//! - `railroad`: the cities a flood from the capital over railroads and cities reaches, once the
//!   railroad is known;
//! - `harbor`: a harbour of the owner's that a flood over water and cities from a harbour
//!   reached reaches, itself included.
//!
//! A route passes through a civilization's land only where the owner may: its own, a
//! city-state's it is not at war with, one it has met that gives it open borders
//! (`_can_enter_borders`), and never the barbarians'.

use smallvec::SmallVec;

use super::super::Game;
use crate::base::ids::{CityId, PlayerId, TileIdx};
use crate::base::sets::BitSet;
use crate::game::core::has_type;
use crate::rules::defs::Route;
use crate::unique::{Ctx, UniqueType, uq};

bitflags::bitflags! {
    /// How a city is linked to its capital (`connected_cities`' medium sets).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Media: u8 {
        /// The capital itself.
        const START = 1 << 0;
        const ROAD = 1 << 1;
        const RAILROAD = 1 << 2;
        const HARBOR = 1 << 3;
    }
}

/// Which of a civilization's cities are linked to its capital, and by what: its cities that are,
/// in id order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connectivity {
    pub cities: SmallVec<[(CityId, Media); 8]>,
}

impl Connectivity {
    /// How a city is linked, if it is.
    #[must_use]
    pub fn media(&self, c: CityId) -> Option<Media> {
        self.cities.binary_search_by_key(&c, |&(x, _)| x).ok().map(|i| self.cities[i].1)
    }
}

impl super::super::derive::rev::BitEq for Connectivity {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// Whether a trade route of `p` may pass through `other`'s land (`cities._can_enter_borders`,
/// `cities.py:1967-1977`).
#[must_use]
pub fn can_enter_borders(g: &Game, p: PlayerId, other: PlayerId) -> bool {
    if other == p {
        return true;
    }
    if g.is_barbarian(other) || g.is_barbarian(p) || !g.has_met(p, other) {
        return false;
    }
    if g.is_city_state(other) && !g.at_war(p, other) {
        return true;
    }
    g.has_open_borders(other, p)
}

/// Whether a city has a harbour, which links it to other harbours over water
/// (`connected_cities`' `harbor`, `cities.py:2010-2012`).
fn harbor(v: &crate::game::EvalView<'_>, c: CityId) -> bool {
    use crate::unique::FilterFacts as _;
    let r = v.game().rules();
    v.game().city(c).is_some()
        && v.city_buildings(c)
            .iter()
            .any(|b| has_type(r, &r.buildings()[b].uniques, UniqueType::ConnectTradeRoutes))
}

/// The tiles each medium may pass, besides cities.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Medium {
    Road,
    Railroad,
    Water,
}

/// What a civilization's routes may pass: the facts every flood reads, gathered once.
struct Map<'a> {
    g: &'a Game,
    p: PlayerId,
    /// The tiles of the cities whose owners a route may pass, with the city.
    cities: Vec<(TileIdx, CityId)>,
    /// Forests and jungles of its own count as roads (`ForestsAndJunglesAreRoads`, once the
    /// Wheel is known).
    forest_roads: bool,
    /// Whether each owner may be passed, by player id.
    enter: Vec<bool>,
}

impl Map<'_> {
    fn city_at(&self, t: TileIdx) -> Option<CityId> {
        self.cities.binary_search_by_key(&t, |&(x, _)| x).ok().map(|i| self.cities[i].1)
    }

    /// Whether a flood of `medium` may step onto `t` (`bfs`' test, `cities.py:2014-2027`): a
    /// city, or a tile the medium passes; and land a route may pass through.
    fn passes(&self, t: TileIdx, medium: Medium) -> bool {
        let g = self.g;
        let Some(tile) = g.tile(t) else { return false };
        if let Some(o) = tile.owner()
            && !self.enter.get(usize::from(o.0)).copied().unwrap_or(false)
        {
            return false;
        }
        if self.city_at(t).is_some() {
            return true;
        }
        match medium {
            Medium::Water => g.is_water(t),
            Medium::Railroad => tile.route() == Some(Route::Railroad) && !tile.route_pillaged(),
            Medium::Road => {
                if tile.route().is_some() && !tile.route_pillaged() {
                    return true;
                }
                // _has_connection (cities.py:1980-1988): forests and jungles of its own.
                if !self.forest_roads || tile.owner() != Some(self.p) {
                    return false;
                }
                let known = &g.rules().derived().known.map;
                let top = tile
                    .features()
                    .top()
                    .and_then(|f| g.rules().derived().features.get(f).copied());
                top.is_some() && (top == known.forest || top == known.jungle)
            }
        }
    }
}

/// One medium's flood: its visited tiles, grown from each source added to it.
struct Flood {
    medium: Medium,
    seen: BitSet,
    stack: Vec<TileIdx>,
}

impl Flood {
    fn new(medium: Medium, tiles: usize) -> Self {
        Self {
            medium,
            seen: BitSet::with_capacity(u32::try_from(tiles).unwrap_or(u32::MAX)),
            stack: Vec::new(),
        }
    }

    /// Floods on from `start`, adding the cities it reaches to `reached`.
    fn from(&mut self, m: &Map<'_>, start: TileIdx, reached: &mut Vec<CityId>) {
        if !self.seen.insert(start.0) {
            return;
        }
        if let Some(c) = m.city_at(start) {
            reached.push(c);
        }
        self.stack.push(start);
        while let Some(cur) = self.stack.pop() {
            for n in m.g.grid().neighbors(cur) {
                if self.seen.contains(n.0) || !m.passes(n, self.medium) {
                    continue;
                }
                self.seen.insert(n.0);
                if let Some(c) = m.city_at(n) {
                    reached.push(c);
                }
                self.stack.push(n);
            }
        }
    }
}

/// How each of civilization `p`'s cities is linked to its capital (`cities.connected_cities`,
/// `cities.py:1991-2064`): what the memo `Connectivity` holds.
#[must_use]
pub fn connected_cities(g: &Game, p: PlayerId) -> Connectivity {
    connected_cities_in(&g.view(), p)
}

/// The same, with its uniques read in view `v`: the supply's view reads it afresh, without the
/// memo, which reads the index the supply feeds.
#[must_use]
pub fn connected_cities_in(v: &crate::game::EvalView<'_>, p: PlayerId) -> Connectivity {
    let g = v.game();
    let mut out = Connectivity::default();
    let Some(cap) = g.player(p).and_then(|x| x.capital).and_then(|c| g.city(c)) else {
        return out;
    };
    if cap.owner() != p {
        return out;
    }
    let r = g.rules();
    let known = &r.derived().known;
    let road_ok = g.has_tech(p, r.improvements()[known.road].tech_required);
    let rail_ok = g.has_tech(p, r.improvements()[known.railroad].tech_required);
    let st = g.state();
    let enter: Vec<bool> = st.players().ids().map(|q| can_enter_borders(g, p, q)).collect();
    let mut cities: Vec<(TileIdx, CityId)> = st
        .cities()
        .iter()
        .filter(|c| enter.get(usize::from(c.owner().0)).copied().unwrap_or(false))
        .map(|c| (c.tile(), c.id()))
        .collect();
    cities.sort_by_key(|&(t, _)| t);
    let forest_roads = g.has_tech(p, known.the_wheel)
        && uq::any(uq::civ(v, p, UniqueType::ForestsAndJunglesAreRoads, &Ctx::civ(p)));
    let m = Map { g, p, cities, forest_roads, enter };
    let n = st.tiles().len();
    // The media each reached city has, by the raw id of the city.
    let mut media: Vec<(CityId, Media)> = vec![(cap.id(), Media::START)];
    let add = |media: &mut Vec<(CityId, Media)>, c: CityId, how: Media| -> bool {
        match media.iter_mut().find(|(x, _)| *x == c) {
            Some((_, m)) => {
                *m |= how;
                false
            }
            None => {
                media.push((c, how));
                true
            }
        }
    };
    let mut reached = Vec::new();
    if rail_ok {
        let mut rail = Flood::new(Medium::Railroad, n);
        rail.from(&m, cap.tile(), &mut reached);
        for c in reached.drain(..) {
            add(&mut media, c, Media::RAILROAD);
        }
    }
    let mut road = Flood::new(Medium::Road, n);
    let mut water = Flood::new(Medium::Water, n);
    // Each city reached is searched from, once, by the media it can use.
    let mut done = 0;
    while done < media.len() {
        let (c, _) = media[done];
        done += 1;
        let Some(tile) = g.city(c).map(crate::state::cities::City::tile) else { continue };
        if harbor(v, c) {
            water.from(&m, tile, &mut reached);
            for x in reached.drain(..) {
                if g.city(x).is_some_and(|y| y.owner() == p) && harbor(v, x) {
                    add(&mut media, x, Media::HARBOR);
                }
            }
        }
        if road_ok {
            // A city a flood reached before is in its visited set, so it is searched from again:
            // Python searched from every city it reached, and every city the flood reaches from
            // it was reached from the city before.
            road.seen.remove(tile.0);
            road.from(&m, tile, &mut reached);
            for x in reached.drain(..) {
                add(&mut media, x, Media::ROAD);
            }
        }
    }
    media.retain(|(c, _)| g.city(*c).is_some_and(|x| x.owner() == p));
    media.sort_by_key(|&(c, _)| c);
    out.cities = media.into_iter().collect();
    out
}

/// A plain port of Python's walk, one search per city and medium: for the property test that
/// the floods agree with it.
#[cfg(any(test, feature = "test-ops"))]
#[must_use]
pub fn connected_cities_naive(g: &Game, p: PlayerId) -> Connectivity {
    let mut out = Connectivity::default();
    let Some(cap) = g.player(p).and_then(|x| x.capital).and_then(|c| g.city(c)) else {
        return out;
    };
    if cap.owner() != p {
        return out;
    }
    let r = g.rules();
    let known = &r.derived().known;
    let road_ok = g.has_tech(p, r.improvements()[known.road].tech_required);
    let rail_ok = g.has_tech(p, r.improvements()[known.railroad].tech_required);
    let st = g.state();
    let enter: Vec<bool> = st.players().ids().map(|q| can_enter_borders(g, p, q)).collect();
    let mut cities: Vec<(TileIdx, CityId)> = st
        .cities()
        .iter()
        .filter(|c| enter.get(usize::from(c.owner().0)).copied().unwrap_or(false))
        .map(|c| (c.tile(), c.id()))
        .collect();
    cities.sort_by_key(|&(t, _)| t);
    let v = g.view();
    let forest_roads = g.has_tech(p, known.the_wheel)
        && uq::any(uq::civ(&v, p, UniqueType::ForestsAndJunglesAreRoads, &Ctx::civ(p)));
    let m = Map { g, p, cities: cities.clone(), forest_roads, enter };
    let bfs = |start: TileIdx, medium: Medium| -> Vec<TileIdx> {
        let mut seen = vec![start];
        let mut stack = vec![start];
        while let Some(cur) = stack.pop() {
            for n in g.grid().neighbors(cur) {
                if seen.contains(&n) || !m.passes(n, medium) {
                    continue;
                }
                seen.push(n);
                stack.push(n);
            }
        }
        seen
    };
    let mut media: Vec<(CityId, Media)> = vec![(cap.id(), Media::START)];
    let mut frontier = vec![cap.id()];
    let mut searched: Vec<(CityId, u8)> = Vec::new();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for c in frontier {
            let meds = media.iter().find(|(x, _)| *x == c).map_or(Media::empty(), |&(_, m)| m);
            let Some(tile) = g.city(c).map(crate::state::cities::City::tile) else { continue };
            let mut checks: Vec<(Media, Medium, bool)> = Vec::new();
            if harbor(&g.view(), c) {
                checks.push((Media::HARBOR, Medium::Water, true));
            }
            if rail_ok && meds.intersects(Media::START | Media::RAILROAD) {
                checks.push((Media::RAILROAD, Medium::Railroad, false));
            }
            if road_ok {
                checks.push((Media::ROAD, Medium::Road, false));
            }
            for (how, medium, harbours_only) in checks {
                if searched.contains(&(c, how.bits())) {
                    continue;
                }
                searched.push((c, how.bits()));
                let reached = bfs(tile, medium);
                for &(t, x) in &cities {
                    if !reached.contains(&t) {
                        continue;
                    }
                    if harbours_only
                        && !(g.city(x).is_some_and(|y| y.owner() == p) && harbor(&g.view(), x))
                    {
                        continue;
                    }
                    match media.iter_mut().find(|(y, _)| *y == x) {
                        Some((_, m)) => *m |= how,
                        None => {
                            media.push((x, how));
                            next.push(x);
                        }
                    }
                }
            }
        }
        frontier = next;
    }
    media.retain(|(c, _)| g.city(*c).is_some_and(|x| x.owner() == p));
    media.sort_by_key(|&(c, _)| c);
    out.cities = media.into_iter().collect();
    out
}
