//! What a refused call says (DESIGN.md 8.5).
//!
//! Replaces Python's `ActionError` (`game.py:26-27`), whose message is what a model reads: it
//! explains the rule and names what is valid. [`ActionError`] adds a stable [`ErrCode`] for tests
//! and refcheck to match on. An action that fails leaves the game as it was: nothing written, no
//! event, no settle (property P2).
//!
//! These live in `game`, not `api`, because the rules raise them and `game` may not use `api`;
//! the host surface re-exports them.

use crate::rules::RulesetErrors;
use crate::save::LoadError;
use crate::state::StateError;

/// Why an action or a command was refused, in a word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrCode {
    /// No tool by that name.
    UnknownTool,
    /// The player is not a major civilization of this game.
    InvalidPlayer,
    /// The player has been eliminated.
    Eliminated,
    /// The game is over.
    GameOver,
    /// It is another player's turn.
    NotYourTurn,
    /// A required argument is missing.
    MissingParam,
    /// An argument has the wrong type or an impossible value.
    BadParam,
    /// A tile off the map.
    OffMap,
    /// No such unit, or not the caller's.
    NoSuchUnit,
    /// No such city, or not the caller's.
    NoSuchCity,
    /// No path to the target.
    NoPath,
    /// A negotiation refused the step.
    Negotiation,
    /// A game rule refused it.
    Rule,
    /// The rule behind it is not ported yet (DESIGN.md 3.4, rule 4).
    NotPorted,
    /// The game stopped after an internal error and takes no more commands.
    Poisoned,
}

/// A refused action or command: a code, and the text the caller reads.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActionError {
    pub code: ErrCode,
    pub message: String,
}

impl ActionError {
    /// A refusal with this code and text.
    #[must_use]
    pub fn new(code: ErrCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// A refusal by a game rule.
    #[must_use]
    pub fn rule(message: impl Into<String>) -> Self {
        Self::new(ErrCode::Rule, message)
    }

    /// The refusal of a game that has been poisoned by an internal error.
    #[must_use]
    pub fn poisoned() -> Self {
        Self::new(
            ErrCode::Poisoned,
            "This game stopped after an internal error and takes no more commands; it can only \
             be saved for debugging.",
        )
    }
}

/// Why a host command failed (DESIGN.md 8.5).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// An action or command was refused.
    #[error(transparent)]
    Action(#[from] ActionError),
    /// The settings do not make a game.
    #[error("{0}")]
    Config(String),
    /// The map does not make a game.
    #[error("{0}")]
    Map(String),
    /// A saved game does not load.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// The ruleset does not load.
    #[error(transparent)]
    Rules(#[from] RulesetErrors),
    /// The game stopped after an internal error.
    #[error("the game stopped after an internal error: {0}")]
    Poisoned(Box<str>),
    /// A state operation was refused: a bug in the caller, reported rather than panicking.
    #[error("the state refused a write: {0}")]
    State(#[from] StateError),
}
