//! The binary's command line: `--version` prints the build and exits 0; a command line it does not
//! take exits 2, as argparse did. The connection's tests are `tests/real_binary.rs`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

#[test]
fn version_prints_the_build_and_exits_zero() {
    let out = Command::new(BIN).arg("--version").output().expect("the binary runs");
    assert!(out.status.success(), "{out:?}");
    let line = String::from_utf8(out.stdout).expect("UTF-8");
    let want = citar_helper::version::build_info().line();
    assert_eq!(line.trim_end(), want);
    assert!(line.contains("protocol 2") && line.contains("jobs build none"), "{line}");
}

#[test]
fn a_command_line_it_does_not_take_exits_two() {
    for args in [&["--bogus"][..], &["--provider", "vllm"], &["scan-everything"], &["--no"]] {
        let out = Command::new(BIN).args(args).output().expect("the binary runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}
