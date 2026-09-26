//! The golden set of package 1a-09 (DESIGN.md 4.10, 9.6):
//! - **`states.json`**: the digests of the three states checked in under `testdata/states/`, each
//!   loaded under the embedded ruleset. The states are fixed inputs: synthetic, made once by
//!   `golden states` from [`crate::states`], and kept as files so that a change to the generator
//!   cannot move them. A digest that moves is a change to `CANON_V1` or to how a save loads, and
//!   one that differs between targets is a determinism bug.
//!
//! The check also loads each state back from its own save and asks for the same digest and the
//! same bytes again, so a save that does not round-trip on some target shows here too.
//!
//! Written by `golden bless` when the encoding or the embedded ruleset changes, which changes
//! every digest (the ruleset's id is part of it).

use std::path::PathBuf;

use citar_engine::rules::Ruleset;
use citar_engine::save::{self, canon, json};
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};
use crate::states::{self as generate, Shape};

/// The checked-in states: file name, and the seed and shape `golden states` made them from.
pub const STATES: [(&str, u64, Shape); 3] = [
    ("tiny", 1, Shape::TINY),
    ("duel", 2, Shape::DUEL),
    (
        "crowded",
        3,
        Shape {
            width: 24,
            height: 20,
            majors: 8,
            city_states: 6,
            cities: 30,
            units: 120,
            explored: 70,
        },
    ),
];

/// Where the checked-in states live.
#[must_use]
pub fn states_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/states"))
}

/// The bytes of a checked-in state.
///
/// # Errors
/// If the file cannot be read.
#[allow(clippy::disallowed_methods, reason = "the checked-in states are files")]
pub fn read_state_file(name: &str) -> Result<Vec<u8>, String> {
    let path = states_dir().join(format!("{name}.json"));
    std::fs::read(&path)
        .map_err(|e| format!("cannot read {}: {e} (run `golden states`?)", path.display()))
}

/// The checked-in states as `golden states` writes them: (file name, save).
///
/// # Panics
/// If a generated state does not save, which is a bug.
#[must_use]
pub fn generated() -> Vec<(String, Vec<u8>)> {
    let r = Ruleset::shared();
    STATES
        .iter()
        .map(|(name, seed, shape)| {
            let st = generate::build(r, *seed, shape);
            let bytes =
                save::to_json(r, &st).unwrap_or_else(|e| panic!("{name} does not save: {e}"));
            (format!("{name}.json"), bytes)
        })
        .collect()
}

/// Each checked-in state's digest and canonical size, and what went wrong reading it.
fn answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    for (name, _, _) in STATES {
        let bytes = match read_state_file(name) {
            Ok(b) => b,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let st = match json::read_state(r, &bytes) {
            Ok((st, _)) => st,
            Err(e) => {
                problems.push(format!("states/{name}.json does not load: {e}"));
                continue;
            }
        };
        let (digest, len) = match (save::digest(r, &st), canon::state_bytes(&st)) {
            (Ok(d), Ok(b)) => (d.to_hex(), b.len()),
            (Err(e), _) | (_, Err(e)) => {
                problems.push(format!("states/{name}.json does not digest: {e}"));
                continue;
            }
        };
        // Loaded from its own save, it is the same state and saves to the same bytes.
        match save::to_json(r, &st) {
            Ok(again) => {
                let back = json::read_state(r, &again).map(|(s, _)| s);
                if back.as_ref().ok() != Some(&st)
                    || back.ok().and_then(|b| save::to_json(r, &b).ok()) != Some(again)
                {
                    problems.push(format!("states/{name}.json does not round-trip"));
                }
            }
            Err(e) => problems.push(format!("states/{name}.json does not save: {e}")),
        }
        rows.push(json!([name, digest, len]));
    }
    let v = json!({
        "format": 1,
        "digest": "blake3(CITAR-DIGEST, CANON_V1, RulesetId, canon(State)) (DESIGN.md 4.10)",
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
    vec![("states.json", render(&answers().0))]
}

/// The `states` set, checked against `states.json`.
#[must_use]
pub fn check_states() -> SetReport {
    let (got, mut problems) = answers();
    match read_committed("states.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("ruleset_id") != got.get("ruleset_id") {
                problems.push("states.json: the ruleset id differs from this build's".to_owned());
            }
            problems.extend(diff_rows("states.json", "states", want.get("states"), &got["states"]));
        }
    }
    SetReport {
        name: "states",
        computed: digest_of(&got),
        problems: capped(problems),
        waiting: Vec::new(),
    }
}
