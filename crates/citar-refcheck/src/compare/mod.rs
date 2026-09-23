//! Comparing a Rust answer with the recorded Python answer (DESIGN.md 9.2, "Comparison").
//!
//! Both answers are JSON values, compared node by node from the root:
//!
//! - integers exactly, other numbers within [`value::TOLERANCE`] (3 and 3.0 are equal);
//! - strings exactly, with a line diff for long or multi-line text ([`text`]);
//! - object keys as a union: a key on one side only is [`DiffKind::Missing`] (Python has it) or
//!   [`DiffKind::Extra`] (Rust has it), and `null` is not the same as absent;
//! - lists in order, unless the group's [`CompareSpec`] makes them keyed, multisets or custom.
//!
//! Every difference carries its concrete [`Path`], its kind and both values, which is all the
//! intended list, the enforced list and the reports need.

pub mod path;
pub mod route;
pub mod spec;
pub mod text;
pub mod value;

use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub use path::{Path, Pattern, Scalar, Seg};
pub use route::Grid;
pub use spec::{CompareSpec, ListKey, RouteFields, Rule};

use path::Cursor;
use spec::Treatment;

/// What kind of difference a [`Diff`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiffKind {
    /// Python has a key or list element that Rust lacks.
    Missing,
    /// Rust has a key or list element that Python lacks.
    Extra,
    /// The two values are of different JSON types (`null` against a number, say).
    Type,
    Number,
    Bool,
    Text,
    /// A keyed list whose elements lack the key or repeat it; it was compared in order instead.
    Key,
    /// A route of Rust's that is not as good as Python's: wrong ends, a jump, or more cost.
    Route,
    /// A different route with the same ends, turns and summed cost: accepted without an entry.
    PathEquivalent,
    /// A route with fewer turns than Python's. Needs `rule = "rust_le_python"`.
    Better,
    /// The answer module failed or panicked, so there was nothing to compare.
    Error,
}

impl DiffKind {
    /// The name in reports.
    pub fn name(self) -> &'static str {
        match self {
            DiffKind::Missing => "missing",
            DiffKind::Extra => "extra",
            DiffKind::Type => "type",
            DiffKind::Number => "number",
            DiffKind::Bool => "bool",
            DiffKind::Text => "text",
            DiffKind::Key => "key",
            DiffKind::Route => "route",
            DiffKind::PathEquivalent => "path_equivalent",
            DiffKind::Better => "better",
            DiffKind::Error => "error",
        }
    }

    /// Whether the difference is accepted by the comparison rules themselves, with no entry.
    pub fn is_accepted(self) -> bool {
        self == DiffKind::PathEquivalent
    }
}

/// One difference between the Python and the Rust answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub path: Path,
    pub kind: DiffKind,
    /// Python's value there, or `None` when Python has nothing there.
    pub python: Option<Value>,
    /// Rust's value there, or `None` when Rust has nothing there.
    pub rust: Option<Value>,
    /// More to say: a line diff, why a route fails, which key repeats.
    pub detail: Option<String>,
}

impl Diff {
    /// An answer module's failure, reported at the root of the group's answer.
    pub fn error(message: impl Into<String>) -> Diff {
        Diff {
            path: Path::root(),
            kind: DiffKind::Error,
            python: None,
            rust: None,
            detail: Some(message.into()),
        }
    }
}

/// Settings of one comparison.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    /// Compare the bot's valuations too (Phase 2).
    pub with_bot: bool,
    /// The fixture's map, for checking the steps of a different route.
    pub grid: Option<&'a Grid>,
}

/// Compares one group's answers.
pub fn compare(spec: &CompareSpec, python: &Value, rust: &Value, opts: &Options<'_>) -> Vec<Diff> {
    let mut cmp = Comparer { spec, opts, path: Vec::new(), out: Vec::new() };
    let root = spec.start();
    if let Treatment::Compare(rule) = spec.treatment(&root, opts.with_bot) {
        cmp.node(&root, rule, python, rust);
    }
    cmp.out
}

struct Comparer<'s> {
    spec: &'s CompareSpec,
    opts: &'s Options<'s>,
    path: Vec<Seg>,
    out: Vec<Diff>,
}

impl Comparer<'_> {
    fn emit(
        &mut self,
        kind: DiffKind,
        python: Option<&Value>,
        rust: Option<&Value>,
        detail: Option<String>,
    ) {
        self.out.push(Diff {
            path: Path(self.path.clone()),
            kind,
            python: python.cloned(),
            rust: rust.cloned(),
            detail,
        });
    }

    /// Compares one pair of values at the current path, whose cursor and rule are given.
    fn node(&mut self, cursor: &Cursor, rule: Option<&Rule>, py: &Value, rs: &Value) {
        match (py, rs) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(a), Value::Bool(b)) => {
                if a != b {
                    self.emit(DiffKind::Bool, Some(py), Some(rs), None);
                }
            }
            (Value::Number(a), Value::Number(b)) => {
                if !value::numbers_equal(a, b) {
                    self.emit(DiffKind::Number, Some(py), Some(rs), None);
                }
            }
            (Value::String(a), Value::String(b)) => {
                if a != b {
                    let detail = text::wants_line_diff(a, b).then(|| text::line_diff(a, b));
                    self.emit(DiffKind::Text, Some(py), Some(rs), detail);
                }
            }
            (Value::Array(a), Value::Array(b)) => match rule {
                Some(Rule::Keyed(key)) => self.keyed(cursor, key, a, b),
                Some(Rule::Multiset) => self.multiset(cursor, a, b),
                _ => self.in_order(cursor, a, b),
            },
            (Value::Object(a), Value::Object(b)) => match rule {
                Some(Rule::Route(fields)) => self.route(cursor, fields, a, b),
                _ => self.object(cursor, a, b, &[]),
            },
            _ => self.emit(DiffKind::Type, Some(py), Some(rs), None),
        }
    }

    /// Compares one child, which may be absent on either side.
    fn child(&mut self, cursor: &Cursor, seg: Seg, py: Option<&Value>, rs: Option<&Value>) {
        let next = self.spec.step(cursor, &seg);
        let Treatment::Compare(rule) = self.spec.treatment(&next, self.opts.with_bot) else {
            return;
        };
        self.path.push(seg);
        match (py, rs) {
            (Some(p), Some(r)) => self.node(&next, rule, p, r),
            (Some(p), None) => self.emit(DiffKind::Missing, Some(p), None, None),
            (None, Some(r)) => self.emit(DiffKind::Extra, None, Some(r), None),
            (None, None) => {}
        }
        self.path.pop();
    }

    fn object(
        &mut self,
        cursor: &Cursor,
        a: &Map<String, Value>,
        b: &Map<String, Value>,
        except: &[&str],
    ) {
        for (k, p) in a {
            if !except.contains(&k.as_str()) {
                self.child(cursor, Seg::Key(k.clone()), Some(p), b.get(k));
            }
        }
        for (k, r) in b {
            if !except.contains(&k.as_str()) && !a.contains_key(k) {
                self.child(cursor, Seg::Key(k.clone()), None, Some(r));
            }
        }
    }

    fn in_order(&mut self, cursor: &Cursor, a: &[Value], b: &[Value]) {
        for i in 0..a.len().max(b.len()) {
            self.child(cursor, Seg::Index(i), a.get(i), b.get(i));
        }
    }

    /// Elements matched by key. Python's order decides the report's order, then Rust's extras.
    fn keyed(&mut self, cursor: &Cursor, key: &ListKey, a: &[Value], b: &[Value]) {
        let (ka, kb) = match (keys_of(key, a), keys_of(key, b)) {
            (Ok(ka), Ok(kb)) => (ka, kb),
            (Err(why), _) => {
                self.emit(DiffKind::Key, None, None, Some(format!("Python's list: {why}")));
                return self.in_order(cursor, a, b);
            }
            (_, Err(why)) => {
                self.emit(DiffKind::Key, None, None, Some(format!("Rust's list: {why}")));
                return self.in_order(cursor, a, b);
            }
        };
        let rust_at: BTreeMap<&str, usize> =
            kb.iter().enumerate().map(|(j, (_, c))| (c.as_str(), j)).collect();
        let python_has: BTreeMap<&str, usize> =
            ka.iter().enumerate().map(|(i, (_, c))| (c.as_str(), i)).collect();
        for (i, (scalar, canon)) in ka.iter().enumerate() {
            let other = rust_at.get(canon.as_str()).map(|&j| &b[j]);
            self.child(cursor, key.seg(scalar), Some(&a[i]), other);
        }
        for (j, (scalar, canon)) in kb.iter().enumerate() {
            if !python_has.contains_key(canon.as_str()) {
                self.child(cursor, key.seg(scalar), None, Some(&b[j]));
            }
        }
    }

    /// Elements matched as a multiset: first exactly, by canonical text, then within the number
    /// tolerance. What is left over is Missing (at Python's index) or Extra (at Rust's).
    fn multiset(&mut self, cursor: &Cursor, a: &[Value], b: &[Value]) {
        let mut unmatched: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (j, r) in b.iter().enumerate().rev() {
            unmatched.entry(value::canonical(r)).or_default().push(j);
        }
        let mut left_a = Vec::new();
        for (i, p) in a.iter().enumerate() {
            match unmatched.get_mut(&value::canonical(p)).and_then(Vec::pop) {
                Some(_) => {}
                None => left_a.push(i),
            }
        }
        let mut left_b: Vec<usize> = unmatched.into_values().flatten().collect();
        left_b.sort_unstable();
        for i in left_a {
            // Only a float, or something holding one, can be equal without the same canonical text.
            let tolerant = match &a[i] {
                Value::Number(n) => n.is_f64(),
                Value::Array(_) | Value::Object(_) => true,
                _ => false,
            };
            let found = tolerant
                .then(|| {
                    left_b
                        .iter()
                        .position(|&j| self.equal_below(cursor, Seg::Index(i), &a[i], &b[j]))
                })
                .flatten();
            match found {
                Some(k) => {
                    left_b.remove(k);
                }
                None => self.child(cursor, Seg::Index(i), Some(&a[i]), None),
            }
        }
        for j in left_b {
            self.child(cursor, Seg::Index(j), None, Some(&b[j]));
        }
    }

    /// Whether two values compare equal as a child of the current node, under the same spec.
    fn equal_below(&self, cursor: &Cursor, seg: Seg, py: &Value, rs: &Value) -> bool {
        let mut trial =
            Comparer { spec: self.spec, opts: self.opts, path: self.path.clone(), out: Vec::new() };
        trial.child(cursor, seg, Some(py), Some(rs));
        trial.out.iter().all(|d| d.kind.is_accepted())
    }

    /// A route entry (DESIGN.md 9.2, `PathEquivalent`). The same tiles compare as any object; a
    /// different route is judged by its ends, its steps, its turns and its summed cost.
    fn route(
        &mut self,
        cursor: &Cursor,
        f: &RouteFields,
        a: &Map<String, Value>,
        b: &Map<String, Value>,
    ) {
        let (Some(Value::Array(pa)), Some(Value::Array(pb))) = (a.get(&f.path), b.get(&f.path))
        else {
            // A route against none: reachability disagrees, which the plain comparison reports.
            return self.object(cursor, a, b, &[]);
        };
        if pa.len() == pb.len() && pa.iter().zip(pb).all(|(x, y)| value::values_equal(x, y)) {
            return self.object(cursor, a, b, &[]);
        }
        let path_key = f.path.as_str();
        let costs_key = f.costs.as_str();
        let turns_key = f.turns.as_str();
        let rust_route = &b[path_key];
        let python_route = &a[path_key];

        if let Some(why) = route::invalid_route(self.opts.grid, pa, pb, b.get(costs_key)) {
            self.at_key(path_key, DiffKind::Route, python_route, rust_route, why);
            return self.object(cursor, a, b, &[path_key, costs_key]);
        }
        let turns =
            (a.get(turns_key).and_then(Value::as_f64), b.get(turns_key).and_then(Value::as_f64));
        let costs = (route::summed_cost(a.get(costs_key)), route::summed_cost(b.get(costs_key)));
        let (Some(pt), Some(rt)) = turns else {
            self.at_key(
                path_key,
                DiffKind::Route,
                python_route,
                rust_route,
                "a route without turns".into(),
            );
            return self.object(cursor, a, b, &[path_key, costs_key]);
        };
        let (Some(pc), Some(rc)) = costs else {
            self.at_key(
                path_key,
                DiffKind::Route,
                python_route,
                rust_route,
                "a route without step costs".into(),
            );
            return self.object(cursor, a, b, &[path_key, costs_key]);
        };
        if rt < pt && !value::floats_equal(rt, pt) {
            let detail = format!(
                "Rust's route takes {rt} turns where Python's takes {pt}: {} against {}",
                value::short(rust_route, 120),
                value::short(python_route, 120)
            );
            let (py_turns, rs_turns) = (&a[turns_key], &b[turns_key]);
            self.at_key(turns_key, DiffKind::Better, py_turns, rs_turns, detail);
            return self.object(cursor, a, b, &[path_key, costs_key, turns_key]);
        }
        if value::floats_equal(rt, pt) && value::floats_equal(rc, pc) {
            let detail =
                format!("same ends, adjacent steps, {rt} turns and summed cost {rc} on both sides");
            self.at_key(path_key, DiffKind::PathEquivalent, python_route, rust_route, detail);
            return self.object(cursor, a, b, &[path_key, costs_key]);
        }
        let detail = format!(
            "a valid route, but {rt} turns and summed cost {rc} against Python's {pt} and {pc}"
        );
        self.at_key(path_key, DiffKind::Route, python_route, rust_route, detail);
        self.object(cursor, a, b, &[path_key, costs_key, turns_key]);
    }

    fn at_key(&mut self, key: &str, kind: DiffKind, py: &Value, rs: &Value, detail: String) {
        self.path.push(Seg::Key(key.to_string()));
        self.emit(kind, Some(py), Some(rs), Some(detail));
        self.path.pop();
    }
}

impl ListKey {
    fn seg(&self, key: &Scalar) -> Seg {
        match self {
            ListKey::Field(name) => Seg::Field(name.clone(), key.clone()),
            ListKey::Pos(n) => Seg::Pos(*n, key.clone()),
        }
    }

    fn describe(&self) -> String {
        match self {
            ListKey::Field(name) => format!("field `{name}`"),
            ListKey::Pos(n) => format!("position {n}"),
        }
    }
}

/// Each element's key, with its canonical text, or why the list cannot be keyed.
fn keys_of(key: &ListKey, list: &[Value]) -> std::result::Result<Vec<(Scalar, String)>, String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(list.len());
    for (i, element) in list.iter().enumerate() {
        let raw = match key {
            ListKey::Field(name) => element.as_object().and_then(|o| o.get(name)),
            ListKey::Pos(n) => element.as_array().and_then(|a| a.get(*n)),
        };
        let Some(scalar) = raw.and_then(Scalar::from_value) else {
            return Err(format!("element [{i}] has no scalar {} to key by", key.describe()));
        };
        let canon = scalar.canonical();
        if let Some(first) = seen.insert(canon.clone(), i) {
            return Err(format!(
                "elements [{first}] and [{i}] share the key {scalar} ({})",
                key.describe()
            ));
        }
        out.push((scalar, canon));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Group;
    use serde_json::json;

    fn spec() -> CompareSpec {
        CompareSpec::new(Group::Civs)
            .ignore("fn")
            .keyed("civs", "pid")
            .multiset("civs[*].tags")
            .keyed_pos("civs[*].reach", 0)
    }

    fn diffs(spec: &CompareSpec, py: Value, rs: Value) -> Vec<(String, DiffKind)> {
        compare(spec, &py, &rs, &Options::default())
            .into_iter()
            .map(|d| (d.path.to_string(), d.kind))
            .collect()
    }

    #[test]
    fn identical_answers_have_no_differences() {
        let a =
            json!({"fn": {"x": "f"}, "civs": [{"pid": 0, "tags": ["a", "b"], "reach": [[1, 2]]}]});
        assert!(diffs(&spec(), a.clone(), a).is_empty());
    }

    #[test]
    fn numbers_use_the_tolerance_and_integers_are_exact() {
        let py = json!({"a": 3, "b": 0.1, "c": 10, "d": 2.5});
        let rs = json!({"a": 3.0, "b": 0.100_000_000_1, "c": 11, "d": 3.0});
        assert_eq!(
            diffs(&CompareSpec::new(Group::Civs), py, rs),
            [("c".into(), DiffKind::Number), ("d".into(), DiffKind::Number)]
        );
    }

    #[test]
    fn key_order_is_irrelevant_but_keys_are_a_union() {
        let py = json!({"a": 1, "b": 2, "gone": null});
        let rs = json!({"b": 2, "a": 1, "new": 5});
        assert_eq!(
            diffs(&CompareSpec::new(Group::Civs), py, rs),
            [("gone".into(), DiffKind::Missing), ("new".into(), DiffKind::Extra)],
            "null is not the same as absent"
        );
        let typed = diffs(&CompareSpec::new(Group::Civs), json!({"a": null}), json!({"a": 0}));
        assert_eq!(typed, [("a".into(), DiffKind::Type)]);
    }

    #[test]
    fn ignored_paths_are_not_compared() {
        let py = json!({"fn": {"x": "python.function"}, "v": 1});
        let rs = json!({"v": 1});
        assert!(diffs(&spec(), py, rs).is_empty());
    }

    #[test]
    fn lists_compare_in_order_unless_keyed_or_multisets() {
        let plain = CompareSpec::new(Group::Civs);
        assert_eq!(
            diffs(&plain, json!({"l": [1, 2, 3]}), json!({"l": [1, 3]})),
            [("l[1]".into(), DiffKind::Number), ("l[2]".into(), DiffKind::Missing)]
        );
        // Keyed: order does not matter, selectors name the key.
        let py = json!({"civs": [{"pid": 0, "v": 1}, {"pid": 1, "v": 2}, {"pid": 2, "v": 3}]});
        let rs = json!({"civs": [{"pid": 3, "v": 9}, {"pid": 1, "v": 2}, {"pid": 0, "v": 5}]});
        assert_eq!(
            diffs(&spec(), py, rs),
            [
                ("civs[pid=0].v".into(), DiffKind::Number),
                ("civs[pid=2]".into(), DiffKind::Missing),
                ("civs[pid=3]".into(), DiffKind::Extra)
            ]
        );
        // Keyed by tuple position.
        let py = json!({"civs": [{"pid": 0, "reach": [[5, 60], [6, 30]]}]});
        let rs = json!({"civs": [{"pid": 0, "reach": [[6, 30], [5, 50]]}]});
        assert_eq!(
            diffs(&spec(), py, rs),
            [("civs[pid=0].reach[#0=5][1]".into(), DiffKind::Number)]
        );
    }

    #[test]
    fn multisets_ignore_order_and_count_repeats() {
        let py = json!({"civs": [{"pid": 0, "tags": ["a", "b", "b", "c"]}]});
        let same = json!({"civs": [{"pid": 0, "tags": ["b", "c", "a", "b"]}]});
        assert!(diffs(&spec(), py.clone(), same).is_empty());
        let short = json!({"civs": [{"pid": 0, "tags": ["b", "c", "a", "d"]}]});
        assert_eq!(
            diffs(&spec(), py, short),
            [
                ("civs[pid=0].tags[2]".into(), DiffKind::Missing),
                ("civs[pid=0].tags[3]".into(), DiffKind::Extra)
            ]
        );
        // Within the tolerance, elements still pair up.
        let py = json!({"civs": [{"pid": 0, "tags": [[1.0, "x"], [2.0, "y"]]}]});
        let rs = json!({"civs": [{"pid": 0, "tags": [[2.000_000_000_1, "y"], [1, "x"]]}]});
        assert!(diffs(&spec(), py, rs).is_empty());
    }

    #[test]
    fn a_keyed_list_without_unique_keys_is_reported_and_compared_in_order() {
        let py = json!({"civs": [{"pid": 0, "v": 1}, {"pid": 0, "v": 2}]});
        let got = diffs(&spec(), py.clone(), py);
        assert_eq!(got, [("civs".into(), DiffKind::Key)]);
    }

    #[test]
    fn long_texts_carry_a_line_diff() {
        let py = json!({"t": "line one\nline two\n"});
        let rs = json!({"t": "line one\nline 2\n"});
        let d = compare(&CompareSpec::new(Group::Civs), &py, &rs, &Options::default());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiffKind::Text);
        assert!(d[0].detail.as_deref().is_some_and(|t| t.contains("+line 2")));
        let d =
            compare(&CompareSpec::new(Group::Civs), &json!("a"), &json!("b"), &Options::default());
        assert_eq!(d[0].detail, None);
    }

    fn movement() -> CompareSpec {
        CompareSpec::for_group(Group::Movement)
    }

    fn entry(path: Value, turns: u32, costs: Value) -> Value {
        json!({"paths": [{"unit": 4, "from": 1, "to": 6, "moves": 60, "path": path, "turns": turns, "step_costs": costs}]})
    }

    fn route_diffs(py: Value, rs: Value) -> Vec<(String, DiffKind)> {
        let grid = Grid::new(4, 4, false, false);
        let opts = Options { with_bot: false, grid: Some(&grid) };
        compare(&movement(), &py, &rs, &opts)
            .into_iter()
            .map(|d| (d.path.to_string(), d.kind))
            .collect()
    }

    #[test]
    fn routes_are_path_equivalent_better_or_different() {
        // On a 4x4 map, 1 -> 5 -> 6 and 1 -> 2 -> 6 are both two steps.
        let python = entry(json!([1, 5, 6]), 1, json!([30, 30]));
        let same = entry(json!([1, 2, 6]), 1, json!([40, 20]));
        assert_eq!(
            route_diffs(python.clone(), same),
            [("paths[0].path".into(), DiffKind::PathEquivalent)]
        );
        let costly = entry(json!([1, 2, 6]), 1, json!([40, 40]));
        assert_eq!(
            route_diffs(python.clone(), costly),
            [("paths[0].path".into(), DiffKind::Route)]
        );
        let quicker = entry(json!([1, 2, 6]), 0, json!([30, 30]));
        assert_eq!(
            route_diffs(python.clone(), quicker),
            [("paths[0].turns".into(), DiffKind::Better)]
        );
        let jump = entry(json!([1, 6]), 1, json!([60]));
        assert_eq!(route_diffs(python.clone(), jump), [("paths[0].path".into(), DiffKind::Route)]);
        let slower = entry(json!([1, 5, 6]), 2, json!([30, 30]));
        assert_eq!(
            route_diffs(python.clone(), slower),
            [("paths[0].turns".into(), DiffKind::Number)]
        );
        // Reachability must agree: a route against none is a plain difference.
        let none = json!({"paths": [{"unit": 4, "from": 1, "to": 6, "moves": 60, "path": null}]});
        let got = route_diffs(python, none);
        assert_eq!(got[0], ("paths[0].path".into(), DiffKind::Type));
        assert!(got.iter().any(|d| d == &("paths[0].turns".into(), DiffKind::Missing)));
    }
}
