//! Layer 1: the unique language, compiled once at load and evaluated without string compares.
//!
//! Unique types, the compiler, filters, conditionals, countables, triggers and the unique
//! indexes (DESIGN.md 5, packages 1a-05 to 1a-07). It shares layer 1 with [`crate::rules`].
//!
//! Replaces `citar/engine/unique_types.py`, `uniques.py:28-1083`, `economy.py:64-147`,
//! `triggers.py:13-74` and `cities.py:1169-1193`.
//!
//! - [`text`]: taking a unique's text apart (package 1a-03, which the ruleset loader needs).

pub mod text;
