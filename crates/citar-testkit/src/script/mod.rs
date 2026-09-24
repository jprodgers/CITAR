//! The Rust runner of rule scripts: TOML files in `tests/rules/` that set a game up with
//! scenario and test operations, act through the tools, and check what `inspect` reads
//! (DESIGN.md 9.3). `tests/rules/README.md` is the language; `tests/rulescript.py` is the
//! Python runner, and `tests/rules/_selftest.toml` keeps the two from drifting apart.
//!
//! - [`load`] reads a script, [`run`] plays it on the engine, [`discover`] lists them;
//! - [`path`], [`matchers`] and [`expr`]: paths into values, what checks assert, and the `=`
//!   expressions;
//! - [`tiles`]: tile references;
//! - [`new_game`] and [`map_doc`]: new games from settings and the maps of `tests/rules/maps/`,
//!   through the engine's own setup (`Game::new`).
//!
//! `crates/citar-testkit/tests/rules.rs` runs every script as its own test.

pub mod expr;
pub mod matchers;
pub mod path;
mod runner;
pub mod tiles;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use citar_engine::game::Game;
use citar_engine::game::setup::config_from_value;
use citar_engine::rules::Ruleset;
use serde_json::{Map, Number, Value};

pub use self::runner::run;

/// A map of `tests/rules/maps/`, as the engines take it, and its anchors: the runners read the
/// anchors, and the engines get the document without them.
pub fn map_doc(name: &str) -> Result<(Value, Map<String, Value>), String> {
    let file = rules_dir().join("maps").join(format!("{name}.json"));
    #[allow(clippy::disallowed_methods, reason = "the maps are files")]
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut doc: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let anchors = doc
        .as_object_mut()
        .and_then(|m| m.shift_remove("anchors"))
        .and_then(|a| a.as_object().cloned())
        .unwrap_or_default();
    Ok((doc, anchors))
}

/// A new game from settings as a lobby sends them (`Game::config_from_json`, then `Game::new`);
/// a refusal is the engine's own text.
pub fn new_game(rules: &'static Ruleset, cfg: &Map<String, Value>) -> Result<Game, String> {
    let setup = config_from_value(rules, &Value::Object(cfg.clone())).map_err(|e| e.to_string())?;
    let (g, _) = Game::new(rules, &setup).map_err(|e| e.to_string())?;
    Ok(g)
}

/// The folder of the rule scripts: `tests/rules` at the repository's root.
#[must_use]
pub fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/rules")
}

/// A script, read and checked for its shape.
#[derive(Clone, Debug)]
pub struct Script {
    /// Its file's stem: the test's name.
    pub name: String,
    /// What it pins down.
    pub about: String,
    /// The map it plays on: `tests/rules/maps/<map>.json`.
    pub map: String,
    /// Whether the bare prelude applies.
    pub bare: bool,
    /// The settings it gives over the runner's defaults.
    pub config: Map<String, Value>,
    /// Its steps, in order.
    pub steps: Vec<Value>,
}

/// The top-level keys a script may have.
const TOP: [&str; 6] = ["about", "from", "map", "start", "config", "step"];

/// Every script in [`rules_dir`], sorted by name.
pub fn discover() -> Result<Vec<PathBuf>, String> {
    let dir = rules_dir();
    #[allow(clippy::disallowed_methods, reason = "the scripts are files")]
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "toml")
                && p.file_stem().is_some_and(|s| s != "intended")
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Reads a script.
pub fn load(path: &Path) -> Result<Script, String> {
    #[allow(clippy::disallowed_methods, reason = "the scripts are files")]
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("script").to_owned();
    parse(&name, &text)
}

/// Reads a script from its text.
pub fn parse(name: &str, text: &str) -> Result<Script, String> {
    let doc: toml::Table = text.parse().map_err(|e| format!("{name}: {e}"))?;
    let doc = match from_toml(toml::Value::Table(doc)).map_err(|e| format!("{name}: {e}"))? {
        Value::Object(m) => m,
        _ => return Err(format!("{name}: a script is a table")),
    };
    if let Some(k) = doc.keys().find(|k| !TOP.contains(&k.as_str())) {
        return Err(format!("{name}: unknown key {k:?} (a script has {})", TOP.join(", ")));
    }
    let about = doc
        .get("about")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{name}: `about` must say what the script pins down"))?
        .to_owned();
    let map = doc.get("map").and_then(Value::as_str).unwrap_or("arena").to_owned();
    let bare = match doc.get("start").map(|v| v.as_str()) {
        None | Some(Some("bare")) => true,
        Some(Some("full")) => false,
        _ => return Err(format!("{name}: start is \"bare\" or \"full\"")),
    };
    let config = match doc.get("config") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(format!("{name}: config is a table")),
    };
    let steps = match doc.get("step") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(format!("{name}: steps are [[step]] tables")),
    };
    Ok(Script { name: name.to_owned(), about, map, bare, config, steps })
}

/// A TOML value as JSON. Dates have no JSON form and are refused.
pub fn from_toml(v: toml::Value) -> Result<Value, String> {
    Ok(match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => {
            Value::Number(Number::from_f64(f).ok_or("a script's numbers must be finite")?)
        }
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Array(a) => {
            Value::Array(a.into_iter().map(from_toml).collect::<Result<_, _>>()?)
        }
        toml::Value::Table(t) => Value::Object(
            t.into_iter().map(|(k, v)| Ok((k, from_toml(v)?))).collect::<Result<_, String>>()?,
        ),
        toml::Value::Datetime(d) => return Err(format!("{d}: a script has no dates")),
    })
}

/// The ids of the deliberate differences scripts may cite with `intended`: those of
/// `refcheck/intended.toml` and of `tests/rules/intended.toml`.
pub fn intended_ids() -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for file in
        [rules_dir().join("../../refcheck/intended.toml"), rules_dir().join("intended.toml")]
    {
        #[allow(clippy::disallowed_methods, reason = "the lists are files")]
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let doc: toml::Table = text.parse().map_err(|e| format!("{}: {e}", file.display()))?;
        for entry in doc.get("differences").and_then(toml::Value::as_array).into_iter().flatten() {
            if let Some(id) = entry.get("id").and_then(toml::Value::as_str) {
                out.insert(id.to_owned());
            }
        }
    }
    Ok(out)
}

/// Where a script types a number as a string, which the tools and operations would coerce:
/// scripts must pass numbers as numbers, unless a step says `coerce = true` to test the coercion
/// on purpose. Expressions (`"=..."`) are not literals.
#[must_use]
pub fn number_as_string(v: &Value, at: &str) -> Option<String> {
    match v {
        Value::String(s) if !s.starts_with('=') && looks_numeric(s) => Some(format!(
            "{at} is the number {s:?} typed as a string; write it as a number, or add \
             coerce = true to test the coercion on purpose"
        )),
        Value::Array(a) => {
            a.iter().enumerate().find_map(|(i, x)| number_as_string(x, &format!("{at}[{i}]")))
        }
        Value::Object(o) => o.iter().find_map(|(k, x)| number_as_string(x, &format!("{at}.{k}"))),
        _ => None,
    }
}

/// An optional sign, digits and at most one point, with at least one digit, spaces around.
fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let digits = t.chars().filter(char::is_ascii_digit).count();
    digits > 0
        && t.chars().all(|c| c.is_ascii_digit() || c == '.')
        && t.chars().filter(|&c| c == '.').count() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_typed_as_strings_are_found() {
        assert!(number_as_string(&json!({"player": "0"}), "args").is_some());
        assert!(number_as_string(&json!({"list": [" 4 ", "x"]}), "args").is_some());
        assert!(
            number_as_string(&json!({"x": "=1 + 2", "t": "Writing", "r": "(3,4)"}), "args")
                .is_none()
        );
        assert!(number_as_string(&json!({"v": "-2.5"}), "args").is_some());
        assert!(number_as_string(&json!({"v": "1.2.3"}), "args").is_none());
    }
}
