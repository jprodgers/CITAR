//! The game core (package 1b-01) on the recorded fixtures:
//! - `Game::from_python` loads every committed fixture (9 mini and 3 late), and the corpus when
//!   `CITAR_REFCHECK_CORPUS` names its folder, with every invariant of DESIGN.md 9.4 holding and
//!   the caches equal to a cold rebuild (gate 6);
//! - the settle on load changes nothing the state holds while no system reacts yet, and a game
//!   saves and loads back to the same digest;
//! - reads are free: views, scrubbed feeds, queries and refused calls leave the digest and the
//!   revision alone (a first taste of property P8).

use citar_engine::game::{Game, invariants::Violation};
use citar_engine::rules::Ruleset;
use citar_engine::save;
use citar_testkit::fixtures::{self, Fixture};

fn every_fixture() -> Vec<Fixture> {
    let mut out = fixtures::committed().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out.len(), 12, "9 mini and 3 late states");
    out.extend(fixtures::corpus().unwrap_or_else(|e| panic!("{e}")).into_iter().flatten());
    out
}

fn load(f: &Fixture) -> Game {
    let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    let (g, _report) = Game::from_python(Ruleset::shared(), &bytes)
        .unwrap_or_else(|e| panic!("{} does not load: {e}", f.name));
    g
}

#[test]
fn every_fixture_loads_with_its_invariants_holding() {
    let mut broken: Vec<String> = Vec::new();
    for f in every_fixture() {
        let mut g = load(&f);
        let found: Vec<Violation> =
            g.take_violations().into_iter().chain(g.check_invariants()).collect();
        broken.extend(found.iter().map(|v| format!("{}: {v}", f.name)));
        broken.extend(g.verify_caches().into_iter().map(|why| format!("{}: {why}", f.name)));
    }
    assert!(broken.is_empty(), "{} violations:\n{}", broken.len(), broken.join("\n"));
}

#[test]
fn the_settle_on_load_changes_nothing_yet_and_a_save_round_trips() {
    for f in fixtures::committed().unwrap_or_else(|e| panic!("{e}")) {
        let bytes = fixtures::read_state(&f).unwrap_or_else(|e| panic!("{e}"));
        let converted = citar_engine::compat::python::state_from_python(&bytes, Ruleset::shared())
            .unwrap_or_else(|e| panic!("{}: {e}", f.name));
        let mut g = load(&f);
        let r = Ruleset::shared();
        let digest = g.digest().unwrap_or_else(|e| panic!("{}: {e}", f.name));
        // No system reacts to a settle yet (sight 1c-01, citizens 1b-06), so the loaded state is
        // the converted one, but for the civilians Python left at 0 health, which keep 1.
        let zero: Vec<_> =
            converted.state.units().iter().filter(|u| u.hp <= 0).map(|u| u.id()).collect();
        if zero.is_empty() {
            assert_eq!(save::digest(r, &converted.state).ok(), Some(digest), "{}", f.name);
        }
        for u in converted.state.units().iter() {
            let hp = g.unit(u.id()).map(|x| x.hp);
            let want = if zero.contains(&u.id()) { 1 } else { u.hp };
            assert_eq!(hp, Some(want), "{}: unit {}", f.name, u.id());
        }
        let mut chunks = Vec::new();
        while let Some(c) = g.take_journal_chunk().unwrap_or_else(|e| panic!("{}: {e}", f.name)) {
            chunks.push(c.json);
        }
        // Taking the chunks counted them in the host heads, so the save is taken after it.
        let json = g.snapshot().to_json().unwrap_or_else(|e| panic!("{}: {e}", f.name));
        let mut it = chunks.iter().map(Vec::as_slice);
        let (back, report) = Game::load(r, &json, &mut it)
            .unwrap_or_else(|e| panic!("{} does not load back: {e}", f.name));
        assert!(!report.chronicle_incomplete, "{}: the history came back whole", f.name);
        assert_eq!(back.digest().ok(), Some(digest), "{}", f.name);
        assert_eq!(back.chronicle().events().len(), g.chronicle().events().len(), "{}", f.name);
        assert!(back.check_invariants().is_empty(), "{}", f.name);
    }
}

#[test]
fn reads_leave_the_game_as_it_was() {
    for f in fixtures::committed().unwrap_or_else(|e| panic!("{e}")) {
        let g = load(&f);
        let before = (g.digest().ok(), g.rev());
        for (p, _) in g.state().players().iter() {
            let feed = g.events_for(Some(p), 0, 50);
            assert!(feed.len() <= 50);
            assert!(g.known_to(Some(p)).is_some_and(|k| k.contains(p)));
            assert!(g.name_refs(&g.state().players()[p].name, &[]).len() <= 1);
        }
        assert_eq!(g.events_for(None, 0, usize::MAX).len(), g.chronicle().events().len());
        assert!(g.check_invariants().is_empty());
        assert!(g.verify_caches().is_empty());
        assert_eq!((g.digest().ok(), g.rev()), before, "{}", f.name);
    }
}
