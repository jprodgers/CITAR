//! The jobs crate's content code, `CITAR_JOBS_CODE` (design/phase4-helper.md P4.4.5): blake3 over
//! its sources, the golden set its self-check embeds, its version and the locked versions of what
//! it links. The jobs build combines it with the build id, the runner's code and the build's
//! flavour, so a change to how a job's result is built is a new jobs build. `content_code.rs`
//! (the engine's file, copied) says what is hashed and how.

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
    for path in ["src", "golden", "content_code.rs", "Cargo.toml", "../../Cargo.lock"] {
        println!("cargo::rerun-if-changed={path}");
    }
    let code = code::crate_code(code::JOBS_TAG, &version, &dir, &code::jobs_inputs(&dir))
        .unwrap_or_else(|e| panic!("the jobs crate's content code: {e}"));
    println!("cargo::rustc-env=CITAR_JOBS_CODE={code}");
}
