//! Gate 4: arbitrary bytes as a container or a journal are always an error, never a panic
//! (10,000 cases each in the gate's run, `PROPTEST_CASES=10000`), and valid files mangled at
//! random (cut, bytes changed, inserted or overwritten) never panic either, and whatever still
//! reads is self-consistent.
//!
//! "Always an error" has one exception by design: a journal file that is the start of a
//! journal's 10-byte header (the empty file included) is one whose creation a crash cut short,
//! and opens as a fresh journal.

use std::path::Path;
use std::sync::OnceLock;

use citar_store::journal::{JournalRef, JournalWriter};
use citar_store::{BodyParts, fork, read_container, read_header, read_upto};
use proptest::prelude::*;

use super::common::{self, Body, Dir, write_journal};

const JOURNAL_HEADER: &[u8] = b"CITARJNL\x01\x00";

fn empty_ref() -> JournalRef {
    JournalRef { file: "j.cjnl".into(), records: 0, bytes: 10, head: String::new() }
}

/// Bytes that start like nothing in particular, or like each format's magic, so the parsers
/// past the magic are reached too.
fn arbitrary() -> impl Strategy<Value = Vec<u8>> {
    let tail = prop::collection::vec(any::<u8>(), 0..2048);
    prop_oneof![
        3 => tail.clone(),
        1 => tail.clone().prop_map(|t| [b"CITARSV2".as_slice(), &t].concat()),
        1 => (any::<u16>(), tail.clone()).prop_map(|(n, t)| {
            [b"CITARSV2".as_slice(), &u32::from(n).to_le_bytes(), &t].concat()
        }),
        1 => tail.clone().prop_map(|t| [b"CITARJNL".as_slice(), &t].concat()),
        1 => tail.prop_map(|t| [JOURNAL_HEADER, &t].concat()),
    ]
}

/// A valid container's bytes and a valid journal's, written once, to mangle.
struct Originals {
    container: Vec<u8>,
    journal: Vec<u8>,
    chunks: Vec<Vec<u8>>,
    refs: Vec<JournalRef>,
}

fn originals() -> &'static Originals {
    static O: OnceLock<Originals> = OnceLock::new();
    O.get_or_init(|| {
        let dir = Dir::new("originals");
        let c = dir.join("a.citar");
        let state = format!("{{\"tiles\":{:?},\"n\":[1,2.5,-3]}}", "AQID".repeat(300));
        let b = Body::small();
        citar_store::write_container(
            &c,
            &common::header(None),
            &BodyParts { state: state.as_bytes(), ..b.parts() },
        )
        .expect("writes");
        let j = dir.join("j.cjnl");
        let chunks: Vec<Vec<u8>> = (0..12u8)
            .map(|i| {
                if i % 3 == 0 {
                    b"{\"entries\":[]}".repeat(20)
                } else {
                    vec![i; usize::from(i) * 5]
                }
            })
            .collect();
        let refs = write_journal(&j, &chunks);
        Originals {
            container: std::fs::read(&c).expect("reads"),
            journal: std::fs::read(&j).expect("reads"),
            chunks,
            refs,
        }
    })
}

/// One change to a file's bytes.
#[derive(Clone, Debug)]
enum Edit {
    Cut(prop::sample::Index),
    Flip(prop::sample::Index, u8),
    Insert(prop::sample::Index, Vec<u8>),
    Overwrite(prop::sample::Index, Vec<u8>),
}

fn edits() -> impl Strategy<Value = Vec<Edit>> {
    let edit = prop_oneof![
        any::<prop::sample::Index>().prop_map(Edit::Cut),
        (any::<prop::sample::Index>(), 1..=255u8).prop_map(|(i, m)| Edit::Flip(i, m)),
        (any::<prop::sample::Index>(), prop::collection::vec(any::<u8>(), 1..16))
            .prop_map(|(i, b)| Edit::Insert(i, b)),
        (any::<prop::sample::Index>(), prop::collection::vec(any::<u8>(), 1..32))
            .prop_map(|(i, b)| Edit::Overwrite(i, b)),
    ];
    prop::collection::vec(edit, 1..4)
}

fn mangle(original: &[u8], edits: &[Edit]) -> Vec<u8> {
    let mut b = original.to_vec();
    for e in edits {
        if b.is_empty() {
            break;
        }
        match e {
            Edit::Cut(i) => b.truncate(i.index(b.len())),
            Edit::Flip(i, m) => {
                let at = i.index(b.len());
                b[at] ^= m;
            }
            Edit::Insert(i, x) => {
                let at = i.index(b.len() + 1);
                b.splice(at..at, x.iter().copied());
            }
            Edit::Overwrite(i, x) => {
                let at = i.index(b.len());
                for (k, v) in x.iter().enumerate() {
                    if let Some(slot) = b.get_mut(at + k) {
                        *slot = *v;
                    }
                }
            }
        }
    }
    b
}

/// Everything a reader of containers does with `path`: never a panic; a header that reads is
/// the one the whole container reads with.
fn read_as_container(path: &Path) -> Result<(), TestCaseError> {
    let header = read_header(path);
    if let Ok(c) = read_container(path) {
        let header = header.ok();
        prop_assert_eq!(header.as_ref(), Some(c.header()), "the header it reads alone");
        let doc: serde_json::Value = serde_json::from_slice(c.body())
            .map_err(|e| TestCaseError::fail(format!("its body is JSON: {e}")))?;
        prop_assert!(doc.is_object());
        prop_assert!(c.state().first() == Some(&b'{'), "the state is an object");
    }
    Ok(())
}

/// Everything a journal's reader and writer do with `path`, which holds `bytes`: never a panic;
/// what opens is consistent with what reads.
fn use_as_journal(dir: &Dir, path: &Path, bytes: &[u8]) -> Result<(), TestCaseError> {
    let fresh = JOURNAL_HEADER.starts_with(bytes);
    let refs = [empty_ref()];
    let read = read_upto(path, &refs[0]);
    prop_assert_eq!(read.is_ok(), bytes.starts_with(JOURNAL_HEADER), "{:?}", read.err());
    let forked = dir.join("fork.cjnl");
    let _left_from_the_last_case = std::fs::remove_file(&forked);
    let f = fork(path, &refs[0], &forked);
    prop_assert_eq!(f.is_ok(), bytes.starts_with(JOURNAL_HEADER), "{:?}", f.err());
    match JournalWriter::open(path) {
        Ok((w, rec)) => {
            prop_assert!(fresh || bytes.starts_with(JOURNAL_HEADER), "only a journal opens");
            let r = w.reference();
            prop_assert_eq!((r.records, r.bytes), (rec.records, rec.bytes));
            drop(w);
            // What it kept is what it reads, and a second open finds nothing more to do.
            let back = read_upto(path, &r)
                .map_err(|e| TestCaseError::fail(format!("the good prefix reads: {e}")))?;
            prop_assert_eq!(back.len(), r.records as usize);
            let (_, again) = JournalWriter::open(path)
                .map_err(|e| TestCaseError::fail(format!("it opens again: {e}")))?;
            prop_assert_eq!(again.torn, None, "the tail was truncated the first time");
            prop_assert_eq!((again.records, again.corrupt_at), (rec.records, rec.corrupt_at));
        }
        Err(e) => prop_assert!(
            !fresh && !bytes.starts_with(JOURNAL_HEADER),
            "a journal's header, whole or cut short, opens: {}",
            e
        ),
    }
    Ok(())
}

/// Writes `bytes` over the case's file.
fn put(path: &Path, bytes: &[u8]) -> Result<(), TestCaseError> {
    std::fs::write(path, bytes).map_err(|e| TestCaseError::fail(format!("{}: {e}", path.display())))
}

/// Gate 4: arbitrary bytes are no container.
#[test]
fn arbitrary_bytes_are_no_container() {
    common::check(file!(), "any-container", arbitrary(), |dir, bytes| {
        let path = dir.join("a.citar");
        put(&path, &bytes)?;
        prop_assert!(read_header(&path).is_err());
        prop_assert!(read_container(&path).is_err());
        Ok(())
    });
}

/// Gate 4: arbitrary bytes are no journal. A journal's whole header and noise after it (which
/// the strategy makes on purpose) is one, whose records are what it recovers.
#[test]
fn arbitrary_bytes_are_no_journal() {
    common::check(file!(), "any-journal", arbitrary(), |dir, bytes| {
        let path = dir.join("j.cjnl");
        put(&path, &bytes)?;
        if bytes.starts_with(JOURNAL_HEADER) {
            return use_as_journal(dir, &path, &bytes);
        }
        prop_assert!(read_upto(&path, &empty_ref()).is_err());
        prop_assert!(fork(&path, &empty_ref(), &dir.join("fork.cjnl")).is_err());
        let fresh = JOURNAL_HEADER.starts_with(&bytes);
        match JournalWriter::open(&path) {
            Ok((_, rec)) => {
                prop_assert!(fresh && rec.records == 0, "only a cut short header opens");
            }
            Err(_) => prop_assert!(!fresh, "a cut short header opens"),
        }
        Ok(())
    });
}

/// A valid container mangled at random: never a panic.
#[test]
fn mangled_containers_never_panic() {
    common::check(file!(), "mangled-container", edits(), |dir, edits| {
        let path = dir.join("a.citar");
        let bytes = mangle(&originals().container, &edits);
        put(&path, &bytes)?;
        read_as_container(&path)?;
        // Read as a journal too: nothing that is not one opens (a file cut to nothing is a fresh
        // journal).
        use_as_journal(dir, &path, &bytes)
    });
}

/// A valid journal mangled at random: never a panic, and every prefix the original names either
/// reads back the original's chunks or is refused (the chained hashes see to it).
#[test]
fn mangled_journals_never_panic() {
    common::check(file!(), "mangled-journal", edits(), |dir, edits| {
        let path = dir.join("j.cjnl");
        let o = originals();
        let bytes = mangle(&o.journal, &edits);
        put(&path, &bytes)?;
        for r in &o.refs {
            if let Ok(chunks) = read_upto(&path, r) {
                prop_assert_eq!(chunks.as_slice(), &o.chunks[..r.records as usize]);
            }
        }
        use_as_journal(dir, &path, &bytes)?;
        read_as_container(&path)
    });
}
