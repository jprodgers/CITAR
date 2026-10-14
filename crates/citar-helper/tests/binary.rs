//! The binary as 4-01 leaves it: `--version` prints the build and exits 0; anything else says
//! what this build does and exits 2.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

#[test]
fn version_prints_the_build_and_exits_zero() {
    let out = Command::new(BIN).arg("--version").output().expect("the binary runs");
    assert!(out.status.success(), "{out:?}");
    let line = String::from_utf8(out.stdout).expect("UTF-8");
    let want = citar_helper::version::build_info().line();
    assert_eq!(line.trim_end(), want);
    assert!(line.contains("protocol 2"), "{line}");
}

#[test]
fn anything_else_exits_two_and_says_so() {
    for args in [&[][..], &["run"][..], &["--version", "x"][..]] {
        let out = Command::new(BIN).args(args).output().expect("the binary runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("only prints --version"), "{err}");
    }
}
