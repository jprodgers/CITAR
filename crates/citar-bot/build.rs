//! The bot's content code, `CITAR_BOT_CODE` (DESIGN.md P2.2.1): blake3 over its sources, its
//! versions' parameter schemas (`params/*.json`), its version and the locked versions of what it
//! links. `citar_bot::build_id` combines it with the engine's code and a ruleset's id.
//! `content_code.rs` (the engine's file, copied) says what is hashed and how.

#![forbid(unsafe_code)]
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    reason = "a build script reads its environment and its crate's files, and talks to cargo \
              on its standard output"
)]

#[path = "content_code.rs"]
mod code;

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets it");
    // The crate's folder is small and scanned whole, so a schema added under params/ (which may
    // not exist yet) is seen; the lock file is the workspace's.
    for path in [".", "../../Cargo.lock"] {
        println!("cargo::rerun-if-changed={path}");
    }
    let code = code::crate_code(code::BOT_TAG, &version, &dir, &code::bot_inputs(&dir))
        .unwrap_or_else(|e| panic!("the bot's content code: {e}"));
    println!("cargo::rustc-env=CITAR_BOT_CODE={code}");
}
