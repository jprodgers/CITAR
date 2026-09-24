//! The civilization-level economy (package 1b-05), on games built by hand:
//! - the extra unit supply types of the kitchen sink (`[n] Unit Supply`, `[n] Unit Supply per
//!   city`, `[n] Unit Supply per [k] population [cities]`) and the route upkeep of
//!   `Costs [n] [stat] per turn when in your territory`;
//! - unit upkeep after the free allowance, growing with the game;
//! - stage E2 writes the gold rate from the civilization's stats and E3 banks it, and bankruptcy
//!   disbands military units, the abroad and the least promoted first, reading the stats afresh
//!   after each; stage E5 runs temporary uniques out;
//! - the scenario operation `remove_units`;
//! - every check, the cache oracle among them, clean after each.

use citar_engine::base::ids::{
    BarbarianLevelId, BaseUnitId, BuildingId, CityId, DifficultyId, EraId, MapSizeId, MapTypeId,
    NationId, PlayerId, PromotionId, SpeedId, TerrainId, TileIdx, UnitId,
};
use citar_engine::base::sets::PlayerVec;
use citar_engine::base::stats::Stat;
use citar_engine::game::{DebugOptions, ErrCode, Game, economy};
use citar_engine::rules::defs::Route;
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::{Cities, City};
use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
use citar_engine::state::map::{MapInfo, RouteBits, Tile, Tiles};
use citar_engine::state::players::{
    Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides, TempUnique,
};
use citar_engine::state::units::{Unit, Units};
use citar_engine::state::{IdCounters, State, TileClaim};
use citar_testkit::rulesets::{KITCHEN_SINK, files_of, kitchen_sink, overlay};
use citar_testkit::script::{map_doc, new_game};
use serde_json::json;

const W: u16 = 12;
const H: u16 = 10;
const ME: PlayerId = PlayerId(0);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

fn cid(n: u32) -> CityId {
    CityId::new(n).unwrap_or(CityId::FIRST)
}

fn uid(n: u32) -> UnitId {
    UnitId::new(n).unwrap_or(UnitId::FIRST)
}

/// What a test game holds.
struct Setup<'a> {
    nation: &'a str,
    controller: Controller,
    /// (tile, population) of each of the first player's cities, which claim their neighbours.
    cities: &'a [(u32, u16)],
    /// (base unit, tile, promotions) of the first player's units.
    units: &'a [(&'a str, u32, &'a [&'a str])],
    /// The first player's treasury and the gold rate its last stage E2 committed.
    gold: f64,
    gold_rate: f64,
    temp: Vec<TempUnique>,
    /// Tiles with a road, and whether it is pillaged.
    roads: &'a [(u32, bool)],
    /// The buildings of the first player's first city.
    buildings: &'a [&'a str],
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Self {
            nation: "BenchmarkCiv",
            controller: Controller::Human,
            cities: &[],
            units: &[],
            gold: 0.0,
            gold_rate: 0.0,
            temp: Vec::new(),
            roads: &[],
            buildings: &[],
        }
    }
}

/// A 12 by 10 grassland game: the player of `s` and a second major with nothing.
fn game(r: &'static Ruleset, s: &Setup<'_>) -> Game {
    let size = u32::from(W) * u32::from(H);
    let player = |n: u8, nation: &str, controller: Controller| {
        let seat = Seat::new(controller, SeatOverrides::default(), None);
        let mut p = Player::new(
            PlayerId(n),
            PlayerKind::Major,
            format!("Civ {n}").into(),
            id::<NationId>(r, nation),
            Rgb::default(),
            seat,
            size,
        );
        if n == 0 {
            p.econ.gold = s.gold;
            p.econ.last_gold_rate = s.gold_rate;
            p.civ.temp_uniques = s.temp.clone();
        }
        p
    };
    let players: PlayerVec<Player> =
        [player(0, s.nation, s.controller), player(1, "BenchmarkCiv", Controller::Bot)]
            .into_iter()
            .collect();
    let map = MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let grass = id::<TerrainId>(r, "Grassland");
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg = GameConfig::new(
        3,
        src,
        id::<SpeedId>(r, "Standard"),
        id::<DifficultyId>(r, "Prince"),
        EraId(0),
        BarbarianLevelId(1),
        500,
    );
    let st = State::new(cfg, map, Tiles::new(vec![Tile::new(grass); size as usize]), players)
        .expect("a new state");
    let mut parts = st.into_parts();
    let mut tiles: Vec<Tile> = parts.tiles.as_slice().to_vec();
    let grid = parts.map.grid().expect("a grid");
    let mut cities = Vec::new();
    for (i, &(t, pop)) in s.cities.iter().enumerate() {
        let c = cid(u32::try_from(i).unwrap_or(0) + 1);
        for n in grid.within(TileIdx(t), 1) {
            tiles[n.0 as usize] = tiles[n.0 as usize].with_claim(TileClaim::city(ME, c));
        }
        let mut city = City::new(c, format!("City {}", c.get()).into(), ME, TileIdx(t), 1);
        city.pop = pop;
        if i == 0 {
            for &b in s.buildings {
                city.buildings.insert(id::<BuildingId>(r, b));
            }
        }
        cities.push(city);
    }
    for &(t, pillaged) in s.roads {
        let bits = RouteBits::EMPTY.with_route(Some(Route::Road)).with_route_pillaged(pillaged);
        tiles[t as usize] = tiles[t as usize].with_route_bits(bits);
    }
    parts.tiles = Tiles::new(tiles);
    if let Some(p) = parts.players.get_mut(ME) {
        p.capital = cities.first().map(City::id);
        p.original_capital = p.capital;
    }
    parts.cities = Cities::from_cities(cities).expect("cities");
    let mut next = 1;
    let mut units = Vec::new();
    for &(base, t, promotions) in s.units {
        let mut u = Unit::new(uid(next), id::<BaseUnitId>(r, base), ME, TileIdx(t), 1);
        for &p in promotions {
            u.promotions.insert(id::<PromotionId>(r, p));
        }
        units.push(u);
        next += 1;
    }
    parts.units = Units::from_units(units, size).expect("units");
    parts.ids = IdCounters::starting_at(next.max(s.cities.len() as u32 + 1));
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    g
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

#[test]
fn the_kitchen_sink_supports_more_units() {
    // `[+2] Unit Supply`, `[+1] Unit Supply per city` and `[+1] Unit Supply per [2] population
    // [in all cities]`: 2, 1 for each of two cities, and 1 for every 2 citizens of each city.
    let r = kitchen_sink();
    let cities = [(22, 3), (27, 4)];
    let plain = game(r, &Setup { cities: &cities, ..Setup::default() });
    let mut sink = game(r, &Setup { nation: "Kitchen Sink", cities: &cities, ..Setup::default() });
    let d = &r.difficulties()[id::<DifficultyId>(r, "Prince")];
    let from_pop = |pop: f64| (pop * r.constants().formulas.unit_supply_per_population) as i32;
    let base = d.unit_supply_base + 2 * d.unit_supply_per_city + from_pop(7.0);
    assert_eq!(economy::unit_supply(&plain, ME), base);
    assert_eq!(economy::unit_supply(&sink, ME), base + 2 + 2 + (3 / 2 + 4 / 2));
    assert_eq!(economy::unit_supply_deficit(&sink, ME), 0);
    clean(&mut sink);
}

#[test]
fn routes_cost_their_upkeep_in_owned_land_alone() {
    // The kitchen sink's road also costs 2 gold in its owner's land: every unpillaged road the
    // player owns but its city's costs 1 + 2.
    let patch = json!({"Road": {"uniques": [
        "Can be built outside your borders",
        "Costs [1] [Gold] per turn",
        "Costs [2] [Gold] per turn when in your territory",
    ]}})
    .to_string();
    let mut patches = KITCHEN_SINK.to_vec();
    patches.push(("ruleset/improvements.json", &patch));
    let files = overlay(&patches).expect("the patches apply");
    let r: &'static Ruleset = Ruleset::leak(&files_of(&files)).expect("the ruleset loads");
    // The city on 22 claims 21, 23, 10, 11, 33, 34; 60 is nobody's.
    let roads = [(22, false), (21, false), (23, false), (11, true), (60, false)];
    let mut g = game(r, &Setup { cities: &[(22, 1)], roads: &roads, ..Setup::default() });
    let gold = economy::transport_upkeep(&g, ME)[citar_engine::base::stats::Stat::Gold];
    assert!((gold - 6.0).abs() < 1e-9, "{gold}");
    assert!(economy::transport_upkeep(&g, PlayerId(1)).is_zero());
    clean(&mut g);
}

#[test]
fn unit_upkeep_starts_after_the_free_units_and_grows_with_the_game() {
    let r = kitchen_sink();
    let warriors: Vec<(&str, u32, &[&str])> =
        (0..6).map(|i| ("Warrior", 40 + i, &[][..])).collect();
    let mut g = game(r, &Setup { units: &warriors, ..Setup::default() });
    // Six units at 1 each, 3 free: 3 to pay, (0.5 * 3 * (1 + x))^(1 + x/3) at progress x.
    let x = 1.0 / 500.0;
    let want = citar_engine::base::num::pow(0.5 * 3.0 * (1.0 + x), 1.0 + x / 3.0) as i32;
    assert_eq!(economy::unit_maintenance(&g, ME), want);
    let few: Vec<(&str, u32, &[&str])> = (0..3).map(|i| ("Warrior", 40 + i, &[][..])).collect();
    assert_eq!(
        economy::unit_maintenance(&game(r, &Setup { units: &few, ..Setup::default() }), ME),
        0
    );
    clean(&mut g);
}

/// The first player's treasury, and the gold rate its last stage E2 wrote.
fn gold(g: &Game) -> (f64, f64) {
    g.state().player(ME).map_or((0.0, 0.0), |p| (p.econ.gold, p.econ.last_gold_rate))
}

#[test]
fn the_turns_gold_is_banked_and_bankruptcy_disbands_units() {
    let r = kitchen_sink();
    // A civilization with nothing earns nothing: stage E2 writes a rate of 0 over the one the
    // last turn left, and E3 banks it.
    let mut g = game(r, &Setup { gold: 10.0, gold_rate: 3.7, ..Setup::default() });
    g.end_turn(ME).expect("my turn");
    assert_eq!(gold(&g), (10.0, 0.0));
    clean(&mut g);

    // A city pays for its buildings: the rate E2 writes is its stats' gold, and E3 banks it,
    // truncated.
    let mut g = game(
        r,
        &Setup {
            gold: 10.0,
            cities: &[(22, 1)],
            buildings: &["Monument", "Temple"],
            ..Setup::default()
        },
    );
    let rate = citar_engine::game::query::civ_stats(&g, ME).total[Stat::Gold];
    assert!(rate < 0.0, "{rate}");
    g.end_turn(ME).expect("my turn");
    assert_eq!(gold(&g), (10.0 + rate.trunc(), rate));
    clean(&mut g);

    // At -200 or below with a negative rate, the military go: those in its own land first, the
    // least promoted of them first, and the Worker stays.
    let units: [(&str, u32, &[&str]); 4] = [
        ("Warrior", 21, &["Shock I"]),
        ("Worker", 23, &[]),
        ("Warrior", 23, &[]),
        ("Archer", 70, &["Shock I"]),
    ];
    let mut g = game(
        r,
        &Setup {
            cities: &[(22, 1)],
            units: &units,
            gold: -250.0,
            buildings: &["Monument", "Granary", "Shrine", "Library", "Temple", "Courthouse"],
            ..Setup::default()
        },
    );
    let batch = g.end_turn(ME).expect("my turn");
    let bankrupt: Vec<&str> = batch
        .events()
        .iter()
        .filter(|e| e.kind.name() == "bankrupt")
        .map(|e| e.text.as_ref())
        .collect();
    assert_eq!(
        bankrupt,
        [
            "Cannot provide unit upkeep for Warrior - unit has been disbanded!",
            "Cannot provide unit upkeep for Warrior - unit has been disbanded!",
            "Cannot provide unit upkeep for Archer - unit has been disbanded!",
        ]
    );
    let left: Vec<UnitId> = g.state().units().of(ME).to_vec();
    assert_eq!(left, [uid(2)], "the Worker");
    let order: Vec<Option<TileIdx>> =
        batch.events().iter().filter(|e| e.kind.name() == "bankrupt").map(|e| e.tile).collect();
    assert_eq!(order, [Some(TileIdx(23)), Some(TileIdx(21)), Some(TileIdx(70))]);
    // Each Warrior disbanded in its own land refunds a twentieth of its gold price
    // (`units.disband_gold`); the Archer abroad refunds nothing.
    let warrior = r.lookup::<citar_engine::base::ids::BaseUnitId>("Warrior").expect("a Warrior");
    let price = citar_engine::game::cities::purchase::base_gold_cost(
        &g,
        ME,
        citar_engine::state::cities::Constructible::Unit(warrior),
        None,
    );
    let refund = 2.0 * (price.trunc() / 20.0).floor();
    assert!(refund > 0.0);
    // Once the last military unit went, the rate read afresh is banked.
    let left_rate = citar_engine::game::query::civ_stats(&g, ME).total[Stat::Gold];
    assert!(left_rate < 0.0);
    assert!((gold(&g).0 - (-250.0 + refund + left_rate.trunc())).abs() < 1e-9, "{:?}", gold(&g));
    clean(&mut g);
}

#[test]
fn temporary_uniques_run_out_at_the_end_of_their_turns() {
    let r = kitchen_sink();
    let t = r.uniques();
    let temp = t.iter().find_map(|(u, _)| t.meta(u).temp_variant).expect("a timed unique");
    let mut g =
        game(r, &Setup { temp: vec![TempUnique { unique: temp, turns: 2 }], ..Setup::default() });
    let held = |g: &Game| g.state().player(ME).map_or(0, |p| p.civ.temp_uniques.len());
    g.end_turn(ME).expect("my turn");
    assert_eq!(held(&g), 1);
    assert_eq!(g.state().player(ME).map(|p| p.civ.temp_uniques[0].turns), Some(1));
    while g.current() != ME {
        g.end_turn(g.current()).expect("a turn");
    }
    g.end_turn(ME).expect("my turn");
    assert_eq!(held(&g), 0);
    clean(&mut g);
}

#[test]
fn remove_units_takes_a_tile_or_one_unit_and_says_which() {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 5,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    let mut g =
        new_game(citar_engine::rules::Ruleset::shared(), cfg.as_object().expect("an object"))
            .unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    let (added, _) = g
        .apply_ops(&json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 4, "y": 4, "count": 2},
            {"op": "add_unit", "player": 1, "unit": "Scout", "x": 4, "y": 4},
            {"op": "add_unit", "player": 1, "unit": "Scout", "x": 6, "y": 4},
        ]))
        .expect("units");
    let ids: Vec<u64> = added
        .iter()
        .flat_map(|v| v["unit_ids"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_u64())
        .collect();
    assert_eq!(ids.len(), 4);
    let (out, _) = g
        .apply_ops(&json!([{"op": "remove_units", "x": 4, "y": 4, "player": 0}]))
        .expect("removed");
    assert_eq!(out, vec![json!({"removed": [ids[0], ids[1]]})]);
    let (out, _) = g.apply_ops(&json!([{"op": "remove_units", "unit": ids[3]}])).expect("removed");
    assert_eq!(out, vec![json!({"removed": [ids[3]]})]);
    let e = g.apply_ops(&json!([{"op": "remove_units", "unit": 999}])).expect_err("no such unit");
    assert_eq!(
        (e.code, e.message.as_str()),
        (ErrCode::NoSuchUnit, "Operation 1 (remove_units): No such unit.")
    );
    let left: Vec<u32> = g.state().units().iter().map(|u| u.id().get()).collect();
    assert!(left.contains(&u32::try_from(ids[2]).unwrap_or(0)));
    assert_eq!(g.state().units().len(), left.len());
    clean(&mut g);
}
