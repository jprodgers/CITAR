//! Protocol errors: a code and the path of the offending value (PROTOCOL.md, "Values and
//! checks"). The server's `helper_protocol.ProtocolError` carries the same two, and the invalid
//! fixtures hold both sides to them.

use std::fmt;

/// Why a frame was refused, in the order the checks run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    /// Over 8 MiB.
    TooLarge,
    /// Nested more than 64 deep.
    TooDeep,
    /// Not JSON.
    NotJson,
    /// JSON, but not an object.
    NotObject,
    /// `t` missing, not a string, or empty.
    NoType,
    /// A `hello` whose `protocol` is not exactly 2: the hub answers with the v1 refusal.
    Protocol,
    /// A required field is absent.
    Missing,
    /// A value of the wrong JSON type.
    Type,
    /// The right type, but out of range, too long, not an allowed string or not in its format.
    Value,
    /// A field a closed object does not have.
    Unknown,
}

impl ErrorCode {
    /// Every code, in the order the checks run.
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::TooLarge,
        ErrorCode::TooDeep,
        ErrorCode::NotJson,
        ErrorCode::NotObject,
        ErrorCode::NoType,
        ErrorCode::Protocol,
        ErrorCode::Missing,
        ErrorCode::Type,
        ErrorCode::Value,
        ErrorCode::Unknown,
    ];

    /// The code as PROTOCOL.md and the fixtures write it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::TooLarge => "too_large",
            ErrorCode::TooDeep => "too_deep",
            ErrorCode::NotJson => "not_json",
            ErrorCode::NotObject => "not_object",
            ErrorCode::NoType => "no_type",
            ErrorCode::Protocol => "protocol",
            ErrorCode::Missing => "missing",
            ErrorCode::Type => "type",
            ErrorCode::Value => "value",
            ErrorCode::Unknown => "unknown",
        }
    }

    /// The code PROTOCOL.md writes as `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A frame that cannot be accepted: the receiver logs it and drops it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code} at {}: {message}", if path.is_empty() { "the frame" } else { path.as_str() })]
pub struct ProtocolError {
    pub code: ErrorCode,
    /// The offending value's path from the frame's root (`machine.public_key`); `""` for the
    /// frame itself.
    pub path: String,
    /// For the log.
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, path: impl Into<String>, message: impl Into<String>) -> Self {
        ProtocolError { code, path: path.into(), message: message.into() }
    }
}
