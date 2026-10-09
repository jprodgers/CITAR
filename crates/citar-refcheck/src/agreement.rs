//! `cargo refcheck bot-agreement` (DESIGN.md P2.3.11 point 3, packages 2-01b, 2-03 and 2-05): how
//! often the Rust bot makes the choices the Python bot made on the reference states, per kind of
//! choice.
//!
//! The values of the bot's sub-decisions (tech values, threats, defences, the turn's context)
//! are refcheck's `bot_decisions` group, compared within its tolerance. Its choices are rates
//! here: which technology it researches, which policy, great person and pantheon it takes, now
//! and as if it could, which cities it counts in danger or wanting a garrison, its best three
//! sites, its spare units; what each of its units would attack, and the city its war is fought
//! for with the plan around it; the city of each rival it could reach, the luxury trades it would
//! offer, and its advice: the war it fights or prepares on each rival (its power against theirs,
//! whether its army has gathered), the luxuries it has to spare, and what it would ask for. A
//! choice counts only on the items where either engine's answer says something (a choice that
//! is not null, a list that is not empty, a flag that is true), so a port that never answers
//! cannot pass; `bot_dump.py`'s `CHOICES` named the same items. The items are matched by what
//! names them (a tech mode, a city's id), never by their place in the answer; an item only one
//! engine names is considered, and is a miss. Two answers agree when they are equal under
//! refcheck's number rule (integers exact, other numbers within its tolerance), which matters
//! for the one number a choice holds that is no id, tile or count: the power ratio of the
//! advice's war readiness, rounded to two decimals on each side.
//!
//! Every choice that differs is attributed to a cause ([`Cause`]). The one cause there is today:
//! an intended engine difference (`refcheck/intended.toml`) moved a value the choice weighs.
//! Each kind names the recorded values it weighs ([`Choice::weighs`]): the research path its
//! mode's tech values, a city's danger that city's threat and defence, and so on. A miss is put
//! down to an entry only when one of those values differs between the engines and the entry
//! explains that difference on that state; a value the choice does not read, or a difference no
//! entry explains (which the group's own run reports), explains nothing. A miss with no cause is
//! listed as unattributed and fails the command, as does a kind under its floor of 95%.

use std::collections::BTreeMap;
use std::path::Path;

use citar_bot::decisions::Question;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::answer::bot_decisions::{
    MetOrders, answer_row, ask_recorded, met_orders, player, raised, recorded, values_of,
};
use crate::compare::{self, CompareSpec, Diff, Options, Pattern, value};
use crate::fixture::{self, Fixture, FixtureRef, FixtureSet};
use crate::intended::Intended;
use crate::run::INTENDED;
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
    /// Per unit: the tile it would attack.
    Attacks,
    /// The war plan: its city and rally point, whether the army advances, whether the siege is
    /// ready.
    WarTarget,
    /// Per rival: the city of theirs it could reach.
    Reachable,
    /// The luxury trades it would offer, in order.
    LuxTrade,
    /// Per rival its advice names: the war it fights or prepares on them (whether at war,
    /// whether preparing, the power ratio, whether the army has gathered).
    AdviceWarReadiness,
    /// The luxuries its advice has to spare, sorted.
    AdviceSpareLuxuries,
    /// What its advice would ask for, in order (at most five).
    AdviceWants,
}

/// One item a choice is compared on: the name that matches it across the engines (and that the
/// report prints), the place its own values have in the recording (a tech mode, a city's id),
/// and the choice.
#[derive(Clone, Debug)]
struct Item {
    key: String,
    at: Option<String>,
    choice: Value,
}

impl Choice {
    /// Every kind, in `bot_dump.py`'s `CHOICES` order.
    pub const ALL: [Self; 20] = [
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
        Self::Attacks,
        Self::WarTarget,
        Self::Reachable,
        Self::LuxTrade,
        Self::AdviceWarReadiness,
        Self::AdviceSpareLuxuries,
        Self::AdviceWants,
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
            Self::Attacks => "attacks",
            Self::WarTarget => "war_target",
            Self::Reachable => "reachable",
            Self::LuxTrade => "lux_trade",
            Self::AdviceWarReadiness => "advice.war_readiness",
            Self::AdviceSpareLuxuries => "advice.spare_luxuries",
            Self::AdviceWants => "advice.wants",
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
            Self::Attacks => Question::Attacks,
            Self::WarTarget => Question::WarTarget,
            Self::Reachable => Question::Reachable,
            Self::LuxTrade => Question::LuxTrade,
            Self::AdviceWarReadiness | Self::AdviceSpareLuxuries | Self::AdviceWants => {
                Question::Advice
            }
        }
    }

    /// The places of the `bot_decisions` group (in its paths, `majors[...]` first) whose values
    /// the choice of the item at `at` weighs: a difference there can explain a miss, a
    /// difference elsewhere cannot.
    ///
    /// - The research path ranks every technology by its mode's tech value for its cost
    ///   (basic.py:888-909). The context reaches it only through those values, which the
    ///   recording holds for every technology the path may take, so they are its places.
    /// - The great person goes by the era (1031-1033).
    /// - A city's danger compares its threat with its defence (869-871); a garrison is wanted
    ///   in the exposed cities (865-867); the spare units are the military outside the
    ///   threatened cities (1739-1766). A city only one engine has is a place of each.
    /// - The war plan aims at the enemy cities nearest the civilization's own among its wars,
    ///   and the army it counts gathered is its military (2291-2339).
    /// - The luxury trades weigh the luxuries it owns, and a purchase its happiness and gold per
    ///   turn (2505-2556); the advice's wants the luxuries it owns, its wars and its military,
    ///   whose power the peace it would ask for weighs (2756-2770); its war readiness its wars
    ///   and its military, whose power over the rival's it shows (2736-2756); the luxuries it
    ///   has to spare the luxuries it owns (2757-2758).
    /// - The city of a rival it could reach reads its cities, the map and what it has explored,
    ///   none of which the recording holds (2478-2492).
    /// - The free technology (the most expensive available), the policy, the pantheon and the
    ///   sites read no value the recording holds, nor does an attack, which weighs the combat
    ///   preview (`combat_previews` compares it): no difference explains their misses.
    pub fn weighs(self, at: Option<&str>) -> Vec<String> {
        let city = |field: &str| at.map(|c| format!("majors[*].cities[city={c}]{field}"));
        let places: Vec<Option<String>> = match self {
            Self::NextResearch => vec![at.map(|mode| format!("majors[*].tech_values.{mode}.*"))],
            Self::GreatPerson | Self::PreferredGreatPerson => {
                vec![Some("majors[*].context.era".to_owned())]
            }
            Self::Danger => vec![city(""), city(".threat"), city(".defense")],
            Self::Garrison => vec![city(""), Some("majors[*].context.exposed.**".to_owned())],
            Self::Spare => vec![
                Some("majors[*].context.military.**".to_owned()),
                Some("majors[*].cities[*]".to_owned()),
                Some("majors[*].cities[*].threat".to_owned()),
            ],
            Self::WarTarget => vec![
                Some("majors[*].context.wars.**".to_owned()),
                Some("majors[*].context.military.**".to_owned()),
            ],
            Self::LuxTrade => vec![
                Some("majors[*].context.lux_owned.**".to_owned()),
                Some("majors[*].context.hap".to_owned()),
                Some("majors[*].context.gpt".to_owned()),
            ],
            Self::AdviceWarReadiness => vec![
                Some("majors[*].context.wars.**".to_owned()),
                Some("majors[*].context.military.**".to_owned()),
            ],
            Self::AdviceSpareLuxuries => vec![Some("majors[*].context.lux_owned.**".to_owned())],
            Self::AdviceWants => vec![
                Some("majors[*].context.lux_owned.**".to_owned()),
                Some("majors[*].context.wars.**".to_owned()),
                Some("majors[*].context.military.**".to_owned()),
            ],
            Self::FreeNow
            | Self::PreferredFree
            | Self::Policy
            | Self::PreferredPolicy
            | Self::Pantheon
            | Self::PreferredPantheon
            | Self::Sites
            | Self::Attacks
            | Self::Reachable => Vec::new(),
        };
        places.into_iter().flatten().collect()
    }

    /// The items one answer of its question holds.
    fn items(self, answer: &Value) -> Vec<Item> {
        let one = |choice: Value| vec![Item { key: String::new(), at: None, choice }];
        let at = |k: &str| answer.get(k).cloned().unwrap_or(Value::Null);
        // The advice without a negotiation: the states hold none (P2.3.11 point 3).
        let advice = |k: &str| answer.get("none").and_then(|a| a.get(k)).cloned();
        let modes = |key: &str| {
            ["classic", "potential"]
                .iter()
                .map(|&m| Item {
                    key: m.to_owned(),
                    at: Some(m.to_owned()),
                    choice: answer.get(m).and_then(|x| x.get(key)).cloned().unwrap_or(Value::Null),
                })
                .collect()
        };
        let cities = |key: &str| {
            answer
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| Item {
                    key: format!("city {}", c["city"]),
                    at: Some(c["city"].to_string()),
                    choice: c.get(key).cloned().unwrap_or(Value::Null),
                })
                .collect()
        };
        match self {
            Self::NextResearch => modes("tech"),
            Self::FreeNow => modes("free"),
            Self::PreferredFree => one(at("preferred_free")),
            Self::Policy => one(at("policy")),
            Self::PreferredPolicy => one(at("preferred_policy")),
            Self::GreatPerson => one(at("great_person")),
            Self::PreferredGreatPerson => one(at("preferred_great_person")),
            Self::Pantheon => one(at("pantheon")),
            Self::PreferredPantheon => one(at("preferred_pantheon")),
            Self::Danger => cities("danger"),
            Self::Garrison => cities("garrison"),
            Self::Sites => {
                let mut top: Vec<Value> =
                    answer.as_array().into_iter().flatten().take(3).cloned().collect();
                top.sort_by_key(Value::to_string);
                one(Value::Array(top))
            }
            Self::Spare => one(answer.clone()),
            Self::Attacks => answer
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| Item {
                    key: format!("unit {}", a["unit"]),
                    at: Some(a["unit"].to_string()),
                    choice: a.get("target").cloned().unwrap_or(Value::Null),
                })
                .collect(),
            Self::WarTarget | Self::LuxTrade => one(answer.clone()),
            Self::Reachable => answer
                .as_object()
                .into_iter()
                .flatten()
                .map(|(rival, city)| Item {
                    key: format!("rival {rival}"),
                    at: Some(rival.clone()),
                    choice: city.clone(),
                })
                .collect(),
            Self::AdviceWarReadiness => advice("war_readiness")
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|w| Item {
                    key: format!("rival {}", w["player"]),
                    at: Some(w["player"].to_string()),
                    choice: w.clone(),
                })
                .collect(),
            Self::AdviceSpareLuxuries => one(advice("spare_luxuries").unwrap_or(Value::Null)),
            Self::AdviceWants => one(advice("wants").unwrap_or(Value::Null)),
        }
    }
}

/// The items of both answers matched by their names: Python's in its order, then those only
/// Rust names. An engine that does not name an item has `None` there.
fn paired(python: Vec<Item>, rust: Vec<Item>) -> Vec<(Item, Option<Value>, Option<Value>)> {
    let mut rust: BTreeMap<String, Item> = rust.into_iter().map(|i| (i.key.clone(), i)).collect();
    let mut out: Vec<(Item, Option<Value>, Option<Value>)> = python
        .into_iter()
        .map(|p| {
            let r = rust.remove(&p.key).map(|r| r.choice);
            let choice = p.choice.clone();
            (p, Some(choice), r)
        })
        .collect();
    out.extend(rust.into_values().map(|r| {
        let choice = r.choice.clone();
        (r, None, Some(choice))
    }));
    out
}

/// Whether an item's answer says something: a choice that is not null, a list that is not
/// empty, a true flag.
fn says(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
        && !v.as_array().is_some_and(Vec::is_empty)
        && !v.as_object().is_some_and(serde_json::Map::is_empty)
}

/// Whether two engines' choices of an item agree: both name it, and the choices are equal under
/// refcheck's number rule (integers exact, other numbers within its tolerance).
fn agree(python: Option<&Value>, rust: Option<&Value>) -> bool {
    python.zip(rust).is_some_and(|(p, r)| value::values_equal(p, r))
}

/// Why a choice differs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cause {
    /// Intended engine differences moved values the choice weighs: the entries of
    /// `refcheck/intended.toml` that explain them on this state, and the places that differ.
    Intended { ids: Vec<String>, places: Vec<String> },
    /// Nothing explains it.
    Unattributed,
}

impl Cause {
    /// The cause without its places, as the report counts misses by it.
    pub fn name(&self) -> String {
        match self {
            Cause::Intended { ids, .. } => format!("intended {}", ids.join(", ")),
            Cause::Unattributed => "UNATTRIBUTED".to_owned(),
        }
    }

    /// The cause with the first places it moved.
    pub fn text(&self) -> String {
        match self {
            Cause::Intended { places, .. } => {
                let shown: Vec<&str> = places.iter().take(3).map(String::as_str).collect();
                let more = places.len().saturating_sub(shown.len());
                let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
                format!("{}: {}{tail}", self.name(), shown.join(", "))
            }
            Cause::Unattributed => self.name(),
        }
    }
}

/// One choice that differs.
#[derive(Clone, Debug)]
pub struct Miss {
    pub state: String,
    pub player: u8,
    pub choice: Choice,
    pub item: String,
    /// Each engine's choice, `None` when it does not name the item.
    pub python: Option<Value>,
    pub rust: Option<Value>,
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
        let mut causes: BTreeMap<String, u64> = BTreeMap::new();
        for m in &self.misses {
            *causes.entry(m.cause.name()).or_default() += 1;
        }
        out.push_str(&format!("misses: {}\n", self.misses.len()));
        for (c, n) in &causes {
            out.push_str(&format!("  {n:>5}  {c}\n"));
        }
        let shown = if misses == 0 { self.misses.len() } else { misses };
        let side =
            |v: &Option<Value>| v.as_ref().map_or_else(|| "(none)".to_owned(), Value::to_string);
        for m in self.misses.iter().take(shown) {
            out.push_str(&format!(
                "  {} player {} {} {}: python {} rust {} ({})\n",
                m.state,
                m.player,
                m.choice.name(),
                m.item,
                side(&m.python),
                side(&m.rust),
                m.cause.text()
            ));
        }
        for s in &self.skipped {
            out.push_str(&format!("skipped {s}\n"));
        }
        out
    }
}

/// The agreement of the Rust bot with the recording on the fixtures of `sets` that `keep`
/// selects, misses explained by `root`'s intended list.
///
/// # Errors
/// A fixture folder that cannot be read, or an intended list that does not load.
pub fn run(
    root: &Path,
    sets: &[FixtureSet],
    keep: impl Fn(&FixtureRef) -> bool + Sync,
) -> Result<Agreement> {
    let intended = Intended::load(&root.join(INTENDED))?;
    let refs: Vec<FixtureRef> = fixture::discover(sets)?.into_iter().filter(|r| keep(r)).collect();
    if refs.is_empty() {
        return Err(Error::new("no fixture matches"));
    }
    let per: Vec<StateResult> =
        refs.par_iter().map(|r| one_state(root, r, sets, &intended)).collect();
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
fn one_state(root: &Path, r: &FixtureRef, sets: &[FixtureSet], intended: &Intended) -> StateResult {
    let f = Fixture::load(r, sets).map_err(|e| format!("{}: {e}", r.name))?;
    let rows = recorded(root, &f.meta.case, f.meta.turn).map_err(|e| format!("{}: {e}", r.name))?;
    let (g, _) = Game::from_python(Ruleset::shared(), f.state.get().as_bytes())
        .map_err(|e| format!("{}: does not load: {e}", r.name))?;
    let met = met_orders(f.state.get());
    let (tallies, misses) = compare_state_in_order(&g, &r.name, &rows, intended, &met);
    Ok((rows.len(), tallies, misses))
}

/// The choices of the majors recorded in `rows` on game `g` (fixture `state`, as the intended
/// list's `cases` name it), each against the Rust bot's: the tallies by kind, and the misses
/// with their causes, explained by `intended`. Stage 3 visits the civilizations met in player-id
/// order, the Rust bot's.
#[must_use]
pub fn compare_state(
    g: &Game,
    state: &str,
    rows: &[Value],
    intended: &Intended,
) -> (Vec<(Choice, Tally)>, Vec<Miss>) {
    compare_state_in_order(g, state, rows, intended, &MetOrders::new())
}

/// [`compare_state`], stage 3 visiting the civilizations each major met in `met`'s order
/// (Python's, from its state: `met_orders`) where it gives one.
#[must_use]
pub fn compare_state_in_order(
    g: &Game,
    state: &str,
    rows: &[Value],
    intended: &Intended,
    met: &MetOrders,
) -> (Vec<(Choice, Tally)>, Vec<Miss>) {
    let scoped = intended.scoped(Group::BotDecisions, state);
    let mut tallies: BTreeMap<Choice, Tally> = BTreeMap::new();
    let mut misses = Vec::new();
    for row in rows {
        let Some(pid) = player(row) else { continue };
        let mut answers: BTreeMap<Question, Value> = BTreeMap::new();
        // The civilization's explained value differences, worked out at its first miss.
        let mut explained: Option<Vec<(Diff, String)>> = None;
        for c in Choice::ALL {
            let q = c.question();
            let Some(py) = row.get(q.name()).filter(|v| !raised(v)) else { continue };
            let rust = answers.entry(q).or_insert_with(|| ask_recorded(g, pid, q, met));
            let t = tallies.entry(c).or_default();
            for (item, p, x) in paired(c.items(py), c.items(rust)) {
                t.asked += 1;
                // An item counts when either engine says something about it, or only one names
                // it at all.
                let silent = |v: &Option<Value>| v.as_ref().is_some_and(|v| !says(v));
                if silent(&p) && silent(&x) {
                    continue;
                }
                t.considered += 1;
                if agree(p.as_ref(), x.as_ref()) {
                    t.agree += 1;
                    continue;
                }
                let diffs =
                    explained.get_or_insert_with(|| explained_values(g, row, &scoped, intended));
                misses.push(Miss {
                    state: state.to_owned(),
                    player: pid.0,
                    choice: c,
                    item: item.key.clone(),
                    python: p,
                    rust: x,
                    cause: cause(diffs, &c.weighs(item.at.as_deref())),
                });
            }
        }
    }
    (tallies.into_iter().collect(), misses)
}

/// The differences between the engines' `bot_decisions` values of one civilization (its turn's
/// context, its tech values, its cities' threats and defences) that an intended entry explains
/// on this state, each with the entry's id. Paths are the group's own (`majors[player=N]...`),
/// so the entries' places match them as the group's run matches them.
fn explained_values(
    g: &Game,
    row: &Value,
    scoped: &crate::intended::Scoped<'_>,
    intended: &Intended,
) -> Vec<(Diff, String)> {
    let Ok(rust) = answer_row(g, row) else { return Vec::new() };
    let spec = CompareSpec::for_group(Group::BotDecisions);
    let python = json!({ "majors": [values_of(row)] });
    let rust = json!({ "majors": [rust] });
    compare::compare(&spec, &python, &rust, &Options { with_bot: false, grid: None })
        .into_iter()
        .filter(|d| !d.kind.is_accepted())
        .filter_map(|d| {
            let by = scoped.explain(&d).by.first().copied()?;
            Some((d, intended.entries()[by].id.clone()))
        })
        .collect()
}

/// What explains a miss whose choice weighs the places `weighs`: the explained differences at
/// those places, else nothing.
fn cause(explained: &[(Diff, String)], weighs: &[String]) -> Cause {
    let places: Vec<Pattern> = weighs.iter().filter_map(|w| Pattern::parse(w).ok()).collect();
    let mut ids: Vec<String> = Vec::new();
    let mut at: Vec<String> = Vec::new();
    for (d, id) in explained {
        if !places.iter().any(|p| p.matches(&d.path)) {
            continue;
        }
        if !ids.contains(id) {
            ids.push(id.clone());
        }
        at.push(d.path.to_string());
    }
    if ids.is_empty() { Cause::Unattributed } else { Cause::Intended { ids, places: at } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_a_choice_weighs_is_a_pattern() {
        for c in Choice::ALL {
            for at in [None, Some("classic"), Some("7")] {
                for w in c.weighs(at) {
                    assert!(Pattern::parse(&w).is_ok(), "{}: {w}", c.name());
                }
            }
        }
    }

    #[test]
    fn items_are_paired_by_name_not_by_place() {
        let python = json!([{"city": 3, "danger": true}, {"city": 5, "danger": false}]);
        let rust = json!([{"city": 5, "danger": false}, {"city": 3, "danger": true},
                          {"city": 9, "danger": false}]);
        let pairs = paired(Choice::Danger.items(&python), Choice::Danger.items(&rust));
        let got: Vec<(String, Option<Value>, Option<Value>)> =
            pairs.into_iter().map(|(i, p, r)| (i.key, p, r)).collect();
        assert_eq!(
            got,
            vec![
                ("city 3".to_owned(), Some(json!(true)), Some(json!(true))),
                ("city 5".to_owned(), Some(json!(false)), Some(json!(false))),
                ("city 9".to_owned(), None, Some(json!(false))),
            ]
        );
    }

    #[test]
    fn the_report_counts_misses_by_cause_and_names_the_places() {
        let miss = |item: &str, python: Option<Value>, cause: Cause| Miss {
            state: "s/t1".to_owned(),
            player: 2,
            choice: Choice::NextResearch,
            item: item.to_owned(),
            python,
            rust: Some(json!("Mathematics")),
            cause,
        };
        let places: Vec<String> = (0..5).map(|i| format!("majors[player=2].x{i}")).collect();
        let found = Agreement {
            states: 1,
            civilizations: 1,
            tallies: BTreeMap::from([(
                Choice::NextResearch,
                Tally { asked: 2, considered: 2, agree: 0 },
            )]),
            misses: vec![
                miss("classic", None, Cause::Unattributed),
                miss(
                    "potential",
                    Some(json!("Pottery")),
                    Cause::Intended { ids: vec!["an-entry".to_owned()], places },
                ),
            ],
            skipped: Vec::new(),
        };
        assert!(!found.holds());
        let text = found.report(0);
        assert!(text.contains("BELOW THE FLOOR"), "{text}");
        assert!(text.contains("      1  UNATTRIBUTED\n"), "{text}");
        assert!(text.contains("      1  intended an-entry\n"), "{text}");
        assert!(
            text.contains("classic: python (none) rust \"Mathematics\" (UNATTRIBUTED)"),
            "{text}"
        );
        assert!(
            text.contains(
                "potential: python \"Pottery\" rust \"Mathematics\" (intended an-entry: \
                 majors[player=2].x0, majors[player=2].x1, majors[player=2].x2 and 2 more)"
            ),
            "{text}"
        );
    }
}
