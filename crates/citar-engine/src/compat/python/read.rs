//! A strict reader of the JSON Python wrote: every value is read at a known place, and every key
//! of a record is accounted for.
//!
//! The converter reads Python's `GameState.to_dict()` from a parsed [`Value`] rather than through
//! serde mirror structs, so that each failure names its exact place (`players[3].flags.pairs["5"]
//! .wary`) and the reading and the mapping to typed state are one pass. A record read through an
//! [`Obj`] must have had every key taken by the time [`Obj::finish`] is called: a key the converter
//! does not know is an error, never a value silently lost.
//!
//! Python's typing was loose, so readers are permissive where Python was: a whole number may be
//! written as `50.0`, and a missing field takes the dataclass default, as `from_dict` gave it
//! (`state.py:315-405`). They are strict about everything else.

use core::cell::Cell;
use core::fmt::Write as _;

use serde_json::{Map, Value};

use super::ConvertError;

/// The result of every read.
pub(super) type Res<T> = Result<T, ConvertError>;

/// One step down the document.
#[derive(Clone, Copy, Debug)]
enum Seg<'a> {
    /// The document itself.
    Root,
    /// A field of a record: `.hp`.
    Field(&'a str),
    /// A key of a dict keyed by ids or names: `["12"]`.
    Key(&'a str),
    /// A list element: `[3]`.
    Index(usize),
}

/// A place in the document, built as the reader walks down and written out only when an error
/// needs it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Path<'a> {
    parent: Option<&'a Path<'a>>,
    seg: Seg<'a>,
}

impl<'a> Path<'a> {
    /// The document itself.
    pub(super) const ROOT: Path<'static> = Path { parent: None, seg: Seg::Root };

    /// A field of the record here.
    pub(super) const fn field(&'a self, name: &'a str) -> Self {
        Self { parent: Some(self), seg: Seg::Field(name) }
    }

    /// A key of the dict here.
    pub(super) const fn key(&'a self, key: &'a str) -> Self {
        Self { parent: Some(self), seg: Seg::Key(key) }
    }

    /// An element of the list here.
    pub(super) const fn index(&'a self, i: usize) -> Self {
        Self { parent: Some(self), seg: Seg::Index(i) }
    }

    /// The place as the path grammar writes it: `units["12"].promotions[0]`, or `$` for the
    /// document itself.
    pub(super) fn render(&self) -> String {
        let mut segs = Vec::new();
        let mut at = Some(self);
        while let Some(p) = at {
            segs.push(p.seg);
            at = p.parent;
        }
        let mut out = String::new();
        for seg in segs.iter().rev() {
            match *seg {
                Seg::Root => {}
                Seg::Field(name) if out.is_empty() => out.push_str(name),
                Seg::Field(name) => {
                    out.push('.');
                    out.push_str(name);
                }
                Seg::Key(k) => {
                    out.push_str("[\"");
                    for c in k.chars() {
                        if c == '"' || c == '\\' {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                    out.push_str("\"]");
                }
                Seg::Index(i) => {
                    let _ignored = write!(out, "[{i}]");
                }
            }
        }
        if out.is_empty() { "$".to_owned() } else { out }
    }

    /// An error here.
    pub(super) fn err(&self, message: impl Into<String>) -> ConvertError {
        ConvertError { path: self.render(), message: message.into() }
    }
}

/// A value as a message quotes it: short, with its JSON type evident.
pub(super) fn shown(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() > 60 {
        let cut: String = text.chars().take(57).collect();
        format!("{cut}...")
    } else {
        text
    }
}

/// The record `v` is.
pub(super) fn object<'v>(v: &'v Value, p: &Path<'_>) -> Res<&'v Map<String, Value>> {
    v.as_object().ok_or_else(|| p.err(format!("expected an object, found {}", shown(v))))
}

/// The list `v` is.
pub(super) fn list<'v>(v: &'v Value, p: &Path<'_>) -> Res<&'v [Value]> {
    v.as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| p.err(format!("expected a list, found {}", shown(v))))
}

/// The text `v` is.
pub(super) fn text<'v>(v: &'v Value, p: &Path<'_>) -> Res<&'v str> {
    v.as_str().ok_or_else(|| p.err(format!("expected text, found {}", shown(v))))
}

/// The truth value `v` is.
pub(super) fn flag(v: &Value, p: &Path<'_>) -> Res<bool> {
    v.as_bool().ok_or_else(|| p.err(format!("expected true or false, found {}", shown(v))))
}

/// The number `v` is. JSON holds no NaN or infinity, so it is finite.
pub(super) fn real(v: &Value, p: &Path<'_>) -> Res<f64> {
    v.as_f64().ok_or_else(|| p.err(format!("expected a number, found {}", shown(v))))
}

/// The whole number `v` is, which must fit `T`. Python wrote some whole numbers as floats
/// (`50.0`), which are accepted; a fraction is not.
pub(super) fn int<T: TryFrom<i64>>(v: &Value, p: &Path<'_>) -> Res<T> {
    let whole = match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            let x = n.as_f64()?;
            // Beyond 2^53 a float is no longer exactly a whole number Python meant.
            (x.fract() == 0.0 && x.abs() <= 9_007_199_254_740_992.0).then_some(x as i64)
        }),
        _ => None,
    };
    let Some(whole) = whole else {
        return Err(p.err(format!("expected a whole number, found {}", shown(v))));
    };
    T::try_from(whole).map_err(|_| p.err(format!("{whole} is out of range here")))
}

/// A whole number written as a dict key: `"12"`.
pub(super) fn key_int<T: TryFrom<i64>>(k: &str, p: &Path<'_>) -> Res<T> {
    let whole: i64 =
        k.parse().map_err(|_| p.err(format!("the key {k:?} is not a whole number")))?;
    T::try_from(whole).map_err(|_| p.err(format!("the key {k:?} is out of range here")))
}

/// Whether `v` is Python's `None`, or absent.
pub(super) const fn is_none(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null))
}

/// A record read field by field, which must have had every key taken when it is finished.
pub(super) struct Obj<'a> {
    map: &'a Map<String, Value>,
    path: Path<'a>,
    /// Bit `i` is set once the `i`th key (in document order) has been taken.
    taken: Cell<u128>,
}

impl<'a> Obj<'a> {
    /// The record `v` is, at `path`. A record has at most 128 keys; Python's largest, a player,
    /// has 69.
    pub(super) fn new(v: &'a Value, path: Path<'a>) -> Res<Self> {
        let map = object(v, &path)?;
        if map.len() > 128 {
            return Err(path.err(format!("a record with {} keys; none has so many", map.len())));
        }
        Ok(Self { map, path, taken: Cell::new(0) })
    }

    /// Where the record is.
    pub(super) const fn path(&self) -> &Path<'a> {
        &self.path
    }

    /// Where a field of the record is.
    pub(super) fn at<'s>(&'s self, key: &'s str) -> Path<'s> {
        self.path.field(key)
    }

    /// The field `key`, taken; `None` if the record lacks it.
    pub(super) fn get(&self, key: &str) -> Option<&'a Value> {
        let (i, (_, v)) = self.map.iter().enumerate().find(|(_, (k, _))| *k == key)?;
        self.taken.set(self.taken.get() | 1u128 << i);
        Some(v)
    }

    /// The field `key`, taken; an error if the record lacks it.
    pub(super) fn req(&self, key: &str) -> Res<&'a Value> {
        self.get(key).ok_or_else(|| self.path.err(format!("the field {key:?} is missing")))
    }

    /// Every entry not taken yet, in document order, now taken: for a record that keeps the keys
    /// it does not know (the settings' host keys).
    pub(super) fn rest(&self) -> Vec<(&'a str, &'a Value)> {
        let taken = self.taken.get();
        let out = self
            .map
            .iter()
            .enumerate()
            .filter(|(i, _)| taken & (1u128 << i) == 0)
            .map(|(_, (k, v))| (k.as_str(), v))
            .collect();
        let all = if self.map.len() == 128 { u128::MAX } else { (1u128 << self.map.len()) - 1 };
        self.taken.set(all);
        out
    }

    /// Checks that every key was taken: an unknown key is an error.
    pub(super) fn finish(&self) -> Res<()> {
        let taken = self.taken.get();
        match self.map.keys().enumerate().find(|(i, _)| taken & (1u128 << i) == 0) {
            Some((_, k)) => Err(self.path.field(k).err("a field the converter does not know")),
            None => Ok(()),
        }
    }

    /// A whole number; `default` if absent.
    pub(super) fn int<T: TryFrom<i64>>(&self, key: &str, default: T) -> Res<T> {
        self.get(key).map_or(Ok(default), |v| int(v, &self.at(key)))
    }

    /// A whole number that must be there.
    pub(super) fn int_req<T: TryFrom<i64>>(&self, key: &str) -> Res<T> {
        int(self.req(key)?, &self.at(key))
    }

    /// A whole number, or `None` if absent or null.
    pub(super) fn opt_int<T: TryFrom<i64>>(&self, key: &str) -> Res<Option<T>> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => int(v, &self.at(key)).map(Some),
        }
    }

    /// A number; `default` if absent.
    pub(super) fn real(&self, key: &str, default: f64) -> Res<f64> {
        self.get(key).map_or(Ok(default), |v| real(v, &self.at(key)))
    }

    /// A truth value; `default` if absent.
    pub(super) fn flag(&self, key: &str, default: bool) -> Res<bool> {
        self.get(key).map_or(Ok(default), |v| flag(v, &self.at(key)))
    }

    /// Text; `default` if absent.
    pub(super) fn text(&self, key: &str, default: &'a str) -> Res<&'a str> {
        self.get(key).map_or(Ok(default), |v| text(v, &self.at(key)))
    }

    /// Text that must be there.
    pub(super) fn text_req(&self, key: &str) -> Res<&'a str> {
        text(self.req(key)?, &self.at(key))
    }

    /// Text, or `None` if absent or null.
    pub(super) fn opt_text(&self, key: &str) -> Res<Option<&'a str>> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => text(v, &self.at(key)).map(Some),
        }
    }

    /// Each element of a list field, read by `f` at its place; empty if the field is absent or
    /// null (Python's `Optional[list]`).
    pub(super) fn each<T>(
        &self,
        key: &str,
        mut f: impl FnMut(&'a Value, &Path<'_>) -> Res<T>,
    ) -> Res<Vec<T>> {
        let at = self.at(key);
        match self.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(v) => list(v, &at)?.iter().enumerate().map(|(i, x)| f(x, &at.index(i))).collect(),
        }
    }

    /// Each entry of a dict field, read by `f` with its key at its place; empty if the field is
    /// absent or null.
    pub(super) fn entries<T>(
        &self,
        key: &str,
        f: impl FnMut(&'a str, &'a Value, &Path<'_>) -> Res<T>,
    ) -> Res<Vec<T>> {
        let at = self.at(key);
        match self.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(v) => dict(v, &at, f),
        }
    }
}

/// Each entry of the dict `v`, read by `f` with its key at its place.
pub(super) fn dict<'v, T>(
    v: &'v Value,
    p: &Path<'_>,
    mut f: impl FnMut(&'v str, &'v Value, &Path<'_>) -> Res<T>,
) -> Res<Vec<T>> {
    object(v, p)?.iter().map(|(k, x)| f(k, x, &p.key(k))).collect()
}

/// Where a JSON text stops parsing, as a path: Python writes `NaN` and `Infinity` into JSON,
/// which JSON has no numbers for, and the error should say where they are rather than at which
/// byte. `offset` is the byte serde stopped at.
pub(super) fn path_at(bytes: &[u8], offset: usize) -> String {
    #[derive(Debug)]
    enum Frame {
        /// In an object: the last key read, and whether a value follows it.
        Object(Option<String>),
        /// In a list: the index of the current element.
        List(usize),
    }
    let end = offset.min(bytes.len());
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    // Whether the next string in an object is a key.
    let mut want_key = false;
    while i < end {
        match bytes[i] {
            b'{' => {
                stack.push(Frame::Object(None));
                want_key = true;
            }
            b'[' => {
                stack.push(Frame::List(0));
                want_key = false;
            }
            b'}' | b']' => {
                stack.pop();
                want_key = false;
            }
            b',' => match stack.last_mut() {
                Some(Frame::List(n)) => *n += 1,
                Some(Frame::Object(_)) => want_key = true,
                None => {}
            },
            b':' => want_key = false,
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < end && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if want_key && let Some(Frame::Object(k)) = stack.last_mut() {
                    let raw = bytes.get(start..i.min(end)).unwrap_or_default();
                    *k = Some(String::from_utf8_lossy(raw).into_owned());
                }
            }
            _ => {}
        }
        i += 1;
    }
    let mut out = String::new();
    for f in &stack {
        match f {
            Frame::Object(Some(k)) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            Frame::Object(None) => {}
            Frame::List(n) => {
                let _ignored = write!(out, "[{n}]");
            }
        }
    }
    if out.is_empty() { "$".to_owned() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_render_in_the_path_grammar() {
        let root = Path::ROOT;
        let units = root.field("units");
        let unit = units.key("12");
        let promos = unit.field("promotions");
        assert_eq!(promos.index(3).render(), r#"units["12"].promotions[3]"#);
        assert_eq!(root.render(), "$");
        assert_eq!(root.field("turn").render(), "turn");
        assert_eq!(root.field("x").key("a\"b").render(), r#"x["a\"b"]"#);
    }

    #[test]
    fn numbers_are_read_as_python_wrote_them() {
        let p = Path::ROOT;
        assert_eq!(int::<i32>(&json!(50.0), &p), Ok(50));
        assert_eq!(int::<u8>(&json!(7), &p), Ok(7));
        assert!(int::<u8>(&json!(300), &p).is_err());
        assert!(int::<i32>(&json!(2.5), &p).is_err());
        assert!(int::<i32>(&json!("3"), &p).is_err());
        assert_eq!(real(&json!(3), &p), Ok(3.0));
        assert_eq!(key_int::<u32>("17", &p), Ok(17));
        assert!(key_int::<u32>("-1", &p).is_err());
    }

    #[test]
    fn a_record_refuses_a_key_nobody_took() {
        let v = json!({"a": 1, "b": true, "c": null});
        let o = Obj::new(&v, Path::ROOT.field("rec")).expect("a record");
        assert_eq!(o.int::<i32>("a", 0), Ok(1));
        assert_eq!(o.flag("b", false), Ok(true));
        let e = o.finish().expect_err("c was not taken");
        assert_eq!(e.path, "rec.c");
        assert_eq!(o.opt_int::<i32>("c"), Ok(None));
        assert_eq!(o.finish(), Ok(()));
        assert_eq!(o.int::<i32>("missing", 9), Ok(9));
        assert!(o.req("missing").is_err());
    }

    #[test]
    fn a_parse_error_is_placed_by_its_path() {
        let text = br#"{"players": [{"id": 0}, {"gold": NaN}]}"#;
        let err = serde_json::from_slice::<Value>(text).expect_err("NaN is not JSON");
        let offset = err.column().saturating_sub(1);
        assert_eq!(path_at(text, offset), "players[1].gold");
        assert_eq!(path_at(br#"{"a": {"b": [1, 2, Infinity"#, 26), "a.b[2]");
    }
}
