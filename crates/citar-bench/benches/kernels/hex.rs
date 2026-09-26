//! Hexes (package 1a-02, the first kernel of DESIGN.md 9.7): distances, neighbours and the tiles
//! within a radius on a gargantuan grid (160 by 100, wrapping east to west), which every search,
//! sight and yield reads. DESIGN.md 10 gives them no budget: report-only, and one of the kernels
//! the instruction-count gate of `iai` holds.

use std::hint::black_box;

use citar_bench::{Suite, median};
use citar_engine::base::hex::HexGrid;
use citar_engine::base::ids::TileIdx;
use criterion::Criterion;

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let grid = HexGrid::new(160, 100, true, false).expect("a grid");
    let tiles: Vec<TileIdx> = grid.tiles().step_by(97).collect();
    let n = tiles.len();
    let mut k = 0usize;
    let mut distance = || {
        k = (k + 1) % n;
        black_box(grid.distance(tiles[k], tiles[(k * 7 + 3) % n]))
    };
    let mut j = 0usize;
    let mut neighbours = || {
        j = (j + 1) % n;
        black_box(grid.neighbors(tiles[j]).map(|t| t.0).sum::<u32>())
    };
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut within = || {
        i = (i + 1) % n;
        grid.within_into(tiles[i], 3, &mut out);
        black_box(out.len())
    };
    c.bench_function("hex/distance", |b| b.iter(&mut distance));
    c.bench_function("hex/neighbours", |b| b.iter(&mut neighbours));
    c.bench_function("hex/within_3", |b| b.iter(&mut within));
    s.note(
        "hex/distance",
        median(31, 10_000, || {
            distance();
        }),
    );
    s.note(
        "hex/neighbours",
        median(31, 10_000, || {
            neighbours();
        }),
    );
    s.note(
        "hex/within_3",
        median(31, 10_000, || {
            within();
        }),
    );
}
