//! Reference checks: the Rust engine's answers compared with the recorded Python answers.
//!
//! The Rust side of `refcheck/README.md` (DESIGN.md 9.2). Package 1a-04 adds fixture loading,
//! the comparator, the intended-differences list, the ratchet and the reports; each system
//! package then adds its answer module.

#![forbid(unsafe_code)]
