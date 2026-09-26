//! The map editor's API (package 1d-03, gate 3; `api::maps`): a document is checked, its problems
//! fixed and reported; blank and generated maps check clean; and a game's terrain, exported as a
//! map, reads back as it was written and plays again. The rule scripts' wrapping arena stays the
//! arena's tiles.
//!
//! What the Python engine answered for the committed fixtures' exported maps (their summary,
//! what the editor says of them, whether they read back) is compared in
//! `crates/citar-refcheck/tests/query_tools.rs`.

use citar_engine::api::maps::{blank_map, generate_map, map_summary, validate_map};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;
use citar_engine::state::Phase;
use citar_testkit::games::{self, agents_for};
use citar_testkit::script::rules_dir;
use serde_json::{Value, json};

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

/// A document's tiles with one row changed.
fn with_row(mut doc: Value, i: usize, row: Value) -> Value {
    doc["tiles"][i] = row;
    doc
}

#[test]
fn a_map_s_problems_are_fixed_and_reported() {
    let r = rules();
    let blank = blank_map(r, 10, 9, "Grassland", "").expect("a blank map");
    let mut doc = with_row(
        blank,
        0,
        json!(["Plains", ["Oasis", "Forest"], "Nowhere", 64, "Iron", 0, "Mine", "Canal"]),
    );
    doc = with_row(doc, 3, json!(["Ocean", [], null, 0, null, 0, "Farm", "Road"]));
    doc["name"] = json!("A".repeat(90));
    doc["description"] = json!("d".repeat(2100));
    doc["starts"] = json!([3, 5, 5, "x", 900]);
    doc["cs_starts"] = json!([5, 7]);
    doc["wrap_y"] = json!(true);
    doc["author"] = json!("someone");
    let (clean, warnings) = validate_map(r, &doc).expect("fixed, not refused");
    assert_eq!(clean["name"].as_str().map(|s| s.chars().count()), Some(80));
    assert_eq!(clean["description"].as_str().map(|s| s.chars().count()), Some(2000));
    assert_eq!(clean["id"], json!("a".repeat(60)), "the name's slug");
    assert_eq!(clean["author"], json!("someone"), "kept as it is");
    assert_eq!(clean["tiles"][0], json!(["Plains", ["Forest"], null, 0, "Iron", 2, "Mine", null]));
    assert_eq!(clean["tiles"][3], json!(["Ocean", [], null, 0, null, 0, null, null]));
    assert_eq!((clean["starts"].clone(), clean["cs_starts"].clone()), (json!([5]), json!([7])));
    assert_eq!(clean["wrap_y"], json!(false));
    assert_eq!(
        warnings,
        [
            "(0,0) Oasis cannot be on Plains; removed",
            "(0,0) unknown natural wonder 'Nowhere' removed",
            "(0,0) unknown route 'Canal' removed",
            "(3,0) land-only improvement or route on water removed",
            "(3,0) start position on Ocean removed",
            "North-south wrapping needs an even height; this map does not wrap north-south.",
        ]
    );
    // What cannot be a map is refused, with the reason the editor shows.
    let err = |v: &Value| validate_map(r, v).expect_err("refused").to_string();
    assert_eq!(err(&json!("x")), "A map must be a JSON object.");
    assert_eq!(
        err(&json!({"width": 300, "height": 8})),
        "Maps must be between 8 and 256 tiles on each side."
    );
    let mut short = doc.clone();
    short["tiles"].as_array_mut().expect("rows").pop();
    assert_eq!(err(&short), "A 10x9 map needs 90 tiles, got 89.");
    let bad = with_row(doc, 9, json!(["Lava", [], null, 0, null, 0, null, null]));
    assert_eq!(err(&bad), "Tile (9,0) has unknown base terrain 'Lava'.");
}

#[test]
fn blank_and_generated_maps_check_clean() {
    let r = rules();
    let blank = blank_map(r, 12, 10, "Ocean", "Open Sea!").expect("a blank map");
    assert_eq!(
        (blank["id"].clone(), blank["name"].clone()),
        (json!("open-sea"), json!("Open Sea!"))
    );
    let (clean, warnings) = validate_map(r, &blank).expect("a blank map checks");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(clean["tiles"], blank["tiles"]);
    let summary = map_summary(r, &blank).expect("a summary");
    assert_eq!(summary["land_share"], json!(0.0));
    assert_eq!(summary["name"], json!("Open Sea!"));
    let untitled = blank_map(r, 8, 8, "Grassland", "").expect("a blank map");
    assert_eq!(
        (untitled["id"].clone(), untitled["name"].clone()),
        (json!(""), json!("Untitled map"))
    );
    assert_eq!(map_summary(r, &untitled).expect("a summary")["land_share"], json!(1.0));
    // A base terrain only, by its exact name, and a size in bounds.
    for (w, h, t) in [(8, 8, "Hill"), (8, 8, "grassland"), (8, 8, "Mount Fuji"), (7, 8, "Ocean")] {
        let e = blank_map(r, w, h, t, "").expect_err("refused");
        assert!(matches!(e, citar_engine::api::EngineError::Map(_)), "{e}");
    }
    for (seed, map_type) in [(3, "continents"), (4, "pangaea"), (5, "archipelago")] {
        let settings =
            json!({"map_size": "duel", "map_type": map_type, "players": 2, "city_states": 2});
        let doc = generate_map(r, seed, &settings).expect("a generated map");
        let (clean, warnings) = validate_map(r, &doc).expect("a generated map checks");
        assert!(warnings.is_empty(), "{map_type}: {warnings:?}");
        assert_eq!(clean["tiles"], doc["tiles"], "{map_type}");
        assert_eq!(
            (clean["starts"].clone(), clean["cs_starts"].clone()),
            (doc["starts"].clone(), doc["cs_starts"].clone())
        );
    }
}

/// A game on a generated duel map, played by random agents for `rounds` rounds.
fn played(rounds: u32) -> Game {
    let settings = json!({
        "seed": 17, "map_size": "duel", "map_type": "continents", "players": [{}, {}],
        "city_states": 2, "barbarians": "normal", "turn_limit": 200,
    });
    let mut g = games::new_game(&settings, b"maps", DebugOptions::OFF).expect("a game");
    let mut agents = agents_for(&g);
    games::play_random(&mut g, &mut agents, rounds, &mut |_, _| Ok(())).expect("the game plays");
    assert_eq!(g.phase(), Phase::Playing);
    g
}

#[test]
fn a_game_s_map_exported_reads_back_and_plays_again() {
    let r = rules();
    let g = played(40);
    let doc = g.export_map("");
    assert_eq!(doc["name"], json!("Map from game"));
    assert_eq!(doc["description"], json!(format!("Terrain of game turn {}.", g.turn())));
    let rows = doc["tiles"].as_array().expect("rows");
    assert_eq!(rows.len(), g.grid().size() as usize);
    // No city centre is a map's: the capitals are the starts.
    assert!(rows.iter().all(|row| row[6] != json!("City center")));
    let capitals: Vec<Value> = g
        .majors(false)
        .filter_map(|p| p.capital.and_then(|c| g.city(c)).map(|c| json!(c.tile().0)))
        .collect();
    assert!(!capitals.is_empty(), "40 rounds found capitals");
    for c in &capitals {
        assert!(doc["starts"].as_array().expect("starts").contains(c), "{c} is a start");
    }
    // It reads back as it was written: nothing to fix, nothing moved.
    let (clean, warnings) = validate_map(r, &doc).expect("the game's map checks");
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut same = doc.clone();
    same["id"] = clean["id"].clone();
    assert_eq!(clean, same);
    // And a new game on it starts on those tiles, which it exports again as they were.
    let settings = json!({
        "seed": 2, "players": [{}, {}], "city_states": 0, "barbarians": "off", "ruins": false,
        "map": doc,
    });
    let again = games::new_game(&settings, b"maps", DebugOptions::ALL).expect("a game on the map");
    let second = again.export_map("Again");
    assert_eq!(second["id"], json!("again"));
    assert_eq!(second["tiles"], doc["tiles"]);
    assert_eq!(
        (second["wrap_x"].clone(), second["wrap_y"].clone()),
        (doc["wrap_x"].clone(), doc["wrap_y"].clone())
    );
    let starts: Vec<Value> =
        doc["starts"].as_array().expect("starts").iter().take(2).cloned().collect();
    assert_eq!(
        second["starts"],
        json!(starts),
        "the new civilizations start where the old capitals stood"
    );
}

/// A script map's wrapping copy (`tests/rules/maps/<name>_wrap.json`) is the map itself but for
/// its id, name, description and wrapping: its tiles, starts and anchors must stay the other's,
/// or a script on the copy would test another map than its anchors say.
/// `tests/test_rule_scripts.py` checks the same on the Python side.
#[test]
fn a_script_map_s_wrapping_copy_keeps_its_tiles() {
    let dir = rules_dir().join("maps");
    #[allow(clippy::disallowed_methods, reason = "the maps are files")]
    let read = |name: &str| -> Value {
        let path = dir.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    #[allow(clippy::disallowed_methods, reason = "the maps are files")]
    let mut copies: Vec<String> = std::fs::read_dir(&dir)
        .expect("the maps")
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.strip_suffix("_wrap.json").map(str::to_owned))
        .collect();
    copies.sort();
    assert!(copies.contains(&"arena".to_owned()), "the arena has its copy: {copies:?}");
    for base in copies {
        let (mut map, mut copy) = (read(&base), read(&format!("{base}_wrap")));
        assert_eq!((copy["wrap_x"].clone(), copy["wrap_y"].clone()), (json!(true), json!(true)));
        for key in ["id", "name", "description", "wrap_x", "wrap_y"] {
            map.as_object_mut().map(|o| o.shift_remove(key));
            copy.as_object_mut().map(|o| o.shift_remove(key));
        }
        assert!(map == copy, "{base}_wrap.json is no longer {base}.json wrapping");
    }
}
