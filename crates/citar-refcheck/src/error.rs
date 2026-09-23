//! The one error type of the tool: a message a person reads, with the file or setting it is about.

use std::fmt;

/// Something the tool could not do: a fixture or configuration file it could not read, or one
/// that breaks its format. Every such error ends a run with exit code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error { message: message.into() }
    }

    /// The same error, prefixed with where it happened (a file, an entry).
    #[must_use]
    pub fn context(self, context: impl fmt::Display) -> Self {
        Error { message: format!("{context}: {}", self.message) }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
