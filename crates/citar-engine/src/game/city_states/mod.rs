//! City-states (`city_states.py`).
//!
//! - [`influence`]: influence and allies (`city_states.py:65-153`), from package 1b-02, which
//!   the scenario operations need.
//!
//! Package 1c-06 ports the rest: setup, friend and ally bonuses, gifts, tribute, protection,
//! quests, and the city-states' own turns.

pub mod influence;

pub use self::influence::{ALLY_INFLUENCE, MIN_INFLUENCE};
