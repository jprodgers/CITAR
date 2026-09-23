//! `suggest`: `[[differences]]` stubs for the unexplained differences of a run.
//!
//! Stubs are never accepted automatically (DESIGN.md 9.2). A person decides whether each
//! difference is a porting mistake to fix or a deliberate fix to list, writes the reason, and
//! only then pastes the entry into `refcheck/intended.toml`. What `suggest` saves is the typing:
//! one stub per group, place (every list element as `[*]`) and kind, with the narrowest
//! constraints the differences share, so the entry cannot hide a later, unrelated change.

use serde_json::Value;

use crate::Group;
use crate::compare::value::{short, values_equal};
use crate::compare::{DiffKind, Pattern};
use crate::run::{Checked, Run, Subject};

/// Values longer than this (as JSON) are not written as exact constraints.
const MAX_EXACT: usize = 200;
/// Up to this many fixtures are listed in `cases`; more, and the stub applies everywhere.
const MAX_CASES: usize = 8;

struct Stub<'r> {
    group: Group,
    pattern: Pattern,
    kind: DiffKind,
    found: Vec<(&'r Subject, &'r Checked)>,
}

/// The stubs, as TOML with a comment above each.
pub fn suggest(run: &Run) -> String {
    let mut stubs: Vec<Stub<'_>> = Vec::new();
    for (sub, c) in run.findings().filter(|(_, c)| c.is_unexplained()) {
        let pattern = Pattern::generalize(&c.diff.path);
        match stubs
            .iter_mut()
            .find(|s| s.group == sub.group && s.kind == c.diff.kind && s.pattern == pattern)
        {
            Some(s) => s.found.push((sub, c)),
            None => stubs.push(Stub {
                group: sub.group,
                pattern,
                kind: c.diff.kind,
                found: vec![(sub, c)],
            }),
        }
    }
    if stubs.is_empty() {
        return "# No unexplained differences: nothing to suggest.\n".to_string();
    }
    let mut out = String::from(
        "# Stubs for refcheck/intended.toml, written by `cargo refcheck suggest`. None is accepted\n\
         # until a person has checked the difference, fixed it or written its reason, and pasted it in.\n",
    );
    for (n, stub) in stubs.iter().enumerate() {
        out.push('\n');
        out.push_str(&stub_text(run, stub, n + 1));
    }
    out
}

fn stub_text(run: &Run, stub: &Stub<'_>, n: usize) -> String {
    let mut cases: Vec<&str> = Vec::new();
    for (sub, _) in &stub.found {
        if !cases.contains(&sub.case.as_str()) {
            cases.push(&sub.case);
        }
    }
    let (sub0, c0) = stub.found[0];
    let d0 = &c0.diff;
    let side =
        |v: &Option<Value>| v.as_ref().map_or_else(|| "(absent)".to_string(), |v| short(v, 60));
    let mut out = format!(
        "# {} {} difference(s) in {} fixture(s), e.g. {} at {}: python {}, rust {}\n",
        stub.found.len(),
        stub.kind.name(),
        cases.len(),
        sub0.case,
        d0.path,
        side(&d0.python),
        side(&d0.rust),
    );
    out.push_str("[[differences]]\n");
    out.push_str(&format!("id = \"todo-{}-{n}\"\n", stub.group.name().replace('_', "-")));
    out.push_str("reason = \"TODO: what Python did, and what Rust does instead\"\n");
    out.push_str(&format!(
        "where = [{{ group = \"{}\", path = {} }}]\n",
        stub.group,
        toml::Value::String(stub.pattern.as_str().to_string())
    ));
    let compared = run.summary(stub.group).compared;
    if cases.len() <= MAX_CASES && (cases.len() as u64) < compared {
        let list: Vec<String> =
            cases.iter().map(|c| toml::Value::String((*c).to_string()).to_string()).collect();
        out.push_str(&format!("cases = [{}]\n", list.join(", ")));
    }
    if let Some(c) = constraint(stub.found.iter().map(|(_, c)| c.diff.python.as_ref())) {
        out.push_str(&format!("python = {c}\n"));
    }
    if let Some(c) = constraint(stub.found.iter().map(|(_, c)| c.diff.rust.as_ref())) {
        out.push_str(&format!("rust = {c}\n"));
    }
    if stub.kind == DiffKind::Better {
        out.push_str("rule = \"rust_le_python\"\n");
    }
    out
}

/// The narrowest constraint every value meets: the one value they all are, or the range of
/// numbers they span.
fn constraint<'a>(values: impl Iterator<Item = Option<&'a Value>> + Clone) -> Option<String> {
    let mut all = values.clone();
    let first = all.next()?;
    if all.all(|v| match (first, v) {
        (None, None) => true,
        (Some(a), Some(b)) => values_equal(a, b),
        _ => false,
    }) {
        return match first {
            None => Some("{ is = \"absent\" }".into()),
            Some(Value::Null) => Some("{ is = \"null\" }".into()),
            Some(v) if v.to_string().len() <= MAX_EXACT => to_toml(v).map(|t| t.to_string()),
            Some(_) => None,
        };
    }
    let numbers: Option<Vec<f64>> = values.map(|v| v.and_then(Value::as_f64)).collect();
    let numbers = numbers?;
    let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(format!("{{ min = {}, max = {} }}", toml::Value::Float(min), toml::Value::Float(max)))
}

/// A JSON value as TOML, if TOML can hold it (it has no null).
fn to_toml(v: &Value) -> Option<toml::Value> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(a) => toml::Value::Array(a.iter().map(to_toml).collect::<Option<_>>()?),
        Value::Object(o) => toml::Value::Table(
            o.iter().map(|(k, v)| Some((k.clone(), to_toml(v)?))).collect::<Option<_>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn constraints_are_as_narrow_as_the_values_allow() {
        let one = [Some(json!(3)), Some(json!(3.0))];
        assert_eq!(constraint(one.iter().map(Option::as_ref)).as_deref(), Some("3"));
        let range = [Some(json!(1.5)), Some(json!(4))];
        assert_eq!(
            constraint(range.iter().map(Option::as_ref)).as_deref(),
            Some("{ min = 1.5, max = 4.0 }")
        );
        let absent: [Option<Value>; 2] = [None, None];
        assert_eq!(
            constraint(absent.iter().map(Option::as_ref)).as_deref(),
            Some("{ is = \"absent\" }")
        );
        let mixed = [Some(json!("a")), Some(json!("b"))];
        assert_eq!(constraint(mixed.iter().map(Option::as_ref)), None);
        let text = [Some(json!("Unknown tool 'x'."))];
        assert_eq!(
            constraint(text.iter().map(Option::as_ref)).as_deref(),
            Some("\"Unknown tool 'x'.\"")
        );
        let with_null = [Some(json!([1, null]))];
        assert_eq!(constraint(with_null.iter().map(Option::as_ref)), None);
    }
}
