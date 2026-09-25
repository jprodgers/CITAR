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
    assert_eq!(
        names,
        [
            "rng", "libm", "pyfmt", "ruleset", "uniques", "filters", "gen", "states", "convert",
            "turns", "maps", "newgame"
        ]
    );
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

#[test]
fn the_turns_set_is_blessed_and_checked_once_no_stage_waits() {
    // Package 1b-03's gate 4: `golden bless` refuses a set while the stages it depends on are
    // pending. The turns set depends on every stage of setup and of a turn, and since package
    // 1c-09 none is: it is blessed and checked like any other.
    assert!(golden::turns::waiting().is_empty(), "no stage of setup or of a turn waits");
    assert!(golden::turns::refusal().is_none());
    assert!(golden::bless_refusals().is_empty());
    assert!(
        golden::blessed_files().iter().any(|(file, _)| *file == "turns.json"),
        "bless writes the set"
    );
    let report = golden::turns::check_turns();
    assert!(report.waiting.is_empty());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.computed.len(), 64);
    // The same answers twice: the game is a function of its settings.
    assert_eq!(golden::turns::check_turns().computed, report.computed);
}
