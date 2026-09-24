//! Typed actions, and the one pipeline every action runs through (DESIGN.md 8.3).
//!
//! Replaces the checks of `tools.execute` (`tools.py:84-130`) that come before a tool's own:
//! the player is a living major civilization, the game goes on, and it is the player's turn
//! unless the tool may be used at any time. An action then runs in four steps:
//! 1. **check**, on `&Game`: nothing can be written, so a refusal returns before anything
//!    changed. The digest is as it was, no event is emitted, and nothing settles (property P2);
//! 2. **apply**, infallible: what the check planned, written through `game::mutate`;
//! 3. **settle**: citizens, sight and what they reveal catch up (DESIGN.md 6.7);
//! 4. **render**: the result is read from the settled game, so a tool that reassigns citizens or
//!    reveals tiles reports what the caller will see (`tools.py:680-757`).
//!
//! [`Action`] is a serde enum tagged by tool name. Each system package adds the variants of its
//! tools, with their argument specs, as it ports their rules (DESIGN.md 3.4, rule 2); package
//! 1b-01 lands the pipeline. The JSON registry of tools comes in package 1d-01.

use super::Game;
use super::error::{ActionError, ErrCode};
use super::events::EventBatch;
use crate::base::ids::PlayerId;
use crate::state::Phase;

/// What an action reports: the tool's result, as the model reads it.
pub type Outcome = serde_json::Value;

/// How to report an applied action, read from the game once it has settled.
pub struct OutcomeSpec(Box<dyn FnOnce(&Game) -> Outcome>);

impl OutcomeSpec {
    /// A result known when the action applied.
    #[must_use]
    pub fn value(v: Outcome) -> Self {
        Self(Box::new(move |_| v))
    }

    /// A result read from the settled game: the worked tiles after the citizens moved, the
    /// tiles a move revealed.
    #[must_use]
    pub fn render(f: impl FnOnce(&Game) -> Outcome + 'static) -> Self {
        Self(Box::new(f))
    }

    fn finish(self, g: &Game) -> Outcome {
        (self.0)(g)
    }
}

impl core::fmt::Debug for OutcomeSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("OutcomeSpec")
    }
}

/// One tool's rule: a check that only reads, and an apply that cannot fail.
pub trait Rule {
    /// What the check found, for the apply to carry out.
    type Plan;

    /// Whether the rule allows it now. Reads only.
    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError>;

    /// Carries out what the check planned, and says how to report it.
    fn apply(self, g: &mut Game, pid: PlayerId, plan: Self::Plan) -> OutcomeSpec;
}

/// Checks, then applies: the part of the pipeline each action shares.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "every Action variant calls it; the systems add them from 1b-02")
)]
fn run<R: Rule>(g: &mut Game, pid: PlayerId, r: R) -> Result<OutcomeSpec, ActionError> {
    let plan = r.check(g, pid)?;
    Ok(r.apply(g, pid, plan))
}

/// An action a player takes, by tool name and argument names (DESIGN.md 8.3).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum Action {
    /// The pipeline's own test action.
    #[cfg(test)]
    Probe(tests::Probe),
}

impl Action {
    /// The tool's name.
    #[must_use]
    pub const fn tool(&self) -> &'static str {
        match *self {
            #[cfg(test)]
            Self::Probe(_) => "probe",
        }
    }

    /// Whether the tool may be used outside the player's turn (`Tool.any_time`).
    #[must_use]
    pub const fn any_time(&self) -> bool {
        match *self {
            #[cfg(test)]
            Self::Probe(ref p) => p.any_time,
        }
    }

    #[cfg_attr(
        not(test),
        allow(
            unused_variables,
            reason = "every variant uses them; the systems add them from 1b-02"
        )
    )]
    fn run(self, g: &mut Game, pid: PlayerId) -> Result<OutcomeSpec, ActionError> {
        match self {
            #[cfg(test)]
            Self::Probe(p) => run(g, pid, p),
        }
    }
}

impl Game {
    /// Takes an action for a player (DESIGN.md 8.3): the guard and the rule's check, which only
    /// read; then the apply, a settle, and the result read from the settled game, with the
    /// events the call appended.
    pub fn act(&mut self, pid: PlayerId, a: Action) -> Result<(Outcome, EventBatch), ActionError> {
        self.ensure_live()?;
        self.begin_call();
        self.guard(pid, a.any_time())?;
        let spec = a.run(self, pid)?;
        self.settle();
        let out = spec.finish(self);
        Ok((out, self.take_batch()))
    }

    /// Whether `pid` may act now (`tools.py:105-112`): a major civilization of this game, alive,
    /// in a game that goes on, on its turn unless the tool may be used at any time.
    pub fn guard(&self, pid: PlayerId, any_time: bool) -> Result<(), ActionError> {
        self.ensure_live()?;
        let Some(p) = self.player(pid).filter(|p| p.is_major()) else {
            return Err(ActionError::new(ErrCode::InvalidPlayer, "Invalid player."));
        };
        if !p.alive() {
            return Err(ActionError::new(
                ErrCode::Eliminated,
                "Your civilization has been eliminated.",
            ));
        }
        if self.phase() != Phase::Playing {
            return Err(ActionError::new(ErrCode::GameOver, "The game is over."));
        }
        if !any_time && self.current() != pid {
            let whose = self.player(self.current()).map_or("another player", |p| &p.name);
            return Err(ActionError::new(
                ErrCode::NotYourTurn,
                format!("It is not your turn (it is {whose}'s turn)."),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
