//! The target triple the helper was built for: `--version` prints it and the hello carries it
//! (`helper.target`, PROTOCOL.md), and an update looks its asset up by it (P4.3.8). Cargo gives
//! it only to build scripts, so this hands it on to the crate.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=CITAR_HELPER_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
