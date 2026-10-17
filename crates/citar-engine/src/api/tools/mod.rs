//! The player tools as hosts call them by name (DESIGN.md 8.3).
//!
//! - [`registry`] (package 1d-01): the 61 tools, what a model reads about each and its JSON
//!   schema (`tools.py:15-68, 135-137`);
//! - [`args`]: each tool's parameters, their types and the required ones;
//! - [`normalize`](mod@normalize): a call's arguments coerced as `tools.execute` coerced them
//!   (`tools.py:113-127`), which both rule-script runners use from day one;
//! - [`execute`](mod@execute) (package 1d-01): `Game::execute`, a call by name with JSON
//!   arguments, checked, coerced and run as `tools.execute` ran it (`tools.py:84-132`);
//! - `query_tools` (package 1d-01): the query tools' answers (`tools.py:200-406`).

pub mod args;
pub mod execute;
pub mod normalize;
mod query_tools;
pub mod registry;

pub use self::args::{ArgType, Param, SchemaType, ToolArgs};
pub use self::execute::Executed;
pub use self::normalize::{normalize, normalize_with};
pub use self::registry::{Category, ToolKind, ToolSpec, kind, schemas, schemas_json};
