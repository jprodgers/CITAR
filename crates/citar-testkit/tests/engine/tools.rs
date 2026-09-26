//! The tools as a model sees them.
//!
//! - Tool arguments (package 1b-02): `api::tools::normalize` coerces a call's arguments as
//!   `tools.execute` did (`tools.py:113-127`), case for case with `tests/rules/normalize.json`,
//!   which `tests/test_rule_scripts.py` runs through the Python engine (gate 4). A case marked
//!   `intended` is a deliberate difference, which only this side runs.
//! - The registry (package 1d-01, gate 2): the schemas every model reads equal the Python
//!   engine's `tool_list()`, recorded in `tests/rules/tool_list.json`, apart from the fixes
//!   listed in [`FIXES`].

use citar_engine::api::tools::{
    ArgType, Param, SchemaType, ToolArgs, normalize_with, schemas_json,
};
use citar_testkit::script::{intended_ids, matchers::same, rules_dir};
use serde_json::Value;

/// A tool of the table, leaked: a spec's names are `'static`, as the tools' own are.
fn tool(name: &str, spec: &Value) -> &'static ToolArgs {
    let params: Vec<Param> = spec["params"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: params"))
        .iter()
        .map(|p| {
            let n: &'static str =
                Box::leak(p[0].as_str().unwrap_or_default().to_owned().into_boxed_str());
            Param::new(n, SchemaType::of(ArgType::from_schema(p[1].as_str())))
        })
        .collect();
    let required: Vec<&'static str> = spec["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: required"))
        .iter()
        .map(|r| -> &'static str {
            Box::leak(r.as_str().unwrap_or_default().to_owned().into_boxed_str())
        })
        .collect();
    Box::leak(Box::new(ToolArgs {
        tool: Box::leak(name.to_owned().into_boxed_str()),
        params: Box::leak(params.into_boxed_slice()),
        required: Box::leak(required.into_boxed_slice()),
    }))
}

#[test]
fn arguments_are_coerced_as_python_coerced_them() {
    #[allow(clippy::disallowed_methods, reason = "the table is a file")]
    let text = std::fs::read_to_string(rules_dir().join("normalize.json")).expect("the table");
    let table: Value = serde_json::from_str(&text).expect("the table's JSON");
    let tools = table["tools"].as_object().expect("the tools");
    let cases = table["cases"].as_array().expect("the cases");
    assert!(cases.len() >= 20, "the table covers the rules");
    let listed = intended_ids().expect("the intended lists");
    let mut wrong = Vec::new();
    for case in cases {
        if let Some(id) = case.get("intended") {
            assert!(
                id.as_str().is_some_and(|id| listed.contains(id)),
                "{case}: no such intended id"
            );
        }
        let name = case["tool"].as_str().unwrap_or_default();
        let spec = tool(name, &tools[name]);
        let got = normalize_with(spec, &case["args"]);
        let ok = match (&got, case.get("out"), case.get("error")) {
            (Ok(out), Some(want), None) => same(&Value::Object(out.clone()), want),
            (Err(e), None, Some(want)) => want.as_str() == Some(e.message.as_str()),
            _ => false,
        };
        if !ok {
            wrong.push(format!("{case}: got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{} cases differ:\n{}", wrong.len(), wrong.join("\n"));
}

/// Where the registry says what Python's tool list did not, on purpose: the tool, the field of
/// its entry, and the id of `tests/rules/intended.toml` that explains it.
const FIXES: &[(&str, &str, &str)] =
    &[("city_state_action", "description", "gift-unit-described-as-ruled")];

#[test]
fn the_schemas_equal_python_s_tool_list() {
    #[allow(clippy::disallowed_methods, reason = "the list is a file")]
    let text = std::fs::read_to_string(rules_dir().join("tool_list.json")).expect("the list");
    let python: Vec<Value> = serde_json::from_str(&text).expect("the list's JSON");
    let rust: Vec<Value> = serde_json::from_str(schemas_json()).expect("the schemas' JSON");
    let names = |v: &[Value]| -> Vec<String> {
        v.iter().map(|t| t["name"].as_str().unwrap_or_default().to_owned()).collect()
    };
    assert_eq!(names(&rust), names(&python), "the same tools, in the same order");
    let listed = intended_ids().expect("the intended lists");
    let mut wrong = Vec::new();
    for (r, p) in rust.iter().zip(&python) {
        let (Some(r), Some(p)) = (r.as_object(), p.as_object()) else {
            panic!("a tool is an object: {r} {p}");
        };
        let keys: Vec<&String> = r.keys().collect();
        assert_eq!(keys, p.keys().collect::<Vec<_>>(), "the same fields, in the same order");
        let tool = r["name"].as_str().unwrap_or_default();
        for (field, rv) in r {
            let fix = FIXES.iter().find(|&&(t, f, _)| t == tool && f == field);
            match (rv == &p[field], fix) {
                (true, None) => {}
                (false, Some(&(_, _, id))) => {
                    assert!(listed.contains(id), "{id} is in no intended list");
                }
                (false, None) => wrong.push(format!("{tool}.{field}: {rv} != {}", p[field])),
                (true, Some(_)) => {
                    wrong.push(format!("{tool}.{field}: listed as fixed, but equal"))
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{}",
        wrong.join(
            "
"
        )
    );
}
