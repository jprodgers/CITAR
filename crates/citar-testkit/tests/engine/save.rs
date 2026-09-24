//! Save format v1, the digest and the journal (package 1a-09), against their contracts:
//! - generated states load back equal, bit for bit (`-0.0` and driver memories included), and a
//!   save written again is the same bytes (gate 1);
//! - without a ruleset context a name is an error, not a panic (gate 2);
//! - a save made under a ruleset with one number changed loads with `rules_changed`; one naming
//!   a building the ruleset no longer has fails with `UnknownName` (gate 3);
//! - no state type uses `skip_serializing_if`, `flatten` or `untagged` (gate 4);
//! - the three checked-in states digest as the golden set `states` says (gate 5, which the
//!   determinism workflow compares across the five targets);
//! - the digest ignores map insertion order and the host's heads (gate 6);
//! - NaN is refused by the save, the digest and validation (gate 7);
//! - random chunk and frame sequences round-trip (gate 8);
//! - `summary()` of the checked-in duel state matches `data/summary_duel.json` (gate 9).
//!
//! Gate 10, the digest's speed, is `citar-bench`'s `digest` benchmark.
//!
//! The number of proptest cases follows `PROPTEST_CASES` (proptest's default is 256), except the
//! state round trips, which run 32: each builds and saves a whole state.

use std::collections::BTreeMap;

use citar_engine::base::digest::{CanonError, to_canon_vec};
use citar_engine::base::ids::{BuildingId, PlayerId};
use citar_engine::base::sets::BitSet;
use citar_engine::rules::Ruleset;
use citar_engine::save::journal::{
    self, FrameDecoder, FramePalette, FrameWriter, FullFrame, JournalCursor,
};
use citar_engine::save::{self, LoadError, canon, ctx, json};
use citar_engine::state::State;
use citar_engine::state::chronicle::{Chronicle, ChronicleHeads, HostHeads};
use citar_testkit::rulesets::{files_of, overlay};
use citar_testkit::states::{self, Gen, Shape};
use proptest::prelude::*;
use serde_json::Value;

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

fn saved(st: &State) -> Vec<u8> {
    save::to_json(rules(), st).unwrap_or_else(|e| panic!("the state saves: {e}"))
}

// ---- Gate 1: round trips ----------------------------------------------------------------------

/// Saves, loads and saves again: the state comes back equal and with the same canonical bytes,
/// and the second save is the first.
fn round_trip(st: &State) -> Result<(), TestCaseError> {
    let first = saved(st);
    let (back, report) = json::read_state(rules(), &first)
        .map_err(|e| TestCaseError::fail(format!("the save does not load: {e}")))?;
    prop_assert!(report.rules_changed.is_none());
    prop_assert!(back == *st, "the loaded state differs");
    prop_assert_eq!(canon::state_bytes(&back), canon::state_bytes(st), "not bit for bit");
    prop_assert!(saved(&back) == first, "the second save differs from the first");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn generated_states_round_trip(seed in any::<u64>(), big in any::<bool>()) {
        let shape = if big { Shape::DUEL } else { Shape::TINY };
        round_trip(&states::build(rules(), seed, &shape))?;
    }
}

#[test]
fn a_standard_state_round_trips_with_every_awkward_value() -> Result<(), TestCaseError> {
    let st = states::build(rules(), 7, &Shape::STANDARD);
    round_trip(&st)?;
    // The generator reached what the gate names.
    let bytes = canon::state_bytes(&st).expect("finite");
    let neg_zero = (-0.0f64).to_bits().to_le_bytes();
    assert!(bytes.windows(8).any(|w| w == neg_zero), "some float is -0.0");
    assert!(st.players().iter().any(|(_, p)| p.seat().driver().is_some()), "a driver's memory");
    assert!(st.units().iter().any(|u| u.carried_by().is_some()), "a carried unit");
    Ok(())
}

#[test]
fn the_save_starts_with_its_header_in_order() {
    let st = states::build(rules(), 3, &Shape::TINY);
    let text = String::from_utf8(saved(&st)).expect("UTF-8");
    let doc: Value = serde_json::from_str(&text).expect("JSON");
    let keys: Vec<&str> = doc.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(keys, json::KEYS);
    assert!(text.starts_with(r#"{"format":"citar-state","version":1,"engine":"#));
    assert_eq!(doc["rules"]["id"], rules().id().to_hex());
    // Rule ids are names, and the tiles are columns.
    assert!(doc["tiles"]["palette"]["terrain"][0].is_string());
    assert!(doc["tiles"]["terrain"].is_string());
    assert!(
        doc["players"][0]["tech"]["known"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    );
}

// ---- Gate 2: no context ------------------------------------------------------------------------

#[test]
fn without_a_ruleset_context_names_are_an_error_not_a_panic() {
    let st = states::build(rules(), 5, &Shape::TINY);
    assert!(ctx::current().is_none());
    let e = serde_json::to_string(st.config()).expect_err("the settings name a speed");
    assert!(e.to_string().contains("with_rules"), "{e}");
    let player = st.player(PlayerId(0)).expect("player 0");
    assert!(serde_json::to_string(player).is_err());
    let text = ctx::with_rules(rules(), || serde_json::to_string(player)).expect("in context");
    assert!(serde_json::from_str::<citar_engine::state::players::Player>(&text).is_err());
    // The digest needs no names.
    assert!(save::digest(rules(), &st).is_ok());
}

// ---- Gate 3: another ruleset --------------------------------------------------------------------

#[test]
fn a_save_loads_under_a_changed_ruleset_with_a_warning() {
    let st = states::build(rules(), 11, &Shape::TINY);
    let bytes = saved(&st);
    let changed = changed_cost(&overlay(&[]).expect("the files"));
    let other = Ruleset::leak(&files_of(&changed)).expect("the changed ruleset loads");
    assert_ne!(other.id(), rules().id());
    let (back, report) = json::read_state(other, &bytes).expect("the names all resolve");
    assert_eq!(report.rules_changed.map(|(id, _)| id), Some(rules().id()));
    // Same names, same order: the same game.
    assert_eq!(canon::state_bytes(&back), canon::state_bytes(&st));
}

/// The embedded files with the first building's cost raised by one.
fn changed_cost(files: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    let mut out = files.to_vec();
    let slot = out.iter_mut().find(|(n, _)| n == "ruleset/buildings.json").expect("buildings");
    let mut v: Value = serde_json::from_slice(&slot.1).expect("JSON");
    let first = match &mut v {
        Value::Array(list) => list.first_mut(),
        Value::Object(map) => map.values_mut().find(|b| b.get("cost").is_some()),
        _ => None,
    };
    let b = first.and_then(Value::as_object_mut).expect("a building");
    let cost = b.get("cost").and_then(Value::as_i64).unwrap_or(0);
    b.insert("cost".to_owned(), Value::from(cost + 1));
    slot.1 = serde_json::to_vec(&v).expect("JSON");
    out
}

/// The embedded files without the building called `name`.
fn without_building(name: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = overlay(&[]).expect("the files");
    let slot = out.iter_mut().find(|(n, _)| n == "ruleset/buildings.json").expect("buildings");
    let mut v: Value = serde_json::from_slice(&slot.1).expect("JSON");
    match &mut v {
        Value::Array(list) => list.retain(|b| b.get("name").and_then(Value::as_str) != Some(name)),
        Value::Object(map) => {
            map.shift_remove(name);
        }
        _ => {}
    }
    slot.1 = serde_json::to_vec(&v).expect("JSON");
    out
}

#[test]
fn a_save_naming_a_removed_building_fails_with_unknown_name() {
    // A building no other file names, so the ruleset still loads without it.
    let name = "Mud Pyramid Mosque";
    let files = without_building(name);
    let other = Ruleset::leak(&files_of(&files)).expect("the ruleset loads without it");
    let id = rules().lookup::<BuildingId>(name).expect("the building is shipped");
    let mut parts = states::build(rules(), 13, &Shape::TINY).into_parts();
    let city = parts.cities.ids()[0];
    let st = {
        let mut cities: Vec<_> = parts.cities.iter().cloned().collect();
        cities.iter_mut().find(|c| c.id() == city).expect("the city").buildings.insert(id);
        parts.cities = citar_engine::state::cities::Cities::from_cities(cities).expect("cities");
        State::from_parts(parts).expect("fits")
    };
    let e = json::read_state(other, &saved(&st)).expect_err("the building is gone");
    match e {
        LoadError::UnknownName { path, name: got } => {
            assert_eq!(got, name);
            assert!(path.starts_with("cities[") && path.ends_with("(building)"), "{path}");
        }
        other => panic!("wanted UnknownName, got {other}"),
    }
}

// ---- Gate 4: banned attributes ------------------------------------------------------------------

#[test]
#[allow(clippy::disallowed_methods, reason = "the test reads the engine's sources")]
fn no_state_type_uses_an_attribute_that_makes_the_encoding_ambiguous() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../citar-engine/src/state");
    let mut files = 0;
    for entry in std::fs::read_dir(&dir).expect("the state sources") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        files += 1;
        let text = std::fs::read_to_string(&path).expect("readable");
        for args in serde_attributes(&text) {
            for banned in ["skip_serializing_if", "flatten", "untagged"] {
                assert!(
                    !args.contains(banned),
                    "{} uses serde({banned}), which state types may not: serde({args})",
                    path.display()
                );
            }
        }
    }
    assert!(files >= 12, "found {files} state files");
    let found = serde_attributes("#[serde(\n    untagged,\n)]\nx.flatten()");
    assert_eq!(found, ["\n    untagged,\n"], "an attribute over several lines is read whole");
}

/// The arguments of every `serde(...)` in `text`, to the matching parenthesis.
fn serde_attributes(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (start, _) in text.match_indices("serde(") {
        let args = start + "serde(".len();
        let mut depth = 1;
        for (i, c) in text[args..].char_indices() {
            depth += match c {
                '(' => 1,
                ')' => -1,
                _ => 0,
            };
            if depth == 0 {
                out.push(&text[args..args + i]);
                break;
            }
        }
    }
    out
}

// ---- Gate 6: what the digest ignores ------------------------------------------------------------

#[test]
fn the_digest_ignores_map_order_and_the_hosts_heads() {
    let st = states::build(rules(), 17, &Shape::DUEL);
    let d = save::digest(rules(), &st).expect("finite");
    // The same maps, filled in reverse.
    let mut parts = st.clone().into_parts();
    fn refill<K: Ord + Copy>(m: &mut BTreeMap<K, f64>) {
        let pairs: Vec<(K, f64)> = m.iter().rev().map(|(k, v)| (*k, *v)).collect();
        *m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k, v);
        }
    }
    for (_, p) in parts.players.iter_mut() {
        refill(&mut p.tech.progress);
        refill(&mut p.gp.points);
        refill(&mut p.gp.combat_points);
    }
    let camps: Vec<_> = parts.world.camps.iter().rev().map(|(k, v)| (*k, *v)).collect();
    parts.world.camps = camps.into_iter().collect();
    let opinions: Vec<_> = parts.diplo.opinions.iter().map(|(k, v)| (k, *v)).collect();
    parts.diplo.opinions = Default::default();
    for ((h, a), v) in opinions.into_iter().rev() {
        parts.diplo.opinions.insert(h, a, v);
    }
    let reordered = State::from_parts(parts).expect("fits");
    assert_eq!(save::digest(rules(), &reordered), Ok(d));

    // Host activity and host settings.
    let mut parts = st.clone().into_parts();
    parts.host.next_event_id += 5;
    parts.host.thoughts += 3;
    parts.host.journal_seq += 1;
    parts.config.host.insert("reconnect_seconds".to_owned(), Value::from(30));
    let hosted = State::from_parts(parts).expect("fits");
    assert_ne!(saved(&hosted), saved(&st), "the save keeps them");
    assert_eq!(save::digest(rules(), &hosted), Ok(d), "the digest does not");

    // Anything else moves it.
    let mut parts = st.into_parts();
    parts.chronicle.messages += 1;
    let moved = State::from_parts(parts).expect("fits");
    assert_ne!(save::digest(rules(), &moved), Ok(d));
}

#[test]
fn the_digest_follows_its_formula() {
    let st = states::build(rules(), 19, &Shape::TINY);
    let mut h = blake3::Hasher::new();
    h.update(b"CITAR-DIGEST");
    h.update(b"CANON_V1");
    h.update(&rules().id().0);
    h.update(&canon::state_bytes(&st).expect("finite"));
    assert_eq!(save::digest(rules(), &st).map(|d| d.0), Ok(*h.finalize().as_bytes()));
    let mut again = save::Digester::new();
    assert_eq!(again.digest(rules(), &st), save::digest(rules(), &st));
    assert_eq!(again.digest(rules(), &st), save::digest(rules(), &st), "the buffer is reused");
}

// ---- Gate 7: NaN ---------------------------------------------------------------------------------

#[test]
fn nan_is_refused_by_the_save_the_digest_and_validation() {
    let mut parts = states::build(rules(), 23, &Shape::TINY).into_parts();
    parts.players[PlayerId(0)].econ.gold = f64::NAN;
    let st = State::from_parts(parts).expect("from_parts does not look at floats");
    assert!(matches!(save::to_json(rules(), &st), Err(save::SaveError::NonFinite(_))));
    assert!(matches!(save::digest(rules(), &st), Err(CanonError::NonFinite { .. })));
    assert!(save::validate(&st, rules()).is_err());
    // JSON has no NaN; what a writer that tried would leave does not load.
    let good = saved(&states::build(rules(), 23, &Shape::TINY));
    let mut doc: Value = serde_json::from_slice(&good).expect("JSON");
    doc["players"][0]["econ"]["gold"] = Value::Null;
    let bad = serde_json::to_vec(&doc).expect("JSON");
    assert!(matches!(json::read_state(rules(), &bad), Err(LoadError::Json(_))));
    let text = String::from_utf8(good).expect("UTF-8");
    let huge = text.replacen(r#""gold":"#, r#""gold":1e999,"x":"#, 1);
    assert!(json::read_state(rules(), huge.as_bytes()).is_err());
}

// ---- Loading refuses what is wrong -------------------------------------------------------------

/// The save of a small state as a JSON value, to break.
fn doc() -> Value {
    serde_json::from_slice(&saved(&states::build(rules(), 29, &Shape::TINY))).expect("JSON")
}

fn load_value(v: &Value) -> Result<State, LoadError> {
    json::read_state(rules(), &serde_json::to_vec(v).expect("JSON")).map(|(s, _)| s)
}

#[test]
fn unknown_keys_versions_and_formats_are_refused() {
    let mut v = doc();
    v.as_object_mut().expect("an object").insert("seed".to_owned(), Value::from(1));
    assert_eq!(load_value(&v).err(), Some(LoadError::UnknownKey("seed".to_owned())));
    let mut v = doc();
    v["version"] = Value::from(2);
    assert_eq!(load_value(&v).err(), Some(LoadError::Version(2)));
    let mut v = doc();
    v["format"] = Value::from("citar-save");
    assert!(matches!(load_value(&v), Err(LoadError::Json(_))));
    let mut v = doc();
    v.as_object_mut().expect("an object").shift_remove("world");
    assert!(matches!(load_value(&v), Err(LoadError::Json(_))));
    // Below the top level an unknown key is a format error, with its place.
    let mut v = doc();
    v["clock"]["hour"] = Value::from(3);
    assert!(matches!(load_value(&v), Err(LoadError::Json(m)) if m.starts_with("clock")));
    assert!(json::read_state(rules(), b"[1, 2]").is_err());
    assert!(json::read_state(rules(), b"{").is_err());
}

#[test]
fn inconsistent_saves_are_refused_with_their_place() {
    let invalid = |edit: &dyn Fn(&mut Value), wants: &str| {
        let mut v = doc();
        edit(&mut v);
        match load_value(&v) {
            Err(LoadError::Invalid(errs)) => {
                assert!(errs.iter().any(|e| e.to_string().contains(wants)), "{errs:?}")
            }
            Err(e) => assert!(e.to_string().contains(wants), "wanted {wants:?}: {e}"),
            Ok(_) => panic!("wanted a refusal naming {wants:?}"),
        }
    };
    // A capital that is not the player's.
    invalid(&|v| v["players"][0]["capital"] = Value::from(99_999), "capital");
    // A unit on a tile off the map.
    invalid(&|v| v["units"][0]["goto"] = Value::from(64), "goto");
    // A tile column of the wrong length.
    invalid(&|v| v["tiles"]["owner"] = Value::from("AAAA"), "owner column");
    // A palette position past its palette.
    invalid(&|v| v["tiles"]["palette"]["terrain"] = Value::Array(Vec::new()), "palette");
    // A map the grid does not take.
    invalid(&|v| v["map"]["width"] = Value::from(60_000), "map");
    // A driver's memory over the limit, refused before it is decoded.
    invalid(
        &|v| {
            let huge = "A".repeat(6 << 20);
            v["players"][0]["seat"]["driver"] =
                serde_json::json!({"kind": 1, "version": 1, "bytes": huge});
        },
        "over the limit",
    );
    // An id counter behind an id in use.
    invalid(&|v| v["ids"]["unit"] = Value::from(1), "unit id");
    // A relation of a pair that is not one.
    let fresh =
        serde_json::to_value(citar_engine::state::diplo::Relation::default()).expect("JSON");
    invalid(&|v| v["diplomacy"]["relations"] = serde_json::json!([[2, 1, fresh]]), "relation");
}

proptest! {
    /// Whatever a corrupt save holds, loading it returns; it never panics.
    #[test]
    fn corrupt_saves_never_panic(seed in any::<u64>(), edits in 1usize..6) {
        let mut v = doc();
        let mut g = Gen::new(seed);
        for _ in 0..edits {
            corrupt(&mut v, &mut g);
        }
        let _outcome = load_value(&v);
    }
}

/// Replaces one random leaf of `v` with a random value.
fn corrupt(v: &mut Value, g: &mut Gen) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            let i = g.idx(m.len());
            if let Some((_, child)) = m.iter_mut().nth(i) {
                corrupt(child, g);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            let i = g.idx(a.len());
            corrupt(&mut a[i], g);
        }
        leaf => {
            *leaf = match g.below(8) {
                0 => Value::Null,
                1 => Value::from(-1),
                2 => Value::from(u64::MAX),
                3 => Value::from(4_000_000_000u64),
                4 => Value::from("Moon Base"),
                5 => Value::from(0.5),
                6 => Value::Array(vec![Value::from(1)]),
                _ => Value::from(true),
            };
        }
    }
}

// ---- Gate 8: chunks and frames --------------------------------------------------------------------

proptest! {
    /// A history taken as chunks at random points rebuilds whole, and agrees with its heads.
    #[test]
    fn random_chunk_sequences_rebuild_the_history(
        seed in any::<u64>(),
        cuts in proptest::collection::vec(0usize..12, 1..8),
    ) {
        let r = rules();
        let mut chron = Chronicle::new();
        let mut heads = ChronicleHeads::default();
        let mut host = HostHeads::default();
        let mut cursor = JournalCursor::default();
        let mut chunks = Vec::new();
        for (i, n) in cuts.iter().enumerate() {
            states::history(r, seed ^ i as u64, *n, &mut heads, &mut host, &mut chron);
            if let Some(c) = journal::take_chunk(r, &chron, &mut cursor, &mut host)
                .map_err(|e| TestCaseError::fail(e.to_string()))?
            {
                prop_assert_eq!(c.seq, chunks.len() as u32);
                chunks.push(c.json);
            }
        }
        prop_assert_eq!(cursor, JournalCursor::at_end(&chron));
        let (back, complete) =
            journal::rebuild(r, &mut chunks.iter().map(Vec::as_slice), &heads, &host);
        prop_assert!(complete, "the rebuilt history agrees with its heads");
        prop_assert!(back == chron, "the rebuilt history is the history");
        if !chunks.is_empty() {
            // One chunk short: the history is incomplete, and says so.
            let (_, complete) = journal::rebuild(
                r,
                &mut chunks.iter().skip(1).map(Vec::as_slice),
                &heads,
                &host,
            );
            prop_assert!(!complete);
        }
    }

    /// Frames written as keyframes and deltas decode to the frames written.
    #[test]
    fn random_frame_sequences_round_trip(seed in any::<u64>(), n in 1usize..150) {
        let frames = frame_sequence(seed, n);
        let mut writer = FrameWriter::new();
        let mut decoder = FrameDecoder::new();
        let mut keys = 0;
        for (i, f) in frames.iter().enumerate() {
            let rec = writer.push(f);
            keys += usize::from(rec.keyframe);
            if i == 0 {
                prop_assert!(rec.keyframe, "the first frame is a keyframe");
            }
            let back = decoder.apply(&rec).map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(&back, f);
        }
        prop_assert!(keys >= n.div_ceil(64), "a keyframe at least every 64 frames");
    }
}

/// A plausible run of frames: tiles change a little each round, exploration only grows, and now
/// and then a major forgets a tile, which forces a keyframe.
fn frame_sequence(seed: u64, n: usize) -> Vec<FullFrame> {
    let mut g = Gen::new(seed);
    let tiles = 64 + g.idx(200);
    let palette = FramePalette {
        improvement: vec!["Farm".into(), "Mine".into(), "Ñandú Pen".into()],
        feature: vec!["Forest".into(), "Jungle".into()],
    };
    let mut f = FullFrame {
        turn: 1,
        palette,
        owner: vec![255; tiles],
        improvement: vec![0; tiles],
        route: vec![0; tiles],
        feature: vec![0; tiles],
        explored: vec![(PlayerId(0), BitSet::new()), (PlayerId(2), BitSet::new())],
        ..FullFrame::default()
    };
    let mut out = Vec::with_capacity(n);
    for turn in 1..=n {
        f.turn = turn as i32;
        for _ in 0..g.idx(10) {
            let t = g.idx(tiles);
            f.owner[t] = g.pick(&[255, 0, 2, 254]);
            f.improvement[t] = g.below(4) as u8;
            f.route[t] = g.pick(&[0, 1, 2, 5, 6]);
            f.feature[t] = g.below(3) as u8;
        }
        for (_, set) in &mut f.explored {
            for _ in 0..g.idx(20) {
                set.insert(g.below(tiles as u64) as u32);
            }
            if g.chance(3) {
                set.remove(g.below(tiles as u64) as u32);
            }
        }
        f.cities = (0..g.idx(5))
            .map(|i| journal::FrameCity {
                id: 1 + i as u32,
                name: g.text(),
                owner: g.below(3) as u8,
                tile: g.below(tiles as u64) as u32,
                pop: g.below(30) as u16,
                capital: g.chance(30),
            })
            .collect();
        f.units = (0..g.idx(20))
            .map(|_| journal::FrameUnit {
                base: g.below(127) as u16,
                owner: g.below(3) as u8,
                hp: g.below(101) as u8,
                tile: g.below(tiles as u64) as u32,
            })
            .collect();
        let end = f.event_range.1 + g.below(40) as u32;
        f.event_range = (f.event_range.1, end);
        out.push(f.clone());
    }
    out
}

#[test]
fn frames_capture_a_state_and_are_smaller_as_deltas() {
    let st = states::build(rules(), 31, &Shape::DUEL);
    let frame = FullFrame::capture(rules(), &st, (0, 10));
    let tiles = st.tiles().len();
    assert_eq!(frame.owner.len(), tiles);
    assert_eq!(frame.explored.len(), 2, "one per major");
    assert_eq!(frame.units.len(), st.units().len());
    let mut writer = FrameWriter::new();
    let key = writer.push(&frame);
    let mut next = frame.clone();
    next.turn += 1;
    next.event_range = (10, 12);
    let delta = writer.push(&next);
    assert!(key.keyframe && !delta.keyframe);
    assert!(
        delta.bytes.len() < key.bytes.len() / 4,
        "{} vs {}",
        delta.bytes.len(),
        key.bytes.len()
    );
    let mut dec = FrameDecoder::new();
    assert!(dec.apply(&delta).is_err(), "a delta needs its keyframe");
    assert_eq!(dec.apply(&key).as_ref(), Ok(&frame));
    assert_eq!(dec.apply(&delta).as_ref(), Ok(&next));
}

#[test]
fn a_loaded_game_rebuilds_its_history_from_its_chunks() {
    let r = rules();
    let mut chron = Chronicle::new();
    let mut heads = ChronicleHeads::default();
    let mut host = HostHeads::default();
    let mut cursor = JournalCursor::default();
    let mut chunks = Vec::new();
    for i in 0..3 {
        states::history(r, i, 20, &mut heads, &mut host, &mut chron);
        chunks.extend(journal::take_chunk(r, &chron, &mut cursor, &mut host).expect("saves"));
    }
    let (seq, entries) = journal::decode_chunk(r, &chunks[1].json).expect("decodes");
    assert_eq!(seq, 1);
    assert_eq!(entries.len(), 20, "the second round's entries");
    assert!(journal::decode_chunk(r, b"{\"format\": \"citar-state\"}").is_err());
    let mut parts = states::build(r, 37, &Shape::TINY).into_parts();
    parts.chronicle = heads;
    parts.host.0 = host;
    let st = State::from_parts(parts).expect("fits");
    let bytes = saved(&st);
    let got = save::load(r, &bytes, &mut chunks.iter().map(|c| c.json.as_slice())).expect("loads");
    assert!(!got.report.chronicle_incomplete);
    assert_eq!(got.chronicle, chron);
    let short = save::load(r, &bytes, &mut chunks.iter().take(2).map(|c| c.json.as_slice()))
        .expect("a short history still loads");
    assert!(short.report.chronicle_incomplete);
    assert_eq!(short.state, st, "and the game is the same");
}

// ---- Snapshots -------------------------------------------------------------------------------

#[test]
fn a_snapshot_saves_and_digests_as_its_state() {
    let st = states::build(rules(), 41, &Shape::TINY);
    let snap = save::Snapshot::new(rules(), &st);
    assert_eq!(snap.to_json().ok(), Some(saved(&st)));
    assert_eq!(snap.digest(), save::digest(rules(), &st));
    assert_eq!(to_canon_vec(snap.state()).ok(), canon::state_bytes(&st).ok());
}

// ---- Gate 5: the checked-in states ---------------------------------------------------------------

#[test]
fn the_checked_in_states_digest_as_the_golden_set_says() {
    let report = citar_testkit::golden::states::check_states();
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
}

// ---- Gate 9: the summary --------------------------------------------------------------------------

#[test]
#[allow(clippy::disallowed_methods, reason = "the fixture is a file")]
fn the_summary_of_the_duel_state_matches_its_fixture() {
    let bytes = citar_testkit::golden::states::read_state_file("duel").expect("the duel state");
    let got = serde_json::to_value(save::summary(&bytes).expect("a summary")).expect("JSON");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/summary_duel.json");
    let want: Value =
        serde_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("JSON");
    assert_eq!(got, want);

    // A winner is named; a save that is not a state is refused.
    let mut doc: Value = serde_json::from_slice(&bytes).expect("JSON");
    doc["clock"]["winner"] = Value::from(1);
    let won = save::summary(&serde_json::to_vec(&doc).expect("JSON")).expect("a summary");
    assert_eq!(won.winner_id, Some(1));
    assert_eq!(won.winner.as_deref(), want["names"]["1"].as_str());
    doc["version"] = Value::from(9);
    let newer = save::summary(&serde_json::to_vec(&doc).expect("JSON"));
    assert_eq!(newer.err(), Some(LoadError::Version(9)));
    assert!(save::summary(b"{\"format\": \"citar-journal\"}").is_err());
}
