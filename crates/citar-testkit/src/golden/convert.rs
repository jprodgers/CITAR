//! The golden set of package 1a-10 (DESIGN.md 9.6):
//! - **`convert.json`**: the digests of the twelve committed refcheck fixtures right after the
//!   Python-state converter reads them, before any settle, each with its canonical length and the
//!   count of what the conversion dropped. A digest that moves is a change to the converter, to
//!   `CANON_V1` or to the embedded ruleset; one that differs between targets is a determinism bug
//!   in the conversion (the digest covers the chronicle's running hash, so event wording and name
//!   offsets count too).
//!
//! Written by `golden bless` when one of those changes on purpose.

use citar_engine::compat::python::state_from_python;
use citar_engine::rules::Ruleset;
use citar_engine::save::{self, canon};
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};
use crate::fixtures;

/// Each committed fixture's digest after conversion, and what went wrong converting one.
fn answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let found = match fixtures::committed() {
        Ok(f) => f,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    for f in &found {
        let converted = match fixtures::read_state(f)
            .and_then(|bytes| state_from_python(&bytes, r).map_err(|e| e.to_string()))
        {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("{} does not convert: {e}", f.name));
                continue;
            }
        };
        let st = &converted.state;
        match (save::digest(r, st), canon::state_bytes(st)) {
            (Ok(d), Ok(bytes)) => {
                let dropped: u32 = converted.report.dropped().map(|(_, n)| n).sum();
                rows.push(json!([f.name, d.to_hex(), bytes.len(), dropped]));
            }
            (Err(e), _) | (_, Err(e)) => problems.push(format!("{} does not digest: {e}", f.name)),
        }
    }
    let v = json!({
        "format": 1,
        "digest": "blake3(CITAR-DIGEST, CANON_V1, RulesetId, canon(State)) of each committed fixture, converted (DESIGN.md 4.10, 4.12)",
        "ruleset_id": r.id().to_hex(),
        "states": rows,
    });
    (v, problems)
}

fn render(v: &Value) -> String {
    render_rows(v, &["states"])
}

/// The file `golden bless` writes.
#[must_use]
pub fn blessed() -> Vec<(&'static str, String)> {
    vec![("convert.json", render(&answers().0))]
}

/// The `convert` set, checked against `convert.json`.
#[must_use]
pub fn check_convert() -> SetReport {
    let (got, mut problems) = answers();
    match read_committed("convert.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("ruleset_id") != got.get("ruleset_id") {
                problems.push("convert.json: the ruleset id differs from this build's".to_owned());
            }
            problems.extend(diff_rows(
                "convert.json",
                "states",
                want.get("states"),
                &got["states"],
            ));
        }
    }
    SetReport {
        name: "convert",
        computed: digest_of(&got),
        problems: capped(problems),
        waiting: Vec::new(),
    }
}
