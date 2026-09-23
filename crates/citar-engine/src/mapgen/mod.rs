//! Layer 3a: procedural map generation, with one keyed RNG stream per phase.
//!
//! Landmass, terrain, rivers, starts, wonders, resources and ruins (DESIGN.md 6.14, packages
//! 1b-04 and 1c-09). It may use `state` and the layers below it, but not `game`.
//!
//! Replaces `citar/engine/mapgen.py:1-1721` and `citar/engine/maps.py:80-100, 323-365`.
