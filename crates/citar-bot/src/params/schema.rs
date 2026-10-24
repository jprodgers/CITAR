//! Each version's parameter schema (DESIGN.md P2.3.2): the JSON verbatim, which
//! `/api/bots/schema` serves and the bot's parameters are generated from, and its parameters as
//! data, which `cargo xtask gen-params` writes into `gen.rs` ([`SPECS`](super::SPECS)) for
//! [`clean`](crate::clean) and the resolution of names to read.
//!
//! `idle` has no parameters. `basic-1`'s schema is `params/basic-1.json`, which package 2-00b
//! exported from `PARAM_GROUPS` (basic.py:32-628) and which is the source of truth from then on.

use serde_json::Value;

use crate::versions::VersionId;

/// `idle`'s schema: no groups, as `profiles.schema("idle")` gave.
pub(crate) const IDLE: &str = r#"{"engine":"idle","groups":[]}"#;

/// `basic-1`'s schema, as the file has it.
pub(crate) const BASIC1: &str = include_str!("../../params/basic-1.json");

/// Version `v`'s schema.
pub(crate) const fn of(v: VersionId) -> &'static str {
    match v {
        VersionId::Idle => IDLE,
        VersionId::Basic1 => BASIC1,
    }
}

/// What a parameter takes: the schema's `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A whole number, `i32` in [`Params`](super::Params).
    Int,
    /// A number, `f64`.
    Float,
    /// On or off.
    Bool,
    /// One of the spec's `choices`, which may list `null`.
    Choice,
    /// Names in order of preference, among the spec's `options`; or a preset's name.
    Order,
    /// A set of names among the spec's `options`.
    List,
}

impl Kind {
    /// The schema's name for it, which `clean_params`' messages use: `int`, `float`, `bool`,
    /// `choice`, `order`, `list`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::Choice => "choice",
            Self::Order => "order",
            Self::List => "list",
        }
    }
}

/// A parameter's default, as the schema writes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fixed {
    Int(i32),
    Float(f64),
    Bool(bool),
    /// A choice's name, or the name of an order's preset; `None` for `null`.
    Name(Option<&'static str>),
    /// An order's or a list's names.
    Names(&'static [&'static str]),
}

impl Fixed {
    /// The default as JSON, as the schema has it.
    #[must_use]
    pub fn to_json(self) -> Value {
        match self {
            Self::Int(n) => Value::from(n),
            Self::Float(x) => Value::from(x),
            Self::Bool(b) => Value::Bool(b),
            Self::Name(n) => n.map_or(Value::Null, Value::from),
            Self::Names(names) => names.iter().map(|&n| Value::from(n)).collect(),
        }
    }
}

/// One parameter of `basic-1`'s schema: what [`clean`](crate::clean) checks a value against
/// and the names of an order or a list resolve from. The label and help for people stay in the
/// JSON, which is served verbatim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub key: &'static str,
    pub kind: Kind,
    /// Its label, which `clean_params`' messages name it by.
    pub label: &'static str,
    pub default: Fixed,
    /// A choice's choices, `None` for `null`; empty for any other kind.
    pub choices: &'static [Option<&'static str>],
    /// The names an order or a list may hold; empty for any other kind.
    pub options: &'static [&'static str],
    /// An order's named alternatives, in the schema's order; empty for any other kind.
    pub presets: &'static [(&'static str, &'static [&'static str])],
}

impl Spec {
    /// The preset called `name`.
    #[must_use]
    pub fn preset(&self, name: &str) -> Option<&'static [&'static str]> {
        self.presets.iter().find(|(n, _)| *n == name).map(|&(_, names)| names)
    }
}
