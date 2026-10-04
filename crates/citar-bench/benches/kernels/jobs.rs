//! Worker job maps (package 1c-04): `jobmap_small`, a civilization's whole `JobMap` for the
//! Worker's builder class built cold on a small map.
//!
//! The states are the committed small maps, loaded through the Python converter: a mid-game one
//! (`small-pangaea-raging/t50`), a scenario (`scenario-small-continents-s3001/t61`) and a late one
//! (`small-continents-normal-s1025/t280`). On each, the major with the most cities has its map
//! rebuilt: what the civilization's rules allow (`CivJobs`), then the best job on every tile of
//! its cities, each improvement weighed as `automation::best_job` weighs it. The tile yields,
//! unique indexes and other memos a job reads are warm, as they are when a civilization's rules
//! change mid-game and its map is built again. Each state is printed; the budget holds the late
//! one, the largest map. Budget 200 µs (DESIGN.md 10).

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::{BaseUnitId, PlayerId};
use citar_engine::game::Game;
use citar_engine::game::derive::jobs;
use citar_engine::rules::defs::BuilderClass;
use criterion::Criterion;

/// The committed small-map fixtures, as (case, turn); the last is the budget's.
const SMALL_FIXTURES: [(&str, u32); 3] = [
    ("small-pangaea-raging", 50),
    ("scenario-small-continents-s3001", 61),
    ("small-continents-normal-s1025", 280),
];

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

pub fn run(s: &mut Suite, cr: &mut Criterion) {
    let games: Vec<(String, Game)> = SMALL_FIXTURES
        .iter()
        .map(|&(c, t)| (format!("{c}/t{t}"), fixtures::committed_game(c, t)))
        .collect();
    let last = games.len() - 1;
    for (i, (name, g)) in games.iter().enumerate() {
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
        if i == last {
            s.put("jobmap_small", took);
        } else {
            s.note(&format!("jobmap_small/{name}"), took);
        }
    }
}
