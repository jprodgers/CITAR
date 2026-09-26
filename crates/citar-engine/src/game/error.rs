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

/// The most characters a refusal's text takes (property P5).
pub const MAX_REFUSAL_CHARS: usize = 600;

/// Which of the rules a refusal's text keeps (property P5, DESIGN.md 8.5) `text` breaks, if
/// any: it is not empty, it takes at most [`MAX_REFUSAL_CHARS`] characters, it ends a sentence
/// (`.`, `?` or `)`), and it shows no Rust debug output (`Some(`, `None`, `Idx(`, `::`). A caller's
/// own words quoted back are the caller's: tests that check this send none of those.
#[must_use]
pub fn text_rule_broken(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return Some("it is empty");
    }
    if text.chars().count() > MAX_REFUSAL_CHARS {
        return Some("it is longer than 600 characters");
    }
    if !text.ends_with(['.', '?', ')']) {
        return Some("it does not end in '.', '?' or ')'");
    }
    let debug = ["Some(", "Idx(", "::"].iter().any(|d| text.contains(d))
        || !crate::base::text::find_word(text, "None").is_empty();
    if debug {
        return Some("it shows Rust debug output");
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_reads_as_a_sentence_a_model_can_use() {
        assert_eq!(text_rule_broken("You have no spies."), None);
        assert_eq!(text_rule_broken("Is it your turn?"), None);
        assert_eq!(text_rule_broken("Try again (next turn)"), None);
        assert!(text_rule_broken("").is_some());
        assert!(text_rule_broken("Available: Shock I, Drill I").is_some());
        assert!(text_rule_broken(&format!("{}.", "x".repeat(600))).is_some());
        assert!(text_rule_broken("The unit is Some(UnitId(3)).").is_some());
        assert!(text_rule_broken("Found None here.").is_some());
        assert!(text_rule_broken("Nonesuch is fine.").is_none());
        assert!(text_rule_broken("See game::units.").is_some());
    }
}
