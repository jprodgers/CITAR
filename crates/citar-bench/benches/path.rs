//! Paths (package 1c-02, gate 6): the best path on a small map, across fog on a gargantuan one,
//! and what a unit reaches this turn.
//!
//! - `astar_small_30`: a small map in mid game, the committed fixture
//!   `small-pangaea-raging/t50` loaded through the Python converter (roads, cities, borders,
//!   units): paths of the land unit of a major with the most land targets 25 to 35 tiles away
//!   that it has a path to, to 16 of them in turn. Budget 20 µs. The run also prints the mean on
//!   the other committed small maps, for the report: `scenario-small-continents-s3001/t61`, and
//!   `small-continents-normal-s1025/t280`, late, where a worker's paths bend round bays it could
//!   cross only by embarking, which takes a whole turn each way: the bound, which cannot see
//!   the coast, leaves it two or three times slower.
//! - `astar_garg_fog`: a new gargantuan game (seed 1), whose starting units know only what they
//!   see: paths of a settler to land targets 50 to 58 tiles away (DESIGN.md 10: 54 tiles),
//!   through fog, which is passable at its true cost. Budget 150 µs.
//! - `reachable`: every tile a unit reaches this turn, for the late fixture's units with moves.
//!   Budget 5 µs.
//!
//! Each search goes to the searcher itself, past the cache of paths found at a revision. The
//! budgets are report-only until package 1e-03; after Criterion, the run takes the median of its
//! own timings and fails above three times a budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench path
//! ```

use std::hint::black_box;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::path::Mover;
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures;
use criterion::Criterion;

const SMALL: Duration = Duration::from_micros(20);
const GARG: Duration = Duration::from_micros(150);
const REACH: Duration = Duration::from_micros(5);

/// The committed small-map fixtures, as (case, turn): the first is the one timed against the
/// budget.
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
        let Some(m) = Mover::unit(g, u.id()) else { continue };
        let targets: Vec<TileIdx> = g
            .grid()
            .within(u.tile(), *far.end())
            .into_iter()
            .filter(|&t| far.contains(&g.grid().distance(u.tile(), t)) && g.is_land(t))
            .step_by(7)
            .filter(|&t| m.find_path(t, 40).is_some_and(|p| p.len() > *far.start() as usize))
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

fn main() {
    let smalls: Vec<Game> = SMALL_FIXTURES.iter().map(|&(c, t)| fixture(c, t)).collect();
    let picked: Vec<(UnitId, Vec<TileIdx>)> =
        smalls.iter().map(|g| searches(g, None, 25..=35)).collect();
    let small_movers: Vec<Mover<'_>> =
        smalls.iter().zip(&picked).map(|(g, (u, _))| Mover::unit(g, *u).expect("a unit")).collect();
    let (sm, st) = (&small_movers[0], &picked[0].1);
    let mut k = 0usize;
    let mut search_small = || {
        k = (k + 1) % st.len();
        black_box(sm.find_path(st[k], 40));
    };

    let garg = gargantuan();
    let (gu, gt) = searches(&garg, Some("Settler"), 50..=58);
    let gm = Mover::unit(&garg, gu).expect("a unit");
    let mut j = 0usize;
    let mut search_garg = || {
        j = (j + 1) % gt.len();
        black_box(gm.find_path(gt[j], 40));
    };

    let late = &smalls[2];
    let movers: Vec<Mover<'_>> = late
        .state()
        .units()
        .iter()
        .filter(|u| u.moves > 0 && late.player(u.owner()).is_some_and(|p| p.is_major()))
        .filter_map(|u| Mover::unit(late, u.id()))
        .collect();
    let mut i = 0usize;
    let mut reach = || {
        i = (i + 1) % movers.len();
        black_box(movers[i].reachable());
    };

    for (((case, turn), (u, ts)), m) in SMALL_FIXTURES.iter().zip(&picked).zip(&small_movers) {
        let took = median(11, 20, || {
            for &t in ts {
                black_box(m.find_path(t, 40));
            }
        }) / u32::try_from(ts.len()).unwrap_or(1);
        println!(
            "astar_small_30 on {case}/t{turn}: unit {} to {} targets, {took:?}",
            u.get(),
            ts.len()
        );
    }
    println!(
        "astar_garg_fog: unit {} to {} targets; reachable: {} units",
        gu.get(),
        gt.len(),
        movers.len()
    );

    let mut c = Criterion::default().configure_from_args();
    c.bench_function("astar_small_30", |b| b.iter(&mut search_small));
    c.bench_function("astar_garg_fog", |b| b.iter(&mut search_garg));
    c.bench_function("reachable", |b| b.iter(&mut reach));
    c.final_summary();
    check("astar_small_30", median(31, 200, &mut search_small), SMALL);
    check("astar_garg_fog", median(31, 50, &mut search_garg), GARG);
    check("reachable", median(31, 1_000, &mut reach), REACH);
}
