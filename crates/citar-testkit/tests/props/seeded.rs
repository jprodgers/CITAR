//! Gate 1 of package 1e-01: bugs planted on purpose (`citar_engine::game::seeded`) are found by
//! the properties and shrunk, within the budget of a CI run (DESIGN.md 9.5):
//! - a tool that mutates before refusing, by P2;
//! - a stale visibility cache, by P4;
//! - a touch with the wrong flag, by P4;
//! - a query that writes through interior mutability, by P8 (with the cache oracle on, which sees
//!   the memo it wrote, P4 may find it first; with the oracle off, P8 alone finds it).
//!
//! Each runs its property with proptest's 64 cases and its default shrinking, from a fixed seed so
//! that the run is the same every time, and asks that the bug be found, under the property that
//! should find it, and shrunk to a case of at most three steps, in less than two minutes.

use citar_engine::game::seeded::{SeededBug, seed};
use citar_testkit::stability::{Options, Property, Step};
use proptest::test_runner::{
    Config, FailurePersistence, RngAlgorithm, TestError, TestRng, TestRunner,
};

use super::games::{self, Start};

/// The most steps a shrunk case may keep.
const SHRUNK: usize = 3;

/// The budget of one hunt, CI's runners being slower than the laptop.
const BUDGET_SECONDS: f64 = 120.0;

/// Which property a hunt runs.
#[derive(Clone, Copy, Debug)]
enum Hunt {
    P1ToP7,
    P8,
}

/// Runs `hunt` with `bug` planted for 64 cases, and returns the shrunk case and what it broke.
fn hunt(bug: SeededBug, hunt: Hunt, options: Options) -> (Start, Vec<Step>, String) {
    let config = Config {
        cases: 64,
        failure_persistence: None::<Box<dyn FailurePersistence>>,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    #[allow(clippy::disallowed_types, reason = "the gate is a wall-clock budget; no game sees it")]
    let started = std::time::Instant::now();
    let strategy = (games::start(), games::steps(), proptest::prelude::any::<u64>());
    let result = runner.run(&strategy, |(start, steps, noise)| {
        // The start is built with no bug, as a host would have it; only the steps meet it.
        let g = games::build(&start);
        let _bug = seed(Some(bug));
        match hunt {
            Hunt::P1ToP7 => games::p1_to_p7(&g, &steps, options),
            Hunt::P8 => games::p8(&g, &steps, noise, options),
        }
    });
    let took = started.elapsed().as_secs_f64();
    #[allow(clippy::disallowed_macros, reason = "the gate's time is reported")]
    {
        println!("{bug:?} ({hunt:?}): {took:.1} s");
    }
    assert!(took < BUDGET_SECONDS, "{bug:?}: the hunt took {took:.0} s");
    match result {
        Err(TestError::Fail(why, (start, steps, _))) => (start, steps, why.to_string()),
        other => panic!("{bug:?} was not found in 64 cases: {other:?}"),
    }
}

/// Asks that `bug` be found under one of `properties` and shrunk.
fn found_and_shrunk(bug: SeededBug, how: Hunt, options: Options, properties: &[Property]) {
    let (start, steps, why) = hunt(bug, how, options);
    assert!(
        properties.iter().any(|p| why.starts_with(&format!("{p:?}:"))),
        "{bug:?}: found as {why}"
    );
    assert!(
        steps.len() <= SHRUNK,
        "{bug:?}: shrunk to {} steps from {start:?}: {steps:?}",
        steps.len()
    );
}

#[test]
fn a_tool_that_mutates_before_refusing_is_found_by_p2() {
    let bug = SeededBug::MutatesBeforeRefusing;
    found_and_shrunk(bug, Hunt::P1ToP7, Options::default(), &[Property::P2]);
}

#[test]
fn a_stale_visibility_cache_is_found_by_p4() {
    let bug = SeededBug::StaleVisibility;
    found_and_shrunk(bug, Hunt::P1ToP7, Options::default(), &[Property::P4]);
}

#[test]
fn a_touch_with_the_wrong_flag_is_found_by_p4() {
    found_and_shrunk(SeededBug::WrongTouch, Hunt::P1ToP7, Options::default(), &[Property::P4]);
}

#[test]
fn a_query_that_writes_through_interior_mutability_is_found_by_p8() {
    let bug = SeededBug::QueryWrites;
    // As CI runs it: the oracle, which checks the memo the query wrote, may see it first.
    found_and_shrunk(bug, Hunt::P8, Options::default(), &[Property::P4, Property::P8]);
    // Reads being free is enough on its own.
    let no_oracle = Options { verify_every: 0, ..Options::default() };
    found_and_shrunk(bug, Hunt::P8, no_oracle, &[Property::P8]);
}
