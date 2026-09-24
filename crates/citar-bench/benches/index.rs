//! The unique indexes of a civilization (package 1b-05, gate 4): a lookup in a memo that is
//! verified, and a rebuild from the civilization's sources.
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter; the civilization is the one whose index has the most entries.
//! Budgets (DESIGN.md 10, report-only until 1e-03): a lookup at or under 20 ns, a rebuild at or
//! under 10 µs. After Criterion, the run takes the median of its own timings and fails above
//! three times a budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench index
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::game::derive::civ::sources;
use citar_engine::rules::Ruleset;
use citar_engine::unique::{CivIndex, EvalWorld, IndexLayer, UniqueType};
use citar_testkit::fixtures;
use criterion::Criterion;

const LOOKUP: Duration = Duration::from_nanos(20);
const REBUILD: Duration = Duration::from_micros(10);

/// The late fixture's game, and its civilization with the largest index.
fn late() -> (Game, PlayerId) {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == "small-continents-normal-s1025" && f.turn == 280)
        .expect("the late fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    let p = g
        .majors(true)
        .map(|x| x.id())
        .max_by_key(|&p| (g.view().civ_index(p, IndexLayer::Full).len(), std::cmp::Reverse(p)))
        .expect("a living major");
    (g, p)
}

/// One lookup: the index lent by its verified memo, and one type's run of it.
fn lookup(g: &Game, p: PlayerId) -> usize {
    let v = g.view();
    v.civ_index(p, IndexLayer::Full).get(UniqueType::StatPercentBonus).len()
}

/// The median of `n` timings of `f`, each of `batch` calls.
fn median(n: usize, batch: u32, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..batch {
                f();
            }
            t.elapsed() / batch
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn check(name: &str, took: Duration, budget: Duration) {
    println!("{name} median: {took:?} (budget {budget:?}, hard limit {:?})", budget * 3);
    assert!(took <= budget * 3, "{name} took {took:?}, over three times {budget:?}");
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget (report-only until 1e-03)");
    }
}

fn main() {
    let (g, p) = late();
    let r = g.rules();
    let src = sources(&g, p);
    println!(
        "player {}: {} entries without resources, {} with",
        p.0,
        CivIndex::build(r, &src).len(),
        g.view().civ_index(p, IndexLayer::Full).len()
    );
    let mut c = Criterion::default().configure_from_args();
    c.bench_function("civ_index/lookup", |b| b.iter(|| lookup(black_box(&g), black_box(p))));
    c.bench_function("civ_index/rebuild", |b| {
        b.iter(|| CivIndex::build(black_box(r), black_box(&src)));
    });
    c.bench_function("civ_index/sources", |b| b.iter(|| sources(black_box(&g), black_box(p))));
    c.final_summary();
    check(
        "civ_index/lookup",
        median(31, 10_000, || {
            black_box(lookup(black_box(&g), black_box(p)));
        }),
        LOOKUP,
    );
    check(
        "civ_index/rebuild",
        median(31, 100, || {
            black_box(CivIndex::build(black_box(r), black_box(&src)));
        }),
        REBUILD,
    );
}
