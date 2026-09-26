//! The production advisor (package 1c-07, gate 5).
//!
//! The state is a small map at turn 200 (`small-continents-normal-s1025/t200` of the local
//! corpus, when `CITAR_REFCHECK_CORPUS` names it), else the same game at turn 280, the latest
//! committed fixture; loaded through the Python converter. `advisor/call_per_city` asks the
//! advisor what each city of a living major would build (`advisor::advise_production`, at
//! automatic production's parameters) in turn, the time of one call; `advisor/every_city_kept`
//! asks the same through one `advisor::Advisor` a civilization, as the bot keeps one for a
//! civilization's turn; `advisor/what_if` asks the what-if of each building each of them could
//! build (`cities::what_if::what_if_building`), the time of one. Budget (DESIGN.md 10, report-only): a call at or under 50 µs. After Criterion,
//! the run takes the median of its own timings and warns above it.
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench advisor
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{BuildingId, CityId, PlayerId};
use citar_engine::game::Game;
use citar_engine::game::advisor::{self, AdvisorParams};
use citar_engine::game::cities::construction::buildable_items;
use citar_engine::game::cities::what_if::what_if_building;
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures::{self, Fixture};
use criterion::Criterion;

const CALL: Duration = Duration::from_micros(50);
const CASE: &str = "small-continents-normal-s1025";

/// The small map at turn 200 from the corpus, else at turn 280 from the committed fixtures.
fn fixture() -> (Game, String) {
    let pick =
        |all: Vec<Fixture>, turn: u32| all.into_iter().find(|f| f.case == CASE && f.turn == turn);
    let f = fixtures::corpus()
        .expect("the corpus folder")
        .and_then(|all| pick(all, 200))
        .or_else(|| pick(fixtures::committed().expect("the committed fixtures"), 280))
        .expect("the fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    (g, f.name)
}

/// Every city of a living major, with its owner.
fn cities(g: &Game) -> Vec<(PlayerId, CityId)> {
    g.majors(true).flat_map(|p| g.player_cities(p.id()).map(move |c| (p.id(), c.id()))).collect()
}

/// The median of `n` timings of `f`, each of `batch` calls of it, divided by `per` (the calls one
/// `f` makes).
fn median(n: usize, batch: u32, per: u32, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..batch {
                f();
            }
            t.elapsed() / batch / per.max(1)
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn main() {
    let (g, name) = fixture();
    let pp = AdvisorParams::auto_production();
    let all = cities(&g);
    let asks: Vec<(CityId, BuildingId)> = all
        .iter()
        .flat_map(|&(_, c)| {
            let items = buildable_items(&g, c);
            items
                .buildings
                .iter()
                .chain(items.wonders.iter())
                .map(move |b| (c, b))
                .collect::<Vec<_>>()
        })
        .collect();
    println!(
        "{name}: {} cities of living majors, {} buildings they could build",
        all.len(),
        asks.len()
    );
    let calls = u32::try_from(all.len()).unwrap_or(u32::MAX);
    let whatifs = u32::try_from(asks.len()).unwrap_or(u32::MAX);
    let every_city = || {
        for &(p, c) in &all {
            black_box(advisor::advise_production(black_box(&g), p, c, &pp));
        }
    };
    let every_city_kept = || {
        let mut i = 0;
        while i < all.len() {
            let p = all[i].0;
            let adv = advisor::Advisor::new(black_box(&g), p, &pp);
            while i < all.len() && all[i].0 == p {
                black_box(adv.advise(black_box(&g), all[i].1));
                i += 1;
            }
        }
    };
    let every_building = || {
        for &(c, b) in &asks {
            black_box(what_if_building(black_box(&g), c, b));
        }
    };

    let mut cr = Criterion::default().configure_from_args();
    cr.bench_function("advisor/every_city", |b| b.iter(every_city));
    cr.bench_function("advisor/every_city_kept", |b| b.iter(every_city_kept));
    cr.bench_function("advisor/every_what_if", |b| b.iter(every_building));
    cr.final_summary();

    let call = median(11, 3, calls, every_city);
    println!("advisor/call_per_city median: {call:?} (budget {CALL:?}, report-only)");
    if call > CALL {
        println!("warning: advisor/call_per_city is over its {CALL:?} budget (report-only)");
    }
    let kept = median(11, 3, calls, every_city_kept);
    println!(
        "advisor/call_per_city with one Advisor a civilization median: {kept:?} (report-only)"
    );
    let what_if = median(11, 3, whatifs, every_building);
    println!("advisor/what_if median: {what_if:?} (report-only)");
}
