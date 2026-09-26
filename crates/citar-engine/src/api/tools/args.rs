//! The argument specs of the player tools: each tool's parameters, their JSON types, and which
//! are required (the `properties` and `required` of `tools.tool`, `tools.py:55-81`).
//!
//! [`normalize`](super::normalize()) coerces a call's arguments by its tool's spec, the registry
//! ([`super::registry`], package 1d-01) renders each tool's JSON schema from it, and the property
//! tests build arguments from it (DESIGN.md 9.5). The specs landed with the typed actions, each
//! system package adding its tools' parameters with the argument names Python's tools had
//! (DESIGN.md 3.4, rule 2); package 1d-01 moved them into the registry's one table, with the
//! query tools' and what a model reads about each parameter.

use super::registry;

/// A parameter's JSON type, which says how [`normalize`](super::normalize()) coerces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgType {
    /// Coerced with Python's `int()`.
    Integer,
    /// Taken as it is.
    Number,
    /// Taken as it is.
    String,
    /// A string is read as `true`, `1` or `yes`, in any case; anything else is taken as it is.
    Boolean,
    /// A string is split at commas into its trimmed, non-empty parts.
    Array,
    /// Taken as it is.
    Object,
    /// A parameter whose schema names no type: taken as it is.
    Any,
}

impl ArgType {
    /// The type a JSON schema's `type` names: `integer`, `number`, `string`, `boolean`, `array`
    /// or `object`; anything else, or none, is [`Any`](Self::Any).
    #[must_use]
    pub fn from_schema(name: Option<&str>) -> Self {
        match name {
            Some("integer") => Self::Integer,
            Some("number") => Self::Number,
            Some("string") => Self::String,
            Some("boolean") => Self::Boolean,
            Some("array") => Self::Array,
            Some("object") => Self::Object,
            _ => Self::Any,
        }
    }
}

/// A parameter's type as its JSON schema names it (`tools.INT`, `STR`, `BOOL`, `STRS`, `ITEMS`,
/// `tools.py:70-81`), which says more than how it is coerced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchemaType {
    /// `{"type": "integer"}`.
    Integer,
    /// `{"type": "number"}`.
    Number,
    /// `{"type": "string"}`.
    String,
    /// `{"type": "boolean"}`.
    Boolean,
    /// `{"type": "array", "items": {"type": "string"}}`.
    Strings,
    /// `{"type": "array", "items": {"type": "object"}}`.
    Objects,
    /// `{"type": "object"}`.
    Object,
    /// `{"type": ["integer", "string"]}`: a player id or a word, a city id or `hideout`.
    IntegerOrString,
    /// No type named.
    Any,
}

impl SchemaType {
    /// How a parameter of this type is coerced.
    #[must_use]
    pub const fn arg_type(self) -> ArgType {
        match self {
            Self::Integer => ArgType::Integer,
            Self::Number => ArgType::Number,
            Self::String => ArgType::String,
            Self::Boolean => ArgType::Boolean,
            Self::Strings | Self::Objects => ArgType::Array,
            Self::Object => ArgType::Object,
            Self::IntegerOrString | Self::Any => ArgType::Any,
        }
    }

    /// The schema type of a parameter known only by how it is coerced, as the coercion table of
    /// `tests/rules/normalize.json` gives its probes: an array is one of strings.
    #[must_use]
    pub const fn of(t: ArgType) -> Self {
        match t {
            ArgType::Integer => Self::Integer,
            ArgType::Number => Self::Number,
            ArgType::String => Self::String,
            ArgType::Boolean => Self::Boolean,
            ArgType::Array => Self::Strings,
            ArgType::Object => Self::Object,
            ArgType::Any => Self::Any,
        }
    }

    /// Whether a value, once coerced, is of this type as the schema states it. Only the typed
    /// actions read it: a refusal of an argument their fields could not take names what the
    /// parameter should have been.
    #[must_use]
    pub fn fits(self, v: &serde_json::Value) -> bool {
        use serde_json::Value as V;
        match self {
            Self::Integer => v.is_i64() || v.is_u64(),
            Self::Number => v.is_number(),
            Self::String => v.is_string(),
            Self::Boolean => v.is_boolean(),
            Self::Strings => v.as_array().is_some_and(|a| a.iter().all(V::is_string)),
            Self::Objects => v.as_array().is_some_and(|a| a.iter().all(V::is_object)),
            Self::Object => v.is_object(),
            Self::IntegerOrString => v.is_i64() || v.is_u64() || v.is_string(),
            Self::Any => true,
        }
    }

    /// What a value of this type is, for the refusal of one that is not.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Integer => "an integer",
            Self::Number => "a number",
            Self::String => "a string",
            Self::Boolean => "true or false",
            Self::Strings => "a list of strings",
            Self::Objects => "a list of objects",
            Self::Object => "an object",
            Self::IntegerOrString => "an integer or a string",
            Self::Any => "a JSON value",
        }
    }
}

/// One parameter of a tool: its name, its schema type, and what a model reads about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    /// The argument's name.
    pub name: &'static str,
    /// Its type, as the schema names it.
    pub json: SchemaType,
    /// The schema's `description`, if it has one.
    pub description: Option<&'static str>,
    /// The schema's `enum`: the values it takes, if it names them.
    pub choices: &'static [&'static str],
}

impl Param {
    /// A parameter of this schema type, with nothing more said about it.
    #[must_use]
    pub const fn new(name: &'static str, json: SchemaType) -> Self {
        Self { name, json, description: None, choices: &[] }
    }

    /// The parameter with its schema's description.
    #[must_use]
    pub const fn described(self, text: &'static str) -> Self {
        Self { description: Some(text), ..self }
    }

    /// The parameter with the values its schema allows.
    #[must_use]
    pub const fn one_of(self, choices: &'static [&'static str]) -> Self {
        Self { choices, ..self }
    }

    /// How the parameter is coerced.
    #[must_use]
    pub const fn ty(&self) -> ArgType {
        self.json.arg_type()
    }
}

/// An integer parameter (`tools.INT`).
#[must_use]
pub const fn int(name: &'static str) -> Param {
    Param::new(name, SchemaType::Integer)
}

/// A string parameter (`tools.STR`).
#[must_use]
pub const fn string(name: &'static str) -> Param {
    Param::new(name, SchemaType::String)
}

/// A boolean parameter (`tools.BOOL`).
#[must_use]
pub const fn boolean(name: &'static str) -> Param {
    Param::new(name, SchemaType::Boolean)
}

/// A list of strings (`tools.STRS`).
#[must_use]
pub const fn strings(name: &'static str) -> Param {
    Param::new(name, SchemaType::Strings)
}

/// An object.
#[must_use]
pub const fn object(name: &'static str) -> Param {
    Param::new(name, SchemaType::Object)
}

/// An integer or a string.
#[must_use]
pub const fn int_or_string(name: &'static str) -> Param {
    Param::new(name, SchemaType::IntegerOrString)
}

/// The column of a tile (`tools.XY`).
pub const X: Param = int("x").described("column");
/// The row of a tile (`tools.XY`).
pub const Y: Param = int("y").described("row");

/// What a deal's items look like, as models read it (`tools.ITEMS`, `tools.py:75-81`).
pub const ITEMS_DESCRIPTION: &str = "Deal items, e.g. [{\"type\":\"gold\",\"amount\":50}, \
    {\"type\":\"gold_per_turn\",\"amount\":3,\"turns\":30}, \
    {\"type\":\"resource\",\"resource\":\"Iron\",\"amount\":1,\"turns\":30}, \
    {\"type\":\"open_borders\",\"turns\":30}, {\"type\":\"embassy\"}, {\"type\":\"peace_treaty\"}, \
    {\"type\":\"declaration_of_friendship\"}, {\"type\":\"research_agreement\"}, \
    {\"type\":\"defensive_pact\"}, {\"type\":\"declare_war\",\"target\":3}, \
    {\"type\":\"city\",\"city_id\":12}, {\"type\":\"share_map\"}, \
    {\"type\":\"tech\",\"tech\":\"Writing\"}]";

/// A list of deal items (`tools.ITEMS`).
#[must_use]
pub const fn items(name: &'static str) -> Param {
    Param::new(name, SchemaType::Objects).described(ITEMS_DESCRIPTION)
}

/// One tool's arguments: its parameters in the order the tool declares them, which is the
/// order they are coerced in, and its required ones in the order a refusal names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolArgs {
    /// The tool's name.
    pub tool: &'static str,
    /// Each parameter, in the order the tool declares them.
    pub params: &'static [Param],
    /// The parameters a call must give.
    pub required: &'static [&'static str],
}

impl ToolArgs {
    /// The type of the parameter called `name`, if the tool has it.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<ArgType> {
        self.params.iter().find(|p| p.name == name).map(Param::ty)
    }
}

/// The spec of the tool called `name`, exactly: any of the registry's 61.
#[must_use]
pub fn spec(name: &str) -> Option<&'static ToolArgs> {
    registry::tool(name).map(|t| &t.args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_spec_says_what_it_takes() {
        for t in &registry::TOOLS {
            for r in t.args.required {
                assert!(t.args.param(r).is_some(), "{}: required {r} is not a parameter", t.name());
            }
            assert_eq!(spec(t.name()), Some(&t.args));
        }
        assert_eq!(ArgType::from_schema(Some("integer")), ArgType::Integer);
        assert_eq!(ArgType::from_schema(None), ArgType::Any);
        assert!(spec("no_such_tool").is_none());
    }

    #[test]
    fn schema_types_coerce_as_their_json_type_says() {
        for t in [
            ArgType::Integer,
            ArgType::Number,
            ArgType::String,
            ArgType::Boolean,
            ArgType::Array,
            ArgType::Object,
            ArgType::Any,
        ] {
            assert_eq!(SchemaType::of(t).arg_type(), t);
        }
        assert_eq!(SchemaType::IntegerOrString.arg_type(), ArgType::Any);
        assert!(SchemaType::Strings.fits(&json!(["a", "b"])));
        assert!(!SchemaType::Strings.fits(&json!(["a", 1])));
        assert!(SchemaType::IntegerOrString.fits(&json!("all")));
        assert!(!SchemaType::String.fits(&json!(5)));
        assert_eq!(X.description, Some("column"));
    }
}
