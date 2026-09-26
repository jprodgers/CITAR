//! Python's reading of JSON values: truth, `repr`, `str`, `int()` and `float()`.
//!
//! Hosts, tools and scenario operations hand the engine JSON that Python used to read with its
//! own built-ins: `tools.execute` coerced tool arguments with `int()` (`tools.py:113-127`), and
//! the scenario operations read theirs with `int()`, `float()` and truth tests
//! (`scenario.py:36-469`). Models and editors lean on that leniency, sending `"3"` for 3, so the
//! engine reads such values exactly as Python did, and quotes them back as Python would.
//!
//! What differs, on purpose: an integer beyond `i64` is refused where Python made a big integer
//! of it (refcheck: normalize-refuses-big-ints-and-non-objects, for tool arguments), and
//! `float()` of `inf` or `nan` is left to the caller to refuse, since no game value may be
//! infinite (invariant PLAYER-1).

use serde_json::Value;

use super::fmt::PyFloat;
use super::text::{echo, is_space};

/// Python's truth value of a JSON value: null, false, zero and empty are false.
#[must_use]
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `repr` of a JSON value, for messages that quote a bad setting back: `'deity'`,
/// `None`, `[1, 'a']`.
#[must_use]
pub fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => str_of(other),
    }
}

/// About how many characters of a caller's value a refusal quotes back with [`repr_echo`].
pub const ECHO_REPR_CHARS: usize = 100;

/// Python's `repr` of a caller's value as a refusal quotes it back ("Malformed deal item
/// {'type': 'gold', 'amount': 'lots'}."): each text in it, keys included, cut as
/// [`echo`] cuts one, and once the quote has run to [`ECHO_REPR_CHARS`] characters, the rest of
/// each list or object it is in given as `...` and the brackets closed. The quote stays within
/// about 340 characters whatever was sent (property P5, DESIGN.md 8.5), and its quotes and
/// brackets stay balanced, where cutting the whole `repr` would have left them open.
// refcheck: refusals-quote-at-most-60-characters
#[must_use]
pub fn repr_echo(v: &Value) -> String {
    let mut out = String::new();
    write_repr_echo(v, &mut out);
    out
}

/// Writes `v` for [`repr_echo`]; whether the quote was cut, after which the lists and objects
/// around it only close.
fn write_repr_echo(v: &Value, out: &mut String) -> bool {
    // Checked before each key and each value, so what runs past the mark is one text, about 130
    // characters with every character escaped, then `...` and a bracket for each list or object
    // open, at most one per character before the mark.
    let full = |out: &mut String| {
        let cut = out.chars().count() >= ECHO_REPR_CHARS;
        if cut {
            out.push_str("...");
        }
        cut
    };
    match v {
        Value::String(s) => {
            out.push_str(&repr_str(&echo(s)));
            false
        }
        Value::Array(a) => {
            out.push('[');
            let mut cut = false;
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                cut = full(out) || write_repr_echo(x, out);
                if cut {
                    break;
                }
            }
            out.push(']');
            cut
        }
        Value::Object(o) => {
            out.push('{');
            let mut cut = false;
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if full(out) {
                    cut = true;
                    break;
                }
                out.push_str(&repr_str(&echo(k)));
                out.push_str(": ");
                cut = full(out) || write_repr_echo(x, out);
                if cut {
                    break;
                }
            }
            out.push('}');
            cut
        }
        other => {
            out.push_str(&str_of(other));
            false
        }
    }
}

/// Python's `str` of a JSON value: a string as it is, anything else as its `repr`, with
/// `None`, `True` and `False` for null and the booleans.
#[must_use]
pub fn str_of(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (None, None, Some(f)) => PyFloat(f).to_string(),
            (None, None, None) => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => repr(v),
    }
}

/// A string as Python's `repr` quotes it.
fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `int(v)` of a JSON value: a boolean is 0 or 1, a float is truncated toward zero, a
/// string must be an integer literal ([`int_of_str`]); anything else, and an integer outside
/// `i64`, is `None`.
#[must_use]
pub fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Some(i);
            }
            if n.is_u64() {
                return None;
            }
            let f = n.as_f64()?.trunc();
            // `i64::MAX as f64` rounds up to 2^63, which is out of range, so the upper bound is
            // exclusive; -2^63 is exact.
            #[allow(clippy::cast_precision_loss, reason = "the bounds are powers of two")]
            let (lo, hi) = (i64::MIN as f64, i64::MAX as f64);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a whole float inside the i64 range converts exactly"
            )]
            (f.is_finite() && f >= lo && f < hi).then_some(f as i64)
        }
        Value::String(s) => int_of_str(s),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Python's `int(s)` of a string, in base 10 (`PyLong_FromUnicodeObject`): whitespace around the
/// number, an optional sign, and decimal digits of any script, with single underscores between
/// digits. `" 4 "`, `"+5"`, `"1_000"` and `"٣"` are integers; `"3.0"`, `"- 5"`, `"0x10"` and
/// `"_7"` are not.
///
/// Python first turns every non-ASCII space into a space and every non-ASCII decimal digit into
/// its ASCII digit, then skips the ASCII spaces `\t\n\v\f\r` and ` ` around the number, so the
/// separators U+001C to U+001F, which `str.isspace` counts, are not skipped here.
#[must_use]
pub fn int_of_str(s: &str) -> Option<i64> {
    let mut ascii = String::with_capacity(s.len());
    for c in s.chars() {
        if u32::from(c) < 127 {
            ascii.push(c);
        } else if is_space(c) {
            ascii.push(' ');
        } else {
            ascii.push(char::from(b'0' + decimal_digit(c)?));
        }
    }
    let t = ascii.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let (negative, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut value: i64 = 0;
    let mut last_digit = false;
    for b in digits.bytes() {
        match b {
            b'0'..=b'9' => {
                value = value.checked_mul(10)?.checked_sub(i64::from(b - b'0'))?;
                last_digit = true;
            }
            b'_' if last_digit => last_digit = false,
            _ => return None,
        }
    }
    // Empty, or ending in an underscore.
    if !last_digit {
        return None;
    }
    // Accumulated negatively, so that i64::MIN parses.
    if negative { Some(value) } else { value.checked_neg() }
}

/// Python's `float(v)` of a JSON value: a number as it is, a boolean as 0 or 1, a string as a
/// decimal literal or `inf`, `infinity` or `nan` in any case with an optional sign, underscores
/// allowed between digits; anything else is `None`. The result may be infinite or NaN, as
/// Python's was.
#[must_use]
pub fn float_of(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => float_of_str(s),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Python's `float(s)` of a string (`PyFloat_FromString`).
#[must_use]
pub fn float_of_str(s: &str) -> Option<f64> {
    let mut ascii = String::with_capacity(s.len());
    for c in s.chars() {
        if u32::from(c) < 127 {
            ascii.push(c);
        } else if is_space(c) {
            ascii.push(' ');
        } else {
            ascii.push(char::from(b'0' + decimal_digit(c)?));
        }
    }
    let t = ascii.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    let lower = unsigned.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        let x = if lower == "nan" { f64::NAN } else { f64::INFINITY };
        return Some(if t.starts_with('-') { -x } else { x });
    }
    // A decimal literal: digits, an optional point and more digits, at least one digit in all,
    // then an optional exponent; underscores only between two digits.
    let bytes = unsigned.as_bytes();
    let digit_at = |j: Option<usize>| j.and_then(|j| bytes.get(j)).is_some_and(u8::is_ascii_digit);
    let mut plain = String::with_capacity(t.len());
    if t.starts_with('-') {
        plain.push('-');
    }
    let (mut mantissa_digits, mut point, mut exponent, mut exponent_digits) = (0, false, false, 0);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'_' if digit_at(i.checked_sub(1)) && digit_at(Some(i + 1)) => continue,
            b'0'..=b'9' if exponent => exponent_digits += 1,
            b'0'..=b'9' => mantissa_digits += 1,
            b'.' if !point && !exponent => point = true,
            b'e' | b'E' if !exponent && mantissa_digits > 0 => exponent = true,
            b'+' | b'-' if i > 0 && matches!(bytes[i - 1], b'e' | b'E') => {}
            _ => return None,
        }
        plain.push(char::from(b));
    }
    if mantissa_digits == 0 || (exponent && exponent_digits == 0) {
        return None;
    }
    plain.parse::<f64>().ok()
}

/// The first code point of each run of ten decimal digits Unicode 16.0 has (general category
/// `Nd`), as Python 3.14's `unicodedata.decimal` gives them: every run is contiguous from 0 to 9.
const DIGIT_ZEROS: [u32; 76] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

/// The value of a decimal digit of any script, as Python's `int()` reads it.
fn decimal_digit(c: char) -> Option<u8> {
    let cp = u32::from(c);
    let i = DIGIT_ZEROS.partition_point(|&z| z <= cp).checked_sub(1)?;
    let d = cp - DIGIT_ZEROS[i];
    u8::try_from(d).ok().filter(|&d| d < 10)
}

/// Python's `str.strip()`: whitespace as `str.isspace` counts it, from both ends.
#[must_use]
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    #[test]
    fn int_reads_strings_as_python_does() {
        let ok = [
            (" 4 ", 4),
            ("\u{2003}4\u{2003}", 4),
            ("\u{663}", 3),
            ("1_000", 1000),
            ("+5", 5),
            ("\u{ff13}", 3),
            ("007", 7),
            ("0_7", 7),
            ("\x0b4", 4),
            ("\u{85}4", 4),
            ("4\n", 4),
            ("\u{a0}4", 4),
            ("\u{663}_\u{663}", 33),
            ("-9223372036854775808", i64::MIN),
        ];
        for (s, n) in ok {
            assert_eq!(int_of_str(s), Some(n), "{s:?}");
        }
        let bad = [
            "\x1c4",
            "4\x1c",
            "- 5",
            "0x10",
            "_7",
            "7_",
            "3.0",
            "",
            "  ",
            "1__0",
            "4\0",
            "1e3",
            "9223372036854775808",
        ];
        for s in bad {
            assert_eq!(int_of_str(s), None, "{s:?}");
        }
    }

    #[test]
    fn int_of_other_values() {
        assert_eq!(int_of(&json!(true)), Some(1));
        assert_eq!(int_of(&json!(3.7)), Some(3));
        assert_eq!(int_of(&json!(-3.7)), Some(-3));
        assert_eq!(int_of(&json!(1e20)), None, "beyond i64: refused");
        assert_eq!(int_of(&json!(u64::MAX)), None);
        assert_eq!(int_of(&json!(null)), None);
        assert_eq!(int_of(&json!([1])), None);
    }

    #[test]
    fn float_reads_strings_as_python_does() {
        assert_eq!(float_of_str(" 2.5 "), Some(2.5));
        assert_eq!(float_of_str("1_0.5"), Some(10.5));
        assert_eq!(float_of_str("1e3"), Some(1000.0));
        assert_eq!(float_of_str("-.5"), Some(-0.5));
        assert!(float_of_str("-Infinity").is_some_and(|x| x.is_infinite() && x < 0.0));
        assert!(float_of_str("nan").is_some_and(f64::is_nan));
        assert_eq!(float_of_str("1__0"), None);
        assert_eq!(float_of_str("_1"), None);
        assert_eq!(float_of_str("abc"), None);
        assert_eq!(float_of_str("."), None);
        assert_eq!(float_of_str("+-1"), None);
        assert_eq!(float_of_str("1e"), None);
        assert_eq!(float_of_str("1e-2"), Some(0.01));
        assert_eq!(float_of_str("1."), Some(1.0));
        assert_eq!(float_of(&json!(false)), Some(0.0));
    }

    #[test]
    fn repr_and_str_quote_like_python() {
        assert_eq!(repr(&json!("deity")), "'deity'");
        assert_eq!(repr(&json!("it's")), "\"it's\"");
        assert_eq!(repr(&json!(5)), "5");
        assert_eq!(repr(&json!(2.5)), "2.5");
        assert_eq!(repr(&json!(false)), "False");
        assert_eq!(repr(&json!(["a", 1])), "['a', 1]");
        assert_eq!(repr(&json!({"a": null})), "{'a': None}");
        assert_eq!(str_of(&json!("x")), "x");
        assert_eq!(str_of(&json!(null)), "None");
        assert_eq!(str_of(&json!(3.0)), "3.0");
    }

    #[test]
    fn a_quoted_value_is_cut_and_closed() {
        // Short values read as Python's repr.
        for v in [json!("deity"), json!({"type": "gold", "amount": "lots"}), json!([1, null])] {
            assert_eq!(repr_echo(&v), repr(&v));
        }
        let word = "Zanzibar ".repeat(200);
        assert_eq!(repr_echo(&json!(word)), format!("'{}...'", &word[..60]));
        let item = json!({"type": "gold_per_turn", "amount": 9_223_372_036_854_775_807_i64,
            "turns": 30, "resource": "Iron", "tech": "Writing", "city_id": 12, "target": 3});
        assert_eq!(
            repr_echo(&item),
            "{'type': 'gold_per_turn', 'amount': 9223372036854775807, 'turns': 30, 'resource': \
             'Iron', 'tech': 'Writing', ...}"
        );
        // However it is built, the quote stays short and balanced.
        let slashes = "\\".repeat(500);
        let mut deep = json!(word);
        for _ in 0..120 {
            deep = json!([deep, {slashes.clone(): slashes.clone()}]);
        }
        let wide: Map<String, Value> =
            (0..50).map(|i| (format!("{i}{slashes}"), json!(slashes))).collect();
        let mut keyed = json!({slashes.clone(): slashes.clone()});
        for _ in 0..15 {
            keyed = json!({"a": keyed});
        }
        for v in [deep, Value::Object(wide), keyed, json!([word, word, word, [[[[word]]]]])] {
            let q = repr_echo(&v);
            assert!(q.chars().count() <= 340, "{} characters: {q}", q.chars().count());
            let opened = q.matches(['[', '{']).count();
            assert_eq!(opened, q.matches([']', '}']).count(), "{q}");
            assert!(q.ends_with([']', '}']), "{q}");
        }
    }

    #[test]
    fn truth_and_strip() {
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && !truthy(&json!({})));
        assert!(truthy(&json!("false")) && truthy(&json!(0.5)));
        assert_eq!(strip("\x1c a \u{2003}"), "a");
    }

    #[test]
    fn digits_of_every_script() {
        assert_eq!(decimal_digit('7'), Some(7));
        assert_eq!(decimal_digit('\u{1d7d8}'), Some(0));
        assert_eq!(decimal_digit('\u{116d9}'), Some(9));
        assert_eq!(decimal_digit('\u{116da}'), Some(0));
        assert_eq!(decimal_digit('a'), None);
        assert_eq!(decimal_digit('\u{2f}'), None);
    }
}
