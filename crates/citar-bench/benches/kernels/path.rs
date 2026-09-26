//! Paths (packages 1c-02, 1e-03): the best path on small maps, across fog on a gargantuan one,
//! over the pairs refcheck recorded, and what a unit reaches this turn, each timed as a caller
//! pays for it.
//!
//! - `astar_small_30`: the committed small maps, loaded through the Python converter (roads,
//!   cities, borders, units): a mid-game one (`small-pangaea-raging/t50`), a scenario
//!   (`scenario-small-continents-s3001/t61`) and a late one (`small-continents-normal-s1025/t280`,
//!   where a worker's paths bend round bays and its search's bound falls four turns short). On
//!   each, the land unit of a major with the most land targets 25 to 35 tiles away that it has a
//!   path to, to 16 of them in turn. Each is printed; the budget holds their mean. Budget 20 µs.
//!   A search reads what an earlier search of the same unit at the same revision found of each
//!   tile; `astar_small_30/each_after_a_write` (report-only) times the same searches with a write
//!   before each, so that none does.
//! - `astar_garg_fog`: a new gargantuan game (seed 1), whose starting units know only what they
//!   see: paths of a settler to land targets 50 to 58 tiles away (DESIGN.md 10: 54 tiles),
//!   through fog, which is passable at its true cost. Budget 150 µs.
//! - `astar/recorded_pairs`: every path Python's `movement` group recorded on the twelve committed
//!   fixtures (the unit and the target of `queries.movement.paths`, 40 a state), each searched in
//!   turn; the budget holds the mean of one. Budget 50 µs.
//! - `reachable`: `movement::reachable_this_turn` for each of the late fixture's units with
//!   moves. Budget 5 µs.
//!
//! A path is what `movement::find_path` does when the path cache of the revision does not have
//! it: a mover built for the unit (its profile, its civilization's rules and the zones of
//! control, from the game's memos, `game::path::memo`), then the search. Of those memos a unit's
//! step moves only the zones of control (where units stand); `reachable after a step at war`
//! prints `reachable` for a unit whose civilization is at war right after another of its units
//! stepped, the zones built again.

use std::hint::black_box;
use std::ops::RangeInclusive;
use std::time::Duration;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::path::Mover;
use citar_engine::game::{Game, movement};
use citar_engine::rules::Ruleset;
use criterion::Criterion;

/// The committed small-map fixtures, as (case, turn).
const SMALL_FIXTURES: [(&str, u32); 3] = [
    ("small-pangaea-raging", 50),
    ("scenario-small-continents-s3001", 61),
    ("small-continents-normal-s1025", 280),
];

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

/// Every committed fixture's game with the paths Python recorded on it: (unit, target) of each
/// `queries.movement.paths` entry whose unit the game has.
fn recorded_pairs() -> Vec<(Game, Vec<(UnitId, TileIdx)>)> {
    fixtures::committed()
        .iter()
        .map(|f| {
            let g = fixtures::load(f);
            let rec = fixtures::recorded(f, "movement");
            let pairs = rec
                .get("paths")
                .and_then(serde_json::Value::as_array)
                .map(|all| {
                    all.iter()
                        .filter_map(|e| {
                            let u = u32::try_from(e.get("unit")?.as_u64()?).ok()?;
                            let to = u32::try_from(e.get("to")?.as_u64()?).ok()?;
                            let u = UnitId::new(u)?;
                            g.unit(u)?;
                            Some((u, TileIdx(to)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (g, pairs)
        })
        .collect()
}

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let smalls: Vec<Game> =
        SMALL_FIXTURES.iter().map(|&(c, t)| fixtures::committed_game(c, t)).collect();
    let picked: Vec<(UnitId, Vec<TileIdx>)> =
        smalls.iter().map(|g| searches(g, None, 25..=35)).collect();
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

    let recorded = recorded_pairs();
    let all_pairs: Vec<(usize, UnitId, TileIdx)> = recorded
        .iter()
        .enumerate()
        .flat_map(|(i, (_, ps))| ps.iter().map(move |&(u, t)| (i, u, t)))
        .collect();
    let mut q = 0usize;
    let mut search_recorded = || {
        q = (q + 1) % all_pairs.len();
        let (i, u, t) = all_pairs[q];
        black_box(search(&recorded[i].0, u, t));
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
        "astar_garg_fog: unit {} to {} targets; recorded pairs: {} on {} fixtures; reachable: {} \
         units",
        gu.get(),
        gt.len(),
        all_pairs.len(),
        recorded.len(),
        units.len()
    );

    c.bench_function("astar_small_30", |b| b.iter(&mut search_small));
    c.bench_function("astar_garg_fog", |b| b.iter(&mut search_garg));
    c.bench_function("astar/recorded_pairs", |b| b.iter(&mut search_recorded));
    c.bench_function("reachable", |b| b.iter(&mut reach));

    let mut total = Duration::ZERO;
    for ((case, turn), (g, (u, ts))) in SMALL_FIXTURES.iter().zip(smalls.iter().zip(&picked)) {
        let took = per_search(g, *u, ts, 31);
        total += took;
        s.note(&format!("astar_small_30/{case}/t{turn}"), took);
    }
    s.put("astar_small_30", total / 3);
    // The same searches, each after a write: the revision moves, so no search reads the looks of
    // the one before it (package 1e-03's reuse of a mover's looks at one revision).
    let mut cold = Duration::ZERO;
    for (g, (u, ts)) in smalls.iter().zip(&picked) {
        let mut g = g.clone();
        let mine = g.unit(*u).map(citar_engine::state::units::Unit::owner);
        let other = g.majors(true).map(|p| p.id()).find(|&p| Some(p) != mine).expect("another");
        let n = u32::try_from(ts.len()).unwrap_or(1).max(1);
        cold += median(11, 10, || {
            for &t in ts {
                g.unrelated_change_for_bench(other);
                black_box(search(&g, *u, t));
            }
        }) / n;
    }
    s.note("astar_small_30/each_after_a_write", cold / 3);
    s.put("astar_garg_fog", median(31, 50, &mut search_garg));
    let n = u32::try_from(all_pairs.len()).unwrap_or(1).max(1);
    s.put(
        "astar/recorded_pairs",
        median(11, 1, || {
            for _ in 0..n {
                search_recorded();
            }
        }) / n,
    );
    s.put("reachable", median(31, 1_000, &mut reach));

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
        s.note("reachable/after_a_step_at_war", both.saturating_sub(alone));
    }
}
