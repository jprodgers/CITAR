//! The macro benchmarks (DESIGN.md 9.7, 10; package 1e-03): pass rounds on every reference state,
//! the client views, and a whole engine-only game.
//!
//! - `turn/pass_round/<case>/t<turn>`: a round in which every seat ends its turn with nothing
//!   played, on every corpus state (the twelve committed ones without the corpus). Each state is
//!   loaded through the Python converter, one round is passed untimed (so the memos hold what a
//!   game under way holds), and the next is timed on fresh copies of the game, the median kept:
//!   what `scripts/refcheck/turn_timing.py` times of Python's engine. perfgate
//!   (`cargo xtask perf`) holds each to at least 20 times Python's speed, to the backstops (small
//!   t280 150 ms, large t280 400 ms) and to the target budgets of `thresholds.toml`; the suite
//!   prints the ratios as it goes. Criterion times five of them.
//! - `view/player`: `Game::view_json` for a player on `small-continents-normal-s1025/t280` (the
//!   corpus has no small map at turn 300). Budget 1.5 ms.
//! - `view/god`: a spectator's view of `large-pangaea-normal-s1016/t280` (the corpus). Budget
//!   20 ms, the plan's floor.
//! - `view/god_gargantuan`: a spectator's view of the synthetic gargantuan state (24 majors, 32
//!   city-states, 400 cities, 2,500 units on 160 by 100 tiles). Budget 12 ms.
//! - `briefing/small`: `Game::briefing` for a civilization of the same small state as
//!   `view/player`. Budget 1 ms. `briefing/turn_progress`: the turn's progress there, which a
//!   host asks for after every batch of actions; budget 1 ms. `briefing/large`: the briefing on
//!   `large-pangaea-normal-s1016/t280` (the corpus), report-only (package 1d-03's bench, folded
//!   in here).
//! - `game/random_small_330`: a new small continents game, four `RandomAgent`s, played to its
//!   turn limit of 330 with the checks off (the Phase 1 gate of DESIGN.md 10). Budget 10 s.
//!
//! Every view and briefing is taken on a game whose memos are warm, as a host's is between two
//! calls.
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench turns
//! ```

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_bench::{Suite, fixtures, median_on_copies, median_timed, show};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::Phase;
use citar_testkit::fixtures::Fixture;
use citar_testkit::games;

/// The states Criterion times a pass round of: (case, turn), when the suite has them.
const CRITERION_ROUNDS: [(&str, u32); 5] = [
    ("duel-continents-normal", 50),
    ("standard-pangaea-normal-s1031", 120),
    ("small-continents-normal-s1025", 280),
    ("large-pangaea-normal-s1016", 280),
    ("gargantuan-pangaea-normal-s2002", 25),
];

/// Ends every turn left in the round, as Python's `testops._end_round` does; false if the game
/// is over before it ends or a seat's turn does not end.
fn pass_round(g: &mut Game) -> bool {
    let start = g.turn();
    // A round ends after at most every player's turn, and one more for the round's end.
    for _ in 0..=g.state().players().len() + 1 {
        if g.phase() != Phase::Playing {
            return false;
        }
        if g.turn() != start {
            return true;
        }
        if g.end_turn(g.current()).is_err() {
            return false;
        }
    }
    g.turn() != start
}

/// Python's pass rounds (`refcheck/perf/python-turns.json`), in milliseconds by state.
fn python_rounds() -> BTreeMap<String, f64> {
    let path = citar_testkit::fixtures::repo_root().join("refcheck/perf/python-turns.json");
    let Ok(bytes) = std::fs::read(&path) else { return BTreeMap::new() };
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    doc.get("rounds")
        .and_then(serde_json::Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.get("ms")?.as_f64()?))).collect())
        .unwrap_or_default()
}

/// A state after one untimed pass round, or `None` if its game ends first.
fn warmed(f: &Fixture) -> Option<Game> {
    let mut g = fixtures::load(f);
    pass_round(&mut g).then_some(g)
}

/// Times a pass round of every state, records each, and prints its ratio to Python's.
fn pass_rounds(s: &mut Suite, states: &[Fixture]) {
    let python = python_rounds();
    let mut worst: Option<(f64, String)> = None;
    for f in states {
        let Some(g) = warmed(f) else {
            println!("{}: the game ends in the untimed round (skipped)", f.name);
            continue;
        };
        let n = if g.grid().size() <= 5_000 { 7 } else { 3 };
        let took = median_on_copies(&g, n, |copy| {
            black_box(pass_round(copy));
        });
        s.record(&format!("turn/pass_round/{}", f.name), took);
        let ratio = python.get(&f.name).map(|ms| ms / (took.as_secs_f64() * 1e3));
        match ratio {
            Some(x) => {
                println!("turn/pass_round/{}: {} ({x:.0}x Python)", f.name, show(took));
                if worst.as_ref().is_none_or(|(w, _)| x < *w) {
                    worst = Some((x, f.name.clone()));
                }
            }
            None => println!("turn/pass_round/{}: {} (no Python timing)", f.name, show(took)),
        }
    }
    if let Some((x, name)) = worst {
        println!("the lowest ratio to Python: {x:.0}x on {name}");
    }
}

/// A small continents game of four `RandomAgent`s played to its turn limit of 330, checks off.
fn random_game() -> Duration {
    let settings = games::random_settings("small", "continents", "wrap_x", 330, 330);
    let mut g = games::new_game(&settings, b"small-330", DebugOptions::OFF).expect("a game");
    let mut agents = games::agents_for(&g);
    let t = Instant::now();
    let played = games::play_random(&mut g, &mut agents, 400, &mut |_, _| Ok(())).expect("plays");
    let took = t.elapsed();
    assert_eq!(played, 330, "it reached its turn limit");
    took
}

/// The first living major of a game.
fn first_major(g: &Game) -> PlayerId {
    g.majors(true).map(|p| p.id()).next().unwrap_or(PlayerId(0))
}

/// The briefing and the turn's progress on the small late state, and the briefing on the large
/// one of the corpus.
fn briefings(s: &mut Suite, c: &mut criterion::Criterion) {
    let small = fixtures::late();
    let pid = first_major(&small);
    let text = small.briefing(pid);
    println!(
        "the late fixture: the briefing of {pid:?} is {} lines, {} bytes",
        text.lines().count(),
        text.len()
    );
    let large = fixtures::corpus_game("large-pangaea-normal-s1016", 280);
    let large_pid = large.as_ref().map(|(g, _)| first_major(g));
    if let (Some((g, name)), Some(p)) = (&large, large_pid) {
        println!("{name}: the briefing of {p:?} is {} bytes", g.briefing(p).len());
    } else {
        println!("no corpus (CITAR_REFCHECK_CORPUS): briefing/large is skipped");
    }
    c.bench_function("briefing/small", |b| b.iter(|| black_box(small.briefing(pid))));
    c.bench_function("briefing/turn_progress", |b| {
        b.iter(|| black_box(small.turn_progress(pid)));
    });
    if let (Some((g, _)), Some(p)) = (&large, large_pid) {
        let mut grp = c.benchmark_group("briefing");
        grp.sample_size(20);
        grp.bench_function("large", |b| b.iter(|| black_box(g.briefing(p))));
        grp.finish();
    }
    s.put("briefing/small", citar_bench::median(31, 3, || drop(black_box(small.briefing(pid)))));
    s.put(
        "briefing/turn_progress",
        citar_bench::median(31, 3, || drop(black_box(small.turn_progress(pid)))),
    );
    if let (Some((g, _)), Some(p)) = (&large, large_pid) {
        s.note("briefing/large", citar_bench::median(11, 1, || drop(black_box(g.briefing(p)))));
    }
}

fn main() {
    let mut s = Suite::start("turns");
    let mut c = s.criterion();
    let corpus = fixtures::corpus();
    let states = if corpus.is_empty() { fixtures::committed() } else { corpus };
    let committed = fixtures::committed();

    if s.wants("pass_round") || s.wants("turn/") {
        let mut grp = c.benchmark_group("turn/pass_round");
        grp.sample_size(10);
        for (case, turn) in CRITERION_ROUNDS {
            let Some(f) = fixtures::find(&states, case, turn)
                .or_else(|| fixtures::find(&committed, case, turn))
            else {
                continue;
            };
            let Some(g) = warmed(&f) else { continue };
            grp.bench_function(f.name.as_str(), |b| {
                b.iter_batched(
                    || g.clone(),
                    |mut copy| {
                        pass_round(&mut copy);
                        copy
                    },
                    criterion::BatchSize::LargeInput,
                );
            });
        }
        grp.finish();
        pass_rounds(&mut s, &states);
    }

    if s.wants("view") {
        let small = fixtures::late();
        let pid = small.majors(true).map(|p| p.id()).next().unwrap_or(PlayerId(0));
        let player = || black_box(small.view_json(Some(pid), 150));
        let _warm = player();
        println!("the late fixture: the view of {pid:?} is {} bytes", player().len());
        let large = fixtures::corpus_game("large-pangaea-normal-s1016", 280);
        if let Some((g, name)) = &large {
            let _warm = g.view_json(None, 150);
            println!("{name}: a spectator's view is {} bytes", g.view_json(None, 150).len());
        } else {
            println!("no corpus (CITAR_REFCHECK_CORPUS): view/god on a large map is skipped");
        }
        let huge = fixtures::gargantuan_game();
        let _warm = huge.view_json(None, 150);
        println!("gargantuan: a spectator's view is {} bytes", huge.view_json(None, 150).len());
        c.bench_function("view/player", |b| b.iter(player));
        let mut grp = c.benchmark_group("view");
        grp.sample_size(10);
        if let Some((g, _)) = &large {
            grp.bench_function("god", |b| b.iter(|| black_box(g.view_json(None, 150))));
        }
        grp.bench_function("god_gargantuan", |b| {
            b.iter(|| black_box(huge.view_json(None, 150)));
        });
        grp.finish();
        s.put("view/player", citar_bench::median(31, 3, || drop(player())));
        if let Some((g, _)) = &large {
            s.put(
                "view/god",
                citar_bench::median(11, 1, || drop(black_box(g.view_json(None, 150)))),
            );
        }
        s.put(
            "view/god_gargantuan",
            citar_bench::median(7, 1, || drop(black_box(huge.view_json(None, 150)))),
        );
    }

    if s.wants("briefing") {
        briefings(&mut s, &mut c);
    }

    if s.wants("game") {
        s.put("game/random_small_330", median_timed(3, random_game));
    }
    c.final_summary();
    s.finish();
}
