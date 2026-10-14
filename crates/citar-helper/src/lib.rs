//! The CITAR helper (design/phase4-helper.md P4.3): one process per machine that dials out to a
//! CITAR server over protocol 2 and lends the machine's model endpoint and, if its owner says so,
//! its spare CPU.
//!
//! - [`protocol`]: every frame of protocol 2 as a serde type, and the checks both sides run
//!   (`PROTOCOL.md` is the normative text);
//! - `conformance` (only with the `conformance` feature, or in this crate's own unit tests): the
//!   shared fixtures under `protocol/v2/`, the matcher language of their transcripts, and
//!   PROTOCOL.md's frame table, which this crate's tests and the later players of the transcripts
//!   read. It reads files under this crate's source directory, so it is never compiled into the
//!   helper that ships (cargo xtask check refuses the feature there, as it refuses `dev-guard`);
//! - [`version`]: what `citar-helper --version` prints.
//!
//! A library, so that tests drive the helper in-process; `src/main.rs` is the binary. Package
//! 4-01 lays the protocol down; the connection, the proxy, the scan, jobs, keys, updates and
//! services arrive with packages 4-03 to 4-14.

#![forbid(unsafe_code)]

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
pub mod protocol;
pub mod version;
