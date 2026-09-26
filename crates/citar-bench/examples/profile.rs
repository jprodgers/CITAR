//! A workload to profile, without Criterion (package 1e-03's tuning): one part of the suites,
//! once to warm the memos, then `n` times inside [`measured`], for callgrind or a sampling
//! profiler.
//!
//! ```text
//! cargo build --profile profiling -p citar-bench --example profile
//! valgrind --tool=callgrind --toggle-collect='*measured*' <target>/profiling/examples/profile advisor 3
//! callgrind_annotate --inclusive=yes callgrind.out.<pid> | head -60
//! ```
//!
//! Parts: `advisor` (every city of the small t200 state, else the late fixture), `barbarians`
//! (stage S0 of the small raging t120 state, else the committed raging t50, each on a fresh
//! copy), `astar` (the three small maps' searches, each looking at every tile afresh), `combat` (every preview of
//! the standard t120 fixture), `pass_round` (the late fixture, or the state named by a third
//! argument `<case>/t<turn>` of the committed fixtures or the corpus, each on a fresh copy after
//! one round), `load` (the late fixture's save loaded), `digest` (its digest), `vis` (a step of
//! sight 2 whose line of sight is not cached).

#![allow(clippy::print_stdout, reason = "a tool")]

use std::hint::black_box;

use citar_bench::fixtures;
use citar_engine::base::ids::{PlayerId, TileIdx, UnitId};
use citar_engine::game::advisor::{self, AdvisorParams};
use citar_engine::game::combat::resolve;
use citar_engine::game::path::Mover;
use citar_engine::game::vis::{Sight, sight_of};
use citar_engine::game::{Game, barbarians, units};
use citar_engine::save::Digester;
use citar_engine::state::Phase;

/// Runs `f` `n` times: the only function a toggled profile collects in.
#[inline(never)]
fn measured(n: usize, mut f: impl FnMut()) {
    for _ in 0..n {
        f();
    }
}

fn pass_round(g: &mut Game) {
    let start = g.turn();
    for _ in 0..=g.state().players().len() + 1 {
        if g.phase() != Phase::Playing || g.turn() != start {
            return;
        }
        if g.end_turn(g.current()).is_err() {
            return;
        }
    }
}

fn state(name: &str) -> Game {
    let (case, turn) = name.rsplit_once("/t").expect("<case>/t<turn>");
    let turn: u32 = turn.parse().expect("a turn");
    let all: Vec<_> = fixtures::committed().into_iter().chain(fixtures::corpus()).collect();
    fixtures::load(&fixtures::find(&all, case, turn).expect("no such state"))
}

/// Runs `f` once to warm up, then `n` times measured, and prints the wall clock of one.
fn run(n: usize, mut f: impl FnMut()) {
    f();
    let t = std::time::Instant::now();
    measured(n, f);
    let each = t.elapsed() / u32::try_from(n.max(1)).unwrap_or(1);
    println!("{each:?} a run");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let part = args.first().map_or("advisor", String::as_str);
    let n: usize = args.get(1).and_then(|x| x.parse().ok()).unwrap_or(3);
    match part {
        "advisor" => {
            let g = fixtures::corpus_game("small-continents-normal-s1025", 200)
                .map_or_else(fixtures::late, |(g, _)| g);
            let pp = AdvisorParams::auto_production();
            let all: Vec<(PlayerId, _)> = g
                .majors(true)
                .flat_map(|p| g.player_cities(p.id()).map(move |c| (p.id(), c.id())))
                .collect();
            run(n, || {
                for &(p, c) in &all {
                    black_box(advisor::advise_production(&g, p, c, &pp));
                }
            });
        }
        "barbarians" => {
            let g = fixtures::corpus_game("small-continents-raging-s1005", 120)
                .map_or_else(|| fixtures::committed_game("small-pangaea-raging", 50), |(g, _)| g);
            let bid = g.barbarian_id().expect("the barbarians");
            let copies: Vec<Game> = (0..=n).map(|_| g.clone()).collect();
            let mut it = copies.into_iter();
            run(n, || {
                let mut c = it.next().expect("a copy");
                units::turn::start_units(&mut c, bid);
                barbarians::take_turn(&mut c);
                c.settle_for_bench();
                black_box(c);
            });
        }
        "astar" => {
            let games: Vec<Game> = [
                ("small-pangaea-raging", 50),
                ("scenario-small-continents-s3001", 61),
                ("small-continents-normal-s1025", 280),
            ]
            .iter()
            .map(|&(c, t)| fixtures::committed_game(c, t))
            .collect();
            let jobs: Vec<(usize, UnitId, Vec<TileIdx>)> = games
                .iter()
                .enumerate()
                .filter_map(|(i, g)| {
                    let u = g.state().units().iter().find(|u| {
                        g.player(u.owner()).is_some_and(|p| p.is_major()) && g.is_land(u.tile())
                    })?;
                    let ts: Vec<TileIdx> = g
                        .grid()
                        .within(u.tile(), 35)
                        .into_iter()
                        .filter(|&t| g.grid().distance(u.tile(), t) >= 25 && g.is_land(t))
                        .step_by(7)
                        .take(16)
                        .collect();
                    Some((i, u.id(), ts))
                })
                .collect();
            run(n, || {
                for (i, u, ts) in &jobs {
                    for &t in ts {
                        // Each search looks at every tile afresh, as `astar_small_30` times it.
                        games[*i].forget_path_looks_for_bench();
                        black_box(Mover::unit(&games[*i], *u).and_then(|m| m.find_path(t, 40)));
                    }
                }
            });
        }
        "combat" => {
            let g = fixtures::committed_game("standard-pangaea-normal-s1031", 120);
            let pairs: Vec<(UnitId, TileIdx)> = g
                .state()
                .units()
                .iter()
                .flat_map(|u| g.grid().within(u.tile(), 2).into_iter().map(move |t| (u.id(), t)))
                .filter(|&(u, t)| resolve::preview_of(&g, u, t).is_ok())
                .collect();
            run(n * 100, || {
                for &(u, t) in &pairs {
                    black_box(resolve::preview_of(&g, u, t).ok());
                }
            });
        }
        "pass_round" => {
            let mut warm = args.get(2).map_or_else(fixtures::late, |s| state(s));
            pass_round(&mut warm);
            let copies: Vec<Game> = (0..=n).map(|_| warm.clone()).collect();
            let mut it = copies.into_iter();
            run(n, || {
                let mut c = it.next().expect("a copy");
                pass_round(&mut c);
                black_box(c);
            });
        }
        "load" => {
            let mut g = fixtures::late();
            let chunk = g.take_journal_chunk().expect("a journal").map(|c| c.json);
            let json = g.snapshot().to_json().expect("saves");
            run(n, || {
                let mut it = chunk.iter().map(Vec::as_slice);
                black_box(Game::load(g.rules(), &json, &mut it).expect("it loads"));
            });
        }
        "digest" => {
            let g = fixtures::late();
            let mut d = Digester::new();
            run(n * 10, || {
                black_box(d.digest(g.rules(), g.state()).expect("finite"));
            });
        }
        "vis" => {
            // A unit of sight 2 stepping back and forth, the line-of-sight cache forgotten before
            // each step, as `vis_step/sight2_fresh` times it.
            let mut g = fixtures::late();
            let found = g.state().units().iter().find_map(|u| {
                if sight_of(&g, u.id()) != Some(Sight::Walk(2)) || g.is_barbarian(u.owner()) {
                    return None;
                }
                let to = g.grid().neighbors(u.tile()).find(|&n| {
                    g.is_land(n) && g.units_at(n).next().is_none() && g.city_at(n).is_none()
                })?;
                Some((u.id(), u.tile(), to))
            });
            let (u, a, b) = found.expect("a walker");
            let mut there = false;
            run(n * 100, || {
                there = !there;
                g.derived().vis().forget_line_of_sight();
                black_box(g.step_unit_for_test(u, if there { b } else { a }).expect("a step"));
            });
        }
        other => panic!("no part {other}"),
    }
    println!("{part} x{n}: done");
}
