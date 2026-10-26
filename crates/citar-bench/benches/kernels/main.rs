//! The kernels (DESIGN.md 9.7, 10; packages 1b-05 to 1c-07, gathered in 1e-03): each hot
//! algorithm timed on its own, on the reference fixtures or the synthetic gargantuan state.
//!
//! Each part runs Criterion's benchmarks, then takes the median of its own timings and records
//! it against its budget in `thresholds.toml` (`Suite::put`), or for the record only
//! (`Suite::note`); the run fails at the end if a measure is over 1.5 times its budget. A filter
//! on the command line runs only the parts it names (or whose benchmarks it names):
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench kernels
//! cargo bench -p citar-bench --bench kernels -- advisor
//! ```

mod advisor;
mod barbarians;
mod buildable;
mod combat;
mod hex;
mod index;
mod jobs;
mod mapgen;
mod path;
mod religion;
mod stats;
mod vis;

use citar_bench::Suite;
use criterion::Criterion;

/// A part: its name, the words its benchmarks start with, and what runs it.
type Part = (&'static str, &'static [&'static str], fn(&mut Suite, &mut Criterion));

const PARTS: [Part; 12] = [
    ("hex", &["hex"], hex::run),
    (
        "stats",
        &["tile_yield", "city_stats", "citizens", "happiness", "connectivity", "settle", "memos"],
        stats::run,
    ),
    ("index", &["civ_index"], index::run),
    ("vis", &["vis_step", "los", "vis/"], vis::run),
    ("path", &["astar", "reachable"], path::run),
    ("combat", &["combat"], combat::run),
    ("buildable", &["buildable"], buildable::run),
    ("jobs", &["jobmap"], jobs::run),
    ("religion", &["religion"], religion::run),
    ("barbarians", &["barbarians"], barbarians::run),
    ("advisor", &["advisor"], advisor::run),
    ("mapgen", &["mapgen"], mapgen::run),
];

fn main() {
    let mut s = Suite::start("kernels");
    let mut c = s.criterion();
    for (name, words, run) in PARTS {
        if s.wants(name) || words.iter().any(|w| s.wants(w)) {
            println!("--- {name}");
            run(&mut s, &mut c);
        }
    }
    c.final_summary();
    s.finish();
}
