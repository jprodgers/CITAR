//! Tile yields, city stats, citizens, connectivity and settle (package 1b-06, gate 7).
//!
//! The state is the latest committed fixture (`small-continents-normal-s1025/t280`), loaded
//! through the Python converter. Budgets (DESIGN.md 10, report-only until 1e-03):
//! - a tile's yield: a hit at a stable revision at or under 5 ns, the first read after one
//!   unrelated change at or under 50 ns, a recompute at or under 1 µs. The first reads are those
//!   of every tile a civilization owns, each once after a write no cache reads
//!   (`Game::unrelated_change_for_bench`): the first of them validates what they share (the
//!   cities' tile modifiers, the civilization's indexes and supply) for the rest, and that one
//!   read alone is printed besides;
//! - a city's stats recomputed, its tiles' yields cached, at or under 10 µs;
//! - the citizens of a city of 20 assigned at or under 10 µs;
//! - a civilization's connectivity on a small map at or under 30 µs;
//! - a settle with nothing pending at or under 0.5 µs.
//!
//! After Criterion, the run takes the median of its own timings and fails above three times a
//! budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench stats
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{CityId, PlayerId, TileIdx};
use citar_engine::game::cities::stats::{self as cstats, Work};
use citar_engine::game::cities::{citizens, connections};
use citar_engine::game::{Game, query, tiles};
use citar_engine::rules::Ruleset;
use citar_engine::unique::CondDeps;
use citar_testkit::fixtures;
use criterion::Criterion;
use serde_json::json;

const HIT: Duration = Duration::from_nanos(5);
const FIRST_READ: Duration = Duration::from_nanos(50);
const RECOMPUTE: Duration = Duration::from_micros(1);
const CITY_STATS: Duration = Duration::from_micros(10);
const ASSIGN_POP20: Duration = Duration::from_micros(10);
const CONNECTIVITY: Duration = Duration::from_micros(30);
const SETTLE: Duration = Duration::from_nanos(500);

/// The late fixture's game.
fn late() -> Game {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == "small-continents-normal-s1025" && f.turn == 280)
        .expect("the late fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    Game::from_python(Ruleset::shared(), &bytes).expect("it loads").0
}

/// What the benchmarks read: the largest city, one of the tiles it works, its owner and the
/// tiles its owner's cities own, and the civilization with the most cities.
struct Subject {
    city: CityId,
    tile: TileIdx,
    owner: PlayerId,
    owned: Vec<(TileIdx, CityId)>,
    connected: PlayerId,
}

fn subject(g: &Game) -> Subject {
    let city = g
        .state()
        .cities()
        .iter()
        .filter(|c| !c.worked.is_empty())
        .max_by_key(|c| (c.pop, std::cmp::Reverse(c.id())))
        .expect("a city that works a tile");
    let connected = g
        .majors(true)
        .map(|p| p.id())
        .max_by_key(|&p| (g.state().cities().of(p).len(), std::cmp::Reverse(p)))
        .expect("a living major");
    let owner = city.owner();
    let owned = g
        .state()
        .tiles()
        .iter()
        .filter(|(_, t)| t.owner() == Some(owner))
        .filter_map(|(i, t)| Some((i, t.city()?)))
        .collect();
    Subject { city: city.id(), tile: city.worked[0], owner, owned, connected }
}

fn read(g: &Game, s: &Subject) -> f64 {
    query::tile_yield(g, s.tile, Some(s.owner), Some(s.city))[citar_engine::base::stats::Stat::Food]
}

/// City stats from the city's parts, its tiles' yields cached.
fn city_stats(g: &Game, c: CityId) -> f64 {
    let city = g.city(c).expect("the city");
    let work = Work::of(city);
    let parts = cstats::city_parts(g, c, &work);
    let s = cstats::city_stats_from(g, c, &parts, &work, cstats::current_construction(city), None);
    s.total[citar_engine::base::stats::Stat::Production]
}

/// The median of `n` timings of `f`, each of `batch` calls.
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

/// The first read of every tile the subject's owner owns, each once.
fn read_owned(g: &Game, s: &Subject) -> f64 {
    let food = citar_engine::base::stats::Stat::Food;
    s.owned.iter().map(|&(t, c)| query::tile_yield(g, t, Some(s.owner), Some(c))[food]).sum()
}

fn check(name: &str, took: Duration, budget: Duration) {
    println!("{name} median: {took:?} (budget {budget:?}, hard limit {:?})", budget * 3);
    assert!(took <= budget * 3, "{name} took {took:?}, over three times {budget:?}");
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget (report-only until 1e-03)");
    }
}

fn main() {
    let mut g = late();
    let s = subject(&g);
    // Every memo warm.
    black_box(read(&g, &s));
    black_box(query::city_stats(&g, s.city));
    // The unrelated changes touch another civilization.
    let other = g.majors(true).map(|p| p.id()).find(|&p| p != s.owner).expect("another major");
    black_box(read_owned(&g, &s));
    let mods = tiles::city_mods(&g, s.city);
    let recompute = |g: &Game| {
        let mut deps = CondDeps::empty();
        tiles::compute_tile_yield(g, s.tile, Some(s.owner), Some(s.city), Some(&mods), &mut deps)
    };
    // A city of 20.
    let mut big = g.clone();
    big.apply_ops(&json!([{"op": "set_city", "city": s.city.get(), "pop": 20}]))
        .expect("a city of 20");
    let city = g.city(s.city).expect("the city");
    println!(
        "city {} of player {}: pop {}, {} tiles worked, {} tiles owned; player {} has {} cities",
        s.city.get(),
        s.owner.0,
        city.pop,
        city.worked.len(),
        s.owned.len(),
        s.connected.0,
        g.state().cities().of(s.connected).len()
    );

    let mut c = Criterion::default().configure_from_args();
    c.bench_function("tile_yield/hit", |b| b.iter(|| read(black_box(&g), &s)));
    c.bench_function("tile_yield/first_reads_of_owned_after_change", |b| {
        b.iter(|| {
            g.unrelated_change_for_bench(other);
            read_owned(black_box(&g), &s)
        });
    });
    c.bench_function("tile_yield/recompute", |b| b.iter(|| recompute(black_box(&g))));
    c.bench_function("city_stats/recompute", |b| b.iter(|| city_stats(black_box(&g), s.city)));
    c.bench_function("citizens/assign_pop20", |b| {
        b.iter(|| citizens::assign(black_box(&big), s.city, false));
    });
    c.bench_function("connectivity/small", |b| {
        b.iter(|| connections::connected_cities(black_box(&g), s.connected));
    });
    c.bench_function("settle/nothing_pending", |b| b.iter(|| g.settle_for_bench()));
    c.final_summary();

    check(
        "tile_yield/hit",
        median(31, 10_000, || {
            black_box(read(black_box(&g), &s));
        }),
        HIT,
    );
    let first = median(31, 100, || {
        g.unrelated_change_for_bench(other);
        black_box(read(black_box(&g), &s));
    });
    println!("tile_yield/first_read_after_change, the one that validates for the rest: {first:?}");
    let owned = u32::try_from(s.owned.len()).unwrap_or(1).max(1);
    let reads = median(31, 20, || {
        g.unrelated_change_for_bench(other);
        black_box(read_owned(black_box(&g), &s));
    });
    check("tile_yield/first_read_after_change (per tile owned)", reads / owned, FIRST_READ);
    check(
        "tile_yield/recompute",
        median(31, 1_000, || {
            black_box(recompute(black_box(&g)));
        }),
        RECOMPUTE,
    );
    check(
        "city_stats/recompute",
        median(31, 100, || {
            black_box(city_stats(black_box(&g), s.city));
        }),
        CITY_STATS,
    );
    check(
        "citizens/assign_pop20",
        median(31, 100, || {
            black_box(citizens::assign(black_box(&big), s.city, false));
        }),
        ASSIGN_POP20,
    );
    check(
        "connectivity/small",
        median(31, 100, || {
            black_box(connections::connected_cities(black_box(&g), s.connected));
        }),
        CONNECTIVITY,
    );
    check("settle/nothing_pending", median(31, 10_000, || g.settle_for_bench()), SETTLE);
}
