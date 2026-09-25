//! Whole games (package 1c-10, DESIGN.md 9.5):
//! - every fixture plays five rounds of passing with no panic and no invariant broken, and the
//!   caches agree with a cold rebuild at every settle (gate 1; the local corpus too when
//!   `CITAR_REFCHECK_CORPUS` names it);
//! - a small game of `RandomAgent`s reaches its turn limit, 330 turns, in 10 seconds or less
//!   (gate 2: the time is reported, and fails above the 30-second backstop in any build);
//! - the chain of round digests with a save and a load at every round equals the uninterrupted
//!   run's (gate 3), for random games and for a fixture passed;
//! - reads, saves and refused calls between every two of the agents' moves change no digest
//!   (the early property P8);
//! - the agent tries every action a seat has, and the game carries out most of them.

use citar_engine::api::tools::args::TOOLS;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::Phase;
use citar_testkit::agents::{RandomAgent, take_tally};
use citar_testkit::fixtures::{self, Fixture};
use citar_testkit::games::{self, Round};

/// The committed fixtures, and the local corpus's when `CITAR_REFCHECK_CORPUS` names it.
fn every_fixture() -> Vec<Fixture> {
    let mut all = fixtures::committed().expect("the committed fixtures");
    assert_eq!(all.len(), 12, "the twelve committed fixtures");
    all.extend(fixtures::corpus().expect("the corpus folder").unwrap_or_default());
    all
}

/// Plays a random game on generated settings, calling `hook` after every round, and returns
/// the rounds and the chain's head.
fn random_game(
    size: &str,
    map_type: &str,
    seed: u64,
    rounds: u32,
    debug: DebugOptions,
    noisy: bool,
    hook: &mut games::Hook<'_>,
) -> (Game, u32) {
    let settings = games::random_settings(size, map_type, "wrap_x", seed, rounds + 5);
    let mut g = games::new_game(&settings, b"whole-game", debug).expect("a game");
    let mut agents = if noisy {
        vec![RandomAgent::noisy(); g.state().players().len()]
    } else {
        games::agents_for(&g)
    };
    let played = games::play_random(&mut g, &mut agents, rounds, hook).expect("it plays");
    (g, played)
}

#[test]
fn every_fixture_plays_five_pass_rounds_cleanly() {
    // Gate 1. The committed fixtures verify every cache at every settle; the corpus, which is
    // twenty times as many, checks the invariants there and the caches once it has passed.
    let mut failures = Vec::new();
    for f in every_fixture() {
        let committed = !f.path.to_string_lossy().contains("corpus");
        let debug = if committed { DebugOptions::ALL } else { DebugOptions::default() };
        let mut g = match games::from_fixture(&f, b"pass", debug) {
            Ok(g) => g,
            Err(e) => {
                failures.push(format!("{}: does not load: {e}", f.name));
                continue;
            }
        };
        g.set_debug_options(DebugOptions { invariants: true, ..debug });
        let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            games::pass_rounds(&mut g, 5, &mut |_, _| Ok(()))
        }));
        match passed {
            Err(_) => failures.push(format!("{}: panicked", f.name)),
            Ok(Err(e)) => failures.push(format!("{}: {e}", f.name)),
            Ok(Ok(n)) if n < 5 && g.phase() == Phase::Playing => {
                failures.push(format!("{}: only {n} rounds", f.name));
            }
            Ok(Ok(_)) => {}
        }
        failures.extend(games::problems(&mut g).into_iter().map(|p| format!("{}: {p}", f.name)));
        failures.extend(g.verify_caches().into_iter().map(|p| format!("{}: {p}", f.name)));
    }
    assert!(failures.is_empty(), "{} problems:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn a_random_agent_small_game_reaches_its_turn_limit_in_time() {
    // Gate 2 (DESIGN.md 10): an engine-only small game of 330 turns, RandomAgent in every seat,
    // plays to its turn limit in 10 seconds or less. The time is reported, and any build fails
    // above the 30-second backstop: it takes 1 to 2 s here, in the dev profile as in the ci
    // one. The checks are off, as a shipped build runs.
    let settings = games::random_settings("small", "continents", "wrap_x", 330, 330);
    let mut g = games::new_game(&settings, b"small-330", DebugOptions::OFF).expect("a game");
    let mut agents = games::agents_for(&g);
    #[allow(
        clippy::disallowed_types,
        reason = "the gate is a wall-clock time; the game never sees it"
    )]
    let start = std::time::Instant::now();
    let played =
        games::play_random(&mut g, &mut agents, 400, &mut |_, _| Ok(())).expect("it plays");
    let took = start.elapsed();
    #[allow(clippy::disallowed_macros, reason = "the gate's time is reported")]
    {
        let st = g.state();
        println!(
            "small game: {played} rounds to turn {} in {:.2} s; {} cities and {} units at the end",
            g.turn(),
            took.as_secs_f64(),
            st.cities().len(),
            st.units().len()
        );
    }
    assert_eq!(g.phase(), Phase::Over);
    assert_eq!(played, 330, "it reached its turn limit");
    assert!(g.state().clock().winner.is_some(), "the Time victory has a winner");
    assert!(took.as_secs_f64() <= 30.0, "330 turns took {took:?}, above the 30 s backstop");
    g.set_debug_options(DebugOptions::ALL);
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// The rounds of a random game played straight through, with the checks `straight` asks for at
/// every settle, and with a save and a load after every round.
fn both_ways(size: &str, map_type: &str, seed: u64, rounds: u32, straight: DebugOptions) {
    let mut whole = Vec::new();
    let mut problems = Vec::new();
    let mut hook = |g: &mut Game, r: Round| {
        whole.push(r);
        problems.extend(games::problems(g));
        Ok(())
    };
    let (g, n) = random_game(size, map_type, seed, rounds, straight, false, &mut hook);
    assert_eq!(n, rounds);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
    let straight = (g.chain().copied(), g.digest().ok(), g.chronicle().events().len());

    let mut again: Vec<Round> = Vec::new();
    let mut chunks = Vec::new();
    let mut hook = |g: &mut Game, r: Round| {
        again.push(r);
        games::save_and_load(g, &mut chunks)
    };
    let (g, n) =
        random_game(size, map_type, seed, rounds, DebugOptions::default(), false, &mut hook);
    assert_eq!(n, rounds);
    assert_eq!(again.len(), whole.len());
    for (a, b) in whole.iter().zip(&again) {
        assert_eq!(a, b, "the round of turn {} differs after a save and a load", a.0);
    }
    let reloaded = (g.chain().copied(), g.digest().ok(), g.chronicle().events().len());
    assert_eq!(reloaded, straight, "the same chain, state and history");
}

#[test]
fn a_duel_saved_and_loaded_every_round_chains_as_the_uninterrupted_run() {
    // Gate 3. The uninterrupted run also checks every invariant and every cache at every settle,
    // which only read: the chain is the same.
    both_ways("duel", "continents", 31, 120, DebugOptions::ALL);
}

#[test]
fn a_small_game_saved_and_loaded_every_round_chains_as_the_uninterrupted_run() {
    // Gate 3, with four civilizations, city-states and more of everything.
    both_ways("small", "pangaea", 32, 60, DebugOptions::default());
}

#[test]
fn the_kitchen_sink_plays_random_games_with_every_check_and_a_save_every_round() {
    // The kitchen sink's extra types (package 1a-05b) in whole games: a civilization of the
    // Kitchen Sink nation, whose buildings, units, promotions, beliefs and improvements carry
    // them, and random agents everywhere, with every check at every settle and the game saved
    // and loaded after every round.
    let r = citar_testkit::rulesets::kitchen_sink();
    for (size, seed, rounds) in [("duel", 2, 60), ("small", 3, 30)] {
        let mut settings = games::random_settings(size, "continents", "wrap_x", seed, rounds + 5);
        settings["players"][0]["nation"] = serde_json::json!("Kitchen Sink");
        let cfg = settings.as_object().cloned().unwrap_or_default();
        let mut g = citar_testkit::script::new_game(r, &cfg).expect("a game");
        g.set_debug_options(DebugOptions::ALL);
        g.set_chain(Some(citar_engine::save::chain::DigestChain::new(b"kitchen-sink")));
        let mut agents = games::agents_for(&g);
        let mut problems = Vec::new();
        let mut chunks = Vec::new();
        let mut hook = |g: &mut Game, _: Round| {
            problems.extend(games::problems(g));
            games::save_and_load(g, &mut chunks)
        };
        let n = games::play_random(&mut g, &mut agents, rounds, &mut hook).expect("it plays");
        assert_eq!(n, rounds);
        assert!(problems.is_empty(), "{size}: {problems:?}");
        assert!(g.verify_caches().is_empty(), "{size}: {:?}", g.verify_caches());
    }
}

#[test]
fn a_fixture_saved_and_loaded_every_round_passes_as_it_would_have() {
    // Gate 3 on a Python state, whose history and caches came from the converter.
    let f = fixtures::committed()
        .expect("the fixtures")
        .into_iter()
        .find(|f| f.name == "standard-pangaea-normal-s1031/t120")
        .expect("the fixture");
    let play = |save: bool| {
        let mut g = games::from_fixture(&f, b"pass", DebugOptions::default()).expect("loads");
        let mut rounds: Vec<Round> = Vec::new();
        let mut chunks = Vec::new();
        let mut hook = |g: &mut Game, r: Round| {
            rounds.push(r);
            if save { games::save_and_load(g, &mut chunks) } else { Ok(()) }
        };
        games::pass_rounds(&mut g, 10, &mut hook).expect("it passes");
        (rounds, g.chain().copied(), g.digest().ok())
    };
    assert_eq!(play(true), play(false));
}

#[test]
fn reads_saves_and_refusals_between_moves_change_no_digest() {
    // The early property P8: a noisy agent reads the game (every inspect query, the views and
    // the briefing among them), saves it and makes calls it refuses before every move and
    // every answer; the hook does too after every round. Every round's digest is the quiet
    // game's.
    let mut quiet = Vec::new();
    let (g, n) = random_game(
        "small",
        "fractal",
        33,
        50,
        DebugOptions::default(),
        false,
        &mut games::keep(&mut quiet),
    );
    assert_eq!(n, 50);
    let ends = (g.chain().copied(), g.digest().ok());

    let mut noisy = Vec::new();
    let mut hook = |g: &mut Game, r: Round| {
        noisy.push(r);
        let mut rng = citar_engine::base::rng::Rng::keyed(
            g.state().seed(),
            citar_engine::base::rng::Purpose::TestAgent,
            &[u64::MAX, u64::try_from(r.0).unwrap_or(0)],
        );
        let p = g.current();
        for _ in 0..5 {
            citar_testkit::agents::reads_and_refusals(g, p, &mut rng);
        }
        Ok(())
    };
    let (g, n) = random_game("small", "fractal", 33, 50, DebugOptions::default(), true, &mut hook);
    assert_eq!(n, 50);
    for (a, b) in quiet.iter().zip(&noisy) {
        assert_eq!(a, b, "the round of turn {} differs with reads between the moves", a.0);
    }
    assert_eq!((g.chain().copied(), g.digest().ok()), ends);
}

#[test]
fn the_agent_tries_every_action_and_the_game_takes_most() {
    // Scope (1): the agent is complete across every action. In games from the start it tries
    // every tool a seat has; in a game that starts in the industrial era, everyone met, gold to
    // spend and a free tech owed, the game carries out each of them but the few that need a
    // war won, a promotion or the United Nations, which games this short seldom reach.
    let _stale = take_tally();
    for (size, ty, seed) in [("duel", "continents", 41), ("small", "pangaea", 42)] {
        let settings = games::random_settings(size, ty, "wrap_x", seed, 120);
        let mut g =
            games::new_game(&settings, b"every-action", DebugOptions::default()).expect("a game");
        let mut agents = games::agents_for(&g);
        games::play_random(&mut g, &mut agents, 120, &mut |_, _| Ok(())).expect("it plays");
    }
    let plain = take_tally();
    // Every action tool a seat has: the 40 of the registry's argument specs.
    let every: Vec<&str> = TOOLS.iter().map(|t| t.tool).collect();
    assert_eq!(every.len(), 40);
    let untried: Vec<&&str> = every.iter().filter(|t| !plain.contains_key(**t)).collect();
    assert!(untried.is_empty(), "never tried: {untried:?}\n{plain:#?}");

    let settings = games::random_settings("small", "continents", "wrap_x", 43, 80);
    let mut g =
        games::new_game(&settings, b"every-action", DebugOptions::default()).expect("a game");
    // Each civilization founds its capital where its settler stands, with two spearmen of an
    // era gone by to upgrade (to pikemen, which need no resource); the first seat, a person's, which the engine does not choose for,
    // has finished Liberty and is owed a great person.
    let mut ops = vec![
        serde_json::json!({"op": "grant_era", "player": "all", "era": "Industrial era"}),
        serde_json::json!({"op": "set_player", "player": "all", "gold": 3000, "free_techs": 1}),
        serde_json::json!({"op": "reveal", "player": "all", "meet": true}),
        serde_json::json!({"op": "adopt_policy", "player": 0, "policies": [
            "Liberty", "Republic", "Citizenship", "Collective Rule", "Representation", "Meritocracy"
        ]}),
    ];
    let settler = g.rules().derived().known.settler;
    for p in g.majors(true) {
        let Some(t) = g.player_units(p.id()).find(|u| Some(u.base) == settler).map(|u| u.tile())
        else {
            continue;
        };
        let (x, y) = g.xy(t);
        let (pl, count) = (p.id().0, 2);
        ops.push(serde_json::json!({"op": "found_city", "player": pl, "x": x, "y": y}));
        ops.push(serde_json::json!(
            {"op": "add_unit", "player": pl, "unit": "Spearman", "x": x, "y": y, "count": count}
        ));
    }
    g.apply_ops(&serde_json::Value::Array(ops)).expect("a later era");
    let owed = g.player(citar_engine::base::ids::PlayerId(0)).map_or(0, |p| p.gp.free);
    assert!(owed > 0, "Liberty's finisher owes a great person");
    let mut agents = games::agents_for(&g);
    games::play_random(&mut g, &mut agents, 60, &mut |_, _| Ok(())).expect("it plays");
    let later = take_tally();
    let taken = |t: &str| [&plain, &later].iter().any(|m| m.get(t).is_some_and(|x| x.taken > 0));
    // A sweep needs a promotion no unit starts with; a fate, a conquered city; a civilian back,
    // one the barbarians took; a vote, the United Nations; and `end_turn` is always refused a
    // driven seat, whose turn the drive ends.
    let rare = ["air_sweep", "city_status", "end_turn", "return_civilian", "un_vote"];
    let never: Vec<&&str> = every.iter().filter(|t| !rare.contains(t) && !taken(t)).collect();
    assert!(never.is_empty(), "never taken: {never:?}\n{plain:#?}\n{later:#?}");
    assert!(plain.get("end_turn").is_some_and(|x| x.taken == 0), "a driven seat's end_turn");
}
