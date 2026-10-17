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
//!   endpoint (discovery, and its completions); [`proxy`]: the completions the server asks for,
//!   streamed, assembled raw and really cancelled; [`http1`]: one HTTP exchange on a connection
//!   its task owns; [`quiet`]: local quiet hours; [`scan`]: the hardware scan (the `citar-hardware`
//!   format, the power guess, the probe programs); [`power`]: the power readings; [`jobs`]: CPU
//!   jobs (the job host as a child, the job frames relayed, results signed and kept until
//!   acknowledged), [`jobhost`]: the job host as this binary's child; [`faults`]: a helper that
//!   cheats on purpose (`--test-fault`, the `test-faults` feature only), for the verification
//!   gates;
//! - [`app`]: what the binary does with its command line; [`version`]: `--version`;
//! - `conformance` (only with the `conformance` feature, or in this crate's own unit tests): the
//!   shared fixtures under `protocol/v2/`, the matcher language of their transcripts, and
//!   PROTOCOL.md's frame table, which this crate's tests and the transcript players read. It
//!   reads files under this crate's source directory, so it is never compiled into the helper that
//!   ships (cargo xtask check refuses the feature there, as it refuses `dev-guard`).
//!
//! A library, so that tests drive the helper in-process; `src/main.rs` is the binary. 4-03 adds
//! the configuration, the connection, keys, settings and the catalog, 4-04 the proxy, 4-06 the
//! scan and the power samples, 4-10 the CPU jobs; updates and services arrive with 4-13 and 4-14.
//! `src/bin/fake_probe.rs` is `citar-fake-probe`, a stand-in for the probe programs that only
//! development and test builds make (`required-features = ["dev-guard"]`).

#![forbid(unsafe_code)]

pub mod app;
pub mod cli;
pub mod clock;
pub mod config;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
pub mod faults;
pub mod home;
pub mod http1;
pub mod jobhost;
pub mod jobs;
pub mod key;
pub mod llm;
pub mod log;
pub mod net;
pub mod power;
pub mod protocol;
pub mod proxy;
pub mod quiet;
pub mod scan;
pub mod session;
pub mod settings;
pub mod version;
