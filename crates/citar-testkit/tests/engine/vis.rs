//! What each civilization sees (package 1c-01), on games built by hand with the kitchen-sink
//! ruleset: its two extra types of this subsystem, `No Sight` (the Kitchen Sink Drone sees its
//! own tile alone) and `Invisible to others` (the Kitchen Sink Sub, seen only by a unit that
//! `Can see invisible [Submarine] units`), beside the shipped `Invisible to non-adjacent units`
//! of submarines; every check, the cache oracle among them, clean after each step.

use citar_engine::base::ids::{
    BarbarianLevelId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId, PlayerId, SpeedId,
    TerrainId, TileIdx, UnitId,
};
use citar_engine::base::sets::PlayerVec;
use citar_engine::game::vis::{self, Sight, SourceKey};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::State;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
use citar_engine::state::map::{MapInfo, Tile, Tiles};
use citar_engine::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use citar_testkit::rulesets::kitchen_sink;
use serde_json::json;

const W: u16 = 10;
const H: u16 = 8;
const ME: PlayerId = PlayerId(0);
const THEM: PlayerId = PlayerId(1);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

/// A 10 by 8 game of two majors: coast in the two westmost columns, grassland elsewhere.
fn game(r: &'static Ruleset) -> Game {
    let size = u32::from(W) * u32::from(H);
    let player = |n: u8| {
        let seat = Seat::new(Controller::Bot, SeatOverrides::default(), None);
        Player::new(
            PlayerId(n),
            PlayerKind::Major,
            format!("Civ {n}").into(),
            id::<NationId>(r, "BenchmarkCiv"),
            Rgb::default(),
            seat,
            size,
        )
    };
    let players: PlayerVec<Player> = [player(0), player(1)].into_iter().collect();
    let map = MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let (grass, coast) = (id::<TerrainId>(r, "Grassland"), id::<TerrainId>(r, "Coast"));
    let tiles: Vec<Tile> =
        (0..size).map(|i| Tile::new(if i % u32::from(W) < 2 { coast } else { grass })).collect();
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg = GameConfig::new(
        5,
        src,
        id::<SpeedId>(r, "Standard"),
        id::<DifficultyId>(r, "Prince"),
        EraId(0),
        BarbarianLevelId(1),
        500,
    );
    let st = State::new(cfg, map, Tiles::new(tiles), players).expect("a new state");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    g
}

fn at(x: u32, y: u32) -> TileIdx {
    TileIdx(y * u32::from(W) + x)
}

/// Adds a unit through the scenario operation, and returns its id.
fn add(g: &mut Game, p: PlayerId, unit: &str, t: TileIdx) -> UnitId {
    let (x, y) = g.grid().xy(t);
    let (out, _) = g
        .apply_ops(&json!([{"op": "add_unit", "player": p.0, "unit": unit, "x": x, "y": y}]))
        .unwrap_or_else(|e| panic!("add_unit {unit}: {e}"));
    let n = out[0]["unit_ids"][0].as_u64().expect("a unit id");
    UnitId::new(u32::try_from(n).expect("an id")).expect("an id")
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

#[test]
fn a_unit_with_no_sight_sees_its_own_tile() {
    let r = kitchen_sink();
    let mut g = game(r);
    let t = at(5, 4);
    let drone = add(&mut g, ME, "Kitchen Sink Drone", t);
    clean(&mut g);
    assert_eq!(vis::sight_of(&g, drone), Some(Sight::Blind));
    let seen = g.derived().vis().source(SourceKey::Unit(drone)).map(|s| s.footprint.to_vec());
    assert_eq!(seen, Some(vec![t]));
    let visible: Vec<u32> =
        g.derived().vis().visible(ME).map(|v| v.iter().collect()).unwrap_or_default();
    assert_eq!(visible, [t.0]);
    // A warrior beside it sees the disc round itself.
    let w = add(&mut g, ME, "Warrior", at(6, 4));
    clean(&mut g);
    assert_eq!(vis::sight_of(&g, w), Some(Sight::Walk(2)));
    assert!(g.derived().vis().visible(ME).is_some_and(|v| v.len() > 1));
}

#[test]
fn an_invisible_unit_is_seen_only_by_a_unit_that_can_see_it() {
    let r = kitchen_sink();
    let mut g = game(r);
    let sea = at(1, 4);
    let sub = add(&mut g, THEM, "Kitchen Sink Sub", sea);
    add(&mut g, ME, "Warrior", at(3, 4));
    clean(&mut g);
    assert!(g.derived().vis().sees(ME, sea), "the tile is in sight");
    assert!(!vis::unit_visible_to(&g, ME, sub), "the sub on it is not");
    assert!(vis::unit_visible_to(&g, THEM, sub), "its own owner sees it");
    // Right beside it, still not: only a unit that can see submarines finds it.
    add(&mut g, ME, "Warrior", at(2, 4));
    clean(&mut g);
    assert!(!vis::unit_visible_to(&g, ME, sub));
    add(&mut g, ME, "Destroyer", at(0, 6));
    clean(&mut g);
    assert!(vis::unit_visible_to(&g, ME, sub));
}

#[test]
fn a_submarine_is_seen_from_next_to_it_alone() {
    let r = kitchen_sink();
    let mut g = game(r);
    let sea = at(1, 4);
    let sub = add(&mut g, THEM, "Submarine", sea);
    add(&mut g, ME, "Warrior", at(3, 4));
    clean(&mut g);
    assert!(g.derived().vis().sees(ME, sea) && !vis::unit_visible_to(&g, ME, sub));
    add(&mut g, ME, "Warrior", at(2, 4));
    clean(&mut g);
    assert!(vis::unit_visible_to(&g, ME, sub));
    assert!(g.has_met(ME, THEM), "seeing its tile was enough to meet, as Python's rule has it");
}
