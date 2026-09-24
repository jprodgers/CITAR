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
            "turns", "maps"
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
fn a_set_that_depends_on_pending_stages_is_computed_but_never_blessed() {
    // Package 1b-03's gate 4: `golden bless` refuses while the stages a set depends on are
    // pending. The turns set depends on every stage of setup and of a turn.
    let waiting = golden::turns::waiting();
    assert!(!waiting.is_empty(), "stages are pending until package 1c-10");
    let refused = golden::bless_refusals();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].0, "turns.json");
    assert!(refused[0].1.contains("pending"), "{}", refused[0].1);
    assert!(
        golden::blessed_files().iter().all(|(file, _)| *file != "turns.json"),
        "bless leaves the set out"
    );
    let report = golden::turns::check_turns();
    assert_eq!(report.waiting, waiting);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.computed.len(), 64, "the set is computed all the same");
    // The same answers twice: the game is a function of its settings.
    assert_eq!(golden::turns::check_turns().computed, report.computed);
}
