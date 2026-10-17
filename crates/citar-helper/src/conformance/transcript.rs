//! The transcripts (PROTOCOL.md, "Transcripts"): one behaviour each, as the frames and events both
//! sides see. [`Transcript::load`] reads one and refuses anything a player could misread: an
//! unknown key, a step with no action or two, a frame of a type its sender never sends, a matcher
//! spelled wrong, an `<id:NAME>` without a value, a setup key, an act or a state word outside the
//! vocabulary. `tests/helper_fixtures.py` is the server's copy.

use std::path::Path;

use serde_json::{Map, Value};

use super::matcher::{Matcher, matchers_in};
use crate::protocol::{ErrorCode, Sender};

/// The side whose environment an `act` changes or whose state a `state` step reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Helper,
    Hub,
}

/// What a step does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `s2h` (the server sends) or `h2s` (the helper sends): a frame pattern.
    Frame { from: Sender, pattern: Value },
    /// The sender closes the WebSocket with this code, and this reason when one is given.
    Close { by: Sender, code: u16, reason: Option<String> },
    /// The connection is lost without a close.
    Drop { by: Sender },
    /// The helper opens a new connection.
    Connect,
    /// Time passes; with `quiet`, the side under test sends nothing meanwhile.
    Wait { s: f64, quiet: bool },
    /// The fake model endpoint's next behaviour (helper side only).
    Endpoint(Map<String, Value>),
    /// The fake job host's next behaviour (helper side only).
    Host(Map<String, Value>),
    /// Something happens around one side: one of that side's [`act_words`], alone or as the one
    /// key of an object holding its argument.
    Act { side: Side, what: Value },
    /// An assertion on one side's state.
    State { side: Side, expect: Map<String, Value> },
}

/// One step.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub action: Action,
    /// It must happen within this many seconds of the previous step.
    pub within_s: Option<f64>,
    pub note: Option<String>,
    /// A frame sent broken on purpose, refused with this code.
    pub invalid: Option<ErrorCode>,
    /// A frame lost with the connection: the side under test must send it, and the scripted
    /// side does not (its receiver never gets it).
    pub lost: bool,
}

/// The package from which each side plays a transcript; `None` for a side that cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plays {
    pub helper: Option<String>,
    pub hub: Option<String>,
}

/// One transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub name: String,
    pub about: String,
    /// The design sections it shows.
    pub refs: Vec<String>,
    pub plays: Plays,
    /// Why only one side plays it.
    pub one_side: Option<String>,
    /// An example value for every `<id:NAME>`.
    pub values: Map<String, Value>,
    /// Each side's starting state, in its own terms.
    pub setup_helper: Map<String, Value>,
    pub setup_hub: Map<String, Value>,
    pub steps: Vec<Step>,
}

/// The frames that come on their own clocks (PROTOCOL.md, "Background frames"): a player skips
/// them unless the next step expects their type, and a `quiet` wait allows them.
pub const BACKGROUND: &[&str] = &["ping", "pong", "status", "power"];

/// The helper side's setup keys.
pub const HELPER_SETUP: &[&str] =
    &["config", "clock", "home", "service", "endpoint", "update_key", "host"];
/// The keys of `setup.helper.config`: the helper's own settings.
pub const HELPER_CONFIG: &[&str] =
    &["max_concurrent", "models", "quiet", "cpu", "updates", "no_llm", "stream", "no_power"];
/// The keys of `setup.helper.host`: the fake job host from the start.
pub const HELPER_HOST: &[&str] = &["verifiable", "start"];
/// The hub side's setup keys.
pub const HUB_SETUP: &[&str] = &[
    "machine",
    "token",
    "rate",
    "jobs",
    "update",
    "clock",
    "ping_s",
    "local_executor",
    "verification",
    "answer_pings",
];
/// The keys of `setup.hub.machine`.
pub const HUB_MACHINE: &[&str] = &["settings", "key", "trust"];
/// What can happen around the helper (`act` with side `helper`).
pub const HELPER_ACTS: &[&str] = &["quiet_begins", "stop"];
/// What can happen on the server (`act` with side `hub`).
pub const HUB_ACTS: &[&str] = &[
    "submit_request",
    "force_request",
    "cancel_request",
    "drop_connection",
    "queue_job",
    "force_offer",
    "lab_stop",
    "abort_run",
    "expire_run",
    "set_settings",
    "rescan",
    "reset_key",
    "revoke_token",
    "disable_machine",
    "second_connection",
    "benchmark_start",
    "benchmark_end",
    "manifest_arrives",
    "delete_experiment",
];
/// What a `state` step may expect of the helper.
pub const HELPER_STATE: &[&str] =
    &["in_flight", "exit", "held_results", "running", "downloads", "logged"];
/// What a `state` step may expect of the hub.
pub const HUB_STATE: &[&str] = &[
    "slot_free",
    "draining",
    "flags",
    "step",
    "requeued",
    "request",
    "llm_slots",
    "cpu_threads",
    "cpu_percent",
    "memory_mb",
    "work",
    "cpu_enabled",
    "quiet",
    "models",
    "loaded",
    "last_report",
    "power_rows",
    "key",
    "audit",
    "token_lookups",
    "audit_rows",
    "rate_count",
    "run",
    "job",
    "result_lines",
    "backoff",
    "mem_mb_raised",
    "claim_failures",
    "events",
    "update_status",
    "memory_per_run_mb",
];
/// The longest close reason, in bytes (a WebSocket close frame's limit).
pub const MAX_CLOSE_REASON: usize = 123;

/// The act words of one side.
#[must_use]
pub fn act_words(side: Side) -> &'static [&'static str] {
    match side {
        Side::Helper => HELPER_ACTS,
        Side::Hub => HUB_ACTS,
    }
}

/// The state words of one side.
#[must_use]
pub fn state_words(side: Side) -> &'static [&'static str] {
    match side {
        Side::Helper => HELPER_STATE,
        Side::Hub => HUB_STATE,
    }
}
/// An `endpoint` step's keys.
pub const ENDPOINT_KEYS: &[&str] = &[
    "models",
    "respond",
    "expect_request",
    "absent",
    "expect_disconnect_within_ms",
    "expect_no_tokens_after",
];
/// An endpoint's `respond` keys.
pub const RESPOND_KEYS: &[&str] = &["stream", "delay_ms", "json", "status", "body", "hang"];
/// A `host` step's keys.
pub const HOST_KEYS: &[&str] = &["progress", "finish", "crash_host", "hang"];

const TOP_KEYS: &[&str] =
    &["name", "about", "refs", "plays", "one_side", "values", "setup", "steps"];
const ACTIONS: &[&str] =
    &["s2h", "h2s", "close", "drop", "connect", "wait", "endpoint", "host", "act", "state"];
const MODIFIERS: &[&str] = &["within_s", "note", "invalid", "lost"];

fn unknown_keys(map: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => {
            Err(format!("{what} has the unknown key {k:?} (allowed: {})", allowed.join(", ")))
        }
        None => Ok(()),
    }
}

fn object<'v>(v: Option<&'v Value>, what: &str) -> Result<&'v Map<String, Value>, String> {
    v.and_then(Value::as_object).ok_or_else(|| format!("{what} must be an object"))
}

fn text(v: Option<&Value>, what: &str) -> Result<String, String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{what} must be a non-empty string"))
}

fn sender(v: Option<&Value>, what: &str) -> Result<Sender, String> {
    match v.and_then(Value::as_str) {
        Some("server") => Ok(Sender::Server),
        Some("helper") => Ok(Sender::Helper),
        _ => Err(format!("{what} must be \"server\" or \"helper\"")),
    }
}

fn side(v: Option<&Value>, what: &str) -> Result<Side, String> {
    match v.and_then(Value::as_str) {
        Some("helper") => Ok(Side::Helper),
        Some("hub") => Ok(Side::Hub),
        _ => Err(format!("{what} must be \"helper\" or \"hub\"")),
    }
}

fn package(v: Option<&Value>, what: &str) -> Result<Option<String>, String> {
    match v {
        Some(Value::Null) => Ok(None),
        Some(Value::String(p))
            if p.len() == 4
                && p.starts_with("4-")
                && p[2..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            Ok(Some(p.clone()))
        }
        _ => Err(format!("{what} must be a package (\"4-05\") or null")),
    }
}

impl Transcript {
    /// Reads and checks the transcript at `path`; its file name must be its `name`.
    ///
    /// # Errors
    /// What is wrong with it, naming the file.
    pub fn load(path: &Path) -> Result<Transcript, String> {
        let shown = path.display();
        let raw = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
        let value: Value = serde_json::from_str(&raw).map_err(|e| format!("{shown}: {e}"))?;
        let t = Transcript::from_value(&value).map_err(|e| format!("{shown}: {e}"))?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if stem != t.name {
            return Err(format!(
                "{shown}: the file is not named after the transcript, {:?}",
                t.name
            ));
        }
        Ok(t)
    }

    /// Checks and reads a transcript's JSON.
    ///
    /// # Errors
    /// What is wrong with it.
    pub fn from_value(value: &Value) -> Result<Transcript, String> {
        let top = object(Some(value), "a transcript")?;
        unknown_keys(top, TOP_KEYS, "the transcript")?;
        let name = text(top.get("name"), "name")?;
        let about = text(top.get("about"), "about")?;
        let refs = top
            .get("refs")
            .and_then(Value::as_array)
            .filter(|r| !r.is_empty())
            .and_then(|r| {
                r.iter().map(|x| x.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()
            })
            .ok_or("refs must be a non-empty list of design sections")?;
        let plays_map = object(top.get("plays"), "plays")?;
        unknown_keys(plays_map, &["helper", "hub"], "plays")?;
        let plays = Plays {
            helper: package(plays_map.get("helper"), "plays.helper")?,
            hub: package(plays_map.get("hub"), "plays.hub")?,
        };
        let one_side = match top.get("one_side") {
            None => None,
            Some(v) => Some(text(Some(v), "one_side")?),
        };
        match (&plays.helper, &plays.hub, &one_side) {
            (None, None, _) => return Err("no side plays it".to_owned()),
            (Some(_), Some(_), Some(_)) => {
                return Err("both sides play it, so it has no one_side".to_owned());
            }
            (None, _, None) | (_, None, None) => {
                return Err("one side does not play it: one_side must say why".to_owned());
            }
            _ => {}
        }
        let values = object(top.get("values"), "values")?.clone();
        let setup = object(top.get("setup"), "setup")?;
        unknown_keys(setup, &["helper", "hub"], "setup")?;
        let setup_helper = object(setup.get("helper"), "setup.helper")?.clone();
        let setup_hub = object(setup.get("hub"), "setup.hub")?.clone();
        check_setup(&setup_helper, &setup_hub)?;
        let steps_raw = top
            .get("steps")
            .and_then(Value::as_array)
            .filter(|s| !s.is_empty())
            .ok_or("steps must be a non-empty list")?;
        let mut steps = Vec::with_capacity(steps_raw.len());
        for (i, s) in steps_raw.iter().enumerate() {
            steps.push(step(s).map_err(|e| format!("step {i}: {e}"))?);
        }
        let mut found = Vec::new();
        matchers_in(top.get("steps").unwrap_or(&Value::Null), "steps", &mut found);
        matchers_in(&Value::Object(setup.clone()), "setup", &mut found);
        for (at, m) in found {
            match m {
                Err(e) => return Err(format!("{at}: {e}")),
                Ok(Matcher::Id(n)) if !values.contains_key(&n) => {
                    return Err(format!("{at}: <id:{n}> has no example in values"));
                }
                Ok(_) => {}
            }
        }
        Ok(Transcript {
            name,
            about,
            refs,
            plays,
            one_side,
            values,
            setup_helper,
            setup_hub,
            steps,
        })
    }

    /// Every frame step: who sends it, its pattern and whether it is broken on purpose.
    pub fn frames(&self) -> impl Iterator<Item = (usize, Sender, &Value, Option<ErrorCode>)> {
        self.steps.iter().enumerate().filter_map(|(i, s)| match &s.action {
            Action::Frame { from, pattern } => Some((i, *from, pattern, s.invalid)),
            _ => None,
        })
    }
}

fn check_setup(helper: &Map<String, Value>, hub: &Map<String, Value>) -> Result<(), String> {
    unknown_keys(helper, HELPER_SETUP, "setup.helper")?;
    unknown_keys(hub, HUB_SETUP, "setup.hub")?;
    if let Some(config) = helper.get("config") {
        unknown_keys(
            object(Some(config), "setup.helper.config")?,
            HELPER_CONFIG,
            "setup.helper.config",
        )?;
    }
    if let Some(host) = helper.get("host") {
        unknown_keys(object(Some(host), "setup.helper.host")?, HELPER_HOST, "setup.helper.host")?;
    }
    if let Some(machine) = hub.get("machine") {
        unknown_keys(
            object(Some(machine), "setup.hub.machine")?,
            HUB_MACHINE,
            "setup.hub.machine",
        )?;
    }
    Ok(())
}

/// The word an `act` step's `do` names: a string, or the one key of an object.
fn act_word(what: &Value) -> Result<&str, String> {
    match what {
        Value::String(w) => Ok(w),
        Value::Object(m) if m.len() == 1 => Ok(m.keys().next().map_or("", String::as_str)),
        _ => Err("act.do must be a word or an object of one word".to_owned()),
    }
}

fn step(v: &Value) -> Result<Step, String> {
    let map = object(Some(v), "a step")?;
    let mut allowed = ACTIONS.to_vec();
    allowed.extend_from_slice(MODIFIERS);
    unknown_keys(map, &allowed, "the step")?;
    let actions: Vec<&str> = ACTIONS.iter().copied().filter(|a| map.contains_key(*a)).collect();
    let [kind] = actions.as_slice() else {
        return Err(format!("a step has exactly one action, not {actions:?}"));
    };
    let body = &map[*kind];
    let action = match *kind {
        "s2h" | "h2s" => {
            let from = if *kind == "s2h" { Sender::Server } else { Sender::Helper };
            let pattern = object(Some(body), "a frame")?;
            let t = pattern.get("t").and_then(Value::as_str).ok_or("a frame needs a literal t")?;
            if !from.types().contains(&t) {
                return Err(format!("the {} never sends {t:?}", from.as_str()));
            }
            Action::Frame { from, pattern: body.clone() }
        }
        "close" => {
            let b = object(Some(body), "close")?;
            unknown_keys(b, &["by", "code", "reason"], "close")?;
            let code = b
                .get("code")
                .and_then(Value::as_u64)
                .and_then(|c| u16::try_from(c).ok())
                .ok_or("close.code must be a close code")?;
            let reason = match b.get("reason") {
                None => None,
                Some(Value::String(r)) if r.len() <= MAX_CLOSE_REASON => Some(r.clone()),
                Some(_) => {
                    return Err(format!(
                        "close.reason must be a string of at most {MAX_CLOSE_REASON} bytes"
                    ));
                }
            };
            Action::Close { by: sender(b.get("by"), "close.by")?, code, reason }
        }
        "drop" => {
            let b = object(Some(body), "drop")?;
            unknown_keys(b, &["by"], "drop")?;
            Action::Drop { by: sender(b.get("by"), "drop.by")? }
        }
        "connect" => {
            if !object(Some(body), "connect")?.is_empty() {
                return Err("connect takes {}".to_owned());
            }
            Action::Connect
        }
        "wait" => {
            let b = object(Some(body), "wait")?;
            unknown_keys(b, &["s", "quiet"], "wait")?;
            let s = b
                .get("s")
                .and_then(Value::as_f64)
                .filter(|s| *s > 0.0)
                .ok_or("wait.s must be positive")?;
            let quiet = match b.get("quiet") {
                None => false,
                Some(Value::Bool(q)) => *q,
                Some(_) => return Err("wait.quiet must be a boolean".to_owned()),
            };
            Action::Wait { s, quiet }
        }
        "endpoint" => {
            let b = object(Some(body), "endpoint")?;
            unknown_keys(b, ENDPOINT_KEYS, "endpoint")?;
            if let Some(r) = b.get("respond") {
                unknown_keys(
                    object(Some(r), "endpoint.respond")?,
                    RESPOND_KEYS,
                    "endpoint.respond",
                )?;
            }
            Action::Endpoint(b.clone())
        }
        "host" => {
            let b = object(Some(body), "host")?;
            unknown_keys(b, HOST_KEYS, "host")?;
            Action::Host(b.clone())
        }
        "act" => {
            let b = object(Some(body), "act")?;
            unknown_keys(b, &["side", "do"], "act")?;
            let s = side(b.get("side"), "act.side")?;
            let what = b.get("do").ok_or("act.do must be a word or an object of one word")?;
            let word = act_word(what)?;
            if !act_words(s).contains(&word) {
                return Err(format!(
                    "act.do {word:?} is not a word of that side (allowed: {})",
                    act_words(s).join(", ")
                ));
            }
            Action::Act { side: s, what: what.clone() }
        }
        // "state", the only action left.
        _ => {
            let b = object(Some(body), "state")?;
            unknown_keys(b, &["side", "expect"], "state")?;
            let s = side(b.get("side"), "state.side")?;
            let expect = object(b.get("expect"), "state.expect")?;
            unknown_keys(expect, state_words(s), "state.expect")?;
            Action::State { side: s, expect: expect.clone() }
        }
    };
    let within_s = match map.get("within_s") {
        None => None,
        Some(v) => Some(v.as_f64().filter(|s| *s > 0.0).ok_or("within_s must be positive")?),
    };
    let note = match map.get("note") {
        None => None,
        Some(v) => Some(text(Some(v), "note")?),
    };
    let invalid = match map.get("invalid") {
        None => None,
        Some(v) => {
            if !matches!(action, Action::Frame { .. }) {
                return Err("only a frame step can be invalid".to_owned());
            }
            let code = v
                .as_str()
                .and_then(ErrorCode::parse)
                .ok_or("invalid must be a protocol error code")?;
            Some(code)
        }
    };
    let lost = match map.get("lost") {
        None => false,
        Some(Value::Bool(true)) if matches!(action, Action::Frame { .. }) => true,
        Some(_) => return Err("lost is true, on a frame step only".to_owned()),
    };
    Ok(Step { action, within_s, note, invalid, lost })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({
            "name": "t", "about": "a test", "refs": ["P4.2.2"], "plays": {"helper": "4-03", "hub": "4-05"},
            "values": {"req": "0123456789abcdef"}, "setup": {"helper": {}, "hub": {}},
            "steps": [{"s2h": {"t": "cancel", "id": "<id:req>"}}]
        })
    }

    fn with(path: &[&str], v: Value) -> Value {
        let mut t = base();
        let mut cur = &mut t;
        for p in &path[..path.len() - 1] {
            cur = cur.get_mut(*p).expect("path");
        }
        cur[path[path.len() - 1]] = v;
        t
    }

    #[test]
    fn a_good_transcript_loads_and_each_defect_is_refused() {
        let t = Transcript::from_value(&base()).expect("loads");
        assert_eq!(t.steps.len(), 1);
        for (bad, why) in [
            (with(&["values"], json!({})), "no example"),
            (with(&["plays", "hub"], Value::Null), "one_side must say why"),
            (with(&["plays", "hub"], json!("5-01")), "a package"),
            (with(&["steps"], json!([{"s2h": {"t": "hello"}}])), "never sends"),
            (with(&["steps"], json!([{"h2s": {"t": "welcome"}}])), "never sends"),
            (
                with(&["steps"], json!([{"s2h": {"t": "ping"}, "h2s": {"t": "pong"}}])),
                "exactly one",
            ),
            (with(&["steps"], json!([{"wait": {"s": 0}}])), "positive"),
            (with(&["steps"], json!([{"connect": {}, "invalid": "missing"}])), "only a frame"),
            (with(&["steps"], json!([{"s2h": {"t": "ping"}, "invalid": "broken"}])), "error code"),
            (with(&["steps"], json!([{"s2h": {"t": "ping", "x": "<int:a..>"}}])), "is not a bound"),
            (with(&["steps"], json!([{"s2h": {"t": "ping", "x": "<int:5>"}}])), "LO..HI"),
            (with(&["steps"], json!([{"endpoint": {"respond": {"jsn": {}}}}])), "unknown key"),
            (with(&["steps"], json!([{"state": {"side": "server", "expect": {}}}])), "\"hub\""),
            (with(&["setup", "helper"], json!({"confg": {}})), "unknown key"),
            (with(&["setup", "hub"], json!({"machine": {"settngs": {}}})), "unknown key"),
            (with(&["setup", "helper"], json!({"host": {"canary": false}})), "unknown key"),
            (with(&["extra"], json!(1)), "unknown key"),
            (
                with(&["steps"], json!([{"act": {"side": "hub", "do": "sumbit_request"}}])),
                "not a word",
            ),
            (with(&["steps"], json!([{"act": {"side": "helper", "do": "rescan"}}])), "not a word"),
            (
                with(&["steps"], json!([{"act": {"side": "hub", "do": {"a": 1, "b": 2}}}])),
                "one word",
            ),
            (with(&["steps"], json!([{"act": {"side": "hub", "do": 5}}])), "one word"),
            (
                with(&["steps"], json!([{"state": {"side": "hub", "expect": {"slots": 1}}}])),
                "unknown key",
            ),
            (
                with(&["steps"], json!([{"state": {"side": "helper", "expect": {"draining": 0}}}])),
                "unknown key",
            ),
            (with(&["steps"], json!([{"connect": {}, "lost": true}])), "frame step only"),
            (with(&["steps"], json!([{"s2h": {"t": "ping"}, "lost": false}])), "frame step only"),
            (
                with(&["steps"], json!([{"close": {"by": "server", "code": 4400, "reason": 1}}])),
                "123 bytes",
            ),
            (
                with(
                    &["steps"],
                    json!([{"close": {"by": "server", "code": 4400, "reason": "x".repeat(124)}}]),
                ),
                "123 bytes",
            ),
        ] {
            let e = Transcript::from_value(&bad).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
        }
        let one_side = with(&["plays", "hub"], Value::Null);
        let mut ok = one_side.clone();
        ok["one_side"] = json!("scripts a server");
        assert!(Transcript::from_value(&ok).is_ok());
        let mut both = base();
        both["one_side"] = json!("but both play");
        assert!(Transcript::from_value(&both).is_err());
    }

    #[test]
    fn acts_states_closes_and_lost_frames_load() {
        let t = Transcript::from_value(&with(
            &["steps"],
            json!([
                {"act": {"side": "hub", "do": "submit_request"}},
                {"act": {"side": "hub", "do": {"set_settings": {}}}},
                {"act": {"side": "helper", "do": "quiet_begins"}},
                {"act": {"side": "helper", "do": "stop"}},
                {"s2h": {"t": "ping"}, "lost": true},
                {"close": {"by": "server", "code": 4400, "reason": "missing machine"}},
                {"close": {"by": "helper", "code": 1000}},
                {"state": {"side": "hub", "expect": {"slot_free": true, "draining": 0}}},
                {"state": {"side": "helper", "expect": {"in_flight": 0}}}
            ]),
        ))
        .expect("loads");
        assert!(t.steps[4].lost && !t.steps[0].lost);
        assert_eq!(
            t.steps[5].action,
            Action::Close {
                by: Sender::Server,
                code: 4400,
                reason: Some("missing machine".to_owned())
            }
        );
        assert_eq!(
            t.steps[6].action,
            Action::Close { by: Sender::Helper, code: 1000, reason: None }
        );
    }
}
