//! How often the memos recompute to the value they had (DESIGN.md 6.4, 10; package 1e-03,
//! gate 6), feature `stats`.
//!
//! A recompute that changes nothing is a revision moved that the memo did not need moved: an
//! over-bump. Python recomputed 74-99.5% of its caches to the same value; the budget is under 5%
//! for every memo. The soak slice plays `RandomAgent` games on generated maps (a duel of 200
//! rounds, a small map of 120 and a standard one of 60, the random golden set's sizes) and
//! passes rounds from every committed fixture, with the checks off as a shipped build runs; every
//! memo read on the way is counted where it is read (`derive::rev::tally`), and the job maps
//! count theirs (`derive::jobs::counts`).

use std::collections::BTreeMap;

use citar_engine::game::derive::jobs;
use citar_engine::game::derive::rev::tally::{self, Tally};
use citar_engine::game::{DebugOptions, Game};
use citar_testkit::{fixtures, games};

/// The share of recomputes a memo may make to the value it had.
const LIMIT: f64 = 0.05;

/// Below this many recomputes a memo's share is noise, and is reported but not held.
const SAMPLE: u64 = 200;

/// Adds one run's tallies to the soak's.
fn gather(all: &mut BTreeMap<String, Tally>) {
    for (site, t) in tally::take() {
        all.entry(site).or_default().add(&t);
    }
}

/// The soak slice's games, each counted into `all`; the job maps' own counts, summed.
fn soak(all: &mut BTreeMap<String, Tally>) -> (u64, u64) {
    drop(tally::take());
    let mut jobs_counts = (0, 0);
    let mut add_jobs = |g: &Game| {
        let n = jobs::counts(g);
        jobs_counts.0 += n.recomputed;
        jobs_counts.1 += n.unchanged;
    };
    for (size, map_type, seed, rounds) in [
        ("duel", "continents", 11, 200),
        ("small", "fractal", 12, 120),
        ("standard", "continents", 13, 60),
    ] {
        let settings = games::random_settings(size, map_type, "wrap_x", seed, rounds + 10);
        let mut g = games::new_game(&settings, b"memos", DebugOptions::OFF).expect("a game");
        let mut agents = games::agents_for(&g);
        games::play_random(&mut g, &mut agents, rounds, &mut |_, _| Ok(())).expect("it plays");
        add_jobs(&g);
        gather(all);
    }
    for f in fixtures::committed().expect("the fixtures") {
        let mut g = games::from_fixture(&f, b"memos", DebugOptions::OFF).expect("it loads");
        games::pass_rounds(&mut g, 5, &mut |_, _| Ok(())).expect("it passes");
        add_jobs(&g);
        gather(all);
    }
    jobs_counts
}

#[test]
#[ignore = "package 1e-03, gate 6: the redundancy the memos show is being tuned"]
#[allow(clippy::disallowed_macros, reason = "the soak's table is reported")]
fn no_memo_recomputes_to_the_same_value_more_than_one_time_in_twenty() {
    let mut all = BTreeMap::new();
    let (jobs_recomputed, jobs_unchanged) = soak(&mut all);
    println!(
        "{:<72} {:>10} {:>9} {:>8} {:>8} {:>8} {:>7}",
        "memo (where it is read)", "hits", "valid", "first", "again", "same", "same %"
    );
    let mut over = Vec::new();
    for (site, t) in &all {
        let share = t.redundancy();
        println!(
            "{site:<72} {:>10} {:>9} {:>8} {:>8} {:>8} {:>6.2}%",
            t.hits,
            t.valid,
            t.first,
            t.recomputed,
            t.unchanged,
            share * 100.0
        );
        if t.recomputed >= SAMPLE && share >= LIMIT {
            over.push(format!("{site}: {:.1}% of {} recomputes", share * 100.0, t.recomputed));
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "counts of tiles")]
    let jobs_share =
        if jobs_recomputed == 0 { 0.0 } else { jobs_unchanged as f64 / jobs_recomputed as f64 };
    println!(
        "job maps: {jobs_recomputed} tiles worked out again, {jobs_unchanged} to the same job ({:.2}%)",
        jobs_share * 100.0
    );
    if jobs_recomputed >= SAMPLE && jobs_share >= LIMIT {
        over.push(format!("job maps: {:.1}% of {jobs_recomputed}", jobs_share * 100.0));
    }
    assert!(all.len() >= 20, "the soak reads most memos: {}", all.len());
    assert!(over.is_empty(), "memos over 5% redundant recomputes:\n{}", over.join("\n"));
}
