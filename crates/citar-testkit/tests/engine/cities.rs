//! Cities' stats, connectivity and citizens (package 1b-06), on the arena:
//! - the kitchen sink's `[n]% Food consumption by specialists [cities]` in a city's food and its
//!   citizens' choice, and its `Cost increases by [n] when built` and `[n]% production cost` in
//!   what a building costs;
//! - the flood fills that link cities to their capital agree with a plain port of Python's walk,
//!   one search per city and medium, on random maps of roads, railroads, harbours, borders and
//!   relations, and the memo agrees with both after edits (gate 3);
//! - P8 in miniature (gate 5): queries and refused calls between two actions leave every digest
//!   what it is without them;
//! - every check, the cache and citizen oracles among them, clean after each.

use citar_engine::api::{ActionError, testops, tools};
use citar_engine::base::ids::{BuildingId, CityId, PlayerId, TileIdx};
use citar_engine::base::stats::Stat;
use citar_engine::game::cities::citizens::{RankCtx, rank_stats_for_work};
use citar_engine::game::cities::connections::{connected_cities, connected_cities_naive};
use citar_engine::game::cities::stats::{self as cstats, StatSource};
use citar_engine::game::{Action, DebugOptions, Game, query};
use citar_engine::rules::Ruleset;
use citar_engine::state::State;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::Constructible;
use citar_testkit::rulesets::kitchen_sink;
use citar_testkit::script::{map_doc, new_game};
use proptest::prelude::*;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);

/// A bare game on the arena for `players` Benchmark civilizations: no unit, no city-state, no
/// barbarian.
fn arena(r: &'static Ruleset, players: usize) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let seats: Vec<Value> = (0..players).map(|_| json!({"nation": "BenchmarkCiv"})).collect();
    let cfg = json!({
        "seed": 1,
        "players": seats,
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("cleared");
    g
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// A tool call as a host makes one.
fn tool(g: &mut Game, p: PlayerId, name: &str, args: &Value) -> Result<Value, ActionError> {
    let mut fields = tools::normalize(name, args)?;
    fields.insert("tool".into(), json!(name));
    let action: Action = serde_json::from_value(Value::Object(fields)).expect("the arguments fit");
    g.act(p, action).map(|(out, _)| out)
}

/// The id of the city an operation founded.
fn founded(out: &Value) -> CityId {
    out["city_id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .unwrap_or_else(|| panic!("no city in {out}"))
}

/// What a city's citizens eat, as its stats' `Population` line holds it.
fn population_food(g: &Game, c: CityId) -> f64 {
    let s = query::city_stats(g, c);
    s.breakdown
        .iter()
        .find(|(k, _)| *k == StatSource::Population)
        .map_or(0.0, |(_, y)| y.stats[Stat::Food])
}

// ---- The kitchen sink ---------------------------------------------------------------------------

#[test]
fn specialists_eat_less_where_the_kitchen_sink_says() {
    // `[-50]% Food consumption by specialists [in this city]` on the Kitchen Sink Works: a
    // merchant eats one food where a citizen on a tile eats two.
    let r = kitchen_sink();
    let mut g = arena(r, 2);
    let (out, _) = g
        .apply_ops(&json!([{"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma",
            "pop": 3, "claim_radius": 2, "buildings": ["Market"]}]))
        .expect("a city");
    let c = founded(&out[0]);
    for (x, y) in [(4, 3), (7, 4)] {
        tool(&mut g, ME, "work_tile", &json!({"city_id": c.get(), "x": x, "y": y})).expect("lock");
    }
    tool(
        &mut g,
        ME,
        "set_specialists",
        &json!({"city_id": c.get(), "specialists": {"Merchant": 1}}),
    )
    .expect("a merchant");
    assert!((population_food(&g, c) + 6.0).abs() < 1e-9, "{}", population_food(&g, c));
    let food = query::city_stats(&g, c).total[Stat::Food];
    let rc = RankCtx::of(&g, c).expect("a city");
    let merchant = cstats::specialist_stats(&g, c, r.lookup("Merchant").expect("a merchant"));
    let plain = rank_stats_for_work(&rc, &merchant, true, -1.0);

    g.apply_ops(
        &json!([{"op": "set_city", "city": c.get(), "add_buildings": ["Kitchen Sink Works"]}]),
    )
    .expect("the works");
    let city = g.city(c).expect("Roma");
    assert_eq!(city.worked.len(), 2);
    assert_eq!(city.specialists.iter().map(|&n| u32::from(n)).sum::<u32>(), 1);
    assert!((population_food(&g, c) + 5.0).abs() < 1e-9, "{}", population_food(&g, c));
    assert!((query::city_stats(&g, c).total[Stat::Food] - (food + 1.0)).abs() < 1e-9);
    // A starving city values the food a specialist no longer eats, as a tile's food.
    let rc = RankCtx::of(&g, c).expect("a city");
    let cheaper = rank_stats_for_work(&rc, &merchant, true, -1.0);
    assert!(cheaper > plain, "{cheaper} against {plain}");
    clean(&mut g);
}

#[test]
fn the_kitchen_sink_works_cost_less_and_more_with_each_built() {
    // `[-10]% production cost` and `Cost increases by [30] when built`: 100 less a tenth, and 30
    // more for each one the civilization built before, before the difficulty and the speed.
    let r = kitchen_sink();
    let mut g = arena(r, 2);
    let (out, _) = g
        .apply_ops(&json!([{"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"}]))
        .expect("a city");
    let c = founded(&out[0]);
    let works = Constructible::Building(r.lookup::<BuildingId>("Kitchen Sink Works").expect("it"));
    let monument = Constructible::Building(r.lookup::<BuildingId>("Monument").expect("it"));
    let d = &r.difficulties()[g.difficulty(Some(ME))];
    let scale = d.building_cost_modifier * g.speed().production_cost_modifier;
    let want = |base: f64| citar_engine::base::num::trunc_i32(base * scale);
    assert_eq!(cstats::production_cost(&g, ME, works, Some(c)), want(100.0 * 0.9));
    let plain = f64::from(r.buildings()[r.lookup::<BuildingId>("Monument").expect("it")].cost);
    assert_eq!(cstats::production_cost(&g, ME, monument, Some(c)), want(plain));

    // Two built before: the count lives with the civilization.
    let mut parts = g.state().clone().into_parts();
    if let Some(p) = parts.players.get_mut(ME) {
        p.civ.built_increasing.insert(works, 2);
        p.civ.built_increasing.insert(monument, 2);
    }
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    assert_eq!(cstats::production_cost(&g, ME, works, Some(c)), want((100.0 + 60.0) * 0.9));
    assert_eq!(cstats::production_cost(&g, ME, monument, Some(c)), want(plain), "no such unique");
    let turns = cstats::turns_to_build(&g, c, works);
    assert!(turns > 0, "{turns}");
    clean(&mut g);
}

// ---- Connectivity (gate 3) ----------------------------------------------------------------------

/// City sites on the arena, at least four tiles apart: the first three on the coast.
const SITES: [(i32, i32); 12] = [
    (3, 2),
    (3, 8),
    (3, 13),
    (8, 2),
    (8, 8),
    (8, 13),
    (13, 3),
    (13, 8),
    (13, 13),
    (18, 3),
    (18, 8),
    (21, 12),
];

/// A random world of three civilizations: who founds at each site (0 nobody, else the player
/// plus one), with what border radius and whether it has a harbour; routes laid from site to
/// site, with gaps; which techs each knows; and how each pair gets on.
#[derive(Clone, Debug)]
struct World {
    sites: Vec<(u8, u8, bool)>,
    paths: Vec<(usize, usize, u8, u64)>,
    /// The Wheel unless 0, and the railroad.
    techs: Vec<(u8, bool)>,
    /// For the pairs (0, 1), (0, 2), (1, 2): 0 strangers, 1 met, 2 open borders, 3 war.
    relations: Vec<u8>,
    /// Routes set or cleared afterwards: (site, steps toward the next site, route).
    edits: Vec<(usize, u8, u8)>,
}

fn world() -> impl Strategy<Value = World> {
    (
        proptest::collection::vec((0u8..4, 1u8..3, any::<bool>()), SITES.len()),
        proptest::collection::vec((0..SITES.len(), 0..SITES.len(), 0u8..3, any::<u64>()), 0..16),
        proptest::collection::vec((0u8..4, any::<bool>()), 3),
        proptest::collection::vec(0u8..4, 3),
        proptest::collection::vec((0..SITES.len(), 0u8..6, 0u8..3), 0..6),
    )
        .prop_map(|(sites, paths, techs, relations, edits)| World {
            sites,
            paths,
            techs,
            relations,
            edits,
        })
}

/// The tiles of a walk from `a` toward `b`, a step at a time to the neighbour nearest `b`.
fn walk(g: &Game, a: TileIdx, b: TileIdx) -> Vec<TileIdx> {
    let grid = g.grid();
    let mut out = vec![a];
    let mut cur = a;
    while cur != b && out.len() < 64 {
        let Some(next) = grid.neighbors(cur).min_by_key(|&n| (grid.distance(n, b), n)) else {
            break;
        };
        cur = next;
        out.push(cur);
    }
    out
}

fn route_name(kind: u8) -> Value {
    match kind {
        0 => json!("Road"),
        1 => json!("Railroad"),
        _ => Value::Null,
    }
}

/// Builds the world: every operation in one list, which must apply whole.
fn build(w: &World) -> Game {
    let mut g = arena(Ruleset::shared(), 3);
    // The oracle runs once the world is built, not after every one of its operations.
    g.set_debug_options(DebugOptions::OFF);
    let mut ops = Vec::new();
    for (i, &(who, radius, harbour)) in w.sites.iter().enumerate() {
        if who == 0 {
            continue;
        }
        let (x, y) = SITES[i];
        let mut op = json!({"op": "found_city", "player": who - 1, "x": x, "y": y,
            "claim_radius": radius});
        if harbour && i < 3 {
            op["buildings"] = json!(["Harbor"]);
        }
        ops.push(op);
    }
    for (p, &(wheel, rails)) in w.techs.iter().enumerate() {
        let mut techs = Vec::new();
        if wheel > 0 {
            techs.push("The Wheel");
        }
        if rails {
            techs.push("Railroads");
        }
        if !techs.is_empty() {
            ops.push(json!({"op": "grant_tech", "player": p, "techs": techs}));
        }
    }
    for (k, &(a, b)) in [(0u8, 1u8), (0, 2), (1, 2)].iter().enumerate() {
        match w.relations[k] {
            1 => ops.push(json!({"op": "meet", "a": a, "b": b})),
            2 => ops.push(json!({"op": "set_relation", "a": a, "b": b, "open_borders": true})),
            3 => ops.push(json!({"op": "set_relation", "a": a, "b": b, "state": "war"})),
            _ => {}
        }
    }
    let land = |g: &Game, t: TileIdx| {
        !g.is_water(t) && g.tile(t).is_some_and(|x| !g.rules().terrains()[x.terrain()].impassable)
    };
    for &(a, b, kind, seed) in &w.paths {
        let at = |i: usize| g.grid().idx(SITES[i].0, SITES[i].1).expect("on the map");
        for (step, t) in walk(&g, at(a), at(b)).into_iter().enumerate() {
            // A gap one step in sixteen, and now and then the other route.
            let bits = seed.rotate_left(u32::try_from(step % 64).unwrap_or(0));
            if bits & 15 == 0 || !land(&g, t) {
                continue;
            }
            let route = if bits & 0x70 == 0x70 && kind < 2 { 1 - kind } else { kind };
            let (x, y) = g.xy(t);
            ops.push(json!({"op": "set_tile", "x": x, "y": y, "route": route_name(route)}));
        }
    }
    g.apply_ops(&Value::Array(ops)).unwrap_or_else(|e| panic!("{e:?}"));
    g
}

fn agree(g: &Game) -> Result<(), TestCaseError> {
    for p in [PlayerId(0), PlayerId(1), PlayerId(2)] {
        let naive = connected_cities_naive(g, p);
        prop_assert_eq!(&connected_cities(g, p), &naive, "the floods of player {}", p.0);
        prop_assert_eq!(&query::connectivity(g, p), &naive, "the memo of player {}", p.0);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn connectivity_agrees_with_a_search_from_every_city(w in world()) {
        let mut g = build(&w);
        agree(&g)?;
        for &(site, steps, kind) in &w.edits {
            let (x, y) = SITES[site];
            let from = g.grid().idx(x, y).expect("on the map");
            let (nx, ny) = SITES[(site + 1) % SITES.len()];
            let to = g.grid().idx(nx, ny).expect("on the map");
            let path = walk(&g, from, to);
            let t = path[usize::from(steps).min(path.len() - 1)];
            if g.is_water(t) || g.state().city_at(t).is_some() {
                continue;
            }
            let (x, y) = g.xy(t);
            // A mountain may refuse a route: then nothing changed, and they must agree still.
            let impassable =
                g.tile(t).is_some_and(|x| g.rules().terrains()[x.terrain()].impassable);
            let edit = g.apply_ops(&json!([{"op": "set_tile", "x": x, "y": y, "route": route_name(kind)}]));
            prop_assert!(edit.is_ok() || impassable, "{:?}", edit.err());
            agree(&g)?;
        }
        g.set_debug_options(DebugOptions::ALL);
        prop_assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
        prop_assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    }
}

#[test]
fn a_road_links_a_city_and_a_harbour_links_the_coast() {
    // A hand-made case of each medium, so the property above is known to see links at all.
    let mut g = arena(Ruleset::shared(), 2);
    let road: Vec<Value> =
        (4..8).map(|x| json!({"op": "set_tile", "x": x, "y": 2, "route": "Road"})).collect();
    let mut ops = vec![
        json!({"op": "found_city", "player": 0, "x": 3, "y": 2, "buildings": ["Harbor"]}),
        json!({"op": "found_city", "player": 0, "x": 8, "y": 2}),
        json!({"op": "found_city", "player": 0, "x": 3, "y": 13, "buildings": ["Harbor"]}),
        json!({"op": "found_city", "player": 0, "x": 13, "y": 13}),
        json!({"op": "grant_tech", "player": 0, "tech": "The Wheel"}),
    ];
    ops.extend(road);
    let (out, _) = g.apply_ops(&Value::Array(ops)).unwrap_or_else(|e| panic!("{e:?}"));
    let ids: Vec<CityId> = out[..4].iter().map(founded).collect();
    let links = query::connectivity(&g, ME);
    assert_eq!(links, connected_cities_naive(&g, ME));
    let linked: Vec<CityId> = links.cities.iter().map(|&(c, _)| c).collect();
    assert_eq!(linked, ids[..3], "the road links the second, the sea the third");
    assert!(query::connected_to_capital(&g, ids[1]));
    assert!(!query::connected_to_capital(&g, ids[3]));
    // A gap in the road cuts the second off.
    g.apply_ops(&json!([{"op": "set_tile", "x": 6, "y": 2, "route": null}])).expect("a gap");
    assert!(!query::connected_to_capital(&g, ids[1]));
    assert!(query::connected_to_capital(&g, ids[2]));
    clean(&mut g);
}

// ---- P8 in miniature (gate 5) -------------------------------------------------------------------

/// A game with two civilizations, a city each and a warrior: the first's city works tiles and
/// has a merchant's slot.
fn p8_game() -> (Game, CityId, CityId) {
    let mut g = arena(Ruleset::shared(), 2);
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma", "pop": 4,
             "claim_radius": 2, "buildings": ["Market"]},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Athens", "pop": 2},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 17, "y": 10},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 6},
        ]))
        .expect("the cities");
    (g, founded(&out[0]), founded(&out[1]))
}

/// Fifty reads and refusals: every query of this package, inspections, and calls the engine
/// refuses, from the player whose turn it is and from the other.
fn fifty_reads(g: &mut Game, roma: CityId, athens: CityId) -> usize {
    let mut n = 0;
    let mut count = |ok: bool| {
        assert!(ok, "read {} does not hold", n + 1);
        n += 1;
    };
    for c in [roma, athens] {
        count(query::city_stats(g, c).total[Stat::Production] > 0.0);
        count(query::city_parts(g, c).tiles[Stat::Food] >= 0.0);
        count(cstats::food_to_next_pop(g, c) > 0);
        count(cstats::workable_tiles(g, c).len() < 40);
        count(!query::connected_to_capital(g, c)); // a lone city has no route
    }
    for p in [PlayerId(0), PlayerId(1)] {
        count(query::happiness(g, p).total.abs() < 100);
        count(query::civ_stats(g, p).total[Stat::Gold].is_finite());
        count(query::connectivity(g, p).cities.len() == 1);
    }
    for i in 0..10 {
        let t = TileIdx(24 * 4 + 3 + i);
        count(query::tile_yield(g, t, Some(ME), Some(roma))[Stat::Food] >= 0.0);
        count(query::tile_yield(g, t, None, None)[Stat::Food] >= 0.0);
    }
    for q in [
        json!({"what": "city", "city": roma.get()}),
        json!({"what": "player", "player": 0}),
        json!({"what": "units"}),
        json!({"what": "tile", "x": 5, "y": 5}),
        json!({"what": "game"}),
        json!({"what": "city", "city": athens.get()}),
    ] {
        count(citar_engine::api::inspect::inspect(g, &q).is_ok());
    }
    let (idle, own) = if g.current() == ME { (PlayerId(1), athens) } else { (ME, roma) };
    let refused = [
        (ME, "work_tile", json!({"city_id": roma.get(), "x": 12, "y": 5})),
        (ME, "work_tile", json!({"city_id": athens.get(), "x": 17, "y": 9})),
        (ME, "set_city_focus", json!({"city_id": roma.get(), "focus": "fame"})),
        (ME, "set_specialists", json!({"city_id": roma.get(), "specialists": {"Scientist": 1}})),
        (ME, "set_specialists", json!({"city_id": roma.get(), "specialists": {"Merchant": 9}})),
        // Whoever's turn it is not: their own city, refused as out of turn.
        (idle, "set_city_focus", json!({"city_id": own.get(), "focus": "gold"})),
        (idle, "work_tile", json!({"city_id": own.get(), "x": 17, "y": 11})),
    ];
    for (p, name, args) in refused {
        count(tool(g, p, name, &args).is_err());
    }
    count(g.apply_ops(&json!([{"op": "set_tile", "x": 5, "y": 5, "terrain": "Nowhere"}])).is_err());
    n
}

#[test]
fn reads_and_refusals_between_two_actions_change_nothing() {
    let run = |reads: bool| {
        let (mut g, roma, athens) = p8_game();
        let mut digests = Vec::new();
        let mut outs = Vec::new();
        outs.push(
            tool(
                &mut g,
                ME,
                "set_city_focus",
                &json!({"city_id": roma.get(), "focus": "production"}),
            )
            .expect("a focus"),
        );
        digests.push(g.digest().expect("a digest"));
        if reads {
            assert_eq!(fifty_reads(&mut g, roma, athens), 50);
            assert_eq!(digests.last(), Some(&g.digest().expect("a digest")), "reads changed it");
        }
        outs.push(
            tool(
                &mut g,
                ME,
                "set_specialists",
                &json!({"city_id": roma.get(),
                "specialists": {"Merchant": 1}}),
            )
            .expect("a merchant"),
        );
        digests.push(g.digest().expect("a digest"));
        if reads {
            fifty_reads(&mut g, roma, athens);
        }
        g.end_turn(ME).expect("my turn");
        digests.push(g.digest().expect("a digest"));
        if reads {
            fifty_reads(&mut g, roma, athens);
        }
        g.end_turn(PlayerId(1)).expect("their turn");
        digests.push(g.digest().expect("a digest"));
        outs.push(json!(query::city_stats(&g, roma).total[Stat::Gold]));
        clean(&mut g);
        (digests, outs)
    };
    assert_eq!(run(true), run(false));
}
