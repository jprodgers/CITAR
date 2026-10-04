//! City-states and barbarians (package 1c-06), on the arena, with every check on after each call:
//! - three random agents play a hundred turns with a city-state and raging barbarians: the
//!   city-state founds its city and plays its turns, the barbarians their camps and raids, the
//!   agents deal with the city-state (gifts, pledges, tribute, marriage) and stage coups, and
//!   every check holds after every call; the same game played twice ends the same;
//! - a new game's city-state is set up (its personality) and the first camps are placed out of
//!   everyone's sight;
//! - the `city_state_action` and `stage_coup` tools refuse what Python refused.
//!
//! The city-states' rules are unit tests of `game::city_states` and the barbarians' of
//! `game::barbarians`; the rule scripts `city_states_*` and `barbarians_*` pin what both engines
//! share.

use citar_engine::api::testops;
use citar_engine::base::ids::PlayerId;
use citar_engine::game::city_states::CityStateAction;
use citar_engine::game::espionage::StageCoup;
use citar_engine::game::{Action, DebugOptions, DriveOptions, Drivers, Game, Stop};
use citar_engine::rules::Ruleset;
use citar_testkit::agents::RandomAgent;
use citar_testkit::script::{map_doc, new_game};
use serde_json::json;

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// Three BenchmarkCiv seats, a city-state and raging barbarians on the arena, for `turns` turns,
/// every check on; the starting units kept.
fn arena(seed: u64, turns: u32) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": seed,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 1, "barbarians": "raging", "ruins": false, "turn_limit": turns,
        "map": doc,
    });
    let mut g = new_game(Ruleset::shared(), cfg.as_object().expect("an object")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    g
}

/// Drives the three seats with random agents until the turn limit ends the game.
fn play(g: &mut Game) {
    let (mut a, mut b, mut c) = (RandomAgent::new(), RandomAgent::new(), RandomAgent::new());
    let mut d = Drivers::none(g.state().players().len())
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b)
        .with(PlayerId(2), &mut c);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::GameOver);
}

#[test]
fn a_new_game_sets_up_its_city_states_and_camps() {
    let g = arena(3, 50);
    let cs = PlayerId(3);
    let d = g.player(cs).and_then(|p| p.city_state.as_deref()).expect("a city-state");
    assert!(d.personality.is_some() && d.cs_type.is_some());
    let camps: Vec<_> = g.state().world().camps.values().map(|c| c.tile).collect();
    assert!(!camps.is_empty(), "raging barbarians start with camps");
    for t in camps {
        let seen = g.state().players().ids().any(|p| g.derived().vis().sees(p, t));
        assert!(!seen && g.is_land(t), "a camp out of sight on land: {t:?}");
    }
}

#[test]
fn random_agents_play_with_a_city_state_and_raging_barbarians() {
    let mut g = arena(11, 100);
    // Everyone meets the city-state, so the agents deal with it; spies for coups.
    testops::apply(
        &mut g,
        &json!([
            {"op": "add_spy", "player": 0},
            {"op": "add_spy", "player": 1},
        ]),
    )
    .expect("spies");
    g.apply_ops(&json!([
        {"op": "meet", "a": 3, "b": "all"},
        {"op": "set_player", "player": [0, 1, 2], "gold": 500},
    ]))
    .expect("met");
    let mut h = g.clone();
    play(&mut g);
    clean(&mut g);
    let cs = PlayerId(3);
    assert!(g.player_cities(cs).next().is_some(), "the city-state founded its city");
    let tools: Vec<&str> = g.chronicle().actions().iter().map(|a| &*a.tool).collect();
    assert!(tools.contains(&"city_state_action"), "the agents dealt with the city-state");
    let kinds = |k: &str| g.chronicle().events().iter().filter(|e| e.kind.name() == k).count();
    assert!(kinds("cs_meet") >= 3, "each major was greeted");
    assert!(!g.state().world().camps.is_empty(), "the barbarians kept camps");
    // The same game, played again, ends the same.
    play(&mut h);
    assert_eq!(g.digest().ok(), h.digest().ok());
}

#[test]
fn the_tools_refuse_what_python_refused() {
    let mut g = arena(3, 50);
    let refuse = |g: &mut Game, a: Action| g.act(PlayerId(0), a).expect_err("refused").message;
    let cs = |action: &str| {
        Action::CityStateAction(CityStateAction {
            player_id: 3,
            action: json!(action),
            amount: None,
            unit_id: None,
        })
    };
    let unmet = format!("You have not met {}.", name(&g));
    assert_eq!(refuse(&mut g, cs("pledge")), unmet);
    g.apply_ops(&json!([{"op": "meet", "a": 0, "b": 3}])).expect("met");
    assert_eq!(refuse(&mut g, cs("gift_gold")), "Give an amount of gold.");
    assert_eq!(refuse(&mut g, cs("gift_unit")), "Give the unit_id to gift.");
    assert_eq!(refuse(&mut g, cs("withdraw")), "You are not protecting them.");
    assert!(refuse(&mut g, cs("marry")).starts_with("They must be your ally."));
    assert_eq!(refuse(&mut g, cs("make_peace")), "You are not at war with them.");
    let e = refuse(&mut g, Action::StageCoup(StageCoup { spy: json!("Agent 1") }));
    assert!(e.starts_with("You have no spies."), "{e}");
    clean(&mut g);
}

/// The city-state's name.
fn name(g: &Game) -> String {
    g.player(PlayerId(3)).map(|p| p.name.to_string()).unwrap_or_default()
}
