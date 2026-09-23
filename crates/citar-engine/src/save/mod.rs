//! Layer 2: saving and loading `State`, the canonical digest and the journal chunks.
//!
//! The JSON codec, load-time validation, the digest over the canonical serialisation, and the
//! summary a host can show without loading a game (DESIGN.md 4.9-4.11, package 1a-09). It may use
//! `state` and the layers below it.
//!
//! Replaces the save path of `citar/session.py` and `victory.record_frame`
//! (`citar/engine/victory.py:458-485`).
