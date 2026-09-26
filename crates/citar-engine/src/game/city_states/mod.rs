//! City-states (`city_states.py`; UnCiv's `CityStateFunctions`, the city-state parts of
//! `DiplomacyManager` and `DiplomacyTurnManager`, and `QuestManager`).
//!
//! - [`influence`]: influence and allies (package 1b-02, which the scenario operations need),
//!   and the relationship they make, the resting point and the drift toward it;
//! - [`actions`]: what a major may do with a city-state, the `city_state_action` tool: gifts,
//!   protection, tribute, peace and marriage;
//! - [`quests`]: the quests city-states give and reward;
//! - [`turn`]: setup, the end of a city-state's turn, the great people allies give, first
//!   contact, and the reactions to attacks, conquests and kills;
//! - [`ai`]: a city-state's own turn: founding, production, bombardment and its units.
//!
//! Every write of influence goes through [`influence::set_influence`], which settles the ally
//! and tells a major's unique index when its friend level flips. The rest of a city-state's data
//! (its quests, pairs, protectors and countdowns) feeds no cache.

pub mod actions;
pub mod ai;
pub mod influence;
pub mod quests;
pub mod turn;

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

pub use self::actions::CityStateAction;
pub use self::influence::{ALLY_INFLUENCE, FRIEND_INFLUENCE, MIN_INFLUENCE, Relationship};
