//! The player tools as hosts call them by name (DESIGN.md 8.3).
//!
//! - [`args`]: each tool's argument spec, added by the package that ports its action;
//! - [`normalize`](mod@normalize): a call's arguments coerced as `tools.execute` coerced them
//!   (`tools.py:113-127`), which both rule-script runners use from day one.
//!
//! Package 1d-01 adds the JSON registry: the tool specs with their descriptions and schemas,
//! `execute`, and the query tools.

pub mod args;
pub mod normalize;

pub use self::args::{ArgType, ToolArgs};
pub use self::normalize::{normalize, normalize_with};
