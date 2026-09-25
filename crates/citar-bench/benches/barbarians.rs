//! A barbarian round (package 1c-06, gate 4).
//!
//! The state is a small map with raging barbarians at turn 120 (`small-continents-raging-s1005`
//! of the local corpus, when `CITAR_REFCHECK_CORPUS` names it: the nearest to turn 100 it has),
//! else the committed small map with raging barbarians at turn 50 (`small-pangaea-raging`);
//! loaded through the Python converter. `barbarians/round` times stage S0 on a fresh copy of the
//! game each time: the barbarians' units start their turn, every unit acts, the camps spawn and
//! new ones may appear, and the round's settle. Budget (DESIGN.md 10, report-only): 2 ms. After
//! Criterion, the run takes the median of its own timings and warns above it.
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench barbarians
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::PlayerId;
use citar_engine::game::{Game, barbarians, units};
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures::{self, Fixture};
use criterion::Criterion;

const ROUND: Duration = Duration::from_millis(2);

/// The small raging map at turn 120 from the corpus, else at turn 50 from the committed ones.
fn fixture() -> (Game, String) {
    let pick = |all: Vec<Fixture>, case: &str, turn: u32| {
        all.into_iter().find(|f| f.case == case && f.turn == turn)
    };
    let f = fixtures::corpus()
        .expect("the corpus folder")
        .and_then(|all| pick(all, "small-continents-raging-s1005", 120))
        .or_else(|| {
            pick(fixtures::committed().expect("the committed fixtures"), "small-pangaea-raging", 50)
        })
        .expect("the fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    (g, f.name)
}

/// Stage S0: the barbarians' units start their turn and act, their camps spawn, and it settles.
fn round(g: &mut Game, bid: PlayerId) {
    units::turn::start_units(g, bid);
    barbarians::take_turn(g);
    g.settle_for_bench();
}

/// The median of `n` rounds, each on a fresh copy of the game (the copy is not timed).
fn median_round(g: &Game, bid: PlayerId, n: usize) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let mut copy = g.clone();
            let t = Instant::now();
            round(&mut copy, bid);
            let took = t.elapsed();
            black_box(copy);
            took
        })
        .collect();
    times.sort();
    times[n / 2]
}

/// The median of `n` runs of `f`, each on a fresh copy of the game (the copy is not timed).
fn median_of(g: &Game, n: usize, f: impl Fn(&mut Game)) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let mut copy = g.clone();
            let t = Instant::now();
            f(&mut copy);
            let took = t.elapsed();
            black_box(copy);
            took
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn main() {
    let (g, name) = fixture();
    let bid = g.barbarian_id().expect("the barbarians");
    let camps = g.state().world().camps.values().filter(|c| !c.destroyed).count();
    println!(
        "{name}: {} barbarian units, {camps} standing camps, aggression {}",
        g.player_units(bid).count(),
        barbarians::aggression(&g)
    );

    let mut cr = Criterion::default().configure_from_args();
    cr.bench_function("barbarians/round", |b| {
        b.iter_batched(
            || g.clone(),
            |mut copy| {
                round(&mut copy, bid);
                copy
            },
            criterion::BatchSize::LargeInput,
        );
    });
    cr.final_summary();

    let took = median_round(&g, bid, 31);
    println!("barbarians/round median: {took:?} (budget {ROUND:?}, report-only)");
    // Its parts, for the tuning of package 1e-03: the camps' turn alone, and the units' start.
    let camps = median_of(&g, 31, barbarians::update_camps);
    println!("barbarians/camps median: {camps:?} (report-only)");
    let start = median_of(&g, 31, |g| units::turn::start_units(g, bid));
    println!("barbarians/units_start median: {start:?} (report-only)");
    if took > ROUND {
        println!("warning: barbarians/round is over its {ROUND:?} budget (report-only)");
    }
}
