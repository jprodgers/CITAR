//! What a city can build (package 1b-07).
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter; the city is its largest. Budgets (DESIGN.md 10):
//! - the `Buildable` memo read at a stable revision (`buildable_items`, which copies the lists and
//!   checks the hangar for aircraft) at or under 50 ns;
//! - the lists recomputed, every unit, building and wonder of the ruleset checked
//!   (`compute_buildable`), at or under 20 µs;
//! - the lists of the other cities of the civilization with the most read after one of its
//!   cities changed (a heal, a growth, a queue edit: its `core`), which validate and recompute
//!   nothing, at or under 10 µs for the 12 others of the late fixture's largest civilization of
//!   13 (256 µs when every list read its siblings' `core`).
//!
//! Report-only: the first read after a change to another civilization that the memo does not
//! read, which validates the memo's inputs.

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::CityId;
use citar_engine::game::Game;
use citar_engine::game::cities::construction::{
    Buildable, buildable_items, compute_buildable_for_bench as compute_buildable,
};
use criterion::Criterion;

/// The largest city.
fn subject(g: &Game) -> CityId {
    g.state()
        .cities()
        .iter()
        .max_by_key(|c| (c.pop, std::cmp::Reverse(c.id())))
        .map(citar_engine::state::cities::City::id)
        .expect("a city")
}

/// One city of the civilization with the most cities, its last, and the others.
fn largest_civ(g: &Game) -> (CityId, Vec<CityId>) {
    let owner = g
        .majors(true)
        .map(|p| p.id())
        .max_by_key(|&p| (g.player_cities(p).count(), std::cmp::Reverse(p)))
        .expect("a major");
    let mut cities: Vec<CityId> = g.player_cities(owner).map(|c| c.id()).collect();
    let changed = cities.pop().expect("a city");
    (changed, cities)
}

/// How many items a list holds, so that the work is not optimised away.
fn size(b: &Buildable) -> usize {
    b.units.len() + b.buildings.len() + b.wonders.len()
}

pub fn run(s: &mut Suite, cr: &mut Criterion) {
    let mut g = fixtures::late();
    let c = subject(&g);
    let owner = g.city(c).expect("the city").owner();
    let other = g.majors(true).map(|p| p.id()).find(|&p| p != owner).expect("another major");
    let items = buildable_items(&g, c);
    println!(
        "city {} of player {}: pop {}; {} units, {} buildings, {} wonders buildable",
        c.get(),
        owner.0,
        g.city(c).expect("the city").pop,
        items.units.len(),
        items.buildings.len(),
        items.wonders.len()
    );
    cr.bench_function("buildable/hit", |b| b.iter(|| size(&buildable_items(black_box(&g), c))));
    cr.bench_function("buildable/recompute", |b| {
        b.iter(|| size(&compute_buildable(black_box(&g), c)));
    });
    cr.bench_function("buildable/first_read_after_unrelated_change", |b| {
        b.iter(|| {
            g.unrelated_change_for_bench(other);
            size(&buildable_items(black_box(&g), c))
        });
    });
    let (changed, siblings) = largest_civ(&g);
    println!(
        "the largest civilization: city {} changes, {} others are read",
        changed.get(),
        siblings.len()
    );
    let read_all = |g: &mut Game| {
        g.city_change_for_bench(changed);
        siblings.iter().map(|&x| size(&buildable_items(black_box(g), x))).sum::<usize>()
    };
    cr.bench_function("buildable/other_lists_after_a_sibling_changed", |b| {
        b.iter(|| read_all(&mut g));
    });
    s.put(
        "buildable/hit",
        median(31, 10_000, || {
            black_box(size(&buildable_items(black_box(&g), c)));
        }),
    );
    s.put(
        "buildable/recompute",
        median(31, 100, || {
            black_box(size(&compute_buildable(black_box(&g), c)));
        }),
    );
    let first = median(31, 100, || {
        g.unrelated_change_for_bench(other);
        black_box(size(&buildable_items(black_box(&g), c)));
    });
    s.note("buildable/first_read_after_unrelated_change", first);
    s.put(
        "buildable/other_lists_after_a_sibling_changed",
        median(31, 100, || {
            black_box(read_all(&mut g));
        }),
    );
}
