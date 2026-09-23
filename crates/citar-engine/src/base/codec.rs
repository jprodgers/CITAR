//! Serde forms shared by the save and the digest that need no ruleset (DESIGN.md 4.9, 4.10).
//!
//! State is written two ways from one derive: the JSON save (human-readable) and `CANON_V1`, the
//! canonical binary encoding the digest hashes (not human-readable). The forms here tell the two
//! apart with `is_human_readable`, so the save reads well and stays small while the digest stays
//! fixed-width:
//! - [`pairs`]: a map as a list of `[key, value]` pairs, for maps whose keys JSON cannot write as
//!   strings (a production item, an optional pool). In `CANON_V1` a list of pairs and a map are
//!   the same bytes: a length, then each key and value in turn.
//! - [`bytes_b64`]: bytes as base64 text in JSON, as a length and the bytes in `CANON_V1`.
//! - [`hash_hex`]: a 32-byte hash as 64 hex digits in JSON, as the 32 bytes in `CANON_V1`.
//! - [`serde_by_name`]: a fieldless enum by its `name()` in JSON, by its index in `CANON_V1`.
//! - [`b64_encode`] and [`b64_decode`]: standard base64 with padding, decoded strictly.
//!
//! Rule ids, which the save writes as names, need the ruleset: `save::ctx` gives them their forms.
//!
//! Replaces nothing in Python, whose saves were `json.dumps` of the state's dicts.

use core::fmt;
use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, SerializeTuple, Serializer};

/// Bytes as standard base64, padded.
#[must_use]
pub fn b64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// The bytes of standard padded base64 text. Only the canonical text of some bytes is accepted,
/// so every byte string has exactly one spelling in a save.
pub fn b64_decode(text: &str) -> Result<Vec<u8>, String> {
    STANDARD.decode(text).map_err(|e| format!("not base64: {e}"))
}

/// How many bytes base64 text of this length holds at most, to refuse a column or a memory that
/// is too large before decoding it.
#[must_use]
pub const fn b64_decoded_len(text_len: usize) -> usize {
    text_len / 4 * 3
}

/// A `BTreeMap` as a list of `[key, value]` pairs: `#[serde(with = "crate::base::codec::pairs")]`.
///
/// JSON writes a map's keys as strings, which a production item or an optional id is not. Reading
/// refuses a key listed twice.
pub mod pairs {
    use super::{BTreeMap, Deserialize, Deserializer, Serialize, Serializer, de};

    /// Writes the map as pairs, in key order.
    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_seq(map.iter())
    }

    /// Reads the pairs back into a map.
    pub fn deserialize<'de, K, V, D>(d: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let pairs = Vec::<(K, V)>::deserialize(d)?;
        let n = pairs.len();
        let map: BTreeMap<K, V> = pairs.into_iter().collect();
        if map.len() != n {
            return Err(de::Error::custom("a key is listed twice"));
        }
        Ok(map)
    }
}

/// Bytes as base64 in JSON, as a length and the bytes in `CANON_V1`:
/// `#[serde(with = "crate::base::codec::bytes_b64")]` on a `Box<[u8]>`.
pub mod bytes_b64 {
    use super::{Deserialize, Deserializer, Serializer, b64_decode, b64_encode, de};

    /// Writes the bytes.
    pub fn serialize<T: AsRef<[u8]>, S: Serializer>(bytes: &T, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&b64_encode(bytes.as_ref()))
        } else {
            s.serialize_bytes(bytes.as_ref())
        }
    }

    /// Reads the bytes back.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Box<[u8]>, D::Error> {
        if d.is_human_readable() {
            let text = String::deserialize(d)?;
            b64_decode(&text).map(Vec::into_boxed_slice).map_err(de::Error::custom)
        } else {
            Vec::<u8>::deserialize(d).map(Vec::into_boxed_slice)
        }
    }
}

/// A 32-byte hash as 64 lower-case hex digits in JSON, as its 32 bytes (no length) in
/// `CANON_V1`: `#[serde(with = "crate::base::codec::hash_hex")]`.
pub mod hash_hex {
    use super::{Deserialize, Deserializer, SerializeTuple, Serializer, de};

    /// Writes the hash.
    pub fn serialize<S: Serializer>(hash: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            let mut text = String::with_capacity(64);
            for b in hash {
                text.push(char::from(HEX[usize::from(b >> 4)]));
                text.push(char::from(HEX[usize::from(b & 0xf)]));
            }
            s.serialize_str(&text)
        } else {
            let mut t = s.serialize_tuple(32)?;
            for b in hash {
                t.serialize_element(b)?;
            }
            t.end()
        }
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";

    /// Reads the hash back.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        if !d.is_human_readable() {
            return <[u8; 32]>::deserialize(d);
        }
        let text = String::deserialize(d)?;
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return Err(de::Error::custom(format!("a hash is 64 hex digits, not {text:?}")));
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut out = [0u8; 32];
        for (o, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
            match (digit(pair[0]), digit(pair[1])) {
                (Some(hi), Some(lo)) => *o = hi << 4 | lo,
                _ => {
                    return Err(de::Error::custom(format!(
                        "a hash is lower-case hex, not {text:?}"
                    )));
                }
            }
        }
        Ok(out)
    }
}

/// Reads a fieldless enum written by [`serde_by_name`]: its name in JSON, its index in
/// `CANON_V1`.
pub fn deserialize_by_name<'de, D, T>(
    d: D,
    what: &'static str,
    all: &'static [T],
    name: fn(T) -> &'static str,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Copy + 'static,
{
    struct ByName<T: 'static> {
        what: &'static str,
        all: &'static [T],
        name: fn(T) -> &'static str,
    }

    impl<T: Copy> Visitor<'_> for ByName<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "the name of a {}", self.what)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<T, E> {
            self.all.iter().copied().find(|&x| (self.name)(x) == v).ok_or_else(|| {
                let names: Vec<&str> = self.all.iter().map(|&x| (self.name)(x)).collect();
                E::custom(format!("{v:?} is no {}; one of {}", self.what, names.join(", ")))
            })
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<T, E> {
            usize::try_from(v)
                .ok()
                .and_then(|i| self.all.get(i).copied())
                .ok_or_else(|| E::custom(format!("{v} is no {} index", self.what)))
        }
    }

    let visitor = ByName { what, all, name };
    if d.is_human_readable() { d.deserialize_str(visitor) } else { d.deserialize_u32(visitor) }
}

/// Gives a fieldless enum with `ALL`, `name()` and declaration-order variants its save and
/// digest forms: the name in JSON, the variant's index in `CANON_V1`.
macro_rules! serde_by_name {
    ($t:ident) => {
        impl ::serde::Serialize for $t {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_unit_variant(stringify!($t), *self as u32, self.name())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $t {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                $crate::base::codec::deserialize_by_name(d, stringify!($t), &Self::ALL, Self::name)
            }
        }
    };
}

pub(crate) use serde_by_name;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::digest::to_canon_vec;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Colour {
        Red,
        DarkBlue,
    }

    impl Colour {
        const ALL: [Self; 2] = [Self::Red, Self::DarkBlue];

        const fn name(self) -> &'static str {
            match self {
                Self::Red => "red",
                Self::DarkBlue => "dark_blue",
            }
        }
    }

    serde_by_name!(Colour);

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Holder {
        #[serde(with = "pairs")]
        map: BTreeMap<(u8, u8), i32>,
        #[serde(with = "bytes_b64")]
        bytes: Box<[u8]>,
        #[serde(with = "hash_hex")]
        hash: [u8; 32],
        colour: Colour,
    }

    fn holder() -> Holder {
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[31] = 0x01;
        Holder {
            map: BTreeMap::from([((2, 1), -4), ((1, 9), 7)]),
            bytes: vec![0xff, 0, 7].into(),
            hash,
            colour: Colour::DarkBlue,
        }
    }

    #[test]
    fn json_forms_read_well_and_round_trip() -> Result<(), serde_json::Error> {
        let h = holder();
        let json = serde_json::to_string(&h)?;
        let hex = format!("ab{}01", "0".repeat(60));
        assert_eq!(
            json,
            format!(
                r#"{{"map":[[[1,9],7],[[2,1],-4]],"bytes":"/wAH","hash":"{hex}","colour":"dark_blue"}}"#
            )
        );
        assert_eq!(serde_json::from_str::<Holder>(&json)?, h);
        let twice = r#"{"map":[[[1,9],7],[[1,9],8]],"bytes":"","hash":"00","colour":"red"}"#;
        assert!(serde_json::from_str::<Holder>(twice).is_err());
        assert!(serde_json::from_str::<Colour>(r#""blue""#).is_err());
        Ok(())
    }

    #[test]
    fn canonical_forms_are_fixed() -> Result<(), crate::base::digest::CanonError> {
        let bytes = to_canon_vec(&holder())?;
        let mut want = vec![2, 0, 0, 0, 1, 9, 7, 0, 0, 0, 2, 1, 0xfc, 0xff, 0xff, 0xff];
        want.extend([3, 0, 0, 0, 0xff, 0, 7]);
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[31] = 1;
        want.extend(hash);
        want.extend([1, 0, 0, 0]);
        assert_eq!(bytes, want);
        Ok(())
    }

    #[test]
    fn base64_is_strict() {
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_decode("Zm9v"), Ok(b"foo".to_vec()));
        assert!(b64_decode("Zm9").is_err(), "unpadded");
        assert!(b64_decode("Zm9w=").is_err());
        assert_eq!(b64_decoded_len(8), 6);
    }
}
