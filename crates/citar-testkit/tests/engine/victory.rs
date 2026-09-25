//! Victory, eliminations, the round's records and revolts (package 1c-08), with every check on:
//! - gate 3: each round's statistics row carries every key Python's rows had (baseline.py's and
//!   balance.py's among them), an eliminated civilization's row says so, and every war declared
//!   and city captured names both sides;
//! - gate 4: the replay frames a hundred-turn arena game records, keyframes and deltas across a
//!   save and load, decode to the frames captured directly at each round's end;
//! - eliminations: the player whose turn it is is eliminated as the round ends, anyone else at
//!   once; an eliminated civilization's deals end and a city-state's ally goes;
//! - the United Nations' own votes: a city-state for its living ally, a civilization whose seat
//!   votes for it among those it thinks best of, drawn the same every time;
//! - revolts in a deeply unhappy civilization, whose draws differ from Python's;
//! - the kitchen sink's `Triggers victory`, the neutral victory.
//!
//! The rule scripts `victory_*` pin what both engines share: the Domination, Cultural,
//! Scientific, Diplomatic and Time victories, the turn limit with no winner, and eliminations.

use citar_engine::api::{inspect, testops};
use citar_engine::base::ids::{BaseUnitId, CityId, PlayerId};
use citar_engine::game::diplomacy::actions::DeclareWar;
use citar_engine::game::diplomacy::deals;
use citar_engine::game::victory::{self, Won, records, un};
use citar_engine::game::{Action, DebugOptions, DriverOutcome, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::save::ctx::with_rules;
use citar_engine::save::journal::{FrameDecoder, FullFrame};
use citar_engine::state::Phase;
use citar_engine::state::chronicle::{CivStats, EngineEvent};
use citar_engine::state::diplo::{DealItem, Side, Terms};
use citar_engine::state::players::DriverMemory;
use citar_testkit::agents::RandomAgent;
use citar_testkit::rulesets::kitchen_sink;
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);
const THIRD: PlayerId = PlayerId(2);

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// A game on the arena with `extra` over three benchmark civilizations, no city-states, no
/// barbarians and no ruins; the starting units kept only with `units`.
fn arena_with(r: &'static Ruleset, extra: &Value, units: bool) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let mut cfg = json!({
        "seed": 7,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0, "barbarians": "off", "ruins": false, "map": doc,
    });
    if let (Some(c), Some(e)) = (cfg.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            c.insert(k.clone(), v.clone());
        }
    }
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    if !units {
        testops::apply(
            &mut g,
            &json!([{"op": "clear_units", "player": "all"}, {"op": "clear_camps"}]),
        )
        .expect("cleared");
    }
    g
}

fn arena(extra: &Value) -> Game {
    arena_with(Ruleset::shared(), extra, false)
}

fn ops(g: &mut Game, ops: &Value) -> Vec<Value> {
    g.apply_ops(ops).unwrap_or_else(|e| panic!("{e:?}")).0
}

fn test_ops(g: &mut Game, ops: &Value) -> Vec<Value> {
    testops::apply(g, ops).unwrap_or_else(|e| panic!("{e:?}")).0
}

fn end_round(g: &mut Game) {
    test_ops(g, &json!([{"op": "end_round"}]));
}

fn alive(g: &Game, p: PlayerId) -> bool {
    g.player(p).is_some_and(citar_engine::state::players::Player::alive)
}

fn events(g: &Game, kind: EngineEvent) -> Vec<String> {
    g.chronicle()
        .events()
        .iter()
        .filter(|e| e.kind.name() == kind.name())
        .map(|e| e.text.to_string())
        .collect()
}

fn unit_of(out: &Value) -> u32 {
    out["unit_ids"][0].as_u64().and_then(|n| u32::try_from(n).ok()).expect("a unit")
}

// ---- Gate 3: the statistics rows and the events a runner counts ----------------------------------------

/// The keys `scripts/refcheck/baseline.py` reads of a row (`STAT_KEYS`), and `citar/balance.py`'s.
const BASELINE_KEYS: [&str; 8] =
    ["cities", "population", "techs", "score", "military", "era", "policies", "land"];
const BALANCE_KEYS: [&str; 13] = [
    "score",
    "cities",
    "population",
    "land",
    "techs",
    "era",
    "military",
    "gold",
    "gold_per_turn",
    "science",
    "production",
    "happiness",
    "units",
];

#[test]
fn each_rounds_statistics_row_carries_every_key_python_wrote() {
    let mut g = arena(&json!({}));
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "pop": 3},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 6},
            {"op": "add_unit", "player": 2, "unit": "Scout", "x": 11, "y": 13},
            {"op": "grant_tech", "player": 1, "techs": ["Pottery", "Mining"]},
            {"op": "set_player", "player": 0, "gold": 123.7},
        ]),
    );
    end_round(&mut g);
    let rows = g.chronicle().stats();
    assert_eq!(rows.len(), 1, "one row a round");
    let row = &rows[0];
    assert_eq!(row.turn, 1, "the round's own turn");
    let players: Vec<PlayerId> = row.civs.iter().map(|c| c.player).collect();
    assert_eq!(players, [ME, YOU, THIRD], "a row per major, by id");
    for c in &row.civs {
        let v = with_rules(g.rules(), || serde_json::to_value(c)).expect("a row");
        for k in CivStats::KEYS.iter().chain(&BASELINE_KEYS).chain(&BALANCE_KEYS) {
            assert!(v.get(*k).is_some(), "{k} missing from {v}");
        }
    }
    let me = &row.civs[0];
    assert!(me.alive && me.cities == 1 && me.population == 3 && me.units == 1);
    let gold = g.player(ME).map_or(0.0, |p| p.econ.gold);
    assert!(gold.fract() != 0.0 && me.gold == gold.trunc() as i64, "whole gold, as Python's int()");
    assert_eq!(me.score, victory::score(&g, ME).total);
    assert_eq!(row.civs[1].techs, 3, "Agriculture and the two granted");
    // The heads keep the newest row, which a save's summary reads.
    assert_eq!(g.state().chronicle().last_stats.as_ref(), Some(row));
    // A civilization with no city and no unit is eliminated as the round ends, and its next row
    // says so.
    test_ops(&mut g, &json!([{"op": "clear_units", "player": 2}]));
    end_round(&mut g);
    assert!(!alive(&g, THIRD));
    let last = g.chronicle().stats().last().expect("a row");
    assert_eq!(last.civs[2], CivStats::eliminated(THIRD));
    // The host reads them as the facade's `stats` did.
    assert_eq!(g.stats(Some(1)), &g.chronicle().stats()[1..]);
    assert_eq!(g.stats(None).len(), 2);
    assert_eq!(g.stats(Some(0)).len(), 2, "0 is all, as Python's `if last` read it");
    clean(&mut g);
}

#[test]
fn every_war_declared_and_city_captured_names_both_sides() {
    let mut g = arena(&json!({}));
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "found_city", "player": 1, "x": 18, "y": 4},
            {"op": "found_city", "player": 2, "x": 11, "y": 13},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 17, "y": 4},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "meet", "a": 1, "b": 2},
        ]),
    );
    let declare = Action::DeclareWar(DeclareWar { player_id: 1, message: None });
    g.act(ME, declare).expect("war");
    // A war agreed to in a deal: the second side declares it on the third.
    let terms = Terms {
        sides: [
            Side { giver: ME, items: vec![] },
            Side { giver: THIRD, items: vec![DealItem::DeclareWar { target: YOU }] },
        ],
    };
    deals::execute_deal(&mut g, ME, THIRD, &terms).expect("a deal");
    let wars: Vec<_> = g
        .chronicle()
        .events()
        .iter()
        .filter(|e| e.kind.name() == "war_declared")
        .map(|e| e.data.as_deref().map(|d| (d.attacker, d.defender)))
        .collect();
    assert_eq!(wars, [Some((Some(ME), Some(YOU))), Some((Some(THIRD), Some(YOU)))]);
    // A capture.
    let city = out[2]["city_id"].clone();
    test_ops(&mut g, &json!([{"op": "reload"}]));
    ops(&mut g, &json!([{"op": "set_city", "city": city, "health": 1}]));
    let w = unit_of(&out[4]);
    testops::apply(&mut g, &json!([{"op": "attack_as", "unit": w, "x": 18, "y": 4}]))
        .expect("an attack");
    let caps: Vec<_> = g
        .chronicle()
        .events()
        .iter()
        .filter(|e| e.kind.name() == "city_captured")
        .map(|e| e.data.as_deref().map(|d| (d.old_owner, d.new_owner)))
        .collect();
    assert_eq!(caps, [Some((Some(YOU), Some(ME)))]);
    clean(&mut g);
}

// ---- Gate 4: replay frames ------------------------------------------------------------------------------

/// A hundred turns of three random agents on the arena, with a city-state and barbarians, saved and
/// loaded halfway: the frames in the chronicle decode, keyframe by delta, to the frames captured
/// as each round ended, and the last is the game's own map as it ends.
#[test]
fn the_frames_of_a_hundred_turns_decode_to_what_each_round_captured() {
    let mut g = arena_with(
        Ruleset::shared(),
        &json!({"seed": 21, "city_states": 1, "barbarians": "normal", "turn_limit": 100}),
        true,
    );
    records::capture_frames_for_test(true);
    let mut agents = [RandomAgent::new(), RandomAgent::new(), RandomAgent::new()];
    let mut reloaded = false;
    while g.phase() == Phase::Playing {
        let pid = g.current();
        let Some(agent) = agents.get_mut(usize::from(pid.0)) else { panic!("{pid:?} plays") };
        let mut memory = DriverMemory::empty(0, 0);
        assert_eq!(agent.play_turn(&mut g, pid, &mut memory), DriverOutcome::Done);
        g.end_turn(pid).expect("the turn ends");
        if !reloaded && g.turn() == 50 {
            test_ops(&mut g, &json!([{"op": "reload"}]));
            reloaded = true;
        }
        assert!(g.turn() <= 101, "the game ends at its turn limit");
    }
    clean(&mut g);
    let captured = records::take_frames_for_test();
    records::capture_frames_for_test(false);
    let recs = &g.chronicle().frames().frames;
    assert_eq!(recs.len(), captured.len(), "every frame recorded is in the chronicle");
    assert!(recs.len() >= 99, "a frame a round: {}", recs.len());
    let keys: Vec<i32> = recs.iter().filter(|r| r.keyframe).map(|r| r.turn).collect();
    assert_eq!(keys[0], 1, "the first frame is a keyframe");
    assert!(keys.contains(&50), "a frame after a load is a keyframe: {keys:?}");
    assert!(keys.len() < recs.len() / 4, "the rest are deltas: {keys:?}");
    let mut dec = FrameDecoder::new();
    let mut end = 0;
    for (rec, want) in recs.iter().zip(&captured) {
        let got = dec.apply(rec).unwrap_or_else(|e| panic!("turn {}: {e}", rec.turn));
        assert!(got == *want, "the frame of turn {} decodes to what was captured", rec.turn);
        assert_eq!(got.event_range.0, end, "the ranges follow each other");
        end = got.event_range.1;
    }
    // The last frame is the map as the game ended: the round's end changed no tile, city or unit
    // after it.
    let last = captured.last().expect("a frame");
    let mut now = FullFrame::capture(g.rules(), g.state(), last.event_range);
    now.turn = last.turn;
    assert!(now == *last, "the last frame is the map as it is");
}

// ---- Eliminations -----------------------------------------------------------------------------------------

/// The player whose turn it is loses its last city: it is eliminated as the round ends, not at
/// once, and play goes on with a living player whose turn it is.
#[test]
fn a_player_defeated_on_its_own_turn_is_eliminated_as_the_round_ends() {
    // refcheck: eliminated-on-its-own-turn-at-the-round-end
    let mut g = arena(&json!({}));
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "found_city", "player": 2, "x": 18, "y": 4},
        ]),
    );
    assert_eq!(g.current(), ME);
    ops(&mut g, &json!([{"op": "remove_city", "city": out[0]["city_id"]}]));
    assert!(alive(&g, ME), "not while its turn goes on");
    clean(&mut g);
    // Anyone else is eliminated at once.
    ops(&mut g, &json!([{"op": "remove_city", "city": out[1]["city_id"]}]));
    assert!(!alive(&g, YOU));
    end_round(&mut g);
    assert!(!alive(&g, ME));
    assert_eq!(g.current(), THIRD, "the next round begins with the one left");
    assert_eq!(events(&g, EngineEvent::Eliminated).len(), 2);
    clean(&mut g);
}

/// A player eliminated as the round ends whose turn it was, the last of the round, hands the
/// turn on, so that a living player's turn it always is.
#[test]
fn the_last_player_of_a_round_eliminated_hands_the_turn_on() {
    let mut g = arena(&json!({}));
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
        ]),
    );
    // The third has nothing: it plays its turn, the last of the round, and is gone at its end.
    end_round(&mut g);
    assert!(!alive(&g, THIRD));
    assert_eq!(g.current(), ME);
    clean(&mut g);
}

/// An eliminated civilization's deals end, and a city-state conquered loses its ally and is
/// answered for by its conqueror.
#[test]
fn an_eliminated_civilizations_deals_end_and_a_city_states_ally_goes() {
    let mut g = arena(&json!({"city_states": 1}));
    let cs = g.state().players().iter().find(|(_, p)| p.is_city_state()).map(|(id, _)| id);
    let cs = cs.expect("a city-state");
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "found_city", "player": cs.0, "x": 11, "y": 7},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 10, "y": 7},
            {"op": "add_unit", "player": 2, "unit": "Scout", "x": 11, "y": 13},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "meet", "a": 1, "b": "all"},
            {"op": "set_influence", "city_state": cs.0, "player": 1, "amount": 100},
            {"op": "set_relation", "a": 0, "b": cs.0, "state": "war"},
        ]),
    );
    assert_eq!(
        g.player(cs).and_then(|p| p.city_state.as_deref()).and_then(|d| d.ally()),
        Some(YOU)
    );
    let gold = Terms {
        sides: [
            Side { giver: YOU, items: vec![DealItem::GoldPerTurn { amount: 1, turns: 20 }] },
            Side { giver: THIRD, items: vec![] },
        ],
    };
    let deal = deals::execute_deal(&mut g, YOU, THIRD, &gold).expect("a deal");
    // The third loses its only unit to nobody's attack but a scenario's: it goes as the round
    // ends, with its deal.
    test_ops(&mut g, &json!([{"op": "clear_units", "player": 2}]));
    ops(&mut g, &json!([{"op": "set_city", "city": out[2]["city_id"], "health": 1}]));
    let w = unit_of(&out[3]);
    testops::apply(&mut g, &json!([{"op": "attack_as", "unit": w, "x": 11, "y": 7}]))
        .expect("an attack");
    assert!(!alive(&g, cs), "a city-state that lost its city is gone at once");
    let d = g.player(cs).and_then(|p| p.city_state.as_deref()).expect("its data");
    assert_eq!(d.ally(), None);
    assert!(g.state().diplo().deal(deal).is_some_and(|d| d.active), "not yet");
    end_round(&mut g);
    assert!(!alive(&g, THIRD));
    assert!(g.state().diplo().deal(deal).is_some_and(|d| !d.active), "its deal ended with it");
    clean(&mut g);
}

// ---- The United Nations ---------------------------------------------------------------------------------

/// Everyone who has not voted when the vote is counted votes as its seat decides: a city-state
/// for its ally, a civilization whose seat votes for it for the civilization it thinks best of
/// (one of equals, drawn the same every time), and one whose player casts its own votes and cast
/// none abstains.
#[test]
fn the_vote_counts_everyone_as_its_seat_decides() {
    let run = || {
        let mut g = arena(&json!({"city_states": 1}));
        let cs = g.state().players().iter().find(|(_, p)| p.is_city_state()).map(|(id, _)| id);
        let cs = cs.expect("a city-state");
        ops(
            &mut g,
            &json!([
                {"op": "found_city", "player": 0, "x": 5, "y": 5, "buildings": ["United Nations"]},
                {"op": "found_city", "player": 1, "x": 18, "y": 10},
                {"op": "found_city", "player": 2, "x": 18, "y": 4},
                {"op": "found_city", "player": cs.0, "x": 11, "y": 7},
                {"op": "meet", "a": 0, "b": "all"},
                {"op": "meet", "a": 1, "b": "all"},
                {"op": "meet", "a": 2, "b": "all"},
                {"op": "set_influence", "city_state": cs.0, "player": 2, "amount": 100},
            ]),
        );
        test_ops(
            &mut g,
            &json!([
                {"op": "set_auto", "player": 1, "decision": "un_vote", "on": true},
                {"op": "set_auto", "player": 0, "decision": "un_vote", "on": false},
                {"op": "set_auto", "player": 2, "decision": "un_vote", "on": false},
                {"op": "set_turn", "turn": 16},
            ]),
        );
        let votes_needed = un::votes_needed(&g);
        assert_eq!(votes_needed, 4, "four voters and the builder's second vote: five, four win");
        end_round(&mut g);
        clean(&mut g);
        g.state().world().un.results.clone().expect("a result")
    };
    let first = run();
    assert_eq!(first, run(), "drawn the same every time");
    assert_eq!(first.turn, 17);
    // The city-state for its ally, the third; the second, whose seat votes for it, for the first
    // or the third, whom it thinks as well of; the first and the third, whose players cast their
    // own votes and cast none, abstain.
    let votes: u16 = first.tally.iter().map(|&(_, n)| n).sum();
    assert_eq!(votes, 2, "{first:?}");
    assert!(first.tally.iter().any(|&(p, n)| p == THIRD && n >= 1), "{first:?}");
    assert!(first.tally.iter().all(|&(p, _)| p != YOU), "nobody votes for the second: {first:?}");
    assert_eq!(first.winner, None, "two votes, where four win");
}

// ---- Revolts ----------------------------------------------------------------------------------------------

/// A deeply unhappy civilization in a game with barbarians counts down to a revolt, from the
/// ruleset's base turns plus up to two; when it runs out rebels of a land melee unit it could
/// field appear beside its city, and it is told. Happy again, the countdown goes.
#[test]
fn a_deeply_unhappy_civilization_revolts() {
    let mut g = arena(&json!({"barbarians": "normal"}));
    let barb = g.barbarian_id().expect("barbarians");
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "pop": 40},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "found_city", "player": 2, "x": 18, "y": 4},
        ]),
    );
    let base = g.rules().constants().formulas.base_turns_until_revolt;
    let revolt_in = |g: &Game| g.player(ME).and_then(|p| p.civ.revolt_in);
    end_round(&mut g);
    let first = revolt_in(&g).expect("a countdown");
    assert!((base..=base + 2).contains(&i32::from(first)), "{first} from {base}");
    assert!(g.player(YOU).is_some_and(|p| p.civ.revolt_in.is_none()), "a happy one has none");
    let mut rounds = 0;
    while events(&g, EngineEvent::Revolt).is_empty() {
        end_round(&mut g);
        rounds += 1;
        assert!(rounds <= i32::from(first), "the revolt comes when the countdown runs out");
    }
    assert_eq!(rounds, i32::from(first));
    let revolt = g.chronicle().events().iter().rev().find(|e| e.kind.name() == "revolt");
    let revolt = revolt.expect("an event");
    assert_eq!(&*revolt.text, "Your citizens are revolting due to very high unhappiness!");
    assert!(revolt.audience.is_some_and(|a| a.contains(ME)));
    // One city, one rebel, on the tile of the city where rebels stand best: the barbarians'
    // camps may have spawned others, elsewhere.
    let at = revolt.tile.expect("its tile");
    let rebels: Vec<BaseUnitId> =
        g.player_units(barb).filter(|u| u.tile() == at).map(|u| u.base).collect();
    assert_eq!(rebels.len(), 1, "one city, one rebel");
    let d = &g.rules().base_units()[rebels[0]];
    assert!(d.melee && d.unique_to.is_none() && g.has_tech(ME, d.required_tech));
    let city = out[0]["city_id"].as_u64().and_then(|n| u32::try_from(n).ok());
    let city = city.and_then(CityId::new).and_then(|c| g.city(c)).map(|c| c.tile());
    assert!(city.is_some_and(|c| g.grid().distance(at, c) <= 3), "a tile of its city");
    // Happy again: the countdown goes.
    ops(&mut g, &json!([{"op": "set_city", "city": out[0]["city_id"], "pop": 1}]));
    end_round(&mut g);
    assert_eq!(revolt_in(&g), None);
    clean(&mut g);
}

// ---- The kitchen sink ----------------------------------------------------------------------------------

/// `Triggers victory` (the Kitchen Sink Wonder) wins the neutral victory once its holder's turn
/// starts or ends: a winner and no victory of the ruleset, which `inspect` names `Neutral`.
#[test]
fn triggers_victory_wins_the_neutral_victory() {
    let r = kitchen_sink();
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 1,
        "players": [{"nation": "Kitchen Sink"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0, "barbarians": "off", "ruins": false, "map": doc,
    });
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "buildings": ["Kitchen Sink Wonder"]},
        ]),
    );
    assert_eq!(victory::victory_achieved(&g, YOU), Some(Won::Neutral));
    assert_eq!(victory::victory_achieved(&g, ME), None, "not without the unique");
    assert_eq!(g.phase(), Phase::Playing, "asked as a turn starts or ends");
    g.end_turn(ME).expect("the first turn");
    assert_eq!(g.phase(), Phase::Over, "won as its turn starts");
    let c = *g.state().clock();
    assert_eq!((c.winner, c.victory), (Some(YOU), None));
    let game = inspect::inspect(&g, &json!({"what": "game"})).expect("the game");
    assert_eq!(game["victory"], json!("Neutral"));
    let name = g.player(YOU).map(|p| p.name.to_string()).unwrap_or_default();
    assert_eq!(events(&g, EngineEvent::Victory), [format!("{name} has won the game!")]);
    clean(&mut g);
}
