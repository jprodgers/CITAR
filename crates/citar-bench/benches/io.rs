//! The I/O benchmarks (DESIGN.md 9.7, 10; packages 1a-03, 1a-09, 1e-03): the ruleset, the
//! digest, the snapshot a host takes under its lock, the save written off it, and the load.
//!
//! - `ruleset/load`: the embedded ruleset compiled from its 24 files (`Ruleset::load`, what
//!   `Ruleset::shared` does once a process). Budget 20 ms.
//! - `snapshot/gargantuan`: `Game::snapshot`, the deep copy of the state a host takes under its
//!   lock, of the synthetic gargantuan state (24 majors and 32 city-states on 160 by 100 tiles,
//!   400 cities, 2,500 units, most of the map explored and remembered, about 4 MB of canonical
//!   bytes). Budget 10 ms (the plan's floor is 100 ms, gate 3 of package 1e-03).
//! - `to_json/small_t280` and `to_json/gargantuan`: the save written from a snapshot, off the
//!   lock. Budgets 20 ms and 100 ms.
//! - `load/small_t280`: `Game::load` of the late fixture's save: the state read and validated,
//!   the caches built and every civilization's sight rebuilt. Budget 30 ms. The history is not
//!   in a save (DESIGN.md 4.11): `load/small_t280_history` (report-only) loads the same with the
//!   journal chunk that rebuilds its 280 turns of events, stats rows and frames.
//! - `digest/gargantuan` and `digest/small_t280`: the canonical digest a round's end takes, with a
//!   `Digester` reused as a host reuses one. Budgets 5 ms and 0.2 ms.
//!
//! Report-only besides: `load/gargantuan_state`, the gargantuan save read and validated (no
//! game).
//!
//! ```text
//! cargo bench -p citar-bench --bench io
//! ```

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::game::Game;
use citar_engine::rules::{Ruleset, embedded};
use citar_engine::save::{self, Digester, canon};

fn main() {
    let mut s = Suite::start("io");
    let mut c = s.criterion();
    let r = Ruleset::shared();

    if s.wants("ruleset") {
        c.bench_function("ruleset/load", |b| {
            b.iter(|| Ruleset::load(black_box(&embedded())).expect("the ruleset"));
        });
        s.put(
            "ruleset/load",
            median(11, 3, || {
                black_box(Ruleset::load(black_box(&embedded())).expect("the ruleset"));
            }),
        );
    }

    let wants_big = ["snapshot", "to_json", "load", "digest"].iter().any(|w| s.wants(w));
    if wants_big {
        let st = fixtures::gargantuan_state();
        let bytes = canon::state_bytes(&st).expect("finite").len();
        println!("gargantuan state: {:.1} MB canonical", bytes as f64 / 1e6);
        let huge = fixtures::gargantuan_game();
        let mut late = fixtures::late();
        let chunk = late.take_journal_chunk().expect("a journal").map(|c| c.json);
        let late_json = late.snapshot().to_json().expect("saves");
        let huge_json = save::to_json(r, &st).expect("saves");
        println!(
            "saves: the late fixture {:.2} MB, gargantuan {:.1} MB",
            late_json.len() as f64 / 1e6,
            huge_json.len() as f64 / 1e6
        );
        println!(
            "the late fixture's journal chunk: {:.2} MB",
            chunk.as_ref().map_or(0, Vec::len) as f64 / 1e6
        );
        let load_late = || {
            let mut none = std::iter::empty();
            Game::load(r, black_box(&late_json), &mut none).expect("it loads").0
        };
        let load_late_history = || {
            let mut it = chunk.iter().map(Vec::as_slice);
            let (g, report) = Game::load(r, black_box(&late_json), &mut it).expect("it loads");
            assert!(!report.chronicle_incomplete, "the whole history");
            g
        };
        let mut d = Digester::new();

        let mut grp = c.benchmark_group("io");
        grp.sample_size(20);
        grp.bench_function("snapshot/gargantuan", |b| b.iter(|| black_box(huge.snapshot())));
        grp.bench_function("to_json/small_t280", |b| {
            b.iter(|| save::to_json(r, black_box(late.state())).expect("saves"));
        });
        grp.bench_function("to_json/gargantuan", |b| {
            b.iter(|| save::to_json(r, black_box(&st)).expect("saves"));
        });
        grp.bench_function("load/small_t280", |b| b.iter(load_late));
        grp.bench_function("load/gargantuan_state", |b| {
            b.iter(|| save::json::read_state(r, black_box(&huge_json)).expect("loads"));
        });
        grp.bench_function("digest/gargantuan", |b| {
            b.iter(|| d.digest(r, black_box(&st)).expect("finite"));
        });
        grp.bench_function("digest/small_t280", |b| {
            b.iter(|| d.digest(r, black_box(late.state())).expect("finite"));
        });
        grp.finish();

        s.put("snapshot/gargantuan", median(31, 1, || drop(black_box(huge.snapshot()))));
        s.put(
            "to_json/small_t280",
            median(11, 1, || drop(save::to_json(r, black_box(late.state())).expect("saves"))),
        );
        s.put(
            "to_json/gargantuan",
            median(7, 1, || drop(save::to_json(r, black_box(&st)).expect("saves"))),
        );
        s.put("load/small_t280", median(11, 1, || drop(load_late())));
        s.note("load/small_t280_history", median(5, 1, || drop(load_late_history())));
        s.note(
            "load/gargantuan_state",
            median(5, 1, || {
                black_box(save::json::read_state(r, black_box(&huge_json)).expect("loads"));
            }),
        );
        s.put(
            "digest/gargantuan",
            median(31, 1, || {
                black_box(d.digest(r, black_box(&st)).expect("finite"));
            }),
        );
        s.put(
            "digest/small_t280",
            median(31, 10, || {
                black_box(d.digest(r, black_box(late.state())).expect("finite"));
            }),
        );
    }
    c.final_summary();
    s.finish();
}
