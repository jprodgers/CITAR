//! Benchmarks of the CITAR engine, kept out of the engine's manifest (DESIGN.md 2.1, 9.7).
//!
//! Criterion suites measure wall clock on the laptop and gungraun suites count instructions in
//! CI. Later packages add the suites, `thresholds.toml` and `perfgate` (`cargo xtask perf`).

#![forbid(unsafe_code)]
