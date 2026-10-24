//! The journal (DESIGN.md P2.5.2): gate 1's round trips of record sequences, the format byte
//! for byte, appends in order, the codecs, and what `read_upto`, `fork` and `truncate_to` refuse.

use citar_store::journal::{self, JournalRef, JournalWriter};
use citar_store::{StoreError, fork, read_upto};
use proptest::prelude::*;

use super::common::{self, Dir, renamed, write_journal};

fn fail(e: StoreError) -> TestCaseError {
    TestCaseError::fail(e.to_string())
}

/// Gate 1: any sequence of chunks appends, reopens clean, and reads back whole and by every
/// prefix; a fork reads back as its prefix; truncating to a prefix and appending the same chunks
/// again writes the same bytes.
#[test]
fn record_sequences_round_trip() {
    let cases = (prop::collection::vec(common::chunk(), 0..24), any::<prop::sample::Index>());
    common::check(file!(), "records", cases, |dir, (chunks, cut)| {
        let path = dir.join("journal.cjnl");
        let forked = dir.join("journal-2.cjnl");
        for p in [&path, &forked] {
            let _last_case = std::fs::remove_file(p);
        }
        let refs = write_journal(&path, &chunks);
        let bytes = std::fs::read(&path).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let all = refs.last().cloned().unwrap_or_else(|| refs[0].clone());
        prop_assert_eq!(all.bytes, bytes.len() as u64);
        prop_assert_eq!(all.records as usize, chunks.len());
        prop_assert_eq!(&read_upto(&path, &all).map_err(fail)?, &chunks);

        let k = cut.index(refs.len());
        let prefix = read_upto(&path, &refs[k]).map_err(fail)?;
        prop_assert_eq!(prefix.as_slice(), &chunks[..k]);
        fork(&path, &refs[k], &forked).map_err(fail)?;
        let r2 = renamed(&refs[k], "journal-2.cjnl");
        let prefix = read_upto(&forked, &r2).map_err(fail)?;
        prop_assert_eq!(prefix.as_slice(), &chunks[..k]);
        let copy = std::fs::read(&forked).unwrap_or_default();
        prop_assert_eq!(copy.as_slice(), &bytes[..refs[k].bytes as usize]);

        let (mut w, rec) = JournalWriter::open(&path).map_err(fail)?;
        prop_assert_eq!(
            (rec.records, rec.bytes, rec.torn, rec.corrupt_at),
            (all.records, all.bytes, None, None)
        );
        prop_assert_eq!(w.reference(), all.clone());
        w.truncate_to(&refs[k]).map_err(fail)?;
        prop_assert_eq!(w.reference(), refs[k].clone());
        for (i, c) in chunks.iter().enumerate().skip(k) {
            w.append(i as u32, c).map_err(fail)?;
        }
        w.sync().map_err(fail)?;
        prop_assert_eq!(w.reference(), all);
        drop(w);
        let again = std::fs::read(&path).unwrap_or_default();
        prop_assert_eq!(again, bytes, "the writer is deterministic");
        let (_, rec) = JournalWriter::open(&forked).map_err(fail)?;
        prop_assert_eq!((rec.records, rec.torn, rec.corrupt_at), (k as u32, None, None));
        Ok(())
    });
}

#[test]
fn a_new_journal_is_its_header() {
    let dir = Dir::new("new");
    let path = dir.join("journal.cjnl");
    let (w, rec) = JournalWriter::open(&path).expect("creates");
    assert_eq!(rec, citar_store::Recovered { records: 0, bytes: 10, torn: None, corrupt_at: None });
    assert_eq!(
        w.reference(),
        JournalRef { file: "journal.cjnl".into(), records: 0, bytes: 10, head: String::new() }
    );
    assert_eq!(w.path(), path);
    drop(w);
    assert_eq!(std::fs::read(&path).expect("reads"), b"CITARJNL\x01\x00");
    assert_eq!(
        read_upto(
            &path,
            &JournalRef { file: "journal.cjnl".into(), records: 0, bytes: 10, head: String::new() }
        )
        .expect("reads"),
        Vec::<Vec<u8>>::new()
    );
}

/// The records' bytes, by hand: the head's fields, the chained hash, and the codec chosen.
#[test]
fn records_are_the_format_byte_for_byte() {
    let dir = Dir::new("format");
    let path = dir.join("journal.cjnl");
    let small = b"{\"seq\":0}".to_vec();
    let text = format!(
        "{{\"entries\":[{}]}}",
        vec!["{\"event\":{\"text\":\"Rome founded\"}}"; 200].join(",")
    );
    // xorshift64: bytes zstd cannot shrink.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let noise: Vec<u8> = (0..3000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect();
    let chunks = vec![small.clone(), text.clone().into_bytes(), noise.clone()];
    let refs = write_journal(&path, &chunks);
    let b = std::fs::read(&path).expect("reads");
    assert_eq!(&b[..10], b"CITARJNL\x01\x00");

    let mut at = 10usize;
    let mut prev = [0u8; 8];
    let mut codecs = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let len = u32::from_le_bytes(b[at..at + 4].try_into().expect("4")) as usize;
        let seq = u32::from_le_bytes(b[at + 4..at + 8].try_into().expect("4"));
        let (kind, codec) = (b[at + 8], b[at + 9]);
        let hash: [u8; 8] = b[at + 10..at + 18].try_into().expect("8");
        let payload = &b[at + 18..at + 18 + len];
        assert_eq!((seq, kind), (i as u32, 1));
        let mut h = blake3::Hasher::new();
        h.update(&prev);
        h.update(&seq.to_le_bytes());
        h.update(&[kind, codec]);
        h.update(payload);
        assert_eq!(&h.finalize().as_bytes()[..8], &hash, "record {i}'s chained hash");
        let back = match codec {
            0 => payload.to_vec(),
            1 => zstd::stream::decode_all(payload).expect("one zstd frame"),
            _ => panic!("codec {codec}"),
        };
        assert_eq!(&back, c);
        codecs.push(codec);
        prev = hash;
        at += 18 + len;
        assert_eq!(refs[i + 1].bytes, at as u64);
        assert_eq!(refs[i + 1].head, hash.iter().map(|x| format!("{x:02x}")).collect::<String>());
    }
    assert_eq!(at, b.len());
    assert_eq!(codecs, [0, 1, 0], "small and noisy chunks raw, text compressed");
    assert!(b.len() < small.len() + text.len() / 5 + noise.len() + 100, "the text compresses");
}

#[test]
fn the_head_stands_for_the_whole_prefix() {
    // Two journals that end with the same chunk but differ before it have different heads, so a
    // save naming one cannot read the other.
    let dir = Dir::new("chained");
    let (a, b) = (dir.join("a.cjnl"), dir.join("b.cjnl"));
    let ra = write_journal(&a, &[b"one".to_vec(), b"same".to_vec()]);
    let rb = write_journal(&b, &[b"two".to_vec(), b"same".to_vec()]);
    assert_ne!(ra[2].head, rb[2].head);
    assert_eq!((ra[2].records, ra[2].bytes), (rb[2].records, rb[2].bytes));
    let err = read_upto(&b, &renamed(&ra[2], "b.cjnl")).expect_err("another timeline");
    assert!(
        matches!(&err, StoreError::Mismatch { why, .. } if why.contains("rewritten, forked or swapped")),
        "{err}"
    );
    let err = fork(&b, &ra[2], &dir.join("c.cjnl")).expect_err("another timeline");
    assert!(matches!(err, StoreError::Mismatch { .. }), "{err}");
    assert!(!dir.join("c.cjnl").exists(), "nothing forked");
}

#[test]
fn appends_go_in_order_and_survive_a_reopen() {
    let dir = Dir::new("order");
    let path = dir.join("journal.cjnl");
    let (mut w, _) = JournalWriter::open(&path).expect("opens");
    assert!(matches!(w.append(1, b"x"), Err(StoreError::OutOfOrder { want: 0, got: 1 })));
    w.append(0, b"zero").expect("appends");
    assert!(matches!(w.append(0, b"again"), Err(StoreError::OutOfOrder { want: 1, got: 0 })));
    assert!(matches!(w.append(5, b"x"), Err(StoreError::OutOfOrder { want: 1, got: 5 })));
    w.append(1, b"one").expect("appends");
    w.sync().expect("syncs");
    let r = w.reference();
    drop(w);
    let (mut w, rec) = JournalWriter::open(&path).expect("reopens");
    assert_eq!((rec.records, rec.bytes, rec.torn, rec.corrupt_at), (2, r.bytes, None, None));
    assert_eq!(w.reference(), r);
    assert!(matches!(w.append(1, b"x"), Err(StoreError::OutOfOrder { want: 2, got: 1 })));
    w.append(2, b"two").expect("carries on");
    let all = w.reference();
    drop(w);
    assert_eq!(
        read_upto(&path, &all).expect("reads"),
        [b"zero".to_vec(), b"one".to_vec(), b"two".to_vec()]
    );
}

#[test]
fn read_upto_refuses_what_the_journal_does_not_hold() {
    let dir = Dir::new("refuse");
    let path = dir.join("journal.cjnl");
    let chunks: Vec<Vec<u8>> = (0..5u8).map(|i| vec![i; 10 + usize::from(i)]).collect();
    let refs = write_journal(&path, &chunks);
    let r3 = refs[3].clone();
    let past = JournalRef { records: 6, bytes: refs[5].bytes + 28, ..refs[5].clone() };
    for (r, what) in [
        (past, "past the end"),
        (JournalRef { bytes: r3.bytes + 1, ..r3.clone() }, "another end"),
        (JournalRef { head: "0123456789abcdef".into(), ..r3.clone() }, "another head"),
        (JournalRef { records: 2, ..r3.clone() }, "another count"),
        (JournalRef { head: "XYZ".into(), ..r3.clone() }, "a head that is not hex"),
        (JournalRef { records: 0, ..r3.clone() }, "an empty prefix with a head"),
    ] {
        let err = read_upto(&path, &r).expect_err(what);
        assert!(matches!(err, StoreError::Mismatch { .. }), "{what}: {err}");
    }
    assert_eq!(read_upto(&path, &r3).expect("reads"), chunks[..3].to_vec());
    // A file that is not a journal, of another version, or missing.
    let other = dir.join("other.cjnl");
    std::fs::write(&other, b"CITARJNL\x02\x00").expect("writes");
    let err = read_upto(&other, &refs[0]).expect_err("version 2");
    assert!(matches!(&err, StoreError::Format { why, .. } if why.contains("version 2")), "{err}");
    std::fs::write(&other, b"{\"not\": \"a journal\"}").expect("writes");
    assert!(matches!(read_upto(&other, &refs[0]), Err(StoreError::Format { .. })));
    assert!(matches!(read_upto(&dir.join("missing.cjnl"), &refs[0]), Err(StoreError::Io { .. })));
    assert!(matches!(JournalWriter::open(&other), Err(StoreError::Format { .. })));
}

#[test]
fn a_fork_never_overwrites() {
    let dir = Dir::new("fork");
    let path = dir.join("journal.cjnl");
    let refs = write_journal(&path, &[b"a".to_vec(), b"b".to_vec()]);
    let target = dir.join("journal-2.cjnl");
    std::fs::write(&target, b"someone's").expect("writes");
    let err = fork(&path, &refs[1], &target).expect_err("exists");
    assert!(
        matches!(&err, StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::AlreadyExists),
        "{err}"
    );
    assert_eq!(std::fs::read(&target).expect("reads"), b"someone's");
    let err = fork(&path, &refs[1], &path).expect_err("onto itself");
    assert!(matches!(err, StoreError::Io { .. }), "{err}");
    assert_eq!(read_upto(&path, &refs[2]).expect("unchanged").len(), 2);
    // The empty prefix forks to a bare header.
    let empty = dir.join("journal-3.cjnl");
    fork(&path, &refs[0], &empty).expect("forks");
    assert_eq!(std::fs::read(&empty).expect("reads"), b"CITARJNL\x01\x00");
}

#[test]
fn truncate_to_drops_the_records_of_a_save_that_never_completed() {
    let dir = Dir::new("truncate");
    let path = dir.join("journal.cjnl");
    let chunks: Vec<Vec<u8>> = (0..6u8).map(|i| vec![i; 40]).collect();
    let refs = write_journal(&path, &chunks);
    let (mut w, _) = JournalWriter::open(&path).expect("opens");
    // Not its own prefixes: refused, nothing changes.
    let other = dir.join("other.cjnl");
    let foreign = write_journal(&other, &[b"x".to_vec(), b"y".to_vec(), b"z".to_vec()]);
    for r in [
        renamed(&foreign[3], "journal.cjnl"),
        JournalRef { records: 7, bytes: refs[6].bytes + 58, ..refs[6].clone() },
        JournalRef { bytes: refs[4].bytes - 1, ..refs[4].clone() },
        JournalRef { file: "a/b".into(), ..refs[2].clone() },
    ] {
        let err = w.truncate_to(&r).expect_err("not a prefix");
        assert!(matches!(err, StoreError::Mismatch { .. }), "{r:?}: {err}");
    }
    assert_eq!(w.reference(), refs[6]);
    w.truncate_to(&refs[4]).expect("truncates");
    assert_eq!(w.reference(), refs[4]);
    assert!(matches!(w.append(5, b"x"), Err(StoreError::OutOfOrder { want: 4, got: 5 })));
    w.append(4, b"another four").expect("appends");
    drop(w);
    let got = read_upto(&path, &JournalRef { records: 5, ..refs[5].clone() });
    assert!(got.is_err(), "the old record 4 is gone");
    let (w, rec) = JournalWriter::open(&path).expect("reopens");
    assert_eq!((rec.records, rec.torn, rec.corrupt_at), (5, None, None));
    let r5 = w.reference();
    drop(w);
    let mut want = chunks[..4].to_vec();
    want.push(b"another four".to_vec());
    assert_eq!(read_upto(&path, &r5).expect("reads"), want);
    // To nothing at all.
    let (mut w, _) = JournalWriter::open(&path).expect("reopens");
    w.truncate_to(&refs[0]).expect("truncates");
    drop(w);
    assert_eq!(std::fs::read(&path).expect("reads"), b"CITARJNL\x01\x00");
}

#[test]
fn a_chunk_over_the_limit_is_refused() {
    let dir = Dir::new("limit");
    let path = dir.join("journal.cjnl");
    let (mut w, _) = JournalWriter::open(&path).expect("opens");
    let big = vec![0u8; journal::MAX_CHUNK as usize + 1];
    let err = w.append(0, &big).expect_err("too long");
    assert!(matches!(err, StoreError::Invalid { .. }), "{err}");
    w.append(0, b"fine").expect("the next is still 0");
    assert_eq!(w.reference().records, 1);
}
