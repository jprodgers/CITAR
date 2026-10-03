//! Gate 2: a 50-record journal cut at every byte, and damaged at every byte inside a complete
//! record (DESIGN.md P2.5.2).
//!
//! - Cut anywhere, `open` recovers exactly the complete records before the cut and truncates
//!   only the partial one; the next append then writes the bytes the cut removed.
//! - Damaged anywhere inside a record, `open` truncates nothing, reports `corrupt_at` at that
//!   record and refuses appends, and a fork of the good prefix opens clean.
//! - Either way `read_upto` reads every prefix within the good records and refuses one past them
//!   or with another head.
//!
//! The file-level runs try two changes of each byte (`^ 0x01` and `^ 0x80`: a length one off,
//! within the file, and one that runs past its end or past any record's limit); the journal
//! module's own test tries all 255 of each byte in memory. The cases run on a few threads, each
//! in its own folder.

use std::path::Path;
use std::sync::OnceLock;

use citar_store::journal::{JournalRef, JournalWriter};
use citar_store::{Recovered, StoreError, fork, read_upto};

use super::common::{Dir, renamed, write_journal};

/// The gate's journal: its chunks, its bytes, and the reference after each record.
struct Fifty {
    chunks: Vec<Vec<u8>>,
    bytes: Vec<u8>,
    refs: Vec<JournalRef>,
}

/// Fifty chunks of every kind: noise a few bytes long (raw), every seventh some text (stored
/// as zstd), and some empty.
fn fifty() -> &'static Fifty {
    static F: OnceLock<Fifty> = OnceLock::new();
    F.get_or_init(|| {
        let chunks: Vec<Vec<u8>> = (0..50u32)
            .map(|i| match i {
                _ if i % 7 == 3 => format!(
                    "{{\"seq\":{i},\"entries\":[{}]}}",
                    ["{\"event\":{\"text\":\"a city grew\"}}"; 12].join(",")
                )
                .into_bytes(),
                _ if i % 11 == 5 => Vec::new(),
                _ => (0..(i * 13 % 23) + 1)
                    .map(|j| (j.wrapping_mul(97) ^ i.wrapping_mul(31)) as u8)
                    .collect(),
            })
            .collect();
        let dir = Dir::new("fifty");
        let path = dir.join("journal.cjnl");
        let refs = write_journal(&path, &chunks);
        let bytes = std::fs::read(&path).expect("reads");
        assert_eq!(refs.len(), 51);
        println!("the gate's journal: 50 records, {} bytes", bytes.len());
        Fifty { chunks, bytes, refs }
    })
}

/// The record holding byte `p` (or the whole records before a cut at `p`).
fn record_at(f: &Fifty, p: u64) -> usize {
    f.refs.iter().rposition(|r| r.bytes <= p).unwrap_or(0)
}

/// Runs `case(dir, index)` for each of `n` cases on a few threads, each with its own folder.
fn spread(name: &str, n: usize, case: impl Fn(&Dir, usize) + Sync) {
    let threads = std::thread::available_parallelism().map_or(4, |t| t.get()).clamp(1, 8);
    std::thread::scope(|s| {
        for t in 0..threads {
            let case = &case;
            s.spawn(move || {
                let dir = Dir::new(name);
                for i in (t..n).step_by(threads) {
                    case(&dir, i);
                }
            });
        }
    });
}

/// `read_upto` on every prefix it should read and a sample of those it should refuse: those
/// within the first `good` records read back their chunks; one past them, the whole journal's
/// when it is past them, and one with another head are refused.
fn reads_prefixes_within(f: &Fifty, path: &Path, good: usize, what: &str) {
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for j in [0, good / 2, good] {
        let got = read_upto(path, &renamed(&f.refs[j], &file));
        assert_eq!(got.ok().as_deref(), Some(&f.chunks[..j]), "{what}: prefix {j} reads");
    }
    for j in [good + 1, 50] {
        if j <= 50 && j > good {
            let err =
                read_upto(path, &renamed(&f.refs[j], &file)).expect_err("past the good records");
            assert!(
                matches!(err, StoreError::Mismatch { .. } | StoreError::Corrupt { .. }),
                "{what}: prefix {j}: {err}"
            );
        }
    }
    if good > 0 {
        let mut other = renamed(&f.refs[good], &file);
        other.head =
            if other.head.starts_with('0') { "1" } else { "0" }.to_owned() + &other.head[1..];
        let err = read_upto(path, &other).expect_err("another head");
        assert!(matches!(err, StoreError::Mismatch { .. }), "{what}: {err}");
    }
}

#[test]
fn every_cut_recovers_exactly_the_whole_records_before_it() {
    let f = fifty();
    spread("cut", f.bytes.len() + 1, |dir, t| {
        let path = dir.join("journal.cjnl");
        std::fs::write(&path, &f.bytes[..t]).expect("writes");
        let (w, rec) = JournalWriter::open(&path).unwrap_or_else(|e| panic!("cut at {t}: {e}"));
        let k = record_at(f, t as u64);
        let end = f.refs[k].bytes;
        let torn = if t < 10 {
            (t > 0).then_some(t as u64)
        } else {
            (t as u64 > end).then(|| t as u64 - end)
        };
        assert_eq!(
            rec,
            Recovered { records: k as u32, bytes: end, torn, corrupt_at: None },
            "cut at {t}"
        );
        assert_eq!(w.reference(), f.refs[k], "cut at {t}");
        drop(w);
        assert_eq!(
            std::fs::read(&path).expect("reads"),
            &f.bytes[..end as usize],
            "cut at {t}: only the partial record is gone"
        );
        reads_prefixes_within(f, &path, k, &format!("cut at {t}"));
        // It opens clean now, and the next append writes back what the cut took.
        let (mut w, rec) = JournalWriter::open(&path).expect("reopens");
        assert_eq!((rec.records, rec.torn, rec.corrupt_at), (k as u32, None, None), "cut at {t}");
        if k < 50 {
            w.append(k as u32, &f.chunks[k]).expect("appends");
            drop(w);
            assert_eq!(
                std::fs::read(&path).expect("reads"),
                &f.bytes[..f.refs[k + 1].bytes as usize],
                "cut at {t}: the same record again"
            );
        }
    });
}

#[test]
fn every_damaged_byte_inside_a_record_is_corruption_at_that_record() {
    let f = fifty();
    let first = 10usize;
    let masks = [0x01u8, 0x80];
    spread("damage", (f.bytes.len() - first) * masks.len(), |dir, i| {
        let p = first + i / masks.len();
        let mask = masks[i % masks.len()];
        let what = format!("byte {p} ^ {mask:#04x}");
        let path = dir.join("journal.cjnl");
        let mut bad = f.bytes.clone();
        bad[p] ^= mask;
        std::fs::write(&path, &bad).expect("writes");
        let k = record_at(f, p as u64);
        let at = f.refs[k].bytes;
        let (mut w, rec) = JournalWriter::open(&path).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(
            rec,
            Recovered { records: k as u32, bytes: at, torn: None, corrupt_at: Some(at) },
            "{what}"
        );
        assert_eq!(w.reference(), f.refs[k], "{what}: the good prefix");
        let err = w.append(k as u32, &f.chunks[k]).expect_err("appends are refused");
        assert!(matches!(err, StoreError::Corrupt { .. }), "{what}: {err}");
        let err = w.truncate_to(&f.refs[k]).expect_err("a corrupt journal is forked");
        assert!(matches!(err, StoreError::Corrupt { .. }), "{what}: {err}");
        drop(w);
        assert_eq!(std::fs::read(&path).expect("reads"), bad, "{what}: nothing truncated");
        reads_prefixes_within(f, &path, k, &what);

        // The fork of the good prefix opens clean and carries on.
        let forked = dir.join("journal-2.cjnl");
        fork(&path, &f.refs[k], &forked).unwrap_or_else(|e| panic!("{what}: {e}"));
        let (mut w, rec) = JournalWriter::open(&forked).expect("the fork opens");
        assert_eq!(
            rec,
            Recovered { records: k as u32, bytes: at, torn: None, corrupt_at: None },
            "{what}: the fork is clean"
        );
        w.append(k as u32, &f.chunks[k]).expect("the fork takes appends");
        drop(w);
        assert_eq!(
            std::fs::read(&forked).expect("reads"),
            &f.bytes[..f.refs[k + 1].bytes as usize],
            "{what}: the fork carries on as the original did"
        );
        std::fs::remove_file(&forked).expect("removes");
    });
}

#[test]
fn a_journal_cut_inside_its_header_starts_again() {
    let dir = Dir::new("header");
    let path = dir.join("journal.cjnl");
    for t in 0..10 {
        std::fs::write(&path, &b"CITARJNL\x01\x00"[..t]).expect("writes");
        let (w, rec) = JournalWriter::open(&path).expect("opens");
        assert_eq!(
            rec,
            Recovered {
                records: 0,
                bytes: 10,
                torn: (t > 0).then_some(t as u64),
                corrupt_at: None
            }
        );
        drop(w);
        assert_eq!(std::fs::read(&path).expect("reads"), b"CITARJNL\x01\x00");
    }
    // A short file that is not the start of a journal is not one.
    std::fs::write(&path, b"CITARX").expect("writes");
    assert!(matches!(JournalWriter::open(&path), Err(StoreError::Format { .. })));
    assert_eq!(std::fs::read(&path).expect("reads"), b"CITARX", "left as it was");
}
