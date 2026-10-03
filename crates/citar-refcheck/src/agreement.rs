//! `cargo refcheck bot-agreement` (DESIGN.md P2.3.11 point 3, package 2-01b): how often the Rust
//! bot makes the choices the Python bot made on the reference states, per kind of choice.
//!
//! The values of the bot's sub-decisions (tech values, threats, defences, the turn's context)
//! are refcheck's `bot_decisions` group, compared within its tolerance. Its choices are rates
//! here: which technology it researches, which policy, great person and pantheon it takes, now
//! and as if it could, which cities it counts in danger or wanting a garrison, its best three
//! sites, its spare units. A choice counts only on the items where either engine's answer says
//! something (a choice that is not null, a list that is not empty, a flag that is true), so a
//! port that never answers cannot pass; `bot_dump.py`'s `CHOICES` named the same items.
//!
//! Every choice that differs is attributed to a cause ([`Cause`]): a value the choice weighs that
//! differs between the engines (the `bot_decisions` group's values of that civilization, each
//! named, which `refcheck/intended.toml` explains or the group's run reports). A miss no cause
//! explains is listed as unattributed and fails the command, as does a kind under its floor of
//! 95%.

use std::collections::BTreeMap;
use std::path::Path;

use citar_bot::decisions::{Question, ask};
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use rayon::prelude::*;
use serde_json::Value;

use crate::answer::bot_decisions::{answer_row, player, raised, recorded, values_of};
use crate::compare::{self, CompareSpec, Options};
use crate::fixture::{self, Fixture, FixtureRef, FixtureSet};
use crate::{Error, Group, Result};

/// The agreement each kind must reach (P2.3.11).
pub const FLOOR: f64 = 0.95;

/// A kind of choice: its name in the report, the question that answers it, and what it compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Choice {
    /// The first step of the research path, per civilization and tech mode.
    NextResearch,
    /// The free technology taken now, per civilization and mode.
    FreeNow,
    /// The free technology it would take if it held one.
    PreferredFree,
    Policy,
    PreferredPolicy,
    GreatPerson,
    PreferredGreatPerson,
    Pantheon,
    PreferredPantheon,
    /// Per city.
    Danger,
    /// Per city.
    Garrison,
    /// The best three expansion sites, as a set.
    Sites,
    /// The spare units, in order.
    Spare,
}

impl Choice {
    /// Every kind of stage 1, in `bot_dump.py`'s `CHOICES` order.
    pub const ALL: [Self; 13] = [
        Self::NextResearch,
        Self::FreeNow,
        Self::PreferredFree,
        Self::Policy,
        Self::PreferredPolicy,
        Self::GreatPerson,
        Self::PreferredGreatPerson,
        Self::Pantheon,
        Self::PreferredPantheon,
        Self::Danger,
        Self::Garrison,
        Self::Sites,
        Self::Spare,
    ];

    /// Its name, as `bot_dump.py` printed it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::NextResearch => "next_research.tech",
            Self::FreeNow => "next_research.free",
            Self::PreferredFree => "next_research.preferred_free",
            Self::Policy => "empire.policy",
            Self::PreferredPolicy => "empire.preferred_policy",
            Self::GreatPerson => "empire.great_person",
            Self::PreferredGreatPerson => "empire.preferred_great_person",
            Self::Pantheon => "empire.pantheon",
            Self::PreferredPantheon => "empire.preferred_pantheon",
            Self::Danger => "cities.danger",
            Self::Garrison => "cities.garrison",
            Self::Sites => "sites (top 3 as a set)",
            Self::Spare => "spare",
        }
    }

    /// The question whose answer holds it.
    pub const fn question(self) -> Question {
        match self {
            Self::NextResearch | Self::FreeNow | Self::PreferredFree => Question::NextResearch,
            Self::Policy
            | Self::PreferredPolicy
            | Self::GreatPerson
            | Self::PreferredGreatPerson
            | Self::Pantheon
            | Self::PreferredPantheon => Question::Empire,
            Self::Danger | Self::Garrison => Question::Cities,
            Self::Sites => Question::Sites,
            Self::Spare => Question::Spare,
        }
    }

    /// The items one answer of its question holds: a key naming the item, and the choice.
    fn items(self, answer: &Value) -> Vec<(String, Value)> {
        let at = |k: &str| answer.get(k).cloned().unwrap_or(Value::Null);
        let modes = |key: &str| {
            ["classic", "potential"]
                .iter()
                .map(|m| {
                    (
                        (*m).to_owned(),
                        answer.get(*m).and_then(|x| x.get(key)).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        };
        let cities = |key: &str| {
            answer
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    (format!("city {}", c["city"]), c.get(key).cloned().unwrap_or(Value::Null))
                })
                .collect()
        };
        match self {
            Self::NextResearch => modes("tech"),
            Self::FreeNow => modes("free"),
            Self::PreferredFree => vec![(String::new(), at("preferred_free"))],
            Self::Policy => vec![(String::new(), at("policy"))],
            Self::PreferredPolicy => vec![(String::new(), at("preferred_policy"))],
            Self::GreatPerson => vec![(String::new(), at("great_person"))],
            Self::PreferredGreatPerson => vec![(String::new(), at("preferred_great_person"))],
            Self::Pantheon => vec![(String::new(), at("pantheon"))],
            Self::PreferredPantheon => vec![(String::new(), at("preferred_pantheon"))],
            Self::Danger => cities("danger"),
            Self::Garrison => cities("garrison"),
            Self::Sites => {
                let mut top: Vec<Value> =
                    answer.as_array().into_iter().flatten().take(3).cloned().collect();
                top.sort_by_key(Value::to_string);
                vec![(String::new(), Value::Array(top))]
            }
            Self::Spare => vec![(String::new(), answer.clone())],
        }
    }
}

/// Whether an item's answer says something: a choice that is not null, a list that is not
/// empty, a true flag.
fn says(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
        && !v.as_array().is_some_and(Vec::is_empty)
        && !v.as_object().is_some_and(serde_json::Map::is_empty)
}

/// Why a choice differs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cause {
    /// Values the civilization's choices weigh differ between the engines: the `bot_decisions`
    /// group's places that differ, named (an intended engine difference explains them, or the
    /// group's run reports them).
    Named(String),
    /// Nothing explains it.
    Unattributed,
}

/// One choice that differs.
#[derive(Clone, Debug)]
pub struct Miss {
    pub state: String,
    pub player: u8,
    pub choice: Choice,
    pub item: String,
    pub python: Value,
    pub rust: Value,
    pub cause: Cause,
}

/// One kind's tally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Items asked.
    pub asked: u64,
    /// Items where either engine's answer says something.
    pub considered: u64,
    /// Of those, the items where they agree.
    pub agree: u64,
}

impl Tally {
    /// The share that agree, of those considered (1 when none is).
    pub fn rate(&self) -> f64 {
        if self.considered == 0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss, reason = "counts far below 2^53")]
        let r = self.agree as f64 / self.considered as f64;
        r
    }
}

/// What a run of the agreement found.
#[derive(Clone, Debug, Default)]
pub struct Agreement {
    pub states: usize,
    pub civilizations: usize,
    pub tallies: BTreeMap<Choice, Tally>,
    pub misses: Vec<Miss>,
    /// States no recording holds, or that do not load.
    pub skipped: Vec<String>,
}

impl Agreement {
    /// Whether every kind reached its floor and every miss has a cause.
    pub fn holds(&self) -> bool {
        self.tallies.values().all(|t| t.rate() >= FLOOR)
            && self.misses.iter().all(|m| m.cause != Cause::Unattributed)
    }

    /// The report, as text: each kind's rate, then each miss with its cause.
    pub fn report(&self, misses: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "bot agreement: {} states, {} civilizations (floor {:.0}% per kind, over the items where \
             either engine says something)\n",
            self.states,
            self.civilizations,
            FLOOR * 100.0
        ));
        out.push_str(&format!(
            "  {:<32} {:>9} {:>11} {:>8}\n",
            "kind", "agree", "considered", "rate"
        ));
        for (c, t) in &self.tallies {
            let flag = if t.rate() < FLOOR { "  BELOW THE FLOOR" } else { "" };
            out.push_str(&format!(
                "  {:<32} {:>9} {:>11} {:>7.1}%  (asked {}){flag}\n",
                c.name(),
                t.agree,
                t.considered,
                t.rate() * 100.0,
                t.asked
            ));
        }
        let mut causes: BTreeMap<&Cause, u64> = BTreeMap::new();
        for m in &self.misses {
            *causes.entry(&m.cause).or_default() += 1;
        }
        out.push_str(&format!("misses: {}\n", self.misses.len()));
        for (c, n) in &causes {
            out.push_str(&format!("  {n:>5}  {}\n", c.text()));
        }
        let shown = if misses == 0 { self.misses.len() } else { misses };
        for m in self.misses.iter().take(shown) {
            out.push_str(&format!(
                "  {} player {} {} {}: python {} rust {} ({})\n",
                m.state,
                m.player,
                m.choice.name(),
                m.item,
                m.python,
                m.rust,
                m.cause.text()
            ));
        }
        for s in &self.skipped {
            out.push_str(&format!("skipped {s}\n"));
        }
        out
    }
}

impl Cause {
    /// How the report names it.
    pub fn text(&self) -> &str {
        match self {
            Cause::Named(s) => s,
            Cause::Unattributed => "UNATTRIBUTED",
        }
    }
}

/// The agreement of the Rust bot with the recording on the fixtures of `sets` that `keep`
/// selects.
///
/// # Errors
/// A fixture folder that cannot be read.
pub fn run(
    root: &Path,
    sets: &[FixtureSet],
    keep: impl Fn(&FixtureRef) -> bool + Sync,
) -> Result<Agreement> {
    let refs: Vec<FixtureRef> = fixture::discover(sets)?.into_iter().filter(|r| keep(r)).collect();
    if refs.is_empty() {
        return Err(Error::new("no fixture matches"));
    }
    let per: Vec<StateResult> = refs.par_iter().map(|r| one_state(root, r, sets)).collect();
    let mut out = Agreement::default();
    for p in per {
        match p {
            Ok((civs, tallies, misses)) => {
                out.states += 1;
                out.civilizations += civs;
                for (c, t) in tallies {
                    let e = out.tallies.entry(c).or_default();
                    e.asked += t.asked;
                    e.considered += t.considered;
                    e.agree += t.agree;
                }
                out.misses.extend(misses);
            }
            Err(e) => out.skipped.push(e),
        }
    }
    out.misses.sort_by(|a, b| {
        (a.choice, &a.state, a.player, &a.item).cmp(&(b.choice, &b.state, b.player, &b.item))
    });
    Ok(out)
}

type StateResult = std::result::Result<(usize, Vec<(Choice, Tally)>, Vec<Miss>), String>;

/// One state: every major's choices against the recording.
fn one_state(root: &Path, r: &FixtureRef, sets: &[FixtureSet]) -> StateResult {
    let f = Fixture::load(r, sets).map_err(|e| format!("{}: {e}", r.name))?;
    let rows = recorded(root, &f.meta.case, f.meta.turn).map_err(|e| format!("{}: {e}", r.name))?;
    let (g, _) = Game::from_python(Ruleset::shared(), f.state.get().as_bytes())
        .map_err(|e| format!("{}: does not load: {e}", r.name))?;
    let (tallies, misses) = compare_state(&g, &r.name, &rows);
    Ok((rows.len(), tallies, misses))
}

/// The choices of the majors recorded in `rows` on game `g` (state `state`), each against the
/// Rust bot's: the tallies by kind, and the misses with their causes.
#[must_use]
pub fn compare_state(g: &Game, state: &str, rows: &[Value]) -> (Vec<(Choice, Tally)>, Vec<Miss>) {
    let mut tallies: BTreeMap<Choice, Tally> = BTreeMap::new();
    let mut misses = Vec::new();
    for row in rows {
        let Some(pid) = player(row) else { continue };
        let mut answers: BTreeMap<Question, Value> = BTreeMap::new();
        let mut cause: Option<Cause> = None;
        for c in Choice::ALL {
            let q = c.question();
            let Some(py) = row.get(q.name()).filter(|v| !raised(v)) else { continue };
            let rust = answers.entry(q).or_insert_with(|| ask(g, pid, q)).clone();
            let (pi, ri) = (c.items(py), c.items(&rust));
            let t = tallies.entry(c).or_default();
            for ((key, p), (_, x)) in pi.iter().zip(&ri) {
                t.asked += 1;
                if !(says(p) || says(x)) {
                    continue;
                }
                t.considered += 1;
                if p == x {
                    t.agree += 1;
                    continue;
                }
                let cause = cause.get_or_insert_with(|| values_differ(g, row)).clone();
                misses.push(Miss {
                    state: state.to_owned(),
                    player: pid.0,
                    choice: c,
                    item: key.clone(),
                    python: p.clone(),
                    rust: x.clone(),
                    cause,
                });
            }
        }
    }
    (tallies.into_iter().collect(), misses)
}

/// What explains a civilization's misses: the values of its `bot_decisions` answers (its turn's
/// context, its tech values, its cities' threats and defences) that differ between the engines.
fn values_differ(g: &Game, row: &Value) -> Cause {
    let python = values_of(row);
    let Ok(rust) = answer_row(g, row) else { return Cause::Unattributed };
    let spec = CompareSpec::for_group(Group::BotDecisions);
    let diffs = compare::compare(&spec, &python, &rust, &Options { with_bot: false, grid: None });
    if diffs.is_empty() {
        return Cause::Unattributed;
    }
    let mut places: Vec<String> = diffs.iter().map(|d| d.path.to_string()).collect();
    places.truncate(3);
    let more = diffs.len().saturating_sub(places.len());
    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
    Cause::Named(format!("bot_decisions values differ: {}{tail}", places.join(", ")))
}
