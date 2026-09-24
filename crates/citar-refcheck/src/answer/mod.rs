//! Answer modules: the Rust side of each group (DESIGN.md 9.2, "Answer modules").
//!
//! Each group gets one module, `answer/<group>.rs`, written by the package that ports what the
//! group checks (DESIGN.md 3.4, rule 1). It rebuilds the skeleton of Python's answer from the
//! recorded inputs (the tiles, units and pairs Python sampled are stored next to each answer)
//! and fills it with Rust calls through `game::query`, so the two answers line up key for key.
//!
//! A group without a module reports "not ported" until its module is added to `MODULES`. So far:
//! - [`uniques`] (package 1a-05), which compares the compiled ruleset once per run;
//! - [`state_echo`] (package 1a-10), the fixture's own state read back from its conversion.
//!
//! Each fixture's state is loaded once, through `Game::from_python` (package 1b-01: the converter
//! of 1a-10, then a settle), and handed to every group in [`Ctx::game`]. A game's queries take
//! `&self`, so one load serves every group.

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

use citar_engine::game::Game;
use serde_json::Value;

use crate::Group;
use crate::fixture::Fixture;

pub mod state_echo;
pub mod uniques;

/// What an answer module is asked about.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// The repository root, where run-scope groups find their inputs (`refcheck/uniques.json.gz`).
    pub root: &'a Path,
    /// The fixture, for a fixture-scope group; `None` for a run-scope group.
    pub fixture: Option<&'a Fixture>,
    /// The fixture's game, loaded: its state, history and caches. `None` for a run-scope group.
    pub game: Option<&'a Game>,
}

/// Why an answer module produced no answer. Reported as an `error` difference at the root of the
/// group's answer, and counted as unexplained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerError {
    pub message: String,
}

impl AnswerError {
    pub fn new(message: impl Into<String>) -> Self {
        AnswerError { message: message.into() }
    }
}

impl fmt::Display for AnswerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// One group's Rust side.
pub trait AnswerModule: Sync {
    fn group(&self) -> Group;

    /// The Python side of the comparison. For a recorded group that is the fixture's recorded
    /// answer; a synthetic group (uniques, state_echo, fixed_point) derives its own.
    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        recorded(self.group(), cx).map(Cow::Borrowed)
    }

    /// The Rust answer, in the shape of `expected`.
    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError>;
}

/// The recorded Python answer of a group on the context's fixture.
pub fn recorded<'a>(group: Group, cx: &Ctx<'a>) -> Result<&'a Value, AnswerError> {
    let fixture = cx.fixture.ok_or_else(|| AnswerError::new(format!("{group} needs a fixture")))?;
    fixture.recorded(group).ok_or_else(|| {
        AnswerError::new(format!("{} has no recorded answer for {group}", fixture.name))
    })
}

/// Where a run finds its answer modules. The engine's are [`Engine`]; tests supply their own.
pub trait Answers: Sync {
    fn module(&self, group: Group) -> Option<&dyn AnswerModule>;
}

/// The engine's answer modules: one entry per ported group, in dependency order.
static MODULES: &[&dyn AnswerModule] = &[&uniques::Uniques, &state_echo::StateEcho];

/// The answers of the Rust engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct Engine;

impl Answers for Engine {
    fn module(&self, group: Group) -> Option<&dyn AnswerModule> {
        MODULES.iter().copied().find(|m| m.group() == group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_are_unique_and_in_dependency_order() {
        let groups: Vec<Group> = MODULES.iter().map(|m| m.group()).collect();
        assert!(groups.windows(2).all(|w| w[0] < w[1]), "{groups:?}");
        for g in Group::ALL {
            assert_eq!(Engine.module(g).map(|m| m.group()), groups.contains(&g).then_some(g));
        }
    }
}
