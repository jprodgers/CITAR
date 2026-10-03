//! The interpreter: plays a script's steps on the Rust engine, as `tests/rulescript.py` plays
//! them on the Python one.
//!
//! The `bot` step (DESIGN.md P2.3.11, `tests/rules/README.md`) plays a seat's compiled bot as a
//! host does: `turn` drives that seat alone with a seat limit of 1, so the drive ends its turn;
//! `respond` is `Game::answer`; `advice` is `citar_bot::advice`. Its checks are the Python
//! runner's, with its messages, so a step refused on one runner is refused on the other.

use std::collections::BTreeSet;
use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Owners, Tuning, VersionId};
use citar_engine::api::{ActionError, inspect, testops};
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::base::py;
use citar_engine::game::diplomacy::negotiation;
use citar_engine::game::{DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, Stop};
use citar_engine::rules::Ruleset;
use citar_engine::state::Phase;
use citar_engine::state::diplo::NegStatus;
use serde_json::{Map, Value, json};

use super::expr::{self, Env};
use super::matchers::{self, MATCHERS, WITH};
use super::tiles::Frame;
use super::{Script, intended_ids, map_doc, new_game, number_as_string, path};

/// The kinds of step: each step has exactly one of these keys.
const KINDS: [&str; 8] = ["op", "ops", "tool", "check", "new_game", "set", "repeat", "bot"];
/// Keys any step may have. `as` and `error` belong to the kinds that use them, so a check that
/// says `error` is refused rather than passing without looking.
const COMMON: [&str; 4] = ["note", "must_fail", "intended", "coerce"];

/// What a `bot` step asks the seat's bot.
const BOT_ASKS: [&str; 3] = ["turn", "respond", "advice"];

/// The bot versions a `bot` step may name.
const BOT_VERSIONS: [&str; 2] = ["basic-1", "idle"];

/// The bot's draws a `turn` step must pin (DESIGN.md P2.3.5): its params give each of these.
const PINS: [&str; 7] = [
    "tech_noise",
    "ranged_chance",
    "peace_offer_chance",
    "friend_chance",
    "friend_chance_aggr",
    "war_chance",
    "war_chance_aggr",
];

/// Plays a script; the error says which step failed and why. A script that `needs` a package is
/// refused: its steps use what that package brings to the bot, and until then the harness
/// reports it as ignored ([`super::ignored`]).
pub fn run(script: &Script) -> Result<(), String> {
    if let Some(pkg) = &script.needs {
        return Err(format!(
            "{}: needs package {pkg}, which makes it pass on the Rust engine",
            script.name
        ));
    }
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
            "bot" => &[
                "player",
                "negotiation",
                "version",
                "aggression",
                "params",
                "diplomacy",
                "as",
                "error",
            ],
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
            for key in ["args", "ops", "new_game", "params"] {
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
            "bot" => self.bot(s, label),
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

    /// A seat's bot plays its turn, answers a negotiation, or gives its advice
    /// (`tests/rules/README.md`, `bot`). The step's shape is checked first, as the Python runner
    /// checks it; what the game then refuses goes to the step's `error`.
    fn bot(&mut self, s: &Map<String, Value>, label: &str) -> Result<(), String> {
        let ask = match &s["bot"] {
            Value::String(a) if BOT_ASKS.contains(&a.as_str()) => a.clone(),
            other => {
                return Err(format!(
                    "{label}: bot is one of {}, not {}",
                    BOT_ASKS.join(", "),
                    matchers::show(other)
                ));
            }
        };
        let player =
            s.get("player").ok_or_else(|| format!("{label}: a bot step names its player"))?;
        let player = self.value(player, label)?;
        let pid =
            whole(&player).and_then(|n| u8::try_from(n).ok()).map(PlayerId).ok_or_else(|| {
                format!("{label}: player {} is no player id", matchers::show(&player))
            })?;
        let version = self.value(s.get("version").unwrap_or(&json!("basic-1")), label)?;
        let version = match version.as_str() {
            Some(v) if BOT_VERSIONS.contains(&v) => VersionId::from_id(v).ok_or("a version")?,
            _ => {
                return Err(format!(
                    "{label}: version is one of {}, not {}",
                    BOT_VERSIONS.join(", "),
                    matchers::show(&version)
                ));
            }
        };
        let aggression = self.value(s.get("aggression").unwrap_or(&json!(0.4)), label)?;
        let aggression = match aggression {
            Value::Number(n) => n.as_f64().ok_or("a number")?,
            _ => return Err(format!("{label}: aggression is a number")),
        };
        let params = self.value(s.get("params").unwrap_or(&json!({})), label)?;
        let owners = self.value(s.get("diplomacy").unwrap_or(&json!({})), label)?;
        let (Some(params), Some(owners)) = (params.as_object(), owners.as_object()) else {
            return Err(format!("{label}: params and diplomacy are tables"));
        };
        let idle = version == VersionId::Idle;
        if idle && !(params.is_empty() && owners.is_empty()) {
            return Err(format!("{label}: the idle bot takes no params and no diplomacy"));
        }
        if let Some(k) = params.keys().find(|k| citar_bot::params::spec(k).is_none()) {
            return Err(format!("{label}: params: {k} is not a parameter of basic-1"));
        }
        if ask == "turn"
            && !idle
            && let Some(why) = unpinned_draws(params)
        {
            return Err(format!("{label}: a bot turn pins its draws: {why}"));
        }
        if ask == "turn" && s.contains_key("negotiation") {
            return Err(format!("{label}: a bot turn takes no negotiation"));
        }
        if ask == "advice" && idle {
            return Err(format!("{label}: the idle bot gives no advice"));
        }
        if ask == "respond" && !s.contains_key("negotiation") {
            return Err(format!("{label}: a bot respond step names its negotiation"));
        }
        let nid = match s.get("negotiation") {
            None => None,
            Some(v) => {
                let v = self.value(v, label)?;
                Some(whole(&v).ok_or_else(|| {
                    format!("{label}: negotiation {} is no negotiation id", matchers::show(&v))
                })?)
            }
        };
        let overrides = citar_bot::clean(version.id(), &Value::Object(params.clone()))
            .map_err(|e| format!("{label}: params: {e}"))?;
        let owners = Owners::from_json(&Value::Object(owners.clone()))
            .map_err(|e| format!("{label}: diplomacy: {e}"))?;
        let tuning = Arc::new(Tuning::new(version, overrides));
        let spec = BotSpec::new(version, tuning, None, Some(aggression)).with_owners(owners);
        let mut bot = Bot::new(Arc::new(spec));
        let done = match ask.as_str() {
            "turn" => self.bot_turn(&mut bot, pid, label)?,
            "respond" => {
                // A respond step names its negotiation (checked above).
                let nid = nid.unwrap_or_default();
                match NegotiationId::new(u32::try_from(nid).unwrap_or(0)) {
                    None => Err(format!("Negotiation {nid} does not exist.")),
                    Some(id) => match self.game.answer(pid, id, &mut bot) {
                        Ok((DriverOutcome::Deferred, _)) => Ok(json!({"outcome": "deferred"})),
                        Ok(_) => Ok(json!({"outcome": "done"})),
                        Err(e) => Err(e.message),
                    },
                }
            }
            _ => {
                let id = match nid {
                    None => Ok(None),
                    Some(n) => negotiation::get(&self.game, n).map(|x| Some(x.id)),
                };
                id.map_err(|e| e.message).and_then(|id| {
                    let advice = citar_bot::advice(&self.game, pid, bot.spec(), id);
                    serde_json::to_value(advice).map_err(|e| e.to_string())
                })
            }
        };
        self.outcome(s, label, done)
    }

    /// A bot's turn as a host plays a bot seat: the seat alone driven, a seat at a time, until
    /// its turn has ended. Returns `{turn, current}` after it, or why the turn was not the
    /// seat's to play.
    ///
    /// A negotiation the seat is in that waits on a seat nobody drives stops the drive
    /// (`AwaitingReply`); the runner closes it, as a host does when its wait runs out, and drives
    /// on (Python's bot withdrew it before ending its turn, `_settle_chats`). One that touches
    /// what the seat's language model owns, waiting on either side, is not the bot's to settle:
    /// the turn does not end, as Python's `end_turn` then refused.
    fn bot_turn(
        &mut self,
        bot: &mut Bot,
        pid: PlayerId,
        label: &str,
    ) -> Result<Result<Value, String>, String> {
        if self.game.phase() != Phase::Playing || self.game.current() != pid {
            return Ok(Err(format!("It is not player {}'s turn.", pid.0)));
        }
        let n = self.game.state().players().len();
        // A bound on the stops a turn can make: each closes a negotiation or ends the turn.
        for _ in 0..1_000 {
            let mut d = Drivers::none(n).with(pid, &mut *bot);
            let stop = self.game.drive(&mut d, DriveOptions::default().with_seat_limit(1));
            drop(d);
            let (stop, _) =
                stop.map_err(|e| format!("{label}: the bot's turn did not end: {}", e.message))?;
            match stop {
                Stop::HybridDiplomat(_) => {}
                Stop::AwaitingReply { nids, .. } => {
                    for nid in nids {
                        let models = self
                            .game
                            .negotiation(nid)
                            .is_some_and(|x| x.awaiting == Some(pid) || !bot.spec().owners.owns(x));
                        if models {
                            return Err(format!(
                                "{label}: the bot's turn did not end: negotiation {} waits on                                  the seat's language model",
                                nid.get()
                            ));
                        }
                        self.game
                            .close_negotiation(
                                nid,
                                NegStatus::Expired,
                                "No answer came before the turn ended.",
                                None,
                            )
                            .map_err(|e| format!("{label}: {}", e.message))?;
                    }
                }
                _ => {
                    return Ok(Ok(
                        json!({"turn": self.game.turn(), "current": self.game.current().0}),
                    ));
                }
            }
        }
        Err(format!("{label}: the bot's turn did not end after 1,000 drives"))
    }

    /// A tool call as a host makes one, through `Game::execute`: the caller checked, the
    /// arguments coerced, then the query's answer or the typed action through the one pipeline.
    fn tool(&mut self, pid: PlayerId, name: &str, args: &Value) -> Result<Value, ActionError> {
        self.game.execute(pid, name, args).map(|done| done.result)
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

/// A whole number, as the Python runner reads a player or a negotiation id: an int, or a float
/// that is whole; not a boolean.
fn whole(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            let whole = f.is_finite() && f.trunc().to_bits() == f.to_bits() && f.abs() < 9.0e15;
            #[allow(clippy::cast_possible_truncation, reason = "checked whole and in range")]
            whole.then_some(f as i64)
        }),
        _ => None,
    }
}

/// Why a bot turn with these params could draw a result a script checks, or `None` when every
/// draw is pinned (`tests/rulescript.py`'s `unpinned_draws`, DESIGN.md P2.3.5): the tech noise
/// 0; ranged or melee, a peace offer, a friendship and a war each with a chance of 0 or 1; the
/// aggression's share of those chances 0; and no `war_prep_rate` putting the war chance
/// strictly between 0 and 1.
#[allow(clippy::float_cmp, reason = "a pinned chance is exactly 0 or 1")]
fn unpinned_draws(params: &Map<String, Value>) -> Option<String> {
    let missing: Vec<&str> = PINS.iter().copied().filter(|k| !params.contains_key(*k)).collect();
    if !missing.is_empty() {
        return Some(format!("params must give {}", missing.join(", ")));
    }
    let num = |k: &str| params.get(k).and_then(|v| v.as_f64().filter(|_| v.is_number()));
    if let Some(k) = PINS.iter().find(|k| num(k).is_none()) {
        return Some(format!("{k} is a number"));
    }
    let at = |k: &str| num(k).unwrap_or(f64::NAN);
    if at("tech_noise") != 0.0 {
        return Some("tech_noise is 0".to_owned());
    }
    for k in ["ranged_chance", "peace_offer_chance", "friend_chance", "war_chance"] {
        if at(k) != 0.0 && at(k) != 1.0 {
            return Some(format!("{k} is 0 or 1"));
        }
    }
    for k in ["friend_chance_aggr", "war_chance_aggr"] {
        if at(k) != 0.0 {
            return Some(format!("{k} is 0"));
        }
    }
    let rate = match params.get("war_prep_rate") {
        None => citar_bot::Params::schema_defaults().war_prep_rate,
        Some(v) => v.as_f64().filter(|_| v.is_number()).unwrap_or(f64::NAN),
    };
    let chance = at("war_chance") * rate;
    if rate.is_nan() || (chance > 0.0 && chance < 1.0) {
        return Some("war_prep_rate keeps the war chance 0 or at least 1".to_owned());
    }
    None
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
    let broken = crate::checks::invariants(&g);
    if !broken.is_empty() {
        return Err(format!("the new game breaks invariants: {broken:?}"));
    }
    Ok(g)
}
