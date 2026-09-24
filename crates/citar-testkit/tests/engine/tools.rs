//! Tool arguments (package 1b-02): `api::tools::normalize` coerces a call's arguments as
//! `tools.execute` did (`tools.py:113-127`), case for case with `tests/rules/normalize.json`,
//! which `tests/test_rule_scripts.py` runs through the Python engine (gate 4). A case marked
//! `intended` is a deliberate difference, which only this side runs.

use citar_engine::api::tools::{ArgType, ToolArgs, normalize_with};
use citar_testkit::script::{intended_ids, matchers::same, rules_dir};
use serde_json::Value;

/// A tool of the table, leaked: a spec's names are `'static`, as the tools' own are.
fn tool(name: &str, spec: &Value) -> &'static ToolArgs {
    let params: Vec<(&'static str, ArgType)> = spec["params"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: params"))
        .iter()
        .map(|p| {
            let n: &'static str =
                Box::leak(p[0].as_str().unwrap_or_default().to_owned().into_boxed_str());
            (n, ArgType::from_schema(p[1].as_str()))
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
