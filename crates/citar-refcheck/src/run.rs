//! A refcheck run: load, answer, compare, explain (DESIGN.md 9.2).
//!
//! Fixtures are loaded, answered, compared and explained in parallel with rayon, one fixture per
//! task, so memory holds only as many fixtures as there are threads. Each task also tallies how
//! the intended entries fared on its fixture; the tallies are summed and the results put in
//! report order afterwards, so the report is the same on every run.
//!
//! A subject whose answer module failed or panicked is [`Outcome::Failed`]: one `error`
//! difference, which no intended entry explains, which fails the run wherever its group is
//! enforced, and which covers no intended entry (nothing was compared).
//!
//! Exit codes: 0 clean; 1 unexplained differences where `enforced.toml` covers them (anywhere,
//! with `--strict`, which also wants every selected group compared); 2 a fixture or configuration
//! that could not be loaded; 3 stale intended entries under `--strict`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use citar_engine::compat::python::ConvertReport;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde_json::Value;

use crate::answer::{AnswerModule, Answers, Ctx};
use crate::compare::{self, CompareSpec, Diff, Options};
use crate::enforced::Enforced;
use crate::fixture::{self, Fixture, FixtureRef, FixtureSet};
use crate::group::Scope;
use crate::intended::{Intended, NearMiss, Scoped};
use crate::ratchet::Count;
use crate::{Error, Group, Result};

/// The committed configuration files, relative to the repository root.
pub const INTENDED: &str = "refcheck/intended.toml";
pub const ENFORCED: &str = "refcheck/enforced.toml";
pub const RATCHET: &str = "refcheck/ratchet.json";

/// What to check.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The repository root: where the configuration files and run-scope inputs are.
    pub root: PathBuf,
    pub sets: Vec<FixtureSet>,
    /// The groups to check, in dependency order.
    pub groups: Vec<Group>,
    /// Globs on fixture names; empty means every fixture. Run-scope groups ignore them.
    pub cases: Vec<String>,
    pub strict: bool,
    pub with_bot: bool,
}

impl RunOptions {
    /// Every group, on these fixture sets.
    pub fn new(root: &Path, sets: Vec<FixtureSet>) -> RunOptions {
        RunOptions {
            root: root.to_path_buf(),
            sets,
            groups: Group::ALL.to_vec(),
            cases: Vec::new(),
            strict: false,
            with_bot: false,
        }
    }
}

/// The intended and enforced lists.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub intended: Intended,
    pub enforced: Enforced,
}

impl Config {
    pub fn load(root: &Path) -> Result<Config> {
        Ok(Config {
            intended: Intended::load(&root.join(INTENDED))?,
            enforced: Enforced::load(&root.join(ENFORCED))?,
        })
    }
}

/// A fixture that took part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInfo {
    pub name: String,
    pub set: String,
}

/// A fixture that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFailure {
    pub name: String,
    pub error: String,
}

/// How a difference stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Unexplained,
    /// Explained by the intended entry with this id (the first, if several match).
    Explained(String),
    /// Accepted by the comparison rules themselves (`path_equivalent`).
    Accepted,
}

/// A difference with its verdict.
#[derive(Debug, Clone)]
pub struct Checked {
    pub diff: Diff,
    pub verdict: Verdict,
    /// Whether `enforced.toml` covers its place, so that, unexplained, it fails the run.
    pub enforced: bool,
    /// Entries that point at it but whose constraints fail.
    pub near: Vec<NearMiss>,
}

impl Checked {
    pub fn is_unexplained(&self) -> bool {
        self.verdict == Verdict::Unexplained
    }
}

/// What came of one group on one fixture.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The group has no answer module yet.
    NotPorted,
    /// Python's answer raised while recording: information, never a difference.
    PythonCrashed(String),
    /// The answer module failed or panicked, so nothing was compared: one `error` difference,
    /// always unexplained.
    Failed(Box<Checked>),
    Compared(Vec<Checked>),
}

/// One group on one fixture (or, for a run-scope group, on the run).
#[derive(Debug, Clone)]
pub struct Subject {
    pub group: Group,
    /// The fixture's name, or [`Group::RUN_CASE`].
    pub case: String,
    /// The fixture's set label; empty for a run-scope group.
    pub set: String,
    pub outcome: Outcome,
}

/// How an intended entry fared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryUse {
    pub id: String,
    /// Differences it explained.
    pub used: u64,
    /// Whether the run compared one of its groups on a fixture its cases match.
    pub covered: bool,
}

impl EntryUse {
    pub fn stale(&self) -> bool {
        self.covered && self.used == 0
    }
}

/// One group's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupSummary {
    pub selected: bool,
    pub ported: bool,
    pub enforced: bool,
    /// Subjects compared.
    pub compared: u64,
    /// Subjects whose answer module failed or panicked. Each is one unexplained difference too.
    pub failed: u64,
    pub python_crashed: u64,
    pub unexplained: u64,
    /// Unexplained, where `enforced.toml` covers them.
    pub unexplained_enforced: u64,
    pub explained: u64,
    pub accepted: u64,
}

/// Everything a run found, in report order.
#[derive(Debug, Clone)]
pub struct Run {
    pub options: RunOptions,
    pub states: Vec<StateInfo>,
    pub load_failures: Vec<LoadFailure>,
    /// What converting the fixtures' states dropped, summed over the run (DESIGN.md 4.12).
    pub dropped: ConvertReport,
    /// Sorted by group (dependency order), then fixture.
    pub subjects: Vec<Subject>,
    /// Each recorded group's `fn` block (the Python functions behind its answer), from the
    /// first fixture that has one.
    pub fns: Vec<(Group, Value)>,
    /// Every intended entry, in file order.
    pub intended: Vec<EntryUse>,
    /// Every group, in dependency order.
    pub summaries: Vec<(Group, GroupSummary)>,
}

/// How the intended entries fared on some subjects: one per task, summed afterwards.
#[derive(Debug, Clone)]
struct Tally {
    /// Per entry: differences it explained.
    used: Vec<u64>,
    /// Per entry: whether a subject covered it.
    covered: Vec<bool>,
}

impl Tally {
    fn new(entries: usize) -> Tally {
        Tally { used: vec![0; entries], covered: vec![false; entries] }
    }

    fn add(&mut self, other: &Tally) {
        for (a, b) in self.used.iter_mut().zip(&other.used) {
            *a += b;
        }
        for (a, b) in self.covered.iter_mut().zip(&other.covered) {
            *a |= b;
        }
    }
}

struct FixtureResult {
    state: StateInfo,
    subjects: Vec<(Group, Outcome)>,
    fns: Vec<(Group, Value)>,
    tally: Tally,
    dropped: ConvertReport,
}

/// Runs the checks. An error is a configuration problem or a fixture set that cannot be read
/// (exit 2); a single fixture that fails to load is recorded in [`Run::load_failures`] instead,
/// and the run goes on.
pub fn run(opts: RunOptions, config: &Config, answers: &dyn Answers) -> Result<Run> {
    for e in config.enforced.entries() {
        if answers.module(e.group).is_none() {
            return Err(Error::new(format!(
                "{ENFORCED} enforces {}, which has no answer module; enforce a group once it is compared",
                e.group
            )));
        }
    }
    let case_set = globs(&opts.cases)?;
    let refs: Vec<FixtureRef> = fixture::discover(&opts.sets)?
        .into_iter()
        .filter(|r| case_set.as_ref().is_none_or(|s| s.is_match(&r.name)))
        .collect();
    if refs.is_empty() {
        return Err(Error::new("no fixture matches the --case globs"));
    }

    let fixture_groups: Vec<Group> =
        opts.groups.iter().copied().filter(|g| g.scope() == Scope::Fixture).collect();
    let results: Vec<std::result::Result<FixtureResult, LoadFailure>> = refs
        .par_iter()
        .map(|r| check_fixture(r, &opts, &fixture_groups, answers, config))
        .collect();

    let mut tally = Tally::new(config.intended.entries().len());
    let mut states = Vec::new();
    let mut load_failures = Vec::new();
    let mut dropped = ConvertReport::default();
    let mut per_fixture: Vec<(StateInfo, Vec<(Group, Outcome)>)> = Vec::new();
    let mut fns: Vec<(Group, Value)> = Vec::new();
    for result in results {
        match result {
            Ok(f) => {
                for (g, v) in f.fns {
                    if !fns.iter().any(|(h, _)| *h == g) {
                        fns.push((g, v));
                    }
                }
                tally.add(&f.tally);
                dropped.merge(&f.dropped);
                states.push(f.state.clone());
                per_fixture.push((f.state, f.subjects));
            }
            Err(failure) => load_failures.push(failure),
        }
    }
    fns.sort_by_key(|(g, _)| *g);

    // Group-major order: all fixtures of the first group, then the next group.
    let mut subjects = Vec::new();
    for &group in &opts.groups {
        match group.scope() {
            Scope::Run => subjects.push(Subject {
                group,
                case: Group::RUN_CASE.into(),
                set: String::new(),
                outcome: check_run_group(group, &opts, answers, config, &mut tally),
            }),
            Scope::Fixture => {
                for (state, outcomes) in &mut per_fixture {
                    if let Some(k) = outcomes.iter().position(|(h, _)| *h == group) {
                        let (_, outcome) = outcomes.swap_remove(k);
                        let (case, set) = (state.name.clone(), state.set.clone());
                        subjects.push(Subject { group, case, set, outcome });
                    }
                }
            }
        }
    }

    let uses: Vec<EntryUse> = config
        .intended
        .entries()
        .iter()
        .enumerate()
        .map(|(i, e)| EntryUse { id: e.id.clone(), used: tally.used[i], covered: tally.covered[i] })
        .collect();
    let summaries = summarize(&opts, config, answers, &subjects);
    Ok(Run {
        options: opts,
        states,
        load_failures,
        dropped,
        subjects,
        fns,
        intended: uses,
        summaries,
    })
}

fn globs(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).map_err(|e| Error::new(format!("bad --case glob `{p}`: {e}")))?);
    }
    b.build().map(Some).map_err(|e| Error::new(e.to_string()))
}

fn check_fixture(
    r: &FixtureRef,
    opts: &RunOptions,
    groups: &[Group],
    answers: &dyn Answers,
    config: &Config,
) -> std::result::Result<FixtureResult, LoadFailure> {
    let fail = |error: String| LoadFailure { name: r.name.clone(), error };
    let fixture = Fixture::load(r, &opts.sets).map_err(|e| fail(e.to_string()))?;
    // Loaded once for every group (DESIGN.md 9.2); a state that does not load is a load failure,
    // and so is one whose conversion or settle panics, which must not end the whole run. With no
    // fixture group to answer, there is nothing to load for.
    let loaded = if groups.is_empty() {
        None
    } else {
        let json = fixture.state.get().as_bytes();
        let loaded = guarded(|| Game::from_python(Ruleset::shared(), json))
            .map_err(|e| fail(format!("{}: the state does not load: {e}", r.path.display())))?;
        Some(loaded)
    };
    let dropped = loaded.as_ref().map(|(_, report)| report.clone()).unwrap_or_default();
    let cx =
        Ctx { root: &opts.root, fixture: Some(&fixture), game: loaded.as_ref().map(|(g, _)| g) };
    let mut tally = Tally::new(config.intended.entries().len());
    let mut subjects = Vec::with_capacity(groups.len());
    let mut fns = Vec::new();
    for &g in groups {
        if let Some(f) = fixture.queries.get(g.name()).and_then(|a| a.get("fn")) {
            fns.push((g, f.clone()));
        }
        let outcome =
            match (g.is_recorded().then(|| fixture.python_crash(g)).flatten(), answers.module(g)) {
                (Some(line), _) => Outcome::PythonCrashed(line),
                (None, None) => Outcome::NotPorted,
                (None, Some(m)) => {
                    let found = answer_and_compare(m, &cx, Some(&fixture.grid), opts.with_bot);
                    judge(config, g, &fixture.name, found, opts.with_bot, &mut tally)
                }
            };
        subjects.push((g, outcome));
    }
    Ok(FixtureResult {
        state: StateInfo { name: fixture.name.clone(), set: fixture.set.clone() },
        subjects,
        fns,
        tally,
        dropped,
    })
}

fn check_run_group(
    group: Group,
    opts: &RunOptions,
    answers: &dyn Answers,
    config: &Config,
    tally: &mut Tally,
) -> Outcome {
    match answers.module(group) {
        None => Outcome::NotPorted,
        Some(m) => {
            let cx = Ctx { root: &opts.root, fixture: None, game: None };
            let found = answer_and_compare(m, &cx, None, opts.with_bot);
            judge(config, group, Group::RUN_CASE, found, opts.with_bot, tally)
        }
    }
}

/// Asks a module for both sides and compares them. A failure or a panic in the module is the
/// `error` difference returned as `Err`, so one bad fixture does not end the run.
fn answer_and_compare(
    m: &dyn AnswerModule,
    cx: &Ctx<'_>,
    grid: Option<&compare::Grid>,
    with_bot: bool,
) -> std::result::Result<Vec<Diff>, Box<Diff>> {
    let failed = |what: String| Err(Box::new(Diff::error(what)));
    let expected = match catch_unwind(AssertUnwindSafe(|| m.expected(cx))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return failed(format!("the Python side: {e}")),
        Err(p) => return failed(format!("the Python side panicked: {}", panic_text(p.as_ref()))),
    };
    let actual = match catch_unwind(AssertUnwindSafe(|| m.answer(cx, &expected))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return failed(format!("the Rust answer: {e}")),
        Err(p) => return failed(format!("the Rust answer panicked: {}", panic_text(p.as_ref()))),
    };
    let spec = CompareSpec::for_group(m.group());
    Ok(compare::compare(&spec, &expected, &actual, &Options { with_bot, grid }))
}

/// The verdicts of one subject, tallied. A failed module is never explained and covers no
/// intended entry: nothing was compared.
fn judge(
    config: &Config,
    group: Group,
    case: &str,
    found: std::result::Result<Vec<Diff>, Box<Diff>>,
    with_bot: bool,
    tally: &mut Tally,
) -> Outcome {
    match found {
        Err(error) => Outcome::Failed(Box::new(Checked {
            enforced: config.enforced.covers(group, &error),
            diff: *error,
            verdict: Verdict::Unexplained,
            near: Vec::new(),
        })),
        Ok(diffs) => {
            let scoped = config.intended.scoped(group, case);
            for i in scoped.covered(with_bot) {
                tally.covered[i] = true;
            }
            Outcome::Compared(
                diffs.into_iter().map(|d| verdict(config, &scoped, group, d, tally)).collect(),
            )
        }
    }
}

/// Runs a step that loads a fixture (`Game::from_python`), with a panic turned into its error:
/// one bad fixture is a load failure, not the end of the run.
fn guarded<T, E: std::fmt::Display>(
    step: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<T, String> {
    match catch_unwind(AssertUnwindSafe(step)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(p) => Err(format!("it panicked: {}", panic_text(p.as_ref()))),
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(no message)".to_string()
    }
}

fn verdict(
    config: &Config,
    scoped: &Scoped<'_>,
    group: Group,
    diff: Diff,
    tally: &mut Tally,
) -> Checked {
    let enforced = config.enforced.covers(group, &diff);
    if diff.kind.is_accepted() {
        return Checked { diff, verdict: Verdict::Accepted, enforced, near: Vec::new() };
    }
    let e = scoped.explain(&diff);
    for &i in &e.by {
        tally.used[i] += 1;
    }
    let verdict = match e.by.first() {
        Some(&i) => Verdict::Explained(config.intended.entries()[i].id.clone()),
        None => Verdict::Unexplained,
    };
    Checked { diff, verdict, enforced, near: e.near }
}

fn summarize(
    opts: &RunOptions,
    config: &Config,
    answers: &dyn Answers,
    subjects: &[Subject],
) -> Vec<(Group, GroupSummary)> {
    Group::ALL
        .into_iter()
        .map(|g| {
            let mut s = GroupSummary {
                selected: opts.groups.contains(&g),
                ported: answers.module(g).is_some(),
                enforced: config.enforced.has_group(g),
                ..GroupSummary::default()
            };
            for sub in subjects.iter().filter(|x| x.group == g) {
                match &sub.outcome {
                    Outcome::NotPorted => {}
                    Outcome::PythonCrashed(_) => s.python_crashed += 1,
                    Outcome::Failed(c) => {
                        s.failed += 1;
                        s.unexplained += 1;
                        if c.enforced {
                            s.unexplained_enforced += 1;
                        }
                    }
                    Outcome::Compared(checked) => {
                        s.compared += 1;
                        for c in checked {
                            match c.verdict {
                                Verdict::Unexplained => {
                                    s.unexplained += 1;
                                    if c.enforced {
                                        s.unexplained_enforced += 1;
                                    }
                                }
                                Verdict::Explained(_) => s.explained += 1,
                                Verdict::Accepted => s.accepted += 1,
                            }
                        }
                    }
                }
            }
            (g, s)
        })
        .collect()
}

impl Run {
    pub fn summary(&self, group: Group) -> &GroupSummary {
        // summaries holds every group, in ALL order.
        &self.summaries[Group::ALL.iter().position(|g| *g == group).unwrap_or(0)].1
    }

    pub fn unexplained(&self) -> u64 {
        self.summaries.iter().map(|(_, s)| s.unexplained).sum()
    }

    pub fn unexplained_enforced(&self) -> u64 {
        self.summaries.iter().map(|(_, s)| s.unexplained_enforced).sum()
    }

    /// Selected groups with no answer module: `--strict` fails on them.
    pub fn not_ported(&self) -> Vec<Group> {
        self.summaries.iter().filter(|(_, s)| s.selected && !s.ported).map(|(g, _)| *g).collect()
    }

    pub fn stale(&self) -> impl Iterator<Item = &EntryUse> {
        self.intended.iter().filter(|u| u.stale())
    }

    /// What the ratchet counts, per group compared or failed on some subject: unexplained
    /// differences found by comparing, and failed subjects, apart.
    pub fn counts(&self) -> Vec<(Group, Count)> {
        self.summaries
            .iter()
            .filter(|(_, s)| s.compared + s.failed > 0)
            .map(|(g, s)| (*g, Count { unexplained: s.unexplained - s.failed, failed: s.failed }))
            .collect()
    }

    /// Every difference with the group and fixture it belongs to, in report order; a failed
    /// module's `error` among them.
    pub fn findings(&self) -> impl Iterator<Item = (&Subject, &Checked)> {
        self.subjects.iter().flat_map(|s| match &s.outcome {
            Outcome::Compared(checked) => checked.iter().map(move |c| (s, c)).collect::<Vec<_>>(),
            Outcome::Failed(c) => vec![(s, &**c)],
            Outcome::NotPorted | Outcome::PythonCrashed(_) => Vec::new(),
        })
    }

    pub fn exit_code(&self) -> u8 {
        if !self.load_failures.is_empty() {
            return 2;
        }
        if self.unexplained_enforced() > 0 {
            return 1;
        }
        if self.options.strict && (self.unexplained() > 0 || !self.not_ported().is_empty()) {
            return 1;
        }
        if self.options.strict && self.stale().next().is_some() {
            return 3;
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_that_panics_is_an_error_not_the_end_of_the_run() {
        let ok: std::result::Result<u8, String> = guarded(|| Ok::<u8, String>(3));
        assert_eq!(ok, Ok(3));
        let refused =
            guarded(|| Err::<u8, _>("players[1].grudge: a field the converter does not know"));
        assert_eq!(
            refused,
            Err("players[1].grudge: a field the converter does not know".to_owned())
        );
        let panicked =
            guarded(|| -> std::result::Result<u8, String> { panic!("index out of bounds") });
        assert_eq!(panicked, Err("it panicked: index out of bounds".to_owned()));
    }
}
