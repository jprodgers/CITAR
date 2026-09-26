//! The canonical binary encoding `CANON_V1`, and a writer that feeds it to a hash through a
//! block buffer (DESIGN.md 4.10).
//!
//! A game's digest hashes the canonical encoding of its state, so two targets that played the
//! same game agree on every bit, and proof of work can check one. [`CanonSerializer`] is a serde
//! `Serializer`, so the state's own derives drive it; it is not human-readable, which is how
//! types that save names (rule ids) know to write integers here instead.
//!
//! The encoding:
//! - `bool` as one byte, 0 or 1; integers little-endian at their declared width; `char` as a
//!   `u32`;
//! - floats as their bits, as they are, so `-0.0` and `0.0` differ; NaN or an infinity is an
//!   error, since NaN bit patterns differ between targets and state floats are finite anyway;
//! - strings and byte strings as a `u32` length, then the bytes;
//! - `Option` as a tag byte, 0 for `None` and 1 then the value for `Some`;
//! - enum variants as their `u32` index, then any fields;
//! - sequences and maps as a `u32` length, then the items (or key, value pairs); a sequence of
//!   unknown length, or one that yields a different number of items than it declared, is an
//!   error;
//! - structs and tuples as their fields in order, with no names and no count; unit types as
//!   nothing.
//!
//! Replaces nothing in Python, which had no digest.

use core::fmt;

use serde::ser::{self, Serialize};

/// The tag that goes into every digest ahead of the encoding, naming the encoding's version.
pub const CANON_V1: &[u8; 8] = b"CANON_V1";

/// The block size of the buffered writer.
pub const BLOCK_SIZE: usize = 64 * 1024;

/// Why a value has no canonical encoding.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CanonError {
    /// A NaN or an infinity: state floats must be finite.
    #[error("a float is {}; state floats must be finite", f64::from_bits(*.bits))]
    NonFinite {
        /// The float's bits, widened to `f64`.
        bits: u64,
    },
    /// A sequence or map that did not say how long it is.
    #[error("a sequence or map without a known length has no canonical encoding")]
    UnknownLength,
    /// A sequence or map that yielded a different number of items than it declared.
    #[error("a sequence or map declared {declared} items but gave {actual}")]
    LengthMismatch {
        /// What it declared.
        declared: usize,
        /// What it gave.
        actual: usize,
    },
    /// More items or bytes than a `u32` length can say.
    #[error("{0} items or bytes do not fit a u32 length")]
    TooLong(usize),
    /// An error raised by a type's own `Serialize` impl.
    #[error("{0}")]
    Custom(String),
}

impl ser::Error for CanonError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::Custom(msg.to_string())
    }
}

/// Where the encoded bytes go: a hash, or a vector for tests and tools.
pub trait CanonSink {
    /// Takes the next bytes.
    fn write(&mut self, bytes: &[u8]);
}

impl CanonSink for Vec<u8> {
    fn write(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

impl CanonSink for blake3::Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

impl<S: CanonSink + ?Sized> CanonSink for &mut S {
    fn write(&mut self, bytes: &[u8]) {
        (**self).write(bytes);
    }
}

/// Writes values in `CANON_V1` to a sink.
///
/// Buffered, it collects small writes into whole [`BLOCK_SIZE`] blocks before passing them on,
/// and passes a large byte slice (a tile array) straight through, so blake3 gets its multi-chunk
/// path instead of one tiny update per field. Unbuffered, every write goes straight to the sink.
/// Either way the sink sees the same bytes in the same order.
///
/// Call [`finish`](Self::finish) to flush the buffer and get the sink back.
pub struct CanonSerializer<S: CanonSink> {
    sink: S,
    buf: Vec<u8>,
    buffered: bool,
}

impl<S: CanonSink> CanonSerializer<S> {
    /// A writer with a fresh block buffer.
    pub fn buffered(sink: S) -> Self {
        Self::with_buffer(sink, Vec::with_capacity(BLOCK_SIZE))
    }

    /// A writer that reuses `buf` as its block buffer (it is cleared first), so a digest taken
    /// every round allocates nothing. [`finish_with_buffer`](Self::finish_with_buffer) hands it
    /// back.
    pub fn with_buffer(sink: S, mut buf: Vec<u8>) -> Self {
        buf.clear();
        buf.reserve(BLOCK_SIZE);
        Self { sink, buf, buffered: true }
    }

    /// A writer that passes every write straight to the sink.
    pub fn unbuffered(sink: S) -> Self {
        Self { sink, buf: Vec::new(), buffered: false }
    }

    /// Writes bytes as they are, with no length: for a caller's own framing around the encoding.
    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.put(bytes);
    }

    /// Flushes the buffer and returns the sink.
    pub fn finish(self) -> S {
        self.finish_with_buffer().0
    }

    /// Flushes the buffer and returns the sink and the (empty) buffer, for reuse.
    pub fn finish_with_buffer(mut self) -> (S, Vec<u8>) {
        self.flush();
        (self.sink, self.buf)
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            self.sink.write(&self.buf);
            self.buf.clear();
        }
    }

    #[inline]
    fn put(&mut self, bytes: &[u8]) {
        if !self.buffered {
            self.sink.write(bytes);
            return;
        }
        let room = BLOCK_SIZE - self.buf.len();
        if bytes.len() < room {
            self.buf.extend_from_slice(bytes);
            return;
        }
        if self.buf.is_empty() {
            // At least a whole block, and nothing waiting ahead of it: straight through.
            self.sink.write(bytes);
            return;
        }
        let (fill, rest) = bytes.split_at(room);
        self.buf.extend_from_slice(fill);
        self.flush();
        if rest.len() >= BLOCK_SIZE {
            self.sink.write(rest);
        } else {
            self.buf.extend_from_slice(rest);
        }
    }

    fn put_len(&mut self, n: usize) -> Result<(), CanonError> {
        let n = u32::try_from(n).map_err(|_| CanonError::TooLong(n))?;
        self.put(&n.to_le_bytes());
        Ok(())
    }

    fn put_f64(&mut self, v: f64) -> Result<(), CanonError> {
        if !v.is_finite() {
            return Err(CanonError::NonFinite { bits: v.to_bits() });
        }
        self.put(&v.to_bits().to_le_bytes());
        Ok(())
    }
}

/// The bytes of `value` in `CANON_V1`.
pub fn to_canon_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CanonError> {
    let mut ser = CanonSerializer::unbuffered(Vec::new());
    value.serialize(&mut ser)?;
    Ok(ser.finish())
}

/// blake3 of `domain` followed by the `CANON_V1` encoding of `value`, through the block buffer.
pub fn hash_canon<T: Serialize + ?Sized>(domain: &[u8], value: &T) -> Result<Digest, CanonError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    let mut ser = CanonSerializer::buffered(&mut hasher);
    value.serialize(&mut ser)?;
    ser.finish();
    Ok(Digest::from(hasher.finalize()))
}

/// A 32-byte blake3 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// The bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The digest written as 64 lower-case hex digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// Reads 64 hex digits (either case).
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.as_bytes();
        if s.len() != 64 {
            return None;
        }
        let nibble = |c: u8| (c as char).to_digit(16);
        let mut out = [0u8; 32];
        let (pairs, _) = s.as_chunks::<2>();
        for (byte, &[hi, lo]) in out.iter_mut().zip(pairs) {
            // Two hex digits make at most 255.
            *byte = (nibble(hi)? * 16 + nibble(lo)?) as u8;
        }
        Some(Self(out))
    }
}

impl From<blake3::Hash> for Digest {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

// ---- The serde Serializer ---------------------------------------------------------------------

macro_rules! put_le {
    ($($method:ident: $t:ty),*) => {$(
        #[inline]
        fn $method(self, v: $t) -> Result<(), CanonError> {
            self.put(&v.to_le_bytes());
            Ok(())
        }
    )*};
}

impl<'a, S: CanonSink> ser::Serializer for &'a mut CanonSerializer<S> {
    type Ok = ();
    type Error = CanonError;
    type SerializeSeq = Compound<'a, S>;
    type SerializeTuple = Compound<'a, S>;
    type SerializeTupleStruct = Compound<'a, S>;
    type SerializeTupleVariant = Compound<'a, S>;
    type SerializeMap = Compound<'a, S>;
    type SerializeStruct = Compound<'a, S>;
    type SerializeStructVariant = Compound<'a, S>;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<(), CanonError> {
        self.put(&[u8::from(v)]);
        Ok(())
    }

    put_le!(
        serialize_i8: i8, serialize_i16: i16, serialize_i32: i32, serialize_i64: i64,
        serialize_i128: i128, serialize_u8: u8, serialize_u16: u16, serialize_u32: u32,
        serialize_u64: u64, serialize_u128: u128
    );

    fn serialize_f32(self, v: f32) -> Result<(), CanonError> {
        if !v.is_finite() {
            return Err(CanonError::NonFinite { bits: f64::from(v).to_bits() });
        }
        self.put(&v.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_f64(self, v: f64) -> Result<(), CanonError> {
        self.put_f64(v)
    }

    fn serialize_char(self, v: char) -> Result<(), CanonError> {
        self.put(&u32::from(v).to_le_bytes());
        Ok(())
    }

    fn serialize_str(self, v: &str) -> Result<(), CanonError> {
        self.serialize_bytes(v.as_bytes())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<(), CanonError> {
        self.put_len(v.len())?;
        self.put(v);
        Ok(())
    }

    fn serialize_none(self) -> Result<(), CanonError> {
        self.put(&[0]);
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), CanonError> {
        self.put(&[1]);
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), CanonError> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), CanonError> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), CanonError> {
        self.put(&variant_index.to_le_bytes());
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), CanonError> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), CanonError> {
        self.put(&variant_index.to_le_bytes());
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Compound<'a, S>, CanonError> {
        let n = len.ok_or(CanonError::UnknownLength)?;
        self.put_len(n)?;
        Ok(Compound { ser: self, declared: Some(n), count: 0 })
    }

    fn serialize_tuple(self, _len: usize) -> Result<Compound<'a, S>, CanonError> {
        Ok(Compound { ser: self, declared: None, count: 0 })
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Compound<'a, S>, CanonError> {
        Ok(Compound { ser: self, declared: None, count: 0 })
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'a, S>, CanonError> {
        self.put(&variant_index.to_le_bytes());
        Ok(Compound { ser: self, declared: None, count: 0 })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Compound<'a, S>, CanonError> {
        let n = len.ok_or(CanonError::UnknownLength)?;
        self.put_len(n)?;
        Ok(Compound { ser: self, declared: Some(n), count: 0 })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Compound<'a, S>, CanonError> {
        Ok(Compound { ser: self, declared: None, count: 0 })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'a, S>, CanonError> {
        self.put(&variant_index.to_le_bytes());
        Ok(Compound { ser: self, declared: None, count: 0 })
    }
}

/// The state of a sequence, map, tuple or struct being written. For sequences and maps it
/// counts the items against the declared length.
pub struct Compound<'a, S: CanonSink> {
    ser: &'a mut CanonSerializer<S>,
    declared: Option<usize>,
    count: usize,
}

impl<S: CanonSink> Compound<'_, S> {
    fn end_counted(self) -> Result<(), CanonError> {
        match self.declared {
            Some(declared) if declared != self.count => {
                Err(CanonError::LengthMismatch { declared, actual: self.count })
            }
            _ => Ok(()),
        }
    }
}

impl<S: CanonSink> ser::SerializeSeq for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CanonError> {
        self.count += 1;
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        self.end_counted()
    }
}

impl<S: CanonSink> ser::SerializeTuple for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        Ok(())
    }
}

impl<S: CanonSink> ser::SerializeTupleStruct for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        Ok(())
    }
}

impl<S: CanonSink> ser::SerializeTupleVariant for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        Ok(())
    }
}

impl<S: CanonSink> ser::SerializeMap for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), CanonError> {
        self.count += 1;
        key.serialize(&mut *self.ser)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        self.end_counted()
    }
}

impl<S: CanonSink> ser::SerializeStruct for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        Ok(())
    }
}

impl<S: CanonSink> ser::SerializeStructVariant for Compound<'_, S> {
    type Ok = ();
    type Error = CanonError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), CanonError> {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), CanonError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    enum Kind {
        Plain,
        Wrapped(u8),
        Pair(u8, u16),
        Named { x: i8 },
    }

    #[derive(Serialize)]
    struct Unit;

    #[derive(Serialize)]
    struct Sample {
        a: bool,
        b: u8,
        c: i16,
        d: u32,
        e: f64,
        f: String,
        g: Option<u16>,
        h: Option<u8>,
        i: Vec<u8>,
        j: Kind,
        k: Kind,
        l: Kind,
        m: Kind,
        n: BTreeMap<u8, bool>,
        o: (u8, i64),
        p: Unit,
        q: char,
        r: f32,
    }

    fn sample() -> Sample {
        Sample {
            a: true,
            b: 7,
            c: -2,
            d: 0x0102_0304,
            e: 1.5,
            f: "ab".into(),
            g: Some(0x0201),
            h: None,
            i: vec![9, 8],
            j: Kind::Plain,
            k: Kind::Wrapped(5),
            l: Kind::Pair(1, 2),
            m: Kind::Named { x: -1 },
            n: BTreeMap::from([(3, true), (1, false)]),
            o: (4, -1),
            p: Unit,
            q: 'é',
            r: -0.5,
        }
    }

    /// The encoding of `sample()`, byte by byte from the rules in the module doc.
    const SAMPLE_BYTES: &[u8] = &[
        1, // a: true
        7, // b
        0xfe, 0xff, // c: -2 as i16
        0x04, 0x03, 0x02, 0x01, // d
        0, 0, 0, 0, 0, 0, 0xf8, 0x3f, // e: 1.5 = 0x3FF8000000000000
        2, 0, 0, 0, b'a', b'b', // f
        1, 0x01, 0x02, // g: Some(0x0201)
        0,    // h: None
        2, 0, 0, 0, 9, 8, // i
        0, 0, 0, 0, // j: variant 0
        1, 0, 0, 0, 5, // k: variant 1, 5
        2, 0, 0, 0, 1, 2, 0, // l: variant 2, 1u8, 2u16
        3, 0, 0, 0, 0xff, // m: variant 3, -1i8
        2, 0, 0, 0, 1, 0, 3, 1, // n: two entries, sorted by key
        4, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // o: (4, -1i64)
        // p: nothing
        0xe9, 0, 0, 0, // q: U+00E9
        0, 0, 0, 0xbf, // r: -0.5f32 = 0xBF000000
    ];

    #[test]
    fn known_answer_bytes() {
        assert_eq!(to_canon_vec(&sample()).expect("encodes"), SAMPLE_BYTES);
    }

    #[test]
    fn known_answer_digest() {
        let d = hash_canon(b"TEST", &sample()).expect("encodes");
        let mut h = blake3::Hasher::new();
        h.update(b"TEST");
        h.update(SAMPLE_BYTES);
        assert_eq!(d, Digest::from(h.finalize()));
        assert_eq!(d.to_hex(), KNOWN_DIGEST);
        assert_eq!(Digest::from_hex(KNOWN_DIGEST), Some(d));
    }

    /// blake3("TEST" ++ SAMPLE_BYTES), pinned so that a change of hash or encoding shows.
    const KNOWN_DIGEST: &str = "521b5b81379dabcb4c09c8acc7d197069c16e8eef180cd5e23e6830f54e4e62b";

    #[test]
    fn signed_zeros_differ() {
        let pos = hash_canon(b"", &0.0f64).expect("finite");
        let neg = hash_canon(b"", &-0.0f64).expect("finite");
        assert_ne!(pos, neg);
    }

    #[test]
    fn non_finite_floats_are_refused() {
        assert!(matches!(to_canon_vec(&f64::NAN), Err(CanonError::NonFinite { .. })));
        assert!(matches!(to_canon_vec(&[1.0, f64::INFINITY]), Err(CanonError::NonFinite { .. })));
        assert!(matches!(to_canon_vec(&f32::NEG_INFINITY), Err(CanonError::NonFinite { .. })));
    }

    struct Lying {
        declared: Option<usize>,
        items: usize,
    }

    impl Serialize for Lying {
        fn serialize<Ser: ser::Serializer>(&self, s: Ser) -> Result<Ser::Ok, Ser::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = s.serialize_seq(self.declared)?;
            for i in 0..self.items {
                seq.serialize_element(&(i as u8))?;
            }
            seq.end()
        }
    }

    #[test]
    fn lengths_must_be_known_and_true() {
        let unknown = Lying { declared: None, items: 1 };
        assert_eq!(to_canon_vec(&unknown), Err(CanonError::UnknownLength));
        let short = Lying { declared: Some(2), items: 1 };
        assert_eq!(
            to_canon_vec(&short),
            Err(CanonError::LengthMismatch { declared: 2, actual: 1 })
        );
    }

    /// Serialises as a byte string, which is how large arrays reach the writer in one piece.
    struct Bytes(Vec<u8>);

    impl Serialize for Bytes {
        fn serialize<Ser: ser::Serializer>(&self, s: Ser) -> Result<Ser::Ok, Ser::Error> {
            s.serialize_bytes(&self.0)
        }
    }

    #[test]
    fn buffered_and_unbuffered_write_the_same_bytes() {
        let sizes = [1usize, 3, 65_535, 65_536, 65_537, 7, 200_000, 0, 131_072, 5, 65_530, 11];
        let value: Vec<(u16, Bytes)> = sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| (i as u16, Bytes((0..n).map(|b| (b * 31 + i) as u8).collect())))
            .collect();

        let mut plain = CanonSerializer::unbuffered(Vec::new());
        value.serialize(&mut plain).expect("encodes");
        let plain = plain.finish();

        let mut blocked = CanonSerializer::buffered(Vec::new());
        value.serialize(&mut blocked).expect("encodes");
        let (blocked, buf) = blocked.finish_with_buffer();
        assert_eq!(plain, blocked);
        assert!(buf.is_empty());

        let mut reused = CanonSerializer::with_buffer(Vec::new(), buf);
        value.serialize(&mut reused).expect("encodes");
        assert_eq!(plain, reused.finish());

        assert_eq!(hash_canon(b"x", &value), {
            let mut h = blake3::Hasher::new();
            h.update(b"x");
            h.update(&plain);
            Ok(Digest::from(h.finalize()))
        });
    }

    /// Records the size of every write the sink receives.
    struct Recorder(Vec<usize>);

    impl CanonSink for Recorder {
        fn write(&mut self, bytes: &[u8]) {
            self.0.push(bytes.len());
        }
    }

    #[test]
    fn the_buffer_passes_whole_blocks_and_large_slices() {
        let mut ser = CanonSerializer::buffered(Recorder(Vec::new()));
        for _ in 0..70_000 {
            ser.write_raw(&[1]);
        }
        ser.write_raw(&vec![0; 3 * BLOCK_SIZE]);
        ser.write_raw(&[2; 10]);
        let writes = ser.finish().0;
        assert_eq!(writes[0], BLOCK_SIZE);
        assert!(writes[..writes.len() - 1].iter().all(|&n| n >= BLOCK_SIZE));
        assert_eq!(writes.iter().sum::<usize>(), 70_000 + 3 * BLOCK_SIZE + 10);
    }
}
