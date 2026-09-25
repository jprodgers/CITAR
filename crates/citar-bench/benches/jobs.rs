//! Worker job maps (package 1c-04, gate 2): `jobmap_small`, a civilization's whole `JobMap` for
//! the Worker's builder class built cold on a small map.
//!
//! The states are the committed small maps, loaded through the Python converter: a mid-game one
//! (`small-pangaea-raging/t50`), a scenario (`scenario-small-continents-s3001/t61`) and a late one
//! (`small-continents-normal-s1025/t280`). On each, the major with the most cities has its map
//! rebuilt: what the civilization's rules allow (`CivJobs`), then the best job on every tile of
//! its cities, each improvement weighed as `automation::best_job` weighs it. The tile yields,
//! unique indexes and other memos a job reads are warm, as they are when a civilization's rules
//! change mid-game and its map is built again. Each state is printed against the budget; the
//! gate is the late one, the largest map. Budget 200 µs (DESIGN.md 10, report-only until 1e-03);
//! after Criterion, the run takes the median of its own timings and fails above three times it.
//!
//! ```text
//! cargo bench -p citar-bench --bench jobs
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{BaseUnitId, PlayerId};
use citar_engine::game::Game;
use citar_engine::game::derive::jobs;
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::BuilderClass;
use citar_testkit::fixtures;
use criterion::Criterion;

const REBUILD: Duration = Duration::from_micros(200);

/// The committed small-map fixtures, as (case, turn); the last is the gate's.
const SMALL_FIXTURES: [(&str, u32); 3] = [
    ("small-pangaea-raging", 50),
    ("scenario-small-continents-s3001", 61),
    ("small-continents-normal-s1025", 280),
];

fn fixture(case: &str, turn: u32) -> Game {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == case && f.turn == turn)
        .expect("the fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    Game::from_python(Ruleset::shared(), &bytes).expect("it loads").0
}

/// The major with the most cities, and the Worker's builder class.
fn subject(g: &Game) -> (PlayerId, BuilderClass) {
    let p = g
        .majors(true)
        .map(|p| p.id())
        .max_by_key(|&p| (g.player_cities(p).count(), std::cmp::Reverse(p)))
        .expect("a major");
    let r = g.rules();
    let worker: BaseUnitId = r.lookup("Worker").expect("the Worker");
    (p, r.base_units()[worker].builder.expect("a builder class"))
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

fn main() {
    let games: Vec<(String, Game)> =
        SMALL_FIXTURES.iter().map(|&(c, t)| (format!("{c}/t{t}"), fixture(c, t))).collect();
    let mut cr = Criterion::default().configure_from_args();
    let mut timings = Vec::new();
    for (name, g) in &games {
        let (p, class) = subject(g);
        // Once, so that the memos a job reads are warm.
        let with_job = jobs::rebuild_for_bench(g, p, class);
        println!(
            "{name}: player {} with {} cities, {} tiles with a job",
            p.0,
            g.player_cities(p).count(),
            with_job
        );
        cr.bench_function(&format!("jobmap_small/{name}"), |b| {
            b.iter(|| jobs::rebuild_for_bench(black_box(g), p, class));
        });
        let took = median(31, 20, || {
            black_box(jobs::rebuild_for_bench(black_box(g), p, class));
        });
        timings.push((name.clone(), took));
    }
    cr.final_summary();

    for (name, took) in &timings {
        println!("jobmap_small {name} median: {took:?} (budget {REBUILD:?})");
    }
    let (name, took) = timings.last().expect("the late fixture");
    println!(
        "jobmap_small (the gate, {name}) median: {took:?} (budget {REBUILD:?}, hard limit {:?})",
        REBUILD * 3
    );
    assert!(*took <= REBUILD * 3, "jobmap_small took {took:?}, over three times {REBUILD:?}");
    if *took > REBUILD {
        println!("warning: jobmap_small is over its {REBUILD:?} budget (report-only until 1e-03)");
    }
}
