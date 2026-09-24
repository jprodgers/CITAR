//! Layer 0: the building blocks every other layer uses, and nothing else.
//!
//! Ids and id sets, deterministic collections, Python-compatible maths and number formatting,
//! the keyed RNG, tie-breaking order, the hex grid, text scanning, yields, the canonical digest
//! writer (DESIGN.md 3.2, package 1a-02), the serde forms the save and the digest share that
//! need no ruleset (`codec`, package 1a-09), and Python's reading of JSON values (`py`, package
//! 1b-02). It depends on no other module of this crate.
//!
//! Replaces `citar/engine/hexmap.py:1-214`; the RNG sites `game.py:557-559` (`state_rng`),
//! `118-122` and `995-1002` (`g.rng`, `save_rng`) and `mapgen.py:1476-1490` (`_side_rng`); the
//! name normalisation of `rules.py:26-30`; and the possessives of `game.py:17-23`.

pub mod codec;
pub mod collections;
pub mod digest;
pub mod fmt;
pub mod hex;
pub mod ids;
pub mod num;
pub mod order;
pub mod py;
pub mod rng;
pub mod sets;
pub mod stats;
pub mod text;
