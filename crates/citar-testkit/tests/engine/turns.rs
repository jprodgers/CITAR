//! Turn flow and new-game setup (package 1b-03):
//! - a bare arena game with three majors plays 50 `end_turn` calls with every check clean, and
//!   `inspect` lists the stages that wait for their packages (gate 1);
//! - `config_from_json` refuses a missing seed and a map id, with a message a model can read
//!   (gate 2);
//! - `drive` with a `RandomAgent` in every major seat reaches the turn limit of a small arena
//!   game (gate 3);
//! - whose turn it is and how it passes: city-states and the barbarians play inside the call,
//!   the round ends after the last player, a refusal changes nothing, a forced turn;
//! - `Game::new` on an editor map: the settings, the nations, the players and their seats, the
//!   starting techs, the first turn; and what it refuses;
//! - a chain of round digests, and a driver's memory kept in the save.

use citar_engine::api::{ErrCode, inspect, testops};
use citar_engine::base::ids::{NationId, NegotiationId, PlayerId, TechId};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    DebugOptions, DriveOptions, DriverOutcome, Drivers, EngineError, Game, SeatDriver, Stop,
};
use citar_engine::rules::Ruleset;
use citar_engine::save::chain::DigestChain;
use citar_engine::state::Phase;
use citar_engine::state::config::MapSource;
use citar_engine::state::players::DriverMemory;
use citar_testkit::agents::RandomAgent;
use citar_testkit::rulesets;
use citar_testkit::script::map_doc;
use serde_json::{Map, Value, json};

/// Settings on the arena with `extra` on top, as a lobby sends them.
fn settings(extra: &Value) -> Map<String, Value> {
    let (doc, _) = map_doc("arena").expect("the arena");
    let mut cfg = json!({"seed": 11, "map": doc}).as_object().cloned().unwrap_or_default();
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg.insert(k.clone(), v.clone());
    }
    cfg
}

fn new_game(rules: &'static Ruleset, extra: &Value) -> Result<(Game, Vec<String>), EngineError> {
    let setup = config_from_value(rules, Value::Object(settings(extra)))?;
    let (mut g, batch) = Game::new(rules, &setup)?;
    g.set_debug_options(DebugOptions::ALL);
    let kinds = batch.events().iter().map(|e| e.kind.name().to_owned()).collect();
    Ok((g, kinds))
}

/// `n` seats of the benchmark civilization, named by number.
fn seats(n: usize) -> Value {
    Value::Array(vec![json!({"nation": "BenchmarkCiv"}); n])
}

fn game(extra: &Value) -> Game {
    new_game(Ruleset::shared(), extra).unwrap_or_else(|e| panic!("{e}")).0
}

/// Every check clean: nothing reported since the last look, the invariants and the caches.
fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// Model-readable text (property P5): not empty, at most 600 characters, a sentence's end, and
/// no Rust debug artefacts.
fn readable(text: &str) {
    assert!(!text.is_empty() && text.chars().count() <= 600, "{text}");
    assert!(text.ends_with(['.', '?', ')']), "{text}");
    for bad in ["Some(", "None", "Idx(", "::"] {
        assert!(!text.contains(bad), "{text}");
    }
}

#[test]
fn a_bare_arena_game_plays_fifty_end_turns_cleanly_and_lists_what_waits() {
    let mut g = game(&json!({
        "players": seats(3),
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
    }));
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    clean(&mut g);
    let mut seen = Vec::new();
    for _ in 0..50 {
        let who = g.current();
        seen.push((g.turn(), who.0));
        let batch = g.end_turn(who).expect("its turn");
        let kinds: Vec<&str> = batch.events().iter().map(|e| e.kind.name()).collect();
        assert_eq!(kinds, ["turn_end", "turn_start"], "one turn closes and the next opens");
        clean(&mut g);
    }
    // Three majors in turn, round after round: 50 calls are 16 rounds and two turns.
    assert_eq!(&seen[..4], [(1, 0), (1, 1), (1, 2), (2, 0)]);
    assert_eq!((g.turn(), g.current()), (17, PlayerId(2)));
    assert_eq!(g.phase(), Phase::Playing);

    let pending = inspect::inspect(&g, &json!({"what": "pending"})).expect("the list");
    let stages: Vec<&str> = pending
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"] == "turn_stage")
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(stages.contains(&"player_start S5: cities start their turn"));
    assert!(stages.contains(&"player_end E4: cities end their turn, razing ones first"));
    assert!(stages.contains(&"round_end R2: the round's statistics"));
}

#[test]
fn settings_without_a_seed_or_with_a_map_id_are_refused_readably() {
    let r = Ruleset::shared();
    let refuse = |cfg: Value| match Game::config_from_json(r, cfg.to_string().as_bytes()) {
        Err(EngineError::Config(m)) => m,
        other => panic!("refused as a config error, not {other:?}"),
    };
    let no_seed = refuse(json!({"players": [{}, {}]}));
    assert!(no_seed.contains("need a seed"), "{no_seed}");
    readable(&no_seed);
    let null_seed = refuse(json!({"seed": null}));
    assert_eq!(null_seed, no_seed, "null is no seed, as Python's None was");
    let bad_seed = refuse(json!({"seed": "twelve"}));
    assert!(bad_seed.starts_with("seed must be a whole number"), "{bad_seed}");
    readable(&bad_seed);
    let by_id = refuse(json!({"seed": 4, "map": "twin-islands"}));
    assert!(by_id.contains("must come inline") && by_id.contains("'twin-islands'"), "{by_id}");
    readable(&by_id);
    assert!(matches!(
        Game::config_from_json(r, b"{\"seed\": "),
        Err(EngineError::Config(m)) if m.starts_with("The settings are not valid JSON")
    ));
}

#[test]
fn settings_that_name_nothing_are_refused_naming_what_is_valid() {
    let r = Ruleset::shared();
    let refuse = |extra: Value| {
        let e = config_from_value(r, Value::Object(settings(&extra))).expect_err("refused");
        let text = e.to_string();
        readable(&text);
        text
    };
    // refcheck: config-refuses-unknown-names
    assert!(refuse(json!({"speed": "Warp"})).starts_with("Unknown speed 'Warp'. Known: Quick"));
    assert!(refuse(json!({"difficulty": "Easy"})).starts_with("Unknown difficulty 'Easy'."));
    assert!(refuse(json!({"victories": {"Space": false}})).starts_with("Unknown victory 'Space'."));
    assert!(refuse(json!({"barbarians": "wild"})).starts_with("Unknown barbarians 'wild'."));
    assert!(
        refuse(json!({"players": [{"controller": "robot"}]}))
            .starts_with("Unknown controller 'robot'.")
    );
    assert!(refuse(json!({"players": [{"nation": "Atlantis"}]})).starts_with("Unknown nation"));
    assert!(refuse(json!({"players": [{"nation": "Kabul"}]})).contains("not a civilization"));
    let seats = Value::Array(vec![json!({}); 25]);
    assert_eq!(refuse(json!({"players": seats})), "Games support 1 to 24 players.");
    assert_eq!(
        refuse(json!({"players": [{"handicap": "deity"}]})),
        "handicap must be 'human' or 'ai', not 'deity'."
    );
    assert_eq!(
        refuse(json!({"barbarian_aggression": "lots"})),
        "barbarian_aggression must be a number from 0 to 100."
    );
    // Loose names resolve, and an empty one is the default, as Python read them.
    let ok = config_from_value(
        r,
        Value::Object(settings(
            &json!({"speed": "quick", "difficulty": "", "victories": {"science": false}}),
        )),
    )
    .expect("loose names");
    assert_eq!(r.name(ok.config().speed), Some("Quick"));
    assert_eq!(ok.config().difficulty, r.constants().default_difficulty);
    assert_eq!(ok.config().disabled_victories.len(), 1);
}

#[test]
fn a_new_game_on_the_arena_starts_at_the_first_players_turn() {
    let r = Ruleset::shared();
    let (g, kinds) = new_game(
        r,
        &json!({"players": [{"nation": "BenchmarkCiv"}, {"nation": "Babylon", "name": "Ur"}, {}],
                "city_states": 1, "barbarians": "normal", "on_disconnect": "skip"}),
    )
    .expect("a game");
    assert_eq!(kinds, ["turn_start", "game_start"], "Python's order: the turn, then the world");
    let start = &g.chronicle().events()[1];
    assert_eq!(&*start.text, "The world begins. 3 civilizations and 1 city-states stir.");
    assert_eq!((g.turn(), g.current(), g.phase()), (1, PlayerId(0), Phase::Playing));
    assert!(g.state().clock().turn_started);
    let names: Vec<&str> = g.state().players().iter().map(|(_, p)| &*p.name).collect();
    assert_eq!(names[..2], ["Civilization 1", "Ur"]);
    assert!(g.player(PlayerId(3)).is_some_and(|p| p.is_city_state()));
    assert!(g.player(PlayerId(4)).is_some_and(|p| p.is_barbarian()));
    let nation = |p: u8| g.player(PlayerId(p)).map(|x| x.nation).expect("a player");
    assert_eq!(Some(nation(1)), r.lookup::<NationId>("Babylon"));
    assert!(!r.nations()[nation(2)].benchmark, "a seat naming none draws a civilization");
    // Every seat starts with the first era's starting techs; majors play the game's difficulty.
    let agriculture = r.lookup::<TechId>("Agriculture");
    for p in 0..4 {
        assert!(g.has_tech(PlayerId(p), agriculture), "player {p}");
    }
    assert!(!g.has_tech(PlayerId(4), agriculture), "the barbarians know nothing");
    let seat = g.player(PlayerId(0)).map(|p| p.seat().difficulty());
    assert_eq!(seat, Some(Some(g.state().config().difficulty)));
    // The host's own settings are kept, and so are the seats as the lobby sent them.
    let host = &g.state().config().host;
    assert_eq!(host.get("on_disconnect"), Some(&json!("skip")));
    assert_eq!(host.get("players").and_then(Value::as_array).map(Vec::len), Some(3));
    assert!(matches!(&g.state().config().map, MapSource::Editor { id, .. } if &**id == "arena"));
    // The continents: the arena is one landmass.
    let land = g.grid().idx(5, 5).expect("A");
    assert_eq!(g.continent(land), Some(0));
}

#[test]
fn the_nations_drawn_follow_the_seed() {
    let r = Ruleset::shared();
    let drawn = |seed: u64| {
        let (g, _) =
            new_game(r, &json!({"seed": seed, "players": [{}, {}, {}, {}], "city_states": 1}))
                .expect("a game");
        g.state().players().iter().map(|(_, p)| p.nation).collect::<Vec<_>>()
    };
    assert_eq!(drawn(5), drawn(5));
    assert_ne!(drawn(5), drawn(6));
    let n = drawn(5);
    for (i, a) in n[..4].iter().enumerate() {
        assert!(!n[i + 1..4].contains(a), "no nation twice");
    }
}

#[test]
fn a_civilization_that_starts_with_a_tech_has_it() {
    let r = Ruleset::shared();
    let (g, _) =
        new_game(r, &json!({"players": [{"nation": "The Huns"}, {"nation": "BenchmarkCiv"}]}))
            .expect("a game");
    let husbandry = r.lookup::<TechId>("Animal Husbandry");
    assert!(g.has_tech(PlayerId(0), husbandry), "Starts with [Animal Husbandry]");
    assert!(!g.has_tech(PlayerId(1), husbandry));
}

#[test]
fn what_setup_cannot_do_yet_is_refused_as_not_ported() {
    let r = Ruleset::shared();
    let generated = Game::config_from_json(r, br#"{"seed": 1}"#).expect("settings");
    match Game::new(r, &generated) {
        Err(EngineError::Action(e)) => {
            assert_eq!(e.code, ErrCode::NotPorted);
            assert!(e.message.contains("mapgen::generate"), "{}", e.message);
        }
        other => panic!("not ported, not {:?}", other.map(|_| ())),
    }
    // The arena gives five starts: a sixth seat needs start filling (package 1c-09).
    let six = new_game(r, &json!({"players": [{}, {}, {}, {}, {}, {}]}));
    assert!(matches!(six, Err(EngineError::Action(e)) if e.code == ErrCode::NotPorted));
    // A document the engine cannot read.
    let (mut doc, _) = map_doc("arena").expect("the arena");
    doc["tiles"][0][0] = json!("Nowhere");
    let bad = new_game(r, &json!({"map": doc}));
    assert!(
        matches!(bad, Err(EngineError::Map(m)) if m == "Tile (0,0) has unknown base terrain 'Nowhere'.")
    );
}

#[test]
fn city_states_and_barbarians_play_inside_a_major_civilizations_end_turn() {
    let mut g = game(&json!({"players": seats(2), "city_states": 1, "barbarians": "normal"}));
    assert_eq!(g.state().players().len(), 4);
    g.end_turn(PlayerId(0)).expect("its turn");
    assert_eq!((g.turn(), g.current()), (1, PlayerId(1)));
    let before = g.chronicle().events().len();
    let batch = g.end_turn(PlayerId(1)).expect("its turn");
    // The city-state and the barbarians played, the round ended, and the first major's turn
    // began: they announce nothing of their own.
    assert_eq!((g.turn(), g.current()), (2, PlayerId(0)));
    let kinds: Vec<&str> = batch.events().iter().map(|e| e.kind.name()).collect();
    assert_eq!(kinds, ["turn_end", "turn_start"]);
    assert_eq!(g.chronicle().events().len(), before + 2);
    assert_eq!(&*batch.events()[1].text, "Turn 2 (3960 BC): Civilization 1's turn.");
    clean(&mut g);
}

#[test]
fn a_turn_that_is_not_yours_is_refused_and_changes_nothing() {
    let mut g = game(&json!({"players": seats(2)}));
    let before = (g.digest().ok(), g.rev(), g.chronicle().events().len());
    let e = g.end_turn(PlayerId(1)).expect_err("not its turn");
    assert_eq!(e.code, ErrCode::NotYourTurn);
    assert_eq!(e.message, "It is not your turn (it is Civilization 1's turn).");
    readable(&e.message);
    assert_eq!((g.digest().ok(), g.rev(), g.chronicle().events().len()), before);
}

#[test]
fn a_forced_turn_starts_at_once_and_play_goes_on_from_there() {
    let mut g = game(&json!({"players": seats(3)}));
    let out = testops::apply(&mut g, &json!([{"op": "force_turn", "player": 2}])).expect("forced");
    assert_eq!(out.0, [json!({"turn": 1, "current": 2})]);
    assert_eq!(
        out.1.events().iter().map(|e| &*e.text).collect::<Vec<_>>(),
        ["Turn 1 (4000 BC): Civilization 3's turn."]
    );
    let out = testops::apply(&mut g, &json!([{"op": "end_turn"}])).expect("ended");
    assert_eq!(out.0, [json!({"turn": 2, "current": 0})]);
    let out = testops::apply(&mut g, &json!([{"op": "end_round"}])).expect("the round");
    assert_eq!(out.0, [json!({"turn": 3, "current": 0})]);
    let e = testops::apply(&mut g, &json!([{"op": "end_turn", "player": 1}])).expect_err("not 1's");
    assert_eq!(e.code, ErrCode::NotYourTurn);
    assert!(e.message.starts_with("Test operation 1 (end_turn): It is not your turn"));
    clean(&mut g);
}

#[test]
fn the_game_ends_at_its_turn_limit() {
    let mut g = game(&json!({"players": [{}, {}], "turn_limit": 3}));
    let mut last = Vec::new();
    while g.phase() == Phase::Playing {
        last = g.end_turn(g.current()).expect("a turn").events().to_vec();
        assert!(g.turn() <= 4, "the game ends after turn 3");
    }
    assert_eq!(g.turn(), 4);
    let over: Vec<&str> = last.iter().map(|e| e.kind.name()).collect();
    assert_eq!(over, ["turn_end", "game_over"]);
    let e = g.end_turn(g.current()).expect_err("over");
    assert_eq!((e.code, e.message.as_str()), (ErrCode::GameOver, "The game is over."));
    // Nor is a turn forced, which Python allowed (refcheck: force-turn-only-for-the-living).
    let before = (g.digest().ok(), g.rev(), g.current(), g.state().clock().turn_started);
    let e = g.force_turn(PlayerId(0)).expect_err("over");
    assert_eq!((e.code, e.message.as_str()), (ErrCode::GameOver, "The game is over."));
    assert_eq!((g.digest().ok(), g.rev(), g.current(), g.state().clock().turn_started), before);
    clean(&mut g);
}

#[test]
fn drive_with_a_random_agent_in_every_major_seat_reaches_the_turn_limit() {
    let mut g = game(
        &json!({"players": [{}, {}, {}], "city_states": 1, "barbarians": "normal", "turn_limit": 12}),
    );
    let (mut a, mut b, mut c) = (RandomAgent::new(), RandomAgent::new(), RandomAgent::new());
    let mut d = Drivers::none(g.state().players().len())
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b)
        .with(PlayerId(2), &mut c);
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::GameOver);
    assert_eq!((g.phase(), g.turn()), (Phase::Over, 13));
    assert_eq!([a.turns(), b.turns(), c.turns()], [12, 12, 12]);
    let starts = batch.events().iter().filter(|e| e.kind.name() == "turn_start").count();
    assert_eq!(starts, 3 * 12 - 1, "every turn but the first, which setup began");
    clean(&mut g);
    // A game that is over drives no further.
    let mut d = Drivers::none(g.state().players().len());
    assert_eq!(g.drive(&mut d, DriveOptions::default()).map(|(s, _)| s), Ok(Stop::GameOver));
}

#[test]
fn drive_stops_at_a_seat_the_host_plays() {
    let mut g = game(&json!({"players": [{}, {"controller": "llm"}, {}]}));
    let mut bot = RandomAgent::new();
    let mut d = Drivers::none(3).with(PlayerId(0), &mut bot);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    assert_eq!((g.turn(), g.current()), (1, PlayerId(1)));
    // Again at once: the seat is still the host's.
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    assert!(batch.is_empty());
}

/// A driver that keeps a count of its turns in the seat's memory.
struct Counter;

impl SeatDriver for Counter {
    fn play_turn(&mut self, _: &mut Game, _: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        let n = mem.bytes().first().copied().unwrap_or(0);
        *mem = DriverMemory::new(7, 1, vec![n + 1]).expect("a byte");
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        DriverOutcome::Done
    }
}

#[test]
fn a_drivers_memory_is_handed_back_each_turn_and_kept_in_the_save() {
    let mut g = game(&json!({"players": [{}, {}], "turn_limit": 4}));
    let (mut x, mut y) = (Counter, RandomAgent::new());
    let before = g.digest().ok();
    let mut d = Drivers::none(2).with(PlayerId(0), &mut x).with(PlayerId(1), &mut y);
    g.drive(&mut d, DriveOptions::default()).expect("a live game");
    let mem = g.player(PlayerId(0)).and_then(|p| p.seat().driver()).cloned();
    assert_eq!(mem.as_ref().map(|m| (m.kind(), m.bytes().to_vec())), Some((7, vec![4])));
    assert!(g.player(PlayerId(1)).is_some_and(|p| p.seat().driver().is_none()), "kept nothing");
    assert_ne!(g.digest().ok(), before);
    // A save keeps it.
    let json = g.snapshot().to_json().expect("a save");
    let chunk = g.take_journal_chunk().expect("history").map(|c| c.json).unwrap_or_default();
    let (loaded, _) =
        Game::load(g.rules(), &json, &mut std::iter::once(chunk.as_slice())).expect("it loads");
    assert_eq!(loaded.player(PlayerId(0)).and_then(|p| p.seat().driver()).cloned(), mem);
    assert_eq!(loaded.digest().ok(), g.digest().ok());
}

#[test]
fn a_chained_game_folds_in_every_rounds_digest_and_resumes_across_a_save() {
    let play = |g: &mut Game, rounds: i32| {
        let end = g.turn() + rounds;
        while g.turn() < end {
            g.end_turn(g.current()).expect("a turn");
        }
    };
    let mut whole = game(&json!({"players": [{}, {}]}));
    whole.set_chain(Some(DigestChain::new(b"test")));
    play(&mut whole, 6);
    let head = whole.chain().copied().expect("a chain");
    assert_eq!(head.rounds(), 6);
    assert!(whole.last_round_digest().is_some());
    assert_eq!(whole.last_round().map(|(round, _)| round), Some(6), "under its own turn");

    let mut first = game(&json!({"players": [{}, {}]}));
    first.set_chain(Some(DigestChain::new(b"test")));
    play(&mut first, 3);
    let kept = first.chain().copied().expect("a chain");
    let json = first.snapshot().to_json().expect("a save");
    let chunk = first.take_journal_chunk().expect("history").map(|c| c.json).unwrap_or_default();
    let (mut second, _) =
        Game::load(first.rules(), &json, &mut std::iter::once(chunk.as_slice())).expect("loads");
    second.set_chain(Some(DigestChain::resume(kept.head(), kept.rounds())));
    play(&mut second, 3);
    assert_eq!(second.chain().copied(), Some(head));
}

#[test]
fn the_kitchen_sink_sets_up_and_plays() {
    let r = rulesets::kitchen_sink();
    let (mut g, _) = new_game(
        r,
        &json!({"players": [{"nation": "Kitchen Sink"}, {"nation": "BenchmarkCiv"}], "city_states": 1, "turn_limit": 5}),
    )
    .expect("a game");
    // Its `upon turn start` and `upon turn end` triggers fire once one-time effects and the civ
    // index exist (packages 1b-05 and 1b-08); the turns play meanwhile.
    while g.phase() == Phase::Playing {
        g.end_turn(g.current()).expect("a turn");
    }
    assert_eq!(g.turn(), 6);
    clean(&mut g);
}

#[test]
fn the_host_forces_a_turn_on_a_player_the_game_has() {
    let mut g = game(&json!({"players": seats(2)}));
    let batch = g.force_turn(PlayerId(1)).expect("a player");
    assert_eq!(batch.events().len(), 1);
    assert_eq!(g.current(), PlayerId(1));
    let e = g.force_turn(PlayerId(9)).expect_err("no such player");
    assert_eq!((e.code, e.message.as_str()), (ErrCode::InvalidPlayer, "No player 9."));
    clean(&mut g);
}
