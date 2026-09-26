//! Map generation (package 1b-04, gate 2): a small map and a gargantuan one, every map type.
//!
//! The budgets are 50 ms for a small map and 1.5 s for a gargantuan one in release, where
//! Python took up to 7 s (DESIGN.md 9.7). Criterion reports the times; after it the run takes
//! the median of its own timings for each size, the slowest map type counting, and fails above
//! three times the budget. Between the budget and that it only warns: the budget is report-only.
//!
//! ```text
//! cargo bench -p citar-bench --bench mapgen
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::mapgen::{self, GenSpec, MAP_TYPES, MapOptions, MapType};
use citar_engine::rules::Ruleset;
use criterion::Criterion;

/// (lobby size, report-only budget).
const SIZES: [(&str, Duration); 2] =
    [("small", Duration::from_millis(50)), ("gargantuan", Duration::from_millis(1500))];

fn spec(r: &Ruleset, size: &str, map_type: MapType) -> GenSpec<'static> {
    let c = r.constants();
    let lobby = &c.map_sizes[c.map_size_id(size).expect("a lobby size")];
    GenSpec {
        width: lobby.width,
        height: lobby.height,
        map_type,
        options: MapOptions::default(),
        players: usize::from(lobby.players),
        city_states: usize::from(lobby.city_states),
        nations: &[],
        ruins: true,
    }
}

/// The median of `n` timed maps, seeds 1 to n.
fn median(r: &Ruleset, s: &GenSpec<'_>, n: u64) -> Duration {
    let mut times: Vec<Duration> = (1..=n)
        .map(|seed| {
            let t = Instant::now();
            black_box(mapgen::generate(r, seed, black_box(s)).expect("a map"));
            t.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

fn main() {
    let r = Ruleset::shared();
    let mut c = Criterion::default().configure_from_args();
    for (size, _) in SIZES {
        let mut g = c.benchmark_group(format!("mapgen/{size}"));
        g.sample_size(10);
        for t in MAP_TYPES {
            let s = spec(r, size, t);
            let mut seed = 0;
            g.bench_function(t.key(), |b| {
                b.iter(|| {
                    seed += 1;
                    mapgen::generate(r, seed, black_box(&s)).expect("a map")
                });
            });
        }
        g.finish();
    }
    c.final_summary();
    let mut failed = Vec::new();
    for (size, budget) in SIZES {
        let n = if size == "small" { 31 } else { 7 };
        let (slowest, time) = MAP_TYPES
            .iter()
            .map(|&t| (t, median(r, &spec(r, size, t), n)))
            .max_by_key(|&(_, d)| d)
            .expect("five map types");
        let hard = budget * 3;
        println!(
            "mapgen/{size}: slowest type {} at {time:?}, median of {n} (budget {budget:?}, hard \
             limit {hard:?})",
            slowest.key()
        );
        if time > hard {
            failed.push(format!("{size} maps took {time:?}, over {hard:?}"));
        } else if time > budget {
            println!("warning: over the {budget:?} budget (report-only)");
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("; "));
}
