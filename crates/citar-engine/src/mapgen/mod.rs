//! Layer 3a: procedural map generation, with one keyed RNG stream per phase, and the map
//! editor's documents.
//!
//! Landmass, terrain, rivers, starts, wonders, resources and ruins (DESIGN.md 6.14, packages
//! 1b-04 and 1c-09). It may use `state` and the layers below it, but not `game`.
//!
//! Replaces `citar/engine/mapgen.py:1-1721` and `citar/engine/maps.py:48-60, 80-100, 124-248,
//! 323-365`:
//! - [`options`]: the map types, the edges and the lobby's river and resource settings
//!   (`MAP_TYPES`, `EDGE_MODES`, `MapOptions`);
//! - [`generate()`]: a whole map from a seed (`generate_map`), phase by phase: `noise` and
//!   `landmass` (the land's shape and the polar ice), `terrain` (climate, relief, lakes and
//!   coasts, vegetation, the conversions near rivers), `rivers`, `starts`, `wonders`,
//!   `resources` and `ruins`, over the map in the making (`map`);
//! - [`metrics`]: measures of a generated map, for the properties it must keep;
//! - [`document`]: an editor map read and cleaned (`maps.validate`, `maps.tiles_from_rows`,
//!   package 1b-03);
//! - [`continents`]: each tile's landmass, numbered from the largest (`mapgen._components`,
//!   `_assign_continents`).
//!
//! Python drew the whole map from one `random.Random(seed)`. Here each phase has its own
//! `Purpose` (`MapIce` to `MapRuins`, DESIGN.md 7.1), so tuning one phase never moves another
//! phase's draws; nothing is bit for bit Python's, and nothing needs to be.
//!
//! What map generation names (Ocean, Coast, Lakes, Mountain, Plains, Ice, Forest, Horses, ...)
//! is resolved when the ruleset loads (`rules::derived::KnownMap`); a ruleset without one skips
//! what needs it, where Python failed on the missing name.

pub mod continents;
pub mod document;
pub mod generate;
pub(crate) mod landmass;
pub(crate) mod map;
pub mod metrics;
pub(crate) mod noise;
pub mod options;
pub(crate) mod resources;
pub(crate) mod rivers;
pub(crate) mod ruins;
pub(crate) mod spread;
pub(crate) mod starts;
pub(crate) mod terrain;
pub(crate) mod wonders;

pub use document::MapError;
pub use generate::{ATTEMPTS, GenSpec, GeneratedMap, generate};
pub use landmass::max_depth as ice_depth;
pub use options::{IceSides, MAP_TYPES, MapOptions, MapType};
pub use resources::{luxury_types_wanted, luxury_variety};
pub use ruins::ruins_on;
pub use starts::fill_starts_on;
