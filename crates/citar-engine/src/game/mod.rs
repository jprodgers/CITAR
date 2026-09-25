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
//! - [`derive`](mod@derive): the caches ([`derive::Derived`]) and the revisions and memos they
//!   validate themselves with ([`derive::rev`]), which replace `game.py:565-609`;
//! - [`eval`]: [`eval::EvalView`], the unique evaluator's view of a game;
//! - [`mutate`]: every write to the state, and what each one tells the caches;
//! - [`pending`]: work raised by a write and done by the next settle, and the effect queue;
//! - [`turn`]: settle, the stage tables of a turn, ending turns and rounds, and the seat drivers
//!   (package 1b-03);
//! - [`setup`]: new games, from the lobby's settings (package 1b-03);
//! - [`events`]: emitting events, their name references, and scrubbing them for a viewer
//!   (`game.py:806-990`);
//! - [`action`]: the typed actions and the pipeline every one runs through;
//! - [`invariants`] and [`debug`]: the checks that run in test builds;
//! - [`query`]: the reads refcheck and the views share;
//! - [`error`]: what a refused call says.
//!
//! The first rules of the systems, from package 1b-02, as far as the scenario operations need
//! them: [`research`] (granting, removing and setting research), [`diplomacy`] (war, peace,
//! pacts and opinions) and [`city_states`] (influence and allies). Their packages port the rest.
//!
//! The civilization-level economy (package 1b-05): the unique index memos, unit profiles and the
//! resource supply in `derive::civ`; [`economy`] (resources, unit and route upkeep, unit supply,
//! gold at the end of a turn, temporary uniques); and the uniques of a city
//! ([`cities::uniques`]) and of a unit ([`units`]).
//!
//! Package 1b-08: [`religion`] (pantheons, religions, beliefs, pressure and great prophets),
//! [`great_people`] (points, births, free great people, the Maya calendar, golden ages and the
//! great person actions), [`triggers`] (firing triggers and applying every one-time effect) and
//! [`ruins`].
//!
//! Combat and conquest (package 1c-03): [`combat`] (strengths and modifiers, fights, cities'
//! bombardment, aircraft and nuclear weapons) and [`conquest`] (a city taken, and what its
//! conqueror decides).
//!
//! Diplomacy and espionage (package 1c-05): the rest of [`diplomacy`] (deals, negotiations,
//! messages, declaring war and denouncing) and [`espionage`] (spies and stealing technology).
//!
//! Barbarians and city-states (package 1c-06): [`barbarians`] (camps, spawning, sacking and the
//! raiders' AI), the rest of [`city_states`] (setup, relationships, gifts, protection, tribute,
//! marriage, quests, the city-states' turns and their AI) and the city-state side of
//! [`espionage`] (elections and coups).
//!
//! Victory and defeat (package 1c-08): [`victory`] (score, the United Nations, the milestones,
//! eliminations, the statistics rows and replay frames each round records) and [`revolts`].
//!
//! **Porting markers.** A step whose system a later package ports is written as
//! `pending(Porting::Pending("<package>"))`, or `pending_or` where it must answer
//! something meanwhile. `cargo xtask check` counts these by their literal argument and fails
//! once that package is done (DESIGN.md 3.4, rule 3).

pub mod action;
pub mod actions;
pub mod advisor;
pub mod automation;
pub mod barbarians;
pub mod cities;
pub mod city_states;
pub mod combat;
pub mod conquest;
pub mod core;
pub mod debug;
pub mod derive;
pub mod diplomacy;
pub mod economy;
pub mod error;
pub mod espionage;
pub mod eval;
pub mod events;
pub mod great_people;
pub mod invariants;
pub mod meta;
pub mod movement;
pub mod mutate;
pub mod path;
pub mod pending;
pub mod policies;
pub mod query;
pub mod religion;
pub mod research;
pub mod revolts;
pub mod ruins;
pub mod setup;
pub mod tiles;
pub mod triggers;
pub mod turn;
pub mod units;
pub mod victory;
pub mod vis;
pub mod workers;

pub use self::action::{Action, Outcome, OutcomeSpec};
pub use self::core::Game;
pub use self::debug::DebugOptions;
pub use self::error::{ActionError, EngineError, ErrCode};
pub use self::eval::EvalView;
pub use self::events::{EventBatch, Mention};
pub use self::invariants::{Code, Violation};
pub use self::turn::{DriveOptions, DriverOutcome, Drivers, SeatDriver, Stop};

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
#[allow(dead_code, reason = "no step waits for a package now; a later port may mark one again")]
pub(crate) const fn pending(_: Porting) {}

/// Marks a value whose system a later package ports, and gives `meanwhile` until then. Write the
/// marker as a literal, `pending_or(Porting::Pending("1b-06"), 0)`.
#[inline]
#[allow(dead_code, reason = "no value waits for a package now; a later port may mark one again")]
pub(crate) fn pending_or<T>(_: Porting, meanwhile: T) -> T {
    meanwhile
}
