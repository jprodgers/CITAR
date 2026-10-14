//! The edges of the protocol's checks, the same cases `tests/test_helper_protocol.py` runs on the
//! server's side: numbers read as serde_json reads them, the size and depth caps, the hello's
//! protocol check first, the v1 refusal's words, and frames written by `encode` reading back.

use citar_helper::conformance::{canonical_examples, crate_dir};
use citar_helper::protocol::{
    Cancel, ErrorCode, FrameSet, HelperFrame, Parsed, Ping, ServerFrame, V1_REFUSAL_MESSAGE,
    parse_helper_frame, parse_server_frame,
};
use serde_json::Value;

fn code(text: &str) -> Option<(ErrorCode, String)> {
    parse_helper_frame(text).err().map(|e| (e.code, e.path))
}

#[test]
fn numbers_read_as_serde_json_reads_them() {
    let status = |n: &str| {
        format!(
            r#"{{"t": "status", "quiet_until": null, "llm_in_flight": {n}, "jobs_running": 0, "jobs_paused": 0}}"#
        )
    };
    let at = |c| Some((c, "llm_in_flight".to_owned()));
    assert_eq!(code(&status("3")), None);
    assert_eq!(code(&status("-0")), at(ErrorCode::Type));
    assert_eq!(code(&status("3.0")), at(ErrorCode::Type));
    assert_eq!(code(&status("1e1")), at(ErrorCode::Type));
    assert_eq!(code(&status("18446744073709551616")), at(ErrorCode::Type));
    assert_eq!(code(&status("18446744073709551615")), at(ErrorCode::Value));
    assert_eq!(code(&status("-9223372036854775809")), at(ErrorCode::Type));
    assert_eq!(code(&status(&"9".repeat(400))), Some((ErrorCode::NotJson, String::new())));
    assert_eq!(code(&status("Infinity")), Some((ErrorCode::NotJson, String::new())));
    assert_eq!(code(&status("true")), at(ErrorCode::Type));
}

#[test]
fn the_caps_on_size_and_depth() {
    let ping = |x: &str| format!(r#"{{"t": "ping", "x": {x}}}"#);
    let ok = ping(&format!("{}{}", "[".repeat(63), "]".repeat(63)));
    assert!(matches!(
        parse_helper_frame(&ok),
        Ok(Parsed::Frame { frame: HelperFrame::Ping(_), .. })
    ));
    let deep = ping(&format!("{}{}", "[".repeat(64), "]".repeat(64)));
    assert_eq!(code(&deep).map(|c| c.0), Some(ErrorCode::TooDeep));
    assert_eq!(code(&ping(&"[".repeat(70))).map(|c| c.0), Some(ErrorCode::TooDeep));
    let deeper = ping(&format!("{}{}", "[".repeat(100_000), "]".repeat(100_000)));
    assert_eq!(code(&deeper).map(|c| c.0), Some(ErrorCode::TooDeep));
    let in_string = ping(&Value::from("[".repeat(100)).to_string());
    assert!(parse_helper_frame(&in_string).is_ok());
    let big = format!(r#"{{"t": "ping", "x": "{}"}}"#, "\u{1F600}".repeat(2_100_000));
    assert_eq!(code(&big).map(|c| c.0), Some(ErrorCode::TooLarge));
}

#[test]
fn a_hello_that_is_not_protocol_2_is_refused_before_its_other_fields() {
    for hello in [
        r#"{"t": "hello"}"#,
        r#"{"t": "hello", "protocol": 1, "token": 5}"#,
        r#"{"t": "hello", "protocol": 2.0}"#,
        r#"{"t": "hello", "protocol": true}"#,
    ] {
        assert_eq!(code(hello), Some((ErrorCode::Protocol, "protocol".to_owned())), "{hello}");
    }
    let welcome = parse_server_frame(r#"{"t": "welcome", "protocol": 1}"#).unwrap_err();
    assert_eq!((welcome.code, welcome.path.as_str()), (ErrorCode::Value, "protocol"));
}

#[test]
fn the_v1_refusal_is_the_one_protocol_md_and_the_fixture_give() {
    let md = std::fs::read_to_string(crate_dir().join("PROTOCOL.md")).expect("PROTOCOL.md reads");
    let start = md.find(r#"{"t": "error", "ts": "#).expect("PROTOCOL.md quotes the refusal");
    let line = md[start..].lines().next().unwrap_or_default();
    let quoted: Value = serde_json::from_str(line).expect("the quoted refusal is JSON");
    assert_eq!(quoted["message"], V1_REFUSAL_MESSAGE);
    let ex = canonical_examples().expect("the valid fixtures read");
    let fixture = ex
        .get(&(citar_helper::protocol::Sender::Server, "error".to_owned()))
        .expect("server.error.json");
    assert_eq!(fixture["message"], V1_REFUSAL_MESSAGE);
}

#[test]
fn what_encode_writes_reads_back_with_t_and_ts_first() {
    let frame = ServerFrame::Cancel(Cancel { id: "0123456789abcdef".to_owned() });
    let text = frame.encode(1_760_000_000.5);
    assert!(text.starts_with(r#"{"t":"cancel","ts":1760000000.5,"id":"#), "{text}");
    match parse_server_frame(&text) {
        Ok(Parsed::Frame { ts, frame: back }) => {
            assert_eq!(ts, Some(1_760_000_000.5));
            assert_eq!(back, frame);
        }
        other => panic!("{other:?}"),
    }
    let ping = HelperFrame::Ping(Ping {});
    assert_eq!(ping.to_value(None).to_string(), r#"{"t":"ping"}"#);
    assert_eq!(ping.type_name(), "ping");
}
