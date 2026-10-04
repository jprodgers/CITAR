//! Integration tests of the store (DESIGN.md P2.5), one file per part under `tests/store/`, by
//! package 2-02's gates:
//! - gate 1, round trips: `container` (headers and bodies) and `journal` (record sequences);
//!   the synthetic gargantuan state with 330 chunks is in citar-bench's `tests/store.rs`, the
//!   one crate the graph lets reach both the testkit's states and the store;
//! - gate 2, recovery of a 50-record journal cut and damaged everywhere: `recovery`;
//! - gate 3, the OS lock, in this process and from another: `lock`;
//! - gate 4, arbitrary and mangled bytes: `fuzz`.
//!
//! The property tests run `PROPTEST_CASES` cases, 256 when it is unset and 64 in CI
//! (`rust.yml`). The gates' counts are runs of their own, as nightly.yml's props job makes them:
//!
//! ```text
//! PROPTEST_CASES=1000  cargo nextest run -p citar-store --test store -E 'test(round_trip)'
//! PROPTEST_CASES=10000 cargo nextest run -p citar-store --test store --profile nightly \
//!     -E 'test(/^store::fuzz::/)'
//! ```
//!
//! (The nightly profile lets a test run past the default profile's five minutes: on the laptop
//! the virus scanner reads every file a case writes, some 10 to 30 ms a case.)

mod store {
    pub mod common;
    mod container;
    mod fuzz;
    mod journal;
    mod lock;
    mod recovery;
}
