//! What a check step asserts about its subject: the matchers of `tests/rules/README.md`.
//!
//! Values compare as JSON: numbers by value (3 equals 3.0, and a boolean is never a number),
//! objects as maps whatever their key order, lists in order.

use regex::Regex;
use serde_json::{Map, Value};

use super::path;

/// The matchers a check may use, in the order they are tried.
pub const MATCHERS: [&str; 16] = [
    "absent",
    "is_null",
    "eq",
    "ne",
    "gt",
    "ge",
    "lt",
    "le",
    "approx",
    "contains",
    "not_contains",
    "len",
    "matches",
    "any",
    "none",
    "subset",
];

/// Keys that go with a matcher rather than being one.
pub const WITH: [&str; 1] = ["tol"];

/// Whether two values are equal as JSON, numbers by value.
#[must_use]
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => match (x.as_u64(), y.as_u64()) {
                (Some(i), Some(j)) => i == j,
                _ => x.as_f64() == y.as_f64(),
            },
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// Checks the matchers of `spec` against `subject` (`None` where the path led nowhere). The
/// error says what was expected and what was found.
pub fn check(subject: Option<&Value>, spec: &Map<String, Value>) -> Result<(), String> {
    let used: Vec<&str> = MATCHERS.iter().copied().filter(|m| spec.contains_key(*m)).collect();
    if used.is_empty() {
        return Err("a check needs at least one matcher".to_owned());
    }
    if let Some(want) = spec.get("absent") {
        let want = want.as_bool().ok_or("absent must be true or false")?;
        if used.len() > 1 && want {
            return Err("absent = true takes no other matcher".to_owned());
        }
        if want != subject.is_none() {
            return Err(match subject {
                Some(v) => format!("expected nothing there, found {}", show(v)),
                None => "expected a value there, found nothing".to_owned(),
            });
        }
        if want {
            return Ok(());
        }
    }
    let Some(v) = subject else { return Err("found nothing at the path".to_owned()) };
    for m in used {
        let arg = &spec[m];
        let ok = match m {
            "absent" => true,
            "is_null" => v.is_null() == arg.as_bool().ok_or("is_null must be true or false")?,
            "eq" => same(v, arg),
            "ne" => !same(v, arg),
            "gt" | "ge" | "lt" | "le" => {
                let (a, b) = (num(v)?, num(arg)?);
                match m {
                    "gt" => a > b,
                    "ge" => a >= b,
                    "lt" => a < b,
                    _ => a <= b,
                }
            }
            "approx" => {
                let tol = spec.get("tol").map_or(Ok(1e-6), num)?;
                let (a, b) = (num(v)?, num(arg)?);
                (a - b).abs() <= tol * 1f64.max(a.abs()).max(b.abs())
            }
            "contains" => contains(v, arg)?,
            "not_contains" => !contains(v, arg)?,
            "len" => {
                let n = match v {
                    Value::Array(a) => a.len(),
                    Value::String(s) => s.chars().count(),
                    Value::Object(o) => o.len(),
                    _ => {
                        return Err(format!(
                            "len needs a list, a string or an object, not {}",
                            show(v)
                        ));
                    }
                };
                arg.as_u64().is_some_and(|want| usize::try_from(want).is_ok_and(|w| w == n))
            }
            "matches" => {
                let pattern = arg.as_str().ok_or("matches takes a regular expression")?;
                let re = Regex::new(pattern).map_err(|e| format!("bad pattern: {e}"))?;
                let text =
                    v.as_str().ok_or_else(|| format!("matches needs a string, not {}", show(v)))?;
                re.is_match(text)
            }
            "any" | "none" => {
                let inner = arg.as_object().ok_or("any and none take a table of matchers")?;
                let items =
                    v.as_array().ok_or_else(|| format!("{m} needs a list, not {}", show(v)))?;
                let found = items.iter().any(|item| element_passes(item, inner).unwrap_or(false));
                if m == "any" { found } else { !found }
            }
            "subset" => subset(v, arg)?,
            _ => return Err(format!("unknown matcher {m}")),
        };
        if !ok {
            let tol =
                if m == "approx" { spec.get("tol").map(|t| format!(" (tol {t})")) } else { None };
            return Err(format!(
                "expected {m} {}{}, got {}",
                show(arg),
                tol.unwrap_or_default(),
                show(v)
            ));
        }
    }
    Ok(())
}

/// Whether one list item passes the nested matchers of `any` or `none`, with their own `path`.
fn element_passes(item: &Value, inner: &Map<String, Value>) -> Result<bool, String> {
    let segs = match inner.get("path") {
        Some(Value::String(p)) => path::parse(p)?,
        Some(_) => return Err("path must be a string".to_owned()),
        None => Vec::new(),
    };
    Ok(check(path::get(item, &segs), inner).is_ok())
}

fn num(v: &Value) -> Result<f64, String> {
    v.as_f64().ok_or_else(|| format!("expected a number, got {}", show(v)))
}

fn contains(v: &Value, arg: &Value) -> Result<bool, String> {
    match v {
        Value::Array(items) => Ok(items.iter().any(|x| same(x, arg))),
        Value::String(s) => {
            let part = arg.as_str().ok_or("a string contains only strings")?;
            Ok(s.contains(part))
        }
        Value::Object(o) => {
            let key = arg.as_str().ok_or("an object contains only keys, which are strings")?;
            Ok(o.contains_key(key))
        }
        _ => Err(format!("contains needs a list, a string or an object, not {}", show(v))),
    }
}

/// Whether `part` is a subset of `v`: every key of an object with an equal value, or every item
/// of a list.
fn subset(v: &Value, part: &Value) -> Result<bool, String> {
    match (v, part) {
        (Value::Object(o), Value::Object(p)) => {
            Ok(p.iter().all(|(k, want)| o.get(k).is_some_and(|got| same(got, want))))
        }
        (Value::Array(items), Value::Array(p)) => {
            Ok(p.iter().all(|want| items.iter().any(|got| same(got, want))))
        }
        _ => Err(format!(
            "subset compares two objects or two lists, not {} and {}",
            show(v),
            show(part)
        )),
    }
}

/// A value as compact JSON, for messages.
#[must_use]
pub fn show(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() > 300 {
        let cut: String = text.chars().take(300).collect();
        format!("{cut}...")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn numbers_compare_by_value_and_booleans_are_not_numbers() {
        assert!(same(&json!(3), &json!(3.0)));
        assert!(!same(&json!(1), &json!(true)));
        assert!(same(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
    }

    #[test]
    fn matchers_pass_and_fail() {
        let v = json!({"xs": [1, 2, 3], "s": "hello", "o": {"k": 1}});
        let at = |p: &str| path::get(&v, &path::parse(p).unwrap_or_default()).cloned();
        assert!(check(at("xs").as_ref(), &spec(json!({"len": 3, "contains": 2}))).is_ok());
        assert!(check(at("xs").as_ref(), &spec(json!({"contains": 9}))).is_err());
        assert!(check(at("s").as_ref(), &spec(json!({"matches": "^h.l"}))).is_ok());
        assert!(check(at("nope").as_ref(), &spec(json!({"absent": true}))).is_ok());
        assert!(check(at("s").as_ref(), &spec(json!({"absent": true}))).is_err());
        assert!(check(at("o").as_ref(), &spec(json!({"subset": {"k": 1}}))).is_ok());
        assert!(check(at("xs").as_ref(), &spec(json!({"any": {"gt": 2}}))).is_ok());
        assert!(check(at("xs").as_ref(), &spec(json!({"none": {"gt": 3}}))).is_ok());
        assert!(check(Some(&json!(1.0000001)), &spec(json!({"approx": 1}))).is_ok());
        assert!(check(Some(&json!(1.1)), &spec(json!({"approx": 1}))).is_err());
        assert!(check(Some(&json!(1)), &spec(json!({}))).is_err(), "no matcher");
    }
}
