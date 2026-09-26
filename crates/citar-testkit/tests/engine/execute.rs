//! `Game::execute` (package 1d-01, gate 4): a tool called by name with JSON arguments, as
//! `tools.execute` called it (`tools.py:84-132`), on a game of the arena.
//!
//! - the checks run in Python's order: the tool, then the caller (for an action, its turn too),
//!   then the arguments, whose missing required ones are reported before any is coerced;
//! - unknown keys are dropped, and an action is logged with the arguments it took;
//! - a query is answered at any time, by any major civilization, and changes nothing;
//! - a refused call changes nothing (property P2).
//!
//! Gate 3: every refusal a model can meet reads as a sentence it can use (property P5): every
//! tool called with argument sets right and wrong (`citar_testkit::calls`) on every committed
//! fixture, by the player whose turn it is and by another.

use std::collections::{BTreeMap, BTreeSet};

use citar_engine::api::tools::ToolKind;
use citar_engine::api::{ErrCode, testops, text_rule_broken};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;
use citar_testkit::script::{map_doc, new_game};
use citar_testkit::{calls, fixtures, games};
use serde_json::{Value, json};

/// A game on the arena: two benchmark civilizations with their starting units, no city-state.
fn arena() -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 5,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    let cfg = cfg.as_object().cloned().unwrap_or_default();
    new_game(Ruleset::shared(), &cfg).unwrap_or_else(|e| panic!("{e}"))
}

/// A call's refusal, code and text.
fn refusal(g: &mut Game, pid: u8, tool: &str, args: &Value) -> (ErrCode, String) {
    let before = (g.digest().ok(), g.chronicle().events().len(), g.rev());
    let e = g.execute(PlayerId(pid), tool, args).expect_err("refused");
    let after = (g.digest().ok(), g.chronicle().events().len(), g.rev());
    assert_eq!(before, after, "{tool} {args}: a refusal changes nothing");
    (e.code, e.message)
}

/// The id of one of `pid`'s units, as the tools take it.
fn a_unit(g: &Game, pid: u8) -> i64 {
    g.player_units(PlayerId(pid)).map(|u| i64::from(u.id().get())).next().expect("a unit")
}

#[test]
fn the_caller_is_checked_before_the_arguments_and_the_tool_before_the_caller() {
    let mut g = arena();
    assert_eq!(g.current(), PlayerId(0));
    let whose = g.player(PlayerId(0)).map(|p| p.name.to_string()).expect("a player");
    let not_yours = format!("It is not your turn (it is {whose}'s turn).");
    let (code, text) = refusal(&mut g, 1, "move_unit", &json!({}));
    assert_eq!((code, &text), (ErrCode::NotYourTurn, &not_yours));
    let (code, text) = refusal(&mut g, 1, "set_research", &json!({"tech": "Pottery"}));
    assert_eq!((code, &text), (ErrCode::NotYourTurn, &not_yours));
    let (code, text) = refusal(&mut g, 40, "fly_to_the_moon", &json!({}));
    assert_eq!((code, text.as_str()), (ErrCode::UnknownTool, "Unknown tool 'fly_to_the_moon'."));
    let (code, text) = refusal(&mut g, 40, "get_empire", &json!({}));
    assert_eq!((code, text.as_str()), (ErrCode::InvalidPlayer, "Invalid player."));
    let (code, text) = refusal(&mut g, 40, "end_turn", &json!({}));
    assert_eq!((code, text.as_str()), (ErrCode::InvalidPlayer, "Invalid player."));
    // A tool's name may be anything; a refusal quotes only so much of it.
    let (_, text) = refusal(&mut g, 0, &"x".repeat(5000), &json!({}));
    assert!(text.len() < 100 && text.ends_with("...'."), "{text}");
}

#[test]
fn missing_parameters_are_reported_before_any_is_coerced() {
    let mut g = arena();
    let (code, text) = refusal(&mut g, 0, "move_unit", &json!({"unit_id": "abc"}));
    assert_eq!(
        (code, text.as_str()),
        (ErrCode::MissingParam, "Missing required parameter(s): x, y.")
    );
    let (code, text) =
        refusal(&mut g, 0, "move_unit", &json!({"unit_id": "abc", "x": 0, "y": null}));
    assert_eq!((code, text.as_str()), (ErrCode::MissingParam, "Missing required parameter(s): y."));
    let (code, text) = refusal(&mut g, 0, "move_unit", &json!({"unit_id": "abc", "x": 0, "y": 0}));
    assert_eq!(
        (code, text.as_str()),
        (ErrCode::BadParam, "Parameter 'unit_id' must be an integer.")
    );
    // A query's arguments are coerced the same way.
    let (code, text) = refusal(&mut g, 1, "get_tile", &json!({"x": "one"}));
    assert_eq!((code, text.as_str()), (ErrCode::MissingParam, "Missing required parameter(s): y."));
}

#[test]
fn unknown_keys_are_dropped_and_the_action_logged_as_taken() {
    let mut g = arena();
    let done = g
        .execute(PlayerId(0), "set_research", &json!({"tech": "Pottery", "why": "pots", "x": 1}))
        .expect("research is set");
    assert_eq!(done.kind, ToolKind::Action);
    let last = g.chronicle().actions().last().expect("logged");
    assert_eq!((&*last.tool, &*last.args), ("set_research", r#"{"tech":"Pottery"}"#));
    // Arguments sent as text are read as Python read them.
    let u = a_unit(&g, 0);
    let done = g
        .execute(PlayerId(0), "unit_order", &json!({"unit_id": u.to_string(), "order": "sleep"}))
        .expect("the unit sleeps");
    assert_eq!(done.result, json!({"ok": true}));
}

#[test]
fn a_query_is_answered_at_any_time_and_changes_nothing() {
    let mut g = arena();
    let before = (g.digest().ok(), g.chronicle().events().len(), g.rev());
    let notes = g.execute(PlayerId(1), "read_notes", &json!({})).expect("any time");
    assert_eq!((notes.kind, notes.result), (ToolKind::Query, json!("(empty)")));
    assert!(notes.events.is_empty());
    let events = g.execute(PlayerId(1), "get_events", &json!({"since_id": "0"})).expect("events");
    let list = events.result.as_array().expect("a list");
    assert!(list.len() <= 40);
    for e in list {
        let keys: Vec<&String> = e.as_object().map(|m| m.keys().collect()).unwrap_or_default();
        assert_eq!(keys, ["id", "turn", "type", "text"]);
    }
    assert_eq!((g.digest().ok(), g.chronicle().events().len(), g.rev()), before);
    // A query of an action is refused; execute takes it.
    let e = g.execute_query(PlayerId(0), "end_turn", &json!({})).expect_err("an action");
    assert_eq!(e.code, ErrCode::BadParam);
    // Notes written are read back, by their writer only.
    g.execute(PlayerId(1), "write_notes", &json!({"text": "Build a navy."})).expect("any time");
    let mine = g.execute_query(PlayerId(1), "read_notes", &json!({})).expect("read");
    assert_eq!(mine, json!("Build a navy."));
    let theirs = g.execute_query(PlayerId(0), "read_notes", &json!({})).expect("read");
    assert_eq!(theirs, json!("(empty)"));
}

#[test]
fn preview_attack_answers_as_the_combat_preview_or_refuses_as_it_does() {
    let mut g = arena();
    // Two warriors face each other, at war.
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    g.apply_ops(&json!([
        {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
        {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 6, "y": 5},
        {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
    ]))
    .expect("the units");
    let u = a_unit(&g, 0);
    let p = g
        .execute(PlayerId(1), "preview_attack", &json!({"unit_id": u, "x": 6, "y": 5}))
        .map_err(|e| e.message);
    assert!(p.as_ref().is_err_and(|e| e.starts_with("You have no unit with id")), "{p:?}");
    let p = g
        .execute(PlayerId(0), "preview_attack", &json!({"unit_id": u, "x": 6, "y": 5}))
        .expect("a preview");
    assert!(p.result["damage_to_defender"].is_array(), "{}", p.result);
    let (code, text) =
        refusal(&mut g, 0, "preview_attack", &json!({"unit_id": u, "x": -3, "y": 99}));
    assert_eq!(code, ErrCode::OffMap);
    assert!(text.starts_with("(-3,99) is off the map (map is "), "{text}");
}

#[test]
fn an_argument_the_action_cannot_read_is_refused_by_name() {
    let mut g = arena();
    let u = a_unit(&g, 0);
    let (code, text) = refusal(&mut g, 0, "unit_order", &json!({"unit_id": u, "order": 5}));
    assert_eq!((code, text.as_str()), (ErrCode::BadParam, "Parameter 'order' must be a string."));
    let (code, text) = refusal(&mut g, 0, "get_rules", &json!({"topic": ["units"]}));
    assert_eq!((code, text.as_str()), (ErrCode::BadParam, "Parameter 'topic' must be a string."));
    let (_, text) = refusal(&mut g, 0, "get_rules", &json!({"topic": " Astrology "}));
    assert!(text.starts_with("Unknown topic 'astrology'. Topics: units, buildings,"), "{text}");
}

#[test]
fn an_action_returns_the_events_it_appended() {
    let mut g = arena();
    let done = g.execute(PlayerId(0), "end_turn", &json!({})).expect("the turn ends");
    assert_eq!(done.kind, ToolKind::Action);
    assert!(!done.events.is_empty(), "the turn's events");
    assert_eq!(g.current(), PlayerId(1));
    let (code, _) = refusal(&mut g, 0, "end_turn", &json!({}));
    assert_eq!(code, ErrCode::NotYourTurn);
}

#[test]
fn every_refusal_reads_as_a_sentence_and_changes_nothing() {
    let mut refused: BTreeMap<&str, u32> = BTreeMap::new();
    let mut taken: BTreeMap<&str, u32> = BTreeMap::new();
    let mut texts = BTreeSet::new();
    let mut broken = Vec::new();
    for f in fixtures::committed().expect("the fixtures") {
        let base = games::from_fixture(&f, b"refusals", DebugOptions::default()).expect("loads");
        let current = base.current();
        let other = base.majors(true).map(|p| p.id()).find(|&p| p != current);
        // The player whose turn it is, another, and one the game does not have.
        for pid in [Some(current), other, Some(PlayerId(63))].into_iter().flatten() {
            let mut g = base.clone();
            for (tool, args) in calls::battery(&base, pid) {
                let before = (g.rev(), g.chronicle().events().len());
                match g.execute(pid, tool, &args) {
                    Ok(_) => {
                        *taken.entry(tool).or_default() += 1;
                        g = base.clone();
                    }
                    Err(e) => {
                        *refused.entry(tool).or_default() += 1;
                        let after = (g.rev(), g.chronicle().events().len());
                        assert_eq!(
                            before, after,
                            "{}: {tool} {args}: refused, yet changed",
                            f.name
                        );
                        if let Some(why) = text_rule_broken(&e.message) {
                            broken.push(format!("{tool} {args}: {why}: {}", e.message));
                        }
                        texts.insert(e.message);
                    }
                }
            }
        }
    }
    broken.sort();
    broken.dedup();
    assert!(
        broken.is_empty(),
        "{} refusals break the text rules:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // Every tool was refused somewhere, and the actions were taken somewhere too: the calls reach
    // past the first check.
    for spec in &citar_engine::api::tools::registry::TOOLS {
        assert!(refused.contains_key(spec.name()), "{} was never refused", spec.name());
    }
    assert!(taken.len() >= 25, "the calls that were taken: {taken:?}");
    assert!(texts.len() >= 300, "{} distinct refusals", texts.len());
}

/// Long values for a parameter of this type: the long name where the tool reads a name, in a
/// list, as an object's key and value, and in each place of a deal item.
fn long_values(json: citar_engine::api::tools::SchemaType, name: &str) -> Vec<Value> {
    use citar_engine::api::tools::SchemaType;
    let keyed = |k: &str, v: Value| Value::Object([(k.to_owned(), v)].into_iter().collect());
    match json {
        SchemaType::String | SchemaType::IntegerOrString | SchemaType::Any => vec![json!(name)],
        SchemaType::Strings => vec![json!([name, name]), json!(name)],
        SchemaType::Object => vec![keyed(name, json!(1)), json!({"Scientist": name})],
        SchemaType::Objects => {
            let mut deep = json!({"type": name});
            for _ in 0..40 {
                deep = json!([deep, keyed(name, json!(name))]);
            }
            vec![
                json!([name]),
                json!([{"type": name}]),
                json!([{"type": "resource", "resource": name}]),
                json!([{"type": "tech", "tech": name}]),
                json!([{"type": "gold_per_turn", "amount": name, "turns": name}]),
                json!([{"type": "gold", "amount": "lots", "note": [name, name, {"why": name}]}]),
                json!([keyed(name, json!(name))]),
                json!([{"type": "city", "city_id": name}]),
                json!([deep]),
            ]
        }
        SchemaType::Integer | SchemaType::Number | SchemaType::Boolean => vec![json!(name)],
    }
}

/// Why a refusal that quotes a caller's words back reads badly, if it does: a cut quote run
/// into the full stop (`....`), or a bracket left open.
fn reads_badly(text: &str) -> Option<&'static str> {
    if text.contains("....") {
        return Some("a cut quote runs into the full stop");
    }
    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        if text.matches(open).count() != text.matches(close).count() {
            return Some("a bracket is left open");
        }
    }
    None
}

#[test]
fn a_name_too_long_is_quoted_back_in_part() {
    let name = "Zanzibar ".repeat(200);
    let mut broken = Vec::new();
    let mut quoted = 0;
    let mut items = 0;
    for f in fixtures::committed().expect("the fixtures") {
        let base = games::from_fixture(&f, b"long-names", DebugOptions::default()).expect("loads");
        let pid = base.current();
        let mut g = base.clone();
        // Every argument set of the battery, with each parameter in turn made long: the other
        // arguments carry the call past the checks that come before the one quoting it.
        for (tool, set) in calls::battery(&base, pid) {
            let Some(spec) = citar_engine::api::tools::registry::tool(tool) else { continue };
            for p in spec.args.params {
                for long in long_values(p.json, &name) {
                    let mut args = set.clone();
                    if let Some(m) = args.as_object_mut() {
                        m.insert(p.name.to_owned(), long.clone());
                        for r in spec.args.required {
                            m.entry((*r).to_owned()).or_insert_with(|| json!(1));
                        }
                    }
                    let Err(e) = g.execute(pid, spec.name(), &args) else {
                        g = base.clone();
                        continue;
                    };
                    quoted += usize::from(e.message.contains("Zanzibar"));
                    items += usize::from(e.message.contains("deal item"));
                    let why = text_rule_broken(&e.message).or_else(|| reads_badly(&e.message));
                    if let Some(why) = why {
                        broken.push(format!("{} {}: {why}: {}", spec.name(), p.name, e.message));
                    }
                }
            }
        }
    }
    broken.sort();
    broken.dedup();
    assert!(broken.is_empty(), "{}", broken.join("\n"));
    assert!(quoted > 20, "the refusals quote the name: {quoted}");
    assert!(items > 0, "no call reached the deal items");
}
