//! The golden sets, checked in the test run on every OS rust.yml covers (DESIGN.md 9.6).
//!
//! determinism.yml runs the same check through the `golden` binary on all five targets and
//! compares the targets with each other; this test makes a mismatch with the committed files fail
//! an ordinary test run too.

use citar_testkit::golden;

#[test]
fn golden_sets_match_the_committed_files() {
    let reports = golden::check_all();
    let names: Vec<&str> = reports.iter().map(|r| r.name).collect();
    assert_eq!(names, ["rng", "libm", "pyfmt", "ruleset", "uniques", "filters", "gen"]);
    let problems: Vec<String> = reports
        .iter()
        .flat_map(|r| r.problems.iter().map(move |p| format!("{}: {p}", r.name)))
        .collect();
    assert!(
        problems.is_empty(),
        "golden sets differ (bless with `cargo golden bless` only if the change is deliberate):\n{}",
        problems.join("\n")
    );
}

#[test]
fn blessing_reproduces_the_committed_files() {
    // The rendered text, not just the values: a bless on any target writes the same bytes.
    for (file, text) in golden::blessed_files() {
        let committed = golden::golden_dir().join(file);
        #[allow(clippy::disallowed_methods, reason = "reads the committed golden file")]
        let on_disk = std::fs::read_to_string(&committed).expect("the committed golden file");
        // Git may check text out with CRLF on Windows.
        assert_eq!(on_disk.replace("\r\n", "\n"), text, "{file} differs from a fresh bless");
    }
}
