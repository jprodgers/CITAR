//! Test rulesets made from the shipped one, and the kitchen sink among them.
//!
//! A host hands the engine a ruleset as bytes (DESIGN.md 5.2), so a test ruleset is the embedded
//! files with some of them changed. An overlay changes them by JSON merge patches (RFC 7396), one
//! per ruleset file: an object merges key by key, `null` removes a key, and any other value
//! replaces what was there. A patch therefore adds objects to a table, or changes fields of one,
//! without restating the rest; a list, such as an object's `uniques`, is replaced whole.
//!
//! [`kitchen_sink`] is the shipped ruleset with `testdata/rulesets/kitchen_sink/` over it: new
//! objects that between them use every unique type and conditional the engine supports and the
//! shipped ruleset does not (owner decision 2026-09-23, package 1a-05b). The packages of 1b and
//! 1c test the extra types of their systems with it; `tests/engine/kitchen_sink.rs` holds it and
//! the shipped ruleset together to every supported type.

use std::sync::OnceLock;

use citar_engine::rules::{Ruleset, RulesetFiles, embedded};
use serde_json::Value;

/// The kitchen sink's patches, by the ruleset file each one changes.
pub const KITCHEN_SINK: &[(&str, &str)] = &[
    (
        "ruleset/beliefs.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/beliefs.json"),
    ),
    (
        "ruleset/buildings.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/buildings.json"),
    ),
    (
        "ruleset/city_state_types.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/city_state_types.json"),
    ),
    (
        "ruleset/improvements.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/improvements.json"),
    ),
    (
        "ruleset/nations.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/nations.json"),
    ),
    (
        "ruleset/promotions.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/promotions.json"),
    ),
    ("ruleset/ruins.json", include_str!("../testdata/rulesets/kitchen_sink/ruleset/ruins.json")),
    (
        "ruleset/terrains.json",
        include_str!("../testdata/rulesets/kitchen_sink/ruleset/terrains.json"),
    ),
    ("ruleset/units.json", include_str!("../testdata/rulesets/kitchen_sink/ruleset/units.json")),
];

/// Applies the merge patch `patch` to `target` (RFC 7396).
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(changes) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let Value::Object(fields) = target else { return };
    for (key, change) in changes {
        if change.is_null() {
            fields.shift_remove(key);
        } else {
            merge_patch(fields.entry(key.clone()).or_insert(Value::Null), change);
        }
    }
}

/// The embedded files with `patches` applied, each patch to the file it names, as owned bytes
/// for [`RulesetFiles`].
///
/// # Errors
/// A patch that names no ruleset file, or a patch or file that is not JSON.
pub fn overlay(patches: &[(&str, &str)]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut files: Vec<(String, Vec<u8>)> =
        embedded().files.iter().map(|&(n, b)| (n.to_owned(), b.to_vec())).collect();
    for &(name, patch) in patches {
        let patch: Value =
            serde_json::from_str(patch).map_err(|e| format!("the patch of {name}: {e}"))?;
        let Some(slot) = files.iter_mut().find(|(n, _)| n == name) else {
            return Err(format!("{name} is no file of the ruleset"));
        };
        let mut v: Value = serde_json::from_slice(&slot.1).map_err(|e| format!("{name}: {e}"))?;
        merge_patch(&mut v, &patch);
        slot.1 = serde_json::to_vec(&v).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(files)
}

/// The files as the engine takes them.
#[must_use]
pub fn files_of(files: &[(String, Vec<u8>)]) -> RulesetFiles<'_> {
    RulesetFiles::new(files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect())
}

/// The kitchen-sink ruleset, loaded once.
///
/// # Panics
/// If it does not load: every problem is in the message.
pub fn kitchen_sink() -> &'static Ruleset {
    static RULES: OnceLock<&'static Ruleset> = OnceLock::new();
    RULES.get_or_init(|| {
        let files = overlay(KITCHEN_SINK).unwrap_or_else(|e| panic!("the kitchen sink: {e}"));
        Ruleset::leak(&files_of(&files))
            .unwrap_or_else(|e| panic!("the kitchen sink does not load:\n{e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_merge_patch_merges_objects_and_replaces_the_rest() {
        let mut v = json!({"a": {"x": 1, "y": [1, 2]}, "b": 2, "c": 3});
        merge_patch(&mut v, &json!({"a": {"y": [3], "z": true}, "b": null, "d": {"e": 4}}));
        assert_eq!(v, json!({"a": {"x": 1, "y": [3], "z": true}, "c": 3, "d": {"e": 4}}));
        let keys: Vec<&String> = v.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["a", "c", "d"], "a new key goes last, the rest keep their places");
        let mut w = json!([1]);
        merge_patch(&mut w, &json!({"a": 1}));
        assert_eq!(w, json!({"a": 1}));
    }

    #[test]
    fn an_overlay_names_real_files() {
        let e = overlay(&[("ruleset/nothing.json", "{}")]).expect_err("no such file");
        assert!(e.contains("no file of the ruleset"), "{e}");
        let e = overlay(&[("ruleset/units.json", "{")]).expect_err("not JSON");
        assert!(e.contains("the patch of ruleset/units.json"), "{e}");
        assert!(overlay(&[]).is_ok_and(|f| f.len() == embedded().files.len()));
    }
}
