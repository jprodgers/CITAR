//! Unit tests of the converter on small hand-made Python states, under the embedded ruleset. The
//! fixtures' conversion, and the gates of package 1a-10, are in
//! `crates/citar-testkit/tests/engine/convert.rs`.

use serde_json::{Value, json};

use super::*;

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

/// A two-player duel state on the smallest map, as `GameState.to_dict()` writes one.
fn tiny() -> Value {
    let size = 8 * 8;
    let tiles: Vec<Value> = (0..size)
        .map(|_| {
            json!([
                "Grassland",
                [],
                null,
                0,
                null,
                0,
                null,
                false,
                null,
                false,
                null,
                null,
                false,
                null
            ])
        })
        .collect();
    let player = |id: u8, name: &str, nation: &str| {
        let mut p = json!({
            "id": id, "name": name, "color": "#aa0000", "nation": nation, "kind": "major",
            "controller": "bot", "handicap": "ai",
            "auto": {"un_vote": true, "conquest": true, "free_picks": true},
            "overrides": {}, "difficulty": null, "leader": "", "alive": true, "gold": 10.0,
            "techs": ["Agriculture"], "research_queue": [], "research_goal": null,
            "research_progress": {}, "overflow_science": 0.0, "free_techs": 0, "future_techs": 0,
            "culture": 0.0, "policies": [], "policies_adopted_count": 0, "free_policies": 0,
            "faith": 0.0, "religion_state": "none", "religion": null, "great_prophets_earned": 0,
            "faith_buys": {}, "gp_points": {}, "gp_threshold": 100.0, "gg_points": {},
            "gg_threshold": {}, "free_great_people": 0, "great_people_earned": 0
        });
        let more = json!({
            "golden_age_points": 0.0, "golden_age_turns": 0, "golden_ages": 0,
            "temp_uniques": [], "built_increasing": {}, "bought_increasing": {},
            "free_buildings": {}, "free_stat_buildings": [], "free_specific_buildings": [],
            "natural_wonders": [], "spies": [], "spy_eras": [],
            "explored": crate::base::codec::b64_encode(&[0u8; 64]), "memory": {}, "met": [],
            "capital": null, "original_capital": null, "notes": "", "founded_city": false,
            "eliminated_turn": null, "city_counter": 0, "cs_type": null, "cs_personality": null,
            "cs_resource": null, "cs_unique_unit": null, "influence": {}, "ally": null,
            "protectors": [], "quests": [], "cs_unit_timer": {}, "tribute_turn": {},
            "ruins_rewards": [], "flags": {}
        });
        if let (Some(a), Value::Object(b)) = (p.as_object_mut(), more) {
            a.extend(b);
        }
        p
    };
    json!({
        "config": {"map_size": "duel", "map_type": "continents", "width": 8, "height": 8,
                   "seed": 7, "speed": "Quick", "difficulty": "Prince",
                   "barbarian_difficulty": "Prince", "ai_base_values": "unciv",
                   "starting_era": "Ancient era", "barbarians": "off",
                   "barbarian_aggression": null, "turn_limit": 330,
                   "victories": {"Scientific": true, "Cultural": true, "Domination": false,
                                 "Diplomatic": true, "Time": true},
                   "city_states": 0, "religion": true, "espionage": true,
                   "nuclear_weapons": true, "tech_trading": true, "ruins": true,
                   "map_edges": "ice_caps", "river_density": 1.0, "resources": null,
                   "on_disconnect": "pause", "reconnect_seconds": 180, "players": [],
                   "wrap_x": false, "wrap_y": false},
        "width": 8, "height": 8, "tiles": tiles,
        "players": [player(0, "Rome", "Rome"), player(1, "Greece", "Greece")],
        "units": {}, "cities": {}, "turn": 3, "current": 0, "next_id": 1, "rng_state": null,
        "phase": "playing", "winner": null, "victory": null, "relations": {},
        "open_borders": {}, "deals": [], "negotiations": [], "messages": [], "thoughts": [],
        "events": [], "camps": {}, "stats": [], "turn_started": true, "barbarian_state": {},
        "capture_ids": {}, "religions": {}, "wonders_built": {}, "un": {}, "spaceship": {},
        "continents": [], "first_discovered": {}
    })
}

fn convert_value(v: &Value) -> Result<Converted, ConvertError> {
    state_from_python(v.to_string().as_bytes(), rules())
}

#[test]
fn a_tiny_state_converts_and_drops_nothing() {
    let got = convert_value(&tiny()).expect("converts");
    assert!(got.report.is_empty(), "{}", got.report);
    let st = &got.state;
    assert_eq!(st.players().len(), 2);
    assert_eq!(st.clock().turn, 3);
    assert_eq!(st.config().seed, 7);
    assert_eq!(st.config().disabled_victories.len(), 1);
    assert_eq!(st.ids().unit, 1);
    assert_eq!(st.config().host.len(), 3, "on_disconnect, reconnect_seconds, players");
}

#[test]
fn an_unknown_key_fails_with_its_path() {
    let mut v = tiny();
    v["players"][1]["grudge"] = json!(3);
    let e = convert_value(&v).expect_err("refused");
    assert_eq!(e.path, "players[1].grudge");
    let mut v = tiny();
    v["players"][0]["flags"]["pairs2"] = json!({});
    assert_eq!(convert_value(&v).err().map(|e| e.path), Some("players[0].flags.pairs2".into()));
    let mut v = tiny();
    v["surprise"] = json!(1);
    assert_eq!(convert_value(&v).err().map(|e| e.path), Some("surprise".into()));
}

#[test]
fn an_unknown_name_fails_with_its_path() {
    let mut v = tiny();
    v["players"][0]["techs"] = json!(["Agriculture", "Warp Drive"]);
    let e = convert_value(&v).expect_err("refused");
    assert_eq!(e.path, "players[0].techs[1]");
    assert!(e.message.contains("Warp Drive"), "{e}");
    let mut v = tiny();
    v["tiles"][5][0] = json!("Lava");
    assert_eq!(convert_value(&v).err().map(|e| e.path), Some("tiles[5].terrain".into()));
    // The ruleset's loose resolver takes what an exact match misses.
    let mut v = tiny();
    v["players"][0]["techs"] = json!(["agriculture"]);
    assert!(convert_value(&v).is_ok());
}

#[test]
fn an_unknown_event_type_and_a_nan_fail_with_their_paths() {
    let mut v = tiny();
    v["events"] = json!([{"id": 1, "turn": 1, "type": "meteor", "text": "Boom.", "players": null,
                          "idx": null, "data": {}}]);
    assert_eq!(convert_value(&v).err().map(|e| e.path), Some("events[0].type".into()));
    let text = tiny().to_string().replace("\"gold\":10.0", "\"gold\":NaN");
    let e = state_from_python(text.as_bytes(), rules()).expect_err("NaN is refused");
    assert_eq!(e.path, "players[0].gold", "{e}");
}

#[test]
fn fields_python_lacks_start_as_the_design_says() {
    let mut v2 = tiny();
    v2["players"][0]["flags"] = json!({"start": 9, "culture_last8": [1, 2, 3, 4, 5, 6, 7, 8],
                                       "last_stats": {"gold": 1.0}});
    let got = convert_value(&v2).expect("converts");
    let p = got.state.player(crate::base::ids::PlayerId(0)).expect("player 0");
    assert_eq!(p.start_tile, Some(crate::base::ids::TileIdx(9)));
    assert_eq!(p.econ.culture_hist, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(p.econ.happiness_seen, 0);
    assert_eq!(p.econ.last_gold_rate.to_bits(), 0f64.to_bits());
    assert!(p.seat().driver().is_none());
    assert_eq!(got.state.ids().combat_seq, 0);
    assert_eq!(got.report.count(Drop::LastStats), 1);
}

#[test]
fn name_references_move_from_code_points_to_bytes() {
    let mut v = tiny();
    v["events"] = json!([
        {"id": 1, "turn": 1, "type": "city_founded", "text": "Sweden founded Malmö at Malmö.",
         "players": null, "idx": 9, "x": 1, "y": 1, "data": {"player": 0},
         "refs": [[15, 20, 0, "t"], [24, 29, 0, "t"]]},
        {"id": 2, "turn": 1, "type": "agent_error", "text": "Rome's AI could not play.",
         "players": null, "idx": null, "data": {"player": 0}}
    ]);
    let got = convert_value(&v).expect("converts");
    let events = got.chronicle.events();
    assert_eq!(events.len(), 2);
    let r = &events[0].refs;
    assert_eq!((r[0].start, r[0].end), (15, 21), "ö is two bytes");
    assert_eq!((r[1].start, r[1].end), (25, 31));
    assert_eq!(&events[0].text[r[1].start as usize..r[1].end as usize], "Malmö");
    assert_eq!(got.state.chronicle().engine_events, 1);
    assert_eq!(got.state.host().host_events, 1, "a host event counts only in the host heads");
    assert_eq!(got.state.host().next_event_id, 3);
    // Coordinates that disagree with the tile are refused.
    v["events"][0]["x"] = json!(2);
    assert_eq!(convert_value(&v).err().map(|e| e.path), Some("events[0].x".into()));
}

#[test]
fn the_report_counts_what_was_dropped() {
    let mut v = tiny();
    v["rng_state"] = json!([3, [1, 2], null]);
    v["barbarian_state"] = json!({"x": 1});
    v["players"][0]["faith_buys"] = json!({"Great Prophet": 1});
    v["players"][0]["techs"] = json!(["Pottery", "Agriculture"]);
    let got = convert_value(&v).expect("converts");
    let dropped: Vec<Drop> = got.report.dropped().map(|(d, _)| d).collect();
    assert_eq!(dropped, [Drop::RngState, Drop::BarbarianState, Drop::FaithBuys, Drop::ListOrder]);
    let text = got.report.to_string();
    assert!(text.contains("players[*].faith_buys: 1 (never read)"), "{text}");
}
