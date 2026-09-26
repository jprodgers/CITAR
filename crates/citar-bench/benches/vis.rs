//! Visibility (package 1c-01, gate 5): a unit's step with its owner's sight brought up to date,
//! and line of sight worked out without the cache.
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter. The step moves a military land unit of sight 2 back and forth
//! between two tiles, each move followed by the sight part of a settle (footprint, counts,
//! explored tiles, memories, contacts): Python recomputed every civilization's sight there, at
//! 0.9-8.6 ms. `vis_step/sight2` finds both tiles' footprints in the line-of-sight cache after
//! the first two steps, as a unit pacing in place does; `vis_step/sight2_fresh` forgets the cache
//! before each step (untimed), so each step walks its new footprint and allocates it, as a step
//! onto a tile no unit saw from lately does. Line of sight is the elevation walk at sight 3 from
//! each of 64 tiles in turn. Budgets (DESIGN.md 10, report-only until 1e-03): each step at or
//! under 1.5 µs, the walk at or under 1 µs. After Criterion, the run takes the median of its own
//! timings and fails above three times a budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench vis
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::vis::los::{Heights, LosScratch, viewable_into};
use citar_engine::game::vis::{Sight, sight_of};
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures;
use criterion::Criterion;

const STEP: Duration = Duration::from_nanos(1_500);
const LOS: Duration = Duration::from_micros(1);

/// The late fixture's game.
fn late() -> Game {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == "small-continents-normal-s1025" && f.turn == 280)
        .expect("the late fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    g
}

/// A unit that sees at radius 2 by the walk, and a land tile next to it with no unit on it.
fn walker(g: &Game) -> (UnitId, TileIdx, TileIdx) {
    for u in g.state().units().iter() {
        if sight_of(g, u.id()) != Some(Sight::Walk(2)) || g.is_barbarian(u.owner()) {
            continue;
        }
        let from = u.tile();
        let to = g
            .grid()
            .neighbors(from)
            .find(|&n| g.is_land(n) && g.units_at(n).next().is_none() && g.city_at(n).is_none());
        if let Some(to) = to {
            return (u.id(), from, to);
        }
    }
    panic!("no unit of sight 2 with room to step");
}

/// The median of `n` timings of `f`, each of `batch` calls.
fn median(n: usize, batch: u32, mut f: impl FnMut()) -> Duration {
    median_of(n, batch, || {
        let t = Instant::now();
        f();
        t.elapsed()
    })
}

/// The median of `n` timings of `batch` calls of `f`, each call timing itself.
fn median_of(n: usize, batch: u32, mut f: impl FnMut() -> Duration) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let mut took = Duration::ZERO;
            for _ in 0..batch {
                took += f();
            }
            took / batch
        })
        .collect();
    times.sort();
    times[n / 2]
}

/// One step whose footprint the line-of-sight cache does not hold: the cache is forgotten first,
/// untimed.
fn fresh(g: &mut Game, step: &mut impl FnMut(&mut Game)) -> Duration {
    g.derived().vis().forget_line_of_sight();
    let t = Instant::now();
    step(g);
    t.elapsed()
}

fn check(name: &str, took: Duration, budget: Duration) {
    println!("{name} median: {took:?} (budget {budget:?}, hard limit {:?})", budget * 3);
    assert!(took <= budget * 3, "{name} took {took:?}, over three times {budget:?}");
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget (report-only until 1e-03)");
    }
}

fn main() {
    let mut g = late();
    let (u, a, b) = walker(&g);
    let mut there = false;
    let mut step = move |g: &mut Game| {
        there = !there;
        black_box(g.step_unit_for_test(u, if there { b } else { a }).expect("a step"));
    };
    // Both tiles' surroundings explored once, as they are on a unit's second step.
    step(&mut g);
    step(&mut g);
    let heights = Heights::new(g.rules(), g.state().tiles());
    let grid = g.grid().clone();
    let centres: Vec<TileIdx> = grid.tiles().step_by(7).take(64).collect();
    let mut scratch = LosScratch::default();
    let mut out = Vec::new();
    let mut k = 0usize;
    let mut walk = || {
        k = (k + 1) % centres.len();
        viewable_into(&grid, &heights, centres[k], 3, false, &mut scratch, &mut out);
        black_box(out.len());
    };
    let mut c = Criterion::default().configure_from_args();
    c.bench_function("vis_step/sight2", |bch| bch.iter(|| step(&mut g)));
    c.bench_function("vis_step/sight2_fresh", |bch| {
        bch.iter_custom(|n| (0..n).map(|_| fresh(&mut g, &mut step)).sum())
    });
    c.bench_function("los/sight3_uncached", |bch| bch.iter(&mut walk));
    c.final_summary();
    check("vis_step/sight2", median(31, 1_000, || step(&mut g)), STEP);
    check("vis_step/sight2_fresh", median_of(31, 1_000, || fresh(&mut g, &mut step)), STEP);
    check("los/sight3_uncached", median(31, 1_000, &mut walk), LOS);
}
