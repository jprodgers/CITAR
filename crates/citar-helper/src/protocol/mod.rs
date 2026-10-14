//! Protocol 2 between the helper and a CITAR server's hub: `PROTOCOL.md` is the normative text,
//! `protocol/v2/` its fixtures.
//!
//! - [`parse_server_frame`] and [`parse_helper_frame`] read a frame from either side: the size
//!   and depth caps, JSON, the type, then every field against PROTOCOL.md's tables ([`check`]),
//!   into the typed frames of [`frames`]. A type the receiver does not accept from that sender is
//!   [`Parsed::Ignored`], never an error.
//! - [`HelperFrame::encode`] and [`ServerFrame::encode`] write one, `t` and `ts` first.
//!
//! The server's side is `citar/server/helper_protocol.py`, written from the same text: the
//! fixtures hold both to the same frames, the same refusals and the same frame table. This
//! replaces protocol 1 (`citar/worker/protocol.py`), which 4-05 deletes with the Python worker.

pub mod check;
mod error;
pub mod frames;

use serde_json::{Map, Value};

pub use self::check::{Field, Path, Ty};
pub use self::error::{ErrorCode, ProtocolError};
pub use self::frames::*;

/// The protocol a helper speaks, `hello.protocol`.
pub const PROTOCOL: u32 = 2;

/// The largest frame either side accepts: 8 MiB of UTF-8.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// How deep arrays and objects may nest, the frame itself being depth 1.
pub const MAX_DEPTH: usize = 64;

/// The v1 refusal's message, word for word (PROTOCOL.md, "The v1 refusal").
pub const V1_REFUSAL_MESSAGE: &str = "This server runs CITAR 0.1.6, which needs the new CITAR \
    helper (protocol 2). Download it from the Servers page of this server, or from \
    https://github.com/jprodgers/CITAR/releases/latest, and run it with a token issued there.";

/// The codes the server closes the socket with (PROTOCOL.md, "Close codes").
pub mod close {
    /// The first frame was not a valid `hello`.
    pub const BAD_HELLO: u16 = 4400;
    /// After `refused` `token` or `key`.
    pub const REFUSED: u16 = 4401;
    /// After `shutdown` `revoked`, `disabled`, `deleted` or `suspended`.
    pub const SHUTDOWN: u16 = 4403;
    /// No `hello` within 30 s, or the connection went stale.
    pub const STALE: u16 = 4408;
    /// After `shutdown` `replaced`.
    pub const REPLACED: u16 = 4409;
    /// After the v1 refusal, or `refused` `protocol` or `build`.
    pub const PROTOCOL: u16 = 4426;
    /// After `refused` `rate`.
    pub const RATE: u16 = 4429;
}

/// Who sends a frame: the frame table's `From` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sender {
    Helper,
    Server,
}

impl Sender {
    /// As the frame table writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Sender::Helper => "helper",
            Sender::Server => "server",
        }
    }

    /// The other side.
    #[must_use]
    pub const fn other(self) -> Sender {
        match self {
            Sender::Helper => Sender::Server,
            Sender::Server => Sender::Helper,
        }
    }

    /// The frame types this side sends.
    #[must_use]
    pub const fn types(self) -> &'static [&'static str] {
        match self {
            Sender::Helper => <HelperFrame as FrameSet>::TYPES,
            Sender::Server => <ServerFrame as FrameSet>::TYPES,
        }
    }
}

/// The frames one side sends: [`HelperFrame`] or [`ServerFrame`].
pub trait FrameSet: Sized {
    /// Who sends them.
    const SENDER: Sender;
    /// Their types, as the wire writes them.
    const TYPES: &'static [&'static str];
    /// What the checks hold a frame of type `t` to, or `None` for a type this side never sends.
    fn ty_of(t: &str) -> Option<Ty>;
    /// The typed frame, from fields that passed the checks.
    #[doc(hidden)]
    fn read(t: &str, v: Value) -> Option<Result<Self, serde_json::Error>>;
    /// The frame's type, as the wire writes it.
    fn type_name(&self) -> &'static str;
    /// Its fields, without `t` and `ts`.
    #[doc(hidden)]
    fn fields(&self) -> Result<Value, serde_json::Error>;

    /// The frame as a JSON object: `t`, then `ts` when given, then its fields.
    fn to_value(&self, ts: Option<f64>) -> Value {
        let mut map = Map::new();
        map.insert("t".to_owned(), Value::from(self.type_name()));
        if let Some(ts) = ts {
            map.insert("ts".to_owned(), Value::from(ts));
        }
        if let Ok(Value::Object(fields)) = self.fields() {
            map.extend(fields);
        }
        Value::Object(map)
    }

    /// The frame's text, stamped with the sender's time `ts` (Unix seconds).
    fn encode(&self, ts: f64) -> String {
        self.to_value(Some(ts)).to_string()
    }
}

/// A frame read from the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed<F> {
    /// A frame of a type this side accepts, with its `ts` when it had one.
    Frame { ts: Option<f64>, frame: F },
    /// A type this side does not accept from that sender (a newer frame, or one of the other
    /// direction's): logged once per type, never answered.
    Ignored { t: String },
}

/// Reads a frame the server sent: what the helper receives.
///
/// # Errors
/// A [`ProtocolError`] with the code and path PROTOCOL.md gives for the defect.
pub fn parse_server_frame(text: &str) -> Result<Parsed<ServerFrame>, ProtocolError> {
    parse(text)
}

/// Reads a frame a helper sent: what the hub receives.
///
/// # Errors
/// A [`ProtocolError`] with the code and path PROTOCOL.md gives for the defect; for a `hello`
/// whose `protocol` is not exactly 2, [`ErrorCode::Protocol`], before any other field is read.
pub fn parse_helper_frame(text: &str) -> Result<Parsed<HelperFrame>, ProtocolError> {
    parse(text)
}

/// `ts`, a number like any other.
const TS: Ty = check::NUM;

/// Reads a frame of `F`'s side.
///
/// # Errors
/// As [`parse_helper_frame`].
pub fn parse<F: FrameSet>(text: &str) -> Result<Parsed<F>, ProtocolError> {
    let (map, t) = envelope(text)?;
    let Some(ty) = F::ty_of(&t) else { return Ok(Parsed::Ignored { t }) };
    if F::SENDER == Sender::Helper && t == "hello" {
        let protocol = map.get("protocol").and_then(check::as_integer);
        if protocol != Some(i128::from(PROTOCOL)) {
            return Err(ProtocolError::new(
                ErrorCode::Protocol,
                "protocol",
                "a hello that is not protocol 2: answered with the v1 refusal",
            ));
        }
    }
    let ts = match map.get("ts") {
        None => None,
        Some(v) => {
            check::check(&TS, v, &Path::Key(&Path::Root, "ts"))?;
            v.as_f64()
        }
    };
    let value = Value::Object(map);
    check::check(&ty, &value, &Path::Root)?;
    match F::read(&t, value) {
        Some(Ok(frame)) => Ok(Parsed::Frame { ts, frame }),
        // The checks and the serde types are written by one macro, so this is a bug here, never
        // the sender's: refused rather than panicking, and the fixtures' round trips catch it.
        Some(Err(e)) => Err(ProtocolError::new(
            ErrorCode::Value,
            "",
            format!("the frame passed its checks but did not read: {e}"),
        )),
        None => Ok(Parsed::Ignored { t }),
    }
}

/// The checks every frame passes before its type is known: its size, its depth, JSON, an object,
/// a type. Returns the object and its type.
///
/// # Errors
/// `too_large`, `too_deep`, `not_json`, `not_object` or `no_type`.
pub fn envelope(text: &str) -> Result<(Map<String, Value>, String), ProtocolError> {
    if text.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::new(
            ErrorCode::TooLarge,
            "",
            format!("{} bytes, over the {MAX_FRAME_BYTES}-byte cap", text.len()),
        ));
    }
    if nests_deeper_than(text.as_bytes(), MAX_DEPTH) {
        return Err(ProtocolError::new(
            ErrorCode::TooDeep,
            "",
            format!("nests more than {MAX_DEPTH} deep"),
        ));
    }
    let value: Value = serde_json::from_str(text)
        .map_err(|e| ProtocolError::new(ErrorCode::NotJson, "", e.to_string()))?;
    let Value::Object(map) = value else {
        return Err(ProtocolError::new(ErrorCode::NotObject, "", "not a JSON object"));
    };
    match map.get("t") {
        Some(Value::String(t)) if !t.is_empty() => {
            let t = t.clone();
            Ok((map, t))
        }
        _ => Err(ProtocolError::new(ErrorCode::NoType, "t", "no frame type")),
    }
}

/// Whether the text's arrays and objects nest deeper than `max`, counted outside strings. Run on
/// the raw text before parsing, so a deep frame costs one pass and no recursion.
#[must_use]
pub fn nests_deeper_than(text: &[u8], max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}
