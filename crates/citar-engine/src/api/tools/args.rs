//! The argument specs of the player tools: each tool's parameters, their JSON types, and which
//! are required (the `properties` and `required` of `tools.tool`, `tools.py:55-68`).
//!
//! [`normalize`](super::normalize()) coerces a call's arguments by its tool's spec, and later the
//! JSON registry (package 1d-01) builds each tool's schema from it and the property tests build
//! arguments from it (DESIGN.md 9.5). The specs land with the typed actions: each system package
//! adds its tools' entries to [`TOOLS`] together with their `Action` variants, with the
//! argument names Python's tools had (DESIGN.md 3.4, rule 2). Package 1b-02 lands the form and
//! the coercion; package 1b-06 adds the citizen tools, package 1b-07 the tools of production,
//! purchases, research and policies, and package 1c-02 the unit tools.

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

/// One tool's arguments: its parameters in the order the tool declares them, which is the
/// order they are coerced in, and its required ones in the order a refusal names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolArgs {
    /// The tool's name.
    pub tool: &'static str,
    /// Each parameter's name and type.
    pub params: &'static [(&'static str, ArgType)],
    /// The parameters a call must give.
    pub required: &'static [&'static str],
}

impl ToolArgs {
    /// The type of the parameter called `name`, if the tool has it.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<ArgType> {
        self.params.iter().find(|(n, _)| *n == name).map(|&(_, t)| t)
    }
}

/// Every tool whose action is ported, sorted by name.
pub static TOOLS: &[ToolArgs] = &[
    // Package 1b-07 (tools.py:631-676, 760-795, 833-869).
    ToolArgs {
        tool: "adopt_policy",
        params: &[("policy", ArgType::String)],
        required: &["policy"],
    },
    ToolArgs {
        tool: "buy",
        params: &[
            ("city_id", ArgType::Integer),
            ("item", ArgType::String),
            ("currency", ArgType::String),
        ],
        required: &["city_id", "item"],
    },
    ToolArgs {
        tool: "buy_tile",
        params: &[("city_id", ArgType::Integer), ("x", ArgType::Integer), ("y", ArgType::Integer)],
        required: &["city_id", "x", "y"],
    },
    ToolArgs {
        tool: "change_queue",
        params: &[
            ("city_id", ArgType::Integer),
            ("index", ArgType::Integer),
            ("action", ArgType::String),
        ],
        required: &["city_id", "action"],
    },
    ToolArgs {
        tool: "choose_free_tech",
        params: &[("tech", ArgType::String)],
        required: &["tech"],
    },
    ToolArgs {
        tool: "dequeue_research",
        params: &[("tech", ArgType::String)],
        required: &["tech"],
    },
    ToolArgs {
        tool: "move_unit",
        params: &[("unit_id", ArgType::Integer), ("x", ArgType::Integer), ("y", ArgType::Integer)],
        required: &["unit_id", "x", "y"],
    },
    ToolArgs {
        tool: "promote_unit",
        params: &[("unit_id", ArgType::Integer), ("promotion", ArgType::String)],
        required: &["unit_id", "promotion"],
    },
    ToolArgs {
        tool: "rename_city",
        params: &[("city_id", ArgType::Integer), ("name", ArgType::String)],
        required: &["city_id", "name"],
    },
    ToolArgs {
        tool: "set_auto_production",
        params: &[("city_id", ArgType::Integer), ("enabled", ArgType::Boolean)],
        required: &["city_id", "enabled"],
    },
    // Package 1b-06 (tools.py:679-757).
    ToolArgs {
        tool: "set_city_focus",
        params: &[
            ("city_id", ArgType::Integer),
            ("focus", ArgType::String),
            ("avoid_growth", ArgType::Boolean),
        ],
        required: &["city_id"],
    },
    ToolArgs {
        tool: "set_production",
        params: &[
            ("city_id", ArgType::Integer),
            ("item", ArgType::String),
            ("append", ArgType::Boolean),
        ],
        required: &["city_id", "item"],
    },
    ToolArgs {
        tool: "set_research",
        params: &[("tech", ArgType::String), ("append", ArgType::Boolean)],
        required: &["tech"],
    },
    ToolArgs {
        tool: "set_specialists",
        params: &[("city_id", ArgType::Integer), ("specialists", ArgType::Object)],
        required: &["city_id", "specialists"],
    },
    ToolArgs {
        tool: "unit_order",
        params: &[("unit_id", ArgType::Integer), ("order", ArgType::String)],
        required: &["unit_id", "order"],
    },
    ToolArgs {
        tool: "upgrade_unit",
        params: &[("unit_id", ArgType::Integer)],
        required: &["unit_id"],
    },
    ToolArgs {
        tool: "work_tile",
        params: &[
            ("city_id", ArgType::Integer),
            ("x", ArgType::Integer),
            ("y", ArgType::Integer),
            ("locked", ArgType::Boolean),
        ],
        required: &["city_id", "x", "y"],
    },
];

/// The spec of the tool called `name`, exactly.
#[must_use]
pub fn spec(name: &str) -> Option<&'static ToolArgs> {
    TOOLS.binary_search_by(|t| t.tool.cmp(name)).ok().map(|i| &TOOLS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_says_what_it_takes() {
        assert!(TOOLS.windows(2).all(|w| w[0].tool < w[1].tool), "binary search needs order");
        for t in TOOLS {
            for r in t.required {
                assert!(t.param(r).is_some(), "{}: required {r} is not a parameter", t.tool);
            }
        }
        assert_eq!(ArgType::from_schema(Some("integer")), ArgType::Integer);
        assert_eq!(ArgType::from_schema(None), ArgType::Any);
        assert!(spec("no_such_tool").is_none());
    }
}
