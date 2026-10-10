//! The `bot_decisions` group (DESIGN.md P2.3.11, packages 2-01b and 2-03): the bot's
//! deterministic sub-decisions on each state, the values of stage 1 compared with what the
//! Python bot gave.
//!
//! `scripts/refcheck/bot_dump.py` recorded them from a fresh `BasicBot(seed=0)` with
//! `tech_noise` 0, its tool calls recorded instead of made, for every living major: the 12
//! committed states in `refcheck/bot_decisions.json.gz`, the corpus in a local file
//! (`CITAR_BOT_DUMP`, else `refcheck/corpus/bot_decisions.json.gz`). [`recorded`] reads them.
//!
//! The group compares the values: the turn's context (army target, supply, gold per turn,
//! happiness, era, wars, offense, exposed cities, luxuries owned, resources waiting for an
//! improvement, the enemies seen and the military), every tech value in both modes, and each
//! city's threat and defence. The choices (the research, policy, great person and pantheon
//! picks, the danger and garrison flags, the sites, the spare units; stage 2's attacks and war
//! targets) are agreement rates, which `cargo refcheck bot-agreement` reports
//! ([`crate::agreement`]); a choice that differs is no difference here. The Rust side is
//! `citar_bot::decisions::ask` of the same bot ([`ask_recorded`]). Stages 2 and 3 record no value
//! of their own: what an attack weighs is the combat preview, which `combat_previews` compares;
//! the advice's power ratios are held within the tolerance by the agreement, as part of its war
//! readiness; and the worth of a negotiation's proposal in the advice is `deal_checks`'
//! `bot_value`.
//!
//! Stage 3's luxury trades and advice visit the civilizations met in an order: Python's in the
//! order they were met, which its state records (`players[*].met`, [`met_orders`]), the Rust
//! bot's in player-id order, since the engine keeps no such order
//! (`met-lists-in-player-id-order`). The agreement asks them in Python's order, so that a choice
//! differs only where the bot decides differently.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use citar_bot::decisions::{Question, ask, ask_in_order, fighters};
use citar_engine::api::testops;
use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The committed recording, relative to the repository root.
pub const COMMITTED: &str = "refcheck/bot_decisions.json.gz";
/// The variable naming a local recording (the corpus's).
pub const ENV: &str = "CITAR_BOT_DUMP";
/// Where `bot_dump.py --fixtures refcheck/corpus` writes without the variable.
pub const CORPUS_DEFAULT: &str = "refcheck/corpus/bot_decisions.json.gz";

/// The questions whose values the group compares.
const VALUES: [Question; 3] = [Question::Context, Question::TechValues, Question::Cities];

/// One recording: each state's living majors, by case and turn.
type Recording = BTreeMap<(String, u32), Arc<Vec<Value>>>;

/// The recordings read so far, by path; a path that could not be read holds its error.
static READ: Mutex<BTreeMap<PathBuf, Result<Arc<Recording>, String>>> = Mutex::new(BTreeMap::new());

/// The recordings a run looks in, in order: the committed one, then `CITAR_BOT_DUMP`'s (or the
/// corpus's default place).
#[must_use]
#[allow(clippy::disallowed_methods, reason = "a tool's switch, not a game's input")]
pub fn sources(root: &Path) -> Vec<PathBuf> {
    let local = std::env::var_os(ENV).map_or_else(|| root.join(CORPUS_DEFAULT), PathBuf::from);
    vec![root.join(COMMITTED), local]
}

/// Reads one recording, once per run.
fn read(path: &Path) -> Result<Arc<Recording>, String> {
    let mut cache = READ.lock().unwrap_or_else(PoisonError::into_inner);
    cache
        .entry(path.to_path_buf())
        .or_insert_with(|| {
            let bytes = crate::fixture::read_gz(path).map_err(|e| e.to_string())?;
            let doc: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let mut out = Recording::new();
            for s in doc.get("states").and_then(Value::as_array).into_iter().flatten() {
                let case = s.get("case").and_then(Value::as_str).unwrap_or_default().to_owned();
                let turn =
                    s.get("turn").and_then(Value::as_u64).and_then(|t| u32::try_from(t).ok());
                let majors = s.get("majors").and_then(Value::as_array).cloned().unwrap_or_default();
                out.insert((case, turn.unwrap_or(0)), Arc::new(majors));
            }
            Ok(Arc::new(out))
        })
        .clone()
}

/// The Python bot's answers on state `case`/t`turn`, one row per living major
/// (`{"player": pid, kind: answer, ...}`), from the first recording that holds the state.
///
/// # Errors
/// No recording holds it: the committed one covers the committed states, and the corpus's
/// needs `CITAR_BOT_DUMP` (or its default place).
pub fn recorded(root: &Path, case: &str, turn: u32) -> Result<Arc<Vec<Value>>, AnswerError> {
    let mut tried = Vec::new();
    for path in sources(root) {
        if !path.is_file() {
            tried.push(format!("{} (none)", path.display()));
            continue;
        }
        match read(&path) {
            Ok(r) => {
                if let Some(m) = r.get(&(case.to_owned(), turn)) {
                    return Ok(Arc::clone(m));
                }
                tried.push(path.display().to_string());
            }
            Err(e) => tried.push(format!("{}: {e}", path.display())),
        }
    }
    Err(AnswerError::new(format!(
        "no bot decisions recorded for {case}/t{turn} in {}; the corpus's recording is \
         archived (refcheck/README.md, \"The archive\"): name the file in {ENV}",
        tried.join(", ")
    )))
}

/// Each player's civilizations met, in the order Python's state lists them (the order they
/// were met), by player, from a state's JSON (`GameState.to_dict()`). Empty for a state that
/// does not parse.
#[must_use]
pub fn met_orders(state: &str) -> MetOrders {
    let Ok(doc) = serde_json::from_str::<Value>(state) else { return MetOrders::new() };
    let ids = |v: &Value| -> Vec<PlayerId> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .filter_map(|q| u8::try_from(q).ok())
            .map(PlayerId)
            .collect()
    };
    doc.get("players")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, p)| {
            let pid = p.get("id").and_then(Value::as_u64).or_else(|| u64::try_from(i).ok())?;
            let pid = PlayerId(u8::try_from(pid).ok()?);
            Some((pid, ids(p.get("met").unwrap_or(&Value::Null))))
        })
        .collect()
}

/// The civilizations each player has met, in Python's order ([`met_orders`]).
pub type MetOrders = BTreeMap<PlayerId, Vec<PlayerId>>;

/// The Rust bot's answer to question `q` of major `pid` on `g`, asked as `bot_dump.py` asked it:
/// for the attacks, each of the civilization's [`fighters`] readied first (the `ready_unit` test
/// operation, on a copy of the game: its full movement, no orders, no attack made this turn),
/// since a state saved at one civilization's turn holds the others' units spent; stage 3's
/// questions visit the civilizations met in Python's order where `met` gives it.
#[must_use]
pub fn ask_recorded(g: &Game, pid: PlayerId, q: Question, met: &MetOrders) -> Value {
    if q != Question::Attacks {
        return match met.get(&pid) {
            Some(order) => ask_in_order(g, pid, q, order),
            None => ask(g, pid, q),
        };
    }
    let ops: Vec<Value> =
        fighters(g, pid).iter().map(|u| json!({"op": "ready_unit", "unit": u.get()})).collect();
    if ops.is_empty() {
        return ask(g, pid, q);
    }
    let mut ready = g.clone();
    match testops::apply(&mut ready, &Value::Array(ops)) {
        Ok(_) => ask(&ready, pid, q),
        Err(e) => json!({"error": format!("the units did not ready: {}", e.message)}),
    }
}

/// A major's id in a recorded row.
pub fn player(row: &Value) -> Option<PlayerId> {
    row.get("player").and_then(Value::as_u64).and_then(|p| u8::try_from(p).ok()).map(PlayerId)
}

/// Whether an answer is a question that raised in Python (`{"error": ...}`).
pub fn raised(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.contains_key("error"))
}

/// The values of one major's answers that the group compares: a city's threat and defence of
/// the `cities` answer.
pub(crate) fn values_of(row: &Value) -> Value {
    let mut out = Map::new();
    out.insert("player".to_owned(), row.get("player").cloned().unwrap_or(Value::Null));
    for q in VALUES {
        let Some(v) = row.get(q.name()).filter(|v| !raised(v)) else { continue };
        let v = if q == Question::Cities {
            let rows: Vec<Value> = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| json!({"city": c["city"], "threat": c["threat"], "defense": c["defense"]}))
                .collect();
            Value::Array(rows)
        } else {
            v.clone()
        };
        out.insert(q.name().to_owned(), v);
    }
    Value::Object(out)
}

/// The `bot_decisions` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct BotDecisions;

impl AnswerModule for BotDecisions {
    fn group(&self) -> Group {
        Group::BotDecisions
    }

    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        let f = cx.fixture.ok_or_else(|| AnswerError::new("bot_decisions needs a fixture"))?;
        let majors = recorded(cx.root, &f.meta.case, f.meta.turn)?;
        let rows: Vec<Value> = majors.iter().map(values_of).collect();
        Ok(Cow::Owned(json!({ "majors": rows })))
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let rows = expected
            .get("majors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|row| answer_row(g, row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({ "majors": rows }))
    }
}

/// The Rust bot's answers to what one recorded row asked.
pub(crate) fn answer_row(g: &Game, row: &Value) -> Result<Value, AnswerError> {
    let pid =
        player(row).ok_or_else(|| AnswerError::new(format!("a row without its player: {row}")))?;
    let mut out = Map::new();
    out.insert("player".to_owned(), json!(pid.0));
    for q in VALUES {
        if row.get(q.name()).is_none() {
            continue;
        }
        let v = ask(g, pid, q);
        out.insert(q.name().to_owned(), values_of(&json!({ q.name(): v }))[q.name()].clone());
    }
    Ok(Value::Object(out))
}
