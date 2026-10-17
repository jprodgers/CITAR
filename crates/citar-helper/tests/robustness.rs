//! Package 4-01, gate 3: random mutations of the valid frames (keys dropped, types swapped, deep
//! nesting, huge numbers, NaN-like strings, the text cut or spliced) never panic the parser, in
//! either direction, and whatever it accepts reads back as a frame of the type it claims.
//! `tests/test_helper_protocol.py` mutates the same frames on the server's side.

use citar_helper::conformance::{FixtureKind, frame_fixtures};
use citar_helper::protocol::{FrameSet, Parsed, parse_helper_frame, parse_server_frame};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

/// The valid fixtures' frames, from both senders.
fn frames() -> Vec<(bool, Value)> {
    frame_fixtures("valid")
        .expect("the valid fixtures read")
        .into_iter()
        .filter_map(|f| match f.kind {
            FixtureKind::Valid { frame, .. } => {
                Some((f.from == citar_helper::protocol::Sender::Helper, frame))
            }
            _ => None,
        })
        .collect()
}

/// Every node of a value, as paths of keys and indexes.
fn nodes(v: &Value, here: &mut Vec<Value>, out: &mut Vec<Vec<Value>>) {
    out.push(here.clone());
    match v {
        Value::Object(map) => {
            for (k, x) in map {
                here.push(Value::from(k.as_str()));
                nodes(x, here, out);
                here.pop();
            }
        }
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                here.push(Value::from(i));
                nodes(x, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

fn at<'v>(v: &'v mut Value, path: &[Value]) -> &'v mut Value {
    let mut cur = v;
    for step in path {
        cur = match step {
            Value::String(k) => &mut cur[k.as_str()],
            Value::Number(i) => &mut cur[usize::try_from(i.as_u64().unwrap_or(0)).unwrap_or(0)],
            _ => cur,
        };
    }
    cur
}

/// Values a mutation puts in place of another: every JSON type, numbers at and past the edges,
/// strings that look like numbers.
fn replacement(pick: u32) -> Value {
    let all = [
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(u64::MAX),
        json!(i64::MIN),
        json!(9_007_199_254_740_993_u64),
        json!(1e308),
        json!(-1e308),
        json!("NaN"),
        json!("Infinity"),
        json!("-Infinity"),
        json!("1e400"),
        json!("-0"),
        json!(""),
        json!("\u{0}"),
        json!("x".repeat(5000)),
        json!([]),
        json!({}),
        json!([[[[[[]]]]]]),
        json!({"t": "hello"}),
    ];
    all[pick as usize % all.len()].clone()
}

/// Text that cannot stand as a JSON value, spliced into a number's place.
const BAD_NUMBERS: &[&str] =
    &["NaN", "1e400", "-Infinity", "00", "1.", "-", "+1", "0x10", "1e", "--1"];

fn mutate(frame: &Value, ops: &[(u8, u32, u32)]) -> String {
    let mut v = frame.clone();
    let mut splice: Vec<(usize, String)> = Vec::new();
    let mut cut = None;
    for &(op, a, b) in ops {
        let mut paths = Vec::new();
        nodes(&v, &mut Vec::new(), &mut paths);
        let path = paths[a as usize % paths.len()].clone();
        match op % 8 {
            0 => {
                if let Value::Object(map) = at(&mut v, &path) {
                    let keys: Vec<String> = map.keys().cloned().collect();
                    if !keys.is_empty() {
                        map.shift_remove(&keys[b as usize % keys.len()]);
                    }
                }
            }
            1 | 2 => *at(&mut v, &path) = replacement(b),
            3 => {
                let node = at(&mut v, &path);
                let mut deep = node.take();
                for _ in 0..(b % 140) {
                    deep = Value::Array(vec![deep]);
                }
                *node = deep;
            }
            4 => {
                if let Value::Object(map) = at(&mut v, &path) {
                    let mut renamed = Map::new();
                    for (k, x) in std::mem::take(map) {
                        renamed
                            .insert(if b % 2 == 0 { k.to_uppercase() } else { format!("{k}_") }, x);
                    }
                    *map = renamed;
                }
            }
            5 => {
                let marker = format!("\"@@{}@@\"", splice.len());
                *at(&mut v, &path) = Value::from(&marker[1..marker.len() - 1]);
                splice.push((splice.len(), BAD_NUMBERS[b as usize % BAD_NUMBERS.len()].to_owned()));
            }
            6 => cut = Some(b),
            _ => {
                if let Value::Object(map) = at(&mut v, &path) {
                    map.insert("t".to_owned(), replacement(b));
                }
            }
        }
    }
    let mut text = v.to_string();
    for (i, bad) in splice {
        text = text.replace(&format!("\"@@{i}@@\""), &bad);
    }
    if let Some(c) = cut {
        let mut at = c as usize % (text.len() + 1);
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
    text
}

fn read<F: FrameSet>(r: Result<Parsed<F>, citar_helper::protocol::ProtocolError>) {
    // What is accepted reads back as JSON of its own type.
    if let Ok(Parsed::Frame { ts, frame }) = r {
        let back = frame.to_value(ts);
        assert_eq!(back.get("t").and_then(Value::as_str), Some(frame.type_name()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn mutated_frames_never_panic_the_parser(
        pick in any::<u32>(),
        ops in proptest::collection::vec((any::<u8>(), any::<u32>(), any::<u32>()), 1..5),
    ) {
        let all = frames();
        let (_, frame) = &all[pick as usize % all.len()];
        let text = mutate(frame, &ops);
        read(parse_helper_frame(&text));
        read(parse_server_frame(&text));
    }
}

/// The mutations reach every refusal at least once over a fixed sweep, so the property above
/// is not passing on frames that are all still valid.
#[test]
fn the_mutations_reach_every_kind_of_refusal() {
    use citar_helper::protocol::ErrorCode;
    use std::collections::BTreeSet;
    let all = frames();
    let mut seen = BTreeSet::new();
    let mut accepted = 0;
    for i in 0u32..4000 {
        let (_, frame) = &all[i as usize % all.len()];
        let op = (i % 8) as u8;
        let ops = [(op, i.wrapping_mul(2_654_435_761), i.wrapping_mul(40_503))];
        let text = mutate(frame, &ops);
        for r in [parse_helper_frame(&text).map(|_| ()), parse_server_frame(&text).map(|_| ())] {
            match r {
                Ok(()) => accepted += 1,
                Err(e) => {
                    seen.insert(e.code);
                }
            }
        }
    }
    for code in [
        ErrorCode::TooDeep,
        ErrorCode::NotJson,
        ErrorCode::NoType,
        ErrorCode::Protocol,
        ErrorCode::Missing,
        ErrorCode::Type,
        ErrorCode::Value,
    ] {
        assert!(seen.contains(&code), "no mutation was refused {code}: {seen:?}");
    }
    assert!(accepted > 0);
}
