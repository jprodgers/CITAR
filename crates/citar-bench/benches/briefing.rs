//! The briefing (package 1d-03, gate 4).
//!
//! - `briefing/small`: `Game::briefing` for a civilization of a small map late in the game:
//!   `small-continents-normal-s1025/t280`, the latest committed fixture (the corpus has no small
//!   map at turn 300). Budget (DESIGN.md 10) 1 ms, report-only.
//! - `briefing/turn_progress`: the turn's progress on the same state, which a host asks for after
//!   every batch of actions; report-only against the same budget.
//! - `briefing/large`: the briefing on a large map at turn 280
//!   (`large-pangaea-normal-s1016/t280` of the local corpus, when `CITAR_REFCHECK_CORPUS` names
//!   it; skipped otherwise), report-only.
//!
//! Every briefing is read from a game whose memos are warm, as a host's is between two calls.
//! After Criterion, the run prints the median of its own timings against the budget; nothing
//! fails it (report-only until package 1e-03).
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<absolute path of refcheck/corpus> cargo bench -p citar-bench --bench briefing
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures::{self, Fixture};
use criterion::Criterion;

const BRIEFING: Duration = Duration::from_millis(1);

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

/// Reports a median against its budget.
fn report(name: &str, took: Duration, budget: Duration) {
    println!("{name} median: {took:?} (budget {budget:?})");
    if took > budget {
        println!("warning: {name} is over its {budget:?} budget");
    }
}

/// The first living major civilization of a game.
fn first_major(g: &Game) -> PlayerId {
    g.majors(true).map(|p| p.id()).next().unwrap_or(PlayerId(0))
}

fn main() {
    let (small, small_name) = small();
    let pid = first_major(&small);
    let text = small.briefing(pid);
    println!(
        "{small_name}: the briefing of {pid:?} is {} lines, {} bytes",
        text.lines().count(),
        text.len()
    );
    let large = large();
    let large_pid = large.as_ref().map(|(g, _)| first_major(g));
    if let (Some((g, name)), Some(p)) = (&large, large_pid) {
        println!("{name}: the briefing of {p:?} is {} bytes", g.briefing(p).len());
    } else {
        println!("no corpus (CITAR_REFCHECK_CORPUS): briefing/large is skipped");
    }

    let mut cr = Criterion::default().configure_from_args();
    cr.bench_function("briefing/small", |b| b.iter(|| black_box(small.briefing(pid))));
    cr.bench_function("briefing/turn_progress", |b| {
        b.iter(|| black_box(small.turn_progress(pid)));
    });
    if let (Some((g, _)), Some(p)) = (&large, large_pid) {
        let mut grp = cr.benchmark_group("briefing");
        grp.sample_size(20);
        grp.bench_function("large", |b| b.iter(|| black_box(g.briefing(p))));
        grp.finish();
    }
    cr.final_summary();

    report("briefing/small", median(31, || drop(black_box(small.briefing(pid)))), BRIEFING);
    report(
        "briefing/turn_progress",
        median(31, || drop(black_box(small.turn_progress(pid)))),
        BRIEFING,
    );
    if let (Some((g, _)), Some(p)) = (&large, large_pid) {
        report("briefing/large", median(11, || drop(black_box(g.briefing(p)))), BRIEFING);
    }
}
