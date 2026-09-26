//! A tool called by name with JSON arguments, as every host calls one (`tools.execute`,
//! `tools.py:84-132`; DESIGN.md 8.1, 8.3).
//!
//! The browser, the REST API, the MCP server and the model adapters all go through
//! [`Game::execute`], so a person clicking and a model calling a tool meet the same rules. In
//! Python's order:
//! 1. the tool exists;
//! 2. the caller may use it: a major civilization of the game, and for an action a living one,
//!    in a game that goes on, on its own turn unless the tool may be used at any time
//!    ([`Game::guard`]). A caller who may not act is refused before its arguments are read;
//! 3. the arguments are coerced ([`super::normalize_with`]): the missing required ones are
//!    reported first, unknown keys are dropped, and numbers, flags and lists sent as text are
//!    read as Python read them;
//! 4. a query is answered from the game as it is; an action's arguments become its typed
//!    [`Action`], which [`Game::act`] checks, applies, settles and logs.
//!
//! Python's `execute` also wrote the action to the log and saved the random state; here `act`
//! logs every action, whoever calls it, and the random state is keyed, so there is nothing to
//! save.
//!
//! One difference, on purpose (`tool-arguments-of-the-wrong-type-refused`): an argument an
//! action cannot read as its type (a number for a unit's order) is refused with a sentence that
//! names the parameter and what it takes, where Python's tool raised a Python exception.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::normalize::normalize_with;
use super::query_tools;
use super::registry::{self, Op, Query, ToolKind, ToolSpec};
use crate::base::ids::PlayerId;
use crate::base::text::echo;
use crate::game::error::{ActionError, ErrCode};
use crate::game::{Action, EventBatch, Game, Outcome};

/// What a tool call returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Executed {
    /// Whether the tool was a query or an action.
    pub kind: ToolKind,
    /// The tool's result, as the caller reads it.
    pub result: Outcome,
    /// The events the call appended: none for a query.
    pub events: EventBatch,
}

impl Game {
    /// Calls the tool `tool` for player `pid` with the arguments `args`, a JSON object (DESIGN.md
    /// 8.1; `tools.execute`, `tools.py:84-132`): the checks in Python's order, then the query's
    /// answer, or the action through [`Game::act`], with the events it appended.
    ///
    /// # Errors
    /// An unknown tool, a caller who may not act, an argument missing or of the wrong type, or
    /// whatever the tool's own rule refuses. A refusal changes nothing (property P2).
    pub fn execute(
        &mut self,
        pid: PlayerId,
        tool: &str,
        args: &Value,
    ) -> Result<Executed, ActionError> {
        let spec = find(self, tool)?;
        match spec.op {
            Op::Query(q) => {
                let result = answer(self, pid, spec, q, args)?;
                Ok(Executed { kind: ToolKind::Query, result, events: EventBatch::default() })
            }
            Op::Action => {
                self.guard(pid, spec.any_time)?;
                let action = action_of(spec, normalize_with(&spec.args, args)?)?;
                let (result, events) = self.act(pid, action)?;
                Ok(Executed { kind: ToolKind::Action, result, events })
            }
        }
    }

    /// Answers the query tool `tool` for player `pid` without a mutable borrow: what
    /// [`Game::execute`] does for a query, for a host that reads under a shared lock. A query
    /// changes nothing, not even the digest (property P8).
    ///
    /// # Errors
    /// An unknown tool or an action, a caller who is no major civilization of the game (the one
    /// check Python made of a query's caller, `tools.py:105-106`), an argument missing or of the
    /// wrong type, or the query's own refusal.
    pub fn execute_query(
        &self,
        pid: PlayerId,
        tool: &str,
        args: &Value,
    ) -> Result<Outcome, ActionError> {
        let spec = find(self, tool)?;
        let Op::Query(q) = spec.op else {
            return Err(ActionError::new(
                ErrCode::BadParam,
                format!("{tool} is an action: it changes the game, so call it with execute."),
            ));
        };
        answer(self, pid, spec, q, args)
    }
}

/// A query's answer: the caller must be a major civilization of the game, the one check Python
/// made of a query's caller (`tools.py:105-106`); then its arguments are coerced.
fn answer(
    g: &Game,
    pid: PlayerId,
    spec: &'static ToolSpec,
    q: Query,
    args: &Value,
) -> Result<Outcome, ActionError> {
    if g.player(pid).is_none_or(|p| !p.is_major()) {
        return Err(ActionError::new(ErrCode::InvalidPlayer, "Invalid player."));
    }
    let args = normalize_with(&spec.args, args)?;
    query_tools::answer(g, pid, q, &args)
}

/// The tool called `tool`, in a game that takes calls.
fn find(g: &Game, tool: &str) -> Result<&'static ToolSpec, ActionError> {
    g.ensure_live()?;
    registry::tool(tool).ok_or_else(|| {
        ActionError::new(ErrCode::UnknownTool, format!("Unknown tool '{}'.", echo(tool)))
    })
}

/// An action's typed form, from its coerced arguments. The action's fields take what Python's
/// tool took, most of them any JSON value that the rule then reads; the few that must be of one
/// type (a unit's order, a promotion, an improvement, a unit action's name) refuse anything else
/// here, with the parameter's name and what it takes.
fn action_of(spec: &ToolSpec, mut fields: Map<String, Value>) -> Result<Action, ActionError> {
    fields.insert("tool".to_owned(), Value::from(spec.name()));
    let tagged = Value::Object(fields);
    Action::deserialize(&tagged).map_err(|_| {
        // refcheck: tool-arguments-of-the-wrong-type-refused
        let wrong = spec
            .args
            .params
            .iter()
            .find(|p| tagged.get(p.name).is_some_and(|v| !v.is_null() && !p.json.fits(v)));
        let message = match wrong {
            Some(p) => format!("Parameter '{}' must be {}.", p.name, p.json.what()),
            None => format!("The arguments of {} do not fit it: see its schema.", spec.name()),
        };
        ActionError::new(ErrCode::BadParam, message)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::api::tools::args::SchemaType;

    /// A value of a parameter's type that the action reads.
    fn sample(ty: SchemaType) -> Value {
        match ty {
            SchemaType::Integer | SchemaType::Number => json!(1),
            SchemaType::String | SchemaType::IntegerOrString | SchemaType::Any => json!("x"),
            SchemaType::Boolean => json!(true),
            SchemaType::Strings => json!(["x"]),
            SchemaType::Objects => json!([{"type": "embassy"}]),
            SchemaType::Object => json!({}),
        }
    }

    #[test]
    fn every_action_tool_reads_into_its_action_with_or_without_its_optional_arguments() {
        for spec in registry::TOOLS.iter().filter(|t| t.kind() == ToolKind::Action) {
            let all: Map<String, Value> =
                spec.args.params.iter().map(|p| (p.name.to_owned(), sample(p.json))).collect();
            let least: Map<String, Value> = spec
                .args
                .params
                .iter()
                .filter(|p| spec.args.required.contains(&p.name))
                .map(|p| (p.name.to_owned(), sample(p.json)))
                .collect();
            for fields in [all, least] {
                let a = action_of(spec, fields.clone())
                    .unwrap_or_else(|e| panic!("{}: {fields:?}: {e}", spec.name()));
                assert_eq!(a.tool(), spec.name());
                assert_eq!(a.any_time(), spec.any_time, "{}", spec.name());
                // What the log records is what the tool took.
                let mut back = serde_json::to_value(&a).expect("an action serialises");
                back.as_object_mut().map(|m| m.shift_remove("tool"));
                assert_eq!(back, Value::Object(fields), "{}", spec.name());
            }
        }
    }

    #[test]
    fn a_name_that_is_no_text_is_refused_naming_the_parameter() {
        let spec = registry::tool("unit_order").expect("unit_order");
        let mut fields = Map::new();
        fields.insert("unit_id".into(), json!(3));
        fields.insert("order".into(), json!(5));
        let e = action_of(spec, fields).expect_err("a number is no order");
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrCode::BadParam, "Parameter 'order' must be a string.")
        );
    }
}
