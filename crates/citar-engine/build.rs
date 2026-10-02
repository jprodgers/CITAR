//! The engine's content code, `CITAR_ENGINE_CODE` (DESIGN.md P2.2.1): blake3 over its sources,
//! the ruleset files it embeds, its version and the locked versions of what it links. It is
//! `rules::BUILD_ID`, so the id changes exactly when the engine's code or data can change a game.
//! `content_code.rs` says what is hashed and how.

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
    // A folder is scanned whole for changes: the inputs, this script's helper, and the manifest
    // and lock file whose dependencies the code follows.
    for path in ["src", "content_code.rs", "Cargo.toml", "../../citar/data", "../../Cargo.lock"] {
        println!("cargo::rerun-if-changed={path}");
    }
    let code = code::crate_code(code::ENGINE_TAG, &version, &dir, &code::engine_inputs(&dir))
        .unwrap_or_else(|e| panic!("the engine's content code: {e}"));
    println!("cargo::rustc-env=CITAR_ENGINE_CODE={code}");
}
