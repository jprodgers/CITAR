//! Map generation (package 1b-04): a small map and a gargantuan one, every map type.
//!
//! The budgets are 50 ms for a small map and 1.5 s for a gargantuan one, where Python took up to
//! 7 s (DESIGN.md 10). The suite takes the median of its own timings for each size and map
//! type, and the budget holds the slowest type.

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_bench::{Suite, median_timed};
use citar_engine::mapgen::{self, GenSpec, MAP_TYPES, MapOptions, MapType};
use citar_engine::rules::Ruleset;
use criterion::Criterion;

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
    let mut seed = 0;
    median_timed(usize::try_from(n).unwrap_or(1), || {
        seed += 1;
        let t = Instant::now();
        black_box(mapgen::generate(r, seed, black_box(s)).expect("a map"));
        t.elapsed()
    })
}

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let r = Ruleset::shared();
    for size in ["small", "gargantuan"] {
        let mut g = c.benchmark_group(format!("mapgen/{size}"));
        g.sample_size(10);
        for t in MAP_TYPES {
            let sp = spec(r, size, t);
            let mut seed = 0;
            g.bench_function(t.key(), |b| {
                b.iter(|| {
                    seed += 1;
                    mapgen::generate(r, seed, black_box(&sp)).expect("a map")
                });
            });
        }
        g.finish();
    }
    for size in ["small", "gargantuan"] {
        let n = if size == "small" { 31 } else { 7 };
        let mut slowest = Duration::ZERO;
        for t in MAP_TYPES {
            let took = median(r, &spec(r, size, t), n);
            s.note(&format!("mapgen/{size}/{}", t.key()), took);
            slowest = slowest.max(took);
        }
        s.put(&format!("mapgen/{size}"), slowest);
    }
}
