//! The bot's choices against the Python bot's on the committed states (package 2-01b, gate 2;
//! DESIGN.md P2.3.11): every kind of stage 1 at its floor of 95% over the items where either
//! engine says something, and every miss with a cause. The values are refcheck's
//! `bot_decisions` group, which `cargo refcheck run` enforces. A recording edited by hand shows
//! that a miss is found, and that its cause is named when one of the civilization's values
//! differs and left unattributed otherwise.
//!
//! Set `CITAR_REFCHECK_CORPUS` to the corpus and `CITAR_BOT_DUMP` to its recording to hold the
//! corpus to the same floors.

use std::path::{Path, PathBuf};

use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_refcheck::agreement::{self, Cause, Choice, FLOOR};
use citar_refcheck::answer::bot_decisions::recorded;
use citar_refcheck::fixture::{self, Fixture, FixtureSet};
use citar_refcheck::ratchet::DEFAULT_FIXTURES;
use serde_json::{Value, json};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn committed() -> Vec<FixtureSet> {
    let root = root();
    DEFAULT_FIXTURES.iter().map(|d| FixtureSet::new(&root.join(d), &root)).collect()
}

fn holds(sets: &[FixtureSet], what: &str) {
    let found = agreement::run(&root(), sets, |_| true).expect("the fixtures");
    assert!(found.skipped.is_empty(), "{what}: {:?}", found.skipped);
    for c in Choice::ALL {
        let t = found.tallies.get(&c).copied().unwrap_or_default();
        assert!(t.rate() >= FLOOR, "{what}: {} at {:.3}", c.name(), t.rate());
        assert!(t.asked > 0, "{what}: {} asked of nobody", c.name());
    }
    let unattributed: Vec<String> = found
        .misses
        .iter()
        .filter(|m| m.cause == Cause::Unattributed)
        .map(|m| format!("{} {} {}", m.state, m.player, m.choice.name()))
        .collect();
    assert!(unattributed.is_empty(), "{what}: {unattributed:?}");
    assert!(found.holds(), "{what}");
}

#[test]
fn the_bot_chooses_as_pythons_did_on_the_committed_states() {
    let found = agreement::run(&root(), &committed(), |_| true).expect("the fixtures");
    assert_eq!((found.states, found.civilizations), (12, 38));
    // The choices that say something on the committed states, as 2-00b's as-built note counts
    // them: none of them may be lost by a port that answers nothing.
    let considered = |c: Choice| found.tallies.get(&c).map_or(0, |t| t.considered);
    assert_eq!(considered(Choice::NextResearch), 76);
    assert_eq!(considered(Choice::PreferredPolicy), 38);
    assert_eq!(considered(Choice::Garrison), 96);
    assert_eq!(considered(Choice::Danger), 14);
    assert_eq!(considered(Choice::Sites), 29);
    assert_eq!(considered(Choice::Spare), 21);
    holds(&committed(), "the committed states");
}

#[test]
#[allow(clippy::disallowed_methods, reason = "a test's switch")]
fn the_bot_chooses_as_pythons_did_on_the_corpus() {
    let Some(dir) = std::env::var_os("CITAR_REFCHECK_CORPUS") else { return };
    if std::env::var_os("CITAR_BOT_DUMP").is_none() {
        return;
    }
    holds(&[FixtureSet::new(Path::new(&dir), &root())], "the corpus");
}

/// The recording of the duel at turn 50, and its game loaded as refcheck loads it.
fn duel_50() -> (Game, Vec<Value>) {
    let sets = committed();
    let r = fixture::discover(&sets)
        .expect("the fixtures")
        .into_iter()
        .find(|r| r.name == "duel-continents-normal/t50")
        .expect("the duel at turn 50");
    let f = Fixture::load(&r, &sets).expect("it loads");
    let rows = recorded(&root(), &f.meta.case, f.meta.turn).expect("recorded").to_vec();
    let (g, _) = Game::from_python(Ruleset::shared(), f.state.get().as_bytes()).expect("a game");
    (g, rows)
}

#[test]
fn a_choice_that_differs_is_a_miss_with_its_cause() {
    let (g, rows) = duel_50();
    let (_, misses) = agreement::compare_state(&g, "duel", &rows);
    assert!(misses.is_empty(), "{misses:?}");
    // Another preferred policy: a miss, and nothing in the values explains it.
    let mut edited = rows.clone();
    edited[0]["empire"]["preferred_policy"] = json!("Honor");
    let (tallies, misses) = agreement::compare_state(&g, "duel", &edited);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].choice, Choice::PreferredPolicy);
    assert_eq!(misses[0].python, json!("Honor"));
    assert_eq!(misses[0].cause, Cause::Unattributed);
    let t = tallies.iter().find(|(c, _)| *c == Choice::PreferredPolicy).map(|(_, t)| *t);
    assert_eq!(t.map(|t| (t.considered, t.agree)), Some((2, 1)));
    // With a value of the same civilization differing too, the miss is put down to it.
    edited[0]["context"]["supply"] = json!(-7);
    let (_, misses) = agreement::compare_state(&g, "duel", &edited);
    let Cause::Named(why) = &misses[0].cause else { panic!("{misses:?}") };
    assert!(why.contains("context.supply"), "{why}");
}

#[test]
fn an_answer_that_says_nothing_on_both_sides_is_not_counted() {
    let (g, rows) = duel_50();
    let (tallies, _) = agreement::compare_state(&g, "duel", &rows);
    let t = |c: Choice| tallies.iter().find(|(x, _)| *x == c).map(|(_, t)| *t).unwrap_or_default();
    // Nobody holds a free technology at turn 50: asked of both, considered for neither.
    assert_eq!((t(Choice::FreeNow).asked, t(Choice::FreeNow).considered), (4, 0));
    assert_eq!(t(Choice::FreeNow).rate().to_bits(), 1f64.to_bits());
}
