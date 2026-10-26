//! The matcher language of the transcripts (PROTOCOL.md, "The matcher language"): a frame in a
//! transcript is a pattern, which the side under test's frames must match and the scripted side
//! realizes into a frame to send. `tests/helper_fixtures.py` is the server's copy.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use crate::protocol::check::{as_integer, is_hex};

/// A string that stands for a set of values.
#[derive(Clone, Debug, PartialEq)]
pub enum Matcher {
    /// Any value, `null` included.
    Any,
    Bool,
    /// An integer within the bounds given.
    Int {
        lo: Option<i64>,
        hi: Option<i64>,
    },
    /// A number within the bounds given.
    Number {
        lo: Option<f64>,
        hi: Option<f64>,
    },
    String,
    /// Exactly this many lowercase hex digits.
    Hex(usize),
    /// A non-empty canonical standard base64 string.
    Base64,
    /// Bound to a name the first time it is seen, equal to its binding after.
    Id(String),
    /// A value of a committed golden set: `SET/NAME` or `SET/NAME/FIELD…`.
    Golden {
        set: String,
        path: Vec<String>,
    },
}

/// The matcher kinds: a string `<kind>` or `<kind:arg>` of one of these is a matcher, and must be
/// well formed; any other string is a literal.
pub const KINDS: &[&str] = &[
    "any", "bool", "int", "number", "string", "hex12", "hex16", "hex64", "base64", "id", "golden",
];

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn path_part_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `LO..HI`, either end optional.
fn bounds<T: std::str::FromStr>(arg: &str) -> Result<(Option<T>, Option<T>), String> {
    let (lo, hi) = arg.split_once("..").ok_or_else(|| format!("{arg:?} is not LO..HI"))?;
    let read = |s: &str| -> Result<Option<T>, String> {
        if s.is_empty() {
            Ok(None)
        } else {
            s.parse().map(Some).map_err(|_| format!("{s:?} is not a bound"))
        }
    };
    Ok((read(lo)?, read(hi)?))
}

impl Matcher {
    /// The matcher `s` spells: `None` for a literal, an error for a matcher spelled wrong.
    #[must_use]
    pub fn parse(s: &str) -> Option<Result<Matcher, String>> {
        let inner = s.strip_prefix('<')?.strip_suffix('>')?;
        let (kind, arg) = match inner.split_once(':') {
            Some((k, a)) => (k, Some(a)),
            None => (inner, None),
        };
        if !KINDS.contains(&kind) {
            return None;
        }
        let no_arg = |m: Matcher| match arg {
            None => Ok(m),
            Some(_) => Err(format!("{s}: <{kind}> takes no argument")),
        };
        Some(match kind {
            "any" => no_arg(Matcher::Any),
            "bool" => no_arg(Matcher::Bool),
            "string" => no_arg(Matcher::String),
            "hex12" => no_arg(Matcher::Hex(12)),
            "hex16" => no_arg(Matcher::Hex(16)),
            "hex64" => no_arg(Matcher::Hex(64)),
            "base64" => no_arg(Matcher::Base64),
            "int" => match arg {
                None => Ok(Matcher::Int { lo: None, hi: None }),
                Some(a) => bounds::<i64>(a)
                    .map(|(lo, hi)| Matcher::Int { lo, hi })
                    .map_err(|e| format!("{s}: {e}")),
            },
            "number" => match arg {
                None => Ok(Matcher::Number { lo: None, hi: None }),
                Some(a) => bounds::<f64>(a)
                    .map(|(lo, hi)| Matcher::Number { lo, hi })
                    .map_err(|e| format!("{s}: {e}")),
            },
            "id" => match arg {
                Some(name) if name_ok(name) => Ok(Matcher::Id(name.to_owned())),
                _ => Err(format!("{s}: <id:NAME> needs a name of a-z 0-9 _")),
            },
            // "golden": the only kind left.
            _ => match arg {
                Some(a) => {
                    let parts: Vec<&str> = a.split('/').collect();
                    if parts.len() >= 2 && parts.iter().all(|p| path_part_ok(p)) {
                        Ok(Matcher::Golden {
                            set: parts[0].to_owned(),
                            path: parts[1..].iter().map(|p| (*p).to_owned()).collect(),
                        })
                    } else {
                        Err(format!("{s}: <golden:SET/NAME[/FIELD]> needs a set and a name"))
                    }
                }
                None => Err(format!("{s}: <golden:SET/NAME> needs a set and a name")),
            },
        })
    }

    /// Whether `v` is a value this matcher stands for, ignoring bindings (an `id` takes anything
    /// but `null`; a golden value is not known here).
    #[must_use]
    pub fn accepts(&self, v: &Value) -> bool {
        match self {
            Matcher::Any => true,
            Matcher::Bool => v.is_boolean(),
            Matcher::Int { lo, hi } => as_integer(v).is_some_and(|n| {
                lo.is_none_or(|lo| n >= i128::from(lo)) && hi.is_none_or(|hi| n <= i128::from(hi))
            }),
            Matcher::Number { lo, hi } => v
                .as_f64()
                .is_some_and(|x| lo.is_none_or(|lo| x >= lo) && hi.is_none_or(|hi| x <= hi)),
            Matcher::String => v.is_string(),
            Matcher::Hex(n) => v.as_str().is_some_and(|s| is_hex(s, *n)),
            Matcher::Base64 => v.as_str().is_some_and(|s| {
                !s.is_empty() && STANDARD.decode(s).is_ok_and(|b| STANDARD.encode(&b) == s)
            }),
            Matcher::Id(_) => !v.is_null(),
            Matcher::Golden { .. } => false,
        }
    }

    /// The value realizing this matcher when nothing better is known (PROTOCOL.md, "Realizing").
    #[must_use]
    pub fn default_value(&self) -> Value {
        match self {
            Matcher::Bool => Value::Bool(false),
            Matcher::Int { lo, hi } => Value::from(match (lo, hi) {
                (Some(lo), _) => *lo,
                (None, Some(hi)) => (*hi).min(0),
                (None, None) => 0,
            }),
            Matcher::Number { lo, hi } => Value::from(match (lo, hi) {
                (Some(lo), _) => *lo,
                (None, Some(hi)) => hi.min(0.0),
                (None, None) => 0.0,
            }),
            Matcher::String => Value::from(""),
            Matcher::Hex(n) => Value::from("0".repeat(*n)),
            Matcher::Base64 => Value::from(STANDARD.encode([0u8; 32])),
            Matcher::Any | Matcher::Id(_) | Matcher::Golden { .. } => Value::Null,
        }
    }
}

/// The values of the golden sets a transcript names.
pub trait Golden {
    /// The value at `path` (`NAME`, then fields) in the golden set `set`, if known.
    fn value(&self, set: &str, path: &[String]) -> Option<Value>;
}

/// No golden set: every golden matcher fails to match, and realizes from the canonical example.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGolden;

impl Golden for NoGolden {
    fn value(&self, _set: &str, _path: &[String]) -> Option<Value> {
        None
    }
}

/// The committed golden sets: `jobs`, citar-jobs' `golden/jobs.json` (4-07), whose entry NAME is
/// `jobs[NAME]` and whose FIELD is that entry's (`spec`, `spec_hash`, `bots`, `result`,
/// `result_hash`, `chain_head`, `rounds`, `digests`).
#[derive(Clone, Debug)]
pub struct Committed {
    jobs: Value,
}

impl Committed {
    /// The sets as committed.
    ///
    /// # Errors
    /// A golden file that does not read.
    pub fn load() -> Result<Self, String> {
        let set = citar_jobs::golden::GoldenSet::committed()?;
        Ok(Self { jobs: serde_json::to_value(&set.jobs).map_err(|e| e.to_string())? })
    }
}

impl Golden for Committed {
    fn value(&self, set: &str, path: &[String]) -> Option<Value> {
        if set != "jobs" {
            return None;
        }
        let mut v = &self.jobs;
        for p in path {
            v = v.get(p.as_str())?;
        }
        Some(v.clone())
    }
}

/// The values bound to `<id:NAME>` so far in a transcript, across both directions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bindings(pub BTreeMap<String, Value>);

impl Bindings {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    /// Binds a name before the first step (the player's own token, jobs build, machine id).
    pub fn bind(&mut self, name: &str, value: Value) {
        self.0.insert(name.to_owned(), value);
    }
}

/// Why a value did not match.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("at {}: {why}", if path.is_empty() { "the root" } else { path.as_str() })]
pub struct Mismatch {
    pub path: String,
    pub why: String,
}

/// JSON values equal, numbers compared by value (`2` equals `2.0`).
#[must_use]
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => match (as_integer(a), as_integer(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a.as_f64() == b.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

fn at(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_owned() } else { format!("{path}.{key}") }
}

/// Whether `actual` matches `pattern`. Bindings made by a match are kept only when the whole
/// value matches.
///
/// # Errors
/// The first place it does not match.
pub fn matches(
    pattern: &Value,
    actual: &Value,
    bindings: &mut Bindings,
    golden: &dyn Golden,
) -> Result<(), Mismatch> {
    let mut scratch = bindings.clone();
    match_at(pattern, actual, &mut scratch, golden, "")?;
    *bindings = scratch;
    Ok(())
}

fn miss(path: &str, why: String) -> Mismatch {
    Mismatch { path: path.to_owned(), why }
}

fn match_at(
    pattern: &Value,
    actual: &Value,
    b: &mut Bindings,
    golden: &dyn Golden,
    path: &str,
) -> Result<(), Mismatch> {
    if let Value::String(s) = pattern
        && let Some(m) = Matcher::parse(s)
    {
        let m = m.map_err(|e| miss(path, e))?;
        return match &m {
            Matcher::Id(name) => match b.get(name) {
                Some(bound) if json_eq(bound, actual) => Ok(()),
                Some(bound) => Err(miss(path, format!("{actual} is not {name}'s binding {bound}"))),
                None if actual.is_null() => {
                    Err(miss(path, format!("{name} cannot be bound to null")))
                }
                None => {
                    b.bind(name, actual.clone());
                    Ok(())
                }
            },
            Matcher::Golden { set, path: gp } => match golden.value(set, gp) {
                Some(want) if json_eq(&want, actual) => Ok(()),
                Some(want) => Err(miss(path, format!("{actual} is not the golden {want}"))),
                None => Err(miss(path, format!("no golden value for {s}"))),
            },
            m if m.accepts(actual) => Ok(()),
            _ => Err(miss(path, format!("{actual} does not match {s}"))),
        };
    }
    match (pattern, actual) {
        (Value::Object(p), Value::Object(a)) => {
            for (k, pv) in p {
                let here = at(path, k);
                let av = a.get(k).ok_or_else(|| miss(&here, "absent".to_owned()))?;
                match_at(pv, av, b, golden, &here)?;
            }
            Ok(())
        }
        (Value::Array(p), Value::Array(a)) => {
            if p.len() != a.len() {
                return Err(miss(path, format!("{} items, not {}", a.len(), p.len())));
            }
            for (i, (pv, av)) in p.iter().zip(a).enumerate() {
                match_at(pv, av, b, golden, &at(path, &i.to_string()))?;
            }
            Ok(())
        }
        _ if json_eq(pattern, actual) => Ok(()),
        _ => Err(miss(path, format!("{actual} is not {pattern}"))),
    }
}

/// The frame the scripted side sends for `pattern` (PROTOCOL.md, "Realizing"): literals as
/// written; `<id:NAME>` its binding, else `values[NAME]` (which binds it); a golden value, else
/// the example's; any other matcher the example's value at the same path when it matches, else
/// the matcher's default. `example` is the frame type's canonical example.
///
/// # Errors
/// An `<id:NAME>` with neither a binding nor a value, or a matcher spelled wrong.
pub fn realize(
    pattern: &Value,
    bindings: &mut Bindings,
    values: &Map<String, Value>,
    golden: &dyn Golden,
    example: Option<&Value>,
) -> Result<Value, String> {
    if let Value::String(s) = pattern
        && let Some(m) = Matcher::parse(s)
    {
        let m = m?;
        return match &m {
            Matcher::Id(name) => match bindings.get(name) {
                Some(v) => Ok(v.clone()),
                None => {
                    let v = values
                        .get(name)
                        .ok_or_else(|| format!("<id:{name}> has no binding and no value"))?
                        .clone();
                    bindings.bind(name, v.clone());
                    Ok(v)
                }
            },
            Matcher::Golden { set, path } => golden
                .value(set, path)
                .or_else(|| example.cloned())
                .ok_or_else(|| format!("{s}: no golden value and no example")),
            m => Ok(example.filter(|e| m.accepts(e)).cloned().unwrap_or_else(|| m.default_value())),
        };
    }
    Ok(match pattern {
        Value::Object(p) => {
            let mut out = Map::new();
            for (k, pv) in p {
                let ex = example.and_then(|e| e.get(k));
                out.insert(k.clone(), realize(pv, bindings, values, golden, ex)?);
            }
            Value::Object(out)
        }
        Value::Array(p) => {
            let ex = example.and_then(Value::as_array);
            let mut out = Vec::with_capacity(p.len());
            for (i, pv) in p.iter().enumerate() {
                let e = ex.and_then(|e| e.get(i).or_else(|| e.first()));
                out.push(realize(pv, bindings, values, golden, e)?);
            }
            Value::Array(out)
        }
        other => other.clone(),
    })
}

/// Every matcher spelled in `v`, with its path: for the loaders, which refuse a malformed one.
pub fn matchers_in(v: &Value, path: &str, out: &mut Vec<(String, Result<Matcher, String>)>) {
    match v {
        Value::String(s) => {
            if let Some(m) = Matcher::parse(s) {
                out.push((path.to_owned(), m));
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                matchers_in(item, &at(path, &i.to_string()), out);
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                matchers_in(item, &at(path, k), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn m(s: &str) -> Matcher {
        Matcher::parse(s).expect("a matcher").expect("well formed")
    }

    struct OneGolden;

    impl Golden for OneGolden {
        fn value(&self, set: &str, path: &[String]) -> Option<Value> {
            (set == "jobs" && path == ["duel-a", "rounds"]).then(|| json!(30))
        }
    }

    fn check(pattern: Value, actual: Value) -> Result<(), Mismatch> {
        matches(&pattern, &actual, &mut Bindings::default(), &OneGolden)
    }

    /// Gate 4: each matcher accepts what it stands for and refuses the rest.
    #[test]
    fn each_matcher_accepts_and_refuses() {
        let cases: &[(&str, &[Value], &[Value])] = &[
            ("<any>", &[json!(null), json!(1), json!("x"), json!({})], &[]),
            ("<bool>", &[json!(true), json!(false)], &[json!(0), json!("true"), json!(null)]),
            (
                "<int>",
                &[json!(0), json!(-5), json!(u64::MAX)],
                &[json!(1.5), json!(2.0), json!(true), json!("1")],
            ),
            ("<int:1..>", &[json!(1), json!(99)], &[json!(0), json!(-1)]),
            ("<int:-100..100>", &[json!(-100), json!(100)], &[json!(101), json!(-101)]),
            ("<int:..3>", &[json!(3), json!(-9)], &[json!(4)]),
            ("<number>", &[json!(0), json!(1.5), json!(-2)], &[json!("1"), json!(null)]),
            ("<number:0..1>", &[json!(0), json!(0.5), json!(1)], &[json!(1.01), json!(-0.1)]),
            ("<number:0.001..>", &[json!(0.001), json!(9)], &[json!(0)]),
            ("<string>", &[json!(""), json!("x")], &[json!(1), json!(null)]),
            (
                "<hex12>",
                &[json!("0123456789ab")],
                &[json!("0123456789AB"), json!("0123456789a"), json!(1)],
            ),
            ("<hex16>", &[json!("0123456789abcdef")], &[json!("0123456789abcde")]),
            (
                "<hex64>",
                &[json!("ab".repeat(32))],
                &[json!("ab".repeat(31)), json!("zz".repeat(32))],
            ),
            (
                "<base64>",
                &[json!("AAEC"), json!("AA==")],
                &[json!(""), json!("AA="), json!("AB=="), json!("**")],
            ),
            ("<golden:jobs/duel-a/rounds>", &[json!(30), json!(30.0)], &[json!(31)]),
            ("<golden:jobs/duel-b/rounds>", &[], &[json!(30)]),
        ];
        for (pattern, yes, no) in cases {
            for v in *yes {
                assert_eq!(check(json!(pattern), v.clone()), Ok(()), "{pattern} should accept {v}");
            }
            for v in *no {
                assert!(check(json!(pattern), v.clone()).is_err(), "{pattern} should refuse {v}");
            }
        }
    }

    #[test]
    fn ids_bind_once_and_hold_across_frames() {
        let mut b = Bindings::default();
        let p = json!({"id": "<id:req>"});
        assert_eq!(matches(&p, &json!({"id": "abc"}), &mut b, &NoGolden), Ok(()));
        assert_eq!(b.get("req"), Some(&json!("abc")));
        assert_eq!(matches(&p, &json!({"id": "abc", "x": 1}), &mut b, &NoGolden), Ok(()));
        let e = matches(&p, &json!({"id": "abd"}), &mut b, &NoGolden).unwrap_err();
        assert_eq!(e.path, "id");
        // Null never binds, and a failed match binds nothing.
        let mut fresh = Bindings::default();
        assert!(matches(&json!(["<id:a>", 2]), &json!([1, 3]), &mut fresh, &NoGolden).is_err());
        assert_eq!(fresh, Bindings::default());
        assert!(matches(&json!("<id:a>"), &json!(null), &mut fresh, &NoGolden).is_err());
    }

    #[test]
    fn objects_are_open_and_arrays_closed() {
        assert_eq!(check(json!({"a": 1}), json!({"a": 1, "b": 2})), Ok(()));
        assert_eq!(check(json!({"a": 1}), json!({"a": 1.0})), Ok(()));
        let e = check(json!({"a": {"b": "<int>"}}), json!({"a": {}})).unwrap_err();
        assert_eq!((e.path.as_str(), e.why.as_str()), ("a.b", "absent"));
        assert!(check(json!([1, 2]), json!([1, 2, 3])).is_err());
        assert!(check(json!([1, "<any>"]), json!([1])).is_err());
        let e = check(json!({"xs": [1, "<string>"]}), json!({"xs": [1, 2]})).unwrap_err();
        assert_eq!(e.path, "xs.1");
        assert!(check(json!(null), json!(0)).is_err());
        assert!(check(json!("literal"), json!("literal")).is_ok());
    }

    #[test]
    fn strings_that_are_not_matchers_are_literals_and_bad_matchers_are_errors() {
        assert_eq!(Matcher::parse("<think>"), None);
        assert_eq!(Matcher::parse("<think>hi</think>"), None);
        assert_eq!(Matcher::parse("plain"), None);
        assert_eq!(check(json!("<think>"), json!("<think>")), Ok(()));
        for bad in
            ["<int:a..b>", "<int:5>", "<any:x>", "<id:>", "<id:Up>", "<golden:jobs>", "<hex12:3>"]
        {
            assert!(matches!(Matcher::parse(bad), Some(Err(_))), "{bad}");
            assert!(check(json!(bad), json!(1)).is_err(), "{bad}");
        }
        assert_eq!(m("<int:1..>"), Matcher::Int { lo: Some(1), hi: None });
        assert_eq!(
            m("<golden:jobs/duel-a/spec_hash>"),
            Matcher::Golden { set: "jobs".into(), path: vec!["duel-a".into(), "spec_hash".into()] }
        );
    }

    #[test]
    fn realizing_takes_bindings_values_examples_then_defaults() {
        let values =
            json!({"req": "0123456789abcdef", "n": 7}).as_object().cloned().unwrap_or_default();
        let mut b = Bindings::default();
        b.bind("token", json!("tok"));
        let pattern = json!({
            "t": "x", "token": "<id:token>", "id": "<id:req>", "count": "<int:1..>",
            "rate": "<number:0.5..>", "name": "<string>", "list": ["<hex12>", "<hex12>"],
            "flag": "<bool>", "key": "<base64>", "g": "<golden:jobs/duel-a/rounds>",
            "h": "<golden:jobs/duel-b/rounds>", "any": "<any>"
        });
        let example = json!({"count": 0, "rate": 2.0, "name": "ex", "list": ["aaaaaaaaaaaa"],
                             "h": 12, "any": {"k": 1}});
        let out = realize(&pattern, &mut b, &values, &OneGolden, Some(&example)).unwrap();
        assert_eq!(
            out,
            json!({
                "t": "x", "token": "tok", "id": "0123456789abcdef", "count": 1, "rate": 2.0,
                "name": "ex", "list": ["aaaaaaaaaaaa", "aaaaaaaaaaaa"], "flag": false,
                "key": STANDARD.encode([0u8; 32]), "g": 30, "h": 12, "any": {"k": 1}
            })
        );
        assert_eq!(b.get("req"), Some(&json!("0123456789abcdef")));
        assert!(realize(&json!("<id:nobody>"), &mut b, &values, &NoGolden, None).is_err());
        assert!(realize(&json!("<golden:jobs/x>"), &mut b, &values, &NoGolden, None).is_err());
        // A realized value matches its own pattern.
        let mut b2 = b.clone();
        assert_eq!(
            matches(&pattern, &out, &mut b2, &OneGolden).map_err(|e| e.path),
            Err("h".into())
        );
    }
}
