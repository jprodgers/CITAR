//! Layer 3: `Game`, the caches that serve it, every rule system, and turn flow.
//!
//! All writes to `State` go through `game::mutate`; reads take `&self` and validate their memos
//! lazily; consequential writes happen only in settle (DESIGN.md 6). Rule systems may call each
//! other freely inside this layer.
//!
//! Replaces the rules in `citar/engine/`: `game.py`, `turns.py`, `economy.py`, `tiles.py`,
//! `cities.py`, `research.py`, `policies.py`, `religion.py`, `great_people.py`, `triggers.py`,
//! `ruins.py`, `visibility.py`, `units.py`, `movement.py`, `combat.py`, `conquest.py`,
//! `workers.py`, `actions.py`, `automation.py`, `diplomacy.py`, `espionage.py`,
//! `barbarians.py`, `city_states.py` and `victory.py`, and the production advisor in
//! `citar/bots/basic.py:777-875, 1146-1590` (DESIGN.md 3.3 maps each file to its module).
//!
//! The game core (package 1b-01):
//! - [`core`]: [`Game`] itself, loading, and the accessors of `game.py` (`game.py:100-145`,
//!   `320-540`, `656-812`);
//! - [`derive`]: the caches ([`derive::Derived`]) and the revisions and memos they validate
//!   themselves with ([`derive::rev`]), which replace `game.py:565-609`;
//! - [`eval`]: [`eval::EvalView`], the unique evaluator's view of a game;
//! - [`mutate`]: every write to the state, and what each one tells the caches;
//! - [`pending`]: work raised by a write and done by the next settle, and the effect queue;
//! - [`turn`]: settle (package 1b-03 adds the turn stages);
//! - [`events`]: emitting events, their name references, and scrubbing them for a viewer
//!   (`game.py:806-990`);
//! - [`action`]: the typed actions and the pipeline every one runs through;
//! - [`invariants`] and [`debug`]: the checks that run in test builds;
//! - [`query`]: the reads refcheck and the views share;
//! - [`error`]: what a refused call says.
//!
//! **Porting markers.** A step whose system a later package ports is written as
//! [`pending`]`(Porting::Pending("<package>"))`, or [`pending_or`] where it must answer
//! something meanwhile. `cargo xtask check` counts these by their literal argument and fails
//! once that package is done (DESIGN.md 3.4, rule 3).

pub mod action;
pub mod core;
pub mod debug;
pub mod derive;
pub mod error;
pub mod eval;
pub mod events;
pub mod invariants;
pub mod mutate;
pub mod pending;
pub mod query;
pub mod turn;
pub mod vis;

pub use self::action::{Action, Outcome, OutcomeSpec};
pub use self::core::Game;
pub use self::debug::DebugOptions;
pub use self::error::{ActionError, EngineError, ErrCode};
pub use self::eval::EvalView;
pub use self::events::{EventBatch, Mention};
pub use self::invariants::{Code, Violation};

/// Whether a stage, or a step of one, is ported yet (DESIGN.md 3.4, rule 3; 6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Porting {
    /// Real.
    Ported,
    /// Waiting for the named work package, and an explicit no-op until then.
    Pending(&'static str),
}

/// Marks a step whose system a later package ports: a no-op until then. Write the argument as a
/// literal, `pending(Porting::Pending("1c-02"))`, so `cargo xtask check` counts it.
#[inline]
pub(crate) const fn pending(_: Porting) {}

/// Marks a value whose system a later package ports, and gives `meanwhile` until then. Write the
/// marker as a literal, `pending_or(Porting::Pending("1b-05"), 0)`.
#[inline]
pub(crate) fn pending_or<T>(_: Porting, meanwhile: T) -> T {
    meanwhile
}
