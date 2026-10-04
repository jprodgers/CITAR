//! Gate 3: one writer per journal, by the OS lock (DESIGN.md P2.5.2), on Windows and Linux.
//!
//! A second `JournalWriter::open` of a journal already open fails with `StoreError::Locked`,
//! whether the first writer is in this process (a second handle; Linux's `flock` and Windows's
//! `LockFileEx` both lock per open file) or in another one. While a writer holds it, the readers
//! (`read_upto`, `fork`) are refused alike on both systems; once it is dropped, all of them work.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use citar_store::journal::JournalWriter;
use citar_store::{StoreError, fork, read_upto};

use super::common::{Dir, write_journal};

/// The variable that makes [`child_holds_a_journal`] hold the journal it names.
const HOLD: &str = "CITAR_STORE_TEST_HOLD";

#[test]
fn a_second_writer_is_refused_while_the_first_holds_the_journal() {
    let dir = Dir::new("lock");
    let path = dir.join("journal.cjnl");
    let refs = write_journal(&path, &[b"zero".to_vec(), b"one".to_vec()]);
    let (mut first, _) = JournalWriter::open(&path).expect("the first writer opens");
    let err = JournalWriter::open(&path).expect_err("a second writer is refused");
    assert!(matches!(&err, StoreError::Locked { path: p } if *p == path), "{err}");
    // Readers too, the same on every system.
    assert!(matches!(read_upto(&path, &refs[2]), Err(StoreError::Locked { .. })));
    let err = fork(&path, &refs[2], &dir.join("journal-2.cjnl")).expect_err("refused");
    assert!(matches!(err, StoreError::Locked { .. }), "{err}");
    assert!(!dir.join("journal-2.cjnl").exists());
    // The first writer carries on regardless.
    first.append(2, b"two").expect("appends");
    first.sync().expect("syncs");
    let r = first.reference();
    drop(first);
    let (second, rec) = JournalWriter::open(&path).expect("free once the first is dropped");
    assert_eq!((rec.records, rec.torn, rec.corrupt_at), (3, None, None));
    drop(second);
    assert_eq!(read_upto(&path, &r).expect("reads").len(), 3);
}

#[test]
fn readers_share_and_keep_a_writer_out_while_they_read() {
    // A shared lock is only held inside read_upto and fork, so two reads in turn and a writer
    // after them all succeed.
    let dir = Dir::new("readers");
    let path = dir.join("journal.cjnl");
    let refs = write_journal(&path, &[b"a".to_vec()]);
    assert_eq!(read_upto(&path, &refs[1]).expect("reads").len(), 1);
    fork(&path, &refs[1], &dir.join("copy.cjnl")).expect("forks");
    assert!(JournalWriter::open(&path).is_ok(), "no reader holds it afterwards");
}

/// Not a test of its own: when [`HOLD`] names a journal, this opens it, says so on stdout, and
/// holds it until the file `<journal>.release` appears (or ten seconds pass). The test below
/// runs this test binary again as the other process.
#[test]
fn child_holds_a_journal() {
    let Some(path) = std::env::var_os(HOLD).map(PathBuf::from) else { return };
    let (w, _) = JournalWriter::open(&path).expect("the child opens the journal");
    println!("holding");
    let release = path.with_extension("release");
    let t = Instant::now();
    while !release.exists() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(w);
}

#[test]
fn a_journal_another_process_holds_is_refused() {
    let dir = Dir::new("process");
    let path = dir.join("journal.cjnl");
    let refs = write_journal(&path, &[b"zero".to_vec()]);
    let exe = std::env::current_exe().expect("the test binary");
    let mut child = Command::new(exe)
        .args(["store::lock::child_holds_a_journal", "--exact", "--nocapture", "--test-threads=1"])
        .env(HOLD, &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the child starts");
    let out = child.stdout.take().expect("its stdout");
    let mut lines = BufReader::new(out).lines();
    let held = lines.by_ref().map_while(Result::ok).any(|l| l.contains("holding"));
    assert!(held, "the child took the journal");

    let err = JournalWriter::open(&path).expect_err("another process holds it");
    assert!(matches!(err, StoreError::Locked { .. }), "{err}");
    assert!(matches!(read_upto(&path, &refs[1]), Err(StoreError::Locked { .. })));

    std::fs::write(path.with_extension("release"), b"").expect("writes");
    let status = child.wait().expect("the child ends");
    assert!(status.success(), "the child: {status}");
    let (w, rec) = JournalWriter::open(&path).expect("free once the other process lets go");
    assert_eq!(rec.records, 1);
    drop(w);
}
