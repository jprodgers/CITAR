//! How often the memos recompute, and to the value they had (DESIGN.md 6.4, 10; package 1e-03,
//! gate 6), feature `stats`.
//!
//! The soak slice plays `RandomAgent` games on generated maps (a duel of 200 rounds, a small map
//! of 120 and a standard one of 60, the random golden set's sizes) and passes five rounds from
//! every committed fixture, with the checks off as a shipped build runs; every memo read on the
//! way is counted where it is read (`derive::rev::tally`), and the job maps count theirs
//! (`derive::jobs::counts`). It is deterministic: the same engine counts the same.
//!
//! DESIGN.md 10 bounds the recomputes that come out as they were at 5% of each memo's recomputes
//! (Python's were 74-99.5%). As built in 1e-03 most memos are far above it: a memo recomputes when
//! an input its revisions name moves (a city's pressure, its health, the turn, any city's owner),
//! and such inputs move nearly every round while the value rarely changes. Phase 1 gates the guard
//! against over-bumping instead (DESIGN.md 10's decisions, 1e-03's fix round, on the owner's
//! priority of stability first): no memo may recompute more than a quarter above what this soak
//! recorded, and a memo not recorded must stay under the 5%. The 5% test is kept, ignored, as a
//! diagnostic that prints the table.

use std::collections::BTreeMap;

use citar_engine::game::derive::jobs;
use citar_engine::game::derive::rev::tally::{self, Tally};
use citar_engine::game::{DebugOptions, Game};
use citar_testkit::{fixtures, games};

/// The share of recomputes a memo may make to the value it had.
const LIMIT: f64 = 0.05;

/// Below this many recomputes a memo's share is noise, and is reported but not held.
const SAMPLE: u64 = 200;

/// The recomputes of each memo the soak recorded at package 1e-03, a quarter added: a write that
/// moves a revision more often than before shows here. A memo whose count falls may have its
/// line lowered; one newly over a line says which write over-bumps (or, after a rule change that
/// plays the soak's games differently, that the line should be recorded again).
const RECORDED: [(&str, u64); 26] = [
    ("derive::buildable::buildable", 1_030),
    ("derive::buildable::civ_requirements", 1_040),
    ("derive::civ::aura_units", 1_560),
    ("derive::civ::city_local_full", 2_850),
    ("derive::civ::civ_index", 1_180),
    ("derive::civ::civ_index_full", 540),
    ("derive::civ::era", 1_180),
    ("derive::civ::owned_tiles", 480),
    ("derive::civ::supply", 15_800),
    ("derive::danger::danger", 320),
    ("derive::religion::major_religion", 7_330),
    ("derive::religion::majority", 17_330),
    ("derive::religion::reach", 930),
    ("derive::religion::with_spread", 340),
    ("derive::stats::city_base", 19_060),
    ("derive::stats::city_mods", 16_270),
    ("derive::stats::city_parts", 16_160),
    ("derive::stats::city_stats", 12_440),
    ("derive::stats::civ_stats", 7_770),
    ("derive::stats::connectivity", 8_450),
    ("derive::stats::deficit", 10_050),
    ("derive::stats::happiness", 10_910),
    ("derive::stats::tile_yield_full", 26_490),
    ("derive::stats::upkeep", 5_870),
    ("path::memo::civ_parts", 27_950),
    ("path::memo::zoc", 6_790),
];

/// A place's name without the crate's prefix and the type arguments of a generic function.
fn short(site: &str) -> &str {
    let s = site.strip_prefix("citar_engine::game::").unwrap_or(site);
    s.split('<').next().unwrap_or(s)
}

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
#[allow(clippy::disallowed_macros, reason = "the soak's table is reported")]
fn no_memo_recomputes_more_than_the_soak_recorded() {
    let mut all = BTreeMap::new();
    let _jobs = soak(&mut all);
    let mut over = Vec::new();
    for (site, t) in &all {
        let name = short(site);
        match RECORDED.iter().find(|&&(k, _)| k == name) {
            Some(&(_, line)) => {
                println!("{name}: {} recomputes (line {line})", t.recomputed);
                if t.recomputed > line {
                    over.push(format!("{name}: {} recomputes, above {line}", t.recomputed));
                }
            }
            None if t.recomputed >= SAMPLE && t.redundancy() >= LIMIT => over.push(format!(
                "{name}: {} recomputes, {:.1}% to the same value (a memo not recorded stays \
                 under 5%)",
                t.recomputed,
                t.redundancy() * 100.0
            )),
            None => {}
        }
    }
    assert!(all.len() >= 20, "the soak reads most memos: {}", all.len());
    assert!(over.is_empty(), "memos recomputing more than recorded:\n{}", over.join("\n"));
}

/// Gate 6 of package 1e-03 as the design first stated it, which the memos do not hold and Phase 1
/// does not gate (see the module's doc): run with `--run-ignored all` for the table.
#[test]
#[ignore = "a diagnostic: Phase 1 gates the guard above instead (DESIGN.md 10's decisions)"]
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
