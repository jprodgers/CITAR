//! Scenario and test operations and `inspect` (package 1b-02), on a game of the arena:
//! - a list of operations applies all or nothing: a failing third operation leaves the game's
//!   digest, history and revision as they were before the call (gate 5), and so does a failing
//!   list of test operations;
//! - `inspect` reads without writing, and lists what waits for a later package;
//! - a game survives the `reload` test operation whole;
//! - the rules the operations need report a write the state refuses instead of stopping
//!   halfway, and setting research plans on `&Game` before it writes;
//! - the scenario editor's summary reads a saved scenario's state as the engine writes it.

use citar_engine::api::{ErrCode, inspect, scenario, testops};
use citar_engine::base::ids::{PlayerId, TechId};
use citar_engine::game::Game;
use citar_engine::game::city_states::influence::{set_influence, update_ally};
use citar_engine::game::diplomacy::relations::{WarReason, make_peace, set_war};
use citar_engine::game::research::{apply_research, plan_research, research_result};
use citar_engine::rules::Ruleset;
use citar_engine::state::StateError;
use citar_testkit::script::{map_doc, new_game};
use serde_json::json;

/// A bare game on the arena: three benchmark civilizations and a city-state.
fn arena() -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 5,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 1,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    let cfg = cfg.as_object().cloned().unwrap_or_default();
    new_game(Ruleset::shared(), &cfg).unwrap_or_else(|e| panic!("{e}"))
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
    let e = testops::apply(&mut g, &json!([{"op": "barbarian_act"}])).expect_err("none here");
    assert_eq!(e.code, ErrCode::InvalidPlayer);
    assert_eq!(e.message, "Test operation 1 (barbarian_act): The game has no barbarians.");
    let e = g
        .apply_ops(
            &json!([{"op": "remove_units", "x": 5, "y": 5}, {"op": "remove_city", "x": 5, "y": 5}]),
        )
        .expect_err("no city there");
    assert_eq!(e.code, ErrCode::NoSuchCity);
    assert_eq!(e.message, "Operation 2 (remove_city): No such city.");
    assert_eq!((g.digest().ok(), g.rev()), before);
}

#[test]
fn inspect_reads_and_lists_what_is_pending() {
    let g = arena();
    let before = (g.digest().ok(), g.rev());
    for what in ["game", "ops", "pending", "units", "events", "view"] {
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
    assert!(
        !listed.iter().any(|(name, _)| name == "view" || name == "briefing"),
        "the view and the briefing are answered (1d-02, 1d-03)"
    );
    let kinds: Vec<&str> =
        pending.as_array().into_iter().flatten().filter_map(|p| p["kind"].as_str()).collect();
    let count = |kind: &str| kinds.iter().filter(|&&k| k == kind).count();
    assert_eq!(
        [count("inspect"), count("scenario_op"), count("test_op")],
        [0, 0, 0],
        "no query, operation or test op waits"
    );
    // Gate 1 of package 1c-09: no stage of a turn or of setup waits.
    assert_eq!([count("turn_stage"), count("setup_stage")], [0, 0], "no stage waits");
    assert_eq!(listed.len(), kinds.len());
    for (name, _) in &listed {
        assert!(
            ![
                "end_turn",
                "end_round",
                "force_turn",
                "add_unit",
                "remove_units",
                "negotiation",
                "barbarian_act",
                "sack_city",
            ]
            .contains(&name.as_str())
                && ![
                    "found_city",
                    "set_city",
                    "happiness",
                    "remove_city",
                    "adopt_policy",
                    "complete_construction",
                ]
                .contains(&name.as_str())
                && !name.contains("research")
                && !name.contains("cities start")
                && !name.contains("great person")
                && !name.contains("religion")
                && !name.contains("faith")
                && !name.contains("golden")
                && !name.contains("triggers")
                && !name.contains("cities end")
                && !name.contains("culture and policies")
                && !name.contains("science")
                && !name.contains("gold and bankruptcy")
                && !name.contains("temporary uniques")
                && !name.contains("commit the happiness")
                && !name.contains("gold rate"),
            "{name} is ported"
        );
    }
    let b = inspect::inspect(&g, &json!({"what": "briefing", "player": 0})).expect("a briefing");
    assert!(b["text"].as_str().is_some_and(|t| t.starts_with("=== TURN ")), "{b}");
    assert!(b["progress"].as_str().is_some_and(|t| t.starts_with("TURN PROGRESS")), "{b}");
    assert!(b["alerts"].is_array(), "{b}");
    let e = inspect::inspect(&g, &json!({"what": "briefing", "player": 3}))
        .expect_err("a city-state has no briefing");
    assert_eq!(e.code, ErrCode::BadParam);
    let view = inspect::inspect(&g, &json!({"what": "view", "player": 0})).expect("a view");
    assert_eq!(view["you"], json!(0));
    let spectator = inspect::inspect(&g, &json!({"what": "view"})).expect("a spectator's view");
    assert!(spectator["you"].is_null() && spectator["empires"].is_object());
    let e = inspect::inspect(&g, &json!({"what": "view", "player": 3}))
        .expect_err("a city-state has no view");
    assert_eq!(e.code, ErrCode::BadParam);
    let e = inspect::inspect(&g, &json!({"what": "nothing"})).expect_err("an unknown query");
    assert_eq!(e.code, ErrCode::BadParam);
    assert!(e.message.starts_with(
        "Unknown inspect query 'nothing'. Known: briefing, build_options, buildable, camps, city, \
         city_state, costs, events"
    ));
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

/// The scenario editor's list reads a saved scenario's state as the engine writes it
/// (`scenario_summary`, package 1d-03): the save's own JSON, so a change to the save format
/// that the summary does not follow fails here, and a state it cannot read is refused rather
/// than listed empty.
#[test]
fn a_scenario_s_summary_reads_the_engine_s_save() {
    let mut g = arena();
    g.apply_ops(&json!([{"op": "set_player", "player": 1, "name": "Renamed"}])).expect("renamed");
    testops::apply(&mut g, &json!([{"op": "set_turn", "turn": 42}])).expect("a turn");
    let save = g.snapshot().to_json().expect("a save");
    let state: serde_json::Value = serde_json::from_slice(&save).expect("the save's JSON");
    let seats = json!(scenario::default_seats(&g));
    let doc = json!({
        "id": "arena-42", "name": "The arena", "description": "Three and a city-state.",
        "seats": seats, "modified": 1_700_000_000.5, "state": state,
    });
    let s = scenario::scenario_summary(&doc).expect("a summary");
    assert_eq!(
        s,
        json!({
            "id": "arena-42", "name": "The arena", "description": "Three and a city-state.",
            "width": g.grid().width(), "height": g.grid().height(), "turn": 42,
            "players": [
                {"id": 0, "name": g.player(PlayerId(0)).map(|p| &*p.name), "nation": "BenchmarkCiv"},
                {"id": 1, "name": "Renamed", "nation": "BenchmarkCiv"},
                {"id": 2, "name": g.player(PlayerId(2)).map(|p| &*p.name), "nation": "BenchmarkCiv"},
            ],
            "city_states": 1, "seats": seats, "modified": 1_700_000_000.5,
        })
    );
    assert_eq!(s["width"], json!(24), "the arena's size, not a default");

    // What the summary cannot read is refused, whole.
    let refused = |d: &serde_json::Value| {
        scenario::scenario_summary(d).expect_err("refused").message.to_string()
    };
    let mut without = doc.clone();
    without.as_object_mut().map(|o| o.shift_remove("state"));
    assert_eq!(refused(&without), "The scenario has no state.");
    let mut renamed = doc.clone();
    renamed["state"]["players"][3]["kind"] = json!("CityState");
    assert_eq!(refused(&renamed), "The scenario has no kind for each of its players.");
    let mut no_turn = doc.clone();
    no_turn["state"]["clock"] = json!({"round": 42});
    assert_eq!(refused(&no_turn), "The scenario has no turn in its state.");
    let mut no_width = doc;
    no_width["state"]["map"]["width"] = json!("24");
    assert_eq!(refused(&no_width), "The scenario has no map width in its state.");
}

#[test]
fn the_rules_report_a_write_the_state_refuses() {
    let mut g = arena();
    let (a, cs) = (PlayerId(0), PlayerId(3));
    assert!(g.is_city_state(cs));
    let before = (g.digest().ok(), g.rev());
    // Two ids that are no pair, and a player that is no city-state: engine bugs, reported rather
    // than a rule stopped quietly halfway.
    assert!(matches!(set_war(&mut g, a, a, WarReason::Direct), Err(StateError::Pair(_))));
    assert!(matches!(make_peace(&mut g, a, a), Err(StateError::Pair(_))));
    assert!(matches!(update_ally(&mut g, a), Err(StateError::NotACityState(_))));
    assert!(set_influence(&mut g, cs, PlayerId(1), 10.0).is_ok());
    assert!(set_influence(&mut g, a, PlayerId(1), 10.0).is_err(), "a major has no influence");
    assert_ne!((g.digest().ok(), g.rev()), before, "the good write went through");
    // A war and a peace between two real players go through whole.
    g.apply_ops(&json!([{"op": "meet", "a": 0, "b": 1}])).expect("they meet");
    set_war(&mut g, a, PlayerId(1), WarReason::Scenario).expect("a war");
    assert!(g.at_war(a, PlayerId(1)));
    make_peace(&mut g, a, PlayerId(1)).expect("a peace");
    assert!(!g.at_war(a, PlayerId(1)));
}

#[test]
fn research_is_planned_before_it_is_written() {
    let mut g = arena();
    let p = PlayerId(0);
    let tech = |name: &str| -> TechId { g_rules().resolve(name).expect(name) };
    let (pottery, writing, sailing, mining) =
        (tech("Pottery"), tech("Writing"), tech("Sailing"), tech("Mining"));
    let before = (g.digest().ok(), g.rev());
    let path = plan_research(&g, p, writing, false).expect("a path to Writing");
    assert_eq!(path, vec![pottery, writing]);
    assert_eq!((g.digest().ok(), g.rev()), before, "planning only reads");
    apply_research(&mut g, p, &path);
    let out = research_result(&g, p, &path);
    assert_eq!(
        out,
        json!({"researching": "Pottery", "turns": null, "queue": ["Pottery", "Writing"],
               "goal": "Writing", "path": ["Pottery", "Writing"]})
    );
    // Appended, the path goes after what is queued, and what is queued already is refused.
    let more = plan_research(&g, p, sailing, true).expect("Sailing appended");
    assert_eq!(more, vec![pottery, writing, sailing]);
    let e = plan_research(&g, p, writing, true).expect_err("Writing is queued");
    assert_eq!(e.message, "Writing is already queued.");
    let e = plan_research(&g, p, tech("Agriculture"), false).expect_err("known");
    assert_eq!(e.message, "You already know Agriculture.");
    // One tech has no goal or path in its result.
    let one = plan_research(&g, p, mining, false).expect("Mining alone");
    assert_eq!(one, vec![mining]);
    apply_research(&mut g, p, &one);
    assert_eq!(
        research_result(&g, p, &one),
        json!({"researching": "Mining", "turns": null, "queue": ["Mining"]})
    );
}

fn g_rules() -> &'static Ruleset {
    Ruleset::shared()
}
