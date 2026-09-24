//! Layer 3a: procedural map generation, with one keyed RNG stream per phase, and the map
//! editor's documents.
//!
//! Landmass, terrain, rivers, starts, wonders, resources and ruins (DESIGN.md 6.14, packages
//! 1b-04 and 1c-09). It may use `state` and the layers below it, but not `game`.
//!
//! Replaces `citar/engine/mapgen.py:1-1721` and `citar/engine/maps.py:48-60, 80-100, 124-248,
//! 323-365`. From package 1b-03:
//! - [`document`]: an editor map read and cleaned (`maps.validate`, `maps.tiles_from_rows`);
//! - [`continents`]: each tile's landmass, numbered from the largest (`mapgen._components`,
//!   `_assign_continents`).

pub mod continents;
pub mod document;
