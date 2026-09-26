//! The unique indexes of a civilization (package 1b-05): a lookup in a memo that is verified, and
//! a rebuild from the civilization's sources.
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter; the civilization is the one whose index has the most entries.
//! Budgets (DESIGN.md 10): a lookup at or under 20 ns, a rebuild at or under 10 µs. Report-only:
//! gathering the sources a rebuild reads.

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::game::derive::civ::sources;
use citar_engine::unique::{CivIndex, EvalWorld, IndexLayer, UniqueType};
use criterion::Criterion;

/// One lookup: the index lent by its verified memo, and one type's run of it.
fn lookup(g: &Game, p: PlayerId) -> usize {
    let v = g.view();
    v.civ_index(p, IndexLayer::Full).get(UniqueType::StatPercentBonus).len()
}

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let g = fixtures::late();
    let p = g
        .majors(true)
        .map(|x| x.id())
        .max_by_key(|&p| (g.view().civ_index(p, IndexLayer::Full).len(), std::cmp::Reverse(p)))
        .expect("a living major");
    let r = g.rules();
    let src = sources(&g, p);
    println!(
        "player {}: {} entries without resources, {} with",
        p.0,
        CivIndex::build(r, &src).len(),
        g.view().civ_index(p, IndexLayer::Full).len()
    );
    c.bench_function("civ_index/lookup", |b| b.iter(|| lookup(black_box(&g), black_box(p))));
    c.bench_function("civ_index/rebuild", |b| {
        b.iter(|| CivIndex::build(black_box(r), black_box(&src)));
    });
    c.bench_function("civ_index/sources", |b| b.iter(|| sources(black_box(&g), black_box(p))));
    s.put(
        "civ_index/lookup",
        median(31, 10_000, || {
            black_box(lookup(black_box(&g), black_box(p)));
        }),
    );
    s.put(
        "civ_index/rebuild",
        median(31, 100, || {
            black_box(CivIndex::build(black_box(r), black_box(&src)));
        }),
    );
    s.note(
        "civ_index/sources",
        median(31, 100, || {
            black_box(sources(black_box(&g), black_box(p)));
        }),
    );
}
