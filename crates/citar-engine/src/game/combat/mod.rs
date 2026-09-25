//! Combat (`combat.py:1-1215`): who fights, how strong each side is, what a fight does, cities'
//! bombardment, aircraft and nuclear weapons.
//!
//! - [`combatant`]: one side of a fight, a unit or a city (`combat.py:29-130`);
//! - [`strength`]: strengths, modifiers and damage, gathered once per fight into a
//!   [`CombatSetup`] (`combat.py:146-420`);
//! - [`resolve`]: whether a unit may attack a tile, the preview, and a fight with everything
//!   that follows it (`combat.py:422-888`);
//! - [`city`]: a city's bombardment, and a city taken (`combat.py:874-931`);
//! - [`air`]: interception, air strikes, air sweeps and rebasing (`combat.py:934-1073`);
//! - [`nuke`]: nuclear weapons (`combat.py:1076-1215`);
//! - [`actions`]: the tools `attack`, `air_sweep` and `city_attack` (`tools.py:456-491, 772-781`).
//!
//! **Randomness** (DESIGN.md 7.2). Python drew from the game's saved Mersenne Twister, so every
//! fight's outcome depended on every draw before it. Here every combat event (a fight, an
//! interception, an air sweep, a detonation) takes the next persisted `combat_seq` and draws from
//! a stream of its own, keyed by the turn, that number and the two sides or the target, so a
//! fight after a save and a load is the fight it would have been.
//!
//! Conquest, what a city taken becomes, is [`super::conquest`].

pub mod actions;
pub mod air;
pub mod city;
pub mod combatant;
pub mod nuke;
pub mod resolve;
pub mod strength;

pub use self::combatant::combatant_at;
pub use self::strength::{CombatSetup, setup};
