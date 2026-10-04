//! The extension's link arguments, so a plain `cargo build` links it as maturin does.
//!
//! A Python extension does not link libpython: the interpreter that imports it provides the
//! symbols. Two settings make cargo build it that way on every OS:
//! - `PYO3_BUILD_EXTENSION_MODULE`, which `.cargo/config.toml` sets (PyO3 0.29's replacement for
//!   the deprecated `extension-module` feature, and what maturin sets). Without it pyo3-ffi links
//!   `-lpython3.x` on Linux, which fails where no Python development library is installed.
//! - On macOS, `-undefined dynamic_lookup`, which this script adds: the cdylib's Python symbols
//!   are left for the loader. (maturin adds the same; Windows and Linux need nothing here.)

#![forbid(unsafe_code)]

fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
