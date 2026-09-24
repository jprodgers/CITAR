//! Incremental visibility (package 1c-01): sight, line of sight, explored tiles, memory, first
//! contact in both directions, city-states that never meet, natural wonders, the enemy a move
//! spots, spies and allies; and the gate-2 property: after random writes, what each civilization
//! sees and who has met whom equal a rebuild from scratch by Python's rule.

use std::collections::BTreeSet;

use proptest::prelude::*;

use super::los::Heights;
use super::sight::{enemy_spotted, has_sight, sight_of, unit_visible_to};
use super::visibility::Sight;
use crate::base::ids::{
    BarbarianLevelId, BaseUnitId, CityId, DifficultyId, EraId, FeatureId, MapSizeId, MapTypeId,
    NationId, PlayerId, SpeedId, TerrainId, TileIdx, UnitId,
};
use crate::base::sets::{FeatureSet, PlayerVec};
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::PlayerTouch;
use crate::rules::Ruleset;
use crate::rules::defs::SpyAction;
use crate::state::chronicle::{Chronicle, EngineEvent};
use crate::state::config::{GameConfig, MapEdges, MapSource};
use crate::state::map::{MapInfo, Tile, Tiles};
use crate::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides, Spy};
use crate::state::{State, TileClaim};

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const GENEVA: PlayerId = PlayerId(2);

fn r() -> &'static Ruleset {
    Ruleset::shared()
}

fn terrain(name: &str) -> TerrainId {
    r().lookup::<TerrainId>(name).expect("a terrain of the ruleset")
}

fn feature(name: &str) -> FeatureId {
    let t = terrain(name);
    r().derived().features.iter().find(|(_, x)| **x == t).map(|(f, _)| f).expect("a feature")
}

/// A tile of the 10-wide test map by its offset coordinates.
fn at(x: u32, y: u32) -> TileIdx {
    TileIdx(y * 10 + x)
}

/// A tile of the 12-wide crowded map.
fn at12(x: u32, y: u32) -> TileIdx {
    TileIdx(y * 12 + x)
}

fn sees(g: &Game, p: PlayerId) -> BTreeSet<TileIdx> {
    g.derived().vis().visible(p).map(|v| v.iter().map(TileIdx).collect()).unwrap_or_default()
}

fn events(g: &Game, kind: EngineEvent) -> Vec<String> {
    g.chronicle()
        .events()
        .iter()
        .filter(|e| e.kind == crate::state::chronicle::EventType::Engine(kind))
        .map(|e| e.text.to_string())
        .collect()
}

fn hill(g: &mut Game, t: TileIdx) {
    let mut f = FeatureSet::EMPTY;
    f.insert(feature("Hill"));
    g.set_features(t, f).expect("a tile");
}

/// Gives every living major that holds cities and has no capital its first city, as the conquest
/// rules would.
fn fix_capitals(g: &mut Game) {
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    for p in majors {
        let first = g.state().cities().of(p).first().copied();
        let has = g
            .player(p)
            .and_then(|x| x.capital)
            .is_some_and(|c| g.state().cities().of(p).contains(&c));
        if !has && let Some(pl) = g.player_mut(p, PlayerTouch::CAPITAL) {
            pl.capital = first;
        }
    }
}

fn clean(g: &mut Game) {
    g.settle();
    assert_eq!(g.take_violations(), []);
    assert_eq!(super::verify(g), Vec::<String>::new());
    assert!(g.pending.is_empty() && g.fx.is_empty());
}

#[test]
fn a_new_game_sees_nothing_until_a_unit_or_city_looks() {
    let mut g = testing::duel();
    assert!(g.derived().vis().sources().next().is_none());
    let t = at(4, 4);
    let u = testing::unit(&mut g, ROME, "Warrior", t);
    clean(&mut g);
    assert_eq!(sight_of(&g, u), Some(Sight::Walk(2)));
    let disc: BTreeSet<TileIdx> = g.grid().within(t, 2).into_iter().collect();
    assert_eq!(sees(&g, ROME), disc, "flat land: the whole disc");
    assert!(sees(&g, GREECE).is_empty());
    let explored: BTreeSet<TileIdx> =
        g.player(ROME).map(|p| p.explored.iter().map(TileIdx).collect()).unwrap_or_default();
    assert_eq!(explored, disc);
    // A city sees its tiles and the ring round them.
    let c = testing::city(&mut g, GREECE, at(8, 1), "Athens");
    clean(&mut g);
    let ring: BTreeSet<TileIdx> = g.grid().within(at(8, 1), 1).into_iter().collect();
    assert_eq!(sees(&g, GREECE), ring);
    assert!(g.city(c).is_some());
}

#[test]
fn a_tile_out_of_sight_is_remembered_as_it_was() {
    let mut g = testing::duel();
    let u = testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    let athens = testing::city(&mut g, GREECE, at(4, 4), "Athens");
    clean(&mut g);
    assert!(g.has_met(ROME, GREECE), "Rome sees Athens");
    g.relocate_unit(u, at(0, 0)).expect("a tile");
    clean(&mut g);
    let city_tile = at(4, 4);
    assert!(!g.derived().vis().sees(ROME, city_tile));
    let memory = &g.player(ROME).and_then(|p| p.major.as_deref()).expect("a major").memory;
    assert_eq!(memory.get(city_tile).owner(), Some(GREECE));
    assert_eq!(memory.city(city_tile).map(|c| (&*c.name, c.owner)), Some(("Athens", GREECE)));
    assert!(g.city(athens).is_some());
    // City-states keep no memory; they still explore.
    let scout = testing::unit(&mut g, GENEVA, "Warrior", at(9, 7));
    clean(&mut g);
    g.relocate_unit(scout, at(9, 0)).expect("a tile");
    clean(&mut g);
    let geneva = g.player(GENEVA).expect("Geneva");
    assert!(geneva.major.is_none() && geneva.explored.contains(at(9, 7).0));
}

#[test]
fn first_contact_goes_both_ways() {
    // Rome sees the hill Greece stands on three tiles off, while Greece, looking down from it,
    // does not see Rome's flat tile: they meet all the same.
    for (low, high) in [(ROME, GREECE), (GREECE, ROME)] {
        let mut g = testing::duel();
        let h = at(6, 4);
        hill(&mut g, h);
        testing::unit(&mut g, high, "Warrior", h);
        testing::unit(&mut g, low, "Warrior", at(3, 4));
        clean(&mut g);
        assert!(g.derived().vis().sees(low, h));
        assert!(!g.derived().vis().sees(high, at(3, 4)));
        assert!(g.has_met(ROME, GREECE));
        assert_eq!(events(&g, EngineEvent::FirstContact), ["Rome and Greece have made contact."]);
    }
}

#[test]
fn borders_growing_into_sight_meet_their_owner() {
    let mut g = testing::duel();
    testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    testing::unit(&mut g, GREECE, "Warrior", at(9, 7));
    clean(&mut g);
    assert!(!g.has_met(ROME, GREECE));
    // Out of sight: nothing.
    g.set_tile_owner(at(6, 4), TileClaim { owner: Some(GREECE), city: None }).expect("a tile");
    clean(&mut g);
    assert!(!g.has_met(ROME, GREECE));
    g.set_tile_owner(at(4, 4), TileClaim { owner: Some(GREECE), city: None }).expect("a tile");
    clean(&mut g);
    assert!(g.has_met(ROME, GREECE));
}

/// Two majors (America and Polynesia), two city-states and the barbarians on a 12x10 grassland
/// map.
fn crowded() -> Game {
    let rr = r();
    let (w, h) = (12u16, 10u16);
    let tiles = u32::from(w) * u32::from(h);
    let grass = terrain("Grassland");
    let player = |id: u8, kind: PlayerKind, name: &str, nation: &str| {
        let controller = match kind {
            PlayerKind::Major => Controller::Bot,
            PlayerKind::CityState => Controller::Minor,
            PlayerKind::Barbarian => Controller::Barbarian,
        };
        let nation = rr.lookup::<NationId>(nation).unwrap_or(NationId(0));
        let seat = Seat::new(controller, SeatOverrides::default(), None);
        Player::new(PlayerId(id), kind, name.into(), nation, Rgb::default(), seat, tiles)
    };
    let players: PlayerVec<Player> = [
        // America's land soldiers and Polynesia's embarked units see one tile further.
        player(0, PlayerKind::Major, "Rome", "America"),
        player(1, PlayerKind::Major, "Greece", "Polynesia"),
        player(2, PlayerKind::CityState, "Geneva", "Geneva"),
        player(3, PlayerKind::CityState, "Kabul", "Kabul"),
        player(4, PlayerKind::Barbarian, "Barbarians", "Barbarians"),
    ]
    .into_iter()
    .collect();
    let map = MapInfo { width: w, height: h, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let speed = rr.lookup::<SpeedId>("Standard").unwrap_or(SpeedId(0));
    let difficulty = rr.lookup::<DifficultyId>("Prince").unwrap_or(DifficultyId(0));
    let cfg = GameConfig::new(7, src, speed, difficulty, EraId(0), BarbarianLevelId(1), 500);
    let st = State::new(cfg, map, Tiles::new(vec![Tile::new(grass); tiles as usize]), players)
        .expect("a state");
    Game::from_state(rr, st, Chronicle::new()).expect("a sound state")
}

#[test]
fn city_states_never_meet_each_other() {
    let mut g = crowded();
    let (geneva, kabul) = (PlayerId(2), PlayerId(3));
    testing::unit(&mut g, geneva, "Warrior", at12(5, 5));
    testing::unit(&mut g, kabul, "Warrior", at12(6, 5));
    clean(&mut g);
    assert!(g.derived().vis().sees(geneva, at12(6, 5)));
    assert!(!g.has_met(geneva, kabul));
    testing::unit(&mut g, ROME, "Warrior", at12(5, 7));
    clean(&mut g);
    assert!(g.has_met(ROME, geneva) && g.has_met(ROME, kabul));
    assert!(!g.has_met(geneva, kabul));
    // The barbarians see nothing and meet nobody.
    testing::unit(&mut g, PlayerId(4), "Warrior", at12(5, 6));
    clean(&mut g);
    assert!(sees(&g, PlayerId(4)).is_empty());
    assert!(!g.has_met(ROME, PlayerId(4)));
}

#[test]
fn natural_wonders_pay_the_first_to_find_them() {
    let mut g = testing::duel();
    let rome_gold = g.player(ROME).map_or(0.0, |p| p.econ.gold);
    testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    clean(&mut g);
    let dorado = terrain("El Dorado");
    let t = at(3, 3);
    g.set_wonder(t, Some(dorado)).expect("a tile");
    clean(&mut g);
    let rome = g.player(ROME).expect("Rome");
    assert!(rome.civ.natural_wonders.contains(dorado));
    assert!((rome.econ.gold - rome_gold - 500.0).abs() < 1e-9);
    assert_eq!(
        events(&g, EngineEvent::NaturalWonder),
        ["We have discovered El Dorado! (+500 gold)"]
    );
    // Greece finds it second: no bonus.
    let greek_gold = g.player(GREECE).map_or(0.0, |p| p.econ.gold);
    testing::unit(&mut g, GREECE, "Warrior", at(4, 1));
    clean(&mut g);
    let greece = g.player(GREECE).expect("Greece");
    assert!(greece.civ.natural_wonders.contains(dorado));
    assert!((greece.econ.gold - greek_gold).abs() < 1e-9);
    assert_eq!(
        events(&g, EngineEvent::NaturalWonder),
        ["We have discovered El Dorado! (+500 gold)", "We have discovered El Dorado!"]
    );
    // City-states discover nothing.
    testing::unit(&mut g, GENEVA, "Warrior", at(3, 4));
    clean(&mut g);
    assert!(g.player(GENEVA).is_some_and(|p| p.civ.natural_wonders.is_empty()));
}

#[test]
fn a_move_spots_the_enemies_that_come_into_view() {
    let mut g = testing::duel();
    let u = testing::unit(&mut g, ROME, "Warrior", at(1, 4));
    testing::unit(&mut g, GREECE, "Warrior", at(5, 4));
    testing::unit(&mut g, GENEVA, "Worker", at(4, 3));
    clean(&mut g);
    g.update_relation(ROME, GREECE, |x| x.war = true).expect("a pair");
    g.update_relation(ROME, GENEVA, |x| x.war = true).expect("a pair");
    clean(&mut g);
    let _ = g.take_newly_seen(ROME);
    // One step east: the warrior is three away still, its tile not yet in view.
    g.relocate_unit(u, at(2, 4)).expect("a tile");
    g.settle_sight();
    let seen = g.take_newly_seen(ROME);
    assert!(!seen.is_empty() && !enemy_spotted(&g, ROME, &seen));
    // One more: the Greek warrior comes into view; the Genevan worker is no soldier.
    g.relocate_unit(u, at(3, 4)).expect("a tile");
    g.settle_sight();
    let seen = g.take_newly_seen(ROME);
    assert!(seen.contains(&at(5, 4)) && seen.contains(&at(4, 3)), "{seen:?}");
    assert!(enemy_spotted(&g, ROME, &seen));
    assert!(!enemy_spotted(&g, ROME, &[at(4, 3)]));
    assert!(!enemy_spotted(&g, GREECE, &[at(3, 4)]) || g.at_war(GREECE, ROME));
    clean(&mut g);
    assert!(g.take_newly_seen(ROME).is_empty(), "a settle point forgets them");
}

#[test]
fn a_forest_hides_what_lies_behind_it_and_a_hill_sees_over() {
    let mut g = testing::duel();
    let u = testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    clean(&mut g);
    let behind = at(4, 4);
    assert!(g.derived().vis().sees(ROME, behind));
    let mut f = FeatureSet::EMPTY;
    f.insert(feature("Forest"));
    g.set_features(at(3, 4), f).expect("a tile");
    clean(&mut g);
    assert!(!g.derived().vis().sees(ROME, behind), "the forest blocks it");
    assert!(g.derived().vis().sees(ROME, at(3, 4)), "the forest itself is seen");
    hill(&mut g, at(2, 4));
    clean(&mut g);
    assert!(g.derived().vis().sees(ROME, behind), "from the hill, over the forest");
    assert_eq!(sight_of(&g, u), Some(Sight::Walk(2)));
}

#[test]
fn spies_and_allies_see_their_cities() {
    let mut g = testing::duel();
    let athens = testing::city(&mut g, GREECE, at(8, 6), "Athens");
    let bern = testing::city(&mut g, GENEVA, at(2, 1), "Bern");
    clean(&mut g);
    assert!(sees(&g, ROME).is_empty());
    if let Some(m) = g.player_mut(ROME, PlayerTouch::SPIES).and_then(|p| p.major.as_deref_mut()) {
        m.spies.push(Spy {
            name: "Mata".into(),
            rank: 1,
            city: Some(athens),
            action: SpyAction::ObservingCity,
            turns: 0,
            progress: 0,
        });
    }
    clean(&mut g);
    let ring: BTreeSet<TileIdx> = g.grid().within(at(8, 6), 1).into_iter().collect();
    assert_eq!(sees(&g, ROME), ring);
    assert!(g.has_met(ROME, GREECE));
    // Espionage off: spies see nothing.
    g.edit_config(|c| c.espionage = false);
    clean(&mut g);
    assert!(sees(&g, ROME).is_empty());
    g.set_ally(GENEVA, Some(ROME)).expect("a city-state");
    clean(&mut g);
    assert_eq!(sees(&g, ROME), [at(2, 1)].into_iter().collect(), "Bern's own tile, no ring");
    assert!(g.city(bern).is_some());
}

#[test]
fn units_are_made_out_where_they_are_seen() {
    // `No Sight` and `Invisible to others` are the kitchen sink's, tested in testkit
    // (`tests/engine/vis.rs`).
    let mut g = testing::duel();
    let u = testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    let w = testing::unit(&mut g, GREECE, "Warrior", at(3, 4));
    clean(&mut g);
    assert!(unit_visible_to(&g, ROME, w) && unit_visible_to(&g, ROME, u));
    assert!(!unit_visible_to(&g, GENEVA, w), "Geneva sees nothing");
    assert!(has_sight(&g, ROME) && !has_sight(&g, PlayerId(3)));
}

#[test]
fn a_revived_civilization_is_met_again_by_those_who_see_it() {
    let mut g = testing::duel();
    let scout = testing::unit(&mut g, GREECE, "Warrior", at(8, 7));
    testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    let roma = testing::city(&mut g, ROME, at(3, 4), "Roma");
    clean(&mut g);
    g.despawn_unit(scout).expect("a unit");
    g.kill_player(GREECE).expect("no cities");
    clean(&mut g);
    assert!(sees(&g, GREECE).is_empty());
    g.update_relation(ROME, GREECE, |x| x.met = false).expect("a pair");
    clean(&mut g);
    assert!(!g.has_met(ROME, GREECE), "the dead meet nobody");
    // Liberated: Roma goes to Greece, which comes back, in Rome's sight.
    g.transfer_city(roma, GREECE).expect("a city");
    g.revive_player(GREECE).expect("a player");
    fix_capitals(&mut g);
    clean(&mut g);
    assert!(g.has_met(ROME, GREECE));
    assert!(g.derived().vis().sees(GREECE, at(3, 4)));
}

#[test]
fn a_loaded_game_sees_what_it_saw_and_changes_nothing() {
    let mut g = testing::duel();
    testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    testing::city(&mut g, GREECE, at(7, 4), "Athens");
    clean(&mut g);
    let snap = g.snapshot();
    let json = snap.to_json().expect("a save");
    let (loaded, _) = Game::load(r(), &json, &mut core::iter::empty()).expect("it loads");
    assert_eq!(loaded.derived().vis().differences(g.derived().vis()), Vec::<String>::new());
    assert_eq!(loaded.digest().ok(), g.digest().ok());
    assert!(loaded.pending.is_empty());
}

#[test]
fn heights_come_from_the_terrains() {
    let mut g = testing::duel();
    let t = at(5, 5);
    hill(&mut g, t);
    let h = Heights::new(r(), g.state().tiles());
    assert_eq!((h.stand(t), h.block(t)), (1, 1));
    let mut f = FeatureSet::EMPTY;
    f.insert(feature("Forest"));
    g.set_features(t, f).expect("a tile");
    let h = Heights::new(r(), g.state().tiles());
    assert_eq!((h.stand(t), h.block(t)), (0, 1));
    g.set_terrain(t, terrain("Mountain")).expect("a tile");
    let h = Heights::new(r(), g.state().tiles());
    assert_eq!((h.stand(t), h.block(t)), (2, 3));
}

// ---- The property (gate 2) ----------------------------------------------------------------------

/// What each civilization sees by Python's rule, from nothing (`visibility.compute_visible`),
/// with the elevation walk written as Python wrote it.
fn python_visible(g: &Game, p: PlayerId) -> BTreeSet<TileIdx> {
    let mut vis = BTreeSet::new();
    if !has_sight(g, p) {
        return vis;
    }
    let grid = g.grid();
    let h = Heights::new(g.rules(), g.state().tiles());
    for c in g.player_cities(p) {
        for t in crate::game::economy::city_tiles(g, c.id()) {
            vis.insert(t);
            vis.extend(grid.neighbors(t));
        }
    }
    for u in g.player_units(p) {
        let at = u.tile();
        match sight_of(g, u.id()) {
            Some(Sight::Blind) => {
                vis.insert(at);
            }
            Some(Sight::Clear(r)) => vis.extend(grid.within(at, r)),
            Some(Sight::Walk(r)) => {
                let a = h.stand(at);
                let mut seen: std::collections::BTreeMap<TileIdx, i32> = [(at, a)].into();
                vis.insert(at);
                for i in 1..=r + 1 {
                    let mut layer = Vec::new();
                    for c in grid.within(at, i).into_iter().filter(|&c| grid.distance(at, c) == i) {
                        let ch = h.block(c);
                        if i == r + 1 && ch <= a {
                            continue;
                        }
                        let prev: Vec<i32> = grid
                            .neighbors(c)
                            .filter(|n| seen.contains_key(n) && grid.distance(at, *n) == i - 1)
                            .map(|n| seen[&n])
                            .collect();
                        let Some(&b) = prev.iter().min() else { continue };
                        layer.push((c, ch.max(b)));
                        if a >= b || ch > b {
                            vis.insert(c);
                        }
                    }
                    seen.extend(layer);
                }
            }
            None => {}
        }
    }
    for q in g.city_states(true) {
        let ally = q.city_state.as_deref().and_then(crate::state::players::CityStateData::ally);
        let mine = g
            .player(p)
            .and_then(|x| x.city_state.as_deref())
            .and_then(crate::state::players::CityStateData::ally);
        if ally == Some(p) || (g.is_city_state(p) && mine == Some(q.id())) {
            for c in g.player_cities(q.id()) {
                vis.extend(crate::game::economy::city_tiles(g, c.id()));
            }
        }
    }
    if g.player(p).is_some_and(Player::is_major) && g.espionage_enabled() {
        for s in g.player(p).and_then(|x| x.major.as_deref()).map_or(&[][..], |m| &m.spies) {
            if let Some(c) = s.city.and_then(|c| g.city(c))
                && s.action.is_set_up()
            {
                vis.extend(grid.within(c.tile(), 1));
            }
        }
    }
    vis
}

#[derive(Clone, Debug)]
enum Op {
    Spawn {
        p: u8,
        base: u8,
        t: u16,
    },
    Move {
        u: u8,
        t: u16,
    },
    Remove {
        u: u8,
    },
    Claim {
        t: u16,
        p: Option<u8>,
    },
    Found {
        p: u8,
        t: u16,
    },
    Capture {
        c: u8,
        p: u8,
    },
    Raze {
        c: u8,
    },
    Terrain {
        t: u16,
        kind: u8,
    },
    Ally {
        cs: u8,
        p: Option<u8>,
    },
    Kill {
        p: u8,
    },
    Revive {
        p: u8,
    },
    Unmeet {
        a: u8,
        b: u8,
    },
    Spy {
        p: u8,
        c: u8,
        set_up: bool,
    },
    Promote {
        u: u8,
    },
    /// Builds or pulls down the Great Lighthouse, whose military ships see further.
    Lighthouse {
        c: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let t = 0u16..120;
    prop_oneof![
        4 => (0u8..5, 0u8..4, t.clone()).prop_map(|(p, base, t)| Op::Spawn { p, base, t }),
        6 => (any::<u8>(), t.clone()).prop_map(|(u, t)| Op::Move { u, t }),
        1 => any::<u8>().prop_map(|u| Op::Remove { u }),
        3 => (t.clone(), proptest::option::of(0u8..5)).prop_map(|(t, p)| Op::Claim { t, p }),
        2 => (0u8..4, t.clone()).prop_map(|(p, t)| Op::Found { p, t }),
        1 => (any::<u8>(), 0u8..4).prop_map(|(c, p)| Op::Capture { c, p }),
        1 => any::<u8>().prop_map(|c| Op::Raze { c }),
        2 => (t.clone(), 0u8..5).prop_map(|(t, kind)| Op::Terrain { t, kind }),
        1 => (2u8..4, proptest::option::of(0u8..4)).prop_map(|(cs, p)| Op::Ally { cs, p }),
        // Player 0 plays the current turn, and stays alive.
        1 => (1u8..4).prop_map(|p| Op::Kill { p }),
        1 => (1u8..4).prop_map(|p| Op::Revive { p }),
        1 => (0u8..4, 0u8..4).prop_map(|(a, b)| Op::Unmeet { a, b }),
        1 => (0u8..2, any::<u8>(), any::<bool>())
            .prop_map(|(p, c, set_up)| Op::Spy { p, c, set_up }),
        1 => any::<u8>().prop_map(|u| Op::Promote { u }),
        1 => any::<u8>().prop_map(|c| Op::Lighthouse { c }),
    ]
}

fn nth<T: Copy>(v: &[T], i: u8) -> Option<T> {
    (!v.is_empty()).then(|| v[usize::from(i) % v.len()])
}

fn apply(g: &mut Game, op: &Op) {
    let units: Vec<UnitId> = g.state().units().iter().map(crate::state::units::Unit::id).collect();
    let cities: Vec<CityId> =
        g.state().cities().iter().map(crate::state::cities::City::id).collect();
    let tile = |t: u16| TileIdx(u32::from(t));
    match *op {
        Op::Spawn { p, base, t } => {
            if !g.player(PlayerId(p)).is_some_and(Player::alive) {
                return;
            }
            let name = ["Warrior", "Scout", "Worker", "Trireme"][usize::from(base)];
            testing::unit(g, PlayerId(p), name, tile(t));
        }
        Op::Move { u, t } => {
            if let Some(u) = nth(&units, u) {
                let _moved = g.relocate_unit(u, tile(t));
            }
        }
        Op::Remove { u } => {
            if let Some(u) = nth(&units, u) {
                let _gone = g.despawn_unit(u);
            }
        }
        Op::Claim { t, p } => {
            if g.tile(tile(t)).and_then(Tile::city).is_none() {
                let claim = TileClaim { owner: p.map(PlayerId), city: None };
                let _claimed = g.set_tile_owner(tile(t), claim);
            }
        }
        Op::Found { p, t } => {
            let (p, t) = (PlayerId(p), tile(t));
            if !g.player(p).is_some_and(Player::alive) || g.tile(t).and_then(Tile::city).is_some() {
                return;
            }
            let c = testing::city(g, p, t, "Town");
            for n in g.grid().within(t, 1) {
                if g.tile(n).and_then(Tile::city).is_none() {
                    let _claimed = g.set_tile_owner(n, TileClaim::city(p, c));
                }
            }
        }
        Op::Capture { c, p } => {
            if let Some(c) = nth(&cities, c) {
                let p = PlayerId(p);
                let _taken = g.transfer_city(c, p);
                if !g.player(p).is_some_and(Player::alive) {
                    let _back = g.revive_player(p);
                }
            }
        }
        Op::Raze { c } => {
            if let Some(c) = nth(&cities, c) {
                let owned: Vec<TileIdx> = g
                    .state()
                    .tiles()
                    .iter()
                    .filter(|(_, x)| x.city() == Some(c))
                    .map(|(i, _)| i)
                    .collect();
                for t in owned {
                    let _released = g.set_tile_owner(t, TileClaim::NONE);
                }
                let owner = g.city(c).map(crate::state::cities::City::owner);
                let _razed = g.remove_city(c);
                if let Some(o) = owner
                    && let Some(pl) = g.player_mut(o, PlayerTouch::CAPITAL)
                {
                    if pl.capital == Some(c) {
                        pl.capital = None;
                    }
                    if pl.original_capital == Some(c) {
                        pl.original_capital = None;
                    }
                }
            }
        }
        Op::Terrain { t, kind } => {
            let t = tile(t);
            match kind {
                0 => {
                    let _flat = g.set_features(t, FeatureSet::EMPTY);
                    let _grass = g.set_terrain(t, terrain("Grassland"));
                }
                1 => hill(g, t),
                2 => {
                    let mut f = FeatureSet::EMPTY;
                    f.insert(feature("Forest"));
                    let _forest = g.set_features(t, f);
                }
                3 => {
                    let _mountain = g.set_terrain(t, terrain("Mountain"));
                }
                _ => {
                    // Water: a land unit on it is embarked.
                    let _flat = g.set_features(t, FeatureSet::EMPTY);
                    let _coast = g.set_terrain(t, terrain("Coast"));
                }
            }
        }
        Op::Ally { cs, p } => {
            let cs = PlayerId(cs);
            if g.player(cs).is_some_and(Player::alive) {
                let _allied = g.set_ally(cs, p.map(PlayerId).filter(|&q| q != cs));
            }
        }
        Op::Kill { p } => {
            let p = PlayerId(p);
            if g.player(p).is_some_and(Player::alive) && g.state().cities().of(p).is_empty() {
                let _dead = g.kill_player(p);
            }
        }
        Op::Revive { p } => {
            let p = PlayerId(p);
            if g.player(p).is_some_and(|x| !x.alive()) {
                let _back = g.revive_player(p);
            }
        }
        Op::Unmeet { a, b } => {
            if a != b {
                let _forgot = g.update_relation(PlayerId(a), PlayerId(b), |r| r.met = false);
            }
        }
        Op::Spy { p, c, set_up } => {
            let city = nth(&cities, c);
            if let Some(m) =
                g.player_mut(PlayerId(p), PlayerTouch::SPIES).and_then(|x| x.major.as_deref_mut())
            {
                if m.spies.len() >= 3 {
                    m.spies.remove(0);
                }
                m.spies.push(Spy {
                    name: "Agent".into(),
                    rank: 1,
                    city,
                    action: if set_up { SpyAction::ObservingCity } else { SpyAction::Moving },
                    turns: 0,
                    progress: 0,
                });
            }
        }
        Op::Lighthouse { c } => {
            if let Some(c) = nth(&cities, c) {
                let b = g.rules().lookup::<crate::base::ids::BuildingId>("The Great Lighthouse");
                if let (Some(b), Some(city)) =
                    (b, g.city_mut(c, crate::game::derive::rev::CityTouch::BUILDINGS))
                    && !city.buildings.insert(b)
                {
                    city.buildings.remove(b);
                }
            }
        }
        Op::Promote { u } => {
            if let Some(u) = nth(&units, u) {
                let promo = g.rules().lookup::<crate::base::ids::PromotionId>("Sentry");
                if let (Some(p), Some(x)) =
                    (promo, g.unit_mut(u, crate::game::derive::rev::UnitTouch::CORE))
                {
                    x.promotions.insert(p);
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Gate 2: after random moves, border growth and tile purchases, cities founded, captured and
    /// razed, terrain that blocks sight, alliances, deaths and revivals, forgotten meetings and
    /// spies, the incremental sight equals Python's from nothing, and the met sets hold its rule.
    #[test]
    fn incremental_sight_is_a_rebuild_by_pythons_rule(
        start in proptest::collection::vec((0u8..4, 0u8..3, 0u16..120), 6..14),
        towns in proptest::collection::vec((0u8..4, 0u16..120), 1..5),
        ground in proptest::collection::vec((0u16..120, 0u8..4), 0..12),
        ops in proptest::collection::vec(op(), 10..50),
    ) {
        let mut g = crowded();
        let setup = ground
            .iter()
            .map(|&(t, kind)| Op::Terrain { t, kind })
            .chain(towns.iter().map(|&(p, t)| Op::Found { p, t }))
            .chain(start.iter().map(|&(p, base, t)| Op::Spawn { p, base, t }));
        for o in setup {
            apply(&mut g, &o);
        }
        fix_capitals(&mut g);
        g.settle();
        prop_assert!(g.take_violations().is_empty());
        for (i, o) in ops.iter().enumerate() {
            apply(&mut g, o);
            fix_capitals(&mut g);
            g.settle();
            let bad = g.take_violations();
            prop_assert!(bad.is_empty(), "step {i} {o:?}: {bad:?}");
            let diff = super::verify(&g);
            prop_assert!(diff.is_empty(), "step {i} {o:?}: {diff:?}");
            for p in g.state().players().ids() {
                let want = python_visible(&g, p);
                prop_assert_eq!(sees(&g, p), want, "step {} {:?}: player {}", i, o, p.0);
            }
        }
    }
}

#[test]
fn a_units_base_and_promotions_decide_its_sight() {
    let mut g = testing::duel();
    let base = r().lookup::<BaseUnitId>("Scout").expect("a scout");
    let u = testing::unit(&mut g, ROME, "Scout", at(5, 4));
    clean(&mut g);
    assert!(g.unit(u).is_some_and(|x| x.base == base));
    let s = sight_of(&g, u);
    assert!(matches!(s, Some(Sight::Walk(r)) if r >= 2), "{s:?}");
}

#[test]
fn revealed_tiles_are_explored_and_remembered_out_of_sight() {
    let mut g = testing::duel();
    testing::unit(&mut g, ROME, "Warrior", at(2, 4));
    let athens = testing::city(&mut g, GREECE, at(8, 4), "Athens");
    clean(&mut g);
    let before = g.player(ROME).map_or(0, |p| p.explored.len());
    // A seen tile, the city out of sight, and one twice.
    let n = g.reveal_tiles(ROME, &[at(2, 4), at(8, 4), at(8, 4), at(9, 4)]);
    assert_eq!(n, 2, "Athens' tile and the one beside it are new; the seen one was explored");
    let rome = g.player(ROME).expect("Rome");
    assert_eq!(rome.explored.len(), before + 2);
    let memory = &rome.major.as_deref().expect("a major").memory;
    assert_eq!(memory.city(at(8, 4)).map(|c| &*c.name), Some("Athens"));
    assert!(!memory.get(at(2, 4)).is_remembered(), "what it sees it does not remember");
    assert!(g.city(athens).is_some());
    clean(&mut g);
}
