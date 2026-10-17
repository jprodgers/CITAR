//! The runner's content code, `CITAR_SIM_CODE` (design/phase4-helper.md P4.4.5): blake3 over its
//! sources, its version and the locked versions of what it links. It is not part of the build
//! id, which names the engine, the bot and the ruleset; the jobs build combines it with the build
//! id, since the runner changes games too (a seat with no driver passes, an unanswered chat
//! expires) and a reference job's line is built here. `content_code.rs` (the engine's file,
//! copied) says what is hashed and how.

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
    for path in ["src", "content_code.rs", "Cargo.toml", "../../Cargo.lock"] {
        println!("cargo::rerun-if-changed={path}");
    }
    let code = code::crate_code(code::SIM_TAG, &version, &dir, &code::sim_inputs(&dir))
        .unwrap_or_else(|e| panic!("the runner's content code: {e}"));
    println!("cargo::rustc-env=CITAR_SIM_CODE={code}");
}
