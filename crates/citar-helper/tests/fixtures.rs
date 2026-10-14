//! The shared fixtures of protocol 2 from the helper's side (package 4-01, gates 1, 2 and 4):
//! every valid frame parses and serializes back to its expectation, every invalid frame is
//! refused with its fixture's code and path, every ignored frame is ignored, PROTOCOL.md's frame
//! table is exactly the types this side knows, and every transcript loads, with every frame in it
//! realizable into a frame that passes its checks. `tests/test_helper_protocol.py` runs the same
//! files from the server's side.

use std::collections::BTreeSet;

use citar_helper::conformance::matcher::{Bindings, NoGolden, json_eq, realize};
use citar_helper::conformance::{
    FixtureKind, canonical_examples, crate_dir, frame_fixtures, frame_table, transcripts,
};
use citar_helper::protocol::{
    ErrorCode, FrameSet, HelperFrame, Parsed, ProtocolError, Sender, ServerFrame,
    parse_helper_frame, parse_server_frame,
};
use serde_json::Value;

/// A frame from `from`, parsed and serialized again; or why it was refused; or the type ignored.
fn parse_from(from: Sender, text: &str) -> Result<Result<Value, String>, ProtocolError> {
    fn back<F: FrameSet>(p: Parsed<F>) -> Result<Value, String> {
        match p {
            Parsed::Frame { ts, frame } => Ok(frame.to_value(ts)),
            Parsed::Ignored { t } => Err(t),
        }
    }
    match from {
        Sender::Helper => parse_helper_frame(text).map(back),
        Sender::Server => parse_server_frame(text).map(back),
    }
}

#[test]
fn every_valid_frame_parses_and_serializes_back_to_its_expectation() {
    let fixtures = frame_fixtures("valid").expect("the valid fixtures read");
    assert!(fixtures.len() >= 40, "{} valid fixtures", fixtures.len());
    for f in &fixtures {
        let FixtureKind::Valid { expect, .. } = &f.kind else { unreachable!() };
        let back = parse_from(f.from, &f.text())
            .unwrap_or_else(|e| panic!("{}: refused: {e}", f.name))
            .unwrap_or_else(|t| panic!("{}: ignored as {t}", f.name));
        assert!(json_eq(&back, expect), "{}:\n  got    {back}\n  wanted {expect}", f.name);
    }
}

#[test]
fn every_invalid_frame_is_refused_with_its_code_and_path() {
    let fixtures = frame_fixtures("invalid").expect("the invalid fixtures read");
    assert!(fixtures.len() >= 60, "{} invalid fixtures", fixtures.len());
    let mut codes = BTreeSet::new();
    for f in &fixtures {
        let FixtureKind::Invalid { code, path, .. } = &f.kind else { unreachable!() };
        match parse_from(f.from, &f.text()) {
            Err(e) => {
                assert_eq!((e.code, e.path.as_str()), (*code, path.as_str()), "{}: {e}", f.name);
            }
            Ok(back) => panic!("{}: accepted as {back:?}, not refused {code} at {path:?}", f.name),
        }
        codes.insert(*code);
    }
    assert_eq!(codes, ErrorCode::ALL.into_iter().collect(), "every code has an invalid fixture");
}

#[test]
fn every_ignored_frame_is_ignored() {
    let fixtures = frame_fixtures("ignored").expect("the ignored fixtures read");
    assert!(!fixtures.is_empty());
    for f in &fixtures {
        match parse_from(f.from, &f.text()) {
            Ok(Err(_)) => {}
            other => panic!("{}: not ignored: {other:?}", f.name),
        }
    }
}

/// Each type each side sends has a canonical example, and each type with fields an invalid
/// fixture of its own.
#[test]
fn every_frame_type_has_a_canonical_example_and_its_own_invalid_fixture() {
    let canonical = canonical_examples().expect("the valid fixtures read");
    let invalid = frame_fixtures("invalid").expect("the invalid fixtures read");
    for from in [Sender::Helper, Sender::Server] {
        for t in from.types() {
            assert!(
                canonical.contains_key(&(from, (*t).to_owned())),
                "no {}.{t}.json",
                from.as_str()
            );
            let fieldless = ["ping", "pong", "rescan", "jobs_resume"].contains(t);
            let prefix = format!("{}.{t}.", from.as_str());
            if !fieldless {
                assert!(
                    invalid.iter().any(|f| f.name.starts_with(&prefix)),
                    "no invalid fixture {prefix}*.json"
                );
            }
        }
    }
}

/// Gate 2: the frame types this side knows are PROTOCOL.md's table, row for row.
#[test]
fn the_frame_table_is_exactly_the_types_this_side_knows() {
    let md = std::fs::read_to_string(crate_dir().join("PROTOCOL.md")).expect("PROTOCOL.md reads");
    let table: BTreeSet<(String, Sender)> =
        frame_table(&md).expect("the table reads").into_iter().collect();
    let known: BTreeSet<(String, Sender)> = [Sender::Helper, Sender::Server]
        .into_iter()
        .flat_map(|s| s.types().iter().map(move |t| ((*t).to_owned(), s)))
        .collect();
    let rows = frame_table(&md).expect("the table reads").len();
    assert_eq!(rows, table.len(), "a row appears twice in the frame table");
    assert_eq!(table, known, "PROTOCOL.md's frame table against HelperFrame and ServerFrame");
    assert_eq!(
        <HelperFrame as FrameSet>::TYPES.len() + <ServerFrame as FrameSet>::TYPES.len(),
        rows
    );
}

/// Gate 4: every transcript loads, and every frame in it, realized as the scripted side would
/// send it (from the canonical examples, the golden sets standing in by them until 4-07 commits
/// them), passes its checks and reads back unchanged, so no field in a pattern is one the
/// protocol does not have; a frame sent broken on purpose is refused with its code.
#[test]
fn every_transcript_loads_and_its_frames_realize_into_valid_frames() {
    let all = transcripts().expect("the transcripts load");
    assert!(all.len() >= 60, "{} transcripts", all.len());
    let canonical = canonical_examples().expect("the valid fixtures read");
    let mut frames = 0;
    for t in &all {
        let mut bindings = Bindings::default();
        for (i, from, pattern, invalid) in t.frames() {
            let ty = pattern.get("t").and_then(Value::as_str).unwrap_or_default().to_owned();
            let example = canonical.get(&(from, ty));
            let real = realize(pattern, &mut bindings, &t.values, &NoGolden, example)
                .unwrap_or_else(|e| panic!("{} step {i}: {e}", t.name));
            let got = parse_from(from, &real.to_string());
            match (invalid, got) {
                (None, Ok(Ok(back))) => {
                    assert!(
                        json_eq(&back, &real),
                        "{} step {i}: a field the protocol drops:\n  {real}\n  {back}",
                        t.name
                    );
                }
                (Some(code), Err(e)) => assert_eq!(e.code, code, "{} step {i}: {e}", t.name),
                (_, other) => {
                    panic!("{} step {i}: {real} gave {other:?}, invalid: {invalid:?}", t.name)
                }
            }
            frames += 1;
        }
    }
    assert!(frames > 300, "{frames} frames realized");
}

/// Every field table this side checks equals `protocol/v2/schema.json`, which the server's side
/// is held to as well: a bound written differently on one side fails here even where no fixture
/// reaches it. After a deliberate change, `CITAR_BLESS_SCHEMA=1` rewrites the file from this
/// side, and the server's test then shows where it differs.
#[test]
fn the_field_tables_are_the_committed_schema() {
    let path = citar_helper::conformance::fixtures_dir().join("schema.json");
    let ours = citar_helper::protocol::schema();
    if std::env::var_os("CITAR_BLESS_SCHEMA").is_some() {
        let text = serde_json::to_string_pretty(&ours).expect("the schema serializes");
        std::fs::write(&path, text + "\n").expect("schema.json is written");
    }
    let text = std::fs::read_to_string(&path).expect("protocol/v2/schema.json reads");
    let committed: Value = serde_json::from_str(&text).expect("schema.json is JSON");
    for (side, frames) in committed.as_object().expect("an object") {
        for (t, want) in frames.as_object().expect("an object per side") {
            let got = &ours[side.as_str()][t.as_str()];
            assert!(json_eq(got, want), "{side} {t}:\n  this side {got}\n  committed {want}");
        }
    }
    assert!(json_eq(&ours, &committed), "a frame type is missing on one side");
}

/// The vocabularies the transcript loader enforces are the ones PROTOCOL.md documents: the
/// background frames word for word, and every setup, endpoint and host key named there.
#[test]
fn the_loaders_vocabularies_are_protocol_md_s() {
    use citar_helper::conformance::transcript::{
        BACKGROUND, ENDPOINT_KEYS, HELPER_CONFIG, HELPER_SETUP, HOST_KEYS, HUB_MACHINE, HUB_SETUP,
        RESPOND_KEYS,
    };
    let md = std::fs::read_to_string(crate_dir().join("PROTOCOL.md")).expect("PROTOCOL.md reads");
    let paragraph = |start: &str| -> String {
        let at = md.find(start).unwrap_or_else(|| panic!("PROTOCOL.md has no {start}"));
        let rest = &md[at..];
        rest[..rest.find("\n\n").unwrap_or(rest.len())].to_owned()
    };
    let ticked = |line: &str| -> Vec<String> {
        line.split('`').skip(1).step_by(2).map(str::to_owned).collect()
    };
    let background = paragraph("**Background frames.**");
    let first = background.lines().next().unwrap_or_default();
    assert_eq!(ticked(first.split(" come ").next().unwrap_or_default()), BACKGROUND);
    for (start, keys) in [
        ("**Setup keys**", [HELPER_SETUP, HELPER_CONFIG, HUB_SETUP, HUB_MACHINE].concat()),
        ("**Endpoint steps**", [ENDPOINT_KEYS, RESPOND_KEYS].concat()),
        ("**Host steps**", HOST_KEYS.to_vec()),
    ] {
        let text = paragraph(start);
        for key in keys {
            assert!(
                text.contains(&format!("`{key}`")) || text.contains(&format!("\"{key}\"")),
                "{start} does not name {key}"
            );
        }
    }
}

/// The transcripts the design names for 4-01's scope are all there (P4.2.9, Appendix E 4-01).
#[test]
fn the_transcripts_cover_the_protocol() {
    let names: BTreeSet<String> =
        transcripts().expect("load").into_iter().map(|t| t.name).collect();
    for want in [
        "handshake-welcome",
        "handshake-refused-token",
        "handshake-refused-key",
        "handshake-refused-protocol",
        "handshake-refused-rate",
        "handshake-refused-build",
        "handshake-v015-server",
        "handshake-v1-refusal-v015",
        "settings-capped",
        "settings-pushed",
        "catalog-on-change",
        "hardware-rescan",
        "power-minute",
        "status-quiet-hours",
        "completion-streamed-tool-call",
        "completion-not-streamed",
        "completion-busy",
        "cancel-mid-stream",
        "cancel-after-done",
        "lost-connection-cancels",
        "job-offer-to-ack",
        "job-reject-build",
        "job-abort",
        "job-deadline",
        "job-reconnect-keep",
        "job-reconnect-drop",
        "job-reconnect-after-send",
        "job-duplicate-result",
        "job-lost-offer",
        "job-silent-run",
        "job-reoffer-backoff",
        "jobs-pause",
        "update-notify",
        "keepalive-ping-pong",
        "shutdown-revoked",
    ] {
        assert!(names.contains(want), "no transcript {want}");
    }
}
