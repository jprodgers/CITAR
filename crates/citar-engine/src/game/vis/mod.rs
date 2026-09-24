//! What each civilization sees (DESIGN.md 6.9): incremental visibility, ported from
//! `visibility.py:1-243`, `units.sight` (`units.py:304-310`) and the spies' sight
//! (`espionage.py:444-451`).
//!
//! - [`los`]: line of sight, the elevation walk, and the cache of its answers;
//! - [`visibility`]: the sources, their footprints and each civilization's counts
//!   ([`Visibility`]);
//! - [`sight`](mod@sight): what each source sees (a unit's sight, a city's tiles, an ally's, a
//!   spy's), what a civilization can make out of a unit it sees, and line of sight for an attack;
//! - [`effects`]: bringing dirty sources up to date in settle (`Game::sync_sight`), and what it
//!   reveals: explored tiles, memory, first contact and natural wonders; `reveal_tiles`; the cache
//!   oracle ([`verify`]).
//!
//! Python recomputed every civilization's sight after every unit step, which was 54% of a
//! gargantuan game's time and changed nothing 98.5-99.5% of the time. Here a write marks the
//! sources it made stale, and only those are looked at again.

pub mod effects;
pub mod los;
pub mod sight;
pub mod visibility;

pub use self::effects::verify;
pub use self::sight::{
    enemy_spotted, has_los, has_sight, sight, sight_of, unit_viewable, unit_visible_to,
};
pub use self::visibility::{Sight, SightRules, SourceKey, Transition, VisSource, Visibility};

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;
