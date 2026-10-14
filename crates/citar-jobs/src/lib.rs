//! CITAR's CPU jobs (design/phase4-helper.md P4.4): what the helper and the server must compute
//! the same way, once. Package 4-03 creates the crate holding only [`sys`], the operating-system
//! calls no safe wrapper makes; the job vocabulary, the Rust lab game, hashes, claims and the job
//! host arrive with 4-07 to 4-11.
//!
//! The workspace denies `unsafe_code`; [`sys`]'s two per-OS files allow it, each block with its
//! `// SAFETY:` comment, and `cargo xtask check` refuses an `unsafe` token anywhere else in this
//! crate and anywhere in citar-helper, which calls this module's safe functions (P4.3.1).

pub mod sys;
