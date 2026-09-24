//! The Python-state converter (package 1a-10) on the recorded fixtures, against its gates:
//! - every fixture converts, with no unknown key, name or event type (gate 1);
//! - the counts match Python's: tiles, players, units, cities, events, messages, stats rows,
//!   thoughts, and each player's explored tiles (gate 2);
//! - ids are kept: players, units, cities, camps, deals, negotiations and events, and each tile's
//!   owner and city (gate 3);
//! - a converted state saves and loads back to the same digest, and its history to the same
//!   heads (gate 4);
//! - what the report says was dropped is what the design drops (gate 5).
//!
//! The committed fixtures (9 mini and 3 late) always run. The corpus runs too when
//! `CITAR_REFCHECK_CORPUS` names its folder, which makes the 262 of gate 1. Gate 6 is refcheck's
//! (`state_echo`), gate 7 the golden set `convert`, and gate 8 `cargo xtask check`.

use std::collections::BTreeSet;

use citar_engine::base::ids::{CityId, PlayerId};
use citar_engine::compat::python::{ConvertReport, Converted, Dropped, state_from_python};
use citar_engine::rules::Ruleset;
use citar_engine::save::{self, journal, json};
use citar_engine::state::chronicle::Chronicle;
use citar_testkit::fixtures::{self, Fixture};
use serde_json::{Value, json};

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

/// Every fixture the gates cover in this run, and whether the corpus is among them.
fn every_fixture() -> (Vec<Fixture>, bool) {
    let mut out = fixtures::committed().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out.len(), 12, "9 mini and 3 late states (DESIGN.md 2.2)");
    let corpus = fixtures::corpus().unwrap_or_else(|e| panic!("{e}"));
    let with_corpus = corpus.is_some();
    out.extend(corpus.into_iter().flatten());
    (out, with_corpus)
}

fn state_json(f: &Fixture) -> (Vec<u8>, Value) {
    let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    let v: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    (bytes, v)
}

fn convert(name: &str, bytes: &[u8]) -> Converted {
    state_from_python(bytes, rules()).unwrap_or_else(|e| panic!("{name} does not convert: {e}"))
}

/// Python's count of the explored tiles of one player.
fn explored_count(p: &Value) -> usize {
    let text = p["explored"].as_str().unwrap_or_default();
    let bytes = citar_engine::base::codec::b64_decode(text).unwrap_or_default();
    bytes.iter().filter(|&&b| b != 0).count()
}

fn ids_of(v: &Value) -> BTreeSet<u32> {
    v.as_object().map(|m| m.keys().filter_map(|k| k.parse().ok()).collect()).unwrap_or_default()
}

fn list_ids(v: &Value) -> Vec<u64> {
    v.as_array().map(|a| a.iter().filter_map(|x| x["id"].as_u64()).collect()).unwrap_or_default()
}

/// Gates 1 to 4 on one fixture.
fn check(f: &Fixture, total: &mut ConvertReport) {
    let (bytes, py) = state_json(f);
    let got = convert(&f.name, &bytes);
    total.merge(&got.report);
    let st = &got.state;
    let name = &f.name;

    // Gate 2: counts.
    let len = |k: &str| py[k].as_array().map_or(0, Vec::len);
    let keys = |k: &str| py[k].as_object().map_or(0, serde_json::Map::len);
    assert_eq!(st.tiles().len(), len("tiles"), "{name}: tiles");
    assert_eq!(st.players().len(), len("players"), "{name}: players");
    assert_eq!(st.units().len(), keys("units"), "{name}: units");
    assert_eq!(st.cities().len(), keys("cities"), "{name}: cities");
    assert_eq!(got.chronicle.events().len(), len("events"), "{name}: events");
    assert_eq!(got.chronicle.messages().len(), len("messages"), "{name}: messages");
    assert_eq!(got.chronicle.stats().len(), len("stats"), "{name}: stats rows");
    assert_eq!(got.chronicle.thoughts().len(), len("thoughts"), "{name}: thoughts");
    for (i, p) in py["players"].as_array().into_iter().flatten().enumerate() {
        let id = PlayerId(u8::try_from(i).unwrap_or(u8::MAX));
        let player = st.player(id).unwrap_or_else(|| panic!("{name}: player {i}"));
        // The barbarians keep no explored tiles (DESIGN.md 4.3); the report counts theirs.
        let want = if player.is_barbarian() { 0 } else { explored_count(p) };
        assert_eq!(player.explored.len(), want, "{name}: player {i}'s explored tiles");
    }

    // Gate 3: ids.
    let units: BTreeSet<u32> = st.units().iter().map(|u| u.id().get()).collect();
    assert_eq!(units, ids_of(&py["units"]), "{name}: unit ids");
    let cities: BTreeSet<u32> = st.cities().iter().map(|c| c.id().get()).collect();
    assert_eq!(cities, ids_of(&py["cities"]), "{name}: city ids");
    let camps: BTreeSet<u32> = st.world().camps.keys().map(|c| c.get()).collect();
    assert_eq!(camps, ids_of(&py["camps"]), "{name}: camp ids");
    for (i, p) in st.players().iter() {
        assert_eq!(py["players"][usize::from(i.0)]["id"], json!(i.0), "{name}: player {i}");
        assert_eq!(p.id(), i);
    }
    let deals: Vec<u64> = st.diplo().deals.iter().map(|d| u64::from(d.id.get())).collect();
    assert_eq!(deals, list_ids(&py["deals"]), "{name}: deal ids");
    let negs: Vec<u64> = st.diplo().negotiations.iter().map(|n| u64::from(n.id.get())).collect();
    assert_eq!(negs, list_ids(&py["negotiations"]), "{name}: negotiation ids");
    let events: Vec<u64> = got.chronicle.events().iter().map(|e| u64::from(e.id.get())).collect();
    assert_eq!(events, list_ids(&py["events"]), "{name}: event ids");
    for (t, tile) in st.tiles().iter() {
        let row = &py["tiles"][t.0 as usize];
        assert_eq!(tile.owner().map(|p| u64::from(p.0)), row[10].as_u64(), "{name}: tile {t}");
        assert_eq!(tile.city().map(|c| u64::from(c.get())), row[11].as_u64(), "{name}: tile {t}");
    }
    for u in st.units().iter() {
        let p = &py["units"][u.id().get().to_string()];
        assert_eq!(u64::from(u.tile().0), p["idx"].as_u64().unwrap_or(u64::MAX), "{name}");
        assert_eq!(u64::from(u.owner().0), p["owner"].as_u64().unwrap_or(u64::MAX), "{name}");
    }

    // Gate 4: saved and loaded, the same digest, the same bytes again, and the history's heads.
    let digest = save::digest(rules(), st).unwrap_or_else(|e| panic!("{name}: digest: {e}"));
    let saved = save::to_json(rules(), st).unwrap_or_else(|e| panic!("{name}: save: {e}"));
    let (back, report) =
        json::read_state(rules(), &saved).unwrap_or_else(|e| panic!("{name}: load: {e}"));
    assert!(report.rules_changed.is_none());
    assert_eq!(save::digest(rules(), &back).ok(), Some(digest), "{name}: digest after a load");
    assert!(back == *st, "{name}: the loaded state differs");
    let again = save::to_json(rules(), &back).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(again == saved, "{name}: the second save differs");
    assert_history_rebuilds(name, &got.chronicle, &back);
}

/// The history, taken as one journal chunk, rebuilds to the heads the state holds.
fn assert_history_rebuilds(name: &str, chron: &Chronicle, st: &citar_engine::state::State) {
    let mut cursor = journal::JournalCursor::default();
    let mut host = citar_engine::state::chronicle::HostHeads::default();
    let chunk = journal::take_chunk(rules(), chron, &mut cursor, &mut host)
        .unwrap_or_else(|e| panic!("{name}: the history does not encode: {e}"));
    let mut heads = st.host().0.clone();
    heads.journal_seq = host.journal_seq;
    let chunks: Vec<Vec<u8>> = chunk.into_iter().map(|c| c.json).collect();
    let mut iter = chunks.iter().map(Vec::as_slice);
    let (rebuilt, complete) = journal::rebuild(rules(), &mut iter, st.chronicle(), &heads, true);
    assert!(complete, "{name}: the history does not rebuild to the state's heads");
    assert_eq!(rebuilt.events().len(), chron.events().len(), "{name}");
}

// Gates 1-4 --------------------------------------------------------------------------------------

#[test]
#[allow(clippy::disallowed_macros, reason = "the run's drops, for --no-capture")]
fn every_fixture_converts_with_counts_and_ids_kept_and_round_trips() {
    let mut total = ConvertReport::default();
    let (all, with_corpus) = every_fixture();
    for f in &all {
        check(f, &mut total);
    }
    // What was dropped over the run, for the log.
    println!("{} states converted; dropped:\n{total}", all.len());

    // Gate 5 over the run: a new kind of drop must be acknowledged here. The corpus adds one
    // kind to the committed fixtures', two free-building entries of cities since lost.
    let kinds: BTreeSet<Dropped> = total.dropped().map(|(d, _)| d).collect();
    let mut want: BTreeSet<Dropped> = COMMITTED_DROPS.into_iter().collect();
    if with_corpus {
        want.extend(CORPUS_DROPS);
    }
    assert_eq!(kinds, want, "the kinds dropped over the run:\n{total}");
}

// Gate 5 ---------------------------------------------------------------------------------------

/// The kinds of drop the committed fixtures hold: every dead field they fill.
const COMMITTED_DROPS: [Dropped; 6] = [
    Dropped::RngState,
    Dropped::LastStats,
    Dropped::ExplorerGone,
    Dropped::BarbarianExplored,
    Dropped::MinorMemory,
    Dropped::ListOrder,
];

/// The kinds the corpus drops besides those.
const CORPUS_DROPS: [Dropped; 1] = [Dropped::FreeBuildingsElsewhere];

#[test]
fn the_committed_fixtures_drop_what_the_design_drops() {
    let mut union = BTreeSet::new();
    for f in fixtures::committed().unwrap_or_else(|e| panic!("{e}")) {
        let (bytes, _) = state_json(&f);
        let got = convert(&f.name, &bytes);
        union.extend(got.report.dropped().map(|(d, _)| d));
    }
    let want: BTreeSet<Dropped> = COMMITTED_DROPS.into_iter().collect();
    assert_eq!(union, want);
}

/// A committed state with every dead field filled in, and one of each other drop.
fn everything_dropped() -> Value {
    let f = fixtures::committed()
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .find(|f| f.name == "duel-continents-normal/t50")
        .unwrap_or_else(|| panic!("the duel fixture"));
    let (_, mut v) = state_json(&f);
    v["rng_state"] = json!([3, [1, 2, 3], null]);
    v["barbarian_state"] = json!({"camps_seen": 1});
    v["capture_ids"] = json!({"5": 1});
    v["first_discovered"] = json!({"Krakatoa": 0});
    let p0 = &mut v["players"][0];
    p0["flags"]["last_stats"] = json!({"gold": 1.0});
    p0["flags"]["unreachable_explore"] = json!([4]);
    p0["flags"]["explore_targets"]["99999"] = json!(4);
    p0["cs_unit_timer"] = json!({"2": 3});
    p0["tribute_turn"] = json!({"2": 9});
    p0["ruins_rewards"] = json!(["a stash of gold"]);
    p0["spy_eras"] = json!(["Ancient era"]);
    p0["faith_buys"] = json!({"Great Prophet": 1});
    p0["gp_threshold"] = json!(200.0);
    p0["free_buildings"]["99999"] = json!(["Monument"]);
    let techs = p0["techs"].as_array().cloned().unwrap_or_default();
    p0["techs"] = Value::Array(techs.into_iter().rev().collect());
    let barbarian = v["players"]
        .as_array()
        .and_then(|ps| ps.iter().position(|p| p["kind"] == "barbarian"))
        .unwrap_or_else(|| panic!("a barbarian player"));
    v["players"][barbarian]["explored"] =
        json!(citar_engine::base::codec::b64_encode(&vec![1u8; 44 * 28]));
    let cs = v["players"]
        .as_array()
        .and_then(|ps| ps.iter().position(|p| p["kind"] == "city_state"))
        .unwrap_or_else(|| panic!("a city-state"));
    v["players"][cs]["memory"]["3"] = json!({"f": [], "i": null, "r": null, "o": null, "p": false});
    let quest = v["players"]
        .as_array_mut()
        .and_then(|ps| ps.iter_mut().find_map(|p| p["quests"].as_array_mut()?.first_mut()))
        .unwrap_or_else(|| panic!("a quest"));
    quest["data2"] = json!("x");
    if let Some(u) = v["units"].as_object_mut().and_then(|m| m.values_mut().next()) {
        u["build"] = json!({"target": "Farm", "progress": 1, "total": 5});
        u["due_heal"] = json!(true);
    }
    if let Some(c) = v["cities"].as_object_mut().and_then(|m| m.values_mut().next()) {
        c["spaceship_parts"] = json!({"SS Booster": 1});
    }
    v["negotiations"][0]["exchanges"] = json!(99);
    v
}

#[test]
fn a_state_with_every_dead_field_filled_drops_each_kind() {
    let v = everything_dropped();
    let got = convert("the filled state", v.to_string().as_bytes());
    let dropped: Vec<Dropped> = got.report.dropped().map(|(d, _)| d).collect();
    assert_eq!(dropped, Dropped::ALL, "{}", got.report);
    for d in Dropped::ALL {
        assert!(!d.field().is_empty() && !d.reason().is_empty());
    }
}

#[test]
fn a_city_the_player_holds_gets_its_free_buildings() {
    let f = fixtures::committed()
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .find(|f| f.name == "small-continents-normal-s1025/t280")
        .unwrap_or_else(|| panic!("the late fixture"));
    let (bytes, py) = state_json(&f);
    let got = convert(&f.name, &bytes);
    let mut moved = 0;
    for p in py["players"].as_array().into_iter().flatten() {
        for (cid, list) in p["free_buildings"].as_object().into_iter().flatten() {
            let Some(c) = cid.parse().ok().and_then(CityId::new) else { continue };
            let Some(city) = got.state.cities().get(c) else { continue };
            for b in list.as_array().into_iter().flatten().filter_map(Value::as_str) {
                let id = rules().lookup(b).unwrap_or_else(|| panic!("{b}"));
                assert!(city.free_buildings.contains(id), "{b} in city {c}");
                moved += 1;
            }
        }
    }
    assert!(moved > 0, "the late fixture has free buildings");
}
