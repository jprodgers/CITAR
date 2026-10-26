//! Gate 1 of package 2-02 at full size: the synthetic gargantuan state saved as a `.citar` v2
//! container beside a journal of 330 chunks, read back through citar-store and loaded by the
//! engine, equal to what was saved, history and all (DESIGN.md P2.5). And gate 6 of package 2-11:
//! what a session's save holds its lock for on that state stays far under 100 ms.
//!
//! It lives here because citar-bench is the one crate the crate graph lets reach both the
//! testkit's synthetic states and the store (DESIGN.md P2.2, "As built in 2-00a"); the store's
//! own tests (`crates/citar-store/tests/store.rs`) cover everything else at small sizes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use citar_engine::rules::Ruleset;
use citar_engine::save::journal::{self, JournalCursor};
use citar_engine::save::{self, DigestChain};
use citar_engine::state::State;
use citar_engine::state::chronicle::{Chronicle, ChronicleHeads, HostHeads};
use citar_store::journal::JournalWriter;
use citar_store::{
    BodyParts, ChainRef, Header, SessionRef, container, read_container, read_header, read_upto,
    write_container,
};
use citar_testkit::states::{self, Shape};

#[test]
fn a_gargantuan_rounds_save_snapshot_holds_the_lock_far_under_100_ms() {
    // Package 2-11's gate 6 (DESIGN.md P2.5.3): `Game::save_snapshot` is everything a session's save
    // does under its lock (the binding adds a queue push). The budget, 10 ms, is the io suite's
    // (`cargo xtask perf --suite io`, the bench profile on an idle core); here, in whatever profile
    // and on whatever machine the tests run, the hard line of 100 ms holds, on a round passed on the
    // synthetic state, whose chunk has a keyframe, the largest a round's chunk is.
    let g = citar_bench::fixtures::gargantuan_round_game();
    let seq = g.state().host().0.journal_seq;
    let mut best = Duration::MAX;
    let mut size = 0;
    for _ in 0..5 {
        let mut copy = g.clone();
        let t = Instant::now();
        let (snap, chunk) = copy.save_snapshot().expect("it saves");
        best = best.min(t.elapsed());
        let chunk = chunk.expect("the round's history");
        assert_eq!(
            (chunk.seq, snap.state().host().0.journal_seq),
            (seq, seq + 1),
            "the snapshot counts it"
        );
        size = chunk.json.len();
    }
    println!(
        "save_snapshot of the gargantuan state with a round's chunk ({} KB): {:.2} ms",
        size / 1000,
        best.as_secs_f64() * 1e3
    );
    assert!(size > 100_000, "the round's chunk carries the keyframe: {size} bytes");
    assert!(best < Duration::from_millis(100), "{best:?} under the lock");
}

/// The rounds a whole game journals: one chunk each.
const ROUNDS: u32 = 330;

#[test]
fn the_gargantuan_state_and_330_chunks_round_trip_through_the_store() {
    let r = Ruleset::shared();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("citar-bench-store")
        .join(format!("gargantuan-{}", std::process::id()));
    let _stale = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder");

    // 330 rounds of history, each taken as the engine takes a round's chunk.
    let mut chron = Chronicle::new();
    let mut heads = ChronicleHeads::default();
    let mut host = HostHeads::default();
    let mut cursor = JournalCursor::default();
    let mut chunks = Vec::new();
    for i in 0..ROUNDS {
        states::history(r, 7_000 + u64::from(i), 12, &mut heads, &mut host, &mut chron);
        let c = journal::take_chunk(r, &chron, &mut cursor, &mut host)
            .expect("the chunk encodes")
            .expect("a round's history");
        assert_eq!(c.seq, i);
        chunks.push(c.json);
    }
    let mut parts = states::build(r, 2026, &Shape::GARGANTUAN).into_parts();
    parts.chronicle = heads;
    parts.host.0 = host;
    let st = State::from_parts(parts).expect("the synthetic state fits");
    let state_json = save::to_json(r, &st).expect("the state saves");
    assert!(state_json.len() > 4_000_000, "a gargantuan save: {} bytes", state_json.len());

    // The session's order: the chunks appended and synced, then the container that names them.
    let jpath = dir.join("journal.cjnl");
    let (mut w, rec) = JournalWriter::open(&jpath).expect("a new journal");
    assert_eq!(rec.records, 0);
    for (i, c) in chunks.iter().enumerate() {
        w.append(i as u32, c).expect("appends");
    }
    w.sync().expect("syncs");
    let jref = w.reference();
    drop(w);
    assert_eq!(jref.records, ROUNDS);

    let summary = save::summary(&state_json).expect("the state's summary");
    let mut chain = DigestChain::new(b"gargantuan");
    let digest = save::digest(r, &st).expect("a digest");
    chain.push(330, &digest);
    let chain = ChainRef { head: chain.head().to_hex(), rounds: chain.rounds() };
    let header = Header {
        format: container::FORMAT.to_owned(),
        version: container::VERSION,
        saved_at: "2026-10-02T12:00:00Z".to_owned(),
        engine_build: citar_bot::build_id(r),
        rules: r.id().to_hex(),
        summary: serde_json::to_value(&summary).expect("JSON"),
        session: SessionRef { id: "g-garg".into(), name: "Gargantuan".into(), benchmark: true },
        journal: Some(jref.clone()),
    };
    let session = br#"{"id":"g-garg","seats":[]}"#;
    let metrics = br#"{"turns":{}}"#;
    let cpath = dir.join("turn330.citar");
    let body = BodyParts { session, metrics, state: &state_json, chain: Some(&chain) };
    write_container(&cpath, &header, &body).expect("the container writes");
    let on_disk = std::fs::metadata(&cpath).map(|m| m.len()).unwrap_or(0);
    let chunk_bytes: usize = chunks.iter().map(Vec::len).sum();
    println!(
        "state {} bytes, container {on_disk}; {ROUNDS} chunks {chunk_bytes} bytes, journal {}",
        state_json.len(),
        jref.bytes
    );
    assert!(
        on_disk * 4 < state_json.len() as u64,
        "zstd shrinks the state: {on_disk} bytes on disk for {}",
        state_json.len()
    );

    // Read back: the header alone, the container, the journal's prefix, then the game.
    assert_eq!(read_header(&cpath).expect("the header reads"), header);
    let c = read_container(&cpath).expect("the container reads");
    assert_eq!(c.header(), &header);
    assert!(c.state() == state_json.as_slice(), "the state comes back as it was spliced in");
    assert_eq!(c.chain(), Some(&chain));
    let named = c.header().journal.clone().expect("it names its journal");
    let back = read_upto(&dir.join(&named.file), &named).expect("the journal reads");
    assert!(back == chunks, "the 330 chunks come back as they were appended");
    let loaded = save::load(r, c.state(), &mut back.iter().map(Vec::as_slice)).expect("it loads");
    assert!(!loaded.report.chronicle_incomplete, "the whole history");
    assert!(loaded.state == st, "the same state");
    assert!(loaded.chronicle == chron, "the same history");

    let _done = std::fs::remove_dir_all(&dir);
}
