//! A refcheck run: load, answer, compare, explain (DESIGN.md 9.2).
//!
//! Fixtures load and compare in parallel with rayon, one fixture per task, so memory holds only
//! as many fixtures as there are threads; the results are then sorted, and everything after that
//! (explaining, counting, reporting) is sequential, so the report is the same on every run.
//!
//! Exit codes: 0 clean; 1 unexplained differences where `enforced.toml` covers them (anywhere,
//! with `--strict`, which also wants every selected group compared); 2 a fixture or configuration
//! that could not be loaded; 3 stale intended entries under `--strict`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde_json::Value;

use crate::answer::{AnswerModule, Answers, Ctx};
use crate::compare::{self, CompareSpec, Diff, Options};
use crate::enforced::Enforced;
use crate::fixture::{self, Fixture, FixtureRef, FixtureSet};
use crate::group::Scope;
use crate::intended::{Intended, NearMiss};
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

/// A group's result on one fixture, before the verdicts.
enum Raw {
    NotPorted,
    PythonCrashed(String),
    Compared(Vec<Diff>),
}

struct FixtureResult {
    state: StateInfo,
    subjects: Vec<(Group, Raw)>,
    fns: Vec<(Group, Value)>,
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
    let results: Vec<std::result::Result<FixtureResult, LoadFailure>> =
        refs.par_iter().map(|r| check_fixture(r, &opts, &fixture_groups, answers)).collect();

    let mut states = Vec::new();
    let mut load_failures = Vec::new();
    let mut per_fixture: Vec<(StateInfo, Vec<(Group, Raw)>)> = Vec::new();
    let mut fns: Vec<(Group, Value)> = Vec::new();
    for result in results {
        match result {
            Ok(f) => {
                for (g, v) in f.fns {
                    if !fns.iter().any(|(h, _)| *h == g) {
                        fns.push((g, v));
                    }
                }
                states.push(f.state.clone());
                per_fixture.push((f.state, f.subjects));
            }
            Err(failure) => load_failures.push(failure),
        }
    }
    fns.sort_by_key(|(g, _)| *g);

    // Group-major order: all fixtures of the first group, then the next group.
    let mut raw: Vec<(Group, String, String, Raw)> = Vec::new();
    for g in &opts.groups {
        match g.scope() {
            Scope::Run => raw.push((
                *g,
                Group::RUN_CASE.into(),
                String::new(),
                check_run_group(*g, &opts, answers),
            )),
            Scope::Fixture => {
                for (state, subjects) in &mut per_fixture {
                    if let Some(k) = subjects.iter().position(|(h, _)| h == g) {
                        let (_, r) = subjects.swap_remove(k);
                        raw.push((*g, state.name.clone(), state.set.clone(), r));
                    }
                }
            }
        }
    }

    let mut uses: Vec<EntryUse> = config
        .intended
        .entries()
        .iter()
        .map(|e| EntryUse { id: e.id.clone(), used: 0, covered: false })
        .collect();
    let mut subjects = Vec::with_capacity(raw.len());
    for (group, case, set, r) in raw {
        let outcome = match r {
            Raw::NotPorted => Outcome::NotPorted,
            Raw::PythonCrashed(line) => Outcome::PythonCrashed(line),
            Raw::Compared(diffs) => {
                mark_coverage(config, group, &case, opts.with_bot, &mut uses);
                Outcome::Compared(
                    diffs
                        .into_iter()
                        .map(|d| verdict(config, group, &case, d, &mut uses))
                        .collect(),
                )
            }
        };
        subjects.push(Subject { group, case, set, outcome });
    }

    let summaries = summarize(&opts, config, answers, &subjects);
    Ok(Run { options: opts, states, load_failures, subjects, fns, intended: uses, summaries })
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
) -> std::result::Result<FixtureResult, LoadFailure> {
    let fixture = Fixture::load(r, &opts.sets)
        .map_err(|e| LoadFailure { name: r.name.clone(), error: e.to_string() })?;
    let cx = Ctx { root: &opts.root, fixture: Some(&fixture) };
    let mut subjects = Vec::with_capacity(groups.len());
    let mut fns = Vec::new();
    for &g in groups {
        if let Some(f) = fixture.queries.get(g.name()).and_then(|a| a.get("fn")) {
            fns.push((g, f.clone()));
        }
        let raw =
            match (g.is_recorded().then(|| fixture.python_crash(g)).flatten(), answers.module(g)) {
                (Some(line), _) => Raw::PythonCrashed(line),
                (None, None) => Raw::NotPorted,
                (None, Some(m)) => {
                    Raw::Compared(answer_and_compare(m, &cx, Some(&fixture.grid), opts.with_bot))
                }
            };
        subjects.push((g, raw));
    }
    Ok(FixtureResult {
        state: StateInfo { name: fixture.name.clone(), set: fixture.set.clone() },
        subjects,
        fns,
    })
}

fn check_run_group(group: Group, opts: &RunOptions, answers: &dyn Answers) -> Raw {
    match answers.module(group) {
        None => Raw::NotPorted,
        Some(m) => Raw::Compared(answer_and_compare(
            m,
            &Ctx { root: &opts.root, fixture: None },
            None,
            opts.with_bot,
        )),
    }
}

/// Asks a module for both sides and compares them. A failure or a panic in the module is one
/// `error` difference, so one bad fixture does not end the run.
fn answer_and_compare(
    m: &dyn AnswerModule,
    cx: &Ctx<'_>,
    grid: Option<&compare::Grid>,
    with_bot: bool,
) -> Vec<Diff> {
    let expected = match catch_unwind(AssertUnwindSafe(|| m.expected(cx))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return vec![Diff::error(format!("the Python side: {e}"))],
        Err(p) => {
            return vec![Diff::error(format!(
                "the Python side panicked: {}",
                panic_text(p.as_ref())
            ))];
        }
    };
    let actual = match catch_unwind(AssertUnwindSafe(|| m.answer(cx, &expected))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return vec![Diff::error(format!("the Rust answer: {e}"))],
        Err(p) => {
            return vec![Diff::error(format!(
                "the Rust answer panicked: {}",
                panic_text(p.as_ref())
            ))];
        }
    };
    let spec = CompareSpec::for_group(m.group());
    compare::compare(&spec, &expected, &actual, &Options { with_bot, grid })
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
    group: Group,
    case: &str,
    diff: Diff,
    uses: &mut [EntryUse],
) -> Checked {
    let enforced = config.enforced.covers(group, &diff.path);
    if diff.kind.is_accepted() {
        return Checked { diff, verdict: Verdict::Accepted, enforced, near: Vec::new() };
    }
    let e = config.intended.explain(group, case, &diff);
    for &i in &e.by {
        uses[i].used += 1;
    }
    let verdict = match e.by.first() {
        Some(&i) => Verdict::Explained(uses[i].id.clone()),
        None => Verdict::Unexplained,
    };
    Checked { diff, verdict, enforced, near: e.near }
}

fn mark_coverage(config: &Config, group: Group, case: &str, with_bot: bool, uses: &mut [EntryUse]) {
    let spec = CompareSpec::for_group(group);
    for (entry, u) in config.intended.entries().iter().zip(uses.iter_mut()) {
        if !u.covered
            && entry.matches_case(case)
            && entry
                .wheres
                .iter()
                .any(|w| w.group == group && (with_bot || !spec.is_bot_only(&w.path)))
        {
            u.covered = true;
        }
    }
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

    /// Unexplained differences per compared group: what the ratchet counts.
    pub fn counts(&self) -> Vec<(Group, u64)> {
        self.summaries
            .iter()
            .filter(|(_, s)| s.compared > 0)
            .map(|(g, s)| (*g, s.unexplained))
            .collect()
    }

    /// Every difference with the group and fixture it belongs to, in report order.
    pub fn findings(&self) -> impl Iterator<Item = (&Subject, &Checked)> {
        self.subjects.iter().flat_map(|s| match &s.outcome {
            Outcome::Compared(checked) => checked.iter().map(move |c| (s, c)).collect::<Vec<_>>(),
            _ => Vec::new(),
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
