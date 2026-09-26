//! Paths (package 1c-02, gate 6): the best path on small maps, across fog on a gargantuan one,
//! and what a unit reaches this turn, each timed as a caller pays for it.
//!
//! - `astar_small_30`: the committed small maps, loaded through the Python converter (roads,
//!   cities, borders, units): a mid-game one (`small-pangaea-raging/t50`), a scenario
//!   (`scenario-small-continents-s3001/t61`) and a late one (`small-continents-normal-s1025/t280`,
//!   where a worker's paths bend round bays and its search's bound falls four turns short). On
//!   each, the land unit of a major with the most land targets 25 to 35 tiles away that it has a
//!   path to, to 16 of them in turn. Each is printed against the budget; the gate is their mean,
//!   what Criterion's `astar_small_30`, which takes the three in turn, measures. Budget 20 µs.
//! - `astar_garg_fog`: a new gargantuan game (seed 1), whose starting units know only what they
//!   see: paths of a settler to land targets 50 to 58 tiles away (DESIGN.md 10: 54 tiles),
//!   through fog, which is passable at its true cost. Budget 150 µs.
//! - `reachable`: `movement::reachable_this_turn` for each of the late fixture's units with
//!   moves. Budget 5 µs.
//!
//! A path is what `movement::find_path` does when the path cache of the revision does not have
//! it: a mover built for the unit (its profile, its civilization's rules and the zones of
//! control, from the game's memos, `game::path::memo`), then the search. Of those memos a unit's
//! step moves only the zones of control (where units stand); `reachable after a step at war`
//! prints `reachable` for a unit whose civilization is at war right after another of its units
//! stepped, the zones built again. The budgets are report-only until package 1e-03; after
//! Criterion, the run takes the median of its own timings and fails above three times a budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench path
//! ```

use std::hint::black_box;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::path::Mover;
use citar_engine::game::{Game, movement};
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures;
use criterion::Criterion;

const SMALL: Duration = Duration::from_micros(20);
const GARG: Duration = Duration::from_micros(150);
const REACH: Duration = Duration::from_micros(5);

/// The committed small-map fixtures, as (case, turn).
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
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    g
}

/// A new gargantuan game, at its first turn.
fn gargantuan() -> Game {
    let r = Ruleset::shared();
    let cfg = br#"{"map_size": "gargantuan", "seed": 1, "players": [{}, {}, {}, {}, {}, {}]}"#;
    let setup = Game::config_from_json(r, cfg).expect("settings");
    let (g, _) = Game::new(r, &setup).expect("a new game");
    g
}

/// A path as `movement::find_path` finds one the cache does not hold: a mover, then the search.
fn search(g: &Game, u: UnitId, t: TileIdx) -> Option<Vec<TileIdx>> {
    Mover::unit(g, u)?.find_path(t, 40)
}

/// (unit, targets): a land unit of a major and the land tiles `far` tiles from it that it has a
/// path to (of more steps than the nearest of `far`), at most 16 of them; the unit with the most
/// such targets.
fn searches(g: &Game, unit_type: Option<&str>, far: RangeInclusive<u32>) -> (UnitId, Vec<TileIdx>) {
    let mut best: Option<(UnitId, Vec<TileIdx>)> = None;
    for u in g.state().units().iter() {
        let owner = g.player(u.owner());
        if !owner.is_some_and(|p| p.is_major()) || !g.is_land(u.tile()) {
            continue;
        }
        if unit_type.is_some_and(|t| g.rules().name(u.base) != Some(t)) {
            continue;
        }
        let targets: Vec<TileIdx> = g
            .grid()
            .within(u.tile(), *far.end())
            .into_iter()
            .filter(|&t| far.contains(&g.grid().distance(u.tile(), t)) && g.is_land(t))
            .step_by(7)
            .filter(|&t| search(g, u.id(), t).is_some_and(|p| p.len() > *far.start() as usize))
            .take(16)
            .collect();
        if best.as_ref().is_none_or(|(_, b)| targets.len() > b.len()) {
            best = Some((u.id(), targets));
        }
    }
    let (u, t) = best.expect("a unit with far targets");
    assert!(!t.is_empty(), "far targets with a path");
    (u, t)
}

/// The median of `n` timings of `batch` calls of `f`.
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

/// The median time of one search from `u` to one of `ts`, each in turn.
fn per_search(g: &Game, u: UnitId, ts: &[TileIdx], n: usize) -> Duration {
    median(n, 10, || {
        for &t in ts {
            black_box(search(g, u, t));
        }
    }) / u32::try_from(ts.len()).unwrap_or(1)
}

/// A unit of a major at war that can step back and forth between two land tiles, and another
/// unit of the same player with moves: (the stepper, its tile, the tile it steps to, the other).
fn step_pair(g: &Game, units: &[UnitId]) -> Option<(UnitId, TileIdx, TileIdx, UnitId)> {
    units.iter().find_map(|&u| {
        let x = g.unit(u)?;
        if g.state().diplo().war_mask(x.owner()).is_empty() {
            return None;
        }
        let to = g
            .grid()
            .neighbors(x.tile())
            .find(|&n| g.is_land(n) && g.units_at(n).next().is_none() && g.city_at(n).is_none())?;
        let other = units
            .iter()
            .copied()
            .find(|&o| o != u && g.unit(o).is_some_and(|y| y.owner() == x.owner()))?;
        Some((u, x.tile(), to, other))
    })
}

fn main() {
    let smalls: Vec<Game> = SMALL_FIXTURES.iter().map(|&(c, t)| fixture(c, t)).collect();
    let picked: Vec<(UnitId, Vec<TileIdx>)> =
        smalls.iter().map(|g| searches(g, None, 25..=35)).collect();
    for ((case, turn), (g, (u, ts))) in SMALL_FIXTURES.iter().zip(smalls.iter().zip(&picked)) {
        println!(
            "astar_small_30 on {case}/t{turn}: unit {} to {} targets, {:?}",
            u.get(),
            ts.len(),
            per_search(g, *u, ts, 11)
        );
    }
    let mut k = 0usize;
    let mut search_small = || {
        k = (k + 1) % (3 * 16);
        let (g, (u, ts)) = (&smalls[k % 3], &picked[k % 3]);
        black_box(search(g, *u, ts[(k / 3) % ts.len()]));
    };

    let garg = gargantuan();
    let (gu, gt) = searches(&garg, Some("Settler"), 50..=58);
    let mut j = 0usize;
    let mut search_garg = || {
        j = (j + 1) % gt.len();
        black_box(search(&garg, gu, gt[j]));
    };

    let late = &smalls[2];
    let units: Vec<UnitId> = late
        .state()
        .units()
        .iter()
        .filter(|u| u.moves > 0 && late.player(u.owner()).is_some_and(|p| p.is_major()))
        .map(|u| u.id())
        .collect();
    let mut i = 0usize;
    let mut reach = || {
        i = (i + 1) % units.len();
        black_box(movement::reachable_this_turn(late, units[i]));
    };
    println!(
        "astar_garg_fog: unit {} to {} targets; reachable: {} units",
        gu.get(),
        gt.len(),
        units.len()
    );

    // What `reachable` costs right after another unit's step: a step moves where units stand,
    // which the zones of control read, so the next mover of a player at war builds them again.
    if let Some((stepper, back, to, other)) = step_pair(late, &units) {
        let mut g = late.clone();
        let mut at = [back, to];
        let mut step = |g: &mut Game| {
            at.swap(0, 1);
            black_box(g.step_unit_for_test(stepper, at[0]).is_ok());
        };
        let both = median(11, 200, || {
            step(&mut g);
            black_box(movement::reachable_this_turn(&g, other));
        });
        let alone = median(11, 200, || step(&mut g));
        println!(
            "reachable after a step at war: {:?} (a step and the search {both:?}, the step {alone:?})",
            both.saturating_sub(alone)
        );
    }

    let mut c = Criterion::default().configure_from_args();
    c.bench_function("astar_small_30", |b| b.iter(&mut search_small));
    c.bench_function("astar_garg_fog", |b| b.iter(&mut search_garg));
    c.bench_function("reachable", |b| b.iter(&mut reach));
    c.final_summary();
    let mut total = Duration::ZERO;
    for ((case, turn), (g, (u, ts))) in SMALL_FIXTURES.iter().zip(smalls.iter().zip(&picked)) {
        let took = per_search(g, *u, ts, 31);
        total += took;
        let over = if took > SMALL { " (over the budget)" } else { "" };
        println!("astar_small_30 on {case}/t{turn} median: {took:?}{over}");
    }
    check("astar_small_30 (the three fixtures' mean)", total / 3, SMALL);
    check("astar_garg_fog", median(31, 50, &mut search_garg), GARG);
    check("reachable", median(31, 1_000, &mut reach), REACH);
}
