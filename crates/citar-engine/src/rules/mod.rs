//! Layer 1: the ruleset, loaded once and shared by every game as `&'static Ruleset`.
//!
//! Typed tables, derived tables, name resolution, constants and the client JSON (DESIGN.md 5,
//! package 1a-03). It shares layer 1 with [`crate::unique`]: tables hold compiled uniques, and
//! the unique compiler resolves names against the tables.
//!
//! Replaces `citar/engine/rules.py:37-342` and reads the data in `citar/data/**`.
