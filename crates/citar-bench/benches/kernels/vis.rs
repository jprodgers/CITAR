//! Visibility (packages 1c-01, 1e-03): a unit's step with its owner's sight brought up to date,
//! the whole sight rebuilt, and line of sight worked out without the cache.
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter. The step moves a military land unit back and forth between two
//! tiles, each move followed by the sight part of a settle (footprint, counts, explored tiles,
//! memories, contacts): Python recomputed every civilization's sight there, at 0.9-8.6 ms.
//! - `vis_step/sight2`: a unit of sight 2, both tiles' footprints in the line-of-sight cache after
//!   the first two steps, as a unit pacing in place finds them. Budget 1.5 µs.
//! - `vis_step/sight2_fresh`: the same, the cache forgotten before each step (untimed), so each
//!   step walks its new footprint and allocates it, as a step onto a tile no unit saw from lately
//!   does. Budget 1.5 µs.
//! - `vis_step/sight5`: a Conquistador (`[+2] Sight`) with Sentry (`[+1] Sight`) added to the
//!   largest civilization, sight 5 by the walk, stepping the same way. Budget 4 µs.
//! - `los/sight3_uncached`: the elevation walk at sight 3 from each of 64 tiles in turn. Budget
//!   1 µs.
//!
//! Report-only: `vis/rebuild`, every civilization's sight worked out afresh from the state (what
//! a load does).

use std::hint::black_box;
use std::time::Instant;

use citar_bench::{Suite, fixtures, median, median_timed};
use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::vis::los::{Heights, LosScratch, viewable_into};
use citar_engine::game::vis::{Sight, sight_of};
use criterion::Criterion;
use serde_json::json;

/// A free land tile next to `from`: no unit, no city.
fn free_neighbour(g: &Game, from: TileIdx) -> Option<TileIdx> {
    g.grid()
        .neighbors(from)
        .find(|&n| g.is_land(n) && g.units_at(n).next().is_none() && g.city_at(n).is_none())
}

/// A unit of a major that sees by the walk at radius `r`, and a land tile next to it with no unit
/// on it.
fn walker(g: &Game, r: u32) -> Option<(UnitId, TileIdx, TileIdx)> {
    g.state().units().iter().find_map(|u| {
        if sight_of(g, u.id()) != Some(Sight::Walk(r)) || !g.player(u.owner())?.is_major() {
            return None;
        }
        Some((u.id(), u.tile(), free_neighbour(g, u.tile())?))
    })
}

/// A Conquistador with Sentry for the largest major, on a free land tile of its land with a free
/// land neighbour and no hill (so that it sees at 5): the unit, its tile and the neighbour.
fn add_sight5(g: &mut Game) -> (UnitId, TileIdx, TileIdx) {
    let p: PlayerId = g
        .majors(true)
        .map(|x| x.id())
        .max_by_key(|&p| (g.state().cities().of(p).len(), std::cmp::Reverse(p)))
        .expect("a major");
    let spots: Vec<(TileIdx, TileIdx)> = g
        .grid()
        .tiles()
        .filter(|&t| g.state().tiles().get(t).and_then(|x| x.owner()) == Some(p))
        .filter(|&t| g.is_land(t) && g.units_at(t).next().is_none() && g.city_at(t).is_none())
        .filter_map(|t| Some((t, free_neighbour(g, t)?)))
        .collect();
    for (at, to) in spots {
        let (x, y) = g.grid().xy(at);
        let before: Vec<UnitId> = g.state().units().of(p).to_vec();
        g.apply_ops(&json!([{
            "op": "add_unit", "player": p.0, "unit": "Conquistador", "x": x, "y": y,
            "promotions": ["Sentry"],
        }]))
        .expect("a unit added");
        let u = g.state().units().of(p).iter().copied().find(|u| !before.contains(u)).expect("it");
        if sight_of(g, u) == Some(Sight::Walk(5)) {
            return (u, at, to);
        }
    }
    panic!("no tile where a Conquistador with Sentry sees at 5");
}

/// A step back and forth between `a` and `b`, each followed by the sight part of a settle.
fn stepper(u: UnitId, a: TileIdx, b: TileIdx) -> impl FnMut(&mut Game) {
    let mut there = false;
    move |g: &mut Game| {
        there = !there;
        black_box(g.step_unit_for_test(u, if there { b } else { a }).expect("a step"));
    }
}

/// One step whose footprint the line-of-sight cache does not hold: the cache is forgotten first,
/// untimed.
fn fresh(g: &mut Game, step: &mut impl FnMut(&mut Game)) -> std::time::Duration {
    g.derived().vis().forget_line_of_sight();
    let t = Instant::now();
    step(g);
    t.elapsed()
}

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let mut g = fixtures::late();
    let (u2, a2, b2) = walker(&g, 2).expect("a unit of sight 2 with room to step");
    let (u5, a5, b5) = add_sight5(&mut g);
    let mut step2 = stepper(u2, a2, b2);
    let mut step5 = stepper(u5, a5, b5);
    // Both tiles' surroundings explored once, as they are on a unit's second step.
    for _ in 0..2 {
        step2(&mut g);
        step5(&mut g);
    }
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
    c.bench_function("vis_step/sight2", |bch| bch.iter(|| step2(&mut g)));
    c.bench_function("vis_step/sight2_fresh", |bch| {
        bch.iter_custom(|n| (0..n).map(|_| fresh(&mut g, &mut step2)).sum());
    });
    c.bench_function("vis_step/sight5", |bch| bch.iter(|| step5(&mut g)));
    c.bench_function("los/sight3_uncached", |bch| bch.iter(&mut walk));
    s.put("vis_step/sight2", median(31, 1_000, || step2(&mut g)));
    s.put(
        "vis_step/sight2_fresh",
        median_timed(31, || {
            (0..1_000).map(|_| fresh(&mut g, &mut step2)).sum::<std::time::Duration>() / 1_000
        }),
    );
    s.put("vis_step/sight5", median(31, 1_000, || step5(&mut g)));
    s.put("los/sight3_uncached", median(31, 1_000, &mut walk));
    s.note(
        "vis/rebuild",
        median(11, 3, || {
            black_box(g.sight_rebuild_for_bench());
        }),
    );
}
