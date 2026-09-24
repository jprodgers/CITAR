//! Layer 2: reading states written by the Python engine (feature `legacy`, test-only).
//!
//! [`python`] is the strict converter from Python's `GameState.to_dict()` format that refcheck,
//! testkit and bench use to start from recorded states (DESIGN.md 4.12, package 1a-10). It never
//! ships: neither citar-py nor the helper turns `legacy` on, and `cargo xtask check` holds every
//! other crate to that. It may use `save`, `state` and the layers below them.
//!
//! Replaces nothing at run time; it reads the format of `citar/engine/state.py`
//! (`GameState.to_dict`).

pub mod python;
