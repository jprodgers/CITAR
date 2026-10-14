//! The CITAR helper (design/phase4-helper.md P4.3): one process per machine that dials out to a
//! CITAR server over protocol 2 and lends the machine's model endpoint and, if its owner says so,
//! its spare CPU.
//!
//! - [`protocol`]: every frame of protocol 2 as a serde type, and the checks both sides run
//!   (`PROTOCOL.md` is the normative text);
//! - [`cli`], [`config`], [`home`]: the command line, the settings (protocol 1's sources with
//!   their meanings, and the new ones), the home and the development guard;
//! - [`log`]: the redacting log; [`clock`]: the wall clock, fake in the transcripts;
//! - [`key`]: the machine key, owner-only on disk (through citar-jobs' `sys` module);
//! - [`net`]: dialling, TLS, the loopback guard; [`session`]: the connection, the handshake and
//!   every reaction; [`settings`]: the acknowledgement of the site's settings; [`llm`]: the model
//!   endpoint's discovery; [`quiet`]: local quiet hours;
//! - [`app`]: what the binary does with its command line; [`version`]: `--version`;
//! - `conformance` (only with the `conformance` feature, or in this crate's own unit tests): the
//!   shared fixtures under `protocol/v2/`, the matcher language of their transcripts, and
//!   PROTOCOL.md's frame table, which this crate's tests and the transcript players read. It
//!   reads files under this crate's source directory, so it is never compiled into the helper that
//!   ships (cargo xtask check refuses the feature there, as it refuses `dev-guard`).
//!
//! A library, so that tests drive the helper in-process; `src/main.rs` is the binary. 4-03 adds
//! the configuration, the connection, keys, settings and the catalog; the proxy, the scan, jobs,
//! updates and services arrive with 4-04 to 4-14.

#![forbid(unsafe_code)]

pub mod app;
pub mod cli;
pub mod clock;
pub mod config;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
pub mod home;
pub mod key;
pub mod llm;
pub mod log;
pub mod net;
pub mod protocol;
pub mod quiet;
pub mod session;
pub mod settings;
pub mod version;
