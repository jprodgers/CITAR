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
//! The store (citar-store, package 2-02, DESIGN.md P2.5), report-only:
//! - `container/write_gargantuan`: the gargantuan state's `.citar` v2 container written from
//!   its JSON (zstd, a synced temporary file, the rename), what the session's writer does off the
//!   lock. Budget 150 ms.
//! - `container/read_header`: its header read alone, what listing saves costs a save. Budget
//!   1 ms.
//! - Noted: `container/read_gargantuan` (read, decompressed and its shape checked),
//!   `journal/append_sync` (one round's chunk appended and synced, as each autosave does) and
//!   `journal/read_330` (330 rounds' chunks read back, as a load does).
//!
//! ```text
//! cargo bench -p citar-bench --bench io
//! ```

use std::hint::black_box;
use std::path::PathBuf;

use citar_bench::{Suite, fixtures, median};
use citar_engine::game::Game;
use citar_engine::rules::{Ruleset, embedded};
use citar_engine::save::journal::{self as chunks, JournalCursor};
use citar_engine::save::{self, Digester, canon};
use citar_engine::state::chronicle::{Chronicle, ChronicleHeads, HostHeads};
use citar_store::journal::{JournalRef, JournalWriter};
use citar_store::{
    BodyParts, Header, SessionRef, container, read_container, read_header, read_upto,
    write_container,
};
use citar_testkit::states;

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
    if s.wants("container") || s.wants("journal") {
        store(&mut s, &mut c, r);
    }
    c.final_summary();
    s.finish();
}

/// The store's cases, in a folder of their own under cargo's temporary folder for benchmarks.
fn store(s: &mut Suite, c: &mut criterion::Criterion, r: &'static Ruleset) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("io-store");
    let _stale = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder");

    let json = save::to_json(r, &fixtures::gargantuan_state()).expect("saves");
    let summary = save::summary(&json).expect("a summary");
    let header = Header {
        format: container::FORMAT.to_owned(),
        version: container::VERSION,
        saved_at: "2026-10-02T12:00:00Z".to_owned(),
        engine_build: citar_bot::build_id(r),
        rules: r.id().to_hex(),
        summary: serde_json::to_value(&summary).expect("JSON"),
        session: SessionRef { id: "bench".into(), name: "Gargantuan".into(), benchmark: true },
        journal: None,
    };
    let body = BodyParts { session: b"{}", metrics: b"{}", state: &json, chain: None };
    let path = dir.join("gargantuan.citar");
    write_container(&path, &header, &body).expect("writes");
    println!(
        "the gargantuan container: {:.1} MB of JSON, {:.2} MB on disk",
        json.len() as f64 / 1e6,
        std::fs::metadata(&path).map_or(0, |m| m.len()) as f64 / 1e6
    );

    // 330 rounds of history, each the chunk the engine takes for a round.
    let (mut chron, mut heads, mut host) =
        (Chronicle::new(), ChronicleHeads::default(), HostHeads::default());
    let mut cursor = JournalCursor::default();
    let rounds: Vec<Vec<u8>> = (0..330u64)
        .map(|i| {
            states::history(r, 7_000 + i, 12, &mut heads, &mut host, &mut chron);
            chunks::take_chunk(r, &chron, &mut cursor, &mut host)
                .expect("encodes")
                .expect("a round's history")
                .json
        })
        .collect();
    let full = dir.join("full.cjnl");
    let (mut w, _) = JournalWriter::open(&full).expect("a journal");
    for (i, chunk) in rounds.iter().enumerate() {
        w.append(i as u32, chunk).expect("appends");
    }
    w.sync().expect("syncs");
    let full_ref: JournalRef = w.reference();
    drop(w);
    // A journal that grows by a round each time an append is timed.
    let (mut growing, _) = JournalWriter::open(&dir.join("growing.cjnl")).expect("a journal");
    let mut next = 0u32;
    let mut append = move || {
        let chunk = &rounds[next as usize % rounds.len()];
        growing.append(next, chunk).expect("appends");
        growing.sync().expect("syncs");
        next += 1;
    };

    let mut grp = c.benchmark_group("io");
    grp.sample_size(10);
    grp.bench_function("container/write_gargantuan", |b| {
        b.iter(|| write_container(&path, &header, &body).expect("writes"));
    });
    grp.bench_function("container/read_header", |b| {
        b.iter(|| read_header(black_box(&path)).expect("reads"));
    });
    grp.bench_function("container/read_gargantuan", |b| {
        b.iter(|| read_container(black_box(&path)).expect("reads"));
    });
    grp.bench_function("journal/append_sync", |b| b.iter(&mut append));
    grp.bench_function("journal/read_330", |b| {
        b.iter(|| read_upto(black_box(&full), &full_ref).expect("reads"));
    });
    grp.finish();

    s.put(
        "container/write_gargantuan",
        median(7, 1, || write_container(&path, &header, &body).expect("writes")),
    );
    s.put(
        "container/read_header",
        median(31, 10, || drop(black_box(read_header(&path).expect("reads")))),
    );
    s.note(
        "container/read_gargantuan",
        median(7, 1, || drop(black_box(read_container(&path).expect("reads")))),
    );
    s.note("journal/append_sync", median(31, 1, &mut append));
    s.note(
        "journal/read_330",
        median(7, 1, || drop(black_box(read_upto(&full, &full_ref).expect("reads")))),
    );
    let _done = std::fs::remove_dir_all(&dir);
}
