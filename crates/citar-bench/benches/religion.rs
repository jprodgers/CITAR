//! Religious pressure (package 1b-08, gate 5).
//!
//! The state is a gargantuan pangaea of twelve civilizations with a city on every site that
//! allows one, six of them with a religion founded in their capital, played for twenty rounds of
//! pressure so that the religions have spread. Budget (DESIGN.md 10, report-only until 1e-03):
//! - one round of pressure, every city's religious turn in id order (`religion::city_end_turn`:
//!   the pressure of its surroundings added and a new majority taken), at or under 1 ms.
//!
//! Report-only besides: every city's surroundings asked at a stable revision
//! (`pressures_from_surroundings`), which validates each spread and reads the grid.
//!
//! After Criterion, the run takes the median of its own timings and fails above three times the
//! budget.
//!
//! ```text
//! cargo bench -p citar-bench --bench religion
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::api::testops;
use citar_engine::base::ids::{CityId, PlayerId, TileIdx};
use citar_engine::game::Game;
use citar_engine::game::cities::founding;
use citar_engine::game::derive::religion::surroundings_for_bench;
use citar_engine::game::religion::{self, found};
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::{BeliefKind, BeliefType};
use citar_testkit::script::new_game;
use criterion::Criterion;
use serde_json::json;

const ROUND: Duration = Duration::from_millis(1);
const MAJORS: u8 = 12;
const RELIGIONS: u8 = 6;

/// The gargantuan game: its cities founded, its religions founded and spread.
fn gargantuan() -> (Game, Vec<CityId>) {
    let r = Ruleset::shared();
    let seats: Vec<_> = (0..MAJORS).map(|_| json!({"nation": "BenchmarkCiv"})).collect();
    let cfg = json!({
        "seed": 7,
        "map_size": "gargantuan",
        "map_type": "pangaea",
        "players": seats,
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
    });
    let mut g = new_game(r, cfg.as_object().expect("an object")).expect("a gargantuan game");
    let tiles: Vec<TileIdx> = g.grid().tiles().collect();
    let mut n = 0u8;
    for t in tiles.into_iter().step_by(3) {
        let p = PlayerId(n % MAJORS);
        if founding::found_check(&g, p, t).is_none()
            && founding::found_city(&mut g, p, t, None).is_ok()
        {
            n = n.wrapping_add(1);
        }
    }
    g.apply_ops(&json!([{"op": "set_player", "player": "all", "faith": 10_000}])).expect("faith");
    for p in 0..RELIGIONS {
        let p = PlayerId(p);
        let pantheon = religion::beliefs_available(&g, BeliefKind::Type(BeliefType::Pantheon))[0];
        let name = g.rules().beliefs()[pantheon].name.to_string();
        let (b, pay) = found::plan_pantheon(&g, p, &name).expect("a pantheon");
        found::apply_pantheon(&mut g, p, b, pay);
        let cap = g.player(p).and_then(|x| x.capital).expect("a capital");
        let at = g.city(cap).expect("the capital").tile();
        let pick = |g: &Game, t| {
            let b = religion::beliefs_available(g, BeliefKind::Type(t))[0];
            g.rules().beliefs()[b].name.to_string()
        };
        let beliefs = [pick(&g, BeliefType::Founder), pick(&g, BeliefType::Follower)];
        let plan = found::plan_religion(&g, p, at, &format!("Faith {}", p.0), &beliefs, None)
            .expect("a religion");
        found::apply_religion(&mut g, p, &plan, |_| {});
    }
    let cities: Vec<CityId> = g.state().cities().iter().map(|c| c.id()).collect();
    for _ in 0..20 {
        round(&mut g, &cities);
    }
    testops::apply(&mut g, &json!([])).expect("settled");
    (g, cities)
}

/// One round of pressure: every city's religious turn, in id order.
fn round(g: &mut Game, cities: &[CityId]) {
    for &c in cities {
        religion::city_end_turn(g, c);
    }
}

/// The median of `n` rounds, each on a fresh copy of the game (the copy is not timed).
fn median_round(g: &Game, cities: &[CityId], n: usize) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let mut copy = g.clone();
            let t = Instant::now();
            round(&mut copy, cities);
            let took = t.elapsed();
            black_box(copy);
            took
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn check(name: &str, took: Duration, budget: Duration) {
    println!("{name} median: {took:?} (budget {budget:?}, hard limit {:?})", budget * 3);
    assert!(took <= budget * 3, "{name} took {took:?}, over three times {budget:?}");
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget (report-only until 1e-03)");
    }
}

fn main() {
    let (g, cities) = gargantuan();
    let following = cities
        .iter()
        .filter(|&&c| religion::majority_religion(&g, c).is_some_and(|r| religion::is_major(&g, r)))
        .count();
    println!(
        "{}x{} map: {} cities, {} following a religion",
        g.grid().width(),
        g.grid().height(),
        cities.len(),
        following
    );

    let mut cr = Criterion::default().configure_from_args();
    cr.bench_function("religion/round", |b| {
        b.iter_batched(
            || g.clone(),
            |mut copy| {
                round(&mut copy, &cities);
                copy
            },
            criterion::BatchSize::LargeInput,
        );
    });
    cr.bench_function("religion/surroundings", |b| {
        b.iter(|| surroundings_for_bench(black_box(&g)));
    });
    cr.final_summary();

    check("religion/round", median_round(&g, &cities, 31), ROUND);
    let t = Instant::now();
    for _ in 0..31 {
        black_box(surroundings_for_bench(black_box(&g)));
    }
    println!("religion/surroundings: {:?} a round (report-only)", t.elapsed() / 31);
}
