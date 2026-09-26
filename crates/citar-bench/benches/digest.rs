//! The state digest, the save and the load of a synthetic gargantuan state (package 1a-09).
//!
//! A job digests the state at the end of every round (DESIGN.md 4.10), so the digest's budget is
//! 5 ms at gargantuan late game: 24 majors and 32 city-states on 160 by 100 tiles, 400 cities
//! and 2,500 units, most of the map explored and remembered, about 4 MB of canonical bytes.
//!
//! Criterion reports the times. After it, the run takes the median of its own timing of the
//! digest and fails above 15 ms, the hard limit; between 5 and 15 ms it only warns, as the
//! budget is report-only until 1e-03 tunes it.
//!
//! ```text
//! cargo bench -p citar-bench --bench digest
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::rules::Ruleset;
use citar_engine::save::{self, Digester, canon};
use citar_engine::state::State;
use citar_testkit::states::{self, Shape};
use criterion::Criterion;

/// The report-only budget.
const BUDGET: Duration = Duration::from_millis(5);

/// Above this the run fails.
const HARD: Duration = Duration::from_millis(15);

fn gargantuan() -> (&'static Ruleset, State) {
    let r = Ruleset::shared();
    (r, states::build(r, 2026, &Shape::GARGANTUAN))
}

fn benches(c: &mut Criterion, r: &'static Ruleset, st: &State) {
    let mut d = Digester::new();
    c.bench_function("digest/gargantuan", |b| {
        b.iter(|| d.digest(r, black_box(st)).expect("finite"));
    });
    let json = save::to_json(r, st).expect("saves");
    let mut g = c.benchmark_group("save");
    g.sample_size(10);
    g.bench_function("to_json/gargantuan", |b| b.iter(|| save::to_json(r, black_box(st))));
    g.bench_function("load/gargantuan", |b| {
        b.iter(|| save::json::read_state(r, black_box(&json)).expect("loads"));
    });
    g.finish();
}

/// The median of `n` timed digests.
fn median_digest(r: &'static Ruleset, st: &State, n: usize) -> Duration {
    let mut d = Digester::new();
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            black_box(d.digest(r, black_box(st)).expect("finite"));
            t.elapsed()
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn main() {
    let (r, st) = gargantuan();
    let bytes = canon::state_bytes(&st).expect("finite").len();
    println!("gargantuan state: {:.1} MB canonical", bytes as f64 / 1e6);
    let mut c = Criterion::default().configure_from_args();
    benches(&mut c, r, &st);
    c.final_summary();
    let median = median_digest(r, &st, 31);
    println!("digest/gargantuan median of 31: {median:?} (budget {BUDGET:?}, hard limit {HARD:?})");
    assert!(median <= HARD, "the digest of a gargantuan state took {median:?}, over {HARD:?}");
    if median > BUDGET {
        println!("warning: over the {BUDGET:?} budget (report-only until 1e-03)");
    }
}
