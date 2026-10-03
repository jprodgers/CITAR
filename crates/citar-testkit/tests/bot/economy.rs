//! The bot's economy (package 2-01b, gate 3): whole games of four `basic-1` bots on small maps
//! with the barbarians off, every check on at every settle, seeds 5000 to 5004 on the Python
//! baseline's map rotation (`refcheck/baseline/python/small.jsonl`: continents, pangaea,
//! archipelago, inland sea, fractal; Quick, Prince), 150 rounds each, the five side by side.
//!
//! These bots play their units and fight (package 2-03), but with the barbarians off and no war
//! declared until 2-05's diplomacy they fight little and trade nothing, so what is measured is
//! the economy: at turn 100 the 20 civilizations hold 2.8 cities on average and 85% of them two
//! or more, know 17 technologies on average and each at least 12; each built a settler by turn
//! 60; no tool is refused more than 50 times in one bot turn; and a game saved and loaded at
//! round 75 plays on as the uninterrupted one.
//!
//! A civilization whose landmass holds no expansion site has no settler to build: Python's bot
//! (`expansion_sites`, basic.py:1080-1088) and its port settle only the landmass their cities
//! are on. Seed 5002's archipelago starts one civilization on an island of 26 land tiles, every
//! one within three of its capital, where no city can be founded. The settler check counts the
//! civilizations whose advisor named a site by turn 60, and the test holds that every other
//! civilization's sites stayed empty every round, and that there is at most one.

use std::collections::{BTreeMap, BTreeSet};

use citar_engine::base::digest::Digest;
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::advisor::{Advisor, AdvisorParams};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::chronicle::{EngineEvent, EventType};
use citar_engine::state::cities::Constructible;
use citar_testkit::bots::CountingBot;
use citar_testkit::games;
use serde_json::json;

/// The baseline's map types, in its rotation (`common.MAP_TYPES`).
const MAPS: [&str; 5] = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"];

/// The rounds each game plays.
const ROUNDS: u32 = 150;

/// Game `i` of the run: the baseline's settings, with the barbarians off.
fn game(i: u32) -> Game {
    let seat = json!({"controller": "bot", "nation": null});
    let settings = json!({
        "seed": 5000 + i,
        "map_size": "small",
        "map_type": MAPS[i as usize % MAPS.len()],
        "barbarians": "off",
        "speed": "Quick",
        "difficulty": "Prince",
        "players": [seat, seat, seat, seat],
    });
    games::new_game(&settings, b"bot-economy", DebugOptions::ALL).expect("a small game")
}

/// What one game showed.
#[derive(Debug, Default)]
struct Played {
    /// At turn 100, per major: cities and technologies.
    at_100: BTreeMap<u8, (usize, usize)>,
    /// The first turn each major built a settler (trained or bought).
    first_settler: BTreeMap<u8, Turn>,
    /// The majors whose advisor named an expansion site in some round up to turn 60.
    had_a_site: BTreeSet<u8>,
    /// The most refusals of one tool in one bot turn: how many, the tool, the turn.
    worst: (u32, &'static str, Turn),
    rounds: Vec<(Turn, Digest)>,
}

/// Plays game `i` for [`ROUNDS`] rounds, saving and loading after round `reload` if given.
fn play(i: u32, reload: Option<u32>) -> Played {
    let mut g = game(i);
    let majors: Vec<u8> = g.majors(true).map(|p| p.id().0).collect();
    assert_eq!(majors.len(), 4, "game {i}");
    let mut bots: Vec<CountingBot> =
        (0..g.state().players().len()).map(|_| CountingBot::basic1()).collect();
    let mut out = Played::default();
    let mut seen_events = 0u32;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let pp = AdvisorParams::default();
    let mut hook = |g: &mut Game, round: games::Round| -> Result<(), String> {
        let problems = games::problems(g);
        if !problems.is_empty() {
            return Err(format!("game {i} round {}: {problems:?}", round.0));
        }
        out.rounds.push(round);
        let founders = &g.rules().derived().advisor.founders;
        for e in g.events(seen_events, usize::MAX) {
            if e.kind == EventType::Engine(EngineEvent::UnitBuilt)
                && let Some(d) = &e.data
                && let (Some(Constructible::Unit(u)), Some(p)) = (d.item, d.player)
                && founders.contains(u)
            {
                out.first_settler.entry(p.0).or_insert(e.turn);
            }
        }
        seen_events = g.events(0, 1).last().map_or(seen_events, |e| e.id.get());
        if g.turn() <= 60 {
            for &p in &majors {
                if !Advisor::new(g, PlayerId(p), &pp).sites(g).is_empty() {
                    out.had_a_site.insert(p);
                }
            }
        }
        if g.turn() == 100 {
            for &p in &majors {
                let pid = PlayerId(p);
                let cities = g.player_cities(pid).count();
                let techs = g.player(pid).map_or(0, |x| x.tech.known.len());
                out.at_100.insert(p, (cities, techs));
            }
        }
        if reload.is_some_and(|r| out.rounds.len() == r as usize) {
            games::save_and_load(g, &mut chunks)?;
        }
        Ok(())
    };
    let played = games::play_random(&mut g, &mut bots, ROUNDS, &mut hook)
        .unwrap_or_else(|e| panic!("game {i}: {e}"));
    assert_eq!(played, ROUNDS, "game {i}");
    out.worst = bots.iter().map(|b| b.worst).max_by_key(|w| w.0).unwrap_or_default();
    out
}

/// Plays each of `runs` (a game's index and the round to reload it at) on its own thread.
fn play_all(runs: &[(u32, Option<u32>)]) -> Vec<Played> {
    // The engine runs no threads (DESIGN.md 6.13); games do run side by side, one a thread.
    #[allow(clippy::disallowed_methods, reason = "threads of the test's, not of the engine")]
    std::thread::scope(|s| {
        let handles: Vec<_> =
            runs.iter().map(|&(i, reload)| s.spawn(move || play(i, reload))).collect();
        handles.into_iter().map(|h| h.join().expect("a game's thread")).collect()
    })
}

#[test]
#[allow(clippy::disallowed_macros, reason = "the test reports what it measured")]
fn four_bots_expand_and_research_on_small_maps() {
    let runs: Vec<(u32, Option<u32>)> = (0..5).map(|i| (i, None)).collect();
    let played = play_all(&runs);
    let mut cities = Vec::new();
    let mut techs = Vec::new();
    let mut boxed_in = Vec::new();
    for (i, p) in played.iter().enumerate() {
        eprintln!(
            "game {i} ({}): at turn 100 {:?}; first settlers {:?}; a site by turn 60 {:?}; most \
             refusals of a tool in a bot turn {:?}",
            MAPS[i], p.at_100, p.first_settler, p.had_a_site, p.worst
        );
        assert_eq!(p.at_100.len(), 4, "game {i} reached turn 100");
        for (&civ, &(c, t)) in &p.at_100 {
            cities.push(c);
            techs.push(t);
            let first = p.first_settler.get(&civ).copied();
            if p.had_a_site.contains(&civ) {
                let by_60 = first.is_some_and(|x| x <= 60);
                assert!(by_60, "game {i} player {civ}: first settler {first:?}");
            } else {
                // No site at all: nothing to settle, and so no settler built.
                assert_eq!(first, None, "game {i} player {civ}");
                boxed_in.push((i, civ, c));
            }
        }
        assert!(p.worst.0 <= 50, "game {i}: {:?}", p.worst);
    }
    eprintln!("civilizations with no site on their landmass by turn 60: {boxed_in:?}");
    assert!(boxed_in.len() <= 1, "{boxed_in:?}");
    #[allow(clippy::cast_precision_loss, reason = "20 small counts")]
    let mean = |v: &[usize]| v.iter().sum::<usize>() as f64 / v.len() as f64;
    let two = cities.iter().filter(|&&c| c >= 2).count();
    eprintln!(
        "turn 100: cities mean {:.2} ({two} of {} with two or more), techs mean {:.2}, least {:?}",
        mean(&cities),
        cities.len(),
        mean(&techs),
        techs.iter().min()
    );
    assert_eq!(cities.len(), 20);
    assert!(mean(&cities) >= 2.8, "cities {cities:?}");
    assert!(two * 100 >= 85 * cities.len(), "two or more: {two} of {}", cities.len());
    assert!(mean(&techs) >= 17.0, "techs {techs:?}");
    assert!(techs.iter().all(|&t| t >= 12), "techs {techs:?}");
}

#[test]
fn a_bot_game_saved_and_loaded_at_round_75_plays_on_as_the_uninterrupted_one() {
    let played = play_all(&[(0, None), (0, Some(75))]);
    let (whole, reloaded) = (&played[0], &played[1]);
    assert_eq!(whole.rounds.len(), ROUNDS as usize);
    assert_eq!(whole.rounds, reloaded.rounds);
    // The bots remember what they do across the save: the same game, the same memory.
    assert_eq!(whole.at_100, reloaded.at_100);
}
