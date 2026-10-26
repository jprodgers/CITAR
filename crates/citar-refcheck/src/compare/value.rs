//! Numbers and values as refcheck compares them (DESIGN.md 9.2, "Comparison").
//!
//! Integers are exact. Any other pair of numbers is equal within a relative tolerance, because
//! Python sums in its own order and so the last bits of a float differ. 3 and 3.0 are equal:
//! Python writes `3.0` for a float that happens to be whole, and Rust may compute it as an integer.

use serde_json::{Number, Value};

/// The relative tolerance for numbers that are not both integers:
/// `|a - b| <= TOLERANCE * max(1, |a|, |b|)`.
pub const TOLERANCE: f64 = 1e-6;

/// Integers are 2^53 or less in every recorded answer, but a Python int can be larger, and one
/// past 2^53 must not compare equal to its neighbour through f64.
fn int_of(n: &Number) -> Option<i128> {
    n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from))
}

/// Every number serde_json parses is finite (JSON has no NaN), so this never fails in practice;
/// the fallback keeps the comparison total.
fn float_of(n: &Number) -> f64 {
    n.as_f64().unwrap_or(f64::NAN)
}

/// The comparison refcheck applies to two numbers: exact for two integers, within [`TOLERANCE`]
/// otherwise.
pub fn numbers_equal(a: &Number, b: &Number) -> bool {
    if let (Some(x), Some(y)) = (int_of(a), int_of(b)) {
        return x == y;
    }
    floats_equal(float_of(a), float_of(b))
}

/// The tolerance applied to two floats.
pub fn floats_equal(x: f64, y: f64) -> bool {
    (x - y).abs() <= TOLERANCE * 1f64.max(x.abs()).max(y.abs())
}

/// Exact numeric equality, used by selectors such as `[pid=0]`: 0 and 0.0 are the same number,
/// but 0.1 and 0.1000001 are not.
pub fn same_number(a: &Number, b: &Number) -> bool {
    number_text(a) == number_text(b)
}

/// A number's canonical text: integers, and floats with a whole value below 2^53, as integers
/// (so `3.0` is `3` and `-0.0` is `0`); every other float in Rust's shortest round-trip form.
pub fn number_text(n: &Number) -> String {
    if let Some(i) = int_of(n) {
        return i.to_string();
    }
    let f = float_of(n);
    // 2^53: every whole float below it is exactly an i64.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    #[allow(clippy::float_cmp, reason = "an exact test for a whole number is what is meant")]
    let whole = f.trunc() == f;
    if whole && f.abs() < EXACT {
        // Whole and below 2^53, so the cast is exact.
        return (f as i64).to_string();
    }
    format!("{f:?}")
}

/// A value's canonical text: object keys sorted, numbers as [`number_text`]. Two values with the
/// same canonical text are equal without any tolerance; multisets and keyed lists match on it.
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_text(n)),
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_canonical(&map[k.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// A string as a JSON literal.
pub fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A string as a JSON literal, returned.
pub fn json_string(s: &str) -> String {
    let mut out = String::new();
    write_json_string(s, &mut out);
    out
}

/// Deep equality under refcheck's number rule, with arrays in order and objects as key sets.
/// Used for value constraints, which name a value rather than a place in an answer.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => numbers_equal(x, y),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| values_equal(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| values_equal(p, q)))
        }
        _ => false,
    }
}

/// A value on one line, cut to about `max` characters, for the human report.
pub fn short(v: &Value, max: usize) -> String {
    let text = v.to_string();
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn num(v: Value) -> Number {
        match v {
            Value::Number(n) => n,
            other => panic!("not a number: {other}"),
        }
    }

    #[test]
    fn integers_are_exact() {
        assert!(numbers_equal(&num(json!(7)), &num(json!(7))));
        assert!(!numbers_equal(&num(json!(1_000_000_000)), &num(json!(1_000_000_001))));
        // One past 2^53: equal as f64, different as integers.
        assert!(!numbers_equal(
            &num(json!(9_007_199_254_740_993_u64)),
            &num(json!(9_007_199_254_740_992_u64))
        ));
    }

    #[test]
    fn other_numbers_use_the_relative_tolerance() {
        assert!(numbers_equal(&num(json!(3)), &num(json!(3.0))));
        assert!(numbers_equal(&num(json!(0.1 + 0.2)), &num(json!(0.3))));
        // Below 1 the tolerance is absolute: 1e-6.
        assert!(numbers_equal(&num(json!(0.0)), &num(json!(0.000_000_9))));
        assert!(!numbers_equal(&num(json!(0.0)), &num(json!(0.000_001_1))));
        // Above 1 it scales with the larger magnitude.
        assert!(numbers_equal(&num(json!(1_000_000.0)), &num(json!(1_000_000.9))));
        assert!(!numbers_equal(&num(json!(1_000_000.0)), &num(json!(1_000_001.1))));
        assert!(!numbers_equal(&num(json!(2.5)), &num(json!(3.0))));
        assert!(numbers_equal(&num(json!(-0.0)), &num(json!(0))));
    }

    #[test]
    fn canonical_text_ignores_key_order_and_float_spelling() {
        assert_eq!(canonical(&json!({"b": 1.0, "a": [2, "x"]})), r#"{"a":[2,"x"],"b":1}"#);
        assert_eq!(canonical(&json!({"a": [2.0, "x"], "b": 1})), r#"{"a":[2,"x"],"b":1}"#);
        assert_eq!(canonical(&json!(-0.0)), "0");
        assert_eq!(canonical(&json!(2.5)), "2.5");
        assert_eq!(canonical(&json!("a\"b\n")), r#""a\"b\n""#);
        assert!(same_number(&num(json!(4)), &num(json!(4.0))));
        assert!(!same_number(&num(json!(0.1)), &num(json!(0.100_000_01))));
    }

    #[test]
    fn deep_equality_uses_the_number_rule() {
        assert!(values_equal(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
        assert!(!values_equal(&json!({"a": [1, 2]}), &json!({"a": [2, 1]})));
        assert!(!values_equal(&json!({"a": null}), &json!({})));
        assert!(!values_equal(&json!(1), &json!(true)));
    }

    #[test]
    fn short_cuts_long_values() {
        assert_eq!(short(&json!("abc"), 10), r#""abc""#);
        assert_eq!(short(&json!([1, 2, 3, 4, 5, 6]), 6), "[1,2,…");
    }
}
