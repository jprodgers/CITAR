//! The bot at war (package 2-03, gates 3 and 4; DESIGN.md P2.3.11 point 6): whole games on
//! small maps with the Python baseline's settings (the five map types in turn, Quick, Prince,
//! barbarians normal), the invariants checked at every settle.
//!
//! - **Mixed games** (gate 3): two `basic-1` bots against two `RandomAgent`s, which declare war
//!   now and then after turn 50, 20 games of 200 rounds: no panic or violation; both bots
//!   out-score both agents in at least 18 of the 20; the bots attack in every game, and take at
//!   least one city across the twenty. The bots declare no war (that is package 2-05's
//!   diplomacy), so their wars are the agents' and the barbarians'. The bots take the first two
//!   seats in one game and the last two in the next.
//! - **Barbarian games** (gate 4): four bots, 10 games of 330 rounds, to the game's end at its
//!   turn limit: no panic or violation, and every game ends.
//!
//! The games play on as many threads as there are cores, in about twenty seconds in the ci
//! profile on the laptop; nextest counts each test as taking every test thread
//! (`.config/nextest.toml`), so that the games do not starve the other whole-game tests.
//! With `CITAR_BOT_CHECKS=all` the cache oracle runs at every settle as well
//! (`DebugOptions::ALL`), which makes each game about sixty times slower: a check to run by hand
//! after a change to the engine's caches, not on every build.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use citar_engine::base::ids::{NegotiationId, PlayerId, Turn};
use citar_engine::game::victory::score::score;
use citar_engine::game::{DebugOptions, DriverOutcome, Game, SeatDriver};
use citar_engine::state::Phase;
use citar_engine::state::chronicle::{EngineEvent, EventType};
use citar_engine::state::players::DriverMemory;
use citar_testkit::agents::RandomAgent;
use citar_testkit::bots::CountingBot;
use citar_testkit::games;
use serde_json::json;

/// The baseline's map types, in its rotation (`common.MAP_TYPES`).
const MAPS: [&str; 5] = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"];

/// The checks at every settle: the invariants, and the cache oracle too with
/// `CITAR_BOT_CHECKS=all`.
#[allow(clippy::disallowed_methods, reason = "a test's switch")]
fn checks() -> DebugOptions {
    let all = std::env::var_os("CITAR_BOT_CHECKS").is_some_and(|v| v == "all");
    DebugOptions { invariants: true, verify_caches: all }
}

/// A seat of a mixed game: a bot or an agent.
enum Seat {
    Bot(CountingBot),
    Agent(RandomAgent),
}

impl SeatDriver for Seat {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        match self {
            Self::Bot(b) => b.play_turn(g, pid, mem),
            Self::Agent(a) => a.play_turn(g, pid, mem),
        }
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        match self {
            Self::Bot(b) => b.respond(g, pid, nid, mem),
            Self::Agent(a) => a.respond(g, pid, nid, mem),
        }
    }
}

/// The baseline's small game of `seed`, the `i`th map type of its rotation.
fn game(seed: u64, i: usize, turn_limit: Option<u32>) -> Game {
    let seat = json!({"controller": "bot", "nation": null});
    let settings = json!({
        "seed": seed,
        "map_size": "small",
        "map_type": MAPS[i % MAPS.len()],
        "barbarians": "normal",
        "speed": "Quick",
        "difficulty": "Prince",
        "turn_limit": turn_limit,
        "players": [seat, seat, seat, seat],
    });
    games::new_game(&settings, b"bot-war", checks()).expect("a small game")
}

/// What one game showed.
#[derive(Debug, Default)]
struct Played {
    seed: u64,
    rounds: u32,
    over: bool,
    /// Each major's score at the end, by seat, and whether a bot played it.
    scores: BTreeMap<u8, (i32, bool)>,
    /// Attacks the bots made.
    attacks: u64,
    /// Cities the bots took.
    captured: u32,
    /// The most refusals of one tool in one bot turn: how many, the tool, the turn.
    worst: (u32, &'static str, Turn),
}

impl Played {
    /// Whether both bots out-scored both agents.
    fn bots_ahead(&self) -> bool {
        let bot_low = self.scores.values().filter(|s| s.1).map(|s| s.0).min();
        let agent_high = self.scores.values().filter(|s| !s.1).map(|s| s.0).max();
        matches!((bot_low, agent_high), (Some(b), Some(a)) if b > a)
    }
}

/// One game of a run.
struct Run {
    seed: u64,
    /// Its place in the run, which picks its map type.
    i: usize,
    turn_limit: Option<u32>,
    /// The seats bots play; agents play the others.
    bots: Vec<u8>,
    /// The most rounds it plays.
    rounds: u32,
}

/// Plays one game of a run.
fn play(run: &Run) -> Played {
    let Run { seed, i, turn_limit, ref bots, rounds } = *run;
    let mut g = game(seed, i, turn_limit);
    let n = g.state().players().len();
    let mut seats: Vec<Seat> = (0..n)
        .map(|i| {
            let bot = u8::try_from(i).is_ok_and(|p| bots.contains(&p));
            if bot { Seat::Bot(CountingBot::basic1()) } else { Seat::Agent(RandomAgent::new()) }
        })
        .collect();
    let mut out = Played { seed, ..Played::default() };
    let mut seen = 0u32;
    let mut hook = |g: &mut Game, round: games::Round| -> Result<(), String> {
        let problems = games::problems(g);
        if !problems.is_empty() {
            return Err(format!("round {}: {problems:?}", round.0));
        }
        for e in g.events(seen, usize::MAX) {
            if e.kind == EventType::Engine(EngineEvent::CityCaptured)
                && e.data.as_ref().and_then(|d| d.new_owner).is_some_and(|p| bots.contains(&p.0))
            {
                out.captured += 1;
            }
        }
        seen = g.events(0, 1).last().map_or(seen, |e| e.id.get());
        Ok(())
    };
    out.rounds = games::play_random(&mut g, &mut seats, rounds, &mut hook)
        .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    out.over = g.phase() != Phase::Playing;
    for p in g.majors(false) {
        out.scores.insert(p.id().0, (score(&g, p.id()).total, bots.contains(&p.id().0)));
    }
    for s in &seats {
        if let Seat::Bot(b) = s {
            out.attacks += b.taken("attack");
            if b.worst.0 > out.worst.0 {
                out.worst = b.worst;
            }
        }
    }
    out
}

/// Plays `runs` on as many threads as the machine has cores, each thread a game at a time, and
/// gives what each showed in the runs' order.
fn play_all(runs: &[Run]) -> Vec<Played> {
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let next = AtomicUsize::new(0);
    // The engine runs no threads (DESIGN.md 6.13); games do run side by side, one a thread.
    #[allow(clippy::disallowed_methods, reason = "threads of the test's, not of the engine")]
    let mut played: Vec<(usize, Played)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.min(runs.len()))
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(run) = runs.get(i) else { return out };
                        out.push((i, play(run)));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a game's thread")).collect()
    });
    played.sort_by_key(|&(i, _)| i);
    played.into_iter().map(|(_, p)| p).collect()
}

#[test]
#[allow(clippy::disallowed_macros, reason = "the test reports what it measured")]
fn two_bots_beat_two_random_agents_and_fight_their_wars() {
    let runs: Vec<Run> = (0..20)
        .map(|i| Run {
            seed: 7000 + u64::try_from(i).unwrap_or(0),
            i,
            turn_limit: None,
            bots: if i % 2 == 0 { vec![0, 1] } else { vec![2, 3] },
            rounds: 200,
        })
        .collect();
    let played = play_all(&runs);
    for p in &played {
        eprintln!(
            "seed {}: {} rounds; scores {:?}; bots ahead {}; {} attacks, {} cities taken; most \
             refusals of a tool in a bot turn {:?}",
            p.seed,
            p.rounds,
            p.scores,
            p.bots_ahead(),
            p.attacks,
            p.captured,
            p.worst
        );
        assert_eq!(p.rounds, 200, "seed {}", p.seed);
        assert!(p.attacks > 0, "seed {}: the bots never attacked", p.seed);
    }
    let ahead = played.iter().filter(|p| p.bots_ahead()).count();
    let captured: u32 = played.iter().map(|p| p.captured).sum();
    eprintln!("bots ahead in {ahead} of {} games; {captured} cities taken by bots", played.len());
    assert!(ahead >= 18, "bots ahead in {ahead} of {}", played.len());
    assert!(captured >= 1, "no city taken by a bot in {} games", played.len());
}

#[test]
#[allow(clippy::disallowed_macros, reason = "the test reports what it measured")]
fn four_bots_play_330_rounds_with_the_barbarians_to_the_end() {
    let runs: Vec<Run> = (0..10)
        .map(|i| Run {
            seed: 8000 + u64::try_from(i).unwrap_or(0),
            i,
            turn_limit: Some(330),
            bots: vec![0, 1, 2, 3],
            rounds: 331,
        })
        .collect();
    for p in play_all(&runs) {
        eprintln!(
            "seed {}: {} rounds, over {}; scores {:?}; {} attacks, {} cities taken; most \
             refusals of a tool in a bot turn {:?}",
            p.seed, p.rounds, p.over, p.scores, p.attacks, p.captured, p.worst
        );
        assert!(p.over, "seed {}: not over after {} rounds", p.seed, p.rounds);
    }
}
