//! Coercing a tool call's arguments, exactly as `tools.execute` did (`tools.py:113-127`).
//!
//! Models routinely send `"3"` for an integer and a comma-separated string for an array, so
//! arguments are coerced rather than refused on type alone, and unknown keys are dropped rather
//! than refused, which would cost a model its turn (DESIGN.md 8.3). In Python's order:
//! 1. every required parameter must be present and not `null`, else the refusal names all the
//!    missing ones, before anything is coerced;
//! 2. keys the tool does not take are dropped;
//! 3. in the order the tool declares its parameters: an integer parameter goes through Python's
//!    `int()` ([`crate::base::py::int_of`]): a float is truncated toward zero, a string must be
//!    an integer literal with optional whitespace around it, and anything else is refused with
//!    "Parameter 'k' must be an integer."; a boolean parameter given as a string is `true`
//!    exactly when it reads `true`, `1` or `yes` in any case; an array parameter given as a
//!    string is split at commas into its stripped, non-empty parts. Everything else, `null`
//!    included, passes as it is.
//!
//! There is no `at` to `x, y` coercion: Python never had one.
//!
//! Two differences, on purpose: an integer outside `i64` is refused where Python made a big
//! integer of it, and arguments that are neither an object nor empty are refused where Python's
//! `dict()` accepted a list of pairs or raised a `TypeError` that escaped as a crash.

use serde_json::{Map, Value};

use super::args::{self, ArgType, ToolArgs};
use crate::base::py;
use crate::game::error::{ActionError, ErrCode};

/// The arguments of a call to `tool`, coerced as `tools.execute` coerced them. An unknown tool is
/// refused as Python refused it.
pub fn normalize(tool: &str, args: &Value) -> Result<Map<String, Value>, ActionError> {
    let spec = args::spec(tool)
        .ok_or_else(|| ActionError::new(ErrCode::UnknownTool, format!("Unknown tool '{tool}'.")))?;
    normalize_with(spec, args)
}

/// The arguments of a call to the tool `spec` describes, coerced.
pub fn normalize_with(spec: &ToolArgs, args: &Value) -> Result<Map<String, Value>, ActionError> {
    // `dict(args or {})`: anything false is no arguments.
    let mut out = match args {
        Value::Object(m) => m.clone(),
        v if !py::truthy(v) => Map::new(),
        _ => {
            return Err(ActionError::new(
                ErrCode::BadParam,
                format!("The arguments of '{}' must be a JSON object.", spec.tool),
            ));
        }
    };
    let missing: Vec<&str> =
        spec.required.iter().copied().filter(|r| out.get(*r).is_none_or(Value::is_null)).collect();
    if !missing.is_empty() {
        return Err(ActionError::new(
            ErrCode::MissingParam,
            format!("Missing required parameter(s): {}.", missing.join(", ")),
        ));
    }
    out.retain(|k, _| spec.param(k).is_some());
    for &(name, ty) in spec.params {
        let Some(v) = out.get_mut(name) else { continue };
        match ty {
            ArgType::Integer if !v.is_null() => {
                let n = py::int_of(v).ok_or_else(|| {
                    ActionError::new(
                        ErrCode::BadParam,
                        format!("Parameter '{name}' must be an integer."),
                    )
                })?;
                *v = Value::from(n);
            }
            ArgType::Boolean => {
                if let Value::String(s) = v {
                    let on = matches!(s.to_lowercase().as_str(), "true" | "1" | "yes");
                    *v = Value::Bool(on);
                }
            }
            ArgType::Array => {
                if let Value::String(s) = v {
                    let parts = s
                        .split(',')
                        .map(py::strip)
                        .filter(|p| !p.is_empty())
                        .map(|p| Value::String(p.to_owned()))
                        .collect();
                    *v = Value::Array(parts);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PROBE: ToolArgs = ToolArgs {
        tool: "probe",
        params: &[
            ("n", ArgType::Integer),
            ("flag", ArgType::Boolean),
            ("names", ArgType::Array),
            ("m", ArgType::Integer),
        ],
        required: &["n"],
    };

    fn run(args: Value) -> Result<Value, String> {
        normalize_with(&PROBE, &args).map(Value::Object).map_err(|e| e.message)
    }

    #[test]
    fn coerces_as_tools_execute_did() {
        assert_eq!(run(json!({"n": "3"})), Ok(json!({"n": 3})));
        assert_eq!(run(json!({"n": 3.7})), Ok(json!({"n": 3})));
        assert_eq!(run(json!({"n": " 4 "})), Ok(json!({"n": 4})));
        assert_eq!(run(json!({"n": "3.0"})), Err("Parameter 'n' must be an integer.".into()));
        assert_eq!(run(json!({"n": 1, "flag": "Yes"})), Ok(json!({"n": 1, "flag": true})));
        assert_eq!(run(json!({"n": 1, "flag": "no"})), Ok(json!({"n": 1, "flag": false})));
        assert_eq!(run(json!({"n": 1, "flag": 0})), Ok(json!({"n": 1, "flag": 0})));
        assert_eq!(
            run(json!({"n": 1, "names": "a, b,,"})),
            Ok(json!({"n": 1, "names": ["a", "b"]}))
        );
        assert_eq!(run(json!({"n": 1, "junk": 5})), Ok(json!({"n": 1})));
        assert_eq!(run(json!({"n": 1, "m": null})), Ok(json!({"n": 1, "m": null})));
    }

    #[test]
    fn missing_parameters_are_reported_before_anything_is_coerced() {
        assert_eq!(
            run(json!({"m": "not a number"})),
            Err("Missing required parameter(s): n.".into())
        );
        assert_eq!(run(json!({"n": null})), Err("Missing required parameter(s): n.".into()));
        assert_eq!(run(json!(null)), Err("Missing required parameter(s): n.".into()));
        let first_bad = run(json!({"n": "x", "m": "y"}));
        assert_eq!(first_bad, Err("Parameter 'n' must be an integer.".into()), "declared order");
    }

    #[test]
    fn an_unknown_tool_is_refused() {
        let e = normalize("no_such_tool", &json!({})).map(|_| ()).map_err(|e| (e.code, e.message));
        assert_eq!(e, Err((ErrCode::UnknownTool, "Unknown tool 'no_such_tool'.".into())));
        let e = normalize_with(&PROBE, &json!([1])).map(|_| ()).map_err(|e| e.code);
        assert_eq!(e, Err(ErrCode::BadParam));
    }
}
