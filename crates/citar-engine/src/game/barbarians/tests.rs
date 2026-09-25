//! The barbarians on the unit tests' small game: Rome and Greece, Geneva, and the barbarians on a
//! 10x8 grassland map (normal barbarians, aggression 50). Every test ends with the checks clean.

#![allow(
    clippy::float_cmp,
    reason = "the numbers the rules give here are exact: whole, halves and tenths"
)]

use super::*;
use crate::base::ids::{BuildingId, TechId};
use crate::game::core::testing;
use crate::state::TurnClock;

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const BARBS: PlayerId = PlayerId(3);

fn set_turn(g: &mut Game, turn: i32) {
    let clock = TurnClock { turn, ..*g.state().clock() };
    g.set_clock(clock);
}

fn clean(g: &mut Game) {
    g.settle();
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    let broken = g.check_invariants();
    assert!(broken.is_empty(), "{broken:?}");
    let stale = g.verify_caches();
    assert!(stale.is_empty(), "{stale:?}");
}

/// Everyone but the barbarians knows `tech`.
fn everyone_knows(g: &mut Game, tech: &str) {
    let t = g.rules().lookup::<TechId>(tech).expect("a tech");
    for p in [ROME, GREECE, PlayerId(2)] {
        crate::game::research::add_tech_silently(g, p, &[t]);
    }
}

fn unit_type(g: &Game, name: &str) -> BaseUnitId {
    g.rules().lookup::<BaseUnitId>(name).expect("a unit")
}

#[test]
fn aggression_sets_every_knob() {
    let mut g = testing::duel();
    assert_eq!(aggression(&g), 50);
    assert_eq!(
        (sack_cooldown(&g), seek_radius(&g), siege_size(&g), max_near_camp(&g)),
        (8, 5, 2, 2)
    );
    assert_eq!(attack_bar(&g), -17.5);
    g.edit_config(|c| c.barbarian_aggression = Some(0));
    assert_eq!(
        (sack_cooldown(&g), seek_radius(&g), siege_size(&g), max_near_camp(&g)),
        (10, 0, 3, 1)
    );
    g.edit_config(|c| c.barbarian_aggression = Some(100));
    assert_eq!(
        (sack_cooldown(&g), seek_radius(&g), siege_size(&g), max_near_camp(&g)),
        (5, 10, 1, 4)
    );
    assert_eq!(attack_bar(&g), -35.0);
}

#[test]
fn camps_spawn_what_the_barbarians_know_weighted_by_force() {
    let mut g = testing::duel();
    let (warrior, swordsman) = (unit_type(&g, "Warrior"), unit_type(&g, "Swordsman"));
    assert!(force_evaluation(&g, warrior) < force_evaluation(&g, swordsman));
    assert_eq!(force_evaluation(&g, unit_type(&g, "Worker")), 0);
    assert!(unit_options(&g, false).is_empty(), "the barbarians know nothing yet");
    // They learn what every civilization knows: Agriculture gives them the Warrior.
    everyone_knows(&mut g, "Agriculture");
    update_barbarian_techs(&mut g);
    assert_eq!(unit_options(&g, false), [warrior]);
    everyone_knows(&mut g, "Bronze Working");
    update_barbarian_techs(&mut g);
    let spear = unit_type(&g, "Spearman");
    assert!(unit_options(&g, false).contains(&spear));
    clean(&mut g);
}

#[test]
fn a_camp_spawns_on_its_countdown_and_lingers_once_destroyed() {
    let mut g = testing::duel();
    everyone_knows(&mut g, "Agriculture");
    set_turn(&mut g, 50);
    let t = TileIdx(45);
    let id = create_camp(&mut g, t).expect("a camp");
    assert!(is_camp_tile(&g, t));
    update_camps(&mut g);
    let camp = g.state().world().camps[&id];
    assert_eq!(camp.spawned, 0);
    assert!((8..=12).contains(&camp.countdown), "{camp:?}");
    assert!(g.military_at(t).is_some_and(|u| u.owner() == BARBS));
    // A camp attacked spawns sooner.
    camp_attacked(&mut g, t);
    assert_eq!(g.state().world().camps[&id].countdown, camp.countdown / 2);
    // Cleared by Rome: 25 gold on Prince, the camp destroyed, gone 15 turns later.
    let gold = g.player(ROME).map_or(0.0, |p| p.econ.gold);
    assert_eq!(clear_camp(&mut g, t, ROME), 25);
    assert_eq!(g.player(ROME).map_or(0.0, |p| p.econ.gold) - gold, 25.0);
    assert!(!is_camp_tile(&g, t));
    assert!(g.state().world().camps[&id].destroyed);
    for _ in 0..15 {
        update_camps(&mut g);
    }
    assert!(g.state().world().camps.contains_key(&id), "15 turns to forget it");
    update_camps(&mut g);
    assert!(!g.state().world().camps.contains_key(&id));
    clean(&mut g);
}

#[test]
fn the_first_camps_appear_out_of_sight_and_apart() {
    let mut g = testing::duel();
    testing::city(&mut g, ROME, TileIdx(11), "Roma");
    g.settle();
    place_initial_camps(&mut g);
    let camps: Vec<TileIdx> = g.state().world().camps.values().map(|c| c.tile).collect();
    assert!(!camps.is_empty());
    let seen = viewable_by_anyone(&g);
    for &t in &camps {
        assert!(!seen.contains(t.0) && g.is_land(t) && is_camp_tile(&g, t), "{t:?}");
        assert!(g.grid().distance(t, TileIdx(11)) > 4, "away from the capital");
    }
    for (i, &a) in camps.iter().enumerate() {
        for &b in &camps[i + 1..] {
            assert!(g.grid().distance(a, b) > 7, "{a:?} and {b:?}");
        }
    }
    clean(&mut g);
}

#[test]
fn a_sack_takes_gold_and_spares_wonders_and_the_palace() {
    let mut g = testing::duel();
    let c = testing::city(&mut g, ROME, TileIdx(22), "Roma");
    let r = g.rules();
    let named = |n: &str| r.lookup::<BuildingId>(n).expect("a building");
    for b in ["Palace", "The Pyramids", "Monument"] {
        crate::game::cities::founding::add_building(&mut g, c, named(b), false);
    }
    if let Some(p) = g.player_mut(ROME, crate::game::derive::rev::PlayerTouch::STOCKS) {
        p.econ.gold = 400.0;
    }
    assert_eq!(sackable_buildings(&g, c), [named("Monument")]);
    set_turn(&mut g, 100);
    let s = sack_city(&mut g, c);
    // A quarter and an eighth of 400 at aggression 50.
    assert_eq!((s.too_recent, s.gold), (false, 150));
    assert_eq!(g.player(ROME).map(|p| p.econ.gold), Some(250.0));
    assert_eq!(g.city(c).map(|x| (x.sacked_turn, x.health)), Some((100, 50)));
    let again = sack_city(&mut g, c);
    assert!(again.too_recent && again.gold == 0);
    let city = g.city(c).expect("the city");
    assert!(
        city.buildings.contains(named("Palace")) && city.buildings.contains(named("The Pyramids"))
    );
    assert_eq!(city.owner(), ROME);
    clean(&mut g);
}

#[test]
fn a_wounded_raider_loots_before_it_fights() {
    let mut g = testing::duel();
    let c = testing::city(&mut g, ROME, TileIdx(22), "Roma");
    let farm = g.rules().lookup::<crate::base::ids::ImprovementId>("Farm").expect("Farm");
    for n in [TileIdx(23), TileIdx(33)] {
        crate::game::cities::borders::take_ownership(&mut g, c, n);
    }
    g.set_improvement(TileIdx(33), Some(farm)).expect("a tile");
    set_turn(&mut g, 100);
    let b = testing::unit(&mut g, BARBS, "Warrior", TileIdx(34));
    crate::game::units::turn::start_turn(&mut g, b);
    if let Some(x) = g.unit_mut(b, UnitTouch::CORE) {
        x.hp = 20;
    }
    g.settle();
    assert_eq!(pillage_value(&g, BARBS, TileIdx(33)), 2);
    assert_eq!(pillage_value(&g, BARBS, TileIdx(22)), 0, "never a city");
    assert!(automate(&mut g, b).is_ok());
    assert!(g.tile(TileIdx(33)).is_some_and(crate::state::map::Tile::improvement_pillaged));
    assert!(g.unit(b).is_some_and(|x| x.hp > 20), "pillaging heals");
    clean(&mut g);
}

#[test]
fn a_barbarian_turn_is_the_same_twice() {
    let mut g = testing::duel();
    testing::city(&mut g, ROME, TileIdx(11), "Roma");
    set_turn(&mut g, 60);
    for t in [44, 57, 66] {
        testing::unit(&mut g, BARBS, "Warrior", TileIdx(t));
    }
    create_camp(&mut g, TileIdx(58));
    g.settle();
    let mut a = g.clone();
    let mut b = g.clone();
    for x in [&mut a, &mut b] {
        crate::game::units::turn::start_units(x, BARBS);
        take_turn(x);
        x.settle();
    }
    assert_eq!(a.digest().ok(), b.digest().ok());
    assert_ne!(a.digest().ok(), g.digest().ok(), "they did something");
    clean(&mut a);
}
