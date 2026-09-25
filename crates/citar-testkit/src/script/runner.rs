//! The interpreter: plays a script's steps on the Rust engine, as `tests/rulescript.py` plays
//! them on the Python one.

use std::collections::BTreeSet;

use citar_engine::api::{ActionError, ErrCode, inspect, testops, tools};
use citar_engine::base::ids::PlayerId;
use citar_engine::base::py;
use citar_engine::game::{Action, DebugOptions, Game};
use citar_engine::rules::Ruleset;
use serde_json::{Map, Value, json};

use super::expr::{self, Env};
use super::matchers::{self, MATCHERS, WITH};
use super::tiles::Frame;
use super::{Script, intended_ids, map_doc, new_game, number_as_string, path};

/// The kinds of step: each step has exactly one of these keys.
const KINDS: [&str; 7] = ["op", "ops", "tool", "check", "new_game", "set", "repeat"];
/// Keys any step may have. `as` and `error` belong to the kinds that use them, so a check that
/// says `error` is refused rather than passing without looking.
const COMMON: [&str; 4] = ["note", "must_fail", "intended", "coerce"];

/// Plays a script; the error says which step failed and why.
pub fn run(script: &Script) -> Result<(), String> {
    let mut r = Runner::start(script)?;
    for (i, step) in script.steps.iter().enumerate() {
        r.step(step, &format!("step {}", i + 1))?;
    }
    Ok(())
}

struct Runner<'s> {
    script: &'s Script,
    rules: &'static Ruleset,
    /// The map document, without the anchors.
    doc: Value,
    frame: Frame,
    game: Game,
    vars: Map<String, Value>,
    intended: BTreeSet<String>,
}

impl<'s> Runner<'s> {
    fn start(script: &'s Script) -> Result<Self, String> {
        if !script.bare {
            return Err(format!(
                "{}: start = \"full\" needs the setup stages package 1c-09 ports; the Rust \
                 runner plays bare scripts only until then",
                script.name
            ));
        }
        let (doc, anchors) = map_doc(&script.map)?;
        let mut frame = Frame {
            width: doc
                .get("width")
                .and_then(Value::as_i64)
                .and_then(|w| i32::try_from(w).ok())
                .unwrap_or(0),
            height: doc
                .get("height")
                .and_then(Value::as_i64)
                .and_then(|h| i32::try_from(h).ok())
                .unwrap_or(0),
            ..Frame::default()
        };
        for (k, v) in anchors {
            let xy = v.as_array().and_then(|a| {
                let n =
                    |i: usize| a.get(i).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok());
                Some((n(0)?, n(1)?))
            });
            frame.anchors.insert(k.clone(), xy.ok_or_else(|| format!("anchor {k}: not [x, y]"))?);
        }
        let rules = Ruleset::shared();
        let game = make_game(script, rules, &doc, &Map::new())?;
        Ok(Self { script, rules, doc, frame, game, vars: Map::new(), intended: intended_ids()? })
    }

    fn new_game(&self, overrides: &Map<String, Value>) -> Result<Game, String> {
        make_game(self.script, self.rules, &self.doc, overrides)
    }

    fn step(&mut self, step: &Value, label: &str) -> Result<(), String> {
        let s = step.as_object().ok_or_else(|| format!("{label}: a step is a table"))?;
        let result = self.step_inner(s, label);
        match s.get("must_fail") {
            None | Some(Value::Bool(false)) => result,
            Some(want) => match result {
                Ok(()) => Err(format!("{label}: must fail, but passed")),
                Err(e) => match want {
                    Value::Bool(true) => Ok(()),
                    Value::String(part) if e.contains(part.as_str()) => Ok(()),
                    Value::String(part) => {
                        Err(format!("{label}: failed, but not with {part:?}: {e}"))
                    }
                    _ => Err(format!("{label}: must_fail is true or a text")),
                },
            },
        }
    }

    fn step_inner(&mut self, s: &Map<String, Value>, label: &str) -> Result<(), String> {
        let kinds: Vec<&str> = KINDS.iter().copied().filter(|k| s.contains_key(*k)).collect();
        let [kind] = kinds[..] else {
            return Err(format!("{label}: a step has exactly one of {}", KINDS.join(", ")));
        };
        let own: &[&str] = match kind {
            "op" => &["args", "as", "error"],
            "ops" => &["as", "error"],
            "tool" => &["player", "args", "as", "error"],
            "check" => &["path", "as"],
            "new_game" => &["error"],
            "repeat" => &["steps"],
            _ => &[],
        };
        let allowed = |k: &str| {
            k == kind
                || COMMON.contains(&k)
                || own.contains(&k)
                || (kind == "check" && (MATCHERS.contains(&k) || WITH.contains(&k)))
        };
        if let Some(k) = s.keys().find(|k| !allowed(k)) {
            return Err(format!("{label}: a {kind} step has no key {k:?}"));
        }
        if let Some(id) = s.get("intended") {
            let id = id.as_str().unwrap_or_default();
            if !self.intended.contains(id) {
                return Err(format!(
                    "{label}: intended = {id:?} is in neither refcheck/intended.toml nor \
                     tests/rules/intended.toml"
                ));
            }
        }
        if s.get("coerce") != Some(&Value::Bool(true)) {
            for key in ["args", "ops", "new_game"] {
                if let Some(bad) = s.get(key).and_then(|v| number_as_string(v, key)) {
                    return Err(format!("{label}: {bad}"));
                }
            }
            if let Some(q @ Value::Object(_)) = s.get("check")
                && let Some(bad) = number_as_string(q, "check")
            {
                return Err(format!("{label}: {bad}"));
            }
        }
        match kind {
            "op" => {
                let name = s["op"].as_str().ok_or_else(|| format!("{label}: op is a name"))?;
                let mut o = Map::new();
                o.insert("op".into(), json!(name));
                o.extend(self.args(s.get("args"), label)?);
                let ops = json!([o]);
                let done = if testops::op(name).is_some() {
                    testops::apply(&mut self.game, &ops)
                } else {
                    self.game.apply_ops(&ops)
                };
                let done = done.map(|(mut v, _)| v.swap_remove(0)).map_err(|e| e.message);
                self.outcome(s, label, done)
            }
            "ops" => {
                let list = s["ops"]
                    .as_array()
                    .ok_or_else(|| format!("{label}: ops is a list of op tables"))?;
                let mut ops = Vec::with_capacity(list.len());
                for o in list {
                    ops.push(Value::Object(self.args(Some(o), label)?));
                }
                let done = self
                    .game
                    .apply_ops(&Value::Array(ops))
                    .map(|(v, _)| Value::Array(v))
                    .map_err(|e| e.message);
                self.outcome(s, label, done)
            }
            "tool" => {
                let name = s["tool"].as_str().ok_or_else(|| format!("{label}: tool is a name"))?;
                let pid = match s.get("player") {
                    None => self.game.current(),
                    Some(v) => {
                        let v = self.value(v, label)?;
                        py::int_of(&v)
                            .and_then(|n| u8::try_from(n).ok())
                            .map(PlayerId)
                            .ok_or_else(|| format!("{label}: player {v} is no player id"))?
                    }
                };
                let args = Value::Object(self.args(s.get("args"), label)?);
                let done = self.tool(pid, name, &args).map_err(|e| e.message);
                self.outcome(s, label, done)
            }
            "check" => self.check(s, label),
            "new_game" => {
                let over = match self.value(&s["new_game"], label)? {
                    Value::Object(m) => m,
                    _ => return Err(format!("{label}: new_game is a table of settings")),
                };
                match self.new_game(&over) {
                    Ok(g) if error_expected(s) => {
                        drop(g);
                        Err(format!("{label}: expected the settings to be refused"))
                    }
                    Ok(g) => {
                        self.game = g;
                        Ok(())
                    }
                    Err(e) => self.outcome(s, label, Err(e)),
                }
            }
            "set" => {
                let table =
                    s["set"].as_object().ok_or_else(|| format!("{label}: set is a table"))?;
                for (k, v) in table {
                    let v = self.value(v, label)?;
                    self.vars.insert(k.clone(), v);
                }
                Ok(())
            }
            _ => {
                let n = self.value(&s["repeat"], label)?;
                let n = n.as_u64().ok_or_else(|| format!("{label}: repeat is a count"))?;
                let steps = s.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
                for round in 1..=n {
                    for (j, st) in steps.iter().enumerate() {
                        self.step(st, &format!("{label} (round {round}, step {})", j + 1))?;
                    }
                }
                Ok(())
            }
        }
    }

    /// A tool call as a host makes one: the arguments coerced, then the typed action through
    /// the one pipeline.
    fn tool(&mut self, pid: PlayerId, name: &str, args: &Value) -> Result<Value, ActionError> {
        let mut fields = tools::normalize(name, args)?;
        fields.insert("tool".into(), json!(name));
        let action: Action = serde_json::from_value(Value::Object(fields)).map_err(|e| {
            ActionError::new(
                ErrCode::BadParam,
                format!("The arguments of '{name}' do not fit it: {e}."),
            )
        })?;
        self.game.act(pid, action).map(|(out, _)| out)
    }

    /// What an op, tool or new game did, against what the step expects: success, or an error
    /// with the text given.
    fn outcome(
        &mut self,
        s: &Map<String, Value>,
        label: &str,
        done: Result<Value, String>,
    ) -> Result<(), String> {
        let broken = self.game.take_violations();
        if !broken.is_empty() {
            return Err(format!("{label}: the game broke invariants: {broken:?}"));
        }
        match (s.get("error"), done) {
            (None | Some(Value::Bool(false)), Ok(v)) => {
                if let Some(name) = s.get("as").and_then(Value::as_str) {
                    self.vars.insert(name.to_owned(), v);
                }
                Ok(())
            }
            (None | Some(Value::Bool(false)), Err(e)) => Err(format!("{label}: {e}")),
            (Some(Value::Bool(true)), Err(_)) => Ok(()),
            (Some(Value::String(part)), Err(e)) if e.contains(part.as_str()) => Ok(()),
            (Some(Value::String(part)), Err(e)) => {
                Err(format!("{label}: expected an error with {part:?}, got: {e}"))
            }
            (Some(_), Ok(v)) => {
                Err(format!("{label}: expected an error, but it succeeded: {}", matchers::show(&v)))
            }
            (Some(_), Err(e)) => Err(format!("{label}: error is true or a text ({e})")),
        }
    }

    fn check(&mut self, s: &Map<String, Value>, label: &str) -> Result<(), String> {
        let (what, subject) = match &s["check"] {
            Value::String(var) => {
                let v = self
                    .vars
                    .get(var)
                    .cloned()
                    .ok_or_else(|| format!("{label}: no variable {var}"))?;
                (var.clone(), v)
            }
            Value::Object(_) => {
                let q = Value::Object(self.args(Some(&s["check"]), label)?);
                let what = q.get("what").and_then(Value::as_str).unwrap_or("?").to_owned();
                let v = inspect::inspect(&self.game, &q)
                    .map_err(|e| format!("{label}: {}", e.message))?;
                (what, v)
            }
            _ => return Err(format!("{label}: check is a query table or a variable")),
        };
        let segs = match s.get("path") {
            None => Vec::new(),
            Some(Value::String(p)) => path::parse(p).map_err(|e| format!("{label}: {e}"))?,
            Some(_) => return Err(format!("{label}: path is a string")),
        };
        let at = path::get(&subject, &segs);
        let mut spec = Map::new();
        for (k, v) in
            s.iter().filter(|(k, _)| MATCHERS.contains(&k.as_str()) || WITH.contains(&k.as_str()))
        {
            spec.insert(k.clone(), self.value(v, label)?);
        }
        let place =
            s.get("path").and_then(Value::as_str).map(|p| format!(" {p}")).unwrap_or_default();
        matchers::check(at, &spec).map_err(|e| format!("{label} ({what}{place}): {e}"))?;
        if let (Some(name), Some(v)) = (s.get("as").and_then(Value::as_str), at) {
            self.vars.insert(name.to_owned(), v.clone());
        }
        Ok(())
    }

    /// A step's arguments, expressions evaluated, with `at` turned into `x` and `y`.
    fn args(&mut self, v: Option<&Value>, label: &str) -> Result<Map<String, Value>, String> {
        let mut out = match v {
            None => Map::new(),
            Some(v) => match self.value(v, label)? {
                Value::Object(m) => m,
                _ => return Err(format!("{label}: args is a table")),
            },
        };
        if let Some(at) = out.shift_remove("at") {
            if out.contains_key("x") || out.contains_key("y") {
                return Err(format!("{label}: at gives x and y; do not give them too"));
            }
            let (x, y) = self.tile(&at).map_err(|e| format!("{label}: {e}"))?;
            out.insert("x".into(), json!(x));
            out.insert("y".into(), json!(y));
        }
        Ok(out)
    }

    /// A value with its expressions evaluated; `==` at the start of a string is a literal `=`.
    fn value(&mut self, v: &Value, label: &str) -> Result<Value, String> {
        Ok(match v {
            Value::String(s) if s.starts_with("==") => Value::String(s[1..].to_owned()),
            Value::String(s) if s.starts_with('=') => {
                expr::eval(&s[1..], self).map_err(|e| format!("{label}: {e}"))?
            }
            Value::Array(a) => {
                Value::Array(a.iter().map(|x| self.value(x, label)).collect::<Result<_, _>>()?)
            }
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| Ok((k.clone(), self.value(x, label)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }
}

impl Env for Runner<'_> {
    fn vars(&self) -> &Map<String, Value> {
        &self.vars
    }

    /// A tile reference: text for the map's frame, or a selector table for `find_tiles`, which
    /// takes its `pick`-th answer (the first by default).
    fn tile(&mut self, reference: &Value) -> Result<(i32, i32), String> {
        match reference {
            Value::String(s) => self.frame.resolve(s),
            Value::Object(sel) => {
                let mut q = sel.clone();
                let pick = match q.shift_remove("pick") {
                    None => 0,
                    Some(p) => {
                        p.as_u64().and_then(|p| usize::try_from(p).ok()).ok_or("pick is a count")?
                    }
                };
                if let Some(at) = q.shift_remove("at") {
                    let (x, y) = self.tile(&at)?;
                    q.insert("x".into(), json!(x));
                    q.insert("y".into(), json!(y));
                }
                q.insert("what".into(), json!("find_tiles"));
                let found =
                    inspect::inspect(&self.game, &Value::Object(q)).map_err(|e| e.message)?;
                let hit = found.get(pick).ok_or_else(|| {
                    format!("the selector {} finds no tile {pick}", matchers::show(reference))
                })?;
                let n = |k: &str| {
                    hit.get(k).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok())
                };
                n("x").zip(n("y")).ok_or_else(|| "find_tiles gave no coordinates".to_owned())
            }
            other => Err(format!("{} is no tile reference", matchers::show(other))),
        }
    }
}

fn error_expected(s: &Map<String, Value>) -> bool {
    s.get("error").is_some_and(|e| e != &Value::Bool(false))
}

/// A game from the script's settings over the runner's defaults, with `overrides` on top; the
/// bare prelude clears every unit and every barbarian camp.
fn make_game(
    script: &Script,
    rules: &'static Ruleset,
    doc: &Value,
    overrides: &Map<String, Value>,
) -> Result<Game, String> {
    let mut cfg = Map::new();
    cfg.insert("seed".into(), json!(1));
    cfg.insert("players".into(), json!([{}, {}]));
    if script.bare {
        cfg.insert("city_states".into(), json!(0));
        cfg.insert("barbarians".into(), json!("off"));
        cfg.insert("ruins".into(), json!(false));
    }
    for (k, v) in script.config.iter().chain(overrides) {
        cfg.insert(k.clone(), v.clone());
    }
    if let Some(Value::Array(players)) = cfg.get_mut("players") {
        for p in players.iter_mut().filter_map(Value::as_object_mut) {
            if p.get("nation").is_none_or(|n| !py::truthy(n)) {
                p.insert("nation".into(), json!("BenchmarkCiv"));
            }
        }
    }
    cfg.insert("map".into(), doc.clone());
    let mut g = new_game(rules, &cfg)?;
    g.set_debug_options(DebugOptions::ALL);
    if script.bare {
        testops::apply(
            &mut g,
            &json!([{"op": "clear_units", "player": "all"}, {"op": "clear_camps"}]),
        )
        .map_err(|e| e.message)?;
    }
    let broken = g.check_invariants();
    if !broken.is_empty() {
        return Err(format!("the new game breaks invariants: {broken:?}"));
    }
    Ok(g)
}
