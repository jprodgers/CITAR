//! The checks every received frame passes (PROTOCOL.md, "Values and checks"): a frame's fields are
//! described once, as [`Field`]s of a [`Ty`], beside the serde type that reads them
//! (`frames.rs`), and [`check`] walks a parsed JSON value against that description.
//!
//! The server runs the same checks in `citar/server/helper_protocol.py`, written from the same
//! text; the invalid fixtures hold the two to one code and one path for every defect. The order of
//! the checks only matters for a value with two defects, which no fixture has.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use super::error::{ErrorCode, ProtocolError};

/// The largest integer an `int` holds unless its range says otherwise: 2^53 − 1, which every
/// JSON reader keeps exact.
pub const INT_MAX: i64 = (1 << 53) - 1;
/// The largest `number` unless its range says otherwise.
pub const NUM_MAX: f64 = 1e15;
/// The longest `text`.
pub const TEXT_MAX: usize = 4000;
/// The longest id, prefix included.
pub const ID_MAX: usize = 64;

/// What a value must be: the types of PROTOCOL.md's field tables.
#[derive(Clone, Copy, Debug)]
pub enum Ty {
    /// A JSON integer within `lo..=hi`.
    Int {
        lo: i64,
        hi: i64,
    },
    /// A finite number within `lo..=hi`.
    Num {
        lo: f64,
        hi: f64,
    },
    Bool,
    /// A string of `min..=max` characters.
    Str {
        min: usize,
        max: usize,
    },
    /// Exactly this many lowercase hex digits.
    Hex(usize),
    /// Canonical standard base64 of exactly this many bytes.
    Base64(usize),
    /// A request or machine id: 1 to 64 characters of `A-Z a-z 0-9 _ -`.
    Id,
    /// A job id, `cj_` and 1 to 61 id characters.
    JobId,
    /// A run id, `cr_` and 1 to 61 id characters.
    RunId,
    /// One of these strings.
    OneOf(&'static [&'static str]),
    /// Exactly this integer.
    Exactly(i64),
    /// Any object, kept whole: data the protocol carries without reading.
    Object,
    /// An array of `min..=max` values of one type.
    List {
        of: &'static Ty,
        min: usize,
        max: usize,
    },
    /// A value of the type, or `null`.
    Nullable(&'static Ty),
    /// An object with these fields; others are dropped by the reader.
    Struct(&'static [Field]),
    /// An object with these fields and no other (an unknown field is refused).
    Closed(&'static [Field]),
    /// An object whose listed fields are checked and whose other fields are kept whole.
    Open(&'static [Field]),
}

/// One field of an object.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// Its name on the wire.
    pub name: &'static str,
    pub ty: Ty,
    /// It may be absent (when present it has its type: `null` is not absent).
    pub optional: bool,
}

/// A field before the frame macro names it after its Rust field.
#[derive(Clone, Copy, Debug)]
pub struct FieldSpec {
    wire: Option<&'static str>,
    ty: Ty,
    optional: bool,
}

/// A required field of this type.
pub const fn req(ty: Ty) -> FieldSpec {
    FieldSpec { wire: None, ty, optional: false }
}

/// An optional field of this type.
pub const fn opt(ty: Ty) -> FieldSpec {
    FieldSpec { wire: None, ty, optional: true }
}

impl FieldSpec {
    /// The field under a wire name other than its Rust name (`type`).
    pub const fn wire(self, name: &'static str) -> Self {
        FieldSpec { wire: Some(name), ..self }
    }

    /// The field, named `rust_name` unless [`wire`](Self::wire) gave it another name.
    pub const fn named(self, rust_name: &'static str) -> Field {
        let name = match self.wire {
            Some(w) => w,
            None => rust_name,
        };
        Field { name, ty: self.ty, optional: self.optional }
    }
}

/// `int lo..hi`.
pub const fn int(lo: i64, hi: i64) -> Ty {
    Ty::Int { lo, hi }
}

/// `int`: 0 to 2^53 − 1.
pub const INT: Ty = Ty::Int { lo: 0, hi: INT_MAX };
/// `number`: 0 to 10^15.
pub const NUM: Ty = Ty::Num { lo: 0.0, hi: NUM_MAX };
/// `text`.
pub const TEXT: Ty = Ty::Str { min: 0, max: TEXT_MAX };

/// `string min..max`.
pub const fn string(min: usize, max: usize) -> Ty {
    Ty::Str { min, max }
}

/// Where a value is, from the frame's root: rendered only when a check fails, so walking a large
/// frame allocates nothing.
#[derive(Clone, Copy, Debug)]
pub enum Path<'a> {
    Root,
    Key(&'a Path<'a>, &'a str),
    Index(&'a Path<'a>, usize),
}

impl Path<'_> {
    /// `machine.public_key`, `llm.models.0.key`; the root is `""`.
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        let mut at = self;
        loop {
            match at {
                Path::Root => break,
                Path::Key(up, k) => {
                    parts.push((*k).to_owned());
                    at = up;
                }
                Path::Index(up, i) => {
                    parts.push(i.to_string());
                    at = up;
                }
            }
        }
        parts.reverse();
        parts.join(".")
    }
}

fn fail(code: ErrorCode, path: &Path<'_>, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, path.render(), message)
}

/// The JSON type of a value, for messages.
fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_f64() => "a float",
        Value::Number(_) => "an integer",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The integer a JSON value holds, if it is one: a literal integer within −2^63 .. 2^64 − 1
/// (serde_json reads any other number, `-0` included, as a float).
pub fn as_integer(v: &Value) -> Option<i128> {
    let Value::Number(n) = v else { return None };
    n.as_u64().map(i128::from).or_else(|| n.as_i64().map(i128::from))
}

fn id_chars(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn prefixed_id(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && s.len() <= ID_MAX && id_chars(rest))
}

/// Canonical standard base64 of exactly `n` bytes.
pub fn is_base64(s: &str, n: usize) -> bool {
    STANDARD.decode(s).is_ok_and(|bytes| bytes.len() == n && STANDARD.encode(&bytes) == s)
}

/// Exactly `n` lowercase hex digits.
pub fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Checks `v` against `ty`.
///
/// # Errors
/// The first defect found, with its code and path.
pub fn check(ty: &Ty, v: &Value, path: &Path<'_>) -> Result<(), ProtocolError> {
    match *ty {
        Ty::Int { lo, hi } => {
            let Some(n) = as_integer(v) else {
                return Err(fail(
                    ErrorCode::Type,
                    path,
                    format!("expected an int, found {}", kind_of(v)),
                ));
            };
            if n < i128::from(lo) || n > i128::from(hi) {
                return Err(fail(ErrorCode::Value, path, format!("{n} is not within {lo}..{hi}")));
            }
        }
        Ty::Num { lo, hi } => {
            let Some(x) = v.as_f64() else {
                return Err(fail(
                    ErrorCode::Type,
                    path,
                    format!("expected a number, found {}", kind_of(v)),
                ));
            };
            if !(x >= lo && x <= hi) {
                return Err(fail(ErrorCode::Value, path, format!("{x} is not within {lo}..{hi}")));
            }
        }
        Ty::Bool => {
            if !v.is_boolean() {
                return Err(fail(
                    ErrorCode::Type,
                    path,
                    format!("expected a boolean, found {}", kind_of(v)),
                ));
            }
        }
        Ty::Str { min, max } => {
            let s = string_of(v, path)?;
            let len = s.chars().count();
            if len < min || len > max {
                return Err(fail(
                    ErrorCode::Value,
                    path,
                    format!("a string of {len} characters, not {min} to {max}"),
                ));
            }
        }
        Ty::Hex(n) => {
            if !is_hex(string_of(v, path)?, n) {
                return Err(fail(ErrorCode::Value, path, format!("not {n} lowercase hex digits")));
            }
        }
        Ty::Base64(n) => {
            if !is_base64(string_of(v, path)?, n) {
                return Err(fail(
                    ErrorCode::Value,
                    path,
                    format!("not standard base64 of {n} bytes"),
                ));
            }
        }
        Ty::Id => {
            let s = string_of(v, path)?;
            if s.is_empty() || s.len() > ID_MAX || !id_chars(s) {
                return Err(fail(ErrorCode::Value, path, "not an id (1 to 64 of A-Z a-z 0-9 _ -)"));
            }
        }
        Ty::JobId => {
            if !prefixed_id(string_of(v, path)?, "cj_") {
                return Err(fail(ErrorCode::Value, path, "not a job id (cj_ and an id)"));
            }
        }
        Ty::RunId => {
            if !prefixed_id(string_of(v, path)?, "cr_") {
                return Err(fail(ErrorCode::Value, path, "not a run id (cr_ and an id)"));
            }
        }
        Ty::OneOf(allowed) => {
            let s = string_of(v, path)?;
            if !allowed.contains(&s) {
                return Err(fail(
                    ErrorCode::Value,
                    path,
                    format!("{s:?} is not one of {}", allowed.join(", ")),
                ));
            }
        }
        Ty::Exactly(k) => {
            if as_integer(v) != Some(i128::from(k)) {
                let code = if v.is_number() { ErrorCode::Value } else { ErrorCode::Type };
                return Err(fail(code, path, format!("expected exactly {k}")));
            }
        }
        Ty::Object => {
            if !v.is_object() {
                return Err(fail(
                    ErrorCode::Type,
                    path,
                    format!("expected an object, found {}", kind_of(v)),
                ));
            }
        }
        Ty::List { of, min, max } => {
            let Value::Array(items) = v else {
                return Err(fail(
                    ErrorCode::Type,
                    path,
                    format!("expected an array, found {}", kind_of(v)),
                ));
            };
            if items.len() < min || items.len() > max {
                return Err(fail(
                    ErrorCode::Value,
                    path,
                    format!("{} items, not {min} to {max}", items.len()),
                ));
            }
            for (i, item) in items.iter().enumerate() {
                check(of, item, &Path::Index(path, i))?;
            }
        }
        Ty::Nullable(inner) => {
            if !v.is_null() {
                check(inner, v, path)?;
            }
        }
        Ty::Struct(fields) | Ty::Open(fields) => fields_of(fields, object_of(v, path)?, path)?,
        Ty::Closed(fields) => {
            let map = object_of(v, path)?;
            fields_of(fields, map, path)?;
            if let Some(key) = map.keys().find(|k| !fields.iter().any(|f| f.name == k.as_str())) {
                return Err(fail(
                    ErrorCode::Unknown,
                    &Path::Key(path, key),
                    "a field this object does not have",
                ));
            }
        }
    }
    Ok(())
}

fn string_of<'v>(v: &'v Value, path: &Path<'_>) -> Result<&'v str, ProtocolError> {
    v.as_str().ok_or_else(|| {
        fail(ErrorCode::Type, path, format!("expected a string, found {}", kind_of(v)))
    })
}

fn object_of<'v>(v: &'v Value, path: &Path<'_>) -> Result<&'v Map<String, Value>, ProtocolError> {
    v.as_object().ok_or_else(|| {
        fail(ErrorCode::Type, path, format!("expected an object, found {}", kind_of(v)))
    })
}

/// Each field of `fields` in `map`: present unless optional, `null` only where nullable.
pub fn fields_of(
    fields: &[Field],
    map: &Map<String, Value>,
    path: &Path<'_>,
) -> Result<(), ProtocolError> {
    for field in fields {
        let at = Path::Key(path, field.name);
        match map.get(field.name) {
            None if field.optional => {}
            None => return Err(fail(ErrorCode::Missing, &at, "a required field is absent")),
            Some(Value::Null) if !matches!(field.ty, Ty::Nullable(_)) => {
                return Err(fail(ErrorCode::Type, &at, "null where it is not allowed"));
            }
            Some(v) => check(&field.ty, v, &at)?,
        }
    }
    Ok(())
}

/// What a type is, as JSON: `protocol/v2/schema.json` holds every frame's description, and both
/// sides' tests compare theirs with it, so a bound written differently on one side is found even
/// where no fixture reaches it. Unbounded string lengths are `null`.
#[must_use]
pub fn describe(ty: &Ty) -> Value {
    use serde_json::json;
    let fields = |fields: &[Field]| -> Value {
        Value::Array(fields.iter().map(|f| json!([f.name, describe(&f.ty), f.optional])).collect())
    };
    match *ty {
        Ty::Int { lo, hi } => json!({"int": [lo, hi]}),
        Ty::Num { lo, hi } => json!({"number": [lo, hi]}),
        Ty::Bool => json!("bool"),
        Ty::Str { min, max } => json!({"string": [min, (max != usize::MAX).then_some(max)]}),
        Ty::Hex(n) => json!({"hex": n}),
        Ty::Base64(n) => json!({"base64": n}),
        Ty::Id => json!("id"),
        Ty::JobId => json!("job"),
        Ty::RunId => json!("run"),
        Ty::OneOf(values) => json!({"one_of": values}),
        Ty::Exactly(k) => json!({"exactly": k}),
        Ty::Object => json!("object"),
        Ty::List { of, min, max } => json!({"list": describe(of), "min": min, "max": max}),
        Ty::Nullable(inner) => json!({"nullable": describe(inner)}),
        Ty::Struct(f) => json!({"struct": fields(f)}),
        Ty::Closed(f) => json!({"closed": fields(f)}),
        Ty::Open(f) => json!({"open": fields(f)}),
    }
}

/// Removes, in place, the fields the protocol's own objects do not have, the way the serde types
/// ignore them: what a reader keeps of a frame. `object` values and the unlisted fields of `Open`
/// objects are kept whole.
pub fn keep_known(ty: &Ty, v: &mut Value) {
    match (ty, v) {
        (Ty::Struct(fields) | Ty::Closed(fields), Value::Object(map)) => {
            map.retain(|k, _| fields.iter().any(|f| f.name == k.as_str()));
            known_fields(fields, map);
        }
        (Ty::Open(fields), Value::Object(map)) => known_fields(fields, map),
        (Ty::List { of, .. }, Value::Array(items)) => {
            for item in items {
                keep_known(of, item);
            }
        }
        (Ty::Nullable(inner), v) => keep_known(inner, v),
        _ => {}
    }
}

fn known_fields(fields: &[Field], map: &mut Map<String, Value>) {
    for field in fields {
        if let Some(v) = map.get_mut(field.name) {
            keep_known(&field.ty, v);
        }
    }
}
