//! The `briefing` group (DESIGN.md 9.2): the turn briefing and the turn's progress a model reads,
//! for the first two living major civilizations (`briefing.briefing(g, pid)` and
//! `briefing.turn_progress(g, pid)`).
//!
//! Package 1d-03 answers it with `Game::briefing` and `Game::turn_progress`.
//!
//! A briefing is a hundred lines of prose, so both texts, Python's and Rust's, are read into the
//! same structure before they are compared ([`structure`]): its sections, a city's line by field
//! (its yields, growth, what it builds and for how many turns), a unit's by field (its orders and
//! its health), and the lists a line holds (the policies, the alerts, what the local map shows,
//! the points of interest, the civilizations met, the city-states, the deals) as lists. A
//! difference is then reported where it is, a number as a number, and an explanation can name
//! the field it explains. The lists whose order is no rule (the policies, a religion's beliefs, a
//! city's specialists, the civilizations met, the alerts) compare as sets (`compare::spec`).
//! What does not read as expected is kept as its line, under `other`, so nothing is dropped.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use citar_engine::base::ids::PlayerId;

use super::{AnswerError, AnswerModule, Ctx, recorded};
use crate::Group;

/// The `briefing` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Briefing;

impl AnswerModule for Briefing {
    fn group(&self) -> Group {
        Group::Briefing
    }

    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        let raw = recorded(Group::Briefing, cx)?;
        let civs = raw
            .get("civs")
            .and_then(Value::as_array)
            .ok_or_else(|| AnswerError::new("the recorded briefings have no civs"))?;
        let mut out = Vec::new();
        for c in civs {
            let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
            out.push(json!({
                "pid": c.get("pid").cloned().unwrap_or(Value::Null),
                "briefing": structure(text("briefing")),
                "turn_progress": text("turn_progress"),
            }));
        }
        Ok(Cow::Owned(json!({ "civs": out })))
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let civs = expected
            .get("civs")
            .and_then(Value::as_array)
            .ok_or_else(|| AnswerError::new("the recorded briefings have no civs"))?;
        let mut out = Vec::new();
        for c in civs {
            let pid = c
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| AnswerError::new("a recorded briefing has no pid"))?;
            let p = PlayerId(pid);
            out.push(json!({
                "pid": pid,
                "briefing": structure(&g.briefing(p)),
                "turn_progress": g.turn_progress(p),
            }));
        }
        Ok(json!({ "civs": out }))
    }
}

/// The sections a briefing may have, by the start of their heading, and the key each is filed
/// under.
const SECTIONS: [(&str, &str); 11] = [
    ("ALERTS", "alerts"),
    ("CITIES", "cities"),
    ("UNITS", "units"),
    ("OPTIONS FOR UNITS", "unit_options"),
    ("IDLE CITIES", "city_options"),
    ("AVAILABLE TECHS", "techs"),
    ("LOCAL MAP", "map"),
    ("EVENTS", "events"),
    ("DIPLOMACY", "diplomacy"),
    ("TO DO", "todo"),
    ("YOUR NOTEBOOK", "notebook"),
];

fn city_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^  (\[#\d+\] .* \(-?\d+,-?\d+\) pop \d+)(?: (.*))?$").unwrap_or_else(|e| {
            unreachable!("a fixed pattern: {e}");
        })
    })
}

fn build_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^building (.*) \((-?\d+)/(-?\d+), (-?\d+) turns\)(?: then (.*))?$")
            .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
    })
}

fn unit_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^  ([ *]) (\[#\d+\] .* \(-?\d+,-?\d+\)) moves (\S+) · (.*)$")
            .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
    })
}

fn option_item() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(.*) (-?\d+)/(-?\d+)t$")
            .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
    })
}

fn unit_entry() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(unit #\d+ .*) hp (-?\d+)$")
            .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
    })
}

/// A number as JSON, or the text where it is none.
fn number(s: &str) -> Value {
    s.parse::<i64>().map_or_else(|_| json!(s), |n| json!(n))
}

/// A list of the parts of `s` between `sep`s.
fn parts(s: &str, sep: &str) -> Value {
    if s.is_empty() {
        return json!([]);
    }
    Value::Array(s.split(sep).map(|x| json!(x)).collect())
}

/// A city's line by field: which city, its tags and specialists, its food and growth, each
/// yield as a number, and what it builds (the item, its progress and cost, the turns, what
/// follows), or the line itself if it does not read so.
fn city(line: &str) -> Value {
    let segs: Vec<&str> = line.split(" · ").collect();
    let Some(caps) = segs.first().and_then(|h| city_line().captures(h)) else { return json!(line) };
    if segs.len() != 8 {
        return json!(line);
    }
    let mut m = Map::new();
    m.insert("city".into(), json!(&caps[1]));
    let tags = caps.get(2).map_or("", |t| t.as_str());
    let (tags, specialists) = match tags.find("specialists ") {
        Some(i) => (tags[..i].trim_end_matches(", "), &tags[i + "specialists ".len()..]),
        None => (tags, ""),
    };
    m.insert("tags".into(), parts(tags, ", "));
    m.insert("specialists".into(), parts(specialists, ", "));
    let food = segs[1].strip_prefix("food ").unwrap_or(segs[1]);
    let (amount, growth) = food.split_once(" (").unwrap_or((food, ""));
    m.insert("food".into(), json!(amount));
    m.insert("growth".into(), json!(growth.trim_end_matches(')')));
    for (k, seg) in ["prod", "gold", "sci", "cul", "faith"].iter().zip(&segs[2..7]) {
        let v = seg.strip_prefix(k).map_or_else(|| json!(seg), |n| number(n.trim()));
        m.insert((*k).into(), v);
    }
    let build = segs[7];
    let b = match build_line().captures(build) {
        Some(c) => json!({
            "item": &c[1],
            "progress": number(&c[2]),
            "cost": number(&c[3]),
            "turns": number(&c[4]),
            "then": c.get(5).map_or("", |x| x.as_str()),
        }),
        None => json!(build),
    };
    m.insert("build".into(), b);
    Value::Object(m)
}

/// A unit's line by field: whether it wants orders, which unit, its moves, its orders, its
/// health and the rest, or the line itself if it does not read so.
fn unit(line: &str) -> Value {
    let Some(c) = unit_line().captures(line) else { return json!(line) };
    let rest = &c[4];
    let (activity, extra) = rest.split_once(" · ").unwrap_or((rest, ""));
    let mut m = Map::new();
    m.insert("idle".into(), json!(&c[1] == "*"));
    m.insert("unit".into(), json!(&c[2]));
    m.insert("moves".into(), json!(&c[3]));
    m.insert("activity".into(), json!(activity));
    let mut others = Vec::new();
    for e in extra.split(", ").filter(|e| !e.is_empty()) {
        match e.strip_prefix("hp ") {
            Some(n) => {
                m.insert("hp".into(), number(n));
            }
            None => others.push(json!(e)),
        }
    }
    m.insert("extra".into(), Value::Array(others));
    Value::Object(m)
}

/// An idle city's line of what it can build: which city, then each kind's items with their cost
/// and turns, or the line itself if it does not read so.
fn city_option(line: &str) -> Value {
    let Some((city, rest)) = line.strip_prefix("  ").and_then(|l| l.split_once(": ")) else {
        return json!(line);
    };
    let mut m = Map::new();
    m.insert("city".into(), json!(city));
    for kind in rest.split(" | ") {
        let Some((k, items)) = kind.split_once(": ") else { return json!(line) };
        let items: Vec<Value> = items
            .split(", ")
            .map(|it| match option_item().captures(it) {
                Some(c) => json!({"item": &c[1], "cost": number(&c[2]), "turns": number(&c[3])}),
                None => json!(it),
            })
            .collect();
        m.insert(k.into(), Value::Array(items));
    }
    Value::Object(m)
}

/// An entry of what the local map shows: a unit's with its health apart.
fn window_entry(e: &str) -> Value {
    match unit_entry().captures(e) {
        Some(c) => json!({"unit": &c[1], "hp": number(&c[2])}),
        None => json!(e),
    }
}

/// Pushes `v` onto the list under `key`, making it.
fn push(m: &mut Map<String, Value>, key: &str, v: Value) {
    if let Value::Array(a) = m.entry(key).or_insert_with(|| json!([])) {
        a.push(v);
    }
}

/// The head's lines: the empire's line of luxuries, score and policies, and the religion's, by
/// field; the rest as they are.
fn head_line(m: &mut Map<String, Value>, line: &str) {
    if let Some(rest) = line.strip_prefix("Luxuries: ") {
        let segs: Vec<&str> = rest.split(" · ").collect();
        if let [lux, score, pol] = segs.as_slice()
            && let (Some(score), Some(pol)) =
                (score.strip_prefix("Score "), pol.strip_prefix("Policies: "))
        {
            m.insert(
                "empire".into(),
                json!({"luxuries": parts(lux, ", "), "score": number(score), "policies": parts(pol, ", ")}),
            );
            return;
        }
    }
    if let Some(rest) = line.strip_prefix("Religion: ")
        && let Some((name, beliefs)) = rest.split_once(" — beliefs: ")
    {
        m.insert("religion".into(), json!({"name": name, "beliefs": parts(beliefs, ", ")}));
        return;
    }
    push(m, "head", json!(line));
}

/// A line of the diplomacy section, filed by what it is.
fn diplomacy_line(m: &mut Map<String, Value>, line: &str) {
    if let Some(cs) = line.strip_prefix("  City-states: ") {
        m.insert("city_states".into(), parts(cs, "; "));
    } else if let Some(d) = line.strip_prefix("  Active deals: ") {
        m.insert("deals".into(), parts(d, " | "));
    } else if line.starts_with("  - player ") || line.starts_with("  You have not met") {
        push(m, "players", json!(line));
    } else if line.starts_with("  ✉") {
        push(m, "messages", json!(line));
    } else if line.starts_with("  ⚖") {
        push(m, "negotiations", json!(line));
    } else {
        push(m, "other", json!(line));
    }
}

/// A briefing's text as the structure refcheck compares (see the module doc).
#[must_use]
pub fn structure(text: &str) -> Value {
    let mut m = Map::new();
    let mut section: Option<&str> = None;
    let mut headers = Vec::new();
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        // A section begins with an empty line and its heading; the notebook runs to the end.
        if section != Some("notebook") && line.is_empty() {
            let next = lines.peek().copied().unwrap_or("");
            if let Some(&(_, key)) = SECTIONS.iter().find(|(h, _)| next.starts_with(h)) {
                lines.next();
                section = Some(key);
                match key {
                    // The to-do list is its heading.
                    "todo" => {
                        m.insert("todo".into(), json!(next));
                    }
                    _ => headers.push(json!(next)),
                }
                continue;
            }
        }
        match section {
            None => head_line(&mut m, line),
            Some("alerts") => {
                push(&mut m, "alerts", json!(line.strip_prefix("  ! ").unwrap_or(line)))
            }
            Some("cities") => push(&mut m, "cities", city(line)),
            Some("units") => push(&mut m, "units", unit(line)),
            Some("city_options") => push(&mut m, "city_options", city_option(line)),
            Some("map") => {
                if let Some(rest) = line.strip_prefix("Also in this window: ") {
                    let entries: Vec<Value> = rest.split("; ").map(window_entry).collect();
                    m.insert("window".into(), Value::Array(entries));
                } else if let Some(rest) =
                    line.strip_prefix("Known points of interest (from your capital/first unit): ")
                {
                    m.insert("poi".into(), parts(rest, "; "));
                } else {
                    push(&mut m, "map", json!(line));
                }
            }
            Some("diplomacy") => diplomacy_line(&mut m, line),
            Some(key) => push(&mut m, key, json!(line)),
        }
    }
    m.insert("headers".into(), Value::Array(headers));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_briefing_reads_into_its_fields() {
        let text = "=== TURN 50/330 (1060 BC) — Russia (player 0, Russia) — Ancient era — YOUR TURN ===\n\
            Luxuries: Marble · Score 114 · Policies: Tradition, Aristocracy\n\
            Religion: Faith of X (religion) — beliefs: A, B\n\
            \n\
            ALERTS (handle these first):\n  ! One.\n  ! Two.\n\
            \n\
            CITIES (1):\n  [#16] Moscow (12,19) pop 5 capital, HP 184/200, specialists Scientist 4, Engineer 2 · food +9 (grows in 3) · prod 6 · gold 7 · sci 8 · cul 6 · faith 0 · building Spearman (6/37, 6 turns) then Stone Works\n\
            \n\
            UNITS (1) — '*' = needs orders:\n  * [#93] Worker (12,19) moves 2.0/2.0 · automate · hp 0, embarked\n\
            \n\
            LOCAL MAP (legend: get_map legend=true):\nMap around (12,19), x 2-22, y 14-24:\ny=14   G.\n\
            Also in this window: city 'Moscow' #16 (12,19) owner Russia pop 5; unit #115 Spearman (13,19) Barbarians hp 100\n\
            Known points of interest (from your capital/first unit): a; b\n\
            \n\
            DIPLOMACY:\n  - player 1 Iroquois (Iroquois): peace; score 146; 2 cities\n  City-states: Almaty #2 (Militaristic, Neutral, influence 0)\n\
            \n\
            TO DO: this. Call end_turn when finished.\n\
            \n\
            YOUR NOTEBOOK:\nline one\n\nline two";
        let s = structure(text);
        assert_eq!(s["empire"]["policies"], json!(["Tradition", "Aristocracy"]));
        assert_eq!(s["empire"]["score"], json!(114));
        assert_eq!(s["religion"]["beliefs"], json!(["A", "B"]));
        assert_eq!(s["alerts"], json!(["One.", "Two."]));
        let c = &s["cities"][0];
        assert_eq!(c["city"], json!("[#16] Moscow (12,19) pop 5"));
        assert_eq!(c["tags"], json!(["capital", "HP 184/200"]));
        assert_eq!(c["specialists"], json!(["Scientist 4", "Engineer 2"]));
        assert_eq!((c["food"].clone(), c["growth"].clone()), (json!("+9"), json!("grows in 3")));
        assert_eq!(c["prod"], json!(6));
        assert_eq!(c["build"]["turns"], json!(6));
        assert_eq!(c["build"]["then"], json!("Stone Works"));
        let u = &s["units"][0];
        assert_eq!((u["idle"].clone(), u["hp"].clone()), (json!(true), json!(0)));
        assert_eq!(u["extra"], json!(["embarked"]));
        assert_eq!(s["window"][1]["hp"], json!(100));
        assert_eq!(s["poi"], json!(["a", "b"]));
        assert_eq!(s["city_states"][0], json!("Almaty #2 (Militaristic, Neutral, influence 0)"));
        assert_eq!(s["todo"], json!("TO DO: this. Call end_turn when finished."));
        assert_eq!(s["notebook"], json!(["line one", "", "line two"]));
        assert_eq!(s["headers"].as_array().map(Vec::len), Some(6));
        let o = city_option(
            "  [#598] Anjar: units: Worker 46/4t | wonders: Statue of Liberty 710/59t | other: Gold, Science",
        );
        assert_eq!(o["city"], json!("[#598] Anjar"));
        assert_eq!(o["wonders"][0], json!({"item": "Statue of Liberty", "cost": 710, "turns": 59}));
        assert_eq!(o["other"], json!(["Gold", "Science"]));
    }
}
