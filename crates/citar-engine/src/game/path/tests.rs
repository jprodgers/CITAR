//! The gates of package 1c-02 for paths, on random worlds: the search gives the path Python's
//! Dijkstra gave (gate 2), a step costs what a direct port of `enter_cost` says (gate 3), and a
//! unit whose moves exceed its full moves gets a correct path (gate 4).

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::astar::Label;
use super::{ALL, Mover, Start, stack_reason};
use crate::base::collections::MinHeap;
use crate::base::ids::{
    BaseUnitId, CityId, FeatureId, PlayerId, PromotionId, TechId, TerrainId, TileIdx, UnitId,
};
use crate::base::sets::FeatureSet;
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::{PlayerTouch, UnitTouch};
use crate::rules::Ruleset;
use crate::rules::defs::{Domain, Route};
use crate::state::TileClaim;
use crate::state::map::Tile;
use crate::unique::filter::UnitScope;
use crate::unique::{Ctx, FilterFacts, UniqueData, UniqueType, uq};

fn r() -> &'static Ruleset {
    Ruleset::shared()
}

fn terrain(name: &str) -> TerrainId {
    r().lookup::<TerrainId>(name).expect("a terrain of the ruleset")
}

fn feature(name: &str) -> FeatureId {
    r().terrains()[terrain(name)].feature.expect("a feature")
}

const BASES: [&str; 7] = ["Grassland", "Plains", "Desert", "Tundra", "Coast", "Ocean", "Mountain"];
const FEATURES: [Option<&str>; 5] =
    [None, Some("Hill"), Some("Forest"), Some("Jungle"), Some("Marsh")];
const UNITS: [&str; 12] = [
    "Warrior",
    "Scout",
    "Horseman",
    "Chariot Archer",
    "Worker",
    "Settler",
    "Trireme",
    "Great General",
    "Helicopter Gunship",
    "Minuteman",
    "Missionary",
    "Work Boats",
];

/// A random world on the 10x8 test map, as the generator's numbers describe it.
#[derive(Clone, Debug)]
struct World {
    ground: Vec<(u8, u8)>,
    rivers: Vec<(u16, u8)>,
    routes: Vec<(u16, u8)>,
    cities: Vec<(u8, u16)>,
    claims: Vec<(u16, u8)>,
    units: Vec<(u8, u8, u16, u8)>,
    wars: Vec<(u8, u8)>,
    explored: Vec<bool>,
    techs: Vec<bool>,
    woodsmen: Vec<bool>,
}

fn world() -> impl Strategy<Value = World> {
    (
        proptest::collection::vec((0u8..7, 0u8..5), 80),
        proptest::collection::vec((0u16..80, 0u8..64), 0..10),
        proptest::collection::vec((0u16..80, 0u8..4), 0..24),
        proptest::collection::vec((0u8..3, 0u16..80), 0..4),
        proptest::collection::vec((0u16..80, 0u8..4), 0..20),
        proptest::collection::vec((0u8..4, 0u8..12, 0u16..80, 0u8..8), 2..14),
        proptest::collection::vec((0u8..3, 0u8..3), 0..3),
        proptest::collection::vec(any::<bool>(), 80),
        proptest::collection::vec(any::<bool>(), 4),
        proptest::collection::vec(any::<bool>(), 14),
    )
        .prop_map(
            |(ground, rivers, routes, cities, claims, units, wars, explored, techs, woodsmen)| {
                World {
                    ground,
                    rivers,
                    routes,
                    cities,
                    claims,
                    units,
                    wars,
                    explored,
                    techs,
                    woodsmen,
                }
            },
        )
}

/// The game a world describes, settled: its sight, and so its fog, built.
fn build(w: &World) -> Game {
    let mut g = testing::duel();
    for (i, &(b, f)) in w.ground.iter().enumerate() {
        let t = TileIdx(u32::try_from(i).unwrap_or(0));
        g.set_terrain(t, terrain(BASES[usize::from(b)])).expect("a tile");
        let water = matches!(b, 4 | 5);
        let mut fs = FeatureSet::EMPTY;
        if !water
            && b != 6
            && let Some(name) = FEATURES[usize::from(f)]
        {
            fs.insert(feature(name));
        }
        g.set_features(t, fs).expect("a tile");
    }
    for &(t, mask) in &w.rivers {
        g.set_river(TileIdx(u32::from(t)), mask).expect("a tile");
    }
    for &(t, kind) in &w.routes {
        let t = TileIdx(u32::from(t));
        let route = if kind == 1 { Route::Railroad } else { Route::Road };
        g.set_route(t, Some(route)).expect("a tile");
        g.set_pillaged(t, kind == 3, false).expect("a tile");
    }
    let mut cities: Vec<CityId> = Vec::new();
    for (i, &(owner, t)) in w.cities.iter().enumerate() {
        let t = TileIdx(u32::from(t));
        if g.city_at(t).is_some() || g.tile(t).and_then(Tile::owner).is_some() {
            continue;
        }
        cities.push(testing::city(&mut g, PlayerId(owner), t, &format!("Town {i}")));
    }
    for &(t, c) in &w.claims {
        let t = TileIdx(u32::from(t));
        let Some(&c) = cities.get(usize::from(c)) else { continue };
        if g.city_at(t).is_some() {
            continue;
        }
        let owner = g.city(c).map(crate::state::cities::City::owner).expect("a city");
        g.set_tile_owner(t, TileClaim::city(owner, c)).expect("a tile");
    }
    for &(a, b) in &w.wars {
        if a != b {
            g.update_relation(PlayerId(a), PlayerId(b), |x| x.war = true).expect("a pair");
        }
    }
    let techs = ["Optics", "Astronomy", "The Wheel", "Railroads"];
    for (i, &on) in w.techs.iter().enumerate() {
        if on {
            let t = r().lookup::<TechId>(techs[i]).expect("a tech");
            for p in [PlayerId(0), PlayerId(1)] {
                crate::game::research::add_tech_silently(&mut g, p, &[t]);
            }
        }
    }
    let woodsman = r().lookup::<PromotionId>("Woodsman").expect("a promotion");
    for (i, &(owner, kind, t, moves)) in w.units.iter().enumerate() {
        let u =
            testing::unit(&mut g, PlayerId(owner), UNITS[usize::from(kind)], TileIdx(u32::from(t)));
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
            x.moves = i32::from(moves) * 30;
            if w.woodsmen.get(i).copied().unwrap_or(false) {
                x.promotions.insert(woodsman);
            }
        }
    }
    for (i, &e) in w.explored.iter().enumerate() {
        if e {
            for p in [PlayerId(0), PlayerId(1)] {
                if let Some(pl) = g.player_mut(p, PlayerTouch::OTHER) {
                    pl.explored.insert(u32::try_from(i).unwrap_or(0));
                }
            }
        }
    }
    g.settle();
    g
}

// ---- Python's search, as it was --------------------------------------------------------------------

/// `movement.find_path`, ported line by line: a Dijkstra over `(turns, -moves_left, tile)` that
/// keeps the first label to reach each tile.
fn dijkstra(m: &Mover<'_>, s: Start, target: TileIdx, max_turns: u32) -> Option<Vec<TileIdx>> {
    let g = m.game();
    if target == s.tile {
        return Some(vec![s.tile]);
    }
    if m.is_air() {
        return None;
    }
    let known = m.barbarian || g.player(m.pid).is_some_and(|p| p.explored.contains(target.0));
    if known && m.terrain_reason(target).is_some() {
        return None;
    }
    let mut best: BTreeMap<TileIdx, (u32, i64)> = BTreeMap::new();
    let mut prev: BTreeMap<TileIdx, TileIdx> = BTreeMap::new();
    let mut heap: MinHeap<(u32, i64, u32), ()> = MinHeap::new();
    best.insert(s.tile, (0, -i64::from(s.moves)));
    heap.push((0, -i64::from(s.moves), s.tile.0), ());
    while let Some(((turns, negleft, raw), ())) = heap.pop() {
        let cur = TileIdx(raw);
        if best.get(&cur) != Some(&(turns, negleft)) {
            continue;
        }
        if cur == target || turns > max_turns {
            break;
        }
        let left = i32::try_from(-negleft).unwrap_or(i32::MAX);
        for nb in g.grid().neighbors(cur) {
            if !m.passable(nb, Some(target)) {
                continue;
            }
            let next = Label { turns, left }.step(m.edge_cost(cur, nb), s.full);
            if next.left == 0
                && nb != target
                && m.own_unit_at(nb)
                && stack_reason(g, m.pid, m.base, nb, m.unit).is_some()
            {
                continue;
            }
            let key = (next.turns, -i64::from(next.left));
            if best.get(&nb).is_none_or(|&old| key < old) {
                best.insert(nb, key);
                prev.insert(nb, cur);
                heap.push((key.0, key.1, nb.0), ());
            }
        }
    }
    best.get(&target)?;
    let mut path = vec![target];
    while path.last() != Some(&s.tile) {
        path.push(*prev.get(path.last()?)?);
    }
    path.reverse();
    Some(path)
}

// ---- Python's step cost, as it was -----------------------------------------------------------------

/// The governing terrain's name (`tiles.last_terrain`).
fn last_terrain(t: &Tile) -> &'static str {
    let rr = r();
    match t.features().top() {
        Some(f) => &rr.terrains()[rr.derived().features[f]].name,
        None => &rr.terrains()[t.wonder().unwrap_or(t.terrain())].name,
    }
}

/// `movement.enter_cost`, ported line by line with the names Python compared.
#[allow(clippy::too_many_lines, reason = "a line-by-line port")]
fn enter_cost_direct(g: &Game, u: UnitId, a: TileIdx, b: TileIdx) -> i32 {
    let rr = r();
    let v = g.view();
    let x = g.unit(u).expect("a unit");
    let pid = x.owner();
    let def = &rr.base_units()[x.base];
    let sc = rr.constants().move_scale;
    let ctx = Ctx::unit(&v, u);
    let has = |ty| uq::any(uq::unit(&v, u, ty, &ctx));
    let civ_has = |ty| uq::any(uq::civ(&v, pid, ty, &Ctx::civ(pid)));
    let city = |t: TileIdx| g.city_at(t).is_some();
    let land = |t: TileIdx| !g.is_water(t) || city(t);
    let tb = g.tile(b).expect("a tile");
    let least = |ty| {
        uq::unit_and_civ(&v, u, ty, &ctx)
            .filter_map(|h| match h.data() {
                UniqueData::ReducedDisembarkCost(y) => Some(y.movement * sc),
                UniqueData::ReducedEmbarkCost(y) => Some(y.movement * sc),
                _ => None,
            })
            .min()
    };
    let on_water = has(UniqueType::CanMoveOnWater);
    if def.domain == Domain::Land && land(a) != land(b) && !on_water {
        if !land(a) && land(b) {
            return least(UniqueType::ReducedDisembarkCost).unwrap_or(ALL);
        }
        return least(UniqueType::ReducedEmbarkCost).unwrap_or(ALL);
    }
    let zoc_between = || {
        for n in g.grid().neighbors(a) {
            if g.grid().distance(n, b) != 1 {
                continue;
            }
            if let Some(c) = g.city_at(n) {
                if g.at_war(pid, c.owner()) {
                    return true;
                }
                continue;
            }
            if let Some(m) = g.military_at(n)
                && g.at_war(pid, m.owner())
            {
                let md = &rr.base_units()[m.base];
                if md.domain == Domain::Water
                    || (def.domain == Domain::Land && !v.unit_embarked(m.id()))
                {
                    return true;
                }
            }
        }
        false
    };
    if !g.is_barbarian(pid) && !has(UniqueType::IgnoresZOC) && zoc_between() {
        return ALL;
    }
    if has(UniqueType::AllTilesCost1Move) {
        return sc;
    }
    let mut extra = 0;
    if let Some(owner) = tb.owner()
        && g.at_war(pid, owner)
    {
        for h in uq::civ(&v, owner, UniqueType::EnemyUnitsSpendExtraMovement, &Ctx::civ(owner)) {
            if let UniqueData::EnemyUnitsSpendExtraMovement(y) = h.data()
                && rr.uniques().filters().unit_matches(y.units, &v, u, UnitScope::default())
            {
                extra += y.movement * sc;
            }
        }
    }
    let known = &rr.derived().known;
    let road_tech = rr.improvements()[known.road].tech_required;
    let rail_tech = rr.improvements()[known.railroad].tech_required;
    let route_at = |t: TileIdx| {
        let tile = g.tile(t).expect("a tile");
        let own = if tile.route_pillaged() { None } else { tile.route() };
        if let Some(c) = g.city_at(t) {
            if g.has_tech(c.owner(), rail_tech) {
                return Some(Route::Railroad);
            }
            if g.has_tech(c.owner(), road_tech) {
                return own.or(Some(Route::Road));
            }
        }
        own
    };
    let connected = |t: TileIdx| {
        if route_at(t).is_some() {
            return true;
        }
        let tile = g.tile(t).expect("a tile");
        let top = tile.features().iter().filter(|&f| f != known.hill).last();
        let name = top.map(|f| &*rr.terrains()[rr.derived().features[f]].name);
        tile.owner() == Some(pid)
            && matches!(name, Some("Forest" | "Jungle"))
            && civ_has(UniqueType::ForestsAndJunglesAreRoads)
    };
    if route_at(a) == Some(Route::Railroad) && route_at(b) == Some(Route::Railroad) {
        return sc / 10 + extra;
    }
    let ta = g.tile(a).expect("a tile");
    let crossing = ta.river_mask() != 0 && tb.river_mask() != 0 && {
        let d = (0..6).find(|&d| g.grid().neighbor_table(a)[d] == b.0);
        d.is_some_and(|d| ta.river_mask() & (1 << d) != 0)
    };
    if connected(a) && connected(b) && (!crossing || civ_has(UniqueType::RoadsConnectAcrossRivers))
    {
        let road = if civ_has(UniqueType::RoadMovementSpeed) { sc / 3 } else { sc / 2 };
        return road + extra;
    }
    if has(UniqueType::IgnoresTerrainCost) {
        return sc + extra;
    }
    if crossing {
        return ALL;
    }
    let terrain_cost = if city(b) {
        sc
    } else {
        rr.terrains()[rr.lookup::<TerrainId>(last_terrain(tb)).expect("a terrain")].movement_cost
            * sc
    };
    let bctx = Ctx { civ: Some(pid), unit: Some(u), tile: Some(b), ..Ctx::default() }.resolve(&v);
    let doubles: Vec<(String, crate::base::ids::UniqueId)> =
        uq::unit(&v, u, UniqueType::DoubleMovementOnTerrain, &Ctx::IGNORE)
            .map(|h| match h.data() {
                UniqueData::DoubleMovementOnTerrain(y) => {
                    (rr.uniques().tile_filter(y.terrain).to_owned(), h.id)
                }
                _ => unreachable!("a double-movement unique"),
            })
            .collect();
    let features: Vec<&str> =
        tb.features().iter().map(|f| &*rr.terrains()[rr.derived().features[f]].name).collect();
    let base = &*rr.terrains()[tb.terrain()].name;
    let applies = |id| crate::unique::applies(id, &bctx, &v);
    for (f, id) in &doubles {
        if features.contains(&f.as_str()) && applies(*id) {
            return terrain_cost / 2 + extra;
        }
    }
    let rough = rr.terrains()[tb.terrain()].rough
        || tb.features().iter().any(|f| rr.terrains()[rr.derived().features[f]].rough);
    if has(UniqueType::RoughTerrainPenalty) && rough {
        return ALL;
    }
    if features.contains(&"Hill") && civ_has(UniqueType::IgnoreHillMovementCost) {
        return sc + extra;
    }
    for (f, id) in &doubles {
        if (f == base || (f == "Hill" && features.contains(&"Hill"))) && applies(*id) {
            return terrain_cost / 2 + extra;
        }
    }
    let filters = rr.uniques().filters();
    for (f, id) in &doubles {
        if !features.contains(&f.as_str()) && f != base && f != "Hill" && applies(*id) {
            let tf = uq::unit(&v, u, UniqueType::DoubleMovementOnTerrain, &Ctx::IGNORE)
                .find(|h| h.id == *id)
                .and_then(|h| match h.data() {
                    UniqueData::DoubleMovementOnTerrain(y) => Some(y.terrain),
                    _ => None,
                })
                .expect("its filter");
            if filters.tile_matches(tf, &v, b, Some(pid)) {
                return terrain_cost / 2 + extra;
            }
        }
    }
    terrain_cost + extra
}

// ---- The gates -------------------------------------------------------------------------------------

/// Every unit of the world that walks, with its search start.
fn movers(g: &Game) -> Vec<UnitId> {
    g.state().units().iter().map(crate::state::units::Unit::id).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Gate 2: on random terrain, routes, rivers, borders, wars, fog and units, the search finds
    /// exactly the path Python's Dijkstra found, to every tile of the map.
    #[test]
    fn the_search_finds_pythons_path(w in world()) {
        let g = build(&w);
        for u in movers(&g) {
            let Some(m) = Mover::unit(&g, u) else { continue };
            let Some(s) = m.start() else { continue };
            for i in 0..80u32 {
                let t = TileIdx(i);
                let fast = m.find_path_from(s, t, 40);
                let slow = dijkstra(&m, s, t, 40);
                prop_assert_eq!(&fast, &slow, "unit {:?} from {:?} to {:?}", u, s.tile, t);
                if let Some(p) = fast {
                    prop_assert_eq!(m.path_turns_from(s, &p), m.path_turns_from(s, &slow.unwrap_or_default()));
                }
            }
        }
    }

    /// Gate 3: a step costs what Python's `enter_cost` said, on every edge of the map.
    #[test]
    fn a_step_costs_what_enter_cost_said(w in world()) {
        let g = build(&w);
        for u in movers(&g) {
            let Some(m) = Mover::unit(&g, u) else { continue };
            for i in 0..80u32 {
                let a = TileIdx(i);
                for b in g.grid().neighbors(a) {
                    prop_assert_eq!(m.edge_cost(a, b), enter_cost_direct(&g, u, a, b), "unit {:?} {:?}->{:?}", u, a, b);
                }
            }
        }
    }

    /// The search's bound is its label at the target, and never falls along a step, from any
    /// label (it is consistent), so it never says more than a path could reach: what lets the
    /// search expand each tile once.
    #[test]
    fn the_bound_grows_along_every_step(w in world(), turns in 0u32..3, left in 0i32..400) {
        let g = build(&w);
        let net = super::route_net(&g);
        let l = Label { turns, left };
        for u in movers(&g) {
            let Some(m) = Mover::unit(&g, u) else { continue };
            let Some(s) = m.start() else { continue };
            for t in (0..80u32).step_by(7).map(TileIdx) {
                let h = super::astar::Heur::new(m.floors(), s.full, &net, t, m.civ.forest_roads);
                prop_assert_eq!(h.key(l, t, 0), l.key());
                for a in (0..80u32).map(TileIdx) {
                    let before = h.key(l, a, g.grid().distance(a, t));
                    for b in g.grid().neighbors(a) {
                        let next = l.step(m.edge_cost(a, b), s.full);
                        let after = h.key(next, b, g.grid().distance(b, t));
                        prop_assert!(before <= after, "unit {:?} {:?}->{:?} toward {:?}", u, a, b, t);
                    }
                }
            }
        }
    }

    /// A tree answers every path as the search does with its turn limit.
    #[test]
    fn a_tree_answers_as_the_search(w in world(), limit in 1u32..6) {
        let g = build(&w);
        for u in movers(&g) {
            let Some(m) = Mover::unit(&g, u) else { continue };
            let Some(tree) = super::PathTree::build(&m, limit) else { continue };
            for i in 0..80u32 {
                let t = TileIdx(i);
                prop_assert_eq!(tree.path_to(&m, t), m.find_path(t, limit), "unit {:?} to {:?}", u, t);
            }
        }
    }
}

/// Gate 4: a unit whose moves are far above its full moves (a key that the earlier `t·(F+1) +
/// (F−l)` would have underflowed) still gets Python's path, and its movement is kept for the
/// first turn.
#[test]
fn moves_above_the_full_moves_find_a_path() {
    let mut g = testing::duel();
    let u = testing::unit(&mut g, PlayerId(0), "Warrior", TileIdx(0));
    let full = crate::game::movement::max_moves(&g, u);
    if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
        x.moves = full * 50;
    }
    for i in 0..80u32 {
        if let Some(pl) = g.player_mut(PlayerId(0), PlayerTouch::OTHER) {
            pl.explored.insert(i);
        }
    }
    g.settle();
    let m = Mover::unit(&g, u).expect("a unit");
    let s = m.start().expect("a start");
    assert!(s.moves > s.full);
    let far = TileIdx(79);
    let p = m.find_path(far, 40).expect("a path across open grassland");
    assert_eq!(Some(p.clone()), dijkstra(&m, s, far, 40));
    assert_eq!(p.first(), Some(&TileIdx(0)));
    assert_eq!(p.last(), Some(&far));
    assert_eq!(m.path_turns(&p), 1, "fifty turns' movement crosses the map in one");
    let reach = m.reachable();
    assert_eq!(reach.len(), 79, "every other tile of the map this turn");
}

/// The key keeps the order of Python's tuples for every label a search makes, above the full
/// moves included.
#[test]
fn keys_keep_pythons_order() {
    let labels: Vec<Label> = (0..4u32)
        .flat_map(|t| [0, 1, 59, 60, 120, 6_000].map(move |l| Label { turns: t, left: l }))
        .collect();
    for a in &labels {
        for b in &labels {
            let py = (a.turns, -i64::from(a.left)).cmp(&(b.turns, -i64::from(b.left)));
            assert_eq!(a.key().cmp(&b.key()), py, "{a:?} {b:?}");
        }
    }
}

/// A base unit id by name, for the tests below.
fn base(name: &str) -> BaseUnitId {
    r().lookup::<BaseUnitId>(name).expect("a unit")
}

/// A land unit may not embark without Optics, and costs all its moves to embark with it.
#[test]
fn embarking_needs_optics_and_takes_every_move() {
    let mut g = testing::duel();
    let coast = terrain("Coast");
    for y in 0..8 {
        g.set_terrain(TileIdx(y * 10), coast).expect("a tile");
    }
    let u = testing::unit(&mut g, PlayerId(0), "Warrior", TileIdx(1));
    if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
        x.moves = 120;
    }
    g.settle();
    let m = Mover::unit(&g, u).expect("a unit");
    assert!(m.pass_reason(TileIdx(0)).is_some(), "no embarking before Optics");
    let optics = r().lookup::<TechId>("Optics").expect("Optics");
    crate::game::research::add_tech_silently(&mut g, PlayerId(0), &[optics]);
    let m = Mover::unit(&g, u).expect("a unit");
    assert_eq!(m.pass_reason(TileIdx(0)), None);
    assert_eq!(m.edge_cost(TileIdx(1), TileIdx(0)), ALL);
    assert!(Mover::of_type(&g, PlayerId(0), base("Trireme")).is_some());
}
