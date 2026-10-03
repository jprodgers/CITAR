//! The fixture sweep (package 2-03, gate 2; DESIGN.md P2.3.11 point 5): every reference state
//! loaded as refcheck loads it (`Game::from_python`), then one turn of `basic-1` for every living
//! major, with every check on at every settle (`DebugOptions::ALL`): no panic, no violation, no
//! broken invariant. This puts the bot in late-game positions (wars, sieges, great people,
//! spaceship parts, navies) in minutes, where whole games would take hours to reach them.
//!
//! The twelve committed states always; the 250 of the corpus as well when
//! `CITAR_REFCHECK_CORPUS` names its folder, the states on threads side by side (nextest counts
//! each test as taking every test thread, `.config/nextest.toml`). Each state's
//! refusals are reported: the most refusals of one tool in one bot turn, and the actions taken
//! and refused over the round. A bot that proposes freely is refused often (P2.3.6), so the
//! report is for reading; the test fails only on a turn that loops on a refusal, more than
//! [`LOOPING`] times one tool.

use std::collections::BTreeSet;

use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::{DebugOptions, DriveOptions, Drivers, Stop};
use citar_engine::state::Phase;
use citar_testkit::bots::CountingBot;
use citar_testkit::fixtures::{self, Fixture};
use citar_testkit::games;

/// More refusals of one tool than this in one bot turn is a bot looping on a refused action:
/// no turn of a reference state comes near it.
const LOOPING: u32 = 200;

/// What one state's round showed.
#[derive(Debug)]
struct Swept {
    name: String,
    majors: usize,
    /// The most refusals of one tool in one bot turn: how many, the tool, the turn.
    worst: (u32, &'static str, Turn),
    /// Actions taken and refused over the round.
    taken: u64,
    refused: u64,
}

/// Plays one turn of every living major of `f` with `basic-1`, the checks on at every settle.
fn sweep(f: &Fixture) -> Result<Swept, String> {
    let mut g = games::from_fixture(f, b"bot-sweep", DebugOptions::ALL)?;
    let majors: BTreeSet<PlayerId> = g.majors(true).map(|p| p.id()).collect();
    let n = g.state().players().len();
    let mut bots: Vec<CountingBot> = (0..n).map(|_| CountingBot::basic1()).collect();
    let mut played: BTreeSet<PlayerId> = BTreeSet::new();
    // Every seat a step: a round has at most every player in it, twice over at a state saved
    // mid-round.
    for _ in 0..2 * n + 2 {
        if played.len() >= majors.len() || g.phase() != Phase::Playing {
            break;
        }
        let current = g.current();
        let mut d = Drivers::none(n);
        for (i, b) in bots.iter_mut().enumerate() {
            let p = PlayerId(u8::try_from(i).map_err(|_| "too many players")?);
            if majors.contains(&p) {
                d = d.with(p, b);
            }
        }
        let (stop, _) = g
            .drive(&mut d, DriveOptions::default().with_seat_limit(1))
            .map_err(|e| format!("turn {}: {}", g.turn(), e.message))?;
        drop(d);
        if !matches!(stop, Stop::SeatLimit | Stop::GameOver) {
            return Err(format!("turn {}: the drive stopped with {stop:?}", g.turn()));
        }
        if majors.contains(&current) {
            played.insert(current);
        }
        let problems = games::problems(&mut g);
        if !problems.is_empty() {
            return Err(format!("after player {}: {problems:?}", current.0));
        }
    }
    if played.len() < majors.len() && g.phase() == Phase::Playing {
        return Err(format!("only {} of {} majors played", played.len(), majors.len()));
    }
    let worst = bots.iter().map(|b| b.worst).max_by_key(|w| w.0).unwrap_or_default();
    let (taken, refused) =
        bots.iter().map(CountingBot::sums).fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
    Ok(Swept { name: f.name.clone(), majors: majors.len(), worst, taken, refused })
}

/// Sweeps `list` on threads, each state on its own: what each showed, or what went wrong.
fn sweep_all(list: &[Fixture]) -> Vec<Result<Swept, String>> {
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get).max(1);
    let chunk = list.len().div_ceil(threads).max(1);
    // The engine runs no threads (DESIGN.md 6.13); states do run side by side, one a thread.
    #[allow(clippy::disallowed_methods, reason = "threads of the test's, not of the engine")]
    std::thread::scope(|s| {
        let handles: Vec<_> = list
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|f| {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(f)))
                                .unwrap_or_else(|_| Err("panicked".to_owned()))
                                .map_err(|e| format!("{}: {e}", f.name))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a sweep's thread")).collect()
    })
}

/// Reports what the sweep of `list` showed and fails on any problem.
#[allow(clippy::disallowed_macros, reason = "the test reports what it measured")]
fn holds(list: &[Fixture], what: &str) {
    let swept = sweep_all(list);
    let mut failures = Vec::new();
    let (mut taken, mut refused, mut majors) = (0, 0, 0);
    for s in &swept {
        match s {
            Ok(s) => {
                eprintln!(
                    "{}: {} majors; {} actions taken, {} refused; at most {} refusals of {} in \
                     one bot turn (turn {})",
                    s.name, s.majors, s.taken, s.refused, s.worst.0, s.worst.1, s.worst.2
                );
                taken += s.taken;
                refused += s.refused;
                majors += s.majors;
                if s.worst.0 > LOOPING {
                    failures.push(format!("{}: {:?}", s.name, s.worst));
                }
            }
            Err(e) => failures.push(e.clone()),
        }
    }
    eprintln!(
        "{what}: {} states, {majors} bot turns, {taken} actions taken and {refused} refused",
        swept.len()
    );
    assert!(failures.is_empty(), "{what}: {} problems:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_committed_state_plays_a_bot_round_cleanly() {
    let committed = fixtures::committed().expect("the committed fixtures");
    assert_eq!(committed.len(), 12, "the twelve committed fixtures");
    holds(&committed, "the committed states");
}

#[test]
fn every_corpus_state_plays_a_bot_round_cleanly() {
    let Some(corpus) = fixtures::corpus().expect("the corpus folder") else { return };
    holds(&corpus, "the corpus");
}
