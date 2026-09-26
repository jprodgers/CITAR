//! Where a ruleset comes from, and what identifies it (DESIGN.md 5.2).
//!
//! The engine does no I/O, so a ruleset arrives as bytes: [`RulesetFiles`], the 24 files of
//! `citar/data/` by name. [`embedded`] compiles the shipped files in; a host that loads a modded
//! ruleset passes its own bytes. Python read the same files from disk (`rules.py:44-85`).
//!
//! A ruleset is identified by its [`RulesetId`], a blake3 over a canonical walk of the parsed
//! JSON rather than over the bytes: a CRLF checkout or a reformatted file gives the same id, and
//! any changed value a different one. Saves record it, and proof of work signs it with
//! [`BUILD_ID`].

use core::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::errors::{Problems, RulesetErrorKind};

/// The ruleset files, by the names [`RulesetFiles`] uses: the 22 UnCiv-derived tables
/// (`rules.py:49-82`).
pub const RULESET_FILES: [&str; 22] = [
    "ruleset/beliefs.json",
    "ruleset/buildings.json",
    "ruleset/city_state_types.json",
    "ruleset/difficulties.json",
    "ruleset/eras.json",
    "ruleset/global_uniques.json",
    "ruleset/improvements.json",
    "ruleset/nations.json",
    "ruleset/personalities.json",
    "ruleset/policies.json",
    "ruleset/promotions.json",
    "ruleset/quests.json",
    "ruleset/religions.json",
    "ruleset/resources.json",
    "ruleset/ruins.json",
    "ruleset/specialists.json",
    "ruleset/speeds.json",
    "ruleset/techs.json",
    "ruleset/terrains.json",
    "ruleset/unit_types.json",
    "ruleset/units.json",
    "ruleset/victories.json",
];

/// CITAR's own nations, merged into `ruleset/nations.json`. Optional, as in Python
/// (`rules.py:76-81`).
pub const CUSTOM_NATIONS: &str = "custom/nations.json";

/// CITAR's constants: formula constants, map sizes, lobby options (`rules.py:83`).
pub const GAME: &str = "game.json";

/// Every file a ruleset may have, required or not.
pub fn all_file_names() -> impl Iterator<Item = &'static str> {
    RULESET_FILES.into_iter().chain([CUSTOM_NATIONS, GAME])
}

/// The build the engine was compiled as, from `CITAR_BUILD_ID` at compile time; `dev` otherwise.
/// Proof of work signs it together with the [`RulesetId`].
pub const BUILD_ID: &str = match option_env!("CITAR_BUILD_ID") {
    Some(s) => s,
    None => "dev",
};

/// A ruleset's files as bytes, by name: `ruleset/techs.json`, `custom/nations.json`, `game.json`.
#[derive(Clone, Debug, Default)]
pub struct RulesetFiles<'a> {
    /// The files, in any order.
    pub files: Vec<(&'a str, &'a [u8])>,
}

impl<'a> RulesetFiles<'a> {
    /// The files given.
    #[must_use]
    pub fn new(files: Vec<(&'a str, &'a [u8])>) -> Self {
        Self { files }
    }

    /// The bytes of the file called `name`, if it was given.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        self.files.iter().find(|(n, _)| *n == name).map(|&(_, bytes)| bytes)
    }
}

/// The shipped ruleset, compiled in: `citar/data/{ruleset,custom,game.json}`.
#[cfg(feature = "embedded-ruleset")]
#[must_use]
pub fn embedded() -> RulesetFiles<'static> {
    macro_rules! files {
        ($($name:literal),* $(,)?) => {
            vec![$((
                $name,
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../citar/data/", $name))
                    .as_slice(),
            )),*]
        };
    }
    RulesetFiles::new(files![
        "ruleset/beliefs.json",
        "ruleset/buildings.json",
        "ruleset/city_state_types.json",
        "ruleset/difficulties.json",
        "ruleset/eras.json",
        "ruleset/global_uniques.json",
        "ruleset/improvements.json",
        "ruleset/nations.json",
        "ruleset/personalities.json",
        "ruleset/policies.json",
        "ruleset/promotions.json",
        "ruleset/quests.json",
        "ruleset/religions.json",
        "ruleset/resources.json",
        "ruleset/ruins.json",
        "ruleset/specialists.json",
        "ruleset/speeds.json",
        "ruleset/techs.json",
        "ruleset/terrains.json",
        "ruleset/unit_types.json",
        "ruleset/units.json",
        "ruleset/victories.json",
        "custom/nations.json",
        "game.json",
    ])
}

// ---- Parsing ----------------------------------------------------------------------------------

/// The parsed files, by their canonical names, in [`all_file_names`] order.
pub(crate) struct Docs {
    docs: Vec<(&'static str, Value)>,
}

impl Docs {
    /// The parsed file called `name`, if it was given.
    pub(crate) fn get(&self, name: &str) -> Option<&Value> {
        self.docs.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    /// Every parsed file with its name.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.docs.iter().map(|(n, v)| (*n, v))
    }

    /// The parsed files, by name.
    pub(crate) fn into_vec(self) -> Vec<(&'static str, Value)> {
        self.docs
    }
}

/// Matches the given files to the ruleset's and parses each, reporting missing, unknown and
/// repeated files and anything that is not strict JSON.
pub(crate) fn parse_files(files: &RulesetFiles<'_>, problems: &mut Problems) -> Docs {
    for (i, (name, _)) in files.files.iter().enumerate() {
        if !all_file_names().any(|known| known == *name) {
            problems.push(
                RulesetErrorKind::UnknownFile,
                name,
                "",
                format!(
                    "no ruleset file has this name; the files are {}",
                    all_file_names().collect::<Vec<_>>().join(", ")
                ),
            );
        } else if files.files[..i].iter().any(|(n, _)| n == name) {
            problems.push(RulesetErrorKind::DuplicateFile, name, "", "the file is given twice");
        }
    }
    let mut docs = Vec::new();
    for name in all_file_names() {
        let Some(bytes) = files.get(name) else {
            if name != CUSTOM_NATIONS {
                problems.push(RulesetErrorKind::MissingFile, name, "", "the file is missing");
            }
            continue;
        };
        match parse_strict(bytes) {
            Ok(v) => docs.push((name, v)),
            Err(e) => problems.push(RulesetErrorKind::Json, name, "", e.to_string()),
        }
    }
    Docs { docs }
}

/// Parses JSON as `serde_json::from_slice::<Value>` does, but refuses an object with the same
/// key twice, which Python's `json.load` and serde's `Value` both resolve by keeping the last one
/// silently: in a ruleset table that hides a whole object.
pub(crate) fn parse_strict(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let v = StrictValue.deserialize(&mut de)?;
    de.end()?;
    Ok(v)
}

/// Builds a `Value`, refusing duplicate keys. serde_json's own recursion limit bounds how deep
/// it goes.
struct StrictValue;

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v).map(Value::Number).ok_or_else(|| E::custom("a number is not finite"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(StrictValue)? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("the key {key:?} appears twice")));
            }
            let v = map.next_value_seed(StrictValue)?;
            out.insert(key, v);
        }
        Ok(Value::Object(out))
    }
}

// ---- RulesetId --------------------------------------------------------------------------------

/// What identifies a ruleset: blake3 over a canonical walk of its parsed files (DESIGN.md 5.2).
///
/// The walk takes the files in name order, object keys in document order, numbers tagged as an
/// unsigned, signed or floating value (the float by its bits) and strings with their length. So
/// line endings, indentation and how a number is spelt (`1e-5` or `0.00001`) do not change it,
/// and any value that does change does; `1` and `1.0` differ, as they do to Python. Hashing the
/// bytes instead would change with a CRLF checkout.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RulesetId(pub [u8; 32]);

impl RulesetId {
    /// The id as 64 lower-case hex digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0xf)]));
        }
        s
    }

    /// The id written as 64 hex digits, in either case.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, [hi, lo]) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let hi = char::from(*hi).to_digit(16)?;
            let lo = char::from(*lo).to_digit(16)?;
            // Two hex digits make one byte.
            out[i] = (hi * 16 + lo) as u8;
        }
        Some(Self(out))
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl fmt::Display for RulesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for RulesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RulesetId({})", self.to_hex())
    }
}

/// The id of these parsed files. The domain tag keeps it apart from any other blake3 the engine
/// takes; a change to the walk must change the tag.
pub(crate) fn ruleset_id(docs: &Docs) -> RulesetId {
    let mut files: Vec<(&str, &Value)> = docs.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));
    let mut h = blake3::Hasher::new();
    h.update(b"citar-ruleset-id-v1\0");
    h.update(&(files.len() as u64).to_le_bytes());
    for (name, value) in files {
        put_str(&mut h, name);
        walk(&mut h, value);
    }
    RulesetId(*h.finalize().as_bytes())
}

fn put_str(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// Feeds one value to the hasher without recursion, so no document can overflow the stack
/// (README rule 5), though serde_json's own depth limit already bounds it.
fn walk(h: &mut blake3::Hasher, root: &Value) {
    enum Item<'a> {
        Value(&'a Value),
        Key(&'a str),
    }
    let mut stack = vec![Item::Value(root)];
    while let Some(item) = stack.pop() {
        let v = match item {
            Item::Key(k) => {
                put_str(h, k);
                continue;
            }
            Item::Value(v) => v,
        };
        match v {
            Value::Null => {
                h.update(&[0]);
            }
            Value::Bool(b) => {
                h.update(&[1, u8::from(*b)]);
            }
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    h.update(&[2]);
                    h.update(&u.to_le_bytes());
                } else if let Some(i) = n.as_i64() {
                    h.update(&[3]);
                    h.update(&i.to_le_bytes());
                } else {
                    // serde_json holds only finite floats; the fallback cannot be reached.
                    let f = n.as_f64().unwrap_or(0.0);
                    h.update(&[4]);
                    h.update(&f.to_bits().to_le_bytes());
                }
            }
            Value::String(s) => {
                h.update(&[5]);
                put_str(h, s);
            }
            Value::Array(items) => {
                h.update(&[6]);
                h.update(&(items.len() as u64).to_le_bytes());
                stack.extend(items.iter().rev().map(Item::Value));
            }
            Value::Object(map) => {
                h.update(&[7]);
                h.update(&(map.len() as u64).to_le_bytes());
                for (k, v) in map.iter().rev() {
                    stack.push(Item::Value(v));
                    stack.push(Item::Key(k));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(files: &[(&str, &str)]) -> RulesetId {
        let docs = Docs {
            docs: files
                .iter()
                .map(|(n, t)| {
                    let name = all_file_names().find(|k| k == n).expect("a ruleset file name");
                    (name, parse_strict(t.as_bytes()).expect("JSON"))
                })
                .collect(),
        };
        ruleset_id(&docs)
    }

    #[test]
    fn duplicate_keys_are_refused() {
        assert!(parse_strict(br#"{"a": 1, "b": {"c": 2, "c": 3}}"#).is_err());
        assert!(parse_strict(br#"{"a": 1} x"#).is_err());
        let v = parse_strict(br#"{"b": [1, -2, 0.5, "x", null, true], "a": {}}"#).expect("JSON");
        assert_eq!(v, serde_json::json!({"b": [1, -2, 0.5, "x", null, true], "a": {}}));
        let keys: Vec<&String> = v.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["b", "a"], "document order is kept");
    }

    #[test]
    fn the_id_ignores_formatting_but_not_values() {
        let a = id_of(&[("game.json", r#"{"x": [1, 2.5, "é"], "y": 1e-5}"#)]);
        let b =
            id_of(&[("game.json", "{\r\n  \"x\" : [ 1 ,2.5,\"\\u00e9\" ],\r\n \"y\": 0.00001 }")]);
        assert_eq!(a, b);
        assert_ne!(a, id_of(&[("game.json", r#"{"x": [1, 2.5, "é"], "y": 2e-5}"#)]));
        assert_ne!(a, id_of(&[("game.json", r#"{"x": [1.0, 2.5, "é"], "y": 1e-5}"#)]));
        assert_ne!(a, id_of(&[("game.json", r#"{"y": 1e-5, "x": [1, 2.5, "é"]}"#)]));
        assert_ne!(a, id_of(&[("custom/nations.json", r#"{"x": [1, 2.5, "é"], "y": 1e-5}"#)]));
        // Strings carry their length, so moving text between neighbours changes the id.
        assert_ne!(
            id_of(&[("game.json", r#"["ab", "c"]"#)]),
            id_of(&[("game.json", r#"["a", "bc"]"#)])
        );
        // File order does not matter; the walk sorts by name.
        let two = [("game.json", "1"), ("ruleset/techs.json", "2")];
        let swapped = [("ruleset/techs.json", "2"), ("game.json", "1")];
        assert_eq!(id_of(&two), id_of(&swapped));
    }

    #[test]
    fn ids_read_back_from_hex() {
        let id = id_of(&[("game.json", "{}")]);
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(RulesetId::from_hex(&hex), Some(id));
        assert_eq!(RulesetId::from_hex(&hex.to_uppercase()), Some(id));
        assert_eq!(RulesetId::from_hex(&hex[1..]), None);
        assert_eq!(RulesetId::from_hex(&"g".repeat(64)), None);
    }

    #[test]
    fn missing_unknown_and_repeated_files_are_reported() {
        let files = RulesetFiles::new(vec![
            ("game.json", b"{}".as_slice()),
            ("game.json", b"{}".as_slice()),
            ("ruleset/tech.json", b"{}".as_slice()),
        ]);
        let mut problems = Problems::default();
        let docs = parse_files(&files, &mut problems);
        assert!(docs.get("game.json").is_some());
        let errs = problems.check().expect_err("problems");
        assert!(errs.has(RulesetErrorKind::DuplicateFile));
        assert!(errs.has(RulesetErrorKind::UnknownFile));
        let missing: Vec<&str> = errs
            .0
            .iter()
            .filter(|e| e.kind == RulesetErrorKind::MissingFile)
            .map(|e| &*e.file)
            .collect();
        assert_eq!(
            missing.len(),
            22,
            "every ruleset table is missing, custom nations are optional"
        );
    }
}
