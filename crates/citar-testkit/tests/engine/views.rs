//! The views (package 1d-02), with every check on.
//!
//! - Gate 3: the replay's frames. A hundred-turn arena game played by random agents, saved and
//!   loaded halfway: `replay_data(Full)` holds, frame by frame, the frames captured from the state
//!   as each round ended, in Python's shape; its last frame is the map of the state as the game
//!   ends, rebuilt here as `victory.record_frame` built it; and `replay_data(Delta)`'s records
//!   decode back to the captured frames.
//! - A view is a read: it changes neither the game nor its digest, and the bytes `view_json`
//!   writes are the view.
//!
//! Gate 1 is the refcheck group `views` and `crates/citar-refcheck/tests/query_tools.rs`; gate 2
//! is `api::views::tests` in the engine, which emits the events it scrubs.

use citar_engine::api::testops;
use citar_engine::api::views::ReplayFormat;
use citar_engine::base::codec::b64_decode;
use citar_engine::base::ids::PlayerId;
use citar_engine::game::victory::records;
use citar_engine::game::{DebugOptions, DriverOutcome, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::save::journal::{FrameDecoder, FullFrame};
use citar_engine::state::Phase;
use citar_engine::state::chronicle::FrameRecord;
use citar_engine::state::players::DriverMemory;
use citar_testkit::agents::RandomAgent;
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Map, Value, json};

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
}

/// Three random agents' game on the arena, with a city-state and barbarians, ending at turn 100.
fn arena_game() -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 33,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 1, "barbarians": "normal", "turn_limit": 100, "map": doc,
    });
    let mut g = new_game(Ruleset::shared(), cfg.as_object().expect("an object"))
        .unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    g
}

/// Plays the game to its end, the players' turns by random agents, reloading it at turn 50.
fn play_out(g: &mut Game) {
    let mut agents = [RandomAgent::new(), RandomAgent::new(), RandomAgent::new()];
    let mut reloaded = false;
    while g.phase() == Phase::Playing {
        let pid = g.current();
        let Some(agent) = agents.get_mut(usize::from(pid.0)) else { panic!("{pid:?} plays") };
        let mut memory = DriverMemory::empty(0, 0);
        assert_eq!(agent.play_turn(g, pid, &mut memory), DriverOutcome::Done);
        g.end_turn(pid).expect("the turn ends");
        if !reloaded && g.turn() == 50 {
            testops::apply(g, &json!([{"op": "reload"}])).expect("a reload");
            reloaded = true;
        }
        assert!(g.turn() <= 101, "the game ends at its turn limit");
    }
    clean(g);
}

fn b64(v: &Value) -> Vec<u8> {
    b64_decode(v.as_str().expect("base64 text")).expect("base64")
}

/// A captured frame in Python's shape, written here from its fields (`victory.record_frame`):
/// the layers as they are, since the game's ruleset numbers the replay's palettes.
fn python_frame(f: &FullFrame, units: &[Box<str>]) -> Value {
    let tiles = f.owner.len();
    let explored: Map<String, Value> = f
        .explored
        .iter()
        .map(|(p, set)| {
            let bytes: Vec<u8> = (0..tiles).map(|i| u8::from(set.contains(i as u32))).collect();
            (p.0.to_string(), json!(citar_engine::base::codec::b64_encode(&bytes)))
        })
        .collect();
    json!({
        "turn": f.turn,
        "owner": citar_engine::base::codec::b64_encode(&f.owner),
        "improvement": citar_engine::base::codec::b64_encode(&f.improvement),
        "route": citar_engine::base::codec::b64_encode(&f.route),
        "feature": citar_engine::base::codec::b64_encode(&f.feature),
        "cities": f.cities.iter().map(|c| json!([c.id, &*c.name, c.owner, c.tile, c.pop, c.capital])).collect::<Vec<_>>(),
        "units": f.units.iter().map(|u| json!([&*units[usize::from(u.base)], u.owner, u.tile, u.hp])).collect::<Vec<_>>(),
        "explored": explored,
        "event_range": [f.event_range.0, f.event_range.1],
    })
}

/// The last frame checked against the state itself, as `record_frame` read it: each tile's
/// owner, improvement, route and top feature but hills, by the replay's names; the cities, the
/// units and what each major has explored.
fn check_against_the_state(g: &Game, replay: &Value, frame: &Value) {
    let r = g.rules();
    let names = |key: &str| -> Vec<String> {
        replay[key]
            .as_array()
            .expect("names")
            .iter()
            .map(|n| n.as_str().unwrap_or("").to_owned())
            .collect()
    };
    let (imps, feats) = (names("improvement_ids"), names("feature_ids"));
    let (owner, imp, route, feat) = (
        b64(&frame["owner"]),
        b64(&frame["improvement"]),
        b64(&frame["route"]),
        b64(&frame["feature"]),
    );
    for (t, tile) in g.state().tiles().iter() {
        let i = t.0 as usize;
        assert_eq!(owner[i], tile.owner().map_or(255, |p| p.0.min(254)), "tile {i}'s owner");
        let want_imp = tile
            .improvement()
            .and_then(|x| r.name(x))
            .map_or(0, |n| imps.iter().position(|x| x == n).map_or(0, |p| p + 1));
        assert_eq!(usize::from(imp[i]), want_imp, "tile {i}'s improvement");
        let level = tile.route().map_or(0, |x| x as u8) + if tile.route_pillaged() { 4 } else { 0 };
        assert_eq!(route[i], level, "tile {i}'s route");
        let hill = r.derived().known.hill;
        let top = tile.features().iter().filter(|&f| f != hill).last();
        let want_feat = top
            .and_then(|f| r.derived().features.get(f))
            .and_then(|&t| r.name(t))
            .map_or(0, |n| feats.iter().position(|x| x == n).map_or(0, |p| p + 1));
        assert_eq!(usize::from(feat[i]), want_feat, "tile {i}'s feature");
    }
    let cities: Vec<Value> = g
        .state()
        .cities()
        .iter()
        .map(|c| {
            let capital = g.player(c.owner()).is_some_and(|p| p.capital == Some(c.id()));
            json!([c.id().get(), &*c.name, c.owner().0, c.tile().0, c.pop, capital])
        })
        .collect();
    assert_eq!(frame["cities"], json!(cities), "the cities");
    let units: Vec<Value> = g
        .state()
        .units()
        .iter()
        .map(|u| json!([r.name(u.base), u.owner().0, u.tile().0, u.hp]))
        .collect();
    assert_eq!(frame["units"], json!(units), "the units");
    for p in g.majors(false) {
        let bytes = b64(&frame["explored"][p.id().0.to_string()]);
        let want: Vec<u8> =
            (0..g.grid().size()).map(|i| u8::from(p.explored.contains(i))).collect();
        assert_eq!(bytes, want, "what {:?} has explored", p.id());
    }
}

// ---- Gate 3: replay data ---------------------------------------------------------------------------------

#[test]
fn the_replay_holds_the_frames_each_round_captured_in_both_formats() {
    let mut g = arena_game();
    records::capture_frames_for_test(true);
    play_out(&mut g);
    let captured = records::take_frames_for_test();
    records::capture_frames_for_test(false);
    assert!(captured.len() >= 99, "a frame a round: {}", captured.len());

    let full: Value = serde_json::from_slice(&g.replay_data(ReplayFormat::Full)).expect("JSON");
    assert_eq!(full, g.replay_json(ReplayFormat::Full), "the bytes are the value");
    assert_eq!(full["format"], json!("full"));
    let units: Vec<Box<str>> =
        g.rules().base_units().as_slice().iter().map(|u| u.name.clone()).collect();
    let frames = full["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), captured.len(), "a frame for each round");
    for (got, want) in frames.iter().zip(&captured) {
        assert_eq!(got, &python_frame(want, &units), "the frame of turn {}", want.turn);
    }
    let last = frames.last().expect("a frame");
    check_against_the_state(&g, &full, last);

    // The rest of what the recap reads.
    let chron = g.chronicle();
    assert_eq!(full["events"].as_array().map(Vec::len), Some(chron.events().len()));
    assert_eq!(full["stats"].as_array().map(Vec::len), Some(chron.stats().len()));
    assert_eq!(full["terrain"].as_array().map(Vec::len), Some(g.grid().size() as usize));
    assert_eq!(full["players"].as_array().map(Vec::len), Some(g.state().players().len()));
    assert_eq!(full["phase"], json!("over"));
    let row = &full["stats"][0]["players"]["0"];
    assert!(row["score"].is_number() && row["alive"] == json!(true), "{row}");

    // Delta: the records as stored, which decode back to the captured frames.
    let delta = g.replay_json(ReplayFormat::Delta);
    assert_eq!(delta["format"], json!("delta"));
    let recs: Vec<FrameRecord> = delta["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|f| FrameRecord {
            turn: i32::try_from(f["turn"].as_i64().expect("a turn")).expect("a turn"),
            keyframe: f["keyframe"].as_bool().expect("a kind"),
            bytes: b64(&f["bytes"]).into_boxed_slice(),
        })
        .collect();
    assert_eq!(recs.as_slice(), g.chronicle().frames().frames.as_slice(), "the records as stored");
    assert!(recs.iter().filter(|r| r.keyframe).count() < recs.len() / 4, "mostly deltas");
    let mut dec = FrameDecoder::new();
    for (rec, want) in recs.iter().zip(&captured) {
        let got = dec.apply(rec).unwrap_or_else(|e| panic!("turn {}: {e}", rec.turn));
        assert!(got == *want, "the delta of turn {} decodes to the captured frame", rec.turn);
    }
    let delta_bytes = g.replay_data(ReplayFormat::Delta).len();
    let full_bytes = g.replay_data(ReplayFormat::Full).len();
    assert!(delta_bytes < full_bytes, "deltas are smaller: {delta_bytes} against {full_bytes}");
}

// ---- Reads ------------------------------------------------------------------------------------------------

/// Every view of a played game, a player's and a spectator's, the query tools among them, leaves
/// the game as it was; and the bytes are the view.
#[test]
fn views_change_nothing_and_their_bytes_are_the_view() {
    let mut g = arena_game();
    let mut agents = [RandomAgent::new(), RandomAgent::new(), RandomAgent::new()];
    for _ in 0..40 {
        let pid = g.current();
        let mut memory = DriverMemory::empty(0, 0);
        let Some(agent) = agents.get_mut(usize::from(pid.0)) else { panic!("{pid:?} plays") };
        assert_eq!(agent.play_turn(&mut g, pid, &mut memory), DriverOutcome::Done);
        g.end_turn(pid).expect("the turn ends");
    }
    clean(&mut g);
    let before = (g.digest().expect("a digest"), g.rev(), g.chronicle().events().len());
    for viewer in [None, Some(PlayerId(0)), Some(PlayerId(1)), Some(PlayerId(2))] {
        let bytes = g.view_json(viewer, 150);
        let parsed: Value = serde_json::from_slice(&bytes).expect("JSON");
        let typed = serde_json::to_value(g.client_view(viewer, 150)).expect("a value");
        assert_eq!(parsed, typed, "{viewer:?}");
        assert_eq!(parsed["you"], json!(viewer.map(|p| p.0)));
        let events = parsed["events"].as_array().expect("events");
        assert!(events.len() <= 150 && !events.is_empty(), "{viewer:?}: {} events", events.len());
        if let Some(p) = viewer {
            // A player's map is what it has explored; a spectator's is all of it.
            let explored = g.player(p).map_or(0, |x| x.explored.len());
            assert_eq!(parsed["tiles"].as_array().map(Vec::len), Some(explored));
            for q in
                citar_engine::api::tools::schemas(Some(citar_engine::api::tools::ToolKind::Query))
                    .iter()
                    .filter_map(|t| t["name"].as_str())
            {
                let _answer =
                    g.execute_query(p, q, &json!({"unit_id": 1, "city_id": 1, "x": 3, "y": 3}));
            }
            assert!(parsed["alerts"].is_array() && parsed["empire"]["score"].is_number());
        } else {
            assert_eq!(parsed["tiles"].as_array().map(Vec::len), Some(g.grid().size() as usize));
            assert!(parsed["empires"].is_object() && parsed["alerts"].is_null());
        }
    }
    // A player the game does not have sees nothing, and nothing breaks.
    let nobody: Value =
        serde_json::from_slice(&g.view_json(Some(PlayerId(60)), 150)).expect("JSON");
    assert_eq!(nobody["tiles"], json!([]));
    assert_eq!(nobody["units"], json!([]));
    assert_eq!(
        nobody["events"].as_array().map(Vec::len),
        g.chronicle().events().iter().filter(|e| e.audience.is_none()).count().min(150).into()
    );
    let _replay = g.replay_data(ReplayFormat::Full);
    let _standings = g.standings();
    let _summary = g.empire_summary(PlayerId(0));
    let _path =
        g.path_preview(PlayerId(0), citar_engine::base::ids::UnitId::new(1).expect("an id"), 5, 5);
    assert_eq!(
        (g.digest().expect("a digest"), g.rev(), g.chronicle().events().len()),
        before,
        "reads change nothing"
    );
    clean(&mut g);
}
