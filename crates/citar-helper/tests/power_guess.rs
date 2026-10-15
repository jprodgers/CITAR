//! The power guess (package 4-06, gate 1): on each of the twelve hwinfo outputs of
//! `tests/data/hwinfo_power_cases.json` (two recorded on Windows and in WSL, ten derived from them
//! so that every branch of `hwinfo.guess_power` is taken), the Rust port gives the answer the
//! Python collector gave, which `tests/test_helper_scan.py` holds to `hwinfo.guess_power` itself.

use std::path::Path;

use citar_helper::scan::guess::guess_power;
use serde_json::Value;

#[test]
fn the_guess_equals_the_collectors_on_every_recorded_output() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/hwinfo_power_cases.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let table: Value = serde_json::from_str(&text).expect("the table is JSON");
    let cases = table["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 12);
    let mut wrong = Vec::new();
    for c in cases {
        let info = c["info"].as_object().expect("an hwinfo output");
        let got = Value::Object(guess_power(info));
        if got != c["guess"] {
            wrong.push(format!("{}:\n  got  {got}\n  want {}", c["about"], c["guess"]));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
