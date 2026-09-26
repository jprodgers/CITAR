//! Instruction counts (DESIGN.md 9.7, the CI performance gate; package 1e-03): gungraun, the
//! successor of iai-callgrind, runs each benchmark once under valgrind's callgrind and counts
//! the instructions it executes, which shared runners reproduce to about 0.1% where their wall
//! clock varies by 10-20%.
//!
//! Eight kernels and two macro benchmarks, each on a committed fixture loaded in its setup (the
//! setup is not counted):
//! - `hex`: the tiles within 3 of 200 tiles of a gargantuan grid;
//! - `tile_yields`: the yield of every tile the late fixture's largest city works, recomputed;
//! - `city_stats`: that city's base, parts and stats recomputed;
//! - `assign_citizens`: that city grown to 20, its citizens assigned;
//! - `paths`: the 40 paths Python recorded on the late fixture, each a fresh mover and search;
//! - `sight`: ten steps of a unit of sight 2, each with its owner's sight brought up to date;
//! - `combat`: the preview of each attack the standard t120 fixture allows;
//! - `buildable`: what the late fixture's largest city can build, recomputed;
//! - `pass_round`: one pass round of the late fixture;
//! - `view`: a player's client view of the late fixture.
//!
//! `rust.yml`'s perf job runs the suite on the base branch (`--save-baseline=base`) and on the
//! pull request (`--baseline=base`); a benchmark whose instructions rise more than 5% fails the
//! job, unless the pull request carries the label `perf-accepted`. The kernels repeat their work
//! ten times; building with `CITAR_IAI_SYNTHETIC=1` in the environment makes it eleven, a
//! synthetic +10% regression that the gate must catch (gate 5 of package 1e-03):
//!
//! ```text
//! cargo bench -p citar-bench --features iai --bench iai -- --save-baseline=base
//! CITAR_IAI_SYNTHETIC=1 cargo bench -p citar-bench --features iai --bench iai -- --baseline=base
//! ```
//!
//! Linux only (valgrind), with `gungraun-runner` 0.19.4 on the path.

// gungraun's harness ends the process with the runner's status: 3 on a regression.
#![allow(clippy::exit, reason = "the gungraun harness's main")]

use std::hint::black_box;

use citar_bench::fixtures;
use citar_engine::base::hex::HexGrid;
use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::game::cities::citizens;
use citar_engine::game::cities::construction::compute_buildable_for_bench;
use citar_engine::game::cities::stats::{self as cstats, Work};
use citar_engine::game::combat::resolve;
use citar_engine::game::path::Mover;
use citar_engine::game::tiles::CityMods;
use citar_engine::game::vis::{Sight, sight_of};
use citar_engine::game::{Game, tiles};
use citar_engine::state::Phase;
use citar_engine::unique::CondDeps;
use gungraun::{
    Callgrind, EventKind, LibraryBenchmarkConfig, library_benchmark, library_benchmark_group, main,
};
use serde_json::json;

/// How many times a kernel repeats its work: ten, or eleven in the synthetic regression build.
const REPS: usize = if option_env!("CITAR_IAI_SYNTHETIC").is_some() { 11 } else { 10 };

/// The late fixture and its largest city that works a tile.
fn late_city() -> (Game, CityId) {
    let g = fixtures::late();
    let c = g
        .state()
        .cities()
        .iter()
        .filter(|c| !c.worked.is_empty())
        .max_by_key(|c| (c.pop, std::cmp::Reverse(c.id())))
        .map(|c| c.id())
        .expect("a city");
    (g, c)
}

/// The late fixture's largest city with its tile modifiers.
fn late_city_mods() -> (Game, CityId, CityMods) {
    let (g, c) = late_city();
    let mods = tiles::city_mods(&g, c);
    (g, c, mods)
}

/// The late fixture with its largest city grown to 20.
fn late_city_of_20() -> (Game, CityId) {
    let (mut g, c) = late_city();
    g.apply_ops(&json!([{"op": "set_city", "city": c.get(), "pop": 20}])).expect("a city of 20");
    (g, c)
}

/// The paths Python recorded on the late fixture: (unit, target).
fn late_paths() -> (Game, Vec<(UnitId, TileIdx)>) {
    let f = fixtures::find(&fixtures::committed(), fixtures::LATE.0, fixtures::LATE.1)
        .expect("the late fixture");
    let g = fixtures::load(&f);
    let rec = fixtures::recorded(&f, "movement");
    let pairs = rec
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|e| {
                    let u = UnitId::new(u32::try_from(e.get("unit")?.as_u64()?).ok()?)?;
                    let to = TileIdx(u32::try_from(e.get("to")?.as_u64()?).ok()?);
                    g.unit(u)?;
                    Some((u, to))
                })
                .collect()
        })
        .unwrap_or_default();
    (g, pairs)
}

/// The late fixture and a unit of a major that sees at 2, with a free land tile beside it.
fn late_walker() -> (Game, UnitId, TileIdx, TileIdx) {
    let g = fixtures::late();
    let found = g.state().units().iter().find_map(|u| {
        if sight_of(&g, u.id()) != Some(Sight::Walk(2)) || !g.player(u.owner())?.is_major() {
            return None;
        }
        let to = g
            .grid()
            .neighbors(u.tile())
            .find(|&n| g.is_land(n) && g.units_at(n).next().is_none() && g.city_at(n).is_none())?;
        Some((u.id(), u.tile(), to))
    });
    let (u, a, b) = found.expect("a walker");
    // Both tiles' surroundings seen once, so that every step the benchmark takes costs the same.
    let mut g = g;
    for to in [b, a] {
        g.step_unit_for_test(u, to).expect("a step");
    }
    (g, u, a, b)
}

/// The standard t120 fixture and every attack its units may make, readied.
fn fights() -> (Game, Vec<(UnitId, TileIdx)>) {
    let mut g = fixtures::committed_game("standard-pangaea-normal-s1031", 120);
    let mut out = Vec::new();
    for u in g.state().units().iter() {
        let def = &g.rules().base_units()[u.base];
        if !def.military {
            continue;
        }
        for t in g.grid().within(u.tile(), 2) {
            if t != u.tile()
                && g.derived().vis().sees(u.owner(), t)
                && resolve::contains_attackable_enemy(
                    &g,
                    t,
                    citar_engine::unique::Combatant::Unit(u.id()),
                )
                .is_none()
            {
                out.push((u.id(), t));
            }
        }
    }
    let ready: Vec<serde_json::Value> =
        out.iter().map(|&(u, _)| json!({"op": "ready_unit", "unit": u.get()})).collect();
    citar_engine::api::testops::apply(&mut g, &serde_json::Value::Array(ready)).expect("readied");
    out.retain(|&(u, t)| resolve::preview_of(&g, u, t).is_ok());
    (g, out)
}

// Each benchmark hands its input back, so that dropping the game is not counted, and repeats
// work that costs the same each time, so that one repetition more is a tenth more.

#[library_benchmark]
#[bench::gargantuan(HexGrid::new(160, 100, true, false).expect("a grid"))]
fn hex(grid: HexGrid) -> (HexGrid, usize) {
    let tiles: Vec<TileIdx> = grid.tiles().step_by(80).collect();
    let mut out = Vec::with_capacity(64);
    let mut n = 0;
    for _ in 0..REPS {
        for &t in &tiles {
            grid.within_into(black_box(t), 3, &mut out);
            n += out.len();
        }
    }
    (grid, n)
}

#[library_benchmark]
#[bench::late(setup = late_city_mods)]
fn tile_yields(input: (Game, CityId, CityMods)) -> ((Game, CityId, CityMods), f64) {
    let mut sum = 0.0;
    {
        let (g, c, mods) = (&input.0, input.1, &input.2);
        let city = g.city(c).expect("the city");
        let owner = city.owner();
        for _ in 0..REPS {
            for &t in &city.worked {
                let mut deps = CondDeps::empty();
                let y = tiles::compute_tile_yield(
                    g,
                    black_box(t),
                    Some(owner),
                    Some(c),
                    Some(mods),
                    &mut deps,
                );
                sum += y[citar_engine::base::stats::Stat::Food];
            }
        }
    }
    (input, sum)
}

#[library_benchmark]
#[bench::late(setup = late_city)]
fn city_stats(input: (Game, CityId)) -> ((Game, CityId), f64) {
    let mut sum = 0.0;
    {
        let (g, c) = (&input.0, input.1);
        let city = g.city(c).expect("the city");
        let work = Work::of(city);
        for _ in 0..REPS {
            black_box(cstats::city_base(g, black_box(c)));
            let parts = cstats::city_parts(g, c, &work);
            let construction = cstats::current_construction(city);
            let s = cstats::city_stats_from(g, c, &parts, &work, construction, None);
            sum += s.total[citar_engine::base::stats::Stat::Production];
        }
    }
    (input, sum)
}

#[library_benchmark]
#[bench::late(setup = late_city_of_20)]
fn assign_citizens(input: (Game, CityId)) -> ((Game, CityId), usize) {
    let (g, c) = (&input.0, input.1);
    let n = (0..REPS)
        .map(|_| black_box(citizens::assign(g, black_box(c), false)).map_or(0, |a| a.worked.len()))
        .sum();
    (input, n)
}

#[library_benchmark]
#[bench::late(setup = late_paths)]
fn paths(input: (Game, Vec<(UnitId, TileIdx)>)) -> ((Game, Vec<(UnitId, TileIdx)>), usize) {
    let mut n = 0;
    {
        let (g, pairs) = (&input.0, &input.1);
        for _ in 0..REPS {
            for &(u, t) in pairs {
                n += Mover::unit(g, u)
                    .and_then(|m| m.find_path(black_box(t), 40))
                    .map_or(0, |p| p.len());
            }
        }
    }
    (input, n)
}

#[library_benchmark]
#[bench::late(setup = late_walker)]
fn sight(input: (Game, UnitId, TileIdx, TileIdx)) -> ((Game, UnitId, TileIdx, TileIdx), usize) {
    let (mut g, u, a, b) = input;
    let mut n = 0;
    for i in 0..REPS {
        let to = if i % 2 == 0 { b } else { a };
        n += g.step_unit_for_test(u, black_box(to)).expect("a step").len();
    }
    ((g, u, a, b), n)
}

#[library_benchmark]
#[bench::standard(setup = fights)]
fn combat(input: (Game, Vec<(UnitId, TileIdx)>)) -> ((Game, Vec<(UnitId, TileIdx)>), usize) {
    let mut n = 0;
    {
        let (g, pairs) = (&input.0, &input.1);
        for _ in 0..REPS {
            for &(u, t) in pairs {
                n += usize::from(resolve::preview_of(g, u, black_box(t)).is_ok());
            }
        }
    }
    (input, n)
}

#[library_benchmark]
#[bench::late(setup = late_city)]
fn buildable(input: (Game, CityId)) -> ((Game, CityId), usize) {
    let (g, c) = (&input.0, input.1);
    let n = (0..REPS)
        .map(|_| {
            let b = compute_buildable_for_bench(g, black_box(c));
            b.units.len() + b.buildings.len() + b.wonders.len()
        })
        .sum();
    (input, n)
}

#[library_benchmark]
#[bench::late(setup = fixtures::late)]
fn pass_round(mut g: Game) -> Game {
    let start = g.turn();
    for _ in 0..=g.state().players().len() + 1 {
        if g.phase() != Phase::Playing || g.turn() != start {
            break;
        }
        if g.end_turn(g.current()).is_err() {
            break;
        }
    }
    g
}

#[library_benchmark]
#[bench::late(setup = fixtures::late)]
fn view(g: Game) -> (Game, usize) {
    let pid = g.majors(true).map(|p| p.id()).next().unwrap_or(PlayerId(0));
    let n = g.view_json(Some(pid), 150).len();
    (g, n)
}

library_benchmark_group!(
    name = kernels,
    benchmarks = [hex, tile_yields, city_stats, assign_citizens, paths, sight, combat, buildable]
);
library_benchmark_group!(name = macros, benchmarks = [pass_round, view]);

main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().soft_limits([(EventKind::Ir, 5.0)]));
    library_benchmark_groups = kernels, macros
);
