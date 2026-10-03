//! The container (DESIGN.md P2.5.1): gate 1's round trips of headers and bodies, the atomic
//! write and its retries, and every refusal of a reader and a writer.

use citar_store::journal::JournalRef;
use citar_store::{
    BodyParts, ChainRef, Header, SessionRef, StoreError, container, read_container, read_header,
    write_container,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use super::common::{self, Body, Dir, hex64};

/// Any valid journal reference: a plain name, and counts a journal could have.
fn journal_ref() -> impl Strategy<Value = Option<JournalRef>> {
    let some = ("[a-z0-9_-]{1,16}", 0u32..5000, 0u64..1 << 40, any::<[u8; 8]>()).prop_map(
        |(stem, records, extra, hash)| {
            let file = format!("{stem}.cjnl");
            if records == 0 {
                return JournalRef { file, records, bytes: 10, head: String::new() };
            }
            let head = hash.iter().map(|b| format!("{b:02x}")).collect();
            JournalRef { file, records, bytes: 10 + 18 * u64::from(records) + extra, head }
        },
    );
    prop::option::of(some)
}

fn any_header() -> impl Strategy<Value = Header> {
    (
        ".{0,30}",
        "[0-9a-f]{12}|.{0,20}",
        "[0-9a-f]{64}",
        common::object(),
        (".{0,20}", ".{0,40}", any::<bool>()),
        journal_ref(),
    )
        .prop_map(
            |(saved_at, engine_build, rules, summary, (id, name, benchmark), journal)| Header {
                format: container::FORMAT.to_owned(),
                version: container::VERSION,
                saved_at,
                engine_build,
                rules,
                summary,
                session: SessionRef { id, name, benchmark },
                journal,
            },
        )
}

fn any_chain() -> impl Strategy<Value = Option<ChainRef>> {
    prop::option::of(
        ("[0-9a-f]{64}", any::<u32>()).prop_map(|(head, rounds)| ChainRef { head, rounds }),
    )
}

fn fail(e: StoreError) -> TestCaseError {
    TestCaseError::fail(e.to_string())
}

/// Gate 1: any header and any body write and read back as they were given; the header reads
/// alone, and the state comes back as the text spliced in.
#[test]
fn headers_and_bodies_round_trip() {
    let parts = (
        any_header(),
        common::object_text(),
        common::object_text(),
        common::object_text(),
        any_chain(),
    );
    common::check(file!(), "round-trip", parts, |dir, value| {
        let (header, (session, session_text), (metrics, metrics_text), (state, state_text), chain) =
            value;
        let path = dir.join("turn001.citar");
        let parts = BodyParts {
            session: &session,
            metrics: &metrics,
            state: &state,
            chain: chain.as_ref(),
        };
        write_container(&path, &header, &parts).map_err(fail)?;
        prop_assert_eq!(dir.names(), vec!["turn001.citar".to_owned()], "no temporary file is left");
        prop_assert_eq!(&read_header(&path).map_err(fail)?, &header);
        let c = read_container(&path).map_err(fail)?;
        prop_assert_eq!(c.header(), &header);
        prop_assert_eq!(c.state(), state_text.as_slice());
        prop_assert_eq!(c.session(), session_text.as_slice());
        prop_assert_eq!(c.metrics(), metrics_text.as_slice());
        prop_assert_eq!(c.chain(), chain.as_ref());
        // The body is exactly the four parts, in order.
        let chain_json = serde_json::to_string(&chain).unwrap_or_default();
        let body = format!(
            "{{\"session\":{},\"metrics\":{},\"state\":{},\"chain\":{chain_json}}}",
            String::from_utf8_lossy(&session_text),
            String::from_utf8_lossy(&metrics_text),
            String::from_utf8_lossy(&state_text),
        );
        prop_assert_eq!(c.body(), body.as_bytes());
        let doc: Value =
            serde_json::from_slice(c.body()).map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert!(doc.is_object());
        Ok(())
    });
}

#[test]
fn the_file_is_the_format_byte_for_byte() {
    let dir = Dir::new("bytes");
    let path = dir.join("a.citar");
    let h = common::header(None);
    let b = Body::small();
    write_container(&path, &h, &b.parts()).expect("writes");
    let bytes = std::fs::read(&path).expect("reads");
    assert_eq!(&bytes[..8], b"CITARSV2");
    let n = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let head: Value = serde_json::from_slice(&bytes[12..12 + n]).expect("the header is JSON");
    let keys: Vec<&str> =
        head.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    assert_eq!(
        keys,
        ["format", "version", "saved_at", "engine_build", "rules", "summary", "session", "journal"]
    );
    assert_eq!(head["format"], "citar-save");
    assert_eq!(head["version"], 2);
    assert_eq!(head["journal"], Value::Null);
    let frame = &bytes[12 + n..];
    assert_eq!(&frame[..4], &[0x28, 0xb5, 0x2f, 0xfd], "one zstd frame");
    let body = zstd::stream::decode_all(frame).expect("the frame decodes");
    let want = format!(
        "{{\"session\":{},\"metrics\":{},\"state\":{},\"chain\":{{\"head\":\"{}\",\"rounds\":41}}}}",
        String::from_utf8_lossy(&b.session),
        String::from_utf8_lossy(&b.metrics),
        String::from_utf8_lossy(&b.state),
        hex64(9)
    );
    assert_eq!(body, want.as_bytes());
    // The frame records its content size and a checksum (its descriptor's bit 2).
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(frame).ok().flatten(),
        Some(want.len() as u64)
    );
    assert_ne!(frame[4] & 0b100, 0, "a content checksum");
}

#[test]
fn a_header_names_its_journal() {
    let dir = Dir::new("journal-ref");
    let path = dir.join("autosave.citar");
    let j = JournalRef {
        file: "journal-2.cjnl".into(),
        records: 3,
        bytes: 130,
        head: "00ff00ff00ff00ff".into(),
    };
    let h = common::header(Some(j.clone()));
    write_container(&path, &h, &Body::small().parts()).expect("writes");
    assert_eq!(read_header(&path).expect("reads").journal, Some(j));
}

#[test]
fn a_write_replaces_the_old_save_whole() {
    let dir = Dir::new("replace");
    let path = dir.join("autosave.citar");
    let mut h = common::header(None);
    let b = Body::small();
    write_container(&path, &h, &b.parts()).expect("writes");
    h.saved_at = "later".into();
    let big = format!("{{\"tiles\":{:?}}}", "x".repeat(100_000));
    let parts = BodyParts { state: big.as_bytes(), ..b.parts() };
    write_container(&path, &h, &parts).expect("replaces");
    assert_eq!(dir.names(), ["autosave.citar"]);
    let c = read_container(&path).expect("reads");
    assert_eq!(c.header().saved_at, "later");
    assert_eq!(c.state(), big.as_bytes());
}

/// A crashed write leaves its temporary file, and a later process is often given the same id (a
/// service restarted at boot, or pid 1 in a container). Its first write must not trip over that
/// file, and must leave it: on a shared folder another machine's process may own it.
#[test]
fn a_write_passes_over_temporary_files_a_crash_left() {
    let dir = Dir::new("stale-tmp");
    let path = dir.join("autosave.citar");
    let pid = std::process::id();
    let mut stale: Vec<String> = (0..8).map(|n| format!("autosave.citar.{pid}.{n}.tmp")).collect();
    stale.extend((0..8).map(|n| format!("autosave.citar.{pid}-00000000.{n}.tmp")));
    for name in &stale {
        std::fs::write(dir.join(name), b"left by a crash").expect("writes");
    }
    write_container(&path, &common::header(None), &Body::small().parts()).expect("writes");
    write_container(&path, &common::header(None), &Body::small().parts()).expect("and again");
    assert_eq!(read_header(&path).expect("reads"), common::header(None));
    let mut want = stale.clone();
    want.push("autosave.citar".to_owned());
    want.sort();
    assert_eq!(dir.names(), want, "the left files stay, and no new one is left");
}

#[test]
fn listing_reads_the_header_without_the_body() {
    // Damage deep in the body: the header still reads, the container does not.
    let dir = Dir::new("header-only");
    let path = dir.join("a.citar");
    let state = format!(
        "{{\"tiles\":{:?}}}",
        (0..20_000).map(|i| char::from(b'a' + (i * 7 % 26) as u8)).collect::<String>()
    );
    let b = Body::small();
    write_container(
        &path,
        &common::header(None),
        &BodyParts { state: state.as_bytes(), ..b.parts() },
    )
    .expect("writes");
    let mut bytes = std::fs::read(&path).expect("reads");
    let at = bytes.len() - 40;
    bytes[at] ^= 0x55;
    std::fs::write(&path, &bytes).expect("writes");
    assert_eq!(read_header(&path).expect("the header reads"), common::header(None));
    let err = read_container(&path).expect_err("the body is damaged");
    assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
}

#[test]
fn what_breaks_the_format_is_refused_before_anything_is_written() {
    let dir = Dir::new("invalid");
    let path = dir.join("a.citar");
    let b = Body::small();
    let good = common::header(None);
    let bad_headers = [
        Header { format: "citar-game".into(), ..good.clone() },
        Header { version: 1, ..good.clone() },
        Header { rules: "abc".into(), ..good.clone() },
        Header { rules: hex64(1).to_uppercase(), ..good.clone() },
        Header { summary: json!([1, 2]), ..good.clone() },
        Header {
            journal: Some(JournalRef {
                file: "../j.cjnl".into(),
                records: 0,
                bytes: 10,
                head: String::new(),
            }),
            ..good.clone()
        },
        Header {
            journal: Some(JournalRef {
                file: "j.cjnl".into(),
                records: 2,
                bytes: 10,
                head: "0".repeat(16),
            }),
            ..good.clone()
        },
    ];
    for h in &bad_headers {
        let err = write_container(&path, h, &b.parts()).expect_err("refused");
        assert!(matches!(err, StoreError::Invalid { .. }), "{h:?}: {err}");
    }
    let bad_parts: [&[u8]; 7] = [b"", b"[]", b"1", b"{", b"{} {}", b"{\"a\":1}x", b"\"text\""];
    for p in bad_parts {
        for which in 0..3 {
            let mut parts = b.parts();
            match which {
                0 => parts.session = p,
                1 => parts.metrics = p,
                _ => parts.state = p,
            }
            let err = write_container(&path, &good, &parts).expect_err("refused");
            assert!(
                matches!(err, StoreError::Invalid { .. }),
                "{:?}: {err}",
                String::from_utf8_lossy(p)
            );
        }
    }
    let chain = ChainRef { head: "not hex".into(), rounds: 1 };
    let err = write_container(&path, &good, &BodyParts { chain: Some(&chain), ..b.parts() })
        .expect_err("refused");
    assert!(matches!(err, StoreError::Invalid { .. }), "{err}");
    assert!(dir.names().is_empty(), "nothing was written: {:?}", dir.names());
}

/// A good container's bytes, to damage.
fn good_bytes() -> Vec<u8> {
    let dir = Dir::new("good");
    let path = dir.join("a.citar");
    write_container(&path, &common::header(None), &Body::small().parts()).expect("writes");
    std::fs::read(&path).expect("reads")
}

/// Reads `bytes` as a container both ways, and returns the two errors.
fn both_errors(bytes: &[u8]) -> (StoreError, StoreError) {
    let dir = Dir::new("bad");
    let path = dir.join("a.citar");
    std::fs::write(&path, bytes).expect("writes");
    let h = read_header(&path).map(|_| ()).expect_err("the header is refused");
    let c = read_container(&path).map(|_| ()).expect_err("the container is refused");
    (h, c)
}

#[test]
fn what_is_not_a_container_is_refused_by_kind() {
    let good = good_bytes();
    let n = u32::from_le_bytes([good[8], good[9], good[10], good[11]]) as usize;

    // Not a container at all.
    for bytes in
        [&b""[..], b"CITARSV", b"CITARSV1\x00\x00\x00\x00{}", b"{\"format\":\"citar-save\"}"]
    {
        let (h, c) = both_errors(bytes);
        assert!(matches!(h, StoreError::Format { .. }), "{h}");
        assert!(matches!(c, StoreError::Format { .. }), "{c}");
    }
    // A version 1 save: gzip JSON.
    let (h, c) = both_errors(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]);
    assert!(matches!(h, StoreError::Legacy { .. }), "{h}");
    assert!(matches!(c, StoreError::Legacy { .. }), "{c}");

    // Another version's header, extra keys and all: a version this build does not read.
    let mut head: Value = serde_json::from_slice(&good[12..12 + n]).expect("JSON");
    head["version"] = json!(3);
    head["new_key"] = json!(true);
    let text = serde_json::to_vec(&head).expect("JSON");
    let mut v3 = b"CITARSV2".to_vec();
    v3.extend_from_slice(&(text.len() as u32).to_le_bytes());
    v3.extend_from_slice(&text);
    v3.extend_from_slice(&good[12 + n..]);
    let (h, c) = both_errors(&v3);
    assert!(matches!(&h, StoreError::Format { why, .. } if why.contains("version 3")), "{h}");
    assert!(matches!(c, StoreError::Format { .. }), "{c}");

    // Cut short anywhere past the magic: corrupt. The header alone reads while the frame's
    // first bytes are there.
    for cut in [8, 10, 12, 12 + n / 2, 12 + n, 12 + n + 2] {
        let (h, c) = both_errors(&good[..cut]);
        assert!(matches!(h, StoreError::Corrupt { .. }), "cut at {cut}: {h}");
        assert!(matches!(c, StoreError::Corrupt { .. }), "cut at {cut}: {c}");
    }
    for cut in [12 + n + 4, 12 + n + 9, good.len() - 1] {
        let dir = Dir::new("cut");
        let path = dir.join("a.citar");
        std::fs::write(&path, &good[..cut]).expect("writes");
        assert!(read_header(&path).is_ok(), "cut at {cut}");
        let c = read_container(&path).map(|_| ()).expect_err("cut short");
        assert!(matches!(c, StoreError::Corrupt { .. }), "cut at {cut}: {c}");
    }
    // A header length past the limit, or past the end.
    let mut huge = good.clone();
    huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    let (h, _) = both_errors(&huge);
    assert!(matches!(h, StoreError::Corrupt { at: 8, .. }), "{h}");
    // A header that is not JSON, or has a key it should not.
    let mut garbled = good.clone();
    garbled[12] = b'[';
    let (h, _) = both_errors(&garbled);
    assert!(matches!(h, StoreError::Corrupt { at: 12, .. }), "{h}");
    head["version"] = json!(2);
    let text = serde_json::to_vec(&head).expect("JSON");
    let mut extra = b"CITARSV2".to_vec();
    extra.extend_from_slice(&(text.len() as u32).to_le_bytes());
    extra.extend_from_slice(&text);
    extra.extend_from_slice(&good[12 + n..]);
    let (h, _) = both_errors(&extra);
    assert!(matches!(&h, StoreError::Corrupt { why, .. } if why.contains("new_key")), "{h}");
    // Bytes after the frame, and a second frame.
    let mut trailing = good.clone();
    trailing.push(0);
    let dir = Dir::new("trailing");
    let path = dir.join("a.citar");
    std::fs::write(&path, &trailing).expect("writes");
    assert!(read_header(&path).is_ok(), "the header does not look past the frame's start");
    assert!(matches!(read_container(&path), Err(StoreError::Corrupt { .. })));
    let mut doubled = good.clone();
    doubled.extend_from_slice(&good[12 + n..]);
    std::fs::write(&path, &doubled).expect("writes");
    assert!(matches!(read_container(&path), Err(StoreError::Corrupt { .. })));
    // A frame whose body is not a container's (a body of another shape, compressed whole).
    let frame = zstd::bulk::compress(b"{\"state\":{}}", 3).expect("compresses");
    let mut odd = good[..12 + n].to_vec();
    odd.extend_from_slice(&frame);
    std::fs::write(&path, &odd).expect("writes");
    let err = read_container(&path).map(|_| ()).expect_err("refused");
    assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
    // A missing file.
    let gone = dir.join("missing.citar");
    assert!(matches!(read_header(&gone), Err(StoreError::Io { .. })));
    assert!(matches!(read_container(&gone), Err(StoreError::Io { .. })));
}

#[test]
fn a_folder_that_does_not_exist_is_an_error_and_leaves_nothing() {
    let dir = Dir::new("no-folder");
    let path = dir.join("missing").join("a.citar");
    let err = write_container(&path, &common::header(None), &Body::small().parts())
        .expect_err("no folder");
    assert!(matches!(err, StoreError::Io { .. }), "{err}");
    assert!(dir.names().is_empty());
}

/// Windows: a save another program holds open (OneDrive, a virus scanner) refuses the rename
/// until it lets go; the write waits for it, 250 ms at a time.
#[cfg(windows)]
#[test]
fn a_save_another_program_holds_is_written_when_it_lets_go() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    let dir = Dir::new("held");
    let path = dir.join("autosave.citar");
    let b = Body::small();
    write_container(&path, &common::header(None), &b.parts()).expect("writes");
    // Opened without FILE_SHARE_DELETE, as a scanner does: a rename over it is refused.
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).expect("opens");
    let t = Instant::now();
    let letting_go = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        drop(held);
    });
    let mut h = common::header(None);
    h.saved_at = "after the scanner".into();
    write_container(&path, &h, &b.parts()).expect("written once it is let go");
    assert!(t.elapsed() >= Duration::from_millis(500), "it waited: {:?}", t.elapsed());
    letting_go.join().expect("the holder ends");
    assert_eq!(read_header(&path).expect("reads").saved_at, "after the scanner");
    assert_eq!(dir.names(), ["autosave.citar"]);
}

/// Windows: a save held past the retries is an error, and the old save and no temporary file are
/// left.
#[cfg(windows)]
#[test]
fn a_save_held_past_the_retries_is_an_error_and_the_old_one_stays() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    let dir = Dir::new("held-long");
    let path = dir.join("autosave.citar");
    let b = Body::small();
    write_container(&path, &common::header(None), &b.parts()).expect("writes");
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).expect("opens");
    let t = Instant::now();
    let mut h = common::header(None);
    h.saved_at = "never".into();
    let err = write_container(&path, &h, &b.parts()).expect_err("refused");
    let took = t.elapsed();
    assert!(
        matches!(&err, StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied),
        "{err}"
    );
    assert!(took >= Duration::from_millis(1900), "eight retries 250 ms apart: {took:?}");
    drop(held);
    assert_eq!(dir.names(), ["autosave.citar"], "the temporary file is removed");
    assert_eq!(read_header(&path).expect("reads").saved_at, common::header(None).saved_at);
}
