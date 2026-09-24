//! Scenario and test operations and `inspect` (package 1b-02), on a game of the arena:
//! - a list of operations applies all or nothing: a failing third operation leaves the game's
//!   digest, history and revision as they were before the call (gate 5), and so does a failing
//!   list of test operations;
//! - `inspect` reads without writing, and lists what waits for a later package;
//! - a game survives the `reload` test operation whole.

use citar_engine::api::{ErrCode, inspect, testops};
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_testkit::script::{rules_dir, setup};
use serde_json::{Value, json};

/// A bare game on the arena: three benchmark civilizations and a city-state.
fn arena() -> Game {
    #[allow(clippy::disallowed_methods, reason = "the map is a file")]
    let text = std::fs::read_to_string(rules_dir().join("maps/arena.json")).expect("the arena");
    let mut doc: Value = serde_json::from_str(&text).expect("the arena's JSON");
    doc.as_object_mut().map(|m| m.shift_remove("anchors"));
    let cfg = json!({
        "seed": 5,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 1,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    let cfg = cfg.as_object().cloned().unwrap_or_default();
    setup::new_game(Ruleset::shared(), &cfg).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_failing_third_op_leaves_the_game_as_it_was() {
    let mut g = arena();
    let ops = json!([
        {"op": "set_player", "player": 0, "gold": 500, "name": "Changed"},
        {"op": "set_relation", "a": 0, "b": 1, "state": "war", "embassies": true},
        {"op": "set_tile", "x": 5, "y": 5, "terrain": "Nowhere"},
    ]);
    let before = (g.digest().ok(), g.chronicle().events().len(), g.rev());
    let e = g.apply_ops(&ops).expect_err("the third op fails");
    assert_eq!(e.message, "Operation 3 (set_tile): Unknown terrain 'Nowhere'.");
    assert_eq!(e.code, ErrCode::BadParam);
    assert_eq!((g.digest().ok(), g.chronicle().events().len(), g.rev()), before);
    assert!(g.check_invariants().is_empty());

    // The first two alone apply, meet on the way, and settle.
    let two = json!([ops[0], ops[1]]);
    let (results, batch) = g.apply_ops(&two).expect("two good ops");
    assert_eq!(results, vec![json!({}), json!({"war": true})]);
    assert!(batch.events().iter().any(|e| e.kind.name() == "first_contact"));
    assert_ne!(g.digest().ok(), before.0);
    assert!(g.check_invariants().is_empty());

    // A war with a friendship in force is no state a game can reach: refused, whole.
    let at = (g.digest().ok(), g.rev());
    let e = g
        .apply_ops(
            &json!([{"op": "set_relation", "a": 1, "b": 2, "state": "war", "friends": true}]),
        )
        .expect_err("war and friendship at once");
    assert!(e.message.ends_with("can have no friendship, defensive pact or open borders."));
    assert_eq!((g.digest().ok(), g.rev()), at);
}

#[test]
fn a_failing_list_of_test_ops_changes_nothing_either() {
    let mut g = arena();
    let before = (g.digest().ok(), g.rev());
    let e = testops::apply(
        &mut g,
        &json!([{"op": "set_turn", "turn": 9}, {"op": "unmeet", "a": 0, "b": 0}]),
    )
    .expect_err("the second test op fails");
    assert_eq!(e.message, "Test operation 2 (unmeet): A player cannot forget itself.");
    assert_eq!((g.digest().ok(), g.rev()), before);
    let e = testops::apply(&mut g, &json!([{"op": "end_turn"}])).expect_err("not ported yet");
    assert_eq!(e.code, ErrCode::NotPorted);
    let e = g
        .apply_ops(&json!([{"op": "found_city", "player": 0, "x": 5, "y": 5}]))
        .expect_err("not yet");
    assert_eq!(e.code, ErrCode::NotPorted);
    assert!(e.message.starts_with("Operation 1 (found_city): This operation is not ported"));
    assert_eq!((g.digest().ok(), g.rev()), before);
}

#[test]
fn inspect_reads_and_lists_what_is_pending() {
    let g = arena();
    let before = (g.digest().ok(), g.rev());
    for what in ["game", "ops", "pending", "units", "events"] {
        inspect::inspect(&g, &json!({"what": what}))
            .unwrap_or_else(|e| panic!("{what}: {}", e.message));
    }
    for p in 0..4 {
        inspect::inspect(&g, &json!({"what": "player", "player": p})).expect("a player");
    }
    inspect::inspect(&g, &json!({"what": "relation", "a": 0, "b": 3})).expect("a relation");
    inspect::inspect(&g, &json!({"what": "tile", "x": 3, "y": 4})).expect("a tile");
    inspect::inspect(&g, &json!({"what": "find_tiles", "x": 5, "y": 5, "radius": 2}))
        .expect("tiles");
    assert_eq!((g.digest().ok(), g.rev()), before, "reads are free");

    let pending = inspect::inspect(&g, &json!({"what": "pending"})).expect("the pending list");
    let listed: Vec<(String, String)> = pending
        .as_array()
        .expect("a list")
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap_or_default().to_owned(),
                p["package"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    for (name, pkg) in [
        ("found_city", "1b-07"),
        ("add_unit", "1c-02"),
        ("end_turn", "1b-03"),
        ("attack_as", "1c-03"),
    ] {
        assert!(listed.contains(&(name.to_owned(), pkg.to_owned())), "{name} waits for {pkg}");
    }
    assert_eq!(listed.len(), 6 + 15, "six scenario ops and fifteen test ops wait");
    let e = inspect::inspect(&g, &json!({"what": "nothing"})).expect_err("an unknown query");
    assert!(e.message.starts_with("Unknown inspect query 'nothing'. Known: city, events"));
}

#[test]
fn a_reload_keeps_the_game_and_its_history() {
    let mut g = arena();
    g.apply_ops(&json!([{"op": "meet", "a": 0, "b": "all"}, {"op": "grant_tech", "player": 1, "tech": "Writing"}]))
        .expect("ops");
    let before = (g.digest().ok(), g.chronicle().events().len());
    testops::apply(&mut g, &json!([{"op": "reload"}])).expect("a reload");
    assert_eq!((g.digest().ok(), g.chronicle().events().len()), before);
    assert!(g.check_invariants().is_empty());
    // And again, from a game that was itself loaded.
    testops::apply(&mut g, &json!([{"op": "reload"}])).expect("a second reload");
    assert_eq!((g.digest().ok(), g.chronicle().events().len()), before);
}
