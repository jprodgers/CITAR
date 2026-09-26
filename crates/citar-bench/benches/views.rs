//! The client view (package 1d-02, gate 4).
//!
//! - `view/player`: `Game::view_json` for a player, the bytes a host sends a browser, on a small
//!   map late in the game: `small-continents-normal-s1025/t280`, the latest committed fixture
//!   (the corpus has no small map at turn 300). Budget (DESIGN.md 10) 1.5 ms.
//! - `view/spectator`: the same for a spectator (`None`), everything whole, on a large map at
//!   turn 280 (`large-pangaea-normal-s1016/t280` of the local corpus, when
//!   `CITAR_REFCHECK_CORPUS` names it; skipped otherwise). Budget 20 ms (the plan's).
//! - `view/spectator_gargantuan`: a spectator's view of the synthetic gargantuan state (24
//!   majors, 32 city-states, 400 cities, 2,500 units on 160 by 100 tiles), report-only against
//!   the 12 ms target.
//!
//! Every view is taken on a game whose memos are warm, as a host's is between two calls. The
//! state is loaded through the Python converter. After Criterion, the run takes the median of its
//! own timings and fails above three times a budget (report-only until 1e-03).
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench views
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::Chronicle;
use citar_testkit::fixtures::{self, Fixture};
use citar_testkit::states::{self, Shape};
use criterion::Criterion;

const PLAYER: Duration = Duration::from_micros(1500);
const SPECTATOR: Duration = Duration::from_millis(20);
const GARGANTUAN: Duration = Duration::from_millis(12);

fn load(f: &Fixture) -> Game {
    let bytes = fixtures::read_state(f).expect("a state");
    Game::from_python(Ruleset::shared(), &bytes).expect("it loads").0
}

/// The small map at turn 280, from the committed fixtures.
fn small() -> (Game, String) {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == "small-continents-normal-s1025" && f.turn == 280)
        .expect("the late fixture");
    (load(&f), f.name)
}

/// The large map at turn 280 from the corpus, if there is one.
fn large() -> Option<(Game, String)> {
    let f = fixtures::corpus()
        .expect("the corpus folder")?
        .into_iter()
        .find(|f| f.case == "large-pangaea-normal-s1016" && f.turn == 280)?;
    Some((load(&f), f.name))
}

/// The synthetic gargantuan state as a game.
fn gargantuan() -> Game {
    let r = Ruleset::shared();
    let st = states::build(r, 2026, &Shape::GARGANTUAN);
    Game::from_state(r, st, Chronicle::new()).expect("a sound state")
}

/// The median of `n` timings of `f`.
fn median(n: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times[n / 2]
}

/// Reports a median against its budget, and fails above three times it unless `report_only`.
fn gate(name: &str, took: Duration, budget: Duration, report_only: bool) -> bool {
    println!("{name} median: {took:?} (budget {budget:?}, hard limit {:?})", budget * 3);
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget");
    }
    report_only || took <= budget * 3
}

fn main() {
    let (small, small_name) = small();
    let pid = small.majors(true).map(|p| p.id()).next().unwrap_or(PlayerId(0));
    let player = || black_box(small.view_json(Some(pid), 150));
    let _warm = player();
    let player_bytes = player().len();
    println!("{small_name}: the view of {pid:?} is {player_bytes} bytes");
    let large = large();
    if let Some((g, name)) = &large {
        let _warm = g.view_json(None, 150);
        println!("{name}: a spectator's view is {} bytes", g.view_json(None, 150).len());
    } else {
        println!("no corpus (CITAR_REFCHECK_CORPUS): view/spectator on a large map is skipped");
    }
    let huge = gargantuan();
    let _warm = huge.view_json(None, 150);
    println!("gargantuan: a spectator's view is {} bytes", huge.view_json(None, 150).len());

    let mut cr = Criterion::default().configure_from_args();
    cr.bench_function("view/player", |b| b.iter(player));
    let mut grp = cr.benchmark_group("view");
    grp.sample_size(10);
    if let Some((g, _)) = &large {
        grp.bench_function("spectator", |b| b.iter(|| black_box(g.view_json(None, 150))));
    }
    grp.bench_function("spectator_gargantuan", |b| {
        b.iter(|| black_box(huge.view_json(None, 150)));
    });
    grp.finish();
    cr.final_summary();

    let mut ok = gate("view/player", median(31, || drop(player())), PLAYER, false);
    if let Some((g, _)) = &large {
        let took = median(11, || drop(black_box(g.view_json(None, 150))));
        ok &= gate("view/spectator", took, SPECTATOR, false);
    }
    let took = median(7, || drop(black_box(huge.view_json(None, 150))));
    gate("view/spectator_gargantuan", took, GARGANTUAN, true);
    assert!(ok, "a view is over three times its budget");
}
